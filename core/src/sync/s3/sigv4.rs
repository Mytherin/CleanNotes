//! AWS Signature Version 4 for S3 requests, done by hand on top of `hmac`
//! and `sha2` (the AWS SDK needs an async runtime).

use chrono::{DateTime, Utc};
use hmac::{Hmac, Mac};
use sha2::{Digest, Sha256};

pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

pub fn sha256_hex(data: &[u8]) -> String {
    hex(&Sha256::digest(data))
}

fn hmac(key: &[u8], data: &str) -> Vec<u8> {
    let mut m = Hmac::<Sha256>::new_from_slice(key).expect("HMAC takes any key length");
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

/// Signs a request. `path` is the already encoded path, `query` the
/// canonical query string, and `headers` (lowercase names) the headers to
/// sign, which must include `host` and `x-amz-content-sha256`
/// (`x-amz-date` is added).
#[allow(clippy::too_many_arguments)]
pub fn sign(
    method: &str,
    path: &str,
    query: &str,
    headers: &[(&str, String)],
    payload_hash: &str,
    access_key: &str,
    secret_key: &str,
    region: &str,
    now: DateTime<Utc>,
) -> Signed {
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
    Signed {
        authorization: format!("AWS4-HMAC-SHA256 Credential={access_key}/{scope},SignedHeaders={signed_headers},Signature={signature}"),
        amz_date,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The "GET Object" example from AWS's Signature Version 4 docs for S3.
    #[test]
    fn aws_documented_example() {
        let now = DateTime::parse_from_rfc3339("2013-05-24T00:00:00Z").unwrap().with_timezone(&Utc);
        let empty = sha256_hex(b"");
        assert_eq!(empty, "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855");
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
        assert_eq!(s.amz_date, "20130524T000000Z");
    }

    #[test]
    fn query_is_sorted_and_encoded() {
        let q = canonical_query(&[("prefix".into(), "My Notes/".into()), ("list-type".into(), "2".into())]);
        assert_eq!(q, "list-type=2&prefix=My%20Notes%2F");
        assert_eq!(uri_encode("a b/ü", true), "a%20b/%C3%BC");
    }
}
