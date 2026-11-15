//! The background thread that runs syncs, so the window never waits on the
//! network. Backends stay open between syncs (keeping access tokens and
//! connections), and are reopened when an account's settings or secret
//! change.

use std::collections::{HashMap, HashSet};
use std::sync::mpsc::{self, Receiver, Sender};

use super::engine::{self, AccountSnapshot, Outcome};
use super::{Backend, BackendConfig, NeedsSignIn};

pub enum Job {
    Sync { config: BackendConfig, secret: Option<String>, snapshot: AccountSnapshot },
    /// Close an account's backend (removed, or signed out).
    Forget { account: String },
}

pub struct Synced {
    pub account: String,
    pub snapshot: AccountSnapshot,
    pub result: Result<Outcome, String>,
    /// The saved credentials no longer work.
    pub needs_sign_in: bool,
    /// Credentials the backend renewed, to save.
    pub renewed_secret: Option<String>,
}

pub struct Worker {
    tx: Sender<Job>,
    pub rx: Receiver<Synced>,
    /// Accounts with a sync running or queued.
    pub busy: HashSet<String>,
}

struct Open {
    config: BackendConfig,
    secret: Option<String>,
    backend: Box<dyn Backend>,
}

impl Worker {
    pub fn spawn(on_reply: impl Fn() + Send + 'static) -> Worker {
        let (tx, jobs) = mpsc::channel::<Job>();
        let (replies, rx) = mpsc::channel();
        std::thread::Builder::new()
            .name("sync".into())
            .spawn(move || {
                let mut open: HashMap<String, Open> = HashMap::new();
                for job in jobs {
                    match job {
                        Job::Forget { account } => {
                            open.remove(&account);
                        }
                        Job::Sync { config, secret, snapshot } => {
                            let reply = run(&mut open, config, secret, snapshot);
                            if replies.send(reply).is_err() {
                                return;
                            }
                            on_reply();
                        }
                    }
                }
            })
            .expect("starting the sync thread");
        Worker { tx, rx, busy: HashSet::new() }
    }

    pub fn sync(&mut self, config: BackendConfig, secret: Option<String>, snapshot: AccountSnapshot) {
        if self.busy.insert(snapshot.account.clone()) {
            let _ = self.tx.send(Job::Sync { config, secret, snapshot });
        }
    }

    pub fn forget(&mut self, account: &str) {
        let _ = self.tx.send(Job::Forget { account: account.to_owned() });
    }
}

fn run(open: &mut HashMap<String, Open>, config: BackendConfig, secret: Option<String>, snapshot: AccountSnapshot) -> Synced {
    let account = snapshot.account.clone();
    let mut reply =
        Synced { account: account.clone(), snapshot, result: Err(String::new()), needs_sign_in: false, renewed_secret: None };
    let stale = open.get(&account).map_or(true, |o| o.config != config || o.secret != secret);
    if stale {
        open.remove(&account);
        match super::open(&config, secret.clone()) {
            Ok(backend) => {
                open.insert(account.clone(), Open { config, secret, backend });
            }
            Err(e) => {
                reply.needs_sign_in = e.is::<NeedsSignIn>();
                reply.result = Err(format!("{e:#}"));
                return reply;
            }
        }
    }
    let o = open.get_mut(&account).unwrap();
    let result = engine::sync_account(o.backend.as_mut(), &reply.snapshot, chrono::Utc::now());
    reply.renewed_secret = o.backend.renewed_secret();
    if let Some(s) = &reply.renewed_secret {
        o.secret = Some(s.clone());
    }
    match result {
        Ok(out) => reply.result = Ok(out),
        Err(e) => {
            reply.needs_sign_in = e.is::<NeedsSignIn>();
            reply.result = Err(format!("{e:#}"));
            // Start from scratch next time (a dropped connection, a token
            // that went bad, …).
            open.remove(&account);
        }
    }
    reply
}
