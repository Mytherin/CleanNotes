//! A stand-in Bitwarden server for tests, answering on 127.0.0.1 the way
//! a self-hosted server does (`/identity/…`, `/api/…`).
//!
//! Like the real server it only stores what clients send it (EncStrings it
//! can't read), checks `lastKnownRevisionDate`, refuses notes and fields over
//! Bitwarden's length limits, expires access tokens, and can ask for
//! two-step login or a new-device code.

use std::collections::{BTreeMap, HashMap, HashSet};


use std::sync::{Arc, Mutex};

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use serde_json::{json, Value};

use super::crypto::{self, Kdf, Keys};
use crate::sync::test_http::{self, parse_form, Request, Response};

pub const TOTP_CODE: &str = "123456";
pub const EMAIL_CODE: &str = "654321";
pub const DEVICE_CODE: &str = "777888";
pub const API_CLIENT_ID: &str = "user.11111111-2222-3333-4444-555555555555";
pub const API_CLIENT_SECRET: &str = "s3cr3tkey";

pub struct State {
    pub email: String,
    pub kdf: Kdf,
    hash: String,
    protected_key: String,
    /// The user key, for tests that write into the vault "from elsewhere".
    pub user_key: Keys,
    /// Two-step methods (0 authenticator, 1 email, 3 YubiKey, …).
    pub two_factor: Vec<u8>,
    pub require_new_device: bool,
    known_devices: HashSet<String>,
    pub emails_sent: usize,
    folders: BTreeMap<String, Value>,
    pub ciphers: BTreeMap<String, Value>,
    revision: i64,
    clock: i64,
    tokens: HashMap<String, bool>,
    refresh_tokens: HashSet<String>,
    /// Requests served, as "METHOD /path".
    pub log: Vec<String>,
    /// When set, `/sync` answers 200 with this body as a web page (a
    /// captive portal, a proxy).
    pub sync_page: Option<String>,
}

impl State {
    fn tick(&mut self) -> String {
        self.clock += 1;
        self.revision = 1_763_200_000_000 + self.clock;
        let t = chrono::DateTime::from_timestamp_millis(self.revision).unwrap();
        // The real server uses 7 fractional digits.
        format!("{}0000Z", t.format("%Y-%m-%dT%H:%M:%S%.3f"))
    }

    fn issue(&mut self, refresh: bool) -> Value {
        let access = uuid::Uuid::new_v4().to_string();
        self.tokens.insert(access.clone(), true);
        let mut v = json!({
            "access_token": access,
            "expires_in": 3600,
            "token_type": "Bearer",
            "Key": self.protected_key,
            "Kdf": match self.kdf { Kdf::Pbkdf2 { .. } => 0, Kdf::Argon2id { .. } => 1 },
        });
        if refresh {
            let r = uuid::Uuid::new_v4().to_string();
            self.refresh_tokens.insert(r.clone());
            v["refresh_token"] = json!(r);
        }
        v
    }

    /// Makes every access token stop working (as if they expired).
    pub fn expire_tokens(&mut self) {
        for v in self.tokens.values_mut() {
            *v = false;
        }
    }

    /// Adds a secure note as another app would (encrypted with the user key).
    pub fn add_note(&mut self, folder_id: Option<&str>, name: &str, notes: &str, extra: Value) -> String {
        let id = uuid::Uuid::new_v4().to_string();
        let rev = self.tick();
        let mut c = json!({
            "id": id, "type": 2, "folderId": folder_id, "organizationId": null,
            "name": crypto::encrypt_str(&self.user_key, name),
            "notes": crypto::encrypt_str(&self.user_key, notes),
            "secureNote": {"type": 0}, "fields": [], "favorite": false, "reprompt": 0,
            "creationDate": rev, "revisionDate": rev, "deletedDate": null, "object": "cipher",
        });
        if let (Some(o), Some(e)) = (c.as_object_mut(), extra.as_object()) {
            for (k, v) in e {
                o.insert(k.clone(), v.clone());
            }
        }
        self.ciphers.insert(id.clone(), c);
        id
    }

    pub fn add_folder(&mut self, name: &str) -> String {
        let id = uuid::Uuid::new_v4().to_string();
        let rev = self.tick();
        self.folders.insert(id.clone(), json!({"id": id, "name": crypto::encrypt_str(&self.user_key, name), "revisionDate": rev, "object": "folder"}));
        id
    }

    pub fn folder_names(&self) -> Vec<String> {
        let mut v: Vec<String> = self
            .folders
            .values()
            .map(|f| crypto::decrypt_str(&self.user_key, f["name"].as_str().unwrap()).unwrap())
            .collect();
        v.sort();
        v
    }

    /// Makes a cipher's notes unreadable to us, as a newer client's
    /// encryption type would.
    pub fn scramble_note(&mut self, cipher_id: &str) {
        let rev = self.tick();
        let c = self.ciphers.get_mut(cipher_id).unwrap();
        c["notes"] = json!("7.dW5rbm93bg==");
        c["revisionDate"] = json!(rev);
    }

    /// Makes the name of the folder called `name` unreadable to us.
    pub fn scramble_folder(&mut self, name: &str) {
        let key = self.user_key.clone();
        let rev = self.tick();
        let f = self
            .folders
            .values_mut()
            .find(|f| f["name"].as_str().and_then(|n| crypto::decrypt_str(&key, n).ok()).as_deref() == Some(name))
            .unwrap();
        f["name"] = json!("9.c29tZXRoaW5n");
        f["revisionDate"] = json!(rev);
    }

    pub fn decrypt_field(&self, cipher: &Value, field: &str) -> String {
        crypto::decrypt_str(&self.user_key, cipher[field].as_str().unwrap()).unwrap()
    }
}

pub struct FakeServer {
    pub url: String,
    pub state: Arc<Mutex<State>>,
}

impl FakeServer {
    pub fn start(email: &str, password: &str, kdf: Kdf) -> FakeServer {
        let mk = crypto::master_key(password, email, kdf).unwrap();
        let user_key = Keys::random();
        let state = State {
            email: crypto::normalize_email(email),
            kdf,
            hash: crypto::master_password_hash(&mk, password),
            protected_key: crypto::encrypt(&crypto::stretch(&mk), &user_key.to_bytes()),
            user_key,
            two_factor: vec![],
            require_new_device: false,
            known_devices: HashSet::new(),
            emails_sent: 0,
            folders: BTreeMap::new(),
            ciphers: BTreeMap::new(),
            revision: 1_763_200_000_000,
            clock: 0,
            tokens: HashMap::new(),
            refresh_tokens: HashSet::new(),
            log: vec![],
            sync_page: None,
        };
        let state = Arc::new(Mutex::new(state));
        let s = state.clone();
        let url = test_http::spawn(move |req| {
            let mut s = s.lock().unwrap();
            s.log.push(format!("{} {}", req.method, req.path));
            if let Some(page) = s.sync_page.as_ref().filter(|_| req.path.ends_with("/sync")) {
                return Response { status: 200, body: page.clone(), content_type: "text/html", headers: vec![] };
            }
            let (status, body) = route(&mut s, req);
            Response::json(status, &body)
        });
        FakeServer { url, state }
    }

    pub fn state(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap()
    }
}

fn bad(msg: &str) -> (u16, Value) {
    (400, json!({"message": msg, "validationErrors": null, "object": "error"}))
}

fn route(s: &mut State, req: &Request) -> (u16, Value) {
    let path = req.path.as_str();
    let body: Value = serde_json::from_str(&req.body).unwrap_or(Value::Null);
    match (req.method.as_str(), path) {
        ("POST", "/identity/accounts/prelogin") => {
            let v = match s.kdf {
                Kdf::Pbkdf2 { iterations } => json!({"kdf": 0, "kdfIterations": iterations}),
                Kdf::Argon2id { iterations, memory, parallelism } => {
                    json!({"kdf": 1, "kdfIterations": iterations, "kdfMemory": memory, "kdfParallelism": parallelism})
                }
            };
            (200, v)
        }
        ("POST", "/identity/connect/token") => token(s, req),
        ("POST", "/api/two-factor/send-email-login") => {
            if body["masterPasswordHash"].as_str() != Some(&s.hash) {
                return bad("Username or password is incorrect.");
            }
            s.emails_sent += 1;
            (200, Value::Null)
        }
        _ if path.starts_with("/api/") => {
            let token = req.headers.get("authorization").and_then(|a| a.strip_prefix("Bearer ")).unwrap_or("");
            if s.tokens.get(token) != Some(&true) {
                return (401, json!({"message": "unauthorized"}));
            }
            api(s, &req.method, &path[4..], body)
        }
        _ => (404, json!({"message": "no such endpoint"})),
    }
}

fn token(s: &mut State, req: &Request) -> (u16, Value) {
    let f = parse_form(&req.body);
    let grant = f.get("grant_type").map(String::as_str).unwrap_or("");
    let invalid = |d: &str| (400, json!({"error": "invalid_grant", "error_description": d, "ErrorModel": {"Message": d, "Object": "error"}}));
    match grant {
        "password" => {
            let email_header = req.headers.get("auth-email").and_then(|e| URL_SAFE_NO_PAD.decode(e).ok());
            if email_header.as_deref() != Some(s.email.as_bytes()) {
                return invalid("Auth-Email header invalid.");
            }
            if f.get("username") != Some(&s.email) || f.get("password") != Some(&s.hash) {
                return invalid("Username or password is incorrect. Try again.");
            }
            if !s.two_factor.is_empty() {
                match (f.get("twoFactorToken"), f.get("twoFactorProvider").and_then(|p| p.parse::<u8>().ok())) {
                    (None, _) => {
                        let providers: serde_json::Map<String, Value> = s.two_factor.iter().map(|p| (p.to_string(), Value::Null)).collect();
                        return (
                            400,
                            json!({"error": "invalid_grant", "error_description": "Two factor required.",
                                   "TwoFactorProviders": s.two_factor.iter().map(|p| p.to_string()).collect::<Vec<_>>(),
                                   "TwoFactorProviders2": providers}),
                        );
                    }
                    (Some(code), Some(0)) if code == TOTP_CODE => {}
                    (Some(code), Some(1)) if code == EMAIL_CODE => {}
                    _ => return invalid("Two-step token is invalid. Try again."),
                }
            }
            let device = f.get("deviceIdentifier").cloned().unwrap_or_default();
            if s.require_new_device && !s.known_devices.contains(&device) {
                match f.get("newDeviceOtp") {
                    None => return invalid("New device verification required."),
                    Some(c) if c == DEVICE_CODE => {
                        s.known_devices.insert(device);
                    }
                    Some(_) => return invalid("Invalid new device OTP."),
                }
            }
            (200, s.issue(true))
        }
        "client_credentials" => {
            if f.get("client_id").map(String::as_str) != Some(API_CLIENT_ID)
                || f.get("client_secret").map(String::as_str) != Some(API_CLIENT_SECRET)
            {
                return (400, json!({"error": "invalid_client"}));
            }
            (200, s.issue(false))
        }
        "refresh_token" => {
            let Some(r) = f.get("refresh_token").filter(|r| s.refresh_tokens.contains(*r)).cloned() else {
                return (400, json!({"error": "invalid_grant"}));
            };
            s.refresh_tokens.remove(&r);
            (200, s.issue(true))
        }
        _ => (400, json!({"error": "unsupported_grant_type"})),
    }
}

/// Bitwarden's length checks on a cipher request.
fn too_long(body: &Value) -> Option<String> {
    let len = |v: &Value| v.as_str().map_or(0, str::len);
    if len(&body["notes"]) > 10_000 {
        return Some("The field Notes exceeds the maximum encrypted value length of 10000 characters.".into());
    }
    if len(&body["name"]) > 1_000 {
        return Some("The field Name exceeds the maximum encrypted value length of 1000 characters.".into());
    }
    for f in body["fields"].as_array().into_iter().flatten() {
        if len(&f["value"]) > 5_000 || len(&f["name"]) > 1_000 {
            return Some("The field Value exceeds the maximum encrypted value length of 5000 characters.".into());
        }
    }
    None
}

fn api(s: &mut State, method: &str, path: &str, body: Value) -> (u16, Value) {
    let segs: Vec<&str> = path.trim_matches('/').split('/').collect();
    match (method, segs.as_slice()) {
        ("GET", ["accounts", "revision-date"]) => (200, json!(s.revision)),
        ("GET", ["sync"]) => (
            200,
            json!({
                "object": "sync",
                "profile": {"email": s.email},
                "folders": s.folders.values().collect::<Vec<_>>(),
                "ciphers": s.ciphers.values().collect::<Vec<_>>(),
            }),
        ),
        ("GET", ["folders"]) => (200, json!({"object": "list", "data": s.folders.values().collect::<Vec<_>>()})),
        ("POST", ["folders"]) => {
            let id = uuid::Uuid::new_v4().to_string();
            let rev = s.tick();
            let f = json!({"id": id, "name": body["name"], "revisionDate": rev, "object": "folder"});
            s.folders.insert(id, f.clone());
            (200, f)
        }
        ("PUT", ["folders", id]) => {
            let rev = s.tick();
            match s.folders.get_mut(*id) {
                Some(f) => {
                    f["name"] = body["name"].clone();
                    f["revisionDate"] = json!(rev);
                    (200, f.clone())
                }
                None => (404, json!({"message": "Folder not found."})),
            }
        }
        ("DELETE", ["folders", id]) => {
            if s.folders.remove(*id).is_none() {
                return (404, json!({"message": "Folder not found."}));
            }
            for c in s.ciphers.values_mut() {
                if c["folderId"].as_str() == Some(id) {
                    c["folderId"] = Value::Null;
                }
            }
            s.tick();
            (200, Value::Null)
        }
        ("POST", ["ciphers"]) => {
            if let Some(m) = too_long(&body) {
                return bad(&m);
            }
            let id = uuid::Uuid::new_v4().to_string();
            let rev = s.tick();
            let mut c = body;
            c["id"] = json!(id);
            c["creationDate"] = json!(rev);
            c["revisionDate"] = json!(rev);
            c["deletedDate"] = Value::Null;
            c["object"] = json!("cipher");
            c.as_object_mut().unwrap().remove("lastKnownRevisionDate");
            s.ciphers.insert(id, c.clone());
            (200, c)
        }
        ("PUT", ["ciphers", id]) => {
            let Some(cur) = s.ciphers.get(*id).cloned() else { return (404, json!({"message": "Resource not found."})) };
            if !cur["deletedDate"].is_null() {
                return (404, json!({"message": "Resource not found."}));
            }
            if let Some(m) = too_long(&body) {
                return bad(&m);
            }
            let parse = |v: &Value| v.as_str().and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok());
            if let (Some(known), Some(current)) = (parse(&body["lastKnownRevisionDate"]), parse(&cur["revisionDate"])) {
                if (current - known).num_milliseconds().abs() >= 1 {
                    return bad("The cipher you are updating is out of date. Please save your work, sync your vault, and try again.");
                }
            }
            let rev = s.tick();
            let mut c = body;
            for k in ["id", "creationDate", "attachments"] {
                if let Some(v) = cur.get(k) {
                    c[k] = v.clone();
                }
            }
            c["revisionDate"] = json!(rev);
            c["deletedDate"] = Value::Null;
            c["object"] = json!("cipher");
            c.as_object_mut().unwrap().remove("lastKnownRevisionDate");
            s.ciphers.insert(id.to_string(), c.clone());
            (200, c)
        }
        ("GET", ["ciphers", id]) => match s.ciphers.get(*id) {
            Some(c) => (200, c.clone()),
            None => (404, json!({"message": "Resource not found."})),
        },
        ("PUT", ["ciphers", id, "delete"]) => {
            let rev = s.tick();
            match s.ciphers.get_mut(*id) {
                Some(c) => {
                    c["deletedDate"] = json!(rev);
                    c["revisionDate"] = json!(rev);
                    (200, Value::Null)
                }
                None => (404, json!({"message": "Resource not found."})),
            }
        }
        _ => (404, json!({"message": "no such endpoint"})),
    }
}
