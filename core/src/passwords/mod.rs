//! CleanPassword's vault, shown read-only in CleanNotes when it's kept in
//! the same S3 bucket as an account's notes.
//!
//! [CleanPassword](../../CleanPassword) is the password manager whose vault
//! lives in an S3 folder of the user's own, end-to-end encrypted:
//!
//! ```text
//! <folder>/vault.json         the vault's id, Argon2id settings and salt, and
//!                             the vault key sealed with the master key
//! <folder>/items/<id>.bin     one item: "CPV1" ‖ XChaCha20-Poly1305 sealed
//!                             JSON (space padded), bound to the vault and id
//! ```
//!
//! It may share the notes' folder (CleanPassword's README suggests it; the
//! two apps' objects don't overlap) or be in its own folder of the bucket,
//! `CleanPassword` by default. [`find`] looks in both with the account's
//! access key. Nothing is written: CleanNotes only reads the vault, so
//! CleanPassword's sync and merging stay CleanPassword's alone.
//!
//! The master password unlocks the vault key (Argon2id, then
//! XChaCha20-Poly1305), and that opens the items. Neither the password nor
//! the key is saved; they're kept in memory until [`Passwords::lock`] (Lock
//! Now, 15 minutes without use, or quitting), and wiped then.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use argon2::{Algorithm, Argon2, Params, Version};
use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine;
use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};
use chrono::{DateTime, Utc};
use serde::Deserialize;
use zeroize::{Zeroize, Zeroizing};

use crate::sync::s3::{self, Keys, S3Backend};

pub mod totp;
#[cfg(test)]
mod tests;

/// The vault's header in its folder.
pub const VAULT_OBJECT: &str = "vault.json";
/// Where the items are, in its folder.
pub const ITEMS: &str = "items/";
/// CleanPassword's own folder, when it doesn't share the notes'.
pub const DEFAULT_FOLDER: &str = "CleanPassword";
/// The vault locks after this long without use.
pub const AUTO_LOCK: Duration = Duration::from_secs(15 * 60);
/// An unlocked vault is read again this often while the app runs.
pub const REFRESH_EVERY: Duration = Duration::from_secs(5 * 60);

const VAULT_FORMAT: &str = "cleanpassword-vault";
/// Sealed items start with this, then `nonce ‖ ciphertext ‖ tag`.
const MAGIC: &[u8] = b"CPV1";
const NONCE_LEN: usize = 24;
const TAG_LEN: usize = 16;
/// Downloads running at once.
const PARALLEL: usize = 16;
/// Argon2 memory the vault may ask for (KiB): CleanPassword's own limits, so
/// a damaged or hostile `vault.json` can neither make unlocking cheap to
/// attack nor make this computer allocate gigabytes.
const MIN_MEMORY_KIB: u32 = if cfg!(test) { 8 } else { 19 * 1024 };
const MAX_MEMORY_KIB: u32 = 1024 * 1024;

/// A 256-bit key, wiped when dropped.
pub type Key = Zeroizing<[u8; 32]>;

// ---- The format -------------------------------------------------------------

/// How the master key is made from the master password.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct KdfParams {
    pub alg: String,
    pub memory_kib: u32,
    pub passes: u32,
    pub lanes: u32,
    /// Base64.
    pub salt: String,
}

/// `vault.json`: what's needed to unlock the vault with the master password.
/// Not secret on its own.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct VaultHeader {
    pub format: String,
    pub version: u32,
    pub vault_id: String,
    pub kdf: KdfParams,
    /// The vault key sealed with the master key (base64).
    pub wrapped_key: String,
    pub created: DateTime<Utc>,
}

/// The master password doesn't open the vault.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WrongPassword;

impl std::fmt::Display for WrongPassword {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("That isn't the master password.")
    }
}

impl std::error::Error for WrongPassword {}

impl VaultHeader {
    /// `vault.json`'s contents, if it's a vault this version can open.
    pub fn parse(data: &[u8]) -> Result<VaultHeader> {
        let h: VaultHeader = serde_json::from_slice(data).context("the vault's vault.json is damaged")?;
        if h.format != VAULT_FORMAT {
            bail!("this vault.json isn't a CleanPassword vault");
        }
        if h.version != 1 {
            bail!("this vault was made by a newer version of CleanPassword; update CleanNotes");
        }
        Ok(h)
    }

    /// The vault key, if `password` is the master password ([`WrongPassword`]
    /// inside the error if it isn't). Slow on purpose (Argon2id): call it
    /// off the UI thread.
    pub fn unwrap_key(&self, password: &str) -> Result<Key> {
        let p = &self.kdf;
        if p.alg != "argon2id" {
            bail!("this vault uses {}, which this version of CleanNotes doesn't know", p.alg);
        }
        if !(MIN_MEMORY_KIB..=MAX_MEMORY_KIB).contains(&p.memory_kib) || !(1..=32).contains(&p.passes) || !(1..=16).contains(&p.lanes) {
            bail!("the vault's key settings are out of the allowed range");
        }
        let salt = B64.decode(p.salt.trim()).context("the vault's salt is damaged")?;
        if salt.len() < 16 {
            bail!("the vault's salt is too short");
        }
        let params = Params::new(p.memory_kib, p.passes, p.lanes, Some(32)).map_err(|e| anyhow::anyhow!("argon2: {e}"))?;
        let mut master: Key = Zeroizing::new([0u8; 32]);
        Argon2::new(Algorithm::Argon2id, Version::V0x13, params)
            .hash_password_into(password.as_bytes(), &salt, master.as_mut())
            .map_err(|e| anyhow::anyhow!("argon2: {e}"))?;
        let wrapped = B64.decode(self.wrapped_key.trim()).context("the vault's key is damaged")?;
        let aad = format!("cleanpassword/vault-key/v1/{}", self.vault_id);
        let key = open(&master, aad.as_bytes(), &wrapped).ok_or(WrongPassword)?;
        let arr: [u8; 32] = key.as_slice().try_into().map_err(|_| anyhow::anyhow!("the vault's key is damaged"))?;
        Ok(Zeroizing::new(arr))
    }
}

/// `nonce ‖ ciphertext ‖ tag` opened, or `None` if it wasn't sealed with
/// this key and `aad`, or was changed.
fn open(key: &[u8; 32], aad: &[u8], sealed: &[u8]) -> Option<Zeroizing<Vec<u8>>> {
    if sealed.len() < NONCE_LEN + TAG_LEN {
        return None;
    }
    let (nonce, ct) = sealed.split_at(NONCE_LEN);
    XChaCha20Poly1305::new(key.into()).decrypt(XNonce::from_slice(nonce), Payload { msg: ct, aad }).ok().map(Zeroizing::new)
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ItemKind {
    #[default]
    Login,
    /// A secure note (also what cards, identities and SSH keys imported
    /// into CleanPassword become, with their details as fields).
    Note,
    /// A kind a newer CleanPassword has.
    #[serde(other)]
    Other,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct ItemUri {
    pub uri: String,
}

/// A custom field.
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct Field {
    pub name: String,
    pub value: String,
    /// Shown as dots until revealed (a PIN, a security answer).
    pub hidden: bool,
}

/// A password the item had before.
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct HistoryEntry {
    pub password: String,
    /// When it stopped being the password.
    pub retired: Option<DateTime<Utc>>,
}

/// A passkey, as far as it's shown: its site and account. (Its private key
/// isn't even read.)
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct Passkey {
    pub rp_id: String,
    pub user_name: String,
    pub user_display_name: String,
    pub created: Option<DateTime<Utc>>,
}

/// A vault item: a login or a secure note. Fields this version doesn't
/// know are ignored.
#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(default)]
pub struct Item {
    pub id: String,
    pub kind: ItemKind,
    pub name: String,
    pub username: String,
    pub password: String,
    pub uris: Vec<ItemUri>,
    /// A TOTP secret: base32, an `otpauth://` URI, or `steam://…` (see
    /// [`totp`]).
    pub totp: String,
    pub notes: String,
    /// A folder name; nested folders are written `Work/Email`.
    pub folder: String,
    pub favorite: bool,
    pub fields: Vec<Field>,
    pub history: Vec<HistoryEntry>,
    pub passkeys: Vec<Passkey>,
    pub created: Option<DateTime<Utc>>,
    pub modified: Option<DateTime<Utc>>,
    pub password_modified: Option<DateTime<Utc>>,
    /// In CleanPassword's Recently Deleted since then.
    pub deleted: Option<DateTime<Utc>>,
}

impl Item {
    /// The name shown: the item's, or its first website's host, or its
    /// user name.
    pub fn title(&self) -> String {
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

    /// Whether every word of `query` is in its name, user name, websites,
    /// folder, notes or field names.
    pub fn matches(&self, query: &str) -> bool {
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

    /// Whether it's in `folder` or a folder inside it.
    pub fn in_folder(&self, folder: &str) -> bool {
        self.folder == folder || self.folder.strip_prefix(folder).is_some_and(|r| r.starts_with('/'))
    }

    fn wipe(&mut self) {
        self.password.zeroize();
        self.totp.zeroize();
        self.notes.zeroize();
        self.username.zeroize();
        for f in &mut self.fields {
            f.value.zeroize();
        }
        for h in &mut self.history {
            h.password.zeroize();
        }
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

/// What an item object holds.
#[derive(Deserialize)]
#[serde(rename_all = "lowercase")]
enum Stored {
    Item(Box<Item>),
    /// Deleted for good; kept a while so other devices delete it too.
    Tombstone { id: String },
}

/// The item id in an object's name inside the folder, if it's an item.
fn item_id(name: &str) -> Option<&str> {
    let id = name.strip_prefix(ITEMS)?.strip_suffix(".bin")?;
    (!id.is_empty() && id.len() <= 64 && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')).then_some(id)
}

/// An item object opened: `Some(None)` for a tombstone, `None` if it can't
/// be (another vault's, damaged, or a format this version doesn't know).
fn open_item(key: &[u8; 32], vault_id: &str, id: &str, data: &[u8]) -> Option<Option<Item>> {
    let aad = format!("cleanpassword/item/v1/{vault_id}/{id}");
    let plain = open(key, aad.as_bytes(), data.strip_prefix(MAGIC)?)?;
    let stored: Stored = serde_json::from_slice(&plain).ok()?;
    // The id inside must be the one in the name (the AAD binds them too).
    match stored {
        Stored::Item(i) if i.id == id => Some(Some(*i)),
        Stored::Tombstone { id: t } if t == id => Some(None),
        _ => None,
    }
}

// ---- Reading it from S3 -----------------------------------------------------

/// Where a vault was found.
#[derive(Clone, Debug, PartialEq)]
pub struct Found {
    /// The CleanNotes account whose access key reads it.
    pub account: String,
    /// The bucket, with the vault's folder as its prefix.
    pub config: s3::Config,
    pub header: VaultHeader,
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
}

/// The access key of a CleanNotes S3 account, for reading the bucket in
/// the clear (the vault has its own encryption).
fn plain_backend(config: &s3::Config, secret: &str, folder: &str) -> Result<S3Backend> {
    let keys: Keys = serde_json::from_str(secret).context("the saved access key is unreadable")?;
    let keys = Keys { encryption_key: None, ..keys };
    let config = s3::Config { prefix: folder.to_owned(), encrypted: false, ..config.clone() };
    S3Backend::open(&config, Some(keys.to_secret()))
}

/// Looks for a vault with an S3 account's access key: in the notes'
/// folder, then in [`DEFAULT_FOLDER`]. `Ok(None)` if neither has one; an
/// error only if neither could be looked in (offline, say).
pub fn find(account: &str, config: &s3::Config, secret: &str) -> Result<Option<Found>> {
    let notes = config.prefix.trim().trim_matches('/').to_owned();
    let mut folders = vec![notes.clone()];
    if notes != DEFAULT_FOLDER {
        folders.push(DEFAULT_FOLDER.to_owned());
    }
    let mut first_error = None;
    let mut looked = false;
    for folder in folders {
        let attempt = plain_backend(config, secret, &folder).and_then(|b| b.get_bytes(VAULT_OBJECT));
        match attempt {
            Ok(Some((data, _))) => {
                // Something else's vault.json is no vault: not found here.
                if let Ok(header) = VaultHeader::parse(&data) {
                    let config = s3::Config { prefix: folder, encrypted: false, ..config.clone() };
                    return Ok(Some(Found { account: account.to_owned(), config, header }));
                }
                looked = true;
            }
            Ok(None) => looked = true,
            Err(e) => {
                first_error.get_or_insert(e);
            }
        }
    }
    match first_error {
        Some(e) if !looked => Err(e),
        _ => Ok(None),
    }
}

/// An unlocked vault's items, and their objects' ETags (so reading it again
/// downloads only what changed).
#[derive(Default)]
pub struct Contents {
    /// Item id → (ETag, the item; `None` for a tombstone).
    objects: BTreeMap<String, (String, Option<Item>)>,
    /// Objects that couldn't be opened.
    pub unreadable: usize,
}

impl Contents {
    /// Every item, in CleanPassword's Recently Deleted or not.
    pub fn all(&self) -> impl Iterator<Item = &Item> {
        self.objects.values().filter_map(|(_, i)| i.as_ref())
    }

    pub fn get(&self, id: &str) -> Option<&Item> {
        self.objects.get(id).and_then(|(_, i)| i.as_ref())
    }
}

impl Drop for Contents {
    fn drop(&mut self) {
        for (_, item) in self.objects.values_mut() {
            if let Some(i) = item {
                i.wipe();
            }
        }
    }
}

/// Reads the vault's items, downloading only those whose ETag isn't the
/// one in `old`. Fails if the folder's vault isn't `vault_id` any more.
pub fn fetch(found: &Found, secret: &str, key: &[u8; 32], old: &Contents) -> Result<Contents> {
    let b = plain_backend(&found.config, secret, &found.config.prefix)?;
    let header = b.get_bytes(VAULT_OBJECT)?.context("the vault is no longer in the bucket")?;
    if VaultHeader::parse(&header.0)?.vault_id != found.header.vault_id {
        bail!("the bucket holds another vault now");
    }
    let listed: Vec<(String, String)> =
        b.list_names(ITEMS)?.into_iter().filter_map(|(name, etag)| Some((item_id(&name)?.to_owned(), etag))).collect();
    let mut out = Contents::default();
    let mut wanted = Vec::new();
    for (id, etag) in listed {
        match old.objects.get(&id) {
            Some((e, item)) if *e == etag => {
                out.objects.insert(id, (etag, item.clone()));
            }
            _ => wanted.push((id, etag)),
        }
    }
    // Several at once, as the notes' sync does.
    let next = Mutex::new(0usize);
    // (id, ETag, its bytes)
    type Downloaded = (String, String, Result<Option<Vec<u8>>>);
    let results: Mutex<Vec<Downloaded>> = Mutex::new(Vec::new());
    std::thread::scope(|s| {
        for _ in 0..PARALLEL.min(wanted.len()) {
            s.spawn(|| loop {
                let i = {
                    let mut n = next.lock().unwrap();
                    *n += 1;
                    *n - 1
                };
                let Some((id, etag)) = wanted.get(i) else { return };
                let r = b.get_bytes(&format!("{ITEMS}{id}.bin")).map(|o| o.map(|(d, _)| d));
                results.lock().unwrap().push((id.clone(), etag.clone(), r));
            });
        }
    });
    for (id, etag, r) in results.into_inner().unwrap() {
        // Deleted between the listing and the download: gone.
        let Some(data) = r? else { continue };
        match open_item(key, &found.header.vault_id, &id, &data) {
            Some(item) => {
                out.objects.insert(id, (etag, item));
            }
            None => out.unreadable += 1,
        }
    }
    Ok(out)
}

// ---- The service the apps use -------------------------------------------------

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
    /// Why unlocking or reading the vault last failed.
    pub error: Option<String>,
    /// The vault key and items, while unlocked.
    open: Option<(Key, Contents)>,
    pub last_refresh: Option<Instant>,
    last_use: Option<Instant>,
    last_search: Option<Instant>,
    /// Bumped by every lock, so a background unlock or refresh finishing
    /// after one is thrown away.
    generation: u64,
    /// The access key it was found with (the account's).
    secret: Option<String>,
}

impl State {
    pub fn unlocked(&self) -> bool {
        self.open.is_some()
    }

    pub fn contents(&self) -> Option<&Contents> {
        self.open.as_ref().map(|(_, c)| c)
    }

    /// Items not in CleanPassword's Recently Deleted, by name.
    pub fn items(&self) -> Vec<&Item> {
        let mut v: Vec<&Item> = self.contents().map(|c| c.all().filter(|i| i.deleted.is_none()).collect()).unwrap_or_default();
        v.sort_by_cached_key(|i| (i.title().to_lowercase(), i.id.clone()));
        v
    }

    /// Items in CleanPassword's Recently Deleted, most recent first.
    pub fn deleted(&self) -> Vec<&Item> {
        let mut v: Vec<&Item> = self.contents().map(|c| c.all().filter(|i| i.deleted.is_some()).collect()).unwrap_or_default();
        v.sort_by_key(|i| std::cmp::Reverse(i.deleted));
        v
    }

    pub fn item(&self, id: &str) -> Option<&Item> {
        self.contents()?.get(id)
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

/// CleanPassword's vault for the app: found, unlocked and read on
/// background threads; the app reads [`Passwords::state`] each frame.
#[derive(Clone)]
pub struct Passwords {
    state: Arc<Mutex<State>>,
    wake: Arc<dyn Fn() + Send + Sync>,
}

/// An S3 account to look for a vault with: its id, settings and secret.
pub type Candidate = (String, s3::Config, String);

impl Passwords {
    /// `wake` is called when something changed (to redraw).
    pub fn new(wake: impl Fn() + Send + Sync + 'static) -> Passwords {
        Passwords { state: Arc::new(Mutex::new(State::default())), wake: Arc::new(wake) }
    }

    pub fn state(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap()
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

    /// Unlocks the vault with the master password and reads its items, in
    /// the background ([`State::busy`], then [`State::unlocked`] or
    /// [`State::error`]).
    pub fn unlock(&self, password: String) {
        let password = Zeroizing::new(password);
        let (found, secret, generation) = {
            let mut s = self.state();
            let (Some(f), Some(secret)) = (s.found.clone(), s.secret.clone()) else { return };
            if s.busy.is_some() || s.open.is_some() {
                return;
            }
            s.busy = Some(Busy::Unlocking);
            s.error = None;
            (f, secret, s.generation)
        };
        let this = self.clone();
        std::thread::spawn(move || {
            let r = found.header.unwrap_key(&password).and_then(|key| {
                let c = fetch(&found, &secret, &key, &Contents::default())?;
                Ok((key, c))
            });
            let mut s = this.state();
            if s.generation == generation {
                s.busy = None;
                match r {
                    Ok(open) => {
                        s.open = Some(open);
                        s.last_refresh = Some(Instant::now());
                        s.last_use = Some(Instant::now());
                    }
                    Err(e) => s.error = Some(message(&e)),
                }
            }
            drop(s);
            (this.wake)();
        });
    }

    /// Reads the unlocked vault again (only what changed), in the
    /// background.
    pub fn refresh(&self) {
        let (found, secret, key, old, generation) = {
            let mut s = self.state();
            let (Some(f), Some(secret)) = (s.found.clone(), s.secret.clone()) else { return };
            if s.busy.is_some() {
                return;
            }
            let Some((key, contents)) = s.open.as_ref() else { return };
            let key = key.clone();
            // What's unchanged is kept rather than downloaded again.
            let old = Contents { objects: contents.objects.clone(), unreadable: 0 };
            s.busy = Some(Busy::Refreshing);
            s.last_refresh = Some(Instant::now());
            (f, secret, key, old, s.generation)
        };
        let this = self.clone();
        std::thread::spawn(move || {
            let r = fetch(&found, &secret, &key, &old);
            drop(old);
            let mut s = this.state();
            if s.generation == generation {
                s.busy = None;
                match r {
                    Ok(c) => {
                        s.error = None;
                        if let Some(open) = s.open.as_mut() {
                            open.1 = c;
                        }
                    }
                    Err(e) => s.error = Some(message(&e)),
                }
            }
            drop(s);
            (this.wake)();
        });
    }

    /// For the apps' tests: a vault found at `found` and, with `items`,
    /// unlocked holding them (no S3 involved).
    #[doc(hidden)]
    pub fn set_for_test(&self, found: Found, items: Option<Vec<Item>>) {
        let mut s = self.state();
        s.found = Some(found);
        s.secret = Some(String::new());
        s.open = items.map(|items| {
            let objects = items.into_iter().map(|i| (i.id.clone(), (String::new(), Some(i)))).collect();
            (Zeroizing::new([0u8; 32]), Contents { objects, unreadable: 0 })
        });
    }

    /// Locks the vault: the key and items are wiped from memory.
    pub fn lock(&self) {
        Self::close(&mut self.state());
    }

    fn close(s: &mut State) {
        s.open = None;
        s.busy = None;
        s.error = None;
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
    /// (returns true then), and reads it again every [`REFRESH_EVERY`].
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
        if stale {
            self.refresh();
        }
        false
    }
}

/// An error as shown under the password field.
fn message(e: &anyhow::Error) -> String {
    if e.is::<WrongPassword>() {
        return WrongPassword.to_string();
    }
    let s = format!("{e:#}");
    let mut c = s.chars();
    match c.next() {
        Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
        None => s,
    }
}
