//! A small S3 client: signed GET, HEAD, PUT (conditional or not), DELETE
//! and listing, over a [`Transport`] the app supplies, so the same code
//! runs in a browser (on `fetch`, as WebAssembly), on iOS and on the
//! desktop.
//!
//! Requests are signed with AWS Signature Version 4 by hand (the AWS SDK
//! needs an async runtime and doesn't build for WebAssembly). Writes that
//! update are conditional: `If-Match: <etag>` for updates and
//! `If-None-Match: *` for new objects, so S3 refuses a write based on an
//! outdated copy instead of losing another device's change (S3 has
//! supported both since 2024; Cloudflare R2 and MinIO do too).
//!
//! **Change marker.** [`Client::touch`] rewrites a small object after a
//! device's writes, and [`Client::head`] reads its ETag: a device that has
//! nothing to send and sees the ETag it saw last time knows nothing changed,
//! with one `HEAD`. Every writer must touch it; readers should still list
//! everything now and then, since a writer can stop between writing and
//! touching.

use std::future::Future;

use chrono::{DateTime, Utc};
use hmac::{Hmac, Mac};
use sha2::{Digest, Sha256};
use zeroize::{Zeroize, ZeroizeOnDrop};

use crate::{Error, Result};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HttpRequest {
    pub method: String,
    pub url: String,
    /// Lowercase names. `host` isn't among them: it follows from the URL
    /// (and browsers refuse to set it).
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HttpResponse {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl HttpResponse {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.iter().find(|(k, _)| k.eq_ignore_ascii_case(name)).map(|(_, v)| v.as_str())
    }

    pub fn ok(&self) -> bool {
        (200..300).contains(&self.status)
    }

    /// Refused because of `If-Match` / `If-None-Match` (or a concurrent
    /// conditional write), or the object to update is gone.
    pub fn conflict(&self) -> bool {
        matches!(self.status, 404 | 409 | 412) && error_code(self) != "NoSuchBucket"
    }

    /// The error S3 answered with.
    pub fn error(&self) -> S3Error {
        let body = String::from_utf8_lossy(&self.body);
        let region = self.header("x-amz-bucket-region").map(str::to_owned).or_else(|| xml_tag(&body, "Region").map(decode)).filter(|s| !s.is_empty());
        S3Error { status: self.status, code: error_code(self), message: xml_tag(&body, "Message").map(decode).unwrap_or_default(), region }
    }
}

/// Sends HTTP requests. Must not follow redirects (S3 answers a request
/// for a bucket in another region with one, and the signature doesn't
/// carry over); return the 3xx response instead. The future needn't be
/// `Send` (it isn't, on WebAssembly); a blocking client returns a ready one
/// and is driven with [`crate::block_on`].
pub trait Transport {
    fn send(&self, request: HttpRequest) -> impl Future<Output = std::result::Result<HttpResponse, String>>;
}

/// An error S3 answered with.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct S3Error {
    pub status: u16,
    pub code: String,
    pub message: String,
    /// The bucket's region, when S3 named it.
    pub region: Option<String>,
}

impl S3Error {
    /// The access key itself is refused (wrong, deleted or expired).
    pub fn is_auth(&self) -> bool {
        matches!(self.code.as_str(), "InvalidAccessKeyId" | "SignatureDoesNotMatch" | "InvalidToken" | "ExpiredToken")
    }
}

impl std::fmt::Display for S3Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match (self.status, self.code.as_str()) {
            (301 | 307, _) | (_, "PermanentRedirect" | "TemporaryRedirect" | "AuthorizationHeaderMalformed") => match &self.region {
                Some(r) => write!(f, "the bucket is in region {r}; enter that as its region"),
                None => write!(f, "the bucket is in another region than the one entered ({} {})", self.status, self.code),
            },
            (_, "NoSuchBucket") => f.write_str("there's no bucket with that name"),
            (_, "AccessDenied") => write!(f, "S3 denied access; check that the IAM policy allows this bucket and folder ({})", self.message),
            (_, "InvalidAccessKeyId") => f.write_str("S3 doesn't know that access key ID"),
            (_, "SignatureDoesNotMatch") => f.write_str("S3 refused the secret access key"),
            (_, "InvalidToken" | "ExpiredToken") => f.write_str("the session token is invalid or has expired"),
            (_, "RequestTimeTooSkewed") => f.write_str("this device's clock is off, so S3 refuses the request"),
            (s, "") => write!(f, "S3 answered {s}"),
            (s, c) => write!(f, "S3 answered {s} {c}: {}", self.message),
        }
    }
}

impl std::error::Error for S3Error {}

/// Where things are kept: a bucket and a folder in it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Location {
    pub bucket: String,
    pub region: String,
    /// The folder (key prefix), e.g. `CleanNotes`; empty for the top.
    pub prefix: String,
    /// An S3-compatible service's address (Cloudflare R2, MinIO, tests),
    /// used with path-style URLs. `None` for Amazon S3.
    pub endpoint: Option<String>,
}

impl Location {
    /// The key prefix: the folder with a trailing `/`, or nothing.
    pub fn root(&self) -> String {
        let p = self.prefix.trim().trim_matches('/');
        if p.is_empty() {
            String::new()
        } else {
            format!("{p}/")
        }
    }

    /// Base URL and encoded path for a key (`""` for the bucket itself).
    pub fn url_parts(&self, key: &str) -> (String, String) {
        let key = uri_encode(key, true);
        let bucket = uri_encode(self.bucket.trim(), false);
        let region = self.region.trim();
        match self.endpoint.as_deref().map(str::trim).filter(|e| !e.is_empty()) {
            Some(e) => (e.trim_end_matches('/').to_owned(), format!("/{bucket}/{key}")),
            // A bucket name with dots doesn't match the wildcard certificate.
            None if self.bucket.contains('.') => (format!("https://s3.{region}.amazonaws.com"), format!("/{bucket}/{key}")),
            None => (format!("https://{}.s3.{region}.amazonaws.com", self.bucket.trim()), format!("/{key}")),
        }
    }
}

/// An IAM access key. Wiped from memory when dropped.
#[derive(Clone, Default, PartialEq, Eq, Zeroize, ZeroizeOnDrop)]
pub struct Credentials {
    pub access_key_id: String,
    pub secret_access_key: String,
    /// For temporary credentials (STS).
    pub session_token: Option<String>,
}

impl std::fmt::Debug for Credentials {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Credentials").field("access_key_id", &self.access_key_id).field("secret_access_key", &"…").finish()
    }
}

/// How a write is conditioned.
#[derive(Clone, Copy, Debug)]
pub enum Cond<'a> {
    /// Only if the object is still at this ETag.
    Match(&'a str),
    /// Only if there's no such object yet.
    Absent,
    /// Whatever is there (the change marker).
    None,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Put {
    Stored { etag: String },
    /// Refused: the object changed (or appeared, or is gone) since.
    Conflict,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Object {
    pub data: Vec<u8>,
    pub etag: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Listed {
    /// Relative to the folder.
    pub name: String,
    pub etag: String,
    pub modified: Option<DateTime<Utc>>,
}

pub struct Client<'a, T: Transport> {
    location: &'a Location,
    credentials: &'a Credentials,
    transport: &'a T,
}

impl<'a, T: Transport> Client<'a, T> {
    pub fn new(location: &'a Location, credentials: &'a Credentials, transport: &'a T) -> Self {
        Client { location, credentials, transport }
    }

    pub fn location(&self) -> &Location {
        self.location
    }

    /// Sends a signed request for `key` (a whole key, not relative to the
    /// folder; `""` for the bucket), with `extra` headers unsigned.
    pub async fn send(&self, method: &str, key: &str, query: &[(String, String)], body: Vec<u8>, extra: &[(&str, String)]) -> Result<HttpResponse> {
        let (base, path) = self.location.url_parts(key);
        let q = canonical_query(query);
        let full = if q.is_empty() { format!("{base}{path}") } else { format!("{base}{path}?{q}") };
        let url = url::Url::parse(&full).map_err(|_| Error::invalid(format!("the S3 address {full} isn't valid")))?;
        let host = match url.port() {
            Some(p) => format!("{}:{p}", url.host_str().unwrap_or_default()),
            None => url.host_str().unwrap_or_default().to_owned(),
        };
        let payload = sha256_hex(&body);
        let mut signed_headers = vec![("host", host), ("x-amz-content-sha256", payload.clone())];
        if let Some(t) = self.credentials.session_token.as_deref().filter(|t| !t.is_empty()) {
            signed_headers.push(("x-amz-security-token", t.to_owned()));
        }
        let signed = sign(
            method,
            &path,
            &q,
            &signed_headers,
            &payload,
            self.credentials.access_key_id.trim(),
            self.credentials.secret_access_key.trim(),
            self.location.region.trim(),
            Utc::now(),
        );
        let mut headers: Vec<(String, String)> = signed_headers.into_iter().filter(|(k, _)| *k != "host").map(|(k, v)| (k.to_owned(), v)).collect();
        headers.push(("x-amz-date".into(), signed.amz_date));
        headers.push(("authorization".into(), signed.authorization));
        headers.extend(extra.iter().map(|(k, v)| (k.to_string(), v.clone())));
        let req = HttpRequest { method: method.to_owned(), url: full, headers, body };
        self.transport.send(req).await.map_err(Error::Network)
    }

    fn key(&self, name: &str) -> String {
        format!("{}{name}", self.location.root())
    }

    /// An object in the folder, `None` if there's none.
    pub async fn get(&self, name: &str) -> Result<Option<Object>> {
        let r = self.send("GET", &self.key(name), &[], Vec::new(), &[]).await?;
        if r.status == 404 && error_code(&r) != "NoSuchBucket" {
            return Ok(None);
        }
        if !r.ok() {
            return Err(r.error().into());
        }
        let etag = r.header("etag").ok_or_else(|| Error::invalid("S3 sent no ETag"))?.to_owned();
        Ok(Some(Object { data: r.body, etag }))
    }

    /// An object's ETag, without its contents; `None` if there's none.
    pub async fn head(&self, name: &str) -> Result<Option<String>> {
        let r = self.send("HEAD", &self.key(name), &[], Vec::new(), &[]).await?;
        if r.status == 404 {
            return Ok(None);
        }
        if !r.ok() {
            return Err(r.error().into());
        }
        Ok(Some(r.header("etag").ok_or_else(|| Error::invalid("S3 sent no ETag"))?.to_owned()))
    }

    pub async fn put(&self, name: &str, data: Vec<u8>, cond: Cond<'_>) -> Result<Put> {
        let mut extra = vec![("content-type", "application/octet-stream".to_owned())];
        match cond {
            Cond::Match(e) => extra.push(("if-match", e.to_owned())),
            Cond::Absent => extra.push(("if-none-match", "*".to_owned())),
            Cond::None => {}
        }
        let r = self.send("PUT", &self.key(name), &[], data, &extra).await?;
        if !matches!(cond, Cond::None) && r.conflict() {
            return Ok(Put::Conflict);
        }
        if !r.ok() {
            return Err(r.error().into());
        }
        let etag = r.header("etag").ok_or_else(|| Error::invalid("S3 sent no ETag"))?.to_owned();
        Ok(Put::Stored { etag })
    }

    pub async fn delete(&self, name: &str) -> Result<()> {
        let r = self.send("DELETE", &self.key(name), &[], Vec::new(), &[]).await?;
        if !r.ok() && r.status != 404 {
            return Err(r.error().into());
        }
        Ok(())
    }

    /// Rewrites the change marker `name` (see the module's docs), after a
    /// device's writes. Returns its new ETag.
    pub async fn touch(&self, name: &str) -> Result<String> {
        let mark: String = crate::seal::random_bytes::<16>().iter().map(|b| format!("{b:02x}")).collect();
        match self.put(name, mark.into_bytes(), Cond::None).await? {
            Put::Stored { etag } => Ok(etag),
            Put::Conflict => Err(Error::invalid("S3 refused to write the change marker")),
        }
    }

    /// Every object whose name (relative to the folder) starts with
    /// `prefix`. Anything short of the whole list is an error.
    pub async fn list(&self, prefix: &str) -> Result<Vec<Listed>> {
        let root = self.location.root();
        let mut out = Vec::new();
        let mut token: Option<String> = None;
        loop {
            let mut q = vec![("list-type".to_owned(), "2".to_owned()), ("prefix".to_owned(), format!("{root}{prefix}"))];
            if let Some(t) = &token {
                q.push(("continuation-token".to_owned(), t.clone()));
            }
            let r = self.send("GET", "", &q, Vec::new(), &[]).await?;
            if !r.ok() {
                return Err(r.error().into());
            }
            let xml = String::from_utf8_lossy(&r.body);
            if !xml.contains("<ListBucketResult") {
                return Err(Error::invalid("S3's reply isn't a bucket listing (a proxy or captive portal in the way?)"));
            }
            for c in xml_tags(&xml, "Contents") {
                let (Some(key), Some(etag)) = (xml_tag(c, "Key"), xml_tag(c, "ETag")) else { continue };
                let key = decode(key);
                let Some(name) = key.strip_prefix(&root) else { continue };
                let modified = xml_tag(c, "LastModified").and_then(|m| DateTime::parse_from_rfc3339(m).ok()).map(|t| t.with_timezone(&Utc));
                out.push(Listed { name: name.to_owned(), etag: decode(etag), modified });
            }
            match (xml_tag(&xml, "IsTruncated"), xml_tag(&xml, "NextContinuationToken").map(decode)) {
                (Some("true"), Some(t)) if token.as_deref() != Some(t.as_str()) => token = Some(t),
                (Some("true"), _) => return Err(Error::invalid("S3 said the listing goes on but gave no way to continue it")),
                _ => return Ok(out),
            }
        }
    }
}

/// The `<Code>` of an S3 error reply.
pub fn error_code(r: &HttpResponse) -> String {
    xml_tag(&String::from_utf8_lossy(&r.body), "Code").map(decode).unwrap_or_default()
}

// ---- Signature Version 4 ----------------------------------------------------

pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

pub fn sha256_hex(data: &[u8]) -> String {
    hex(&Sha256::digest(data))
}

fn hmac(key: &[u8], data: &str) -> Vec<u8> {
    let mut m = <Hmac<Sha256> as Mac>::new_from_slice(key).expect("HMAC takes any key length");
    m.update(data.as_bytes());
    m.finalize().into_bytes().to_vec()
}

/// URI-encodes as AWS wants: everything but unreserved characters, and `/`
/// too unless `keep_slash`.
pub fn uri_encode(s: &str, keep_slash: bool) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => out.push(b as char),
            b'/' if keep_slash => out.push('/'),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// The canonical query string: pairs encoded and sorted.
pub fn canonical_query(params: &[(String, String)]) -> String {
    let mut enc: Vec<(String, String)> = params.iter().map(|(k, v)| (uri_encode(k, false), uri_encode(v, false))).collect();
    enc.sort();
    enc.iter().map(|(k, v)| format!("{k}={v}")).collect::<Vec<_>>().join("&")
}

pub struct Signed {
    pub authorization: String,
    pub amz_date: String,
}

/// Signs a request. `path` is the encoded path, `query` the canonical query
/// string, and `headers` the headers to sign (including `host` and
/// `x-amz-content-sha256`; `x-amz-date` is added).
#[allow(clippy::too_many_arguments)]
pub fn sign(method: &str, path: &str, query: &str, headers: &[(&str, String)], payload_hash: &str, access_key: &str, secret_key: &str, region: &str, now: DateTime<Utc>) -> Signed {
    let amz_date = now.format("%Y%m%dT%H%M%SZ").to_string();
    let date = now.format("%Y%m%d").to_string();
    let mut hs: Vec<(String, String)> = headers.iter().map(|(k, v)| (k.to_lowercase(), v.trim().to_owned())).collect();
    hs.push(("x-amz-date".into(), amz_date.clone()));
    hs.sort();
    let canonical_headers: String = hs.iter().map(|(k, v)| format!("{k}:{v}\n")).collect();
    let signed_headers = hs.iter().map(|(k, _)| k.as_str()).collect::<Vec<_>>().join(";");
    let canonical = format!("{method}\n{path}\n{query}\n{canonical_headers}\n{signed_headers}\n{payload_hash}");
    let scope = format!("{date}/{region}/s3/aws4_request");
    let to_sign = format!("AWS4-HMAC-SHA256\n{amz_date}\n{scope}\n{}", sha256_hex(canonical.as_bytes()));
    let k = hmac(format!("AWS4{secret_key}").as_bytes(), &date);
    let k = hmac(&k, region);
    let k = hmac(&k, "s3");
    let k = hmac(&k, "aws4_request");
    let signature = hex(&hmac(&k, &to_sign));
    Signed { authorization: format!("AWS4-HMAC-SHA256 Credential={access_key}/{scope},SignedHeaders={signed_headers},Signature={signature}"), amz_date }
}

// ---- XML ------------------------------------------------------------------

/// The text inside the first `<tag>…</tag>`.
pub fn xml_tag<'x>(xml: &'x str, tag: &str) -> Option<&'x str> {
    let open = format!("<{tag}>");
    let start = xml.find(&open)? + open.len();
    let end = xml[start..].find(&format!("</{tag}>"))? + start;
    Some(&xml[start..end])
}

/// Every `<tag>…</tag>`'s text.
pub fn xml_tags<'x>(xml: &'x str, tag: &str) -> Vec<&'x str> {
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

/// XML character references and the predefined entities.
pub fn decode(s: &str) -> String {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::block_on;
    use crate::fake::FakeS3;

    /// The "GET Object" example from AWS's Signature Version 4 docs.
    #[test]
    fn aws_documented_example() {
        let now = DateTime::parse_from_rfc3339("2013-05-24T00:00:00Z").unwrap().with_timezone(&Utc);
        let empty = sha256_hex(b"");
        let s = sign(
            "GET",
            "/test.txt",
            "",
            &[("host", "examplebucket.s3.amazonaws.com".into()), ("range", "bytes=0-9".into()), ("x-amz-content-sha256", empty.clone())],
            &empty,
            "AKIAIOSFODNN7EXAMPLE",
            "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY",
            "us-east-1",
            now,
        );
        assert_eq!(
            s.authorization,
            "AWS4-HMAC-SHA256 Credential=AKIAIOSFODNN7EXAMPLE/20130524/us-east-1/s3/aws4_request,SignedHeaders=host;range;x-amz-content-sha256;x-amz-date,Signature=f0e8bdb87c964420e857bd35b5d6ed310bd44f0170aba48dd91039c6036bdb41"
        );
    }

    #[test]
    fn query_and_xml() {
        assert_eq!(canonical_query(&[("prefix".into(), "My Vault/".into()), ("list-type".into(), "2".into())]), "list-type=2&prefix=My%20Vault%2F");
        assert_eq!(decode("&quot;abc&quot; &amp; &#x41;&#66;"), "\"abc\" & AB");
        assert_eq!(xml_tags("<a>1</a><a>2</a>", "a"), vec!["1", "2"]);
    }

    #[test]
    fn errors_read_well() {
        let e = S3Error { status: 301, code: "PermanentRedirect".into(), message: String::new(), region: Some("eu-west-1".into()) };
        assert!(e.to_string().contains("eu-west-1"));
        assert!(S3Error { status: 403, code: "InvalidAccessKeyId".into(), message: String::new(), region: None }.is_auth());
    }

    fn location() -> Location {
        Location { bucket: "b".into(), region: "us-east-1".into(), prefix: "/My Folder/".into(), endpoint: None }
    }

    #[test]
    fn objects_writes_listing_and_the_change_marker() {
        let s3 = FakeS3::new();
        let loc = location();
        let creds = Credentials { access_key_id: "AK".into(), secret_access_key: "SK".into(), session_token: None };
        let c = Client::new(&loc, &creds, &s3);
        assert_eq!(block_on(c.get("a")).unwrap(), None);
        let Put::Stored { etag } = block_on(c.put("items/a", b"1".to_vec(), Cond::Absent)).unwrap() else { panic!() };
        assert_eq!(block_on(c.put("items/a", b"2".to_vec(), Cond::Absent)).unwrap(), Put::Conflict);
        assert_eq!(block_on(c.put("items/a", b"2".to_vec(), Cond::Match("\"other\""))).unwrap(), Put::Conflict);
        assert!(matches!(block_on(c.put("items/a", b"2".to_vec(), Cond::Match(&etag))).unwrap(), Put::Stored { .. }));
        assert_eq!(block_on(c.get("items/a")).unwrap().unwrap().data, b"2");
        assert!(s3.objects.borrow().contains_key("My Folder/items/a"));
        for i in 0..5 {
            block_on(c.put(&format!("items/{i}"), vec![], Cond::Absent)).unwrap();
        }
        let names: Vec<String> = block_on(c.list("items/")).unwrap().into_iter().map(|l| l.name).collect();
        assert_eq!(names.len(), 6, "every page");

        assert_eq!(block_on(c.head("head")).unwrap(), None);
        let t1 = block_on(c.touch("head")).unwrap();
        assert_eq!(block_on(c.head("head")).unwrap(), Some(t1.clone()));
        let t2 = block_on(c.touch("head")).unwrap();
        assert_ne!(t1, t2);
        block_on(c.delete("head")).unwrap();
        assert_eq!(block_on(c.head("head")).unwrap(), None);

        *s3.fail.borrow_mut() = true;
        assert!(matches!(block_on(c.get("items/a")), Err(Error::Network(_))));
    }
}
