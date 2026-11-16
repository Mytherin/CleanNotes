//! Syncing notes with an S3 bucket (Amazon S3, or a compatible service such
//! as Cloudflare R2 or MinIO) under a folder (key prefix) of your choice:
//!
//! ```text
//! <folder>/head                  changes after every write (cheap polling)
//! <folder>/notes/<id>.md         a note: front matter + Markdown (see format)
//! <folder>/folders/<id>.json     a folder: its path, as a JSON list
//! ```
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
//! - Requests are signed with AWS Signature V4 ([`sigv4`]). The access key
//!   is kept in the OS credential store.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use super::{Backend, Conflict, NeedsSignIn, NoteData, RemoteFolder, RemoteNote, Snapshot};

pub mod crypto;
pub mod format;
pub mod sigv4;
#[cfg(test)]
mod fake;
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
    /// The key prefix: the folder with a trailing `/`, or nothing.
    fn root(&self) -> String {
        let p = self.prefix.trim().trim_matches('/');
        if p.is_empty() {
            String::new()
        } else {
            format!("{p}/")
        }
    }

    /// Base URL and path for a key (`""` for the bucket itself).
    fn url_parts(&self, key: &str) -> (String, String) {
        let key = sigv4::uri_encode(key, true);
        let bucket = sigv4::uri_encode(&self.bucket, false);
        match &self.endpoint {
            Some(e) => (e.trim().trim_end_matches('/').to_owned(), format!("/{bucket}/{key}")),
            // A bucket name with dots doesn't match the wildcard certificate
            // as a host name.
            None if self.bucket.contains('.') => (format!("https://s3.{}.amazonaws.com", self.region), format!("/{bucket}/{key}")),
            None => (format!("https://{}.s3.{}.amazonaws.com", self.bucket, self.region), format!("/{key}")),
        }
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

/// An error S3 answered with.
#[derive(Debug)]
pub struct S3Error {
    pub status: u16,
    pub code: String,
    pub message: String,
    /// The bucket's region, when S3 named it.
    pub region: Option<String>,
}

impl std::fmt::Display for S3Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match (self.status, self.code.as_str()) {
            (301 | 307, _) | (_, "PermanentRedirect" | "TemporaryRedirect" | "AuthorizationHeaderMalformed") => match &self.region {
                Some(r) => write!(f, "the bucket is in region {r}; enter that as its region"),
                None => write!(f, "the bucket is in another region than the one entered ({} {})", self.status, self.code),
            },
            (_, "NoSuchBucket") => f.write_str("there's no bucket with that name"),
            (_, "AccessDenied") => write!(f, "S3 denied access; check the IAM policy allows this bucket and folder ({})", self.message),
            (_, "RequestTimeTooSkewed") => f.write_str("this computer's clock is off; S3 refuses the request"),
            (s, "") => write!(f, "S3 answered {s}"),
            (s, c) => write!(f, "S3 answered {s} {c}: {}", self.message),
        }
    }
}

impl std::error::Error for S3Error {}

struct Resp {
    status: u16,
    etag: Option<String>,
    last_modified: Option<String>,
    /// `x-amz-bucket-region`.
    region: Option<String>,
    body: String,
}

impl Resp {
    fn ok(&self) -> bool {
        (200..300).contains(&self.status)
    }

    /// Refused because of `If-Match` / `If-None-Match` (or a concurrent
    /// conditional write), or the object to update is gone.
    fn conflict(&self) -> bool {
        matches!(self.status, 404 | 409 | 412)
    }

    fn error(&self) -> anyhow::Error {
        let code = xml_tag(&self.body, "Code").map(decode).unwrap_or_default();
        if matches!(code.as_str(), "InvalidAccessKeyId" | "SignatureDoesNotMatch" | "InvalidToken" | "ExpiredToken") {
            return NeedsSignIn(format!("S3 refused the access key ({code})")).into();
        }
        let region = self.region.clone().or_else(|| xml_tag(&self.body, "Region").map(decode)).filter(|r| !r.is_empty());
        S3Error { status: self.status, code, message: xml_tag(&self.body, "Message").map(decode).unwrap_or_default(), region }.into()
    }
}

fn http() -> reqwest::blocking::Client {
    reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(60))
        // A redirect (wrong region) is reported, not followed: the request
        // is signed for its host, and the signature is dropped on the way.
        .redirect(reqwest::redirect::Policy::none())
        .user_agent(format!("CleanNotes/{}", env!("CARGO_PKG_VERSION")))
        .build()
        .expect("HTTP client")
}

/// The text inside the first `<tag>…</tag>`.
fn xml_tag<'a>(xml: &'a str, tag: &str) -> Option<&'a str> {
    let open = format!("<{tag}>");
    let start = xml.find(&open)? + open.len();
    let end = xml[start..].find(&format!("</{tag}>"))? + start;
    Some(&xml[start..end])
}

/// Every `<tag>…</tag>`'s text.
fn xml_tags<'a>(xml: &'a str, tag: &str) -> Vec<&'a str> {
    let (open, close) = (format!("<{tag}>"), format!("</{tag}>"));
    let mut out = Vec::new();
    let mut rest = xml;
    while let Some(i) = rest.find(&open) {
        let after = &rest[i + open.len()..];
        let Some(j) = after.find(&close) else { break };
        out.push(&after[..j]);
        rest = &after[j + close.len()..];
    }
    out
}

/// XML character references and the five predefined entities.
fn decode(s: &str) -> String {
    let mut out = String::new();
    let mut rest = s;
    while let Some(i) = rest.find('&') {
        out.push_str(&rest[..i]);
        let after = &rest[i..];
        let Some(end) = after.find(';') else {
            out.push_str(after);
            return out;
        };
        let ent = &after[1..end];
        let c = match ent {
            "quot" => Some('"'),
            "amp" => Some('&'),
            "lt" => Some('<'),
            "gt" => Some('>'),
            "apos" => Some('\''),
            _ if ent.starts_with("#x") => u32::from_str_radix(&ent[2..], 16).ok().and_then(char::from_u32),
            _ if ent.starts_with('#') => ent[1..].parse().ok().and_then(char::from_u32),
            _ => None,
        };
        match c {
            Some(c) => out.push(c),
            None => out.push_str(&after[..=end]),
        }
        rest = &after[end + 1..];
    }
    out.push_str(rest);
    out
}

fn http_date(s: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc2822(s).or_else(|_| DateTime::parse_from_rfc3339(s)).ok().map(|t| t.with_timezone(&Utc))
}

/// Whether a key (relative to the folder) is a note or folder of ours.
/// Keys with `.` or `..` segments are left alone: a URL can't carry them
/// as they are (they'd be collapsed, and the signature then fails).
fn ours(rel: &str) -> bool {
    let kind = (rel.starts_with("notes/") && rel.ends_with(".md")) || (rel.starts_with("folders/") && rel.ends_with(".json"));
    kind && !rel.split('/').any(|s| s == "." || s == "..")
}

/// A listed object.
struct Listed {
    key: String,
    etag: String,
    modified: DateTime<Utc>,
}

/// What an object was found to hold, by ETag.
#[derive(Clone, Debug)]
enum Cached {
    Note(NoteData),
    Gone(DateTime<Utc>),
    Folder(Vec<String>),
    Unreadable,
}

/// A conditional write.
enum Cond<'a> {
    None,
    Match(&'a str),
    Absent,
}

pub struct S3Backend {
    http: reqwest::blocking::Client,
    config: Config,
    keys: Keys,
    /// The encryption key, if the folder is encrypted.
    key: Option<[u8; 32]>,
    /// Objects as last downloaded: key → (ETag, contents).
    cache: HashMap<String, (String, Cached)>,
    /// Written since `head` was last rewritten (see [`Self::mark_changed`]).
    pending_touch: bool,
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
        Ok(S3Backend { http: http(), config: config.clone(), keys, key, cache: HashMap::new(), pending_touch: false })
    }

    /// A key's name inside the folder (what encryption binds an object to).
    fn rel<'a>(&self, key: &'a str) -> &'a str {
        key.strip_prefix(&self.config.root()).unwrap_or(key)
    }

    /// Whether objects under this key are encrypted (notes and folders).
    fn sealable(rel: &str) -> bool {
        rel.starts_with("notes/") || rel.starts_with("folders/")
    }

    /// An object's text, decrypted if needed. `None` if it's encrypted and
    /// can't be read with our key (or we have none). Unencrypted objects
    /// are read as they are, also in an encrypted folder.
    fn unseal(&self, key: &str, raw: String) -> Option<String> {
        if !crypto::is_sealed(&raw) {
            return Some(raw);
        }
        crypto::open(self.key.as_ref()?, self.rel(key), &raw)
    }

    fn request(&self, method: &str, key: &str, query: &[(String, String)], body: Option<&[u8]>, extra: &[(&str, String)]) -> Result<Resp> {
        let (base, path) = self.config.url_parts(key);
        let q = sigv4::canonical_query(query);
        let full = if q.is_empty() { format!("{base}{path}") } else { format!("{base}{path}?{q}") };
        let url = reqwest::Url::parse(&full).with_context(|| format!("the S3 address {full} isn't valid"))?;
        let host = match url.port() {
            Some(p) => format!("{}:{p}", url.host_str().unwrap_or_default()),
            None => url.host_str().unwrap_or_default().to_owned(),
        };
        let payload = sigv4::sha256_hex(body.unwrap_or_default());
        let signed = sigv4::sign(
            method,
            &path,
            &q,
            &[("host", host), ("x-amz-content-sha256", payload.clone())],
            &payload,
            &self.keys.access_key_id,
            &self.keys.secret_access_key,
            &self.config.region,
            Utc::now(),
        );
        let method = reqwest::Method::from_bytes(method.as_bytes()).expect("HTTP method");
        let mut req = self
            .http
            .request(method, url)
            .header("x-amz-date", signed.amz_date)
            .header("x-amz-content-sha256", payload)
            .header("authorization", signed.authorization);
        for (k, v) in extra {
            req = req.header(*k, v);
        }
        if let Some(b) = body {
            req = req.body(b.to_vec());
        }
        let resp = req.send().context("contacting S3")?;
        let status = resp.status().as_u16();
        let header = |n: &str| resp.headers().get(n).and_then(|h| h.to_str().ok()).map(str::to_owned);
        let (etag, last_modified, region) = (header("etag"), header("last-modified"), header("x-amz-bucket-region"));
        let ok = (200..300).contains(&status);
        let body = match resp.text() {
            Ok(t) => t,
            Err(e) if ok => return Err(anyhow::Error::new(e).context("reading S3's reply")),
            Err(_) => String::new(),
        };
        Ok(Resp { status, etag, last_modified, region, body })
    }

    fn put(&self, key: &str, body: &str, content_type: &str, cond: Cond) -> Result<Resp> {
        let mut extra = vec![("content-type", content_type.to_owned())];
        match cond {
            Cond::None => {}
            Cond::Match(e) => extra.push(("if-match", e.to_owned())),
            Cond::Absent => extra.push(("if-none-match", "*".to_owned())),
        }
        let rel = self.rel(key);
        let sealed;
        let body = match &self.key {
            Some(k) if Self::sealable(rel) => {
                sealed = crypto::seal(k, rel, body);
                extra[0].1 = "application/octet-stream".to_owned();
                sealed.as_str()
            }
            _ => body,
        };
        self.request("PUT", key, &[], Some(body.as_bytes()), &extra)
    }

    /// The object's text and ETag; `None` if there's none.
    fn get(&self, key: &str) -> Result<Option<(String, String, Option<DateTime<Utc>>)>> {
        let r = self.request("GET", key, &[], None, &[])?;
        if r.status == 404 {
            return Ok(None);
        }
        if !r.ok() {
            return Err(r.error());
        }
        let etag = r.etag.clone().context("S3 sent no ETag")?;
        Ok(Some((r.body, etag, r.last_modified.as_deref().and_then(http_date))))
    }

    fn head_key(&self) -> String {
        format!("{}head", self.config.root())
    }

    fn note_key(&self, stem: &str) -> String {
        format!("{}notes/{stem}.md", self.config.root())
    }

    fn folder_key(&self, id: &str) -> String {
        format!("{}folders/{id}.json", self.config.root())
    }

    /// Marks that something changed, for other devices' [`Backend::cursor`].
    fn touch(&self) -> Result<()> {
        let r = self.put(&self.head_key(), &uuid::Uuid::new_v4().to_string(), "text/plain", Cond::None)?;
        if !r.ok() {
            return Err(r.error());
        }
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
        let prefix = format!("{}{sub}", self.config.root());
        let mut out = Vec::new();
        let mut token: Option<String> = None;
        for _ in 0..100_000 {
            let mut q = vec![("list-type".to_owned(), "2".to_owned()), ("prefix".to_owned(), prefix.clone())];
            if let Some(t) = &token {
                q.push(("continuation-token".to_owned(), t.clone()));
            }
            let r = self.request("GET", "", &q, None, &[])?;
            if !r.ok() {
                return Err(r.error());
            }
            if !r.body.contains("<ListBucketResult") {
                bail!("S3's reply isn't a bucket listing (a proxy or captive portal in the way?)");
            }
            for c in xml_tags(&r.body, "Contents") {
                let (Some(key), Some(etag)) = (xml_tag(c, "Key"), xml_tag(c, "ETag")) else { continue };
                let modified = xml_tag(c, "LastModified").and_then(http_date).unwrap_or_default();
                out.push(Listed { key: decode(key), etag: decode(etag), modified });
            }
            if xml_tag(&r.body, "IsTruncated") != Some("true") {
                return Ok(out);
            }
            match xml_tag(&r.body, "NextContinuationToken").map(decode) {
                Some(t) if token.as_deref() != Some(&t) => token = Some(t),
                _ => bail!("S3 said the listing goes on but gave no way to continue it"),
            }
        }
        bail!("the S3 folder has too many objects to list")
    }

    /// Lists `sub`, downloads what changed since last time (several at
    /// once), and returns the listing; the contents are in `self.cache`.
    fn refresh(&mut self, sub: &str) -> Result<Vec<Listed>> {
        let listed = self.list(sub)?;
        let root = self.config.root();
        let wanted: Vec<&Listed> = listed
            .iter()
            .filter(|l| ours(&l.key[root.len().min(l.key.len())..]))
            .filter(|l| self.cache.get(&l.key).map_or(true, |(e, _)| *e != l.etag))
            .collect();
        let next = Mutex::new(0usize);
        let results: Mutex<Vec<(String, Result<Option<(String, String, Option<DateTime<Utc>>)>>)>> = Mutex::new(Vec::new());
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
                    let r = this.get(&l.key);
                    results.lock().unwrap().push((l.key.clone(), r));
                });
            }
        });
        let modified: HashMap<&str, DateTime<Utc>> = listed.iter().map(|l| (l.key.as_str(), l.modified)).collect();
        for (key, r) in results.into_inner().unwrap() {
            let r = match r {
                // This one object refused (e.g. encrypted with a key the
                // user may not use): kept as it is, not taken as deleted,
                // and the rest still syncs. (A refused access key is an
                // error for the whole account.)
                Err(e) if e.downcast_ref::<S3Error>().is_some_and(|s| (400..500).contains(&s.status)) => {
                    self.cache.insert(key, (String::new(), Cached::Unreadable));
                    continue;
                }
                r => r?,
            };
            let Some((text, etag, lm)) = r else {
                // Gone between listing and downloading: not known to be
                // deleted, so it's left alone this time.
                self.cache.insert(key, (String::new(), Cached::Unreadable));
                continue;
            };
            let rel = key[root.len()..].to_owned();
            let lm = lm.or_else(|| modified.get(key.as_str()).copied()).unwrap_or_else(Utc::now);
            let Some(text) = self.unseal(&key, text) else {
                self.cache.insert(key, (etag, Cached::Unreadable));
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
            self.cache.insert(key, (etag, cached));
        }
        let prefix = format!("{root}{sub}");
        let present: std::collections::HashSet<&str> = listed.iter().map(|l| l.key.as_str()).collect();
        self.cache.retain(|k, _| !k.starts_with(&prefix) || present.contains(k.as_str()));
        Ok(listed)
    }

    /// Folders: (id, path, ETag).
    fn folders(&mut self) -> Result<Vec<(String, Vec<String>, String)>> {
        let listed = self.refresh("folders/")?;
        let root = self.config.root();
        Ok(listed
            .iter()
            .filter_map(|l| {
                let id = l.key[root.len()..].strip_prefix("folders/")?.strip_suffix(".json")?;
                match self.cache.get(&l.key) {
                    Some((etag, Cached::Folder(p))) => Some((id.to_owned(), p.clone(), etag.clone())),
                    _ => None,
                }
            })
            .collect())
    }

    fn put_folder(&mut self, id: &str, path: &[String], cond: Cond) -> Result<Resp> {
        let key = self.folder_key(id);
        let r = self.put(&key, &serde_json::to_string(path)?, "application/json", cond)?;
        if r.ok() {
            if let Some(e) = &r.etag {
                self.cache.insert(key, (e.clone(), Cached::Folder(path.to_vec())));
            }
        }
        Ok(r)
    }
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
    b.get(&b.head_key()).context("reading from the bucket")?.context("what was just written can't be read back")?;
    let r = b.put(&b.head_key(), "check", "text/plain", Cond::Absent)?;
    let r2 = b.put(&b.head_key(), "check", "text/plain", Cond::Match("\"0123456789abcdef0123456789abcdef\""))?;
    if r.ok() || r2.ok() {
        bail!("this storage service ignores conditional writes (If-Match / If-None-Match), so devices could overwrite each other's edits; it can't be used");
    }
    // Refused, but not as outdated (e.g. 501: not supported): every save
    // would fail the same way.
    for r in [r, r2] {
        if !matches!(r.status, 409 | 412) {
            return Err(r.error().context("this storage service doesn't support conditional writes (If-Match / If-None-Match), which syncing needs"));
        }
    }
    b.touch()?;

    let settings_key = format!("{}{}", config.root(), crypto::SETTINGS);
    let existing = match b.get(&settings_key)? {
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
            let r = b.put(&settings_key, &serde_json::to_string_pretty(&s)?, "application/json", Cond::Absent)?;
            if r.conflict() {
                bail!("another device set up encryption for this folder just now; try again");
            }
            if !r.ok() {
                return Err(r.error());
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
        let r = self.request("HEAD", &self.head_key(), &[], None, &[])?;
        let etag = match r.status {
            404 => "none".to_owned(),
            _ if r.ok() => r.etag.unwrap_or_default(),
            _ => return Err(r.error()),
        };
        Ok(Some(format!("{etag}@{}", Utc::now().timestamp() / FULL_LIST_EVERY)))
    }

    fn pull(&mut self) -> Result<Snapshot> {
        let listed = self.refresh("")?;
        let root = self.config.root();
        let mut snap = Snapshot::default();
        let mut old_tombstones = Vec::new();
        for l in &listed {
            let rel = &l.key[root.len().min(l.key.len())..];
            let Some((etag, cached)) = self.cache.get(&l.key) else { continue };
            if let Some(stem) = rel.strip_prefix("notes/").and_then(|r| r.strip_suffix(".md")) {
                match cached {
                    Cached::Note(n) => snap.notes.push(RemoteNote { remote_id: stem.to_owned(), revision: etag.clone(), data: n.clone() }),
                    Cached::Gone(t) => {
                        if Utc::now() - *t > chrono::Duration::days(TOMBSTONE_DAYS) {
                            old_tombstones.push((l.key.clone(), etag.clone()));
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
        for (key, etag) in old_tombstones {
            // Only if it's still that tombstone: another device may have
            // just brought the note back over it. (Best effort: one left
            // over is cleaned up next time.)
            let _ = self.request("DELETE", &key, &[], None, &[("if-match", etag)]);
        }
        Ok(snap)
    }

    fn create_folder(&mut self, path: &[String]) -> Result<String> {
        if let Some((id, _, _)) = self.folders()?.into_iter().find(|(_, p, _)| p == path) {
            return Ok(id);
        }
        let id = uuid::Uuid::new_v4().to_string();
        let r = self.put_folder(&id, path, Cond::Absent)?;
        if !r.ok() {
            return Err(r.error());
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
            let r = self.put_folder(&id, &renamed, Cond::Match(&etag))?;
            if r.conflict() {
                bail!("a folder was changed on another device during the rename; will try again");
            }
            if !r.ok() {
                return Err(r.error());
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
            let r = self.put(&key, &format::write(&n), "text/markdown; charset=utf-8", Cond::Match(&etag))?;
            if r.conflict() {
                if changed {
                    self.mark_changed();
                }
                bail!("a note was changed on another device during the folder rename; will try again");
            }
            if !r.ok() {
                return Err(r.error());
            }
            if let Some(e) = r.etag {
                self.cache.insert(key, (e, Cached::Note(n)));
            }
            changed = true;
        }
        if changed {
            self.mark_changed();
        }
        Ok(())
    }

    fn delete_folder(&mut self, remote_id: &str, _path: &[String]) -> Result<()> {
        let key = self.folder_key(remote_id);
        let r = self.request("DELETE", &key, &[], None, &[])?;
        if !r.ok() && r.status != 404 {
            return Err(r.error());
        }
        self.cache.remove(&key);
        self.mark_changed();
        Ok(())
    }

    fn save_note(&mut self, remote_id: Option<&str>, base_revision: Option<&str>, note: &NoteData) -> Result<(String, String)> {
        let text = format::write(note);
        const MD: &str = "text/markdown; charset=utf-8";
        let (stem, r) = match remote_id {
            Some(stem) => {
                let cond = base_revision.map_or(Cond::None, Cond::Match);
                let r = self.put(&self.note_key(stem), &text, MD, cond)?;
                if r.conflict() {
                    return Err(Conflict.into());
                }
                (stem.to_owned(), r)
            }
            None => {
                let mut stem = note.id.replace(['/', '\\'], "_");
                let mut r = self.put(&self.note_key(&stem), &text, MD, Cond::Absent)?;
                if r.conflict() {
                    // Something is there already under this id.
                    let key = self.note_key(&stem);
                    let existing = self.get(&key)?.map(|(raw, etag, _)| (self.unseal(&key, raw), etag));
                    match existing {
                        Some((there, etag)) => match there.map(|t| format::read(&t, &stem, Utc::now())) {
                            // Deleted for good earlier: the note comes back.
                            Some(format::Object::Gone) => {
                                r = self.put(&self.note_key(&stem), &text, MD, Cond::Match(&etag))?;
                                if r.conflict() {
                                    return Err(Conflict.into());
                                }
                            }
                            // Our own create whose reply got lost: the next
                            // pull matches it up.
                            Some(format::Object::Note(n)) if n.id == note.id => return Err(Conflict.into()),
                            // Something else (or unreadable): this note
                            // goes next to it.
                            _ => {
                                stem = format!("{stem}-{}", &uuid::Uuid::new_v4().simple().to_string()[..8]);
                                r = self.put(&self.note_key(&stem), &text, MD, Cond::Absent)?;
                                if r.conflict() {
                                    return Err(Conflict.into());
                                }
                            }
                        },
                        None => return Err(Conflict.into()),
                    }
                }
                (stem, r)
            }
        };
        if !r.ok() {
            return Err(r.error());
        }
        let etag = r.etag.context("S3 sent no ETag for the saved note")?;
        self.cache.insert(self.note_key(&stem), (etag.clone(), Cached::Note(note.clone())));
        self.mark_changed();
        Ok((stem, etag))
    }

    fn delete_note(&mut self, remote_id: &str, base_revision: Option<&str>) -> Result<()> {
        let key = self.note_key(remote_id);
        let now = Utc::now();
        let r = self.put(&key, &format::tombstone(remote_id, now), "text/markdown; charset=utf-8", base_revision.map_or(Cond::None, Cond::Match))?;
        match r.status {
            // Gone already.
            404 => return Ok(()),
            409 | 412 => {
                // Refused: fine if it's deleted already, else it was edited.
                return match self.get(&key)? {
                    None => Ok(()),
                    Some((raw, _, _)) => match self.unseal(&key, raw) {
                        Some(t) if format::read(&t, remote_id, now) == format::Object::Gone => Ok(()),
                        _ => Err(Conflict.into()),
                    },
                };
            }
            _ if !r.ok() => return Err(r.error()),
            _ => {}
        }
        if let Some(e) = r.etag {
            self.cache.insert(key, (e, Cached::Gone(now)));
        }
        self.mark_changed();
        Ok(())
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
