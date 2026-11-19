//! An in-memory S3 for tests (feature `fake`): objects with ETags,
//! conditional PUTs, HEAD, listing with paging, and a switch to fail
//! requests. Requests must be signed (it checks there's a SigV4
//! authorization header, not the signature itself).

use std::cell::RefCell;
use std::collections::BTreeMap;

use crate::s3::{HttpRequest, HttpResponse, Transport};

#[derive(Default)]
pub struct FakeS3 {
    /// Key → (contents, ETag).
    pub objects: RefCell<BTreeMap<String, (Vec<u8>, String)>>,
    counter: RefCell<u64>,
    /// Every request fails as if offline.
    pub fail: RefCell<bool>,
    /// "METHOD key" of every request.
    pub requests: RefCell<Vec<String>>,
    /// Keys per listing page.
    pub page: usize,
}

impl FakeS3 {
    pub fn new() -> FakeS3 {
        FakeS3 { page: 2, ..Default::default() }
    }

    fn key_of(url: &url::Url) -> String {
        // Virtual-hosted style: the path is the key.
        percent_decode(url.path().trim_start_matches('/'))
    }

    /// Puts an object directly, as another program would.
    pub fn insert(&self, key: &str, data: Vec<u8>) -> String {
        *self.counter.borrow_mut() += 1;
        let etag = format!("\"etag-{}\"", self.counter.borrow());
        self.objects.borrow_mut().insert(key.to_owned(), (data, etag.clone()));
        etag
    }
}

fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            out.push(u8::from_str_radix(&s[i + 1..i + 3], 16).unwrap());
            i += 3;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    String::from_utf8(out).unwrap()
}

fn resp(status: u16, headers: Vec<(String, String)>, body: impl Into<Vec<u8>>) -> HttpResponse {
    HttpResponse { status, headers, body: body.into() }
}

impl Transport for FakeS3 {
    async fn send(&self, req: HttpRequest) -> std::result::Result<HttpResponse, String> {
        if *self.fail.borrow() {
            return Err("offline".into());
        }
        let url = url::Url::parse(&req.url).unwrap();
        assert!(req.headers.iter().any(|(k, v)| k == "authorization" && v.starts_with("AWS4-HMAC-SHA256 ")));
        let key = Self::key_of(&url);
        self.requests.borrow_mut().push(format!("{} {key}", req.method));
        let header = |n: &str| req.headers.iter().find(|(k, _)| k == n).map(|(_, v)| v.clone());
        let current = self.objects.borrow().get(&key).map(|(_, e)| e.clone());
        match req.method.as_str() {
            "GET" if key.is_empty() => {
                let objects = self.objects.borrow();
                let q: BTreeMap<String, String> = url.query_pairs().map(|(k, v)| (k.into_owned(), v.into_owned())).collect();
                let prefix = q.get("prefix").cloned().unwrap_or_default();
                let after = q.get("continuation-token").cloned().unwrap_or_default();
                let keys: Vec<&String> = objects.keys().filter(|k| k.starts_with(&prefix) && **k > after).collect();
                let page: Vec<&String> = keys.iter().take(self.page.max(1)).copied().collect();
                let mut xml = String::from("<ListBucketResult>");
                for k in &page {
                    let etag = &objects[*k].1;
                    xml += &format!("<Contents><Key>{k}</Key><LastModified>2024-01-01T00:00:00.000Z</LastModified><ETag>{}</ETag></Contents>", etag.replace('"', "&quot;"));
                }
                if keys.len() > page.len() {
                    xml += &format!("<IsTruncated>true</IsTruncated><NextContinuationToken>{}</NextContinuationToken>", page.last().unwrap());
                } else {
                    xml += "<IsTruncated>false</IsTruncated>";
                }
                xml += "</ListBucketResult>";
                Ok(resp(200, vec![], xml))
            }
            "GET" => match self.objects.borrow().get(&key) {
                Some((data, etag)) => Ok(resp(200, vec![("ETag".into(), etag.clone())], data.clone())),
                None => Ok(resp(404, vec![], "<Error><Code>NoSuchKey</Code></Error>")),
            },
            "HEAD" => match current {
                Some(etag) => Ok(resp(200, vec![("ETag".into(), etag)], "")),
                None => Ok(resp(404, vec![], "")),
            },
            "PUT" => {
                if header("if-none-match").as_deref() == Some("*") && current.is_some() {
                    return Ok(resp(412, vec![], "<Error><Code>PreconditionFailed</Code></Error>"));
                }
                if let Some(m) = header("if-match") {
                    match current {
                        None => return Ok(resp(404, vec![], "<Error><Code>NoSuchKey</Code></Error>")),
                        Some(c) if c != m => return Ok(resp(412, vec![], "<Error><Code>PreconditionFailed</Code></Error>")),
                        _ => {}
                    }
                }
                let etag = self.insert(&key, req.body);
                Ok(resp(200, vec![("ETag".into(), etag)], ""))
            }
            "DELETE" => {
                self.objects.borrow_mut().remove(&key);
                Ok(resp(204, vec![], ""))
            }
            m => panic!("unexpected {m}"),
        }
    }
}
