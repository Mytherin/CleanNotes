//! Syncing notes with Standard Notes (the official server at
//! `api.standardnotes.com`, or a self-hosted one), speaking its client
//! protocol directly: everything is end-to-end encrypted here, with
//! protocol 004 ([`crypto`]), before it's sent.
//!
//! - Each note is a Standard Notes **`Note`** item: its text is the note's
//!   Markdown as it is (new notes use Standard Notes' plain text editor),
//!   its title the note's title, its pin and modification date in
//!   `appData` as Standard Notes keeps them, and Recently Deleted is
//!   Standard Notes' trash. Fields the app doesn't own (other `appData`,
//!   references, the editor, spellcheck, …) are kept on update.
//! - A note whose title isn't its text's first line (one written in
//!   Standard Notes) shows with the title on top as a heading; saving takes
//!   it off again.
//! - Notes written with another editor (Super, rich text, code, …) or
//!   locked are shown **read-only**: their text as plain as possible, under
//!   a line saying so. Pinning, moving and trashing them works on the
//!   original; changing their text saves the change as a *new* plain note
//!   next to it, so the original is never overwritten.
//! - Folders are **tags**, nested through the parent-tag reference the
//!   client uses. A note is in the folder of the tag holding it (if several
//!   do, the deepest); untagged notes are in the default folder *Notes*.
//!   Moving a note takes it out of its other tags.
//! - Deleting a note for good deletes the item.
//! - The password is never stored: signing in derives the root key, and
//!   only the session tokens and the master key (which unlocks the
//!   account's items keys) are kept in the OS credential store.

use std::collections::HashMap;

use anyhow::{anyhow, bail, Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use super::{Backend, Conflict, NeedsSignIn, NoteData, RemoteFolder, RemoteNote, Snapshot};
use crate::model::DEFAULT_FOLDER;

pub mod api;
pub mod crypto;
pub mod items;
#[cfg(test)]
mod fake;
#[cfg(test)]
mod kat;
#[cfg(test)]
mod tests;

use api::{Client, Session};
use items::{iso, Kind};

pub const DEFAULT_SERVER: &str = "https://api.standardnotes.com";
/// The remote id of the default folder, which is no tag: untagged notes.
pub const DEFAULT_FOLDER_ID: &str = "cleannotes:untagged";
const NOTE: &str = "Note";
const TAG: &str = "Tag";
const ITEMS_KEY: &str = "SN|ItemsKey";
const RETRIES: usize = 4;

/// A Standard Notes account's settings (no secrets).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Config {
    /// The sync server's address: [`DEFAULT_SERVER`] or a self-hosted one.
    pub server: String,
    pub email: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct SignInRequest {
    pub password: String,
    /// The authenticator code, when [`SignInStep::NeedsMfaCode`] asked for it.
    pub mfa_code: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum SignInStep {
    /// Signed in: what to keep in the credential store (opaque).
    Done { secret: String },
    /// The account has two-factor authentication: sign in again with the
    /// code from the authenticator app.
    NeedsMfaCode,
}

/// Signs in (blocking). Like the official client, tries the email as typed
/// and then trimmed and in lowercase.
pub fn sign_in(config: &Config, req: &SignInRequest) -> Result<SignInStep> {
    let email = config.email.trim();
    match sign_in_as(config, email, req) {
        Err(e) if email.to_lowercase() != email && !matches!(api::tag_of(&e), Some("mfa-required" | "mfa-invalid")) => {
            sign_in_as(config, &email.to_lowercase(), req)
        }
        r => r,
    }
}

fn sign_in_as(config: &Config, email: &str, req: &SignInRequest) -> Result<SignInStep> {
    let http = api::http();
    // PKCE: the login must come from whoever asked for the key params.
    let verifier = crypto::random_hex(32);
    let mut body = json!({ "email": email, "code_challenge": crypto::code_challenge(&verifier) });
    if let Some(code) = &req.mfa_code {
        body["mfa_code"] = json!(code.trim());
    }
    let params = match api::post(&http, &config.server, "/v2/login-params", body) {
        Ok(v) => api::payload(&v).clone(),
        Err(e) => {
            return match api::tag_of(&e) {
                Some("mfa-required") if req.mfa_code.is_none() => Ok(SignInStep::NeedsMfaCode),
                Some("mfa-required" | "mfa-invalid") => bail!("That two-factor code isn't right. Try again."),
                Some("u2f-required") => {
                    bail!("This account signs in with a hardware security key, which CleanNotes can't use. Use an authenticator app for two-factor authentication instead.")
                }
                _ => Err(e),
            }
        }
    };
    let field = |k: &str| params.get(k).and_then(Value::as_str);
    let version = field("version").unwrap_or("");
    if version != crypto::VERSION {
        bail!(
            "This account uses Standard Notes' encryption protocol {version}, and CleanNotes only speaks {}. Sign in once with the Standard Notes app, which offers to upgrade it.",
            crypto::VERSION
        );
    }
    let nonce = field("pw_nonce").context("the server sent no key params")?;
    let root = crypto::root_key(&req.password, email, nonce)?;
    let body = json!({ "email": email, "password": root.server_password, "ephemeral": false, "code_verifier": verifier });
    let v = match api::post(&http, &config.server, "/v2/login", body) {
        Ok(v) => v,
        Err(e) => {
            if matches!(api::status_of(&e), Some(401 | 403)) || api::tag_of(&e) == Some("invalid-auth") {
                bail!("The email address or password is incorrect.");
            }
            return Err(e);
        }
    };
    let (access_token, refresh_token, access_expiration, refresh_expiration) = api::tokens_of(&v)?;
    let p = api::payload(&v);
    let session = Session {
        access_token,
        refresh_token,
        access_expiration,
        refresh_expiration,
        user_uuid: p.get("user").and_then(|u| u.get("uuid")).and_then(Value::as_str).map(str::to_owned),
        master_key: root.master_key,
        key_params: key_params_json(email, &params, p.get("key_params")),
    };
    Ok(SignInStep::Done { secret: serde_json::to_string(&session)? })
}

/// The key params as the client keeps them (`identifier`, `pw_nonce`,
/// `version`, `origination`, `created`, in that order), preferring what the
/// login answer says over the login params.
fn key_params_json(email: &str, params: &Value, from_login: Option<&Value>) -> String {
    let get = |k: &str| from_login.and_then(|l| l.get(k)).or_else(|| params.get(k)).filter(|v| !v.is_null()).cloned();
    let mut members = vec![("identifier", json!(email))];
    members.extend(get("pw_nonce").map(|v| ("pw_nonce", v)));
    members.push(("version", get("version").unwrap_or(json!(crypto::VERSION))));
    members.push(("origination", get("origination").unwrap_or(json!("registration"))));
    members.extend(get("created").map(|v| ("created", v)));
    let body: Vec<String> = members.iter().map(|(k, v)| format!("{}:{}", json!(k), v)).collect();
    format!("{{{}}}", body.join(","))
}

/// An item's `updated_at_timestamp` (microseconds): its version.
fn ts(item: &Value) -> i64 {
    match item.get("updated_at_timestamp") {
        Some(Value::Number(n)) => n.as_i64().or_else(|| n.as_f64().map(|f| f as i64)).unwrap_or(0),
        _ => 0,
    }
}

fn uuid_of(item: &Value) -> &str {
    item.get("uuid").and_then(Value::as_str).unwrap_or("")
}

fn type_of(item: &Value) -> &str {
    item.get("content_type").and_then(Value::as_str).unwrap_or("")
}

fn has(item: &Value, k: &str) -> bool {
    item.get(k).is_some_and(|v| !v.is_null())
}

fn list<'a>(v: &'a Value, k: &str) -> &'a [Value] {
    v.get(k).and_then(Value::as_array).map(Vec::as_slice).unwrap_or(&[])
}

fn from_micros(us: i64) -> Option<DateTime<Utc>> {
    DateTime::from_timestamp_micros(us)
}

/// What one sync request (and the pages after it) brought back.
#[derive(Default)]
struct Exchange {
    saved: Vec<Value>,
    conflicts: Vec<Value>,
}

enum Pushed {
    Saved,
    /// Changed on the server since; its version is now in `items`.
    Conflict,
    /// The uuid belongs to an item elsewhere.
    UuidTaken,
}

/// The account's tags, as folders.
#[derive(Default)]
struct Tags {
    paths: HashMap<String, Vec<String>>,
    /// The tag that stands for each path (the oldest, if two have one).
    canonical: HashMap<Vec<String>, String>,
    /// The tags holding each note.
    holding: HashMap<String, Vec<String>>,
}

impl Tags {
    /// A note's folder, and the tag standing for it ("" for the default
    /// folder).
    fn folder_of(&self, note: &str) -> (Vec<String>, String) {
        let best = self
            .holding
            .get(note)
            .into_iter()
            .flatten()
            .filter_map(|t| self.paths.get(t))
            .max_by(|a, b| a.len().cmp(&b.len()).then_with(|| b.cmp(a)));
        match best {
            Some(p) if p.as_slice() != [DEFAULT_FOLDER] => (p.clone(), self.canonical.get(p).cloned().unwrap_or_default()),
            _ => (vec![DEFAULT_FOLDER.to_owned()], String::new()),
        }
    }
}

pub struct StandardNotesBackend {
    client: Client,
    /// The account's live items as the server has them (encrypted).
    items: HashMap<String, Value>,
    /// Decrypted contents, with the timestamp they belong to.
    contents: HashMap<String, (i64, Value)>,
    /// Decrypted items keys (hex), by uuid.
    items_keys: HashMap<String, String>,
    default_key: Option<String>,
    sync_token: Option<String>,
    loaded: bool,
    /// The newest item timestamp seen.
    high_water: i64,
    /// Changes came in that the last [`Backend::pull`] didn't hand on.
    pending: bool,
}

impl StandardNotesBackend {
    pub fn open(config: &Config, secret: Option<String>) -> Result<StandardNotesBackend> {
        let secret = secret.ok_or_else(|| NeedsSignIn("not signed in".into()))?;
        let session: Session = serde_json::from_str(&secret).map_err(|_| NeedsSignIn("the saved sign-in is unreadable".into()))?;
        Ok(StandardNotesBackend {
            client: Client::new(&config.server, session),
            items: HashMap::new(),
            contents: HashMap::new(),
            items_keys: HashMap::new(),
            default_key: None,
            sync_token: None,
            loaded: false,
            high_water: 0,
            pending: false,
        })
    }

    fn apply(&mut self, item: Value, foreign: bool) {
        let uuid = uuid_of(&item).to_owned();
        if uuid.is_empty() {
            return;
        }
        let t = ts(&item);
        self.high_water = self.high_water.max(t);
        if self.items.get(&uuid).is_some_and(|cur| ts(cur) > t) {
            return;
        }
        if item.get("deleted").and_then(Value::as_bool) == Some(true) {
            self.items.remove(&uuid);
            self.contents.remove(&uuid);
            self.items_keys.remove(&uuid);
        } else {
            self.items.insert(uuid, item);
        }
        if foreign {
            self.pending = true;
        }
    }

    /// One sync: uploads `push`, takes in everything that changed since the
    /// last one (all pages of it).
    fn exchange(&mut self, push: Vec<Value>) -> Result<Exchange> {
        let pushed: HashMap<String, Value> = push.iter().map(|i| (uuid_of(i).to_owned(), i.clone())).collect();
        let mut out = Exchange::default();
        let mut cursor: Option<String> = None;
        let mut first = true;
        loop {
            let up: &[Value] = if first { &push } else { &[] };
            let r = self.client.sync(up, self.sync_token.as_deref(), cursor.as_deref())?;
            for it in list(&r, "retrieved_items") {
                self.apply(it.clone(), true);
            }
            if first {
                for s in list(&r, "saved_items") {
                    // Saved items come back without their content.
                    let mut full = pushed.get(uuid_of(s)).cloned().unwrap_or_else(|| s.clone());
                    for k in ["updated_at", "updated_at_timestamp", "created_at", "created_at_timestamp", "deleted", "duplicate_of"] {
                        if let Some(v) = s.get(k) {
                            full[k] = v.clone();
                        }
                    }
                    self.apply(full, false);
                    out.saved.push(s.clone());
                }
                for c in list(&r, "conflicts").iter().chain(list(&r, "unsaved")) {
                    if let Some(si) = c.get("server_item").filter(|s| s.is_object()) {
                        self.apply(si.clone(), true);
                    }
                    out.conflicts.push(c.clone());
                }
            }
            if let Some(t) = r.get("sync_token").and_then(Value::as_str) {
                self.sync_token = Some(t.to_owned());
            }
            cursor = r.get("cursor_token").and_then(Value::as_str).filter(|c| !c.is_empty()).map(str::to_owned);
            first = false;
            if cursor.is_none() {
                break;
            }
        }
        self.loaded = true;
        Ok(out)
    }

    fn catch_up(&mut self) -> Result<()> {
        self.exchange(vec![]).map(|_| ())
    }

    /// Uploads one item.
    fn push_one(&mut self, item: Value) -> Result<Pushed> {
        let uuid = uuid_of(&item).to_owned();
        let ex = self.exchange(vec![item])?;
        if ex.saved.iter().any(|s| uuid_of(s) == uuid) {
            return Ok(Pushed::Saved);
        }
        for c in &ex.conflicts {
            let about = ["server_item", "unsaved_item"].iter().filter_map(|k| c.get(*k)).any(|i| uuid_of(i) == uuid);
            if !about {
                continue;
            }
            return match c.get("type").and_then(Value::as_str).unwrap_or("") {
                "sync_conflict" => Ok(Pushed::Conflict),
                "uuid_conflict" => Ok(Pushed::UuidTaken),
                t => bail!("Standard Notes refused to save an item ({t})"),
            };
        }
        bail!("Standard Notes didn't confirm saving an item")
    }

    fn ready(&mut self) -> Result<()> {
        if !self.loaded {
            self.catch_up()?;
        }
        if self.default_key.is_none() {
            self.unlock()?;
        }
        Ok(())
    }

    fn decrypt(item: &Value, key_hex: &str) -> Result<Value> {
        let enc = crypto::Encrypted {
            content: item.get("content").and_then(Value::as_str).context("an item without content")?.to_owned(),
            enc_item_key: item.get("enc_item_key").and_then(Value::as_str).context("an item without a key")?.to_owned(),
        };
        let ids = crypto::ItemIds {
            uuid: uuid_of(item),
            key_system_identifier: item.get("key_system_identifier").and_then(Value::as_str),
            shared_vault_uuid: item.get("shared_vault_uuid").and_then(Value::as_str),
        };
        crypto::decrypt_item(ids, &enc, key_hex)
    }

    /// Decrypts the account's items keys with the master key, or makes the
    /// first one if the account has none (as the client does after its
    /// first full sync).
    fn unlock(&mut self) -> Result<()> {
        let master = self.client.session.master_key.clone();
        self.items_keys.clear();
        let mut any = false;
        let mut candidates: Vec<(bool, i64, String)> = Vec::new();
        for it in self.items.values().filter(|i| type_of(i) == ITEMS_KEY) {
            any = true;
            match Self::decrypt(it, &master) {
                Ok(c) => {
                    if let Some(k) = c.get("itemsKey").and_then(Value::as_str) {
                        self.items_keys.insert(uuid_of(it).to_owned(), k.to_owned());
                        let default = c.get("isDefault").and_then(Value::as_bool) == Some(true);
                        candidates.push((default, ts(it), uuid_of(it).to_owned()));
                    }
                }
                Err(e) => eprintln!("Standard Notes: an items key can't be decrypted: {e:#}"),
            }
        }
        if self.items_keys.is_empty() {
            if any {
                return Err(NeedsSignIn("the account's keys can't be unlocked with the saved sign-in (was the password changed?)".into()).into());
            }
            return self.create_items_key();
        }
        candidates.sort();
        self.default_key = candidates.last().map(|c| c.2.clone());
        Ok(())
    }

    fn create_items_key(&mut self) -> Result<()> {
        let now = Utc::now();
        let uuid = uuid::Uuid::new_v4().to_string();
        let key = crypto::random_hex(32);
        let content = json!({
            "itemsKey": key,
            "version": crypto::VERSION,
            "isDefault": true,
            "references": [],
            "appData": { items::APP_DOMAIN: { "client_updated_at": iso(now) } },
        });
        let s = &self.client.session;
        let enc = crypto::encrypt_item(&uuid, &content, &s.master_key, Some(&s.key_params))?;
        let item = json!({
            "uuid": uuid,
            "content_type": ITEMS_KEY,
            "content": enc.content,
            "enc_item_key": enc.enc_item_key,
            "created_at": iso(now),
            "created_at_timestamp": now.timestamp_micros(),
            "updated_at": iso(DateTime::UNIX_EPOCH),
            "updated_at_timestamp": 0,
            "deleted": false,
        });
        match self.push_one(item)? {
            Pushed::Saved => {
                self.items_keys.insert(uuid.clone(), key);
                self.default_key = Some(uuid);
                Ok(())
            }
            _ => bail!("couldn't save a new items key"),
        }
    }

    /// An item's decrypted content.
    fn content_of(&mut self, uuid: &str) -> Option<Value> {
        let it = self.items.get(uuid)?;
        let t = ts(it);
        if let Some((ct, c)) = self.contents.get(uuid) {
            if *ct == t {
                return Some(c.clone());
            }
        }
        // Items in shared vaults are encrypted with keys of their own.
        if has(it, "key_system_identifier") || has(it, "shared_vault_uuid") {
            return None;
        }
        let key_id = it.get("items_key_id").and_then(Value::as_str).map(str::to_owned).or_else(|| self.default_key.clone())?;
        let Some(key) = self.items_keys.get(&key_id) else {
            eprintln!("Standard Notes: item {uuid} uses an items key that isn't here");
            return None;
        };
        match Self::decrypt(it, key) {
            Ok(c) => {
                self.contents.insert(uuid.to_owned(), (t, c.clone()));
                Some(c)
            }
            Err(e) => {
                eprintln!("Standard Notes: skipping item {uuid} that can't be decrypted: {e:#}");
                None
            }
        }
    }

    fn uuids_of(&self, content_type: &str) -> Vec<String> {
        let mut v: Vec<String> = self.items.values().filter(|i| type_of(i) == content_type).map(|i| uuid_of(i).to_owned()).collect();
        v.sort();
        v
    }

    fn tags(&mut self) -> Tags {
        let mut tags: HashMap<String, (String, Option<String>, i64, Value)> = HashMap::new();
        for id in self.uuids_of(TAG) {
            let Some(c) = self.content_of(&id) else { continue };
            let title = items::str_field(&c, "title").trim();
            let title = if title.is_empty() { "Untitled" } else { title }.to_owned();
            let parent = items::parent_of(&c).map(str::to_owned);
            let created = self.items[&id].get("created_at_timestamp").and_then(Value::as_i64).unwrap_or(0);
            tags.insert(id, (title, parent, created, c));
        }
        let mut out = Tags::default();
        for id in tags.keys() {
            let mut path = Vec::new();
            let mut cur = Some(id.as_str());
            while let Some(c) = cur {
                let Some((title, parent, ..)) = tags.get(c) else { break };
                path.insert(0, title.clone());
                cur = parent.as_deref();
                if path.len() > 32 {
                    break; // a cycle
                }
            }
            out.paths.insert(id.clone(), path);
        }
        let mut order: Vec<(&Vec<String>, i64, &String)> = out.paths.iter().map(|(id, p)| (p, tags[id].2, id)).collect();
        order.sort();
        let mut canonical = HashMap::new();
        for (p, _, id) in order {
            canonical.entry(p.clone()).or_insert_with(|| id.clone());
        }
        out.canonical = canonical;
        for (id, (.., c)) in &tags {
            for r in items::references(c) {
                if r.get("content_type").and_then(Value::as_str) == Some(NOTE) {
                    if let Some(n) = r.get("uuid").and_then(Value::as_str) {
                        out.holding.entry(n.to_owned()).or_default().push(id.clone());
                    }
                }
            }
        }
        out
    }

    fn revision(&self, tags: &Tags, uuid: &str) -> String {
        format!("{}/{}", self.items.get(uuid).map(ts).unwrap_or(0), tags.folder_of(uuid).1)
    }

    /// An item ready to upload, encrypted with the default items key.
    fn encrypted(&self, uuid: &str, content_type: &str, content: &Value, existing: Option<&Value>, created: DateTime<Utc>) -> Result<Value> {
        let key_id = self.default_key.clone().context("no items key")?;
        let enc = crypto::encrypt_item(uuid, content, &self.items_keys[&key_id], None)?;
        let keep = |k: &str, default: Value| existing.and_then(|e| e.get(k)).filter(|v| !v.is_null()).cloned().unwrap_or(default);
        Ok(json!({
            "uuid": uuid,
            "content_type": content_type,
            "content": enc.content,
            "enc_item_key": enc.enc_item_key,
            "items_key_id": key_id,
            "created_at": keep("created_at", json!(iso(created))),
            "created_at_timestamp": keep("created_at_timestamp", json!(created.timestamp_micros())),
            "updated_at": keep("updated_at", json!(iso(DateTime::UNIX_EPOCH))),
            "updated_at_timestamp": existing.map(ts).unwrap_or(0),
            "duplicate_of": keep("duplicate_of", Value::Null),
            "deleted": false,
        }))
    }

    fn deletion(existing: &Value) -> Value {
        json!({
            "uuid": uuid_of(existing),
            "content_type": type_of(existing),
            "content": null,
            "enc_item_key": null,
            "items_key_id": null,
            "created_at": existing.get("created_at").cloned().unwrap_or(Value::Null),
            "created_at_timestamp": existing.get("created_at_timestamp").cloned().unwrap_or(Value::Null),
            "updated_at": existing.get("updated_at").cloned().unwrap_or(Value::Null),
            "updated_at_timestamp": ts(existing),
            "deleted": true,
        })
    }

    /// Deletes an item for good, whatever version it's at.
    fn delete_item(&mut self, uuid: &str) -> Result<()> {
        for _ in 0..RETRIES {
            let Some(existing) = self.items.get(uuid).cloned() else { return Ok(()) };
            match self.push_one(Self::deletion(&existing))? {
                Pushed::Saved => return Ok(()),
                Pushed::Conflict => continue,
                Pushed::UuidTaken => return Ok(()),
            }
        }
        bail!("the item kept changing while deleting it")
    }

    /// Saves a new note. Returns its uuid.
    fn create_note(&mut self, uuid: Option<&str>, note: &NoteData) -> Result<String> {
        let content = items::new_note(note);
        let mut uuid = uuid.filter(|u| uuid::Uuid::parse_str(u).is_ok() && !self.items.contains_key(*u)).map(str::to_lowercase);
        for _ in 0..2 {
            let id = uuid.take().unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
            let item = self.encrypted(&id, NOTE, &content, None, note.created)?;
            match self.push_one(item)? {
                Pushed::Saved => return Ok(id),
                // The note is there already (a create whose reply was lost):
                // the engine matches it up after pulling again.
                Pushed::Conflict => return Err(Conflict.into()),
                Pushed::UuidTaken => continue,
            }
        }
        bail!("couldn't find a free id for a new note")
    }

    /// The tag for a folder path, made (with its parents) if missing.
    fn ensure_tag(&mut self, path: &[String]) -> Result<String> {
        if let Some(id) = self.tags().canonical.get(path) {
            return Ok(id.clone());
        }
        let parent = if path.len() > 1 { Some(self.ensure_tag(&path[..path.len() - 1])?) } else { None };
        let now = Utc::now();
        let id = uuid::Uuid::new_v4().to_string();
        let content = items::new_tag(path.last().context("an empty folder path")?, parent.as_deref(), now);
        let item = self.encrypted(&id, TAG, &content, None, now)?;
        match self.push_one(item)? {
            Pushed::Saved => Ok(id),
            _ => bail!("couldn't save a new tag"),
        }
    }

    /// Updates a tag's content with `f`, retrying if it changed meanwhile.
    fn update_tag(&mut self, id: &str, f: impl Fn(&mut Value)) -> Result<()> {
        for _ in 0..RETRIES {
            let Some(existing) = self.items.get(id).cloned() else { return Ok(()) };
            let Some(mut c) = self.content_of(id) else { bail!("a tag can't be decrypted") };
            let before = c.clone();
            f(&mut c);
            if c == before {
                return Ok(());
            }
            items::touch(&mut c, Utc::now());
            let item = self.encrypted(id, TAG, &c, Some(&existing), Utc::now())?;
            match self.push_one(item)? {
                Pushed::Saved => return Ok(()),
                Pushed::Conflict => continue,
                Pushed::UuidTaken => bail!("a tag's id is taken"),
            }
        }
        bail!("the tag kept changing")
    }

    /// Puts a note in the tag for `path` (none for the default folder),
    /// taking it out of the others.
    fn move_note(&mut self, note: &str, path: &[String]) -> Result<()> {
        let target = if path.is_empty() || path == [DEFAULT_FOLDER] { None } else { Some(self.ensure_tag(path)?) };
        let holding = self.tags().holding.remove(note).unwrap_or_default();
        for t in holding.iter().filter(|t| Some(*t) != target.as_ref()) {
            self.update_tag(t, |c| items::remove_note_from_tag(c, note))?;
        }
        if let Some(t) = target {
            self.update_tag(&t, |c| {
                if !items::tag_has_note(c, note) {
                    items::add_note_to_tag(c, note)
                }
            })?;
        }
        Ok(())
    }
}

impl Backend for StandardNotesBackend {
    /// The newest change seen. Catching up is one request when nothing
    /// changed. (The server's sync token isn't used for this: it's a time,
    /// not a version, and changes with every request.)
    fn cursor(&mut self) -> Result<Option<String>> {
        self.catch_up()?;
        Ok(Some(if self.pending {
            // Changes the engine hasn't seen yet: a marker that won't match.
            format!("{}~{}", self.high_water, uuid::Uuid::new_v4())
        } else {
            self.high_water.to_string()
        }))
    }

    fn pull(&mut self) -> Result<Snapshot> {
        self.catch_up()?;
        self.unlock()?;
        let tags = self.tags();
        let mut snap = Snapshot::default();
        snap.folders.push(RemoteFolder { remote_id: DEFAULT_FOLDER_ID.into(), path: vec![DEFAULT_FOLDER.into()] });
        for (path, id) in &tags.canonical {
            if path.as_slice() != [DEFAULT_FOLDER] {
                snap.folders.push(RemoteFolder { remote_id: id.clone(), path: path.clone() });
            }
        }
        snap.folders.sort_by(|a, b| a.path.cmp(&b.path));
        for id in self.uuids_of(NOTE) {
            let Some(content) = self.content_of(&id) else { continue };
            let it = &self.items[&id];
            let read = items::read_note(&content);
            let updated = from_micros(ts(it)).unwrap_or_else(Utc::now);
            let created = it
                .get("created_at")
                .and_then(items::parse_date)
                .or_else(|| it.get("created_at_timestamp").and_then(Value::as_i64).and_then(from_micros))
                .unwrap_or(updated);
            let modified = read.modified.unwrap_or(updated);
            let data = NoteData {
                id: id.clone(),
                folder: tags.folder_of(&id).0,
                body: read.body,
                created,
                modified,
                pinned: read.pinned,
                deleted: read.trashed.then(|| read.trashed_at.unwrap_or(modified)),
            };
            snap.notes.push(RemoteNote { remote_id: id.clone(), revision: self.revision(&tags, &id), data });
        }
        self.pending = false;
        Ok(snap)
    }

    fn create_folder(&mut self, path: &[String]) -> Result<String> {
        if path.is_empty() || path == [DEFAULT_FOLDER] {
            return Ok(DEFAULT_FOLDER_ID.into());
        }
        self.ready()?;
        self.ensure_tag(path)
    }

    fn rename_folder(&mut self, remote_id: &str, _old: &[String], new: &[String]) -> Result<()> {
        if remote_id == DEFAULT_FOLDER_ID || new.is_empty() {
            return Ok(());
        }
        self.ready()?;
        let parent = if new.len() > 1 { Some(self.ensure_tag(&new[..new.len() - 1])?) } else { None };
        let title = new.last().unwrap().clone();
        self.update_tag(remote_id, |c| {
            c["title"] = json!(title);
            items::set_parent(c, parent.as_deref());
        })
    }

    fn delete_folder(&mut self, remote_id: &str, _path: &[String]) -> Result<()> {
        if remote_id == DEFAULT_FOLDER_ID {
            return Ok(());
        }
        self.ready()?;
        self.delete_item(remote_id)
    }

    fn save_note(&mut self, remote_id: Option<&str>, base_revision: Option<&str>, note: &NoteData) -> Result<(String, String)> {
        self.ready()?;
        let Some(id) = remote_id else {
            let id = self.create_note(Some(&note.id), note)?;
            self.move_note(&id, &note.folder)?;
            let tags = self.tags();
            return Ok((id.clone(), self.revision(&tags, &id)));
        };
        if !self.items.contains_key(id) {
            // Deleted on the server: the engine decides after pulling again.
            return Err(Conflict.into());
        }
        let tags = self.tags();
        if base_revision.is_some_and(|b| b != self.revision(&tags, id)) {
            return Err(Conflict.into());
        }
        let content = self.content_of(id).ok_or_else(|| anyhow!("the note can't be decrypted"))?;
        let read = items::read_note(&content);
        if read.kind == Kind::ReadOnly && note.body != read.body {
            // Never overwrite a note the app can only show: the new text
            // becomes a note of its own.
            let copy = NoteData { body: items::without_banner(&note.body), ..note.clone() };
            let new_id = self.create_note(None, &copy)?;
            self.move_note(&new_id, &note.folder)?;
            let tags = self.tags();
            return Ok((new_id.clone(), self.revision(&tags, &new_id)));
        }
        let mut updated = content.clone();
        if items::update_note(&mut updated, note, &read) {
            let existing = self.items[id].clone();
            let item = self.encrypted(id, NOTE, &updated, Some(&existing), note.created)?;
            match self.push_one(item)? {
                Pushed::Saved => {}
                Pushed::Conflict => return Err(Conflict.into()),
                Pushed::UuidTaken => bail!("the note's id is taken"),
            }
        }
        if tags.folder_of(id).0 != note.folder {
            self.move_note(id, &note.folder)?;
        }
        let tags = self.tags();
        Ok((id.to_owned(), self.revision(&tags, id)))
    }

    fn delete_note(&mut self, remote_id: &str) -> Result<()> {
        if !self.loaded {
            self.catch_up()?;
        }
        self.delete_item(remote_id)
    }

    fn renewed_secret(&mut self) -> Option<String> {
        if !self.client.renewed {
            return None;
        }
        self.client.renewed = false;
        serde_json::to_string(&self.client.session).ok()
    }
}
