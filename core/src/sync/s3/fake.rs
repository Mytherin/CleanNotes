//! A stand-in for S3 in tests: one bucket, path-style URLs, Signature V4
//! checked like S3 does, conditional PUTs, and paged ListObjectsV2.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use chrono::{DateTime, NaiveDateTime, Utc};

use cleanbucket::s3 as sigv4;
use crate::sync::test_http::{self, url_decode, Request, Response};

pub const BUCKET: &str = "notes-bucket";
pub const REGION: &str = "eu-central-1";
pub const ACCESS_KEY: &str = "AKIATESTTESTTEST";
pub const SECRET_KEY: &str = "secret/key+123";

#[derive(Clone, Debug)]
pub struct Obj {
    pub body: Vec<u8>,
    pub etag: String,
    pub modified: DateTime<Utc>,
}

pub struct State {
    pub objects: BTreeMap<String, Obj>,
    /// Keys per listing page.
    pub page_size: usize,
    /// Behave like a service without conditional writes.
    pub ignore_conditions: bool,
    /// Behave like a service that refuses conditional writes (501).
    pub reject_conditions: bool,
    /// Every DELETE fails (503).
    pub refuse_deletes: bool,
    /// Every PUT is refused (403), as for a key that may only read.
    pub refuse_puts: bool,
    /// The next PUT with If-Match finds the object rewritten just before
    /// (another device's write): its ETag changes and the PUT gets 412.
    pub reject_if_match_once: bool,
    /// Keys whose objects can't be read (403), e.g. encrypted with a key
    /// the user may not use.
    pub forbidden: std::collections::HashSet<String>,
    /// "METHOD key" (key empty for listings).
    pub log: Vec<String>,
    /// Written right after the next listing (another device's write
    /// landing mid-sync): key, body.
    pub after_list: Option<(String, String)>,
    counter: u64,
}

pub struct FakeS3 {
    pub url: String,
    pub state: Arc<Mutex<State>>,
}

impl FakeS3 {
    pub fn start() -> FakeS3 {
        let state = Arc::new(Mutex::new(State { objects: BTreeMap::new(), page_size: 1000, ignore_conditions: false, reject_conditions: false, refuse_deletes: false, refuse_puts: false, reject_if_match_once: false, forbidden: Default::default(), log: vec![], after_list: None, counter: 0 }));
        let s = state.clone();
        let url = test_http::spawn(move |req| route(&mut s.lock().unwrap(), req));
        FakeS3 { url, state }
    }

    pub fn state(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap()
    }

    /// Puts an object directly, as if written by another program.
    pub fn put(&self, key: &str, body: &str) {
        self.put_bytes(key, body.as_bytes());
    }

    /// [`FakeS3::put`] with bytes.
    pub fn put_bytes(&self, key: &str, body: &[u8]) {
        let mut s = self.state();
        let etag = s.next_etag();
        s.objects.insert(key.to_owned(), Obj { body: body.to_vec(), etag, modified: Utc::now() });
    }
}

impl Obj {
    /// The object as text.
    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }
}

impl State {
    fn next_etag(&mut self) -> String {
        self.counter += 1;
        format!("\"etag{}\"", self.counter)
    }
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

fn error(status: u16, code: &str) -> Response {
    Response { status, body: format!("<?xml version=\"1.0\"?><Error><Code>{code}</Code><Message>{code}</Message></Error>").into_bytes(), content_type: "application/xml", headers: vec![] }
}

fn signature_ok(req: &Request) -> bool {
    let h = |n: &str| req.headers.get(n).cloned().unwrap_or_default();
    let Ok(t) = NaiveDateTime::parse_from_str(&h("x-amz-date"), "%Y%m%dT%H%M%SZ") else { return false };
    let payload = h("x-amz-content-sha256");
    if payload != sigv4::sha256_hex(&req.body) {
        return false;
    }
    let query: Vec<(String, String)> = req.query.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
    let signed = sigv4::sign(
        &req.method,
        &req.path,
        &sigv4::canonical_query(&query),
        &[("host", h("host")), ("x-amz-content-sha256", payload.clone())],
        &payload,
        ACCESS_KEY,
        SECRET_KEY,
        REGION,
        t.and_utc(),
    );
    signed.authorization == h("authorization")
}

fn route(s: &mut State, req: &Request) -> Response {
    let Some(rest) = req.path.strip_prefix(&format!("/{BUCKET}/")) else { return error(404, "NoSuchBucket") };
    let key = url_decode(rest);
    s.log.push(format!("{} {key}", req.method));
    if !req.headers.get("authorization").is_some_and(|a| a.contains(&format!("Credential={ACCESS_KEY}/"))) {
        return error(403, "InvalidAccessKeyId");
    }
    if !signature_ok(req) {
        return error(403, "SignatureDoesNotMatch");
    }
    let found = |o: &Obj, body: bool| Response {
        status: 200,
        body: if body { o.body.clone() } else { Vec::new() },
        content_type: "text/plain",
        headers: vec![("ETag", o.etag.clone()), ("Last-Modified", o.modified.to_rfc2822())],
    };
    match req.method.as_str() {
        "GET" if key.is_empty() && req.query.get("list-type").map(String::as_str) == Some("2") => {
            let prefix = req.query.get("prefix").cloned().unwrap_or_default();
            let after = req.query.get("continuation-token").cloned();
            let keys: Vec<(&String, &Obj)> = s
                .objects
                .iter()
                .filter(|(k, _)| k.starts_with(&prefix) && after.as_ref().map_or(true, |a| *k > a))
                .collect();
            let page = &keys[..keys.len().min(s.page_size)];
            let truncated = keys.len() > page.len();
            let mut xml = String::from("<?xml version=\"1.0\" encoding=\"UTF-8\"?><ListBucketResult xmlns=\"http://s3.amazonaws.com/doc/2006-03-01/\">");
            for (k, o) in page {
                xml.push_str(&format!(
                    "<Contents><Key>{}</Key><LastModified>{}</LastModified><ETag>{}</ETag><Size>{}</Size><StorageClass>STANDARD</StorageClass></Contents>",
                    xml_escape(k),
                    o.modified.to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
                    xml_escape(&o.etag),
                    o.body.len()
                ));
            }
            xml.push_str(&format!("<IsTruncated>{truncated}</IsTruncated>"));
            if truncated {
                xml.push_str(&format!("<NextContinuationToken>{}</NextContinuationToken>", xml_escape(page.last().unwrap().0)));
            }
            xml.push_str("</ListBucketResult>");
            if let Some((k, body)) = s.after_list.take() {
                let etag = s.next_etag();
                s.objects.insert(k, Obj { body: body.into_bytes(), etag, modified: Utc::now() });
            }
            Response { status: 200, body: xml.into_bytes(), content_type: "application/xml", headers: vec![] }
        }
        "GET" if s.forbidden.contains(&key) => error(403, "AccessDenied"),
        "GET" => match s.objects.get(&key) {
            Some(o) => found(o, true),
            None => error(404, "NoSuchKey"),
        },
        "HEAD" => match s.objects.get(&key) {
            Some(o) => found(o, false),
            None => Response { status: 404, body: Vec::new(), content_type: "text/plain", headers: vec![] },
        },
        "PUT" => {
            if s.refuse_puts {
                return error(403, "AccessDenied");
            }
            if s.reject_if_match_once && req.headers.contains_key("if-match") && s.objects.contains_key(&key) {
                s.reject_if_match_once = false;
                let etag = s.next_etag();
                s.objects.get_mut(&key).unwrap().etag = etag;
                return error(412, "PreconditionFailed");
            }
            let existing = s.objects.get(&key);
            if s.reject_conditions && (req.headers.contains_key("if-match") || req.headers.contains_key("if-none-match")) {
                return error(501, "NotImplemented");
            }
            if !s.ignore_conditions {
                if req.headers.get("if-none-match").map(String::as_str) == Some("*") && existing.is_some() {
                    return error(412, "PreconditionFailed");
                }
                if let Some(m) = req.headers.get("if-match") {
                    match existing {
                        None => return error(404, "NoSuchKey"),
                        Some(o) if o.etag != *m => return error(412, "PreconditionFailed"),
                        _ => {}
                    }
                }
            }
            let etag = s.next_etag();
            s.objects.insert(key, Obj { body: req.body.clone(), etag: etag.clone(), modified: Utc::now() });
            Response { status: 200, body: Vec::new(), content_type: "text/plain", headers: vec![("ETag", etag)] }
        }
        "DELETE" => {
            if s.refuse_deletes {
                return error(503, "ServiceUnavailable");
            }
            // Conditional deletes, as S3 does them.
            if let (Some(m), false) = (req.headers.get("if-match"), s.ignore_conditions) {
                match s.objects.get(&key) {
                    None => return error(404, "NoSuchKey"),
                    Some(o) if o.etag != *m => return error(412, "PreconditionFailed"),
                    _ => {}
                }
            }
            s.objects.remove(&key);
            Response { status: 204, body: Vec::new(), content_type: "text/plain", headers: vec![] }
        }
        _ => error(405, "MethodNotAllowed"),
    }
}
