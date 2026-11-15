//! Syncing notes into a Bitwarden vault (bitwarden.com, bitwarden.eu, or a
//! self-hosted server such as Vaultwarden), speaking Bitwarden's client
//! protocol directly: everything is encrypted here before it's sent.
//!
//! - Each note is a **secure note** in the vault, its name the note's title
//!   and its notes the note in Jekyll style (YAML front matter, then
//!   Markdown; see [`frontmatter`]).
//! - Notes live in one vault folder (Settings: "Vault folder", `Notes` by
//!   default). The app's default folder *is* that folder; other app folders
//!   are vault folders inside it: app folder `Work/2026` is the vault folder
//!   `Notes/Work/2026` (Bitwarden nests folders by `/` in their names).
//!   Items elsewhere in the vault are never touched.
//! - Deleting a note for good moves it to Bitwarden's trash.
//! - The master password is never stored: signing in unlocks the user key,
//!   which is kept (with the tokens) in the OS credential store.

use std::collections::{HashMap, HashSet};

use anyhow::{anyhow, bail, Context, Result};
use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine;
use chrono::{DateTime, Utc};
use reqwest::Method as Http;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use super::{Backend, Conflict, NeedsSignIn, NoteData, RemoteFolder, RemoteNote, Snapshot};
use crate::model::DEFAULT_FOLDER as APP_DEFAULT_FOLDER;

pub mod api;
pub mod crypto;
pub mod frontmatter;
#[cfg(test)]
mod fake;
#[cfg(test)]
mod tests;

use api::{get, get_str, status_of, Client, HttpError, Session, Urls};
use crypto::Keys;

pub const DEFAULT_SERVER: &str = "https://vault.bitwarden.com";
pub const DEFAULT_FOLDER: &str = "Notes";

/// A Bitwarden account's settings (no secrets).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Config {
    /// The server's base URL: `https://vault.bitwarden.com`,
    /// `https://vault.bitwarden.eu`, or a self-hosted server.
    pub server: String,
    pub email: String,
    /// The vault folder the notes go in.
    pub folder: String,
}

/// How to sign in.
#[derive(Clone, Debug, PartialEq)]
pub enum Method {
    /// Email (from the config) and master password.
    Password { password: String },
    /// A personal API key (Account Settings → Security → Keys), which skips
    /// two-step login; the master password is still needed to unlock the vault.
    ApiKey { client_id: String, client_secret: String, password: String },
}

impl Method {
    fn password(&self) -> &str {
        match self {
            Method::Password { password } | Method::ApiKey { password, .. } => password,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TwoFactorKind {
    Authenticator,
    Email,
    /// Anything the app can't do (Duo, YubiKey, WebAuthn, …).
    Other(u8),
}

impl TwoFactorKind {
    fn from_code(c: u8) -> TwoFactorKind {
        match c {
            0 => TwoFactorKind::Authenticator,
            1 => TwoFactorKind::Email,
            c => TwoFactorKind::Other(c),
        }
    }

    fn code(self) -> u8 {
        match self {
            TwoFactorKind::Authenticator => 0,
            TwoFactorKind::Email => 1,
            TwoFactorKind::Other(c) => c,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct TwoFactorCode {
    pub kind: TwoFactorKind,
    pub code: String,
    pub remember: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct SignInRequest {
    pub method: Method,
    /// The code for a second step, when the server asked for one.
    pub two_factor: Option<TwoFactorCode>,
    /// The code Bitwarden emailed to confirm a new device, when asked for.
    pub new_device_code: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum SignInStep {
    /// Signed in: what to keep in the credential store (opaque).
    Done { secret: String },
    /// The account uses two-step login with these methods.
    NeedsTwoFactor { kinds: Vec<TwoFactorKind> },
    /// Bitwarden emailed a code to confirm this device.
    NeedsNewDeviceCode,
}

/// Signs in (blocking).
pub fn sign_in(config: &Config, req: &SignInRequest) -> Result<SignInStep> {
    let http = api::http();
    let urls = Urls::of(&config.server);
    let email = crypto::normalize_email(&config.email);
    let kdf = api::prelogin(&http, &urls, &email)?;
    let password = req.method.password();
    let master_key = crypto::master_key(password, &email, kdf)?;
    let hash = crypto::master_password_hash(&master_key, password);
    let device_id = api::device_id(&config.server, &email);

    let mut form: Vec<(&str, String)> = vec![
        ("deviceType", api::device_type().to_string()),
        ("deviceIdentifier", device_id.clone()),
        ("deviceName", api::DEVICE_NAME.into()),
    ];
    let mut api_key = None;
    match &req.method {
        Method::Password { .. } => {
            form.extend([
                ("grant_type", "password".into()),
                ("username", email.clone()),
                ("password", hash.clone()),
                ("scope", "api offline_access".into()),
                ("client_id", api::CLIENT_NAME.into()),
            ]);
        }
        Method::ApiKey { client_id, client_secret, .. } => {
            form.extend([
                ("grant_type", "client_credentials".into()),
                ("scope", "api".into()),
                ("client_id", client_id.clone()),
                ("client_secret", client_secret.clone()),
            ]);
            api_key = Some((client_id.clone(), client_secret.clone()));
        }
    }
    if let Some(tf) = &req.two_factor {
        form.extend([
            ("twoFactorToken", tf.code.clone()),
            ("twoFactorProvider", tf.kind.code().to_string()),
            ("twoFactorRemember", if tf.remember { "1" } else { "0" }.into()),
        ]);
    }
    if let Some(code) = &req.new_device_code {
        form.push(("newDeviceOtp", code.clone()));
    }
    let email_header = matches!(req.method, Method::Password { .. }).then_some(email.as_str());
    let v = match api::token_request(&http, &urls, &form, email_header) {
        Ok(v) => v,
        Err(e) => return sign_in_error(e),
    };

    // Unlock the vault.
    let protected = get_str(&v, "Key").context("the server sent no vault key")?;
    let stretched = crypto::stretch(&master_key);
    let user_key = crypto::decrypt(&stretched, protected)
        .map_err(|_| anyhow!("the master password doesn't unlock this vault"))?;
    Keys::from_bytes(&user_key)?;
    let session = Session {
        access_token: get_str(&v, "access_token").context("no access token")?.to_owned(),
        expires_at: api::now() + get(&v, "expires_in").and_then(Value::as_i64).unwrap_or(3600),
        refresh_token: get_str(&v, "refresh_token").map(str::to_owned),
        api_key,
        user_key: B64.encode(&user_key),
        device_id,
    };
    Ok(SignInStep::Done { secret: serde_json::to_string(&session)? })
}

/// What a refused sign-in means.
fn sign_in_error(e: anyhow::Error) -> Result<SignInStep> {
    let Some(h) = e.downcast_ref::<HttpError>() else { return Err(e) };
    let providers = get(&h.body, "TwoFactorProviders2")
        .and_then(Value::as_object)
        .map(|o| o.keys().filter_map(|k| k.parse::<u8>().ok()).collect::<Vec<_>>())
        .or_else(|| {
            get(&h.body, "TwoFactorProviders")
                .and_then(Value::as_array)
                .map(|a| a.iter().filter_map(|v| v.as_u64().map(|n| n as u8).or_else(|| v.as_str()?.parse().ok())).collect())
        });
    if let Some(mut p) = providers {
        p.sort();
        return Ok(SignInStep::NeedsTwoFactor { kinds: p.into_iter().map(TwoFactorKind::from_code).collect() });
    }
    let msg = h.message.to_lowercase();
    if msg.contains("new device") {
        return Ok(SignInStep::NeedsNewDeviceCode);
    }
    if get(&h.body, "HCaptcha_SiteKey").is_some() || msg.contains("captcha") {
        bail!("Bitwarden wants a captcha for this sign-in. Sign in with an API key instead (Account Settings → Security → Keys).");
    }
    if msg.contains("username or password") || msg.contains("invalid_grant") || msg.contains("invalid_client") {
        bail!("The email address, password or key is incorrect.");
    }
    if msg.contains("two-step") || msg.contains("two factor") || msg.contains("token") && msg.contains("invalid") {
        bail!("That code isn't right. Try again.");
    }
    Err(e)
}

/// Asks the server to email a two-step login code.
pub fn send_email_code(config: &Config, req: &SignInRequest) -> Result<()> {
    let http = api::http();
    let urls = Urls::of(&config.server);
    let email = crypto::normalize_email(&config.email);
    let kdf = api::prelogin(&http, &urls, &email)?;
    let password = req.method.password();
    let hash = crypto::master_password_hash(&crypto::master_key(password, &email, kdf)?, password);
    let body = json!({
        "email": email,
        "masterPasswordHash": hash,
        "deviceIdentifier": api::device_id(&config.server, &email),
    });
    let resp = http.post(format!("{}/two-factor/send-email-login", urls.api)).json(&body).send()?;
    if !resp.status().is_success() {
        bail!("couldn't send the email ({})", resp.status());
    }
    Ok(())
}

/// A vault folder, decrypted.
#[derive(Clone, Debug)]
struct VaultFolder {
    id: String,
    name: String,
}

pub struct BitwardenBackend {
    client: Client,
    config: Config,
    keys: Keys,
    /// The ciphers as last pulled or saved, to keep what the app doesn't
    /// own when updating them.
    ciphers: HashMap<String, Value>,
}

impl BitwardenBackend {
    pub fn open(config: &Config, secret: Option<String>) -> Result<BitwardenBackend> {
        let secret = secret.ok_or_else(|| NeedsSignIn("not signed in".into()))?;
        let session: Session = serde_json::from_str(&secret).map_err(|_| NeedsSignIn("the saved sign-in is unreadable".into()))?;
        let keys = session.keys()?;
        Ok(BitwardenBackend { client: Client::new(&config.server, session), config: config.clone(), keys, ciphers: HashMap::new() })
    }

    fn root(&self) -> &str {
        self.config.folder.trim_matches('/')
    }

    /// The vault folder name for an app folder path.
    fn vault_name(&self, path: &[String]) -> String {
        if path.is_empty() || path == [APP_DEFAULT_FOLDER] {
            self.root().to_owned()
        } else {
            format!("{}/{}", self.root(), path.join("/"))
        }
    }

    /// The app folder path of a vault folder name, if it's one of ours.
    fn app_path(&self, name: &str) -> Option<Vec<String>> {
        let root = self.root();
        if name == root {
            return Some(vec![APP_DEFAULT_FOLDER.to_owned()]);
        }
        let rest = name.strip_prefix(root)?.strip_prefix('/')?;
        let path: Vec<String> = rest.split('/').filter(|s| !s.is_empty()).map(str::to_owned).collect();
        (!path.is_empty()).then_some(path)
    }

    fn folders(&mut self) -> Result<Vec<VaultFolder>> {
        let v = self.client.send(Http::GET, "/folders", None)?;
        let list = get(&v, "data").and_then(Value::as_array).cloned().unwrap_or_default();
        Ok(self.decrypt_folders(&list))
    }

    fn decrypt_folders(&self, list: &[Value]) -> Vec<VaultFolder> {
        list.iter()
            .filter_map(|f| {
                let id = get_str(f, "id")?.to_owned();
                let name = crypto::decrypt_str(&self.keys, get_str(f, "name")?).ok()?;
                Some(VaultFolder { id, name })
            })
            .collect()
    }

    /// The vault folder with this name, made if missing.
    fn ensure_folder(&mut self, name: &str) -> Result<String> {
        if let Some(f) = self.folders()?.into_iter().find(|f| f.name == name) {
            return Ok(f.id);
        }
        let body = json!({ "name": crypto::encrypt_str(&self.keys, name) });
        let v = self.client.send(Http::POST, "/folders", Some(&body))?;
        Ok(get_str(&v, "id").context("no folder id")?.to_owned())
    }

    /// The keys a cipher's fields are encrypted with.
    fn cipher_keys(&self, cipher: &Value) -> Result<Keys> {
        match get_str(cipher, "key") {
            Some(k) => Keys::from_bytes(&crypto::decrypt(&self.keys, k)?),
            None => Ok(self.keys.clone()),
        }
    }

    fn read_cipher(&self, c: &Value, folders: &HashMap<String, Vec<String>>) -> Result<Option<RemoteNote>> {
        // Secure notes of this user (not an organization's), not in the trash,
        // in one of our folders.
        if get(c, "type").and_then(Value::as_u64) != Some(2)
            || get(c, "organizationId").is_some()
            || get(c, "deletedDate").is_some()
        {
            return Ok(None);
        }
        let Some(path) = get_str(c, "folderId").and_then(|f| folders.get(f)) else { return Ok(None) };
        let id = get_str(c, "id").context("a cipher without an id")?;
        let keys = self.cipher_keys(c)?;
        let dec = |k: &str| -> Result<String> {
            match get_str(c, k) {
                Some(e) => crypto::decrypt_str(&keys, e),
                None => Ok(String::new()),
            }
        };
        let name = dec("name")?;
        let notes = dec("notes")?;
        let mut fields = Vec::new();
        for f in get(c, "fields").and_then(Value::as_array).into_iter().flatten() {
            let n = get_str(f, "name").map(|e| crypto::decrypt_str(&keys, e)).transpose()?.unwrap_or_default();
            if n.starts_with(frontmatter::PART_FIELD) {
                let v = get_str(f, "value").map(|e| crypto::decrypt_str(&keys, e)).transpose()?.unwrap_or_default();
                fields.push((n, v));
            }
        }
        let date = |k: &str| get_str(c, k).and_then(|s| DateTime::parse_from_rfc3339(s).ok()).map(|t| t.with_timezone(&Utc));
        let revision = get_str(c, "revisionDate").context("a cipher without a revision date")?.to_owned();
        let item = frontmatter::Item {
            cipher_id: id,
            name: &name,
            notes: &notes,
            fields: &fields,
            created: date("creationDate").unwrap_or_default(),
            revised: date("revisionDate").unwrap_or_default(),
        };
        let mut data = frontmatter::read(&item);
        data.folder = path.clone();
        Ok(Some(RemoteNote { remote_id: id.to_owned(), revision, data }))
    }

    /// The request body for saving a note, keeping what the existing cipher
    /// has that the app doesn't own (favorite, re-prompt, other custom
    /// fields, password history, its own key).
    fn cipher_body(&self, note: &NoteData, folder_id: &str, existing: Option<&Value>) -> Result<Value> {
        let keys = match existing {
            Some(c) => self.cipher_keys(c)?,
            None => self.keys.clone(),
        };
        let w = frontmatter::write(note);
        let mut fields: Vec<Value> = Vec::new();
        if let Some(c) = existing {
            for f in get(c, "fields").and_then(Value::as_array).into_iter().flatten() {
                let ours = get_str(f, "name")
                    .and_then(|e| crypto::decrypt_str(&keys, e).ok())
                    .is_some_and(|n| n.starts_with(frontmatter::PART_FIELD));
                if !ours {
                    fields.push(json!({
                        "type": get(f, "type").cloned().unwrap_or(json!(0)),
                        "name": get(f, "name").cloned(),
                        "value": get(f, "value").cloned(),
                        "linkedId": get(f, "linkedId").cloned(),
                    }));
                }
            }
        }
        for (name, value) in &w.parts {
            fields.push(json!({
                "type": 1,
                "name": crypto::encrypt_str(&keys, name),
                "value": crypto::encrypt_str(&keys, value),
                "linkedId": null,
            }));
        }
        let keep = |k: &str, default: Value| existing.and_then(|c| get(c, k)).cloned().unwrap_or(default);
        let mut body = json!({
            "type": 2,
            "folderId": folder_id,
            "organizationId": null,
            "name": crypto::encrypt_str(&keys, &w.name),
            "notes": crypto::encrypt_str(&keys, &w.notes),
            "favorite": keep("favorite", json!(false)),
            "reprompt": keep("reprompt", json!(0)),
            "secureNote": { "type": 0 },
            "fields": fields,
            "passwordHistory": keep("passwordHistory", Value::Null),
        });
        if let Some(k) = existing.and_then(|c| get(c, "key")) {
            body["key"] = k.clone();
        }
        if let Some(rev) = existing.and_then(|c| get(c, "revisionDate")) {
            body["lastKnownRevisionDate"] = rev.clone();
        }
        Ok(body)
    }
}

impl Backend for BitwardenBackend {
    fn cursor(&mut self) -> Result<Option<String>> {
        let v = self.client.send(Http::GET, "/accounts/revision-date", None)?;
        Ok(Some(match v {
            Value::Number(n) => n.to_string(),
            Value::String(s) => s,
            other => other.to_string(),
        }))
    }

    fn pull(&mut self) -> Result<Snapshot> {
        let v = self.client.send(Http::GET, "/sync?excludeDomains=true", None)?;
        // A reply without these isn't an empty vault: taking it for one would
        // delete every note here.
        let folder_list = get(&v, "folders").and_then(Value::as_array).context("the vault reply has no folder list")?.clone();
        let cipher_list = get(&v, "ciphers").and_then(Value::as_array).context("the vault reply has no item list")?.clone();
        let mut paths: HashMap<String, Vec<String>> = HashMap::new();
        let mut snap = Snapshot::default();
        let root = self.root().to_owned();
        // Folders whose name can't be decrypted might be ours; they and
        // their notes are kept as they are locally.
        let mut unreadable_folders: HashSet<String> = HashSet::new();
        let mut folders = Vec::new();
        for f in &folder_list {
            let Some(id) = get_str(f, "id") else { continue };
            match get_str(f, "name").map(|n| crypto::decrypt_str(&self.keys, n)) {
                Some(Ok(name)) => folders.push(VaultFolder { id: id.to_owned(), name }),
                _ => {
                    unreadable_folders.insert(id.to_owned());
                }
            }
        }
        snap.unreadable.extend(unreadable_folders.iter().cloned());
        for f in folders {
            let Some(path) = self.app_path(&f.name) else { continue };
            // A vault folder "<root>/Notes" holds default-folder notes too, but
            // isn't listed as a folder of its own.
            if f.name != root && path == [APP_DEFAULT_FOLDER] {
                paths.insert(f.id.clone(), path);
                continue;
            }
            snap.folders.push(RemoteFolder { remote_id: f.id.clone(), path: path.clone() });
            paths.insert(f.id, path);
        }
        self.ciphers.clear();
        for c in &cipher_list {
            let in_unreadable_folder = get_str(c, "folderId").is_some_and(|f| unreadable_folders.contains(f));
            match self.read_cipher(c, &paths) {
                Ok(Some(n)) => {
                    self.ciphers.insert(n.remote_id.clone(), c.clone());
                    snap.notes.push(n);
                }
                Ok(None) if in_unreadable_folder => snap.unreadable.extend(get_str(c, "id").map(str::to_owned)),
                Ok(None) => {}
                Err(e) => {
                    // Not a deletion: the note stays as it is here.
                    eprintln!("skipping a vault item that can't be read: {e:#}");
                    snap.unreadable.extend(get_str(c, "id").map(str::to_owned));
                }
            }
        }
        Ok(snap)
    }

    fn create_folder(&mut self, path: &[String]) -> Result<String> {
        let name = self.vault_name(path);
        self.ensure_folder(&name)
    }

    fn rename_folder(&mut self, remote_id: &str, old: &[String], new: &[String]) -> Result<()> {
        let (old_name, new_name) = (self.vault_name(old), self.vault_name(new));
        let prefix = format!("{old_name}/");
        for f in self.folders()? {
            let renamed = if f.id == remote_id {
                new_name.clone()
            } else if let Some(rest) = f.name.strip_prefix(&prefix) {
                format!("{new_name}/{rest}")
            } else {
                continue;
            };
            let body = json!({ "name": crypto::encrypt_str(&self.keys, &renamed) });
            self.client.send(Http::PUT, &format!("/folders/{}", f.id), Some(&body))?;
        }
        Ok(())
    }

    fn delete_folder(&mut self, remote_id: &str, _path: &[String]) -> Result<()> {
        match self.client.send(Http::DELETE, &format!("/folders/{remote_id}"), None) {
            Err(e) if status_of(&e) == Some(404) => Ok(()),
            r => r.map(|_| ()),
        }
    }

    fn save_note(&mut self, remote_id: Option<&str>, base_revision: Option<&str>, note: &NoteData) -> Result<(String, String)> {
        let folder_id = self.ensure_folder(&self.vault_name(&note.folder))?;
        let v = match remote_id {
            Some(id) => {
                let existing = self.ciphers.get(id).cloned();
                let mut body = self.cipher_body(note, &folder_id, existing.as_ref())?;
                if let Some(rev) = base_revision {
                    body["lastKnownRevisionDate"] = json!(rev);
                }
                match self.client.send(Http::PUT, &format!("/ciphers/{id}"), Some(&body)) {
                    Ok(v) => v,
                    Err(e) => {
                        let stale = e.downcast_ref::<HttpError>().is_some_and(|h| {
                            h.status == 404 || h.message.to_lowercase().contains("out of date")
                        });
                        return Err(if stale { Conflict.into() } else { e });
                    }
                }
            }
            None => {
                let body = self.cipher_body(note, &folder_id, None)?;
                self.client.send(Http::POST, "/ciphers", Some(&body))?
            }
        };
        let id = get_str(&v, "id").context("no cipher id")?.to_owned();
        let rev = get_str(&v, "revisionDate").context("no revision date")?.to_owned();
        self.ciphers.insert(id.clone(), v);
        Ok((id, rev))
    }

    fn delete_note(&mut self, remote_id: &str) -> Result<()> {
        self.ciphers.remove(remote_id);
        match self.client.send(Http::PUT, &format!("/ciphers/{remote_id}/delete"), None) {
            Err(e) if status_of(&e) == Some(404) => Ok(()),
            r => r.map(|_| ()),
        }
    }

    fn renewed_secret(&mut self) -> Option<String> {
        if !self.client.renewed {
            return None;
        }
        self.client.renewed = false;
        serde_json::to_string(&self.client.session).ok()
    }
}
