//! A stand-in for CloudKit Web Services in tests: one container's private
//! database with zones, records with change tags, `records/changes` paging,
//! and web-auth-token sign-in.

use std::collections::{BTreeMap, HashSet};
use std::sync::{Arc, Mutex};

use serde_json::{json, Value};

use crate::sync::test_http::{self, Request, Response};

pub const CONTAINER: &str = "iCloud.com.example.notes";
pub const API_TOKEN: &str = "api-token-123";

pub struct State {
    /// Web auth tokens that work.
    pub tokens: HashSet<String>,
    /// Give out a new web auth token with every response.
    pub rotate_tokens: bool,
    zones: HashSet<String>,
    pub records: BTreeMap<String, Value>,
    counter: u64,
    pub log: Vec<String>,
    url: String,
    /// When set, `records/changes` answers 200 with this raw body.
    pub changes_reply: Option<String>,
}

pub struct FakeCloudKit {
    pub url: String,
    pub state: Arc<Mutex<State>>,
}

impl FakeCloudKit {
    pub fn start() -> FakeCloudKit {
        let state = Arc::new(Mutex::new(State {
            tokens: HashSet::new(),
            rotate_tokens: false,
            zones: HashSet::new(),
            records: BTreeMap::new(),
            counter: 0,
            log: vec![],
            url: String::new(),
            changes_reply: None,
        }));
        let s = state.clone();
        let url = test_http::spawn(move |req| route(&mut s.lock().unwrap(), req));
        state.lock().unwrap().url = url.clone();
        FakeCloudKit { url, state }
    }

    pub fn state(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap()
    }

    /// A web auth token, as if the user had signed in.
    pub fn sign_in(&self) -> String {
        let t = format!("tok+{}/=", uuid::Uuid::new_v4());
        self.state().tokens.insert(t.clone());
        t
    }
}

impl State {
    fn tag(&mut self) -> String {
        self.counter += 1;
        format!("tag{}", self.counter)
    }
}

fn route(s: &mut State, req: &Request) -> Response {
    let prefix = format!("/database/1/{CONTAINER}/");
    let Some(rest) = req.path.strip_prefix(&prefix) else { return Response::json(404, &json!({})) };
    let mut parts = rest.splitn(3, '/');
    let (_env, db, op) = (parts.next().unwrap_or(""), parts.next().unwrap_or(""), parts.next().unwrap_or(""));
    s.log.push(op.to_owned());
    if req.query.get("ckAPIToken").map(String::as_str) != Some(API_TOKEN) || db != "private" {
        return Response::json(401, &json!({"serverErrorCode": "AUTHENTICATION_FAILED", "reason": "bad API token"}));
    }
    let token = req.query.get("ckWebAuthToken").cloned().unwrap_or_default();
    if !s.tokens.contains(&token) {
        return Response::json(
            421,
            &json!({"serverErrorCode": "AUTHENTICATION_REQUIRED", "reason": "request needs authorization",
                    "redirectURL": format!("{}/appleid-signin", s.url)}),
        );
    }
    let body: Value = serde_json::from_str(&req.body).unwrap_or(Value::Null);
    let out = match op {
        "users/current" => json!({"userRecordName": "_user1"}),
        "zones/modify" => {
            for o in body["operations"].as_array().into_iter().flatten() {
                if let Some(z) = o["zone"]["zoneID"]["zoneName"].as_str() {
                    s.zones.insert(z.to_owned());
                }
            }
            json!({"zones": [{"zoneID": {"zoneName": "CleanNotes"}}]})
        }
        "zones/lookup" => {
            if s.zones.contains("CleanNotes") {
                json!({"zones": [{"zoneID": {"zoneName": "CleanNotes"}, "syncToken": format!("zone-{}", s.counter)}]})
            } else {
                json!({"zones": [{"zoneID": {"zoneName": "CleanNotes"}, "serverErrorCode": "ZONE_NOT_FOUND"}]})
            }
        }
        "records/changes" if s.changes_reply.is_some() => {
            return Response { status: 200, body: s.changes_reply.clone().unwrap(), content_type: "text/html", headers: vec![] };
        }
        "records/changes" => {
            if !s.zones.contains("CleanNotes") {
                return Response::json(400, &json!({"serverErrorCode": "ZONE_NOT_FOUND", "reason": "no zone"}));
            }
            let limit = body["resultsLimit"].as_u64().unwrap_or(200) as usize;
            let from: usize = body["syncToken"].as_str().and_then(|t| t.strip_prefix("page:")).and_then(|n| n.parse().ok()).unwrap_or(0);
            let all: Vec<&Value> = s.records.values().collect();
            let page: Vec<Value> = all.iter().skip(from).take(limit).map(|v| (*v).clone()).collect();
            let more = from + page.len() < all.len();
            let token = if more { format!("page:{}", from + page.len()) } else { format!("zone-{}", s.counter) };
            json!({"records": page, "syncToken": token, "moreComing": more, "zoneID": {"zoneName": "CleanNotes"}})
        }
        "records/modify" => {
            let mut results = Vec::new();
            for o in body["operations"].as_array().cloned().unwrap_or_default() {
                results.push(modify(s, &o));
            }
            json!({"records": results})
        }
        _ => return Response::json(404, &json!({"serverErrorCode": "NOT_FOUND", "reason": "no such operation"})),
    };
    let mut resp = Response::json(200, &out);
    if s.rotate_tokens {
        let t = format!("tok+{}", uuid::Uuid::new_v4());
        s.tokens.insert(t.clone());
        resp.headers.push(("X-Apple-CloudKit-Web-Auth-Token", t));
    }
    resp
}

fn error(name: &str, code: &str) -> Value {
    json!({"recordName": name, "serverErrorCode": code, "reason": code.to_lowercase()})
}

fn modify(s: &mut State, o: &Value) -> Value {
    let rec = &o["record"];
    let name = rec["recordName"].as_str().unwrap_or("").to_owned();
    let existing = s.records.get(&name).cloned();
    match o["operationType"].as_str().unwrap_or("") {
        "create" => {
            if existing.is_some() {
                return error(&name, "CONFLICT");
            }
        }
        "replace" | "update" => {
            let Some(cur) = &existing else { return error(&name, "NOT_FOUND") };
            if cur["recordChangeTag"] != rec["recordChangeTag"] {
                return error(&name, "CONFLICT");
            }
        }
        "forceReplace" | "forceUpdate" => {}
        "forceDelete" | "delete" => {
            if o["operationType"] == "delete" && existing.as_ref().is_some_and(|cur| cur["recordChangeTag"] != rec["recordChangeTag"]) {
                return error(&name, "CONFLICT");
            }
            return match s.records.remove(&name) {
                Some(_) => {
                    s.tag();
                    json!({"recordName": name, "deleted": true})
                }
                None => error(&name, "NOT_FOUND"),
            };
        }
        other => return error(&name, &format!("BAD_OPERATION_{other}")),
    }
    let tag = s.tag();
    let saved = json!({
        "recordName": name,
        "recordType": rec["recordType"],
        "recordChangeTag": tag,
        "fields": rec["fields"],
        "created": existing.as_ref().map_or(json!({"timestamp": 1_763_200_000_000i64}), |e| e["created"].clone()),
    });
    s.records.insert(name, saved.clone());
    saved
}
