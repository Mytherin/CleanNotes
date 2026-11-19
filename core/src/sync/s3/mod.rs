//! Syncing notes with an S3 bucket (Amazon S3, or a compatible service such
//! as Cloudflare R2 or MinIO) under a folder (key prefix) of your choice:
//!
//! ```text
//! <folder>/head                  changes after every write (cheap polling)
//! <folder>/notes/<id>.md         a note: front matter + Markdown (see format)
//! <folder>/folders/<id>.json     a folder: its path, as a JSON list
//! <folder>/attachments/<name>    a picture a note shows (see crate::attachments)
//! ```
//!
//! - Attachments are named by their content and never change: written with
//!   `If-None-Match: *` (one there already is the same), before the note
//!   that shows them. In an encrypted folder they're sealed and named by an
//!   HMAC of their name instead (see [`crypto`]).
//!
//! - An object's ETag is its revision. Updates are `PUT`s with `If-Match`
//!   and creates use `If-None-Match: *`, so S3 refuses a save based on an
//!   outdated copy (a [`Conflict`]; the sync engine then merges).
//! - Deleting a note for good leaves a tombstone, written with `If-Match`
//!   too, so an edit made elsewhere beats the delete. Tombstones are cleaned
//!   up after 30 days.
//! - A sync's writes are followed by rewriting `head`, once (before the
//!   sync next reads the cursor, or at its end). [`Backend::cursor`] is a
//!   `HEAD` of it (plus a quarter-hour counter, so everything is listed at
//!   least every 15 minutes even if a device died between a write and
//!   updating `head`).
//! - A pull lists the folder and downloads only objects whose ETag changed,
//!   several at once. Notes and folders still at the ETag this device last
//!   synced them at aren't downloaded at all ([`Backend::known`]: the saved
//!   store has them), so a newly started app fetches only what changed.
//! - Requests go through the S3 client shared with CleanPassword
//!   ([`cleanbucket::s3::Client`]), signed with AWS Signature V4. The access
//!   key is kept in the OS credential store.
//!
//! Inside the backend, objects are named relative to the folder (`head`,
//! `notes/<id>.md`, …), as the shared client names them.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use cleanbucket::block_on;
use cleanbucket::s3::{Client, Cond, Credentials, HttpRequest, HttpResponse, Listed, Location, Put, Transport};

use super::{Backend, Conflict, NeedsSignIn, NoteData, RemoteFolder, RemoteNote, Snapshot};

/// S3's errors, as the shared client reads them.
pub use cleanbucket::s3::S3Error;

pub mod crypto;
pub mod format;
#[cfg(test)]
pub(crate) mod fake;
#[cfg(test)]
mod tests;

pub const DEFAULT_REGION: &str = "us-east-1";
pub const DEFAULT_PREFIX: &str = "CleanNotes";
/// Downloads running at once during a pull.
const PARALLEL: usize = 16;
/// How long tombstones are kept.
const TOMBSTONE_DAYS: i64 = 30;
/// Everything is listed at least this often (seconds).
const FULL_LIST_EVERY: i64 = 15 * 60;
const MARKDOWN: &str = "text/markdown; charset=utf-8";
const HEAD: &str = "head";

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Config {
    pub bucket: String,
    pub region: String,
    /// The folder in the bucket, e.g. `CleanNotes` or `notes/work`.
    pub prefix: String,
    /// An S3-compatible service's address (R2, MinIO, tests), used with
    /// path-style URLs. `None` for Amazon S3.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub endpoint: Option<String>,
    /// Notes and folders are encrypted with a key of your own (see
    /// [`crypto`]); the key is in the credential store.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub encrypted: bool,
}

impl Config {
    /// The bucket and folder, for the shared S3 client.
    fn location(&self) -> Location {
        Location { bucket: self.bucket.clone(), region: self.region.clone(), prefix: self.prefix.clone(), endpoint: self.endpoint.clone() }
    }
}

/// What's kept in the credential store.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Keys {
    pub access_key_id: String,
    pub secret_access_key: String,
    /// The encryption key (base64), when the folder is encrypted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub encryption_key: Option<String>,
}

impl Keys {
    pub fn to_secret(&self) -> String {
        serde_json::to_string(self).expect("keys serialize")
    }
}

/// The shared client's errors as the app reads them: a refused access key
/// asks to sign in again, and S3's other errors stay [`S3Error`]s (which
/// callers look for).
fn fail(e: cleanbucket::Error) -> anyhow::Error {
    match e {
        cleanbucket::Error::S3(s) if s.is_auth() => NeedsSignIn(format!("S3 refused the access key ({})", s.code)).into(),
        cleanbucket::Error::S3(s) => s.into(),
        e => anyhow::Error::new(e),
    }
}

/// The shared S3 client's transport here: a blocking HTTP client (calls run
/// on the sync thread), driven with [`cleanbucket::block_on`].
pub(crate) struct Blocking(reqwest::blocking::Client);

impl Blocking {
    pub(crate) fn new() -> Blocking {
        Blocking(
            reqwest::blocking::Client::builder()
                .timeout(Duration::from_secs(60))
                // A redirect (wrong region) is reported, not followed: the
                // request is signed for its host, and the signature is
                // dropped on the way.
                .redirect(reqwest::redirect::Policy::none())
                .user_agent(format!("CleanNotes/{}", env!("CARGO_PKG_VERSION")))
                .build()
                .expect("HTTP client"),
        )
    }
}

impl Transport for Blocking {
    async fn send(&self, req: HttpRequest) -> std::result::Result<HttpResponse, String> {
        let method = reqwest::Method::from_bytes(req.method.as_bytes()).map_err(|e| e.to_string())?;
        let put = method == reqwest::Method::PUT;
        let mut r = self.0.request(method, &req.url);
        for (k, v) in &req.headers {
            r = r.header(k.as_str(), v.as_str());
        }
        if put || !req.body.is_empty() {
            r = r.body(req.body);
        }
        let resp = r.send().map_err(|e| format!("{:#}", anyhow::Error::new(e)))?;
        let status = resp.status().as_u16();
        let headers: Vec<(String, String)> =
            resp.headers().iter().filter_map(|(k, v)| Some((k.as_str().to_owned(), v.to_str().ok()?.to_owned()))).collect();
        let body = match resp.bytes() {
            Ok(b) => b.to_vec(),
            Err(e) if (200..300).contains(&status) => return Err(format!("reading S3's reply: {e}")),
            Err(_) => Vec::new(),
        };
        Ok(HttpResponse { status, headers, body })
    }
}

/// Whether an object (named relative to the folder) is a note or folder of
/// ours. Names with `.` or `..` segments are left alone: a URL can't carry
/// them as they are (they'd be collapsed, and the signature then fails).
fn ours(rel: &str) -> bool {
    let kind = (rel.starts_with("notes/") && rel.ends_with(".md")) || (rel.starts_with("folders/") && rel.ends_with(".json"));
    kind && !rel.split('/').any(|s| s == "." || s == "..")
}

/// What an object was found to hold, by ETag.
#[derive(Clone, Debug)]
enum Cached {
    Note(NoteData),
    Gone(DateTime<Utc>),
    Folder(Vec<String>),
    Unreadable,
}

/// An object's text, ETag and when it was last modified.
type Fetched = (String, String, Option<DateTime<Utc>>);

pub struct S3Backend {
    http: Blocking,
    /// The bucket and folder, and the access key, for the shared client.
    location: Location,
    credentials: Credentials,
    /// The encryption key, if the folder is encrypted.
    key: Option<[u8; 32]>,
    /// Objects as last downloaded: name → (ETag, contents).
    cache: HashMap<String, (String, Cached)>,
    /// Written since `head` was last rewritten (see [`Self::mark_changed`]).
    pending_touch: bool,
    /// The attachment objects (names) the last full listing showed, and
    /// those uploaded since.
    attachments: std::collections::HashSet<String>,
}

impl S3Backend {
    pub fn open(config: &Config, secret: Option<String>) -> Result<S3Backend> {
        use base64::Engine;
        let secret = secret.ok_or_else(|| NeedsSignIn("no access key saved".into()))?;
        let keys: Keys = serde_json::from_str(&secret).map_err(|_| NeedsSignIn("the saved access key is unreadable".into()))?;
        let key = match &keys.encryption_key {
            Some(k) => Some(
                base64::engine::general_purpose::STANDARD
                    .decode(k)
                    .ok()
                    .and_then(|b| <[u8; 32]>::try_from(b).ok())
                    .ok_or_else(|| NeedsSignIn("the saved encryption key is unreadable".into()))?,
            ),
            None if config.encrypted => return Err(NeedsSignIn("the encryption key is missing".into()).into()),
            None => None,
        };
        let credentials = Credentials { access_key_id: keys.access_key_id.clone(), secret_access_key: keys.secret_access_key.clone(), session_token: None };
        Ok(S3Backend {
            http: Blocking::new(),
            location: config.location(),
            credentials,
            key,
            cache: HashMap::new(),
            pending_touch: false,
            attachments: Default::default(),
        })
    }

    /// The client CleanPassword shares.
    fn client(&self) -> Client<'_, Blocking> {
        Client::new(&self.location, &self.credentials, &self.http)
    }

    /// The object an attachment is kept as.
    fn attachment_key(&self, name: &str) -> String {
        match &self.key {
            Some(k) => format!("attachments/{}", crypto::attachment_object(k, name)),
            None => format!("attachments/{name}"),
        }
    }

    /// Whether objects under this name are encrypted (notes and folders).
    fn sealable(rel: &str) -> bool {
        rel.starts_with("notes/") || rel.starts_with("folders/")
    }

    /// An object's text, decrypted if needed. `None` if it's encrypted and
    /// can't be read with our key (or we have none). Unencrypted objects
    /// are read as they are, also in an encrypted folder.
    fn unseal(&self, rel: &str, raw: String) -> Option<String> {
        if !crypto::is_sealed(&raw) {
            return Some(raw);
        }
        crypto::open(self.key.as_ref()?, rel, &raw)
    }

    /// Writes an object, sealed if it's a note or folder in an encrypted
    /// folder. Its new ETag, or `None` if refused as a conflict.
    fn put(&self, rel: &str, body: &str, content_type: &str, cond: Cond) -> Result<Option<String>> {
        let (body, content_type) = match &self.key {
            Some(k) if Self::sealable(rel) => (crypto::seal(k, rel, body), "application/octet-stream"),
            _ => (body.to_owned(), content_type),
        };
        Ok(match block_on(self.client().put_as(rel, body.into_bytes(), cond, content_type)).map_err(fail)? {
            Put::Stored { etag } => Some(etag),
            Put::Conflict => None,
        })
    }

    /// The object's text and ETag; `None` if there's none.
    fn get(&self, rel: &str) -> Result<Option<Fetched>> {
        let o = block_on(self.client().get_dated(rel)).map_err(fail)?;
        Ok(o.map(|(o, modified)| (String::from_utf8_lossy(&o.data).into_owned(), o.etag, modified)))
    }

    fn note_key(&self, stem: &str) -> String {
        format!("notes/{stem}.md")
    }

    fn folder_key(&self, id: &str) -> String {
        format!("folders/{id}.json")
    }

    /// Marks that something changed, for other devices' [`Backend::cursor`].
    fn touch(&self) -> Result<()> {
        block_on(self.client().touch(HEAD)).map_err(fail)?;
        Ok(())
    }

    /// Notes that something was written: `head` is rewritten once, before
    /// the cursor is next read or when the sync ends, rather than after
    /// every write.
    fn mark_changed(&mut self) {
        self.pending_touch = true;
    }

    fn flush_touch(&mut self) -> Result<()> {
        if self.pending_touch {
            self.touch()?;
            self.pending_touch = false;
        }
        Ok(())
    }

    /// Every object under `sub` in the folder. Anything short of the whole
    /// list is an error: the engine would take what's missing as deleted.
    fn list(&self, sub: &str) -> Result<Vec<Listed>> {
        block_on(self.client().list(sub)).map_err(fail)
    }

    /// Lists `sub`, downloads what changed since last time (several at
    /// once), and returns the listing; the contents are in `self.cache`.
    fn refresh(&mut self, sub: &str) -> Result<Vec<Listed>> {
        let listed = self.list(sub)?;
        let wanted: Vec<&Listed> =
            listed.iter().filter(|l| ours(&l.name)).filter(|l| self.cache.get(&l.name).map_or(true, |(e, _)| *e != l.etag)).collect();
        let next = Mutex::new(0usize);
        let results: Mutex<Vec<(String, Result<Option<Fetched>>)>> = Mutex::new(Vec::new());
        let this = &*self;
        std::thread::scope(|s| {
            for _ in 0..PARALLEL.min(wanted.len()) {
                s.spawn(|| loop {
                    let i = {
                        let mut n = next.lock().unwrap();
                        let i = *n;
                        *n += 1;
                        i
                    };
                    let Some(l) = wanted.get(i) else { return };
                    let r = this.get(&l.name);
                    results.lock().unwrap().push((l.name.clone(), r));
                });
            }
        });
        let modified: HashMap<&str, DateTime<Utc>> = listed.iter().map(|l| (l.name.as_str(), l.modified.unwrap_or_default())).collect();
        for (rel, r) in results.into_inner().unwrap() {
            let r = match r {
                // This one object refused (e.g. encrypted with a key the
                // user may not use): kept as it is, not taken as deleted,
                // and the rest still syncs. (A refused access key is an
                // error for the whole account.)
                Err(e) if e.downcast_ref::<S3Error>().is_some_and(|s| (400..500).contains(&s.status)) => {
                    self.cache.insert(rel, (String::new(), Cached::Unreadable));
                    continue;
                }
                r => r?,
            };
            let Some((text, etag, lm)) = r else {
                // Gone between listing and downloading: not known to be
                // deleted, so it's left alone this time.
                self.cache.insert(rel, (String::new(), Cached::Unreadable));
                continue;
            };
            let lm = lm.or_else(|| modified.get(rel.as_str()).copied()).unwrap_or_else(Utc::now);
            let Some(text) = self.unseal(&rel, text) else {
                self.cache.insert(rel, (etag, Cached::Unreadable));
                continue;
            };
            let cached = if let Some(stem) = rel.strip_prefix("notes/").and_then(|r| r.strip_suffix(".md")) {
                match format::read(&text, stem, lm) {
                    format::Object::Note(n) => Cached::Note(n),
                    format::Object::Gone => Cached::Gone(lm),
                }
            } else {
                match serde_json::from_str::<Vec<String>>(&text) {
                    Ok(p) if !p.is_empty() => Cached::Folder(p),
                    _ => Cached::Unreadable,
                }
            };
            self.cache.insert(rel, (etag, cached));
        }
        let present: std::collections::HashSet<&str> = listed.iter().map(|l| l.name.as_str()).collect();
        self.cache.retain(|k, _| !k.starts_with(sub) || present.contains(k.as_str()));
        Ok(listed)
    }

    /// Folders: (id, path, ETag).
    fn folders(&mut self) -> Result<Vec<(String, Vec<String>, String)>> {
        let listed = self.refresh("folders/")?;
        Ok(listed
            .iter()
            .filter_map(|l| {
                let id = l.name.strip_prefix("folders/")?.strip_suffix(".json")?;
                match self.cache.get(&l.name) {
                    Some((etag, Cached::Folder(p))) => Some((id.to_owned(), p.clone(), etag.clone())),
                    _ => None,
                }
            })
            .collect())
    }

    /// Writes a folder; its new ETag, or `None` if refused as a conflict.
    fn put_folder(&mut self, id: &str, path: &[String], cond: Cond) -> Result<Option<String>> {
        let key = self.folder_key(id);
        let r = self.put(&key, &serde_json::to_string(path)?, "application/json", cond)?;
        if let Some(e) = &r {
            self.cache.insert(key, (e.clone(), Cached::Folder(path.to_vec())));
        }
        Ok(r)
    }
}

/// The folder of an account's saved tabs: its `tabs/` directory (see
/// [`cleanbucket::notes::tabs`]), a folder laid out as the account's own,
/// encrypted (with the same key) when the account's is.
pub fn tabs_config(config: &Config) -> Config {
    Config { prefix: cleanbucket::notes::tabs::dir_prefix(&config.prefix), ..config.clone() }
}

/// Whether CleanTabs keeps saved tabs in an account's folder: one `HEAD`
/// of its `tabs/head` (CleanTabs always writes it). An error if it
/// couldn't be looked for (offline, say).
pub fn tabs_exist(config: &Config, secret: &str) -> Result<bool> {
    let keys: Keys = serde_json::from_str(secret).map_err(|_| NeedsSignIn("the saved access key is unreadable".into()))?;
    let credentials = Credentials { access_key_id: keys.access_key_id, secret_access_key: keys.secret_access_key, session_token: None };
    let location = tabs_config(config).location();
    let http = Blocking::new();
    let found = block_on(Client::new(&location, &credentials, &http).head(HEAD)).map_err(fail)?;
    Ok(found.is_some())
}

/// Tests the bucket and access key, and that the service honours
/// conditional writes (without them, two devices could overwrite each
/// other's edits). With a passphrase, sets up encryption for the folder or,
/// if it's encrypted already, checks the passphrase is its key. Returns the
/// config and secret to keep.
pub fn check(config: &Config, keys: &Keys, passphrase: Option<&str>) -> Result<(Config, String)> {
    use base64::Engine;
    if config.bucket.trim().is_empty() {
        bail!("enter the bucket's name");
    }
    let plain = Config { encrypted: false, ..config.clone() };
    let keys = Keys { encryption_key: None, ..keys.clone() };
    let b = S3Backend::open(&plain, Some(keys.to_secret()))?;
    b.touch().context("writing to the bucket")?;
    b.list("").context("listing the bucket")?;
    b.get(HEAD).context("reading from the bucket")?.context("what was just written can't be read back")?;
    let client = b.client();
    let r = block_on(client.put_as(HEAD, b"check".to_vec(), Cond::Absent, "text/plain"));
    let r2 = block_on(client.put_as(HEAD, b"check".to_vec(), Cond::Match("\"0123456789abcdef0123456789abcdef\""), "text/plain"));
    if matches!(r, Ok(Put::Stored { .. })) || matches!(r2, Ok(Put::Stored { .. })) {
        bail!("this storage service ignores conditional writes (If-Match / If-None-Match), so devices could overwrite each other's edits; it can't be used");
    }
    // Refused, but not as outdated (e.g. 501: not supported): every save
    // would fail the same way.
    for r in [r, r2] {
        match r {
            Err(e @ cleanbucket::Error::S3(_)) => {
                return Err(fail(e).context("this storage service doesn't support conditional writes (If-Match / If-None-Match), which syncing needs"))
            }
            r => {
                r.map_err(fail)?;
            }
        }
    }
    b.touch()?;

    let existing = match b.get(crypto::SETTINGS)? {
        Some((text, _, _)) => Some(serde_json::from_str::<crypto::Settings>(&text).context("the folder's encryption.json is damaged")?),
        None => None,
    };
    let passphrase = passphrase.filter(|p| !p.is_empty());
    let key = match (existing, passphrase) {
        (None, None) => None,
        (Some(_), None) => bail!("this folder is encrypted; enter its encryption key"),
        (Some(s), Some(p)) => Some(crypto::unlock(p, &s)?),
        (None, Some(p)) => {
            let (s, key) = crypto::create(p)?;
            if b.put(crypto::SETTINGS, &serde_json::to_string_pretty(&s)?, "application/json", Cond::Absent)?.is_none() {
                bail!("another device set up encryption for this folder just now; try again");
            }
            Some(key)
        }
    };
    let keys = Keys { encryption_key: key.map(|k| base64::engine::general_purpose::STANDARD.encode(k)), ..keys };
    Ok((Config { encrypted: key.is_some(), ..config.clone() }, keys.to_secret()))
}

impl Backend for S3Backend {
    fn cursor(&mut self) -> Result<Option<String>> {
        // Our own writes first, so the cursor read covers them.
        self.flush_touch()?;
        let etag = block_on(self.client().head(HEAD)).map_err(fail)?.unwrap_or_else(|| "none".to_owned());
        Ok(Some(format!("{etag}@{}", Utc::now().timestamp() / FULL_LIST_EVERY)))
    }

    fn pull(&mut self) -> Result<Snapshot> {
        let listed = self.refresh("")?;
        self.attachments = listed.iter().filter(|l| l.name.starts_with("attachments/")).map(|l| l.name.clone()).collect();
        let mut snap = Snapshot::default();
        let mut old_tombstones = Vec::new();
        for l in &listed {
            let rel = l.name.as_str();
            let Some((etag, cached)) = self.cache.get(rel) else { continue };
            if let Some(stem) = rel.strip_prefix("notes/").and_then(|r| r.strip_suffix(".md")) {
                match cached {
                    Cached::Note(n) => snap.notes.push(RemoteNote { remote_id: stem.to_owned(), revision: etag.clone(), data: n.clone() }),
                    Cached::Gone(t) => {
                        if Utc::now() - *t > chrono::Duration::days(TOMBSTONE_DAYS) {
                            old_tombstones.push((rel.to_owned(), etag.clone()));
                        }
                    }
                    _ => snap.unreadable.push(stem.to_owned()),
                }
            } else if let Some(id) = rel.strip_prefix("folders/").and_then(|r| r.strip_suffix(".json")) {
                match cached {
                    Cached::Folder(p) => snap.folders.push(RemoteFolder { remote_id: id.to_owned(), path: p.clone(), revision: Some(etag.clone()) }),
                    _ => snap.unreadable.push(id.to_owned()),
                }
            }
        }
        for (rel, etag) in old_tombstones {
            // Only if it's still that tombstone: another device may have
            // just brought the note back over it. (Best effort: one left
            // over is cleaned up next time.) The client's `delete` isn't
            // conditional, so this one is sent as it is.
            let key = format!("{}{rel}", self.location.root());
            let _ = block_on(self.client().send("DELETE", &key, &[], Vec::new(), &[("if-match", etag)]));
        }
        Ok(snap)
    }

    fn create_folder(&mut self, path: &[String]) -> Result<String> {
        if let Some((id, _, _)) = self.folders()?.into_iter().find(|(_, p, _)| p == path) {
            return Ok(id);
        }
        let id = uuid::Uuid::new_v4().to_string();
        if self.put_folder(&id, path, Cond::Absent)?.is_none() {
            bail!("S3 refused to create the folder: there's one with its new id already");
        }
        self.mark_changed();
        Ok(id)
    }

    fn rename_folder(&mut self, remote_id: &str, old: &[String], new: &[String]) -> Result<()> {
        let mut changed = false;
        for (id, path, etag) in self.folders()? {
            let renamed: Vec<String> = if id == remote_id {
                new.to_vec()
            } else if path.len() > old.len() && path.starts_with(old) {
                new.iter().cloned().chain(path[old.len()..].iter().cloned()).collect()
            } else {
                continue;
            };
            if self.put_folder(&id, &renamed, Cond::Match(&etag))?.is_none() {
                bail!("a folder was changed on another device during the rename; will try again");
            }
            changed = true;
        }
        // Notes name their folder by path: those under the old one move
        // along, or a device syncing later would put them in a new folder
        // with the old name. (Should one be changed meanwhile, the rename
        // is tried again; doing it twice is harmless.)
        let renamed = |p: &[String]| -> Option<Vec<String>> {
            (p.len() >= old.len() && p.starts_with(old)).then(|| new.iter().cloned().chain(p[old.len()..].iter().cloned()).collect())
        };
        self.refresh("notes/")?;
        let moves: Vec<(String, String, NoteData)> = self
            .cache
            .iter()
            .filter_map(|(key, (etag, c))| match c {
                Cached::Note(n) => renamed(&n.folder).map(|folder| (key.clone(), etag.clone(), NoteData { folder, ..n.clone() })),
                _ => None,
            })
            .collect();
        for (key, etag, n) in moves {
            let Some(e) = self.put(&key, &format::write(&n), MARKDOWN, Cond::Match(&etag))? else {
                if changed {
                    self.mark_changed();
                }
                bail!("a note was changed on another device during the folder rename; will try again");
            };
            self.cache.insert(key, (e, Cached::Note(n)));
            changed = true;
        }
        if changed {
            self.mark_changed();
        }
        Ok(())
    }

    fn delete_folder(&mut self, remote_id: &str, _path: &[String]) -> Result<()> {
        let key = self.folder_key(remote_id);
        block_on(self.client().delete(&key)).map_err(fail)?;
        self.cache.remove(&key);
        self.mark_changed();
        Ok(())
    }

    fn save_note(&mut self, remote_id: Option<&str>, base_revision: Option<&str>, note: &NoteData) -> Result<(String, String)> {
        let text = format::write(note);
        let (stem, etag) = match remote_id {
            Some(stem) => {
                let cond = base_revision.map_or(Cond::None, Cond::Match);
                let etag = self.put(&self.note_key(stem), &text, MARKDOWN, cond)?.ok_or(Conflict)?;
                (stem.to_owned(), etag)
            }
            None => {
                let mut stem = note.id.replace(['/', '\\'], "_");
                let mut r = self.put(&self.note_key(&stem), &text, MARKDOWN, Cond::Absent)?;
                if r.is_none() {
                    // Something is there already under this id.
                    let key = self.note_key(&stem);
                    let existing = self.get(&key)?.map(|(raw, etag, _)| (self.unseal(&key, raw), etag));
                    match existing {
                        Some((there, etag)) => match there.map(|t| format::read(&t, &stem, Utc::now())) {
                            // Deleted for good earlier: the note comes back.
                            Some(format::Object::Gone) => r = self.put(&key, &text, MARKDOWN, Cond::Match(&etag))?,
                            // Our own create whose reply got lost: the next
                            // pull matches it up.
                            Some(format::Object::Note(n)) if n.id == note.id => return Err(Conflict.into()),
                            // Something else (or unreadable): this note
                            // goes next to it.
                            _ => {
                                stem = format!("{stem}-{}", &uuid::Uuid::new_v4().simple().to_string()[..8]);
                                r = self.put(&self.note_key(&stem), &text, MARKDOWN, Cond::Absent)?;
                            }
                        },
                        None => return Err(Conflict.into()),
                    }
                }
                (stem, r.ok_or(Conflict)?)
            }
        };
        self.cache.insert(self.note_key(&stem), (etag.clone(), Cached::Note(note.clone())));
        self.mark_changed();
        Ok((stem, etag))
    }

    fn delete_note(&mut self, remote_id: &str, base_revision: Option<&str>) -> Result<()> {
        let key = self.note_key(remote_id);
        let now = Utc::now();
        let Some(etag) = self.put(&key, &format::tombstone(remote_id, now), MARKDOWN, base_revision.map_or(Cond::None, Cond::Match))? else {
            // Refused (or gone): fine if it's deleted already, else it was
            // edited.
            return match self.get(&key)? {
                None => Ok(()),
                Some((raw, _, _)) => match self.unseal(&key, raw) {
                    Some(t) if format::read(&t, remote_id, now) == format::Object::Gone => Ok(()),
                    _ => Err(Conflict.into()),
                },
            };
        };
        self.cache.insert(key, (etag, Cached::Gone(now)));
        self.mark_changed();
        Ok(())
    }

    fn has_attachment(&mut self, name: &str) -> Result<bool> {
        Ok(self.attachments.contains(&self.attachment_key(name)))
    }

    fn put_attachment(&mut self, name: &str, data: &[u8]) -> Result<()> {
        let key = self.attachment_key(name);
        let (body, content_type) = match &self.key {
            Some(k) => (crypto::seal_bytes(k, &key, data), "application/octet-stream".to_owned()),
            None => (data.to_vec(), format!("image/{}", name.rsplit('.').next().unwrap_or("png").replace("jpg", "jpeg"))),
        };
        // Refused as there already: it's the same picture.
        block_on(self.client().put_as(&key, body, Cond::Absent, &content_type)).map_err(fail)?;
        self.attachments.insert(key);
        Ok(())
    }

    fn get_attachment(&mut self, name: &str) -> Result<Option<Vec<u8>>> {
        let key = self.attachment_key(name);
        let Some(o) = block_on(self.client().get(&key)).map_err(fail)? else { return Ok(None) };
        if o.data.starts_with(crypto::MAGIC_BYTES) {
            // One that can't be opened is as good as missing.
            return Ok(self.key.as_ref().and_then(|k| crypto::open_bytes(k, &key, &o.data)));
        }
        Ok(Some(o.data))
    }

    fn known(&mut self, notes: &[(&str, &str, &NoteData)], folders: &[(&str, &str, &[String])]) {
        // An ETag names one version of an object, so a note or folder still
        // at the ETag it was last synced at holds what was synced then: no
        // need to download it again. (What was downloaded since is newer;
        // kept.)
        for (remote_id, revision, data) in notes {
            let key = self.note_key(remote_id);
            self.cache.entry(key).or_insert_with(|| (revision.to_string(), Cached::Note((*data).clone())));
        }
        for (remote_id, revision, path) in folders.iter().filter(|f| !f.2.is_empty()) {
            let key = self.folder_key(remote_id);
            self.cache.entry(key).or_insert_with(|| (revision.to_string(), Cached::Folder(path.to_vec())));
        }
    }

    fn finish(&mut self) -> Result<()> {
        self.flush_touch()
    }
}
