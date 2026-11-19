//! CleanPassword's vault, shown in CleanNotes when it's kept in the same S3
//! bucket as an account's notes.
//!
//! [CleanPassword](../../CleanPassword) is the password manager whose vault
//! lives in an S3 folder of the user's own, end-to-end encrypted:
//!
//! ```text
//! <folder>/vault.json   the vault's id, Argon2id settings and salt, and the
//!                       vault key sealed with the master key
//! <folder>/vault.bin    every item, sealed with the vault key
//! ```
//!
//! Everything about that format is CleanPassword's own core
//! (`cleanpassword-core`, a git dependency pinned to a revision; it uses
//! this repository's `bucket/` as its S3 client, so both run on the same
//! [`Blocking`] transport): reading `vault.json`, unlocking, the items, and
//! syncing. CleanNotes is just another of the vault's devices: it joins the
//! vault with the master password, and deletes, recovers and deletes for
//! good with [`Vault::delete`], [`Vault::restore`] and [`Vault::purge`]
//! followed by [`Vault::sync`], which merges with what other devices saved
//! exactly as CleanPassword does (a `HEAD` of `vault.bin`, a download and
//! three-way merge by item if another device saved, and a `PUT` with
//! `If-Match`).
//!
//! What's CleanNotes' own here:
//! - [`find`]: which S3 account's bucket holds a vault, in the notes' folder
//!   or in its own (`CleanPassword` by default), with the account's access
//!   key;
//! - [`Passwords`], the background-thread controller the app drives:
//!   unlocking, refreshing every minute, locking after 15 minutes without
//!   use, and showing changes at once while they're saved;
//! - [`ItemExt`], [`host`] and [`totp::spaced`]: how items are shown.
//!
//! Between unlocks, the vault as CleanPassword's own devices keep it
//! ([`Vault::save`]: everything but the S3 settings and `vault.json` sealed
//! with the vault key) can be kept on this computer
//! ([`Passwords::set_cache_dir`]), so unlocking again usually downloads
//! nothing and works offline. The master password and the vault key are
//! never saved; they're kept in memory until [`Passwords::lock`] (Lock Now,
//! 15 minutes without use, or quitting).

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use cleanbucket::block_on;
use cleanpassword_core::sync::{fetch_header, SyncReport};
use zeroize::Zeroizing;

use crate::sync::s3::{self, Blocking, Keys};

pub use cleanpassword_core::crypto::KdfParams;
pub use cleanpassword_core::import::ImportedItem;
pub use cleanpassword_core::item::Passkey;
pub use cleanpassword_core::vault::VaultHeader;
pub use cleanpassword_core::{AwsCredentials, Error as VaultError, Field, HistoryEntry, Item, ItemData, ItemKind, ItemUri, S3Config, Vault};

#[cfg(test)]
mod tests;

/// CleanPassword's own folder, when it doesn't share the notes'.
pub const DEFAULT_FOLDER: &str = cleanpassword_core::vault::DEFAULT_PREFIX;
/// The vault locks after this long without use.
pub const AUTO_LOCK: Duration = Duration::from_secs(15 * 60);
/// An unlocked vault is synced this often while the app runs (a `GET` of
/// `vault.json` and a `HEAD` of `vault.bin` when nothing changed).
pub const REFRESH_EVERY: Duration = Duration::from_secs(60);

// ---- How items are shown ----------------------------------------------------

/// How CleanNotes shows and searches CleanPassword's items.
pub trait ItemExt {
    /// The name shown: the item's, or its first website's host, or its user
    /// name.
    fn title(&self) -> String;
    /// Whether every word of `query` is in its name, user name, websites,
    /// folder, notes, field names or passkeys' sites.
    fn matches(&self, query: &str) -> bool;
    /// Whether it's in `folder` or a folder inside it.
    fn in_folder(&self, folder: &str) -> bool;
}

impl ItemExt for Item {
    fn title(&self) -> String {
        if !self.name.trim().is_empty() {
            return self.name.trim().to_owned();
        }
        if let Some(h) = self.uris.first().and_then(|u| host(&u.uri)) {
            return h;
        }
        if !self.username.is_empty() {
            return self.username.clone();
        }
        "Untitled".into()
    }

    fn matches(&self, query: &str) -> bool {
        let hay = [&self.name, &self.username, &self.folder, &self.notes]
            .into_iter()
            .cloned()
            .chain(self.uris.iter().map(|u| u.uri.clone()))
            .chain(self.fields.iter().map(|f| f.name.clone()))
            .chain(self.passkeys.iter().map(|p| p.rp_id.clone()))
            .collect::<Vec<_>>()
            .join("\n")
            .to_lowercase();
        query.split_whitespace().all(|w| hay.contains(&w.to_lowercase()))
    }

    fn in_folder(&self, folder: &str) -> bool {
        self.folder == folder || self.folder.strip_prefix(folder).is_some_and(|r| r.starts_with('/'))
    }
}

/// The host name of a website as saved (`https://github.com/login` →
/// `github.com`; a bare `github.com` is taken as https).
pub fn host(uri: &str) -> Option<String> {
    let u = uri.trim();
    let parsed = reqwest::Url::parse(u).ok().filter(|p| p.host_str().is_some()).or_else(|| reqwest::Url::parse(&format!("https://{u}")).ok())?;
    let h = parsed.host_str()?.trim_start_matches("www.").to_owned();
    (!h.is_empty()).then_some(h)
}

/// Verification codes: CleanPassword's (RFC 6238, `otpauth://` URIs, Steam
/// Guard), as shown here.
pub mod totp {
    pub use cleanpassword_core::totp::{code, Code};

    /// The code now, `None` if the item has no secret codes can be made
    /// from.
    pub fn code_now(totp: &str) -> Option<Code> {
        if totp.trim().is_empty() {
            return None;
        }
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).ok()?.as_secs();
        code(totp, now).ok()
    }

    /// A code as shown: "123 456" (in halves, as Apple shows them).
    pub fn spaced(code: &str) -> String {
        if code.len() == 6 || code.len() == 8 {
            let (a, b) = code.split_at(code.len() / 2);
            format!("{a} {b}")
        } else {
            code.to_owned()
        }
    }
}

// ---- Finding it -------------------------------------------------------------

/// Where a vault was found.
#[derive(Clone, Debug, PartialEq)]
pub struct Found {
    /// The CleanNotes account whose access key reads it.
    pub account: String,
    /// The bucket, with the vault's folder as its prefix.
    pub config: s3::Config,
    /// Its `vault.json`, and that object's ETag.
    pub header: VaultHeader,
    pub header_etag: String,
}

impl Found {
    /// "bucket/folder", for saying where the vault is.
    pub fn place(&self) -> String {
        let f = self.config.prefix.trim_matches('/');
        if f.is_empty() {
            self.config.bucket.clone()
        } else {
            format!("{}/{f}", self.config.bucket)
        }
    }

    /// The vault's place as CleanPassword's core takes it.
    fn vault_config(&self) -> S3Config {
        vault_config(&self.config, &self.config.prefix)
    }
}

fn vault_config(config: &s3::Config, folder: &str) -> S3Config {
    S3Config { bucket: config.bucket.clone(), region: config.region.clone(), prefix: folder.to_owned(), endpoint: config.endpoint.clone() }
}

/// An S3 account's access key (its saved secret) as CleanPassword's core
/// takes it. (The notes' own encryption key, if any, isn't the vault's.)
fn credentials(secret: &str) -> Result<AwsCredentials> {
    let keys: Keys = serde_json::from_str(secret).context("the saved access key is unreadable")?;
    Ok(AwsCredentials { access_key_id: keys.access_key_id.clone(), secret_access_key: keys.secret_access_key.clone(), session_token: None })
}

/// Looks for a vault with an S3 account's access key: in the notes'
/// folder, then in [`DEFAULT_FOLDER`]. `Ok(None)` if neither has one (or
/// only something CleanPassword can't open, such as a vault of its first
/// format); an error only if neither could be looked in (offline, say).
pub fn find(account: &str, config: &s3::Config, secret: &str) -> Result<Option<Found>> {
    let creds = credentials(secret)?;
    let notes = config.prefix.trim().trim_matches('/').to_owned();
    let mut folders = vec![notes.clone()];
    if notes != DEFAULT_FOLDER {
        folders.push(DEFAULT_FOLDER.to_owned());
    }
    let http = Blocking::new();
    let mut first_error = None;
    let mut looked = false;
    for folder in folders {
        match block_on(fetch_header(&http, &vault_config(config, &folder), &creds)) {
            Ok(Some((header, header_etag))) => {
                let config = s3::Config { prefix: folder, encrypted: false, ..config.clone() };
                return Ok(Some(Found { account: account.to_owned(), config, header, header_etag }));
            }
            Ok(None) => looked = true,
            // Something else's vault.json, or one CleanPassword can't
            // open: no vault here.
            Err(VaultError::Invalid(_)) => looked = true,
            Err(e) => {
                first_error.get_or_insert(e);
            }
        }
    }
    match first_error {
        Some(e) if !looked => Err(anyhow::Error::new(e)),
        _ => Ok(None),
    }
}

// ---- Kept on this device ----------------------------------------------------

/// Where an unlocked vault is kept between unlocks ([`Vault::save`]).
/// `None` for a vault id that isn't a plain name.
fn cache_path(dir: &Path, vault_id: &str) -> Option<PathBuf> {
    let plain = !vault_id.is_empty() && vault_id.len() <= 64 && vault_id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-');
    plain.then(|| dir.join(format!("cleanpassword-{vault_id}.vault")))
}

fn save_cache(dir: &Path, vault: &Vault) {
    let Some(path) = cache_path(dir, vault.vault_id()) else { return };
    let tmp = path.with_extension("tmp");
    if std::fs::write(&tmp, vault.save()).is_ok() {
        let _ = std::fs::rename(&tmp, &path);
    }
}

/// A copy of an unlocked vault, to sync off the UI thread.
fn copy(vault: &Vault) -> Vault {
    Vault::unlock_with_key(&vault.save(), &vault.key_b64()).expect("a vault reopens with its own key")
}

// ---- The service the apps use -------------------------------------------------

/// What's done to items.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Change {
    /// To CleanPassword's Recently Deleted.
    Delete,
    /// Back from Recently Deleted.
    Recover,
    /// For good, from every device (unless another device edits it first:
    /// an edit beats a delete).
    DeleteForever,
}

fn apply(vault: &mut Vault, ids: &[String], change: Change) {
    for id in ids {
        // Gone already (deleted elsewhere too): nothing to do.
        let _ = match change {
            Change::Delete => vault.delete(id),
            Change::Recover => vault.restore(id),
            Change::DeleteForever => vault.purge(id),
        };
    }
}

/// What's going on in the background.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Busy {
    Unlocking,
    Refreshing,
}

/// The vault, as the app sees it.
#[derive(Default)]
pub struct State {
    /// Where the vault is, once found.
    pub found: Option<Found>,
    /// Looking for a vault.
    pub searching: bool,
    pub busy: Option<Busy>,
    /// Why unlocking or syncing the vault last failed.
    pub error: Option<String>,
    /// The vault, while unlocked.
    open: Option<Vault>,
    pub last_refresh: Option<Instant>,
    last_use: Option<Instant>,
    last_search: Option<Instant>,
    /// Bumped by every lock, so a background unlock or sync finishing after
    /// one is thrown away.
    generation: u64,
    /// The access key it was found with (the account's).
    secret: Option<String>,
    /// Changes being saved.
    saving: usize,
    /// The changes made, numbered, so a sync that started before one can
    /// make it again on what it brings back.
    changes: Vec<(u64, Vec<String>, Change)>,
    last_change: u64,
    /// Where the vault is kept between unlocks, if anywhere.
    cache_dir: Option<PathBuf>,
}

impl State {
    pub fn unlocked(&self) -> bool {
        self.open.is_some()
    }

    /// Changes are being saved.
    pub fn saving(&self) -> bool {
        self.saving > 0
    }

    /// The unlocked vault.
    pub fn vault(&self) -> Option<&Vault> {
        self.open.as_ref()
    }

    /// Items not in CleanPassword's Recently Deleted, by name as shown.
    pub fn items(&self) -> Vec<&Item> {
        let mut v = self.open.as_ref().map(|v| v.items()).unwrap_or_default();
        v.sort_by_cached_key(|i| (i.title().to_lowercase(), i.id.clone()));
        v
    }

    /// Items in CleanPassword's Recently Deleted, most recent first.
    pub fn deleted(&self) -> Vec<&Item> {
        self.open.as_ref().map(|v| v.trash()).unwrap_or_default()
    }

    pub fn item(&self, id: &str) -> Option<&Item> {
        self.open.as_ref()?.get(id)
    }

    /// The items' folders and those around them (`Money/Banks` gives
    /// `Money` too), sorted.
    pub fn folders(&self) -> Vec<String> {
        let mut set = std::collections::BTreeSet::new();
        for i in self.items() {
            let mut path = String::new();
            for part in i.folder.split('/').filter(|p| !p.is_empty()) {
                if !path.is_empty() {
                    path.push('/');
                }
                path.push_str(part);
                set.insert(path.clone());
            }
        }
        let mut v: Vec<String> = set.into_iter().collect();
        v.sort_by_key(|f| f.to_lowercase());
        v
    }
}

/// CleanPassword's vault for the app: found, unlocked and synced on
/// background threads; the app reads [`Passwords::state`] each frame.
#[derive(Clone)]
pub struct Passwords {
    state: Arc<Mutex<State>>,
    /// Held by a sync from start to end, so one runs at a time.
    syncing: Arc<Mutex<()>>,
    wake: Arc<dyn Fn() + Send + Sync>,
}

/// An S3 account to look for a vault with: its id, settings and secret.
pub type Candidate = (String, s3::Config, String);

impl Passwords {
    /// `wake` is called when something changed (to redraw).
    pub fn new(wake: impl Fn() + Send + Sync + 'static) -> Passwords {
        Passwords { state: Arc::new(Mutex::new(State::default())), syncing: Arc::new(Mutex::new(())), wake: Arc::new(wake) }
    }

    pub fn state(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Keeps the vault in `dir` between unlocks (as CleanPassword's devices
    /// keep it: sealed with the vault key), so unlocking again downloads
    /// only what changed, and works offline.
    pub fn set_cache_dir(&self, dir: PathBuf) {
        self.state().cache_dir = Some(dir);
    }

    /// Looks for a vault with these accounts' access keys, in the
    /// background. A vault no account can see any more is locked and
    /// forgotten; one that can't be looked for (offline) is kept.
    pub fn search(&self, candidates: Vec<Candidate>) {
        {
            let mut s = self.state();
            if s.searching {
                return;
            }
            s.searching = true;
            s.last_search = Some(Instant::now());
        }
        let this = self.clone();
        std::thread::spawn(move || {
            let mut result: Option<(Found, String)> = None;
            let mut failed = false;
            for (account, config, secret) in &candidates {
                match find(account, config, secret) {
                    Ok(Some(f)) => {
                        result = Some((f, secret.clone()));
                        break;
                    }
                    Ok(None) => {}
                    Err(_) => failed = true,
                }
            }
            let mut s = this.state();
            s.searching = false;
            match result {
                Some((f, secret)) => {
                    let same = s.found.as_ref().is_some_and(|o| o.config == f.config && o.header.vault_id == f.header.vault_id);
                    if !same {
                        Self::close(&mut s);
                    }
                    s.found = Some(f);
                    s.secret = Some(secret);
                }
                None if !failed => {
                    Self::close(&mut s);
                    s.found = None;
                    s.secret = None;
                }
                None => {}
            }
            drop(s);
            (this.wake)();
        });
    }

    /// When the last search started.
    pub fn last_search(&self) -> Option<Instant> {
        self.state().last_search
    }

    /// Unlocks the vault with the master password and syncs it, in the
    /// background ([`State::busy`], then [`State::unlocked`] or
    /// [`State::error`]).
    pub fn unlock(&self, password: String) {
        let password = Zeroizing::new(password);
        let (found, secret, generation, dir) = {
            let mut s = self.state();
            let (Some(f), Some(secret)) = (s.found.clone(), s.secret.clone()) else { return };
            if s.busy.is_some() || s.open.is_some() {
                return;
            }
            s.busy = Some(Busy::Unlocking);
            s.error = None;
            (f, secret, s.generation, s.cache_dir.clone())
        };
        let this = self.clone();
        std::thread::spawn(move || {
            let r = open_vault(&found, &secret, &password, dir.as_deref());
            drop(password);
            let mut s = this.state();
            if s.generation == generation {
                s.busy = None;
                match r {
                    Ok((vault, error)) => {
                        if let Some(dir) = &dir {
                            save_cache(dir, &vault);
                        }
                        s.open = Some(vault);
                        s.error = error;
                        s.last_refresh = Some(Instant::now());
                        s.last_use = Some(Instant::now());
                    }
                    Err(e) => s.error = Some(e),
                }
            }
            drop(s);
            (this.wake)();
        });
    }

    /// Syncs the unlocked vault, in the background: a `HEAD` of `vault.bin`
    /// if nothing changed, else what another device saved, merged.
    pub fn refresh(&self) {
        let generation = {
            let mut s = self.state();
            if s.busy.is_some() || s.open.is_none() {
                return;
            }
            s.busy = Some(Busy::Refreshing);
            s.last_refresh = Some(Instant::now());
            s.generation
        };
        let this = self.clone();
        std::thread::spawn(move || {
            this.sync(generation, false);
            let mut s = this.state();
            if s.generation == generation {
                s.busy = None;
            }
            drop(s);
            (this.wake)();
        });
    }

    /// Deletes, recovers or deletes for good the items: shown so at once,
    /// then saved to the vault in the background, as CleanPassword saves
    /// (merged with what other devices saved meanwhile). A change that
    /// can't be saved now (offline) stays on this computer and is tried
    /// again at the next sync, with [`State::error`] saying so.
    pub fn change(&self, ids: Vec<String>, change: Change) {
        let generation = {
            let mut s = self.state();
            let s = &mut *s;
            let Some(vault) = s.open.as_mut() else { return };
            apply(vault, &ids, change);
            s.last_change += 1;
            s.changes.push((s.last_change, ids, change));
            s.saving += 1;
            s.error = None;
            s.last_use = Some(Instant::now());
            s.generation
        };
        let this = self.clone();
        (self.wake)();
        std::thread::spawn(move || {
            this.sync(generation, true);
            let mut s = this.state();
            if s.generation == generation {
                s.saving -= 1;
            }
            drop(s);
            (this.wake)();
        });
    }

    /// Syncs a copy of the vault (one sync at a time), then shows it, with
    /// the changes made meanwhile made again on it.
    fn sync(&self, generation: u64, after_change: bool) {
        let _one_at_a_time = self.syncing.lock().unwrap_or_else(|e| e.into_inner());
        let (mut vault, from) = {
            let s = self.state();
            if s.generation != generation {
                return;
            }
            let Some(v) = s.open.as_ref() else { return };
            (copy(v), s.last_change)
        };
        let r = block_on(vault.sync(&Blocking::new()));
        let mut s = self.state();
        let s = &mut *s;
        if s.generation != generation {
            return;
        }
        // Purged here but edited on another device: the edit won.
        let kept = s
            .changes
            .iter()
            .filter(|(n, _, c)| *n <= from && *c == Change::DeleteForever)
            .flat_map(|(_, ids, _)| ids)
            .filter(|id| vault.get(id).is_some())
            .count();
        for (_, ids, change) in s.changes.iter().filter(|(n, _, _)| *n > from) {
            apply(&mut vault, ids, *change);
        }
        s.changes.retain(|(n, _, _)| *n > from);
        if let Some(dir) = &s.cache_dir {
            save_cache(dir, &vault);
        }
        s.open = Some(vault);
        s.last_refresh = Some(Instant::now());
        s.error = match r {
            Err(e) if after_change => Some(format!("Couldn't save the change to the vault (it's tried again in a minute): {}", message(&e))),
            Err(e) => Some(format!("Couldn't sync the vault: {}", message(&e))),
            Ok(SyncReport { pending: n, .. }) if n > 0 => {
                Some("Other devices kept saving the vault meanwhile; the change is tried again in a minute.".into())
            }
            Ok(_) if kept == 1 => Some("1 item was changed elsewhere meanwhile, so it was kept.".into()),
            Ok(_) if kept > 1 => Some(format!("{kept} items were changed elsewhere meanwhile, so they were kept.")),
            Ok(_) => None,
        };
    }

    /// For the apps' tests: a vault found at `found` and, with `vault`,
    /// unlocked (no S3 involved).
    #[doc(hidden)]
    pub fn set_for_test(&self, found: Found, vault: Option<Vault>) {
        let mut s = self.state();
        s.found = Some(found);
        s.secret = Some(String::new());
        s.open = vault;
    }

    /// Locks the vault: the vault, its key and the access key in it are
    /// dropped (the keys wiped).
    pub fn lock(&self) {
        Self::close(&mut self.state());
    }

    fn close(s: &mut State) {
        s.open = None;
        s.busy = None;
        s.error = None;
        s.saving = 0;
        s.changes.clear();
        s.generation += 1;
    }

    /// The user did something with the vault: it stays unlocked another
    /// while.
    pub fn touch(&self) {
        let mut s = self.state();
        if s.open.is_some() {
            s.last_use = Some(Instant::now());
        }
    }

    /// Call now and then: locks the vault after [`AUTO_LOCK`] without use
    /// (returns true then), and syncs it every [`REFRESH_EVERY`].
    pub fn tick(&self) -> bool {
        let (idle, stale) = {
            let s = self.state();
            if s.open.is_none() {
                return false;
            }
            (s.last_use.is_some_and(|u| u.elapsed() > AUTO_LOCK), s.last_refresh.is_none_or(|r| r.elapsed() > REFRESH_EVERY))
        };
        if idle {
            self.lock();
            return true;
        }
        if stale && !self.state().saving() {
            self.refresh();
        }
        false
    }
}

/// Unlocks the vault at `found` and syncs it: from what this computer kept
/// (`dir`), if anything, else by joining it as a new device. With what was
/// kept, a failed sync still opens it, with the error to show.
fn open_vault(found: &Found, secret: &str, password: &str, dir: Option<&Path>) -> Result<(Vault, Option<String>), String> {
    let config = found.vault_config();
    let creds = credentials(secret).map_err(|e| capitalized(format!("{e:#}")))?;
    let http = Blocking::new();
    // vault.json as it is now (the master password may have changed);
    // offline, as it was found.
    let (header, etag) = match block_on(fetch_header(&http, &config, &creds)) {
        Ok(Some(h)) => h,
        Ok(None) => return Err("The vault is no longer in the bucket.".into()),
        Err(_) => (found.header.clone(), found.header_etag.clone()),
    };
    if header.vault_id != found.header.vault_id {
        return Err("The bucket holds another vault now.".into());
    }
    let kept = dir.and_then(|d| std::fs::read_to_string(cache_path(d, &header.vault_id)?).ok());
    if let Some(kept) = kept {
        let key = header.unwrap_key(password).map_err(|e| message(&e))?;
        let key_b64 = Zeroizing::new(cleanpassword_core::crypto::b64(key.as_ref()));
        if let Ok(mut vault) = Vault::unlock_with_key(&kept, &key_b64) {
            if *vault.config() == config && vault.vault_id() == header.vault_id {
                // The account's access key may have changed since.
                vault.set_credentials(creds).map_err(|e| message(&e))?;
                let error = block_on(vault.sync(&http))
                    .err()
                    .map(|e| format!("Couldn't sync the vault (showing what this computer kept): {}", message(&e)));
                return Ok((vault, error));
            }
        }
    }
    let mut vault = Vault::join(config, creds, header, etag, password).map_err(|e| message(&e))?;
    block_on(vault.sync(&http)).map_err(|e| message(&e))?;
    Ok((vault, None))
}

/// An error as shown under the password field.
fn message(e: &VaultError) -> String {
    match e {
        VaultError::WrongPassword => "That isn't the master password.".into(),
        e => capitalized(e.to_string()),
    }
}

fn capitalized(s: String) -> String {
    let mut c = s.chars();
    match c.next() {
        Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
        None => s,
    }
}
