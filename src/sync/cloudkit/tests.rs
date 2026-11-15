//! The CloudKit backend against the fake ([`super::fake`]).

use chrono::Utc;

use super::fake::*;
use super::*;
use crate::model::{Account, Note, Store};
use crate::sync::engine::{apply, sync_account, AccountSnapshot};

fn config(server: &FakeCloudKit, port: u16) -> Config {
    Config {
        container: CONTAINER.into(),
        environment: "development".into(),
        api_token: API_TOKEN.into(),
        callback_port: port,
        endpoint: Some(server.url.clone()),
    }
}

fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port()
}

fn backend(server: &FakeCloudKit) -> CloudKitBackend {
    let secret = serde_json::to_string(&Session { web_auth_token: server.sign_in(), user: "_user1".into() }).unwrap();
    CloudKitBackend::open(&config(server, 0), Some(secret)).unwrap()
}

fn note(id: &str, folder: &[&str], body: &str) -> NoteData {
    NoteData {
        id: id.into(),
        folder: folder.iter().map(|s| s.to_string()).collect(),
        body: body.into(),
        created: DateTime::from_timestamp_millis(1_763_200_000_000).unwrap(),
        modified: DateTime::from_timestamp_millis(1_763_200_100_000).unwrap(),
        pinned: true,
        deleted: None,
    }
}

#[test]
fn signing_in_through_the_browser() {
    let server = FakeCloudKit::start();
    let port = free_port();
    let token = server.sign_in();
    let cfg = config(&server, port);
    let mut opened = String::new();
    let secret = sign_in_with(&cfg, |u| {
        opened = u.to_owned();
        // Apple's page sends the browser back to the loopback port.
        let t = token.clone();
        std::thread::spawn(move || {
            let _ = reqwest::blocking::get(format!("http://127.0.0.1:{port}/favicon.ico"));
            reqwest::blocking::get(format!("http://127.0.0.1:{port}/?ckWebAuthToken={}", enc(&t))).unwrap();
        });
        Ok(())
    })
    .unwrap();
    assert!(opened.ends_with("/appleid-signin"));
    let s: Session = serde_json::from_str(&secret).unwrap();
    assert_eq!((s.web_auth_token.as_str(), s.user.as_str()), (token.as_str(), "_user1"));
}

#[test]
fn a_wrong_api_token_is_reported() {
    let server = FakeCloudKit::start();
    let cfg = Config { api_token: "nope".into(), ..config(&server, free_port()) };
    let err = sign_in_with(&cfg, |_| panic!("no browser")).unwrap_err();
    assert!(err.to_string().contains("API token"), "{err:#}");
}

#[test]
fn notes_are_records_in_the_private_zone() {
    let server = FakeCloudKit::start();
    let mut b = backend(&server);
    let n = note("n1", &["Work"], "# Plan\n- [ ] ship it\n");
    let (rid, tag) = b.save_note(None, None, &n).unwrap();
    assert_eq!(rid, "n1");
    {
        let s = server.state();
        let r = &s.records["n1"];
        assert_eq!(r["recordType"], "Note");
        assert_eq!(r["fields"]["title"]["value"], "Plan");
        assert_eq!(r["fields"]["folder"]["value"], serde_json::json!(["Work"]));
    }
    let snap = b.pull().unwrap();
    assert_eq!(snap.notes.len(), 1);
    assert_eq!(snap.notes[0].revision, tag);
    assert_eq!(snap.notes[0].data, n);
}

#[test]
fn outdated_saves_are_conflicts() {
    let server = FakeCloudKit::start();
    let mut b = backend(&server);
    let (rid, t1) = b.save_note(None, None, &note("n1", &["Notes"], "a")).unwrap();
    let (_, t2) = b.save_note(Some(&rid), Some(&t1), &note("n1", &["Notes"], "b")).unwrap();
    assert!(b.save_note(Some(&rid), Some(&t1), &note("n1", &["Notes"], "c")).unwrap_err().is::<Conflict>());
    // A retried create finds the record there.
    assert!(b.save_note(None, None, &note("n1", &["Notes"], "c")).unwrap_err().is::<Conflict>());
    b.save_note(Some(&rid), Some(&t2), &note("n1", &["Notes"], "c")).unwrap();
    b.delete_note(&rid).unwrap();
    b.delete_note(&rid).unwrap();
    assert!(b.save_note(Some(&rid), Some(&t2), &note("n1", &["Notes"], "d")).unwrap_err().is::<Conflict>());
}

#[test]
fn many_records_page() {
    let server = FakeCloudKit::start();
    let mut b = backend(&server);
    for i in 0..450 {
        b.save_note(None, None, &note(&format!("n{i}"), &["Notes"], "x")).unwrap();
    }
    assert_eq!(b.pull().unwrap().notes.len(), 450);
}

#[test]
fn folders_rename_with_their_subfolders() {
    let server = FakeCloudKit::start();
    let mut b = backend(&server);
    let p = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
    let work = b.create_folder(&p(&["Work"])).unwrap();
    assert_eq!(b.create_folder(&p(&["Work"])).unwrap(), work);
    b.create_folder(&p(&["Work", "Old"])).unwrap();
    b.create_folder(&p(&["Workshop"])).unwrap();
    b.rename_folder(&work, &p(&["Work"]), &p(&["Projects"])).unwrap();
    let mut paths: Vec<_> = b.pull().unwrap().folders.into_iter().map(|f| f.path).collect();
    paths.sort();
    assert_eq!(paths, vec![p(&["Projects"]), p(&["Projects", "Old"]), p(&["Workshop"])]);
    b.delete_folder(&work, &[]).unwrap();
    b.delete_folder(&work, &[]).unwrap();
    assert_eq!(b.pull().unwrap().folders.len(), 2);
}

#[test]
fn an_expired_sign_in_asks_to_sign_in_again() {
    let server = FakeCloudKit::start();
    let mut b = backend(&server);
    b.pull().unwrap();
    server.state().tokens.clear();
    assert!(b.pull().unwrap_err().is::<NeedsSignIn>());
    assert!(CloudKitBackend::open(&config(&server, 0), None).err().unwrap().is::<NeedsSignIn>());
}

#[test]
fn rotated_tokens_are_kept() {
    let server = FakeCloudKit::start();
    let mut b = backend(&server);
    server.state().rotate_tokens = true;
    b.pull().unwrap();
    let renewed = b.renewed_secret().expect("a new token");
    server.state().rotate_tokens = false;
    let mut b2 = CloudKitBackend::open(&config(&server, 0), Some(renewed)).unwrap();
    b2.pull().unwrap();
}

struct Device {
    store: Store,
    backend: CloudKitBackend,
}

impl Device {
    fn new(server: &FakeCloudKit) -> Device {
        let mut store = Store::new_install();
        store.accounts.push(Account {
            id: "ck".into(),
            name: "iCloud".into(),
            backend: crate::sync::BackendConfig::CloudKit(config(server, 0)),
            enabled: true,
            cursor: None,
            last_sync: None,
        });
        store.ensure_local();
        Device { store, backend: backend(server) }
    }

    fn sync(&mut self) -> Vec<String> {
        let snap = AccountSnapshot::of(&self.store, "ck");
        let out = sync_account(&mut self.backend, &snap, Utc::now()).unwrap();
        assert_eq!(out.error, None);
        apply(&mut self.store, &snap, out)
    }
}

#[test]
fn two_devices_sync_through_cloudkit() {
    let server = FakeCloudKit::start();
    let mut laptop = Device::new(&server);
    let mut phone = Device::new(&server);
    let folder = laptop.store.ensure_folder_at("ck", &["Recipes".into()]);
    let n = Note::new("ck", &folder, "# Pancakes\nflour\neggs\n");
    let id = n.id.clone();
    laptop.store.notes.push(n);
    laptop.sync();
    phone.sync();
    let pn = phone.store.note(&id).unwrap();
    assert_eq!(pn.body, "# Pancakes\nflour\neggs\n");
    assert_eq!(phone.store.folder_path(&pn.folder), vec!["Recipes"]);

    for (dev, body) in [(&mut laptop, "# Pancakes\nflour (200 g)\neggs\n"), (&mut phone, "# Pancakes\nflour\neggs\nmilk\n")] {
        let n = dev.store.note_mut(&id).unwrap();
        n.body = body.into();
        n.touch();
    }
    laptop.sync();
    assert!(phone.sync().is_empty());
    laptop.sync();
    assert_eq!(laptop.store.note(&id).unwrap().body, "# Pancakes\nflour (200 g)\neggs\nmilk\n");

    server.state().log.clear();
    laptop.sync();
    assert!(!server.state().log.contains(&"records/changes".to_owned()), "an unchanged zone isn't listed again");
}
