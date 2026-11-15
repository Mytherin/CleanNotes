//! Syncing notes with CloudKit, Apple's iCloud database, through CloudKit
//! Web Services.
//!
//! - Notes go into the signed-in user's **private database** of a CloudKit
//!   container (one you create in the Apple Developer portal), in a custom
//!   zone `CleanNotes`. This is the app's own data in your iCloud account;
//!   CloudKit doesn't give access to Apple Notes' notes.
//! - Each note is a `Note` record named after the note's id, with fields
//!   `noteId`, `folder` (path, a string list), `body` (Markdown), `title`,
//!   `created`, `modified` (timestamps), `pinned` and `deletedAt` (0 if not
//!   in Recently Deleted). Each folder is a `Folder` record with a `path`.
//! - A record's change tag is its revision: a save based on an old one is
//!   refused by CloudKit, which the sync engine handles as a conflict.
//! - Signing in opens Apple's sign-in page in the browser; CloudKit then
//!   redirects to a local port with a web auth token, which is kept in the
//!   credential store. The container's API token must have its sign-in
//!   callback set to "URL Redirect" `http://127.0.0.1:<port>/`.
//! - In the Development environment CloudKit creates the record types on
//!   first save; for Production, deploy the schema in CloudKit Dashboard.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use super::{Backend, Conflict, NeedsSignIn, NoteData, RemoteFolder, RemoteNote, Snapshot};

#[cfg(test)]
mod fake;
#[cfg(test)]
mod tests;

pub const DEFAULT_CALLBACK_PORT: u16 = 52789;
pub const API_BASE: &str = "https://api.apple-cloudkit.com";
pub const ZONE: &str = "CleanNotes";
const SIGN_IN_TIMEOUT: Duration = Duration::from_secs(300);

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Config {
    /// The CloudKit container, e.g. `iCloud.com.example.CleanNotes`.
    pub container: String,
    /// `development` or `production`.
    pub environment: String,
    /// The container's API token (CloudKit Dashboard → API Access). Not a
    /// secret: it only identifies the app; the user's sign-in is the secret.
    pub api_token: String,
    /// The loopback port the token's sign-in callback redirects to.
    pub callback_port: u16,
    /// Another address for CloudKit Web Services (tests, proxies).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub endpoint: Option<String>,
}

impl Config {
    fn base(&self) -> String {
        self.endpoint.clone().unwrap_or_else(|| API_BASE.to_owned()).trim_end_matches('/').to_owned()
    }
}

/// What's kept in the credential store.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct Session {
    web_auth_token: String,
    user: String,
}

/// A CloudKit error: its HTTP status and `serverErrorCode`.
#[derive(Debug)]
pub struct CkError {
    pub status: u16,
    pub code: String,
    pub reason: String,
    pub body: Value,
}

impl std::fmt::Display for CkError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "CloudKit said {} {}: {}", self.status, self.code, self.reason)
    }
}

impl std::error::Error for CkError {}

fn enc(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => out.push(b as char),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

fn http() -> reqwest::blocking::Client {
    reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(60))
        .user_agent(format!("CleanNotes/{}", env!("CARGO_PKG_VERSION")))
        .build()
        .expect("HTTP client")
}

/// The URL of a private-database operation.
fn url(config: &Config, op: &str, web_token: Option<&str>) -> String {
    let mut u = format!(
        "{}/database/1/{}/{}/private/{op}?ckAPIToken={}",
        config.base(),
        enc(&config.container),
        enc(&config.environment),
        enc(&config.api_token)
    );
    if let Some(t) = web_token {
        u.push_str(&format!("&ckWebAuthToken={}", enc(t)));
    }
    u
}

/// The body of a response, or a [`CkError`] (a refused sign-in as
/// [`NeedsSignIn`]). Also returns a renewed web auth token if CloudKit sent one.
fn read(resp: reqwest::blocking::Response) -> Result<(Value, Option<String>)> {
    let status = resp.status().as_u16();
    let renewed = resp
        .headers()
        .get("X-Apple-CloudKit-Web-Auth-Token")
        .and_then(|h| h.to_str().ok())
        .map(str::to_owned);
    let ok = (200..300).contains(&status);
    let text = match resp.text() {
        Ok(t) => t,
        Err(e) if ok => return Err(anyhow::Error::new(e).context("reading CloudKit's reply")),
        Err(_) => String::new(),
    };
    let body: Value = match serde_json::from_str(&text) {
        Ok(v) => v,
        // A cut-off reply or a proxy's web page mustn't pass for "nothing".
        Err(_) if ok && !text.trim().is_empty() => anyhow::bail!("CloudKit's reply isn't JSON (a proxy or captive portal in the way?)"),
        Err(_) => Value::Null,
    };
    if ok {
        return Ok((body, renewed));
    }
    let code = body["serverErrorCode"].as_str().unwrap_or("").to_owned();
    let reason = body["reason"].as_str().unwrap_or(&text).to_owned();
    Err(CkError { status, code, reason, body }.into())
}

/// Signs in with an Apple ID in the browser (blocking). Returns the secret.
pub fn sign_in(config: &Config) -> Result<String> {
    sign_in_with(config, |u| open::that(u).map_err(|e| anyhow!("couldn't open the browser: {e}")))
}

/// [`sign_in`], with how to show Apple's sign-in page passed in.
pub fn sign_in_with(config: &Config, open_url: impl FnOnce(&str) -> Result<()>) -> Result<String> {
    let listener = TcpListener::bind(("127.0.0.1", config.callback_port))
        .with_context(|| format!("port {} is in use; choose another callback port", config.callback_port))?;
    let http = http();
    // Unauthenticated, CloudKit answers with where to sign in.
    let redirect = match read(http.get(url(config, "users/current", None)).send().context("contacting CloudKit")?) {
        Ok(_) => bail!("CloudKit didn't ask for a sign-in; check the API token"),
        Err(e) => match e.downcast_ref::<CkError>() {
            Some(ck) if ck.code == "AUTHENTICATION_REQUIRED" => {
                ck.body["redirectURL"].as_str().context("CloudKit sent no sign-in address")?.to_owned()
            }
            Some(ck) => bail!("CloudKit refused the container or API token: {} {}", ck.code, ck.reason),
            None => return Err(e),
        },
    };
    open_url(&redirect)?;
    let token = wait_for_token(&listener)?;
    let (me, _) = read(http.get(url(config, "users/current", Some(&token))).send()?).context("checking the sign-in")?;
    let user = me["userRecordName"].as_str().unwrap_or_default().to_owned();
    Ok(serde_json::to_string(&Session { web_auth_token: token, user })?)
}

/// Waits for the browser to come back to the loopback port with
/// `ckWebAuthToken`.
fn wait_for_token(listener: &TcpListener) -> Result<String> {
    listener.set_nonblocking(true)?;
    let deadline = Instant::now() + SIGN_IN_TIMEOUT;
    while Instant::now() < deadline {
        let (stream, _) = match listener.accept() {
            Ok(s) => s,
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(100));
                continue;
            }
            Err(e) => return Err(e.into()),
        };
        stream.set_nonblocking(false)?;
        stream.set_read_timeout(Some(Duration::from_secs(5)))?;
        let mut line = String::new();
        BufReader::new(&stream).read_line(&mut line)?;
        let target = line.split_whitespace().nth(1).unwrap_or("");
        let query = target.split_once('?').map_or("", |(_, q)| q);
        let token = query.split('&').find_map(|kv| {
            let (k, v) = kv.split_once('=')?;
            matches!(k, "ckWebAuthToken" | "ckSession").then(|| percent_decode(v))
        });
        let mut out = &stream;
        let page = |title: &str| {
            format!(
                "<!doctype html><meta charset=utf-8><title>CleanNotes</title><body style=\"font-family:system-ui;text-align:center;margin-top:20vh\"><h2>{title}</h2><p>You can close this tab.</p>"
            )
        };
        match token {
            Some(t) if !t.is_empty() => {
                let body = page("Signed in to CleanNotes");
                let _ = write!(out, "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
                return Ok(t);
            }
            _ => {
                let _ = write!(out, "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
            }
        }
    }
    bail!("the sign-in wasn't finished in the browser")
}

fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 3 <= b.len() && b[i + 1].is_ascii_hexdigit() && b[i + 2].is_ascii_hexdigit() {
            // Both are ASCII, so this slice is on char boundaries.
            out.push(u8::from_str_radix(&s[i + 1..i + 3], 16).expect("two hex digits"));
            i += 3;
            continue;
        }
        out.push(if b[i] == b'+' { b' ' } else { b[i] });
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

pub struct CloudKitBackend {
    http: reqwest::blocking::Client,
    config: Config,
    session: Session,
    renewed: bool,
    zone_ready: bool,
    /// Folders as last listed: remote id → (path, change tag).
    folders: HashMap<String, (Vec<String>, String)>,
}

fn ms(t: DateTime<Utc>) -> i64 {
    t.timestamp_millis()
}

fn from_ms(v: &Value) -> Option<DateTime<Utc>> {
    v.as_i64().and_then(DateTime::from_timestamp_millis)
}

impl CloudKitBackend {
    pub fn open(config: &Config, secret: Option<String>) -> Result<CloudKitBackend> {
        let secret = secret.ok_or_else(|| NeedsSignIn("not signed in".into()))?;
        let session: Session = serde_json::from_str(&secret).map_err(|_| NeedsSignIn("the saved sign-in is unreadable".into()))?;
        Ok(CloudKitBackend { http: http(), config: config.clone(), session, renewed: false, zone_ready: false, folders: HashMap::new() })
    }

    fn post(&mut self, op: &str, body: &Value) -> Result<Value> {
        let u = url(&self.config, op, Some(&self.session.web_auth_token));
        let resp = self.http.post(u).json(body).send().context("contacting CloudKit")?;
        match read(resp) {
            Ok((v, renewed)) => {
                if let Some(t) = renewed.filter(|t| *t != self.session.web_auth_token) {
                    self.session.web_auth_token = t;
                    self.renewed = true;
                }
                Ok(v)
            }
            Err(e) => {
                if let Some(ck) = e.downcast_ref::<CkError>() {
                    if ck.status == 421 || ck.code == "AUTHENTICATION_REQUIRED" || ck.code == "AUTHENTICATION_FAILED" {
                        return Err(NeedsSignIn(ck.reason.clone()).into());
                    }
                }
                Err(e)
            }
        }
    }

    fn zone_id() -> Value {
        json!({ "zoneName": ZONE })
    }

    fn ensure_zone(&mut self) -> Result<()> {
        if self.zone_ready {
            return Ok(());
        }
        let v = self.post("zones/modify", &json!({ "operations": [{ "operationType": "create", "zone": { "zoneID": Self::zone_id() } }] }))?;
        if let Some(err) = v["zones"].as_array().and_then(|z| z.first()).and_then(|z| z["serverErrorCode"].as_str()) {
            bail!("couldn't make the CleanNotes zone: {err}");
        }
        self.zone_ready = true;
        Ok(())
    }

    /// Saves records; returns each result (a record, or an error).
    fn modify(&mut self, ops: Vec<Value>) -> Result<Vec<Value>> {
        self.ensure_zone()?;
        let v = self.post("records/modify", &json!({ "operations": ops, "zoneID": Self::zone_id(), "atomic": false }))?;
        Ok(v["records"].as_array().cloned().unwrap_or_default())
    }

    /// Saves one record. A refusal because it changed (or vanished) is a
    /// [`Conflict`].
    fn modify_one(&mut self, op: Value) -> Result<Value> {
        let r = self.modify(vec![op])?.into_iter().next().context("CloudKit returned no record")?;
        match r["serverErrorCode"].as_str() {
            None => Ok(r),
            Some("CONFLICT" | "RECORD_EXISTS" | "NOT_FOUND" | "UNKNOWN_ITEM") => Err(Conflict.into()),
            Some(code) => bail!("CloudKit refused the change: {code} {}", r["reason"].as_str().unwrap_or("")),
        }
    }

    /// Every record in the zone.
    fn all_records(&mut self) -> Result<Vec<Value>> {
        self.ensure_zone()?;
        let mut out = Vec::new();
        let mut token: Option<String> = None;
        // A partial list would make the engine delete the notes it misses,
        // so anything short of the whole zone is an error.
        for _ in 0..10_000 {
            let mut body = json!({ "zoneID": Self::zone_id(), "resultsLimit": 200 });
            if let Some(t) = &token {
                body["syncToken"] = json!(t);
            }
            let v = self.post("records/changes", &body)?;
            let records = v["records"].as_array().context("CloudKit's reply has no record list")?;
            for r in records {
                if r["deleted"].as_bool() != Some(true) {
                    out.push(r.clone());
                }
            }
            if v["moreComing"].as_bool() != Some(true) {
                return Ok(out);
            }
            match v["syncToken"].as_str() {
                Some(t) if token.as_deref() != Some(t) => token = Some(t.to_owned()),
                _ => anyhow::bail!("CloudKit said more records were coming but gave no way to fetch them"),
            }
        }
        anyhow::bail!("the CloudKit zone has too many records to list")
    }

    fn folder_list(&mut self) -> Result<Vec<(String, Vec<String>, String)>> {
        let recs = self.all_records()?;
        let list: Vec<(String, Vec<String>, String)> = recs
            .iter()
            .filter(|r| r["recordType"] == "Folder")
            .filter_map(|r| {
                let path: Vec<String> = r["fields"]["path"]["value"].as_array()?.iter().filter_map(|s| s.as_str().map(str::to_owned)).collect();
                Some((r["recordName"].as_str()?.to_owned(), path, r["recordChangeTag"].as_str().unwrap_or("").to_owned()))
            })
            .collect();
        self.folders = list.iter().map(|(id, p, t)| (id.clone(), (p.clone(), t.clone()))).collect();
        Ok(list)
    }

    fn note_fields(n: &NoteData) -> Value {
        json!({
            "noteId": { "value": n.id, "type": "STRING" },
            "folder": { "value": n.folder, "type": "STRING_LIST" },
            "title": { "value": crate::markdown::title_of(&n.body), "type": "STRING" },
            "body": { "value": n.body, "type": "STRING" },
            "created": { "value": ms(n.created), "type": "TIMESTAMP" },
            "modified": { "value": ms(n.modified), "type": "TIMESTAMP" },
            "pinned": { "value": n.pinned as i64, "type": "INT64" },
            "deletedAt": { "value": n.deleted.map_or(0, ms), "type": "INT64" },
        })
    }

    fn read_note(r: &Value) -> Option<RemoteNote> {
        let f = &r["fields"];
        let name = r["recordName"].as_str()?;
        let folder: Vec<String> = f["folder"]["value"].as_array().map(|a| a.iter().filter_map(|s| s.as_str().map(str::to_owned)).collect()).unwrap_or_default();
        let created = from_ms(&f["created"]["value"]).or_else(|| from_ms(&r["created"]["timestamp"])).unwrap_or_default();
        let data = NoteData {
            id: f["noteId"]["value"].as_str().unwrap_or(name).to_owned(),
            folder: if folder.is_empty() { vec![crate::model::DEFAULT_FOLDER.to_owned()] } else { folder },
            body: f["body"]["value"].as_str().unwrap_or("").to_owned(),
            created,
            modified: from_ms(&f["modified"]["value"]).unwrap_or(created),
            pinned: f["pinned"]["value"].as_i64().unwrap_or(0) != 0,
            deleted: f["deletedAt"]["value"].as_i64().filter(|&d| d > 0).and_then(DateTime::from_timestamp_millis),
        };
        Some(RemoteNote { remote_id: name.to_owned(), revision: r["recordChangeTag"].as_str()?.to_owned(), data })
    }
}

impl Backend for CloudKitBackend {
    fn cursor(&mut self) -> Result<Option<String>> {
        self.ensure_zone()?;
        let v = self.post("zones/lookup", &json!({ "zones": [Self::zone_id()] }))?;
        Ok(v["zones"].as_array().and_then(|z| z.first()).and_then(|z| z["syncToken"].as_str()).map(str::to_owned))
    }

    fn pull(&mut self) -> Result<Snapshot> {
        let recs = self.all_records()?;
        let mut snap = Snapshot::default();
        self.folders.clear();
        for r in &recs {
            match r["recordType"].as_str() {
                Some("Folder") => {
                    let path: Vec<String> = r["fields"]["path"]["value"].as_array().map(|a| a.iter().filter_map(|s| s.as_str().map(str::to_owned)).collect()).unwrap_or_default();
                    let id = r["recordName"].as_str().unwrap_or_default().to_owned();
                    if path.is_empty() || id.is_empty() {
                        continue;
                    }
                    self.folders.insert(id.clone(), (path.clone(), r["recordChangeTag"].as_str().unwrap_or("").to_owned()));
                    snap.folders.push(RemoteFolder { remote_id: id, path });
                }
                Some("Note") => {
                    match Self::read_note(r) {
                        Some(n) => snap.notes.push(n),
                        // Not a deletion: leave the note alone here.
                        None => snap.unreadable.extend(r["recordName"].as_str().map(str::to_owned)),
                    }
                }
                _ => {}
            }
        }
        Ok(snap)
    }

    fn create_folder(&mut self, path: &[String]) -> Result<String> {
        if let Some((id, _, _)) = self.folder_list()?.into_iter().find(|(_, p, _)| p == path) {
            return Ok(id);
        }
        let id = uuid::Uuid::new_v4().to_string();
        let op = json!({ "operationType": "create", "record": {
            "recordName": id, "recordType": "Folder",
            "fields": { "path": { "value": path, "type": "STRING_LIST" } } } });
        self.modify_one(op)?;
        Ok(id)
    }

    fn rename_folder(&mut self, remote_id: &str, old: &[String], new: &[String]) -> Result<()> {
        let mut ops = Vec::new();
        for (id, path, tag) in self.folder_list()? {
            let renamed: Vec<String> = if id == remote_id {
                new.to_vec()
            } else if path.len() > old.len() && path.starts_with(old) {
                new.iter().cloned().chain(path[old.len()..].iter().cloned()).collect()
            } else {
                continue;
            };
            ops.push(json!({ "operationType": "replace", "record": {
                "recordName": id, "recordType": "Folder", "recordChangeTag": tag,
                "fields": { "path": { "value": renamed, "type": "STRING_LIST" } } } }));
        }
        if ops.is_empty() {
            // Gone from CloudKit already (deleted elsewhere): nothing to do.
            return Ok(());
        }
        for r in self.modify(ops)? {
            if let Some(code) = r["serverErrorCode"].as_str() {
                bail!("couldn't rename a folder: {code}");
            }
        }
        Ok(())
    }

    fn delete_folder(&mut self, remote_id: &str, _path: &[String]) -> Result<()> {
        let op = json!({ "operationType": "forceDelete", "record": { "recordName": remote_id } });
        match self.modify_one(op) {
            Err(e) if e.is::<Conflict>() => Ok(()),
            r => r.map(|_| ()),
        }
    }

    fn save_note(&mut self, remote_id: Option<&str>, base_revision: Option<&str>, note: &NoteData) -> Result<(String, String)> {
        let name = remote_id.unwrap_or(&note.id).to_owned();
        let mut record = json!({ "recordName": name, "recordType": "Note", "fields": Self::note_fields(note) });
        let op_type = match (remote_id, base_revision) {
            (Some(_), Some(tag)) => {
                record["recordChangeTag"] = json!(tag);
                "replace"
            }
            (Some(_), None) => "forceReplace",
            (None, _) => "create",
        };
        let r = self.modify_one(json!({ "operationType": op_type, "record": record }))?;
        let tag = r["recordChangeTag"].as_str().context("CloudKit returned no change tag")?.to_owned();
        Ok((name, tag))
    }

    fn delete_note(&mut self, remote_id: &str, base_revision: Option<&str>) -> Result<()> {
        // With a change tag, CloudKit refuses the delete if the record
        // changed since.
        let op = match base_revision {
            Some(tag) => json!({ "operationType": "delete", "record": { "recordName": remote_id, "recordChangeTag": tag } }),
            None => json!({ "operationType": "forceDelete", "record": { "recordName": remote_id } }),
        };
        let r = self.modify(vec![op])?.into_iter().next().context("CloudKit returned no record")?;
        match r["serverErrorCode"].as_str() {
            None | Some("NOT_FOUND" | "UNKNOWN_ITEM") => Ok(()),
            Some("CONFLICT") => Err(Conflict.into()),
            Some(code) => bail!("CloudKit refused the delete: {code} {}", r["reason"].as_str().unwrap_or("")),
        }
    }

    fn renewed_secret(&mut self) -> Option<String> {
        if !self.renewed {
            return None;
        }
        self.renewed = false;
        serde_json::to_string(&self.session).ok()
    }
}
