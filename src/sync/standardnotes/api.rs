//! Talking to a Standard Notes server (the official one or self-hosted):
//! sign-in requests, the saved session, refreshing it, and the items sync
//! endpoint.
//!
//! Requests are JSON `POST`s with `api` set to the API version. This client
//! uses `20200115`, the version Standard Notes' mobile apps use, which
//! returns the session tokens in the response body (the web app's
//! `20240226` keeps them in browser cookies).

use std::time::Duration;

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::sync::NeedsSignIn;

pub const API_VERSION: &str = "20200115";
/// Items per sync request, up and down, as the client does.
pub const PAGE_LIMIT: usize = 150;
/// The status the server answers with when the access token expired.
pub const EXPIRED_ACCESS_TOKEN: u16 = 498;

/// What's kept in the credential store for a signed-in account: the
/// session's tokens, and the master key (which decrypts the account's items
/// keys). The password and the server password are not kept.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Session {
    pub access_token: String,
    pub refresh_token: String,
    /// Unix milliseconds.
    pub access_expiration: i64,
    pub refresh_expiration: i64,
    #[serde(default)]
    pub user_uuid: Option<String>,
    /// Hex.
    pub master_key: String,
    /// The account's key params as JSON text (in the order the client
    /// writes them), for the authenticated data of new items keys.
    pub key_params: String,
}

/// An error answer: its status, message and tag (`mfa-required`, …).
#[derive(Debug)]
pub struct HttpError {
    pub status: u16,
    pub message: String,
    pub tag: Option<String>,
    pub body: Value,
}

impl std::fmt::Display for HttpError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "the server said {}: {}", self.status, self.message)
    }
}

impl std::error::Error for HttpError {}

/// A response's payload: newer endpoints wrap it in `data`.
pub fn payload(v: &Value) -> &Value {
    match v.get("data") {
        Some(d) if d.is_object() => d,
        _ => v,
    }
}

fn error_of(v: &Value) -> Option<&Value> {
    payload(v).get("error").or_else(|| v.get("error")).filter(|e| e.is_object())
}

pub fn http() -> reqwest::blocking::Client {
    reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(60))
        .user_agent(format!("CleanNotes/{}", env!("CARGO_PKG_VERSION")))
        .build()
        .expect("HTTP client")
}

pub fn base(server: &str) -> String {
    server.trim().trim_end_matches('/').to_owned()
}

pub fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

fn read(resp: reqwest::blocking::Response) -> Result<Value> {
    let status = resp.status().as_u16();
    // Cloudflare, in front of the server, wants a browser check (a page
    // running JavaScript) before letting the request through: the server
    // itself never saw it. Not a wrong password.
    if resp.headers().get("cf-mitigated").and_then(|h| h.to_str().ok()) == Some("challenge") {
        anyhow::bail!(
            "Standard Notes' server is behind a Cloudflare browser check that turned this request away before it reached Standard Notes (HTTP {status}). This isn't about your email or password. Try again later; if it keeps happening, Standard Notes is blocking apps other than its own from this network."
        );
    }
    let ok = (200..300).contains(&status);
    let text = match resp.text() {
        Ok(t) => t,
        // A reply cut off half way mustn't pass for an empty one.
        Err(e) if ok => return Err(anyhow::Error::new(e).context("reading the Standard Notes server's reply")),
        Err(_) => String::new(),
    };
    let body: Value = if text.trim().is_empty() {
        Value::Null
    } else {
        match serde_json::from_str(&text) {
            Ok(v) => v,
            // A captive portal or proxy answering with a web page.
            Err(_) if ok => anyhow::bail!("the Standard Notes server's reply isn't JSON (a proxy or captive portal in the way?)"),
            Err(_) => Value::String(text),
        }
    };
    if ok && error_of(&body).is_none() {
        return Ok(body);
    }
    let err = error_of(&body);
    let message = err
        .and_then(|e| e.get("message"))
        .and_then(Value::as_str)
        .map(str::to_owned)
        // Not the body of a web page (a proxy's error page) as the message.
        .unwrap_or_else(|| format!("the server answered HTTP {status}"));
    let tag = err.and_then(|e| e.get("tag")).and_then(Value::as_str).map(str::to_owned);
    Err(HttpError { status, message, tag, body }.into())
}

/// An unauthenticated POST (sign-in).
pub fn post(http: &reqwest::blocking::Client, server: &str, path: &str, mut body: Value) -> Result<Value> {
    body["api"] = json!(API_VERSION);
    read(http.post(format!("{}{path}", base(server))).json(&body).send().context("contacting the Standard Notes server")?)
}

pub fn status_of(e: &anyhow::Error) -> Option<u16> {
    e.downcast_ref::<HttpError>().map(|h| h.status)
}

pub fn tag_of(e: &anyhow::Error) -> Option<&str> {
    e.downcast_ref::<HttpError>().and_then(|h| h.tag.as_deref())
}

/// A session's tokens from a sign-in or refresh answer.
pub fn tokens_of(v: &Value) -> Result<(String, String, i64, i64)> {
    let s = payload(v).get("session").context("the server sent no session")?;
    let str_of = |k: &str| s.get(k).and_then(Value::as_str).map(str::to_owned).with_context(|| format!("no {k} in the session"));
    let num = |k: &str| s.get(k).and_then(Value::as_i64).unwrap_or(0);
    Ok((str_of("access_token")?, str_of("refresh_token")?, num("access_expiration"), num("refresh_expiration")))
}

/// An authorized client.
pub struct Client {
    pub http: reqwest::blocking::Client,
    pub server: String,
    pub session: Session,
    /// The session changed (new tokens) since last saved.
    pub renewed: bool,
    /// Told the new secret as soon as the session is renewed.
    pub on_renewed: Option<crate::sync::OnRenewed>,
}

impl Client {
    pub fn new(server: &str, session: Session) -> Client {
        Client { http: http(), server: base(server), session, renewed: false, on_renewed: None }
    }

    /// New tokens for the session (`/v1/sessions/refresh`).
    fn refresh(&mut self) -> Result<()> {
        if self.session.refresh_expiration > 0 && self.session.refresh_expiration <= now_ms() {
            return Err(NeedsSignIn("the session expired".into()).into());
        }
        let body = json!({ "access_token": self.session.access_token, "refresh_token": self.session.refresh_token });
        let v = match post(&self.http, &self.server, "/v1/sessions/refresh", body) {
            Ok(v) => v,
            Err(e) => {
                if let Some(h) = e.downcast_ref::<HttpError>() {
                    // A refused refresh means signing in again; being told
                    // to slow down (or a timeout) doesn't.
                    if (400..500).contains(&h.status) && !matches!(h.status, 408 | 425 | 429) {
                        return Err(NeedsSignIn(h.message.clone()).into());
                    }
                }
                return Err(e);
            }
        };
        let (access, refresh, access_exp, refresh_exp) = tokens_of(&v)?;
        self.session.access_token = access;
        self.session.refresh_token = refresh;
        self.session.access_expiration = access_exp;
        self.session.refresh_expiration = refresh_exp;
        self.renewed = true;
        // The old refresh token is dead now: have the new one saved at once.
        if let (Some(f), Ok(s)) = (self.on_renewed.as_mut(), serde_json::to_string(&self.session)) {
            f(s);
        }
        Ok(())
    }

    /// An authorized POST, refreshing the session when the access token
    /// expired. A session the server no longer accepts is [`NeedsSignIn`].
    pub fn post(&mut self, path: &str, body: &Value) -> Result<Value> {
        if self.session.access_expiration > 0 && self.session.access_expiration - now_ms() < 60_000 {
            self.refresh()?;
        }
        for attempt in 0..2 {
            let mut b = body.clone();
            b["api"] = json!(API_VERSION);
            let resp = self
                .http
                .post(format!("{}{path}", self.server))
                .bearer_auth(&self.session.access_token)
                .json(&b)
                .send()
                .context("contacting the Standard Notes server")?;
            match read(resp) {
                Err(e) if status_of(&e) == Some(EXPIRED_ACCESS_TOKEN) && attempt == 0 => self.refresh()?,
                Err(e) if status_of(&e) == Some(401) => {
                    let msg = e.downcast_ref::<HttpError>().map(|h| h.message.clone()).unwrap_or_default();
                    return Err(NeedsSignIn(msg).into());
                }
                r => return r,
            }
        }
        bail!("the session couldn't be renewed")
    }

    /// One items sync request (`/v1/items`): uploads `items`, and downloads
    /// what changed since `sync_token` (a page of it, continued with
    /// `cursor_token`).
    pub fn sync(&mut self, items: &[Value], sync_token: Option<&str>, cursor_token: Option<&str>) -> Result<Value> {
        let mut body = json!({ "items": items, "limit": PAGE_LIMIT });
        if let Some(t) = sync_token {
            body["sync_token"] = json!(t);
        }
        if let Some(t) = cursor_token {
            body["cursor_token"] = json!(t);
        }
        let v = self.post("/v1/items", &body)?;
        Ok(payload(&v).clone())
    }
}
