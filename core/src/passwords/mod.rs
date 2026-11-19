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
//! access key.
//!
//! CleanNotes shows the items, and deletes, recovers and deletes for good
//! ([`apply`]), writing what CleanPassword itself writes, so its devices
//! merge it like their own: `deleted` and `modified` set on the item, or a
//! tombstone. Each change starts from the object as it is in S3 now and
//! touches only those fields of its JSON (fields this version doesn't know,
//! passkeys' keys, stay as they are), and is written with `If-Match`, so
//! an edit made meanwhile is never overwritten.
//!
//! The master password unlocks the vault key (Argon2id, then
//! XChaCha20-Poly1305), and that opens the items. Neither the password nor
//! the key is saved; they're kept in memory until [`Passwords::lock`] (Lock
//! Now, 15 minutes without use, or quitting), and wiped then.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine;
use chrono::{DateTime, Utc};
use cleanbucket::block_on;
use cleanbucket::kdf::{self, Cost};
use cleanbucket::s3::{Client, Cond, Credentials, Location, Object, Put};
use cleanbucket::seal as sealing;
use serde::{Deserialize, Serialize};
use zeroize::{Zeroize, Zeroizing};

use crate::sync::s3::{self, Blocking, Keys};

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
/// Rewritten by every device after it changes the vault (CleanPassword's
/// and CleanNotes'), so one `HEAD` tells whether anything changed.
pub const HEAD_OBJECT: &str = "vault-head";
/// An unlocked vault is checked for changes this often while the app runs
/// (one `HEAD` when nothing changed).
pub const REFRESH_EVERY: Duration = Duration::from_secs(60);
/// The folder is listed in full at least this often anyway: a device can
/// stop between writing an item and [`HEAD_OBJECT`], and older versions of
/// CleanPassword don't write it.
pub const FULL_LIST_EVERY: Duration = Duration::from_secs(15 * 60);

const VAULT_FORMAT: &str = "cleanpassword-vault";
/// Sealed items start with this, then `nonce ‖ ciphertext ‖ tag`.
const MAGIC: &[u8] = b"CPV1";
/// Downloads running at once.
const PARALLEL: usize = 16;
/// Argon2 memory the vault may ask for (KiB), as CleanPassword allows, so a
/// damaged or hostile `vault.json` can neither make unlocking cheap to
/// attack nor make this computer allocate gigabytes (tests use tiny costs).
const MIN_MEMORY_KIB: u32 = if cfg!(test) { 8 } else { kdf::MIN_MEMORY_KIB };

/// A 256-bit key, wiped when dropped.
pub type Key = cleanbucket::seal::Key;

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
        let cost = Cost { memory_kib: p.memory_kib, passes: p.passes, lanes: p.lanes };
        cost.check(MIN_MEMORY_KIB).context("the vault's key settings")?;
        let salt = B64.decode(p.salt.trim()).context("the vault's salt is damaged")?;
        let master = kdf::derive(password.as_bytes(), &salt, cost).context("the vault's salt")?;
        let wrapped = B64.decode(self.wrapped_key.trim()).context("the vault's key is damaged")?;
        let aad = format!("cleanpassword/vault-key/v1/{}", self.vault_id);
        let key = sealing::open(&master, aad.as_bytes(), &wrapped).ok_or(WrongPassword)?;
        let arr: [u8; 32] = key.as_slice().try_into().map_err(|_| anyhow::anyhow!("the vault's key is damaged"))?;
        Ok(Zeroizing::new(arr))
    }
}


#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
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

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ItemUri {
    pub uri: String,
}

/// A custom field.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Field {
    pub name: String,
    pub value: String,
    /// Shown as dots until revealed (a PIN, a security answer).
    pub hidden: bool,
}

/// A password the item had before.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct HistoryEntry {
    pub password: String,
    /// When it stopped being the password.
    pub retired: Option<DateTime<Utc>>,
}

/// A passkey, as far as it's shown: its site and account. (Its private key
/// isn't even read.)
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Passkey {
    pub rp_id: String,
    pub user_name: String,
    pub user_display_name: String,
    pub created: Option<DateTime<Utc>>,
}

/// A vault item: a login or a secure note. Fields this version doesn't
/// know are ignored.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
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
    let plain = sealing::open(key, aad.as_bytes(), data.strip_prefix(MAGIC)?)?;
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


/// The vault's folder, read and written in the clear with an S3 account's
/// access key (the vault has its own encryption), through the client
/// CleanPassword shares.
struct Access {
    location: Location,
    credentials: Credentials,
    http: Blocking,
}

impl Access {
    fn new(config: &s3::Config, secret: &str, folder: &str) -> Result<Access> {
        let keys: Keys = serde_json::from_str(secret).context("the saved access key is unreadable")?;
        let location = Location { bucket: config.bucket.clone(), region: config.region.clone(), prefix: folder.to_owned(), endpoint: config.endpoint.clone() };
        let credentials = Credentials { access_key_id: keys.access_key_id.clone(), secret_access_key: keys.secret_access_key.clone(), session_token: None };
        Ok(Access { location, credentials, http: Blocking::new() })
    }

    fn client(&self) -> Client<'_, Blocking> {
        Client::new(&self.location, &self.credentials, &self.http)
    }

    fn get(&self, name: &str) -> Result<Option<Object>> {
        Ok(block_on(self.client().get(name))?)
    }
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
        match Access::new(config, secret, &folder).and_then(|a| a.get(VAULT_OBJECT)) {
            Ok(Some(obj)) => {
                // Something else's vault.json is no vault: not found here.
                if let Ok(header) = VaultHeader::parse(&obj.data) {
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

/// An unlocked vault's items, their objects' ETags (so reading it again
/// downloads only what changed), and what [`HEAD_OBJECT`] said then.
#[derive(Default, Serialize, Deserialize)]
pub struct Contents {
    /// Item id → (ETag, the item; `None` for a tombstone).
    objects: BTreeMap<String, (String, Option<Item>)>,
    /// Objects that couldn't be opened.
    #[serde(skip)]
    pub unreadable: usize,
    /// [`HEAD_OBJECT`]'s ETag when the folder was last listed.
    #[serde(default)]
    cursor: Option<String>,
    /// When the folder was last listed in full.
    #[serde(default)]
    listed_at: Option<DateTime<Utc>>,
}

impl Contents {
    /// Every item, in CleanPassword's Recently Deleted or not.
    pub fn all(&self) -> impl Iterator<Item = &Item> {
        self.objects.values().filter_map(|(_, i)| i.as_ref())
    }

    pub fn get(&self, id: &str) -> Option<&Item> {
        self.objects.get(id).and_then(|(_, i)| i.as_ref())
    }

    /// A copy to read again from: what's unchanged is kept rather than
    /// downloaded again.
    fn copy(&self) -> Contents {
        Contents { objects: self.objects.clone(), unreadable: 0, cursor: self.cursor.clone(), listed_at: self.listed_at }
    }

    /// Listed in full long enough ago that it's due again.
    fn list_due(&self) -> bool {
        self.listed_at.is_none_or(|t| Utc::now() - t > chrono::Duration::from_std(FULL_LIST_EVERY).expect("in range"))
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
/// one in `old`. First, unless `list_anyway`, one `HEAD` of [`HEAD_OBJECT`]:
/// if it's what it was at the last listing, nothing changed and `None` is
/// returned. Fails if the folder's vault isn't `vault_id` any more.
pub fn fetch(found: &Found, secret: &str, key: &[u8; 32], old: &Contents, list_anyway: bool) -> Result<Option<Contents>> {
    let a = Access::new(&found.config, secret, &found.config.prefix)?;
    let client = a.client();
    // Read before listing: a write landing after this is seen next time.
    let cursor = block_on(client.head(HEAD_OBJECT))?;
    if !list_anyway && !old.list_due() && cursor.is_some() && cursor == old.cursor {
        return Ok(None);
    }
    let header = a.get(VAULT_OBJECT)?.context("the vault is no longer in the bucket")?;
    if VaultHeader::parse(&header.data)?.vault_id != found.header.vault_id {
        bail!("the bucket holds another vault now");
    }
    let listed: Vec<(String, String)> =
        block_on(client.list(ITEMS))?.into_iter().filter_map(|l| Some((item_id(&l.name)?.to_owned(), l.etag))).collect();
    let mut out = Contents { objects: BTreeMap::new(), unreadable: 0, cursor, listed_at: Some(Utc::now()) };
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
                let r = a.get(&format!("{ITEMS}{id}.bin")).map(|o| o.map(|o| o.data));
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
    Ok(Some(out))
}

// ---- Kept on this device ----------------------------------------------------

/// Where an unlocked vault's items are kept between unlocks, sealed with
/// the vault key, so unlocking downloads only what changed. `None` for a
/// vault id that isn't a plain name.
fn cache_path(dir: &Path, vault_id: &str) -> Option<PathBuf> {
    let plain = !vault_id.is_empty() && vault_id.len() <= 64 && vault_id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-');
    plain.then(|| dir.join(format!("cleanpassword-{vault_id}.cache")))
}

fn cache_aad(vault_id: &str) -> Vec<u8> {
    format!("cleannotes/vault-cache/v1/{vault_id}").into_bytes()
}

/// Keeps `contents` on this device, sealed with the vault key.
fn save_cache(dir: &Path, vault_id: &str, key: &[u8; 32], contents: &Contents) {
    let Some(path) = cache_path(dir, vault_id) else { return };
    let plain = Zeroizing::new(serde_json::to_vec(contents).expect("items serialize"));
    let sealed = sealing::seal(key, &cache_aad(vault_id), &plain);
    let tmp = path.with_extension("tmp");
    if std::fs::write(&tmp, sealed).is_ok() {
        let _ = std::fs::rename(&tmp, &path);
    }
}

/// What [`save_cache`] kept, if it opens with this key.
fn load_cache(dir: &Path, vault_id: &str, key: &[u8; 32]) -> Option<Contents> {
    let sealed = std::fs::read(cache_path(dir, vault_id)?).ok()?;
    let plain = sealing::open(key, &cache_aad(vault_id), &sealed)?;
    serde_json::from_slice(&plain).ok()
}

// ---- Changing it ------------------------------------------------------------

/// Item plaintexts are padded to a multiple of this many bytes.
const PAD: usize = 512;
/// Tries per item when it changes in S3 between reading and writing it.
const RETRIES: usize = 3;

/// What's done to items.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Change {
    /// To CleanPassword's Recently Deleted.
    Delete,
    /// Back from Recently Deleted.
    Recover,
    /// For good: a tombstone, so every device deletes it.
    DeleteForever,
}

/// An item object as CleanPassword seals it: `CPV1 ‖ nonce ‖ ciphertext`,
/// the JSON padded with spaces.
fn seal_item(key: &[u8; 32], vault_id: &str, id: &str, json: &[u8]) -> Vec<u8> {
    let plain = Zeroizing::new(sealing::pad(json.to_vec(), PAD));
    let mut out = MAGIC.to_vec();
    out.extend(sealing::seal(key, format!("cleanpassword/item/v1/{vault_id}/{id}").as_bytes(), &plain));
    out
}

/// The JSON of an item object after `change`, from its JSON now; `None`
/// if there's nothing to do (already so, or a tombstone).
fn changed(json: &[u8], id: &str, change: Change) -> Option<Zeroizing<Vec<u8>>> {
    let mut v: serde_json::Value = serde_json::from_slice(json).ok()?;
    let item = v.get_mut("item")?.as_object_mut()?;
    if item.get("id").and_then(|i| i.as_str()) != Some(id) {
        return None;
    }
    let now = serde_json::to_value(Utc::now()).expect("dates serialize");
    let in_trash = item.get("deleted").is_some_and(|d| !d.is_null());
    match change {
        Change::Delete if in_trash => return None,
        Change::Delete => {
            item.insert("deleted".into(), now.clone());
            item.insert("modified".into(), now);
        }
        Change::Recover if !in_trash => return None,
        Change::Recover => {
            item.remove("deleted");
            item.insert("modified".into(), now);
        }
        Change::DeleteForever => v = serde_json::json!({ "tombstone": { "id": id, "at": now } }),
    }
    Some(Zeroizing::new(serde_json::to_vec(&v).expect("JSON serializes")))
}

/// Applies `change` to the items, as CleanPassword would, then rewrites
/// [`HEAD_OBJECT`] so CleanPassword's devices notice. Returns how many
/// couldn't be changed: gone, unreadable, or (deleting for good) edited
/// elsewhere meanwhile, as an edit beats a delete.
pub fn apply(found: &Found, secret: &str, key: &[u8; 32], ids: &[String], change: Change) -> Result<usize> {
    let a = Access::new(&found.config, secret, &found.config.prefix)?;
    let client = a.client();
    let vault_id = &found.header.vault_id;
    let mut skipped = 0;
    let mut wrote = false;
    for id in ids {
        let name = format!("{ITEMS}{id}.bin");
        if item_id(&name).is_none() {
            skipped += 1;
            continue;
        }
        let mut done = false;
        for _ in 0..RETRIES {
            let Some(obj) = a.get(&name)? else { break };
            let aad = format!("cleanpassword/item/v1/{vault_id}/{id}");
            let Some(plain) = obj.data.strip_prefix(MAGIC).and_then(|d| sealing::open(key, aad.as_bytes(), d)) else { break };
            let Some(json) = changed(&plain, id, change) else {
                // Already so (deleted elsewhere too, say): done.
                done = serde_json::from_slice::<serde_json::Value>(&plain).is_ok_and(|v| v.get("item").is_some());
                break;
            };
            let sealed = seal_item(key, vault_id, id, &json);
            if let Put::Stored { .. } = block_on(client.put(&name, sealed, Cond::Match(&obj.etag)))? {
                done = true;
                wrote = true;
                break;
            }
            if change == Change::DeleteForever {
                break;
            }
        }
        if !done {
            skipped += 1;
        }
    }
    if wrote {
        block_on(client.touch(HEAD_OBJECT)).context("marking the vault changed")?;
    }
    Ok(skipped)
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
    /// Changes being written.
    saving: usize,
    /// Where the vault's items are kept between unlocks (sealed), if
    /// anywhere.
    cache_dir: Option<PathBuf>,
}

impl State {
    pub fn unlocked(&self) -> bool {
        self.open.is_some()
    }

    /// Changes are being written.
    pub fn saving(&self) -> bool {
        self.saving > 0
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

    /// Keeps the vault's items in `dir` between unlocks, sealed with the
    /// vault key, so unlocking downloads only what changed.
    pub fn set_cache_dir(&self, dir: PathBuf) {
        self.state().cache_dir = Some(dir);
    }

    /// Takes a read of the vault (`None`: nothing changed) and keeps it on
    /// this device.
    fn take_read(s: &mut State, found: &Found, read: Option<Contents>) {
        let dir = s.cache_dir.clone();
        let Some((key, contents)) = s.open.as_mut() else { return };
        if let Some(c) = read {
            *contents = c;
        }
        if let Some(dir) = dir {
            save_cache(&dir, &found.header.vault_id, key, contents);
        }
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
            let r = found.header.unwrap_key(&password).map(|key| {
                // What this device kept from last time, then only what
                // changed since (one HEAD if nothing did). Offline, what
                // was kept is shown.
                let kept = dir.as_deref().and_then(|d| load_cache(d, &found.header.vault_id, &key));
                let read = match &kept {
                    Some(old) => fetch(&found, &secret, &key, old, false).map_err(|e| (e, true)),
                    None => fetch(&found, &secret, &key, &Contents::default(), true).map_err(|e| (e, false)),
                };
                (key, kept, read)
            });
            let mut s = this.state();
            if s.generation == generation {
                s.busy = None;
                match r {
                    Ok((key, kept, read)) => match read {
                        Ok(read) => {
                            s.open = Some((key, kept.unwrap_or_default()));
                            Self::take_read(&mut s, &found, read);
                            s.last_refresh = Some(Instant::now());
                            s.last_use = Some(Instant::now());
                        }
                        Err((e, true)) => {
                            s.open = Some((key, kept.unwrap_or_default()));
                            s.error = Some(format!("Couldn't read the vault (showing what this computer kept): {}", message(&e)));
                            s.last_refresh = Some(Instant::now());
                            s.last_use = Some(Instant::now());
                        }
                        Err((e, false)) => s.error = Some(message(&e)),
                    },
                    Err(e) => s.error = Some(message(&e)),
                }
            }
            drop(s);
            (this.wake)();
        });
    }

    /// Reads the unlocked vault again, in the background: one `HEAD` if
    /// nothing changed, else only what changed. `list` lists the folder
    /// whatever the change marker says (Refresh).
    pub fn refresh_with(&self, list: bool) {
        let (found, secret, key, old, generation) = {
            let mut s = self.state();
            let (Some(f), Some(secret)) = (s.found.clone(), s.secret.clone()) else { return };
            if s.busy.is_some() {
                return;
            }
            let Some((key, contents)) = s.open.as_ref() else { return };
            let key = key.clone();
            let old = contents.copy();
            s.busy = Some(Busy::Refreshing);
            s.last_refresh = Some(Instant::now());
            (f, secret, key, old, s.generation)
        };
        let this = self.clone();
        std::thread::spawn(move || {
            let r = fetch(&found, &secret, &key, &old, list);
            drop(old);
            let mut s = this.state();
            if s.generation == generation {
                s.busy = None;
                match r {
                    // A change written meanwhile reads the vault again
                    // itself; this read may be from before it.
                    Ok(_) if s.saving > 0 => {}
                    Ok(read) => {
                        s.error = None;
                        if read.is_some() {
                            Self::take_read(&mut s, &found, read);
                        }
                    }
                    Err(e) => s.error = Some(format!("Couldn't read the vault: {}", message(&e))),
                }
            }
            drop(s);
            (this.wake)();
        });
    }

    /// Refresh: reads the vault again, listing it in full.
    pub fn refresh(&self) {
        self.refresh_with(true);
    }

    /// Deletes, recovers or deletes for good the items: shown so at once,
    /// written in the background, then the vault is read again (so what
    /// couldn't be changed shows as it is, with [`State::error`] saying so).
    pub fn change(&self, ids: Vec<String>, change: Change) {
        let (found, secret, key, generation) = {
            let mut s = self.state();
            let (Some(f), Some(secret)) = (s.found.clone(), s.secret.clone()) else { return };
            let Some((key, contents)) = s.open.as_mut() else { return };
            let key = key.clone();
            let now = Utc::now();
            for id in &ids {
                if let Some((etag, item)) = contents.objects.get_mut(id) {
                    // Downloaded again by the read that follows.
                    etag.clear();
                    match change {
                        Change::Delete => {
                            if let Some(i) = item.as_mut().filter(|i| i.deleted.is_none()) {
                                i.deleted = Some(now);
                            }
                        }
                        Change::Recover => {
                            if let Some(i) = item.as_mut() {
                                i.deleted = None;
                            }
                        }
                        Change::DeleteForever => *item = None,
                    }
                }
            }
            s.saving += 1;
            s.error = None;
            s.last_use = Some(Instant::now());
            (f, secret, key, s.generation)
        };
        let this = self.clone();
        (self.wake)();
        std::thread::spawn(move || {
            let r = apply(&found, &secret, &key, &ids, change);
            let old = this.state().open.as_ref().map(|(_, c)| c.copy()).unwrap_or_default();
            let read = fetch(&found, &secret, &key, &old, true);
            drop(old);
            let mut s = this.state();
            if s.generation == generation {
                s.saving -= 1;
                if let Ok(read) = read {
                    Self::take_read(&mut s, &found, read);
                    s.last_refresh = Some(Instant::now());
                }
                s.error = match r {
                    Err(e) => Some(format!("Couldn't change the vault: {}", message(&e))),
                    Ok(0) => None,
                    Ok(1) => Some("1 item was changed elsewhere meanwhile and was left as it is.".into()),
                    Ok(n) => Some(format!("{n} items were changed elsewhere meanwhile and were left as they are.")),
                };
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
            (Zeroizing::new([0u8; 32]), Contents { objects, unreadable: 0, cursor: None, listed_at: None })
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
        s.saving = 0;
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
        if stale && !self.state().saving() {
            self.refresh_with(false);
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
