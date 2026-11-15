//! A stand-in Standard Notes server for tests, on 127.0.0.1.
//!
//! Like the real one it only stores what clients send (protocol strings it
//! can't read), hands out key params and checks the PKCE code verifier at
//! login, can ask for a two-factor code, expires access tokens (status 498)
//! and rotates refresh tokens, and syncs items by timestamp: a save based on
//! an outdated `updated_at_timestamp` is a `sync_conflict` with the server's
//! item; downloads come in pages (`cursor_token`) and later syncs only bring
//! what changed since the `sync_token` (deletions included). The sync token
//! changes with every request, as the real one (a time) does.
//!
//! The test helpers on [`State`] act as another Standard Notes client: they
//! encrypt and decrypt with the account's keys.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::{Arc, Mutex};

use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine;
use serde_json::{json, Value};

use super::crypto::{self, Encrypted, ItemIds};
use super::items::{iso, APP_DOMAIN};
use crate::sync::test_http::{self, Request, Response};

pub const MFA_CODE: &str = "424242";
const START: i64 = 1_763_200_000_000_000;

pub struct State {
    /// The email the account was registered with (its key identifier).
    pub email: String,
    pw_nonce: String,
    server_password: String,
    /// The account's master key and key params, for acting as another client.
    pub master_key: String,
    pub key_params: String,
    pub user_uuid: String,
    pub mfa: bool,
    challenge: Option<String>,
    pub items: BTreeMap<String, Value>,
    clock: i64,
    tokens: HashMap<String, bool>,
    refresh_tokens: HashSet<String>,
    /// The most items a sync answer holds.
    pub limit: usize,
    /// Requests served, as "METHOD /path".
    pub log: Vec<String>,
}

fn ts(v: &Value) -> i64 {
    v["updated_at_timestamp"].as_i64().unwrap_or(0)
}

fn token(us: i64) -> String {
    B64.encode(format!("2:{us}"))
}

fn untoken(t: &str) -> Option<i64> {
    let s = String::from_utf8(B64.decode(t).ok()?).ok()?;
    s.strip_prefix("2:")?.parse().ok()
}

fn date(us: i64) -> String {
    iso(chrono::DateTime::from_timestamp_micros(us).unwrap())
}

impl State {
    fn tick(&mut self) -> i64 {
        self.clock += 1_000;
        START + self.clock
    }

    fn issue(&mut self) -> Value {
        let access = uuid::Uuid::new_v4().to_string();
        let refresh = uuid::Uuid::new_v4().to_string();
        self.tokens.insert(access.clone(), true);
        self.refresh_tokens.insert(refresh.clone());
        let now = chrono::Utc::now().timestamp_millis();
        json!({
            "access_token": access,
            "refresh_token": refresh,
            "access_expiration": now + 3_600_000,
            "refresh_expiration": now + 86_400_000,
            "readonly_access": false,
        })
    }

    /// Makes every access token expire.
    pub fn expire_tokens(&mut self) {
        for v in self.tokens.values_mut() {
            *v = false;
        }
    }

    /// Ends every session (signed out elsewhere).
    pub fn revoke_sessions(&mut self) {
        self.tokens.clear();
        self.refresh_tokens.clear();
    }

    fn store(&mut self, uuid: &str, content_type: &str, enc: Encrypted, items_key_id: Option<&str>) {
        let t = self.tick();
        let created = self.items.get(uuid).map(|i| (i["created_at"].clone(), i["created_at_timestamp"].clone()));
        let (ca, cat) = created.unwrap_or((json!(date(t)), json!(t)));
        self.items.insert(
            uuid.to_owned(),
            json!({
                "uuid": uuid, "content_type": content_type,
                "content": enc.content, "enc_item_key": enc.enc_item_key, "items_key_id": items_key_id,
                "created_at": ca, "created_at_timestamp": cat,
                "updated_at": date(t), "updated_at_timestamp": t,
                "deleted": false, "duplicate_of": null, "user_uuid": self.user_uuid,
                "key_system_identifier": null, "shared_vault_uuid": null,
            }),
        );
    }

    /// Adds an items key as another client would. Returns its uuid.
    pub fn add_items_key(&mut self, default: bool) -> String {
        let uuid = uuid::Uuid::new_v4().to_string();
        let content = json!({"itemsKey": crypto::random_hex(32), "version": "004", "isDefault": default, "references": [], "appData": {APP_DOMAIN: {}}});
        let enc = crypto::encrypt_item(&uuid, &content, &self.master_key, Some(&self.key_params)).unwrap();
        self.store(&uuid, "SN|ItemsKey", enc, None);
        uuid
    }

    fn items_key(&self) -> (String, String) {
        let mut keys: Vec<(bool, String, String)> = self
            .items
            .values()
            .filter(|i| i["content_type"] == "SN|ItemsKey" && !i["deleted"].as_bool().unwrap())
            .map(|i| {
                let c = self.content(i["uuid"].as_str().unwrap());
                (c["isDefault"] == true, i["uuid"].as_str().unwrap().to_owned(), c["itemsKey"].as_str().unwrap().to_owned())
            })
            .collect();
        keys.sort();
        let (_, id, key) = keys.pop().expect("the account has no items key");
        (id, key)
    }

    /// Saves an item's content as another client would.
    pub fn put(&mut self, uuid: &str, content_type: &str, content: &Value) {
        let (key_id, key) = self.items_key();
        let enc = crypto::encrypt_item(uuid, content, &key, None).unwrap();
        self.store(uuid, content_type, enc, Some(&key_id));
    }

    pub fn add(&mut self, content_type: &str, content: Value) -> String {
        let uuid = uuid::Uuid::new_v4().to_string();
        self.put(&uuid, content_type, &content);
        uuid
    }

    pub fn add_tag(&mut self, title: &str, parent: Option<&str>, notes: &[&str]) -> String {
        let mut refs: Vec<Value> = notes.iter().map(|n| json!({"uuid": n, "content_type": "Note"})).collect();
        if let Some(p) = parent {
            refs.push(json!({"uuid": p, "content_type": "Tag", "reference_type": "TagToParentTag"}));
        }
        self.add("Tag", json!({"title": title, "references": refs, "expanded": true, "appData": {APP_DOMAIN: {}}}))
    }

    /// An item's decrypted content.
    pub fn content(&self, uuid: &str) -> Value {
        let it = &self.items[uuid];
        let enc = Encrypted { content: it["content"].as_str().unwrap().into(), enc_item_key: it["enc_item_key"].as_str().unwrap().into() };
        let ids = ItemIds { uuid, key_system_identifier: None, shared_vault_uuid: None };
        let key = match it["items_key_id"].as_str() {
            None => self.master_key.clone(),
            Some(k) => self.content(k)["itemsKey"].as_str().unwrap().to_owned(),
        };
        crypto::decrypt_item(ids, &enc, &key).unwrap()
    }

    /// Changes an item as another client would.
    pub fn edit(&mut self, uuid: &str, f: impl FnOnce(&mut Value)) {
        let mut c = self.content(uuid);
        f(&mut c);
        let ct = self.items[uuid]["content_type"].as_str().unwrap().to_owned();
        self.put(uuid, &ct, &c);
    }

    /// Deletes an item as another client would.
    pub fn delete(&mut self, uuid: &str) {
        let t = self.tick();
        let it = self.items.get_mut(uuid).unwrap();
        it["deleted"] = json!(true);
        it["content"] = Value::Null;
        it["enc_item_key"] = Value::Null;
        it["updated_at_timestamp"] = json!(t);
        it["updated_at"] = json!(date(t));
    }

    /// The live items of a type.
    pub fn live(&self, content_type: &str) -> Vec<String> {
        self.items
            .values()
            .filter(|i| i["content_type"] == content_type && i["deleted"] != true)
            .map(|i| i["uuid"].as_str().unwrap().to_owned())
            .collect()
    }

    /// The live tag with this title.
    pub fn tag(&self, title: &str) -> String {
        self.live("Tag").into_iter().find(|t| self.content(t)["title"] == title).unwrap_or_else(|| panic!("no tag {title}"))
    }
}

pub struct FakeServer {
    pub url: String,
    pub state: Arc<Mutex<State>>,
}

impl FakeServer {
    pub fn start(email: &str, password: &str) -> FakeServer {
        let pw_nonce = crypto::random_hex(32);
        let root = crypto::root_key(password, email, &pw_nonce).unwrap();
        let key_params = format!(r#"{{"identifier":"{email}","pw_nonce":"{pw_nonce}","version":"004","origination":"registration","created":"1700000000000"}}"#);
        let state = State {
            email: email.to_owned(),
            pw_nonce,
            server_password: root.server_password,
            master_key: root.master_key,
            key_params,
            user_uuid: uuid::Uuid::new_v4().to_string(),
            mfa: false,
            challenge: None,
            items: BTreeMap::new(),
            clock: 0,
            tokens: HashMap::new(),
            refresh_tokens: HashSet::new(),
            limit: 150,
            log: vec![],
        };
        let state = Arc::new(Mutex::new(state));
        let s = state.clone();
        let url = test_http::spawn(move |req| {
            let mut s = s.lock().unwrap();
            s.log.push(format!("{} {}", req.method, req.path));
            let (status, body) = route(&mut s, req);
            Response::json(status, &body)
        });
        FakeServer { url, state }
    }

    pub fn state(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap()
    }
}

fn error(status: u16, tag: Option<&str>, message: &str) -> (u16, Value) {
    let mut e = json!({"message": message});
    if let Some(t) = tag {
        e["tag"] = json!(t);
    }
    (status, json!({"error": e}))
}

fn route(s: &mut State, req: &Request) -> (u16, Value) {
    let body: Value = serde_json::from_str(&req.body).unwrap_or(Value::Null);
    if req.method != "POST" || body["api"] != super::api::API_VERSION {
        return error(400, Some("invalid-parameters"), "bad request");
    }
    match req.path.as_str() {
        "/v2/login-params" => {
            let email = body["email"].as_str().unwrap_or("");
            if !email.eq_ignore_ascii_case(&s.email) {
                // Made-up params, so accounts can't be probed for.
                return (200, json!({"identifier": email, "pw_nonce": crypto::sha256_hex(email), "version": "004"}));
            }
            if s.mfa {
                match body["mfa_code"].as_str() {
                    None => return error(401, Some("mfa-required"), "Please enter your two-factor authentication code."),
                    Some(c) if c != MFA_CODE => return error(401, Some("mfa-invalid"), "The two-factor authentication code you entered is incorrect."),
                    Some(_) => {}
                }
            }
            s.challenge = body["code_challenge"].as_str().map(str::to_owned);
            (200, json!({"identifier": email, "pw_nonce": s.pw_nonce, "version": "004", "origination": "registration", "created": "1700000000000"}))
        }
        "/v2/login" => {
            let verifier = body["code_verifier"].as_str().unwrap_or("");
            if s.challenge.take() != Some(crypto::code_challenge(verifier)) {
                return error(401, Some("invalid-auth"), "Invalid login code.");
            }
            let email_ok = body["email"].as_str().is_some_and(|e| e.eq_ignore_ascii_case(&s.email));
            if !email_ok || body["password"].as_str() != Some(&s.server_password) {
                return error(401, None, "Invalid email or password");
            }
            let session = s.issue();
            let key_params: Value = serde_json::from_str(&s.key_params).unwrap();
            (200, json!({"data": {"session": session, "key_params": key_params, "user": {"uuid": s.user_uuid, "email": s.email}}}))
        }
        "/v1/sessions/refresh" => {
            let r = body["refresh_token"].as_str().unwrap_or("").to_owned();
            if !s.refresh_tokens.remove(&r) || !s.tokens.contains_key(body["access_token"].as_str().unwrap_or("")) {
                return error(400, Some("invalid-refresh-token"), "The refresh token is not valid.");
            }
            s.tokens.remove(body["access_token"].as_str().unwrap());
            let session = s.issue();
            (200, json!({"session": session}))
        }
        "/v1/items" => {
            let token = req.headers.get("authorization").and_then(|a| a.strip_prefix("Bearer ")).unwrap_or("");
            match s.tokens.get(token) {
                Some(true) => sync(s, &body),
                Some(false) => error(498, Some("expired-access-token"), "The provided access token has expired."),
                None => error(401, Some("invalid-auth"), "Invalid login credentials."),
            }
        }
        _ => error(404, None, "no such endpoint"),
    }
}

fn sync(s: &mut State, body: &Value) -> (u16, Value) {
    let mut saved = Vec::new();
    let mut conflicts = Vec::new();
    let mut saved_ids = HashSet::new();
    for it in body["items"].as_array().into_iter().flatten() {
        let uuid = it["uuid"].as_str().unwrap_or("").to_owned();
        let existing = s.items.get(&uuid).cloned();
        if let Some(e) = &existing {
            if ts(it) != ts(e) {
                conflicts.push(json!({"type": "sync_conflict", "server_item": e}));
                continue;
            }
        }
        let deleted = it["deleted"] == true;
        if !deleted {
            for k in ["content", "enc_item_key"] {
                assert!(it[k].as_str().is_some_and(|c| c.starts_with("004:")), "only protocol 004 strings reach the server");
            }
        }
        let t = s.tick();
        let mut stored = it.clone();
        stored["updated_at_timestamp"] = json!(t);
        stored["updated_at"] = json!(date(t));
        let created = existing.as_ref().map(|e| e["created_at_timestamp"].clone()).filter(|c| c.as_i64().is_some_and(|c| c > 0));
        stored["created_at_timestamp"] = created.or_else(|| it.get("created_at_timestamp").cloned().filter(|c| c.as_i64().is_some())).unwrap_or(json!(t));
        if let Some(e) = &existing {
            stored["created_at"] = e["created_at"].clone();
        }
        stored["user_uuid"] = json!(s.user_uuid);
        if deleted {
            stored["content"] = Value::Null;
            stored["enc_item_key"] = Value::Null;
            stored["items_key_id"] = Value::Null;
        }
        s.items.insert(uuid.clone(), stored.clone());
        saved.push(json!({
            "uuid": uuid, "content_type": stored["content_type"], "deleted": deleted,
            "created_at": stored["created_at"], "created_at_timestamp": stored["created_at_timestamp"],
            "updated_at": stored["updated_at"], "updated_at_timestamp": t, "duplicate_of": null,
        }));
        saved_ids.insert(uuid);
    }
    let cursor = body["cursor_token"].as_str().and_then(untoken);
    let since = body["sync_token"].as_str().and_then(untoken);
    let initial = cursor.is_none() && since.is_none();
    let from = cursor.or(since).unwrap_or(i64::MIN);
    let mut matching: Vec<&Value> = s
        .items
        .values()
        .filter(|i| ts(i) > from && !saved_ids.contains(i["uuid"].as_str().unwrap()) && !(initial && i["deleted"] == true))
        .collect();
    matching.sort_by_key(|i| ts(i));
    let limit = body["limit"].as_u64().map_or(s.limit, |l| (l as usize).min(s.limit));
    let more = matching.len() > limit;
    let page: Vec<Value> = matching.into_iter().take(limit).cloned().collect();
    let mut out = json!({"retrieved_items": page, "saved_items": saved, "conflicts": conflicts});
    if more {
        out["cursor_token"] = json!(token(ts(page.last().unwrap())));
    }
    let now = s.tick();
    out["sync_token"] = json!(token(now));
    (200, out)
}
