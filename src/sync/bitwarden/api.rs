//! Talking to a Bitwarden server: its identity and API addresses, the saved
//! session (tokens and the unlocked user key), refreshing access tokens,
//! and reading its JSON.

use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use base64::engine::general_purpose::{STANDARD as B64, URL_SAFE_NO_PAD};
use base64::Engine;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use super::crypto::{self, Keys};
use crate::sync::NeedsSignIn;

pub const CLIENT_NAME: &str = "cli";
pub const CLIENT_VERSION: &str = "2025.1.0";
pub const DEVICE_NAME: &str = "CleanNotes";

/// The identity and API base URLs of a server.
#[derive(Clone, Debug, PartialEq)]
pub struct Urls {
    pub identity: String,
    pub api: String,
}

impl Urls {
    pub fn of(server: &str) -> Urls {
        let s = server.trim().trim_end_matches('/');
        let cloud = |region: &str| Urls {
            identity: format!("https://identity.bitwarden.{region}"),
            api: format!("https://api.bitwarden.{region}"),
        };
        match s {
            "https://vault.bitwarden.com" | "https://bitwarden.com" => cloud("com"),
            "https://vault.bitwarden.eu" | "https://bitwarden.eu" => cloud("eu"),
            // Self-hosted (Bitwarden or Vaultwarden) serves both under one address.
            _ => Urls { identity: format!("{s}/identity"), api: format!("{s}/api") },
        }
    }
}

/// The CLI device type for this platform (Bitwarden's DeviceType enum).
pub fn device_type() -> u8 {
    if cfg!(target_os = "windows") {
        23
    } else if cfg!(target_os = "macos") {
        24
    } else {
        25
    }
}

/// A stable device id for this computer and account, so Bitwarden
/// recognizes it again (new-device checks, "remember me").
pub fn device_id(server: &str, email: &str) -> String {
    use sha2::{Digest, Sha256};
    let host = std::env::var("COMPUTERNAME").or_else(|_| std::env::var("HOSTNAME")).unwrap_or_default();
    let h = Sha256::digest(format!("cleannotes|{host}|{server}|{}", crypto::normalize_email(email)));
    let mut b: [u8; 16] = h[..16].try_into().unwrap();
    b[6] = (b[6] & 0x0f) | 0x40;
    b[8] = (b[8] & 0x3f) | 0x80;
    uuid::Uuid::from_bytes(b).to_string()
}

/// What's kept in the credential store for a signed-in account.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Session {
    pub access_token: String,
    /// Unix seconds.
    pub expires_at: i64,
    pub refresh_token: Option<String>,
    /// For API-key sign-ins, which get no refresh token: signing in again
    /// with the key gets a new access token.
    pub api_key: Option<(String, String)>,
    /// The unlocked user key (base64 of 64 bytes).
    pub user_key: String,
    pub device_id: String,
}

impl Session {
    pub fn keys(&self) -> Result<Keys> {
        Keys::from_bytes(&B64.decode(&self.user_key)?)
    }
}

/// An HTTP error with the server's message.
#[derive(Debug)]
pub struct HttpError {
    pub status: u16,
    pub message: String,
    pub body: Value,
}

impl std::fmt::Display for HttpError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "the server said {}: {}", self.status, self.message)
    }
}

impl std::error::Error for HttpError {}

/// Reads a JSON field whatever its case (Bitwarden's cloud answers in
/// camelCase, older servers in PascalCase).
pub fn get<'a>(v: &'a Value, key: &str) -> Option<&'a Value> {
    let o = v.as_object()?;
    o.get(key).or_else(|| o.iter().find(|(k, _)| k.eq_ignore_ascii_case(key)).map(|(_, v)| v)).filter(|v| !v.is_null())
}

pub fn get_str<'a>(v: &'a Value, key: &str) -> Option<&'a str> {
    get(v, key).and_then(Value::as_str)
}

/// The human-readable message in an error body.
fn message_of(v: &Value) -> String {
    for path in [&["ErrorModel", "Message"][..], &["message"], &["error_description"], &["error"]] {
        let mut cur = Some(v);
        for k in path {
            cur = cur.and_then(|c| get(c, k));
        }
        if let Some(s) = cur.and_then(Value::as_str) {
            return s.to_owned();
        }
    }
    // Validation errors: {"validationErrors": {"field": ["message"]}}
    if let Some(errs) = get(v, "validationErrors").and_then(Value::as_object) {
        if let Some(m) = errs.values().filter_map(Value::as_array).flatten().find_map(Value::as_str) {
            return m.to_owned();
        }
    }
    v.to_string()
}

pub fn http() -> reqwest::blocking::Client {
    reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(60))
        .user_agent(format!("CleanNotes/{}", env!("CARGO_PKG_VERSION")))
        .build()
        .expect("HTTP client")
}

fn read(resp: reqwest::blocking::Response) -> Result<Value> {
    let status = resp.status().as_u16();
    let text = resp.text().unwrap_or_default();
    let body: Value = if text.trim().is_empty() { Value::Null } else { serde_json::from_str(&text).unwrap_or(Value::String(text)) };
    if (200..300).contains(&status) {
        Ok(body)
    } else {
        Err(HttpError { status, message: message_of(&body), body }.into())
    }
}

/// Posts a form to the identity server's token endpoint.
pub fn token_request(http: &reqwest::blocking::Client, urls: &Urls, form: &[(&str, String)], email: Option<&str>) -> Result<Value> {
    let mut req = http
        .post(format!("{}/connect/token", urls.identity))
        .header("Bitwarden-Client-Name", CLIENT_NAME)
        .header("Bitwarden-Client-Version", CLIENT_VERSION)
        .header("Device-Type", device_type().to_string())
        .form(form);
    if let Some(e) = email {
        req = req.header("Auth-Email", URL_SAFE_NO_PAD.encode(e.as_bytes()));
    }
    read(req.send().context("contacting the Bitwarden server")?)
}

/// The account's KDF settings.
pub fn prelogin(http: &reqwest::blocking::Client, urls: &Urls, email: &str) -> Result<crypto::Kdf> {
    let body = json!({ "email": crypto::normalize_email(email) });
    let v = read(http.post(format!("{}/accounts/prelogin", urls.identity)).json(&body).send().context("contacting the Bitwarden server")?)?;
    let num = |k: &str| get(&v, k).and_then(Value::as_u64).map(|n| n as u32);
    Ok(match num("kdf").unwrap_or(0) {
        0 => crypto::Kdf::Pbkdf2 { iterations: num("kdfIterations").unwrap_or(600_000) },
        1 => crypto::Kdf::Argon2id {
            iterations: num("kdfIterations").unwrap_or(3),
            memory: num("kdfMemory").unwrap_or(64),
            parallelism: num("kdfParallelism").unwrap_or(4),
        },
        k => bail!("unknown KDF type {k}"),
    })
}

pub fn now() -> i64 {
    chrono::Utc::now().timestamp()
}

/// An authorized client for the vault API.
pub struct Client {
    pub http: reqwest::blocking::Client,
    pub urls: Urls,
    pub session: Session,
    /// The session changed (new tokens) since last saved.
    pub renewed: bool,
}

impl Client {
    pub fn new(server: &str, session: Session) -> Client {
        Client { http: http(), urls: Urls::of(server), session, renewed: false }
    }

    /// A fresh access token.
    fn refresh(&mut self) -> Result<()> {
        let s = &self.session;
        let form: Vec<(&str, String)> = if let Some(rt) = &s.refresh_token {
            vec![("grant_type", "refresh_token".into()), ("client_id", CLIENT_NAME.into()), ("refresh_token", rt.clone())]
        } else if let Some((id, secret)) = &s.api_key {
            vec![
                ("grant_type", "client_credentials".into()),
                ("scope", "api".into()),
                ("client_id", id.clone()),
                ("client_secret", secret.clone()),
                ("deviceType", device_type().to_string()),
                ("deviceIdentifier", s.device_id.clone()),
                ("deviceName", DEVICE_NAME.into()),
            ]
        } else {
            return Err(NeedsSignIn("no way to renew the session".into()).into());
        };
        let v = match token_request(&self.http, &self.urls, &form, None) {
            Ok(v) => v,
            Err(e) => {
                if let Some(h) = e.downcast_ref::<HttpError>() {
                    if h.status == 400 || h.status == 401 {
                        return Err(NeedsSignIn(h.message.clone()).into());
                    }
                }
                return Err(e);
            }
        };
        self.session.access_token = get_str(&v, "access_token").context("no access token")?.to_owned();
        self.session.expires_at = now() + get(&v, "expires_in").and_then(Value::as_i64).unwrap_or(3600);
        if let Some(rt) = get_str(&v, "refresh_token") {
            self.session.refresh_token = Some(rt.to_owned());
        }
        self.renewed = true;
        Ok(())
    }

    /// Sends a request to the API, renewing the access token if needed.
    pub fn send(&mut self, method: reqwest::Method, path: &str, body: Option<&Value>) -> Result<Value> {
        if self.session.expires_at - now() < 60 {
            self.refresh()?;
        }
        for attempt in 0..2 {
            let mut req = self
                .http
                .request(method.clone(), format!("{}{path}", self.urls.api))
                .bearer_auth(&self.session.access_token)
                .header("Bitwarden-Client-Name", CLIENT_NAME)
                .header("Bitwarden-Client-Version", CLIENT_VERSION)
                .header("Device-Type", device_type().to_string());
            if let Some(b) = body {
                req = req.json(b);
            }
            let resp = req.send().context("contacting the Bitwarden server")?;
            if resp.status().as_u16() == 401 && attempt == 0 {
                self.refresh()?;
                continue;
            }
            return read(resp);
        }
        Err(anyhow!("unauthorized"))
    }
}

/// The HTTP status of an error, if it came from the server.
pub fn status_of(e: &anyhow::Error) -> Option<u16> {
    e.downcast_ref::<HttpError>().map(|h| h.status)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn server_addresses() {
        assert_eq!(Urls::of("https://vault.bitwarden.com/").api, "https://api.bitwarden.com");
        assert_eq!(Urls::of("https://vault.bitwarden.eu").identity, "https://identity.bitwarden.eu");
        assert_eq!(Urls::of("https://vw.example.org").api, "https://vw.example.org/api");
    }

    #[test]
    fn device_ids_are_stable_uuids() {
        let a = device_id("s", "Me@x.org");
        assert_eq!(a, device_id("s", " me@x.org "));
        assert_ne!(a, device_id("s", "you@x.org"));
        assert_eq!(uuid::Uuid::parse_str(&a).unwrap().get_version_num(), 4);
    }

    #[test]
    fn fields_in_any_case() {
        let v = json!({"FolderId": "f", "name": null, "ErrorModel": {"Message": "nope"}});
        assert_eq!(get_str(&v, "folderId"), Some("f"));
        assert_eq!(get(&v, "name"), None);
        assert_eq!(message_of(&v), "nope");
    }
}
