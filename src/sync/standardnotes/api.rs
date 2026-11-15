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

/// The SNJS release whose protocol this client follows (the
/// `X-SNJS-Version` the official clients send).
pub const SNJS_VERSION: &str = "2.212.11";

/// `X-Application-Version`: `<Environment>-<app version>`, in the format of
/// the official clients, with this app's own version.
pub fn application_version() -> String {
    format!("Desktop-{}", env!("CARGO_PKG_VERSION"))
}

/// A server's answer, as any transport gets it.
pub struct Answer {
    pub status: u16,
    /// Lowercase names.
    pub headers: Vec<(String, String)>,
    /// The body, or why it couldn't be read in full.
    pub body: std::result::Result<String, String>,
}

impl Answer {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers.iter().find(|(k, _)| k == name).map(|(_, v)| v.as_str())
    }
}

/// How requests reach the server. Every official Standard Notes client
/// (desktop, web, iPhone, Android) makes them from a browser engine with
/// `fetch()`; [`crate::webfetch`] does the same here for the official
/// server. Self-hosted servers and tests use [`Direct`].
pub trait Transport: Send + Sync {
    /// A JSON `POST` with the official clients' headers (and the bearer
    /// token, if any).
    fn post(&self, url: &str, bearer: Option<&str>, body: &str) -> Result<Answer>;
}

/// Plain HTTP from this process.
pub struct Direct(reqwest::blocking::Client);

impl Direct {
    pub fn new() -> Direct {
        // The headers every official client sends with every API request
        // (`FetchRequestHandler.createRequest` in standardnotes/app): the
        // SNJS version and `<Environment>-<app version>`.
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert("X-SNJS-Version", reqwest::header::HeaderValue::from_static(SNJS_VERSION));
        headers.insert("X-Application-Version", reqwest::header::HeaderValue::from_str(&application_version()).expect("ASCII"));
        Direct(
            reqwest::blocking::Client::builder()
                .timeout(Duration::from_secs(60))
                .user_agent(format!("CleanNotes/{}", env!("CARGO_PKG_VERSION")))
                .default_headers(headers)
                .build()
                .expect("HTTP client"),
        )
    }
}

impl Transport for Direct {
    fn post(&self, url: &str, bearer: Option<&str>, body: &str) -> Result<Answer> {
        let mut req = self.0.post(url).header("Content-Type", "application/json").body(body.to_owned());
        if let Some(t) = bearer {
            req = req.bearer_auth(t);
        }
        let resp = req.send().context("contacting the Standard Notes server")?;
        let status = resp.status().as_u16();
        let headers = resp.headers().iter().map(|(k, v)| (k.as_str().to_owned(), v.to_str().unwrap_or("").to_owned())).collect();
        let body = resp.text().map_err(|e| e.to_string());
        Ok(Answer { status, headers, body })
    }
}

/// The browser-engine transport for the official server, once the app's
/// window has made one.
static BROWSER: std::sync::OnceLock<std::sync::Arc<dyn Transport>> = std::sync::OnceLock::new();

/// Sends requests to the official server through `t` from now on.
pub fn use_browser_engine(t: std::sync::Arc<dyn Transport>) {
    let _ = BROWSER.set(t);
}

/// The transport for a server: the browser engine for Standard Notes' own
/// (whose Cloudflare front turns away anything that isn't a browser), plain
/// HTTP otherwise.
pub fn transport_for(server: &str) -> std::sync::Arc<dyn Transport> {
    match BROWSER.get() {
        Some(t) if base(server) == base(super::DEFAULT_SERVER) => t.clone(),
        _ => std::sync::Arc::new(Direct::new()),
    }
}

pub fn base(server: &str) -> String {
    server.trim().trim_end_matches('/').to_owned()
}

pub fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

/// Whether an answer is Cloudflare's browser check rather than the server's.
/// (A `fetch()` can't see the `cf-mitigated` header cross-origin, so the
/// page itself is looked at too.)
pub fn is_browser_check(a: &Answer) -> bool {
    a.header("cf-mitigated") == Some("challenge")
        || (a.status == 403
            && a.header("content-type").map_or(true, |c| c.starts_with("text/html"))
            && a.body.as_deref().is_ok_and(|b| b.contains("challenge-platform") || b.contains("Just a moment")))
}

fn read(a: Answer) -> Result<Value> {
    let status = a.status;
    // Cloudflare, in front of the server, wants a browser check (a page
    // running JavaScript) before letting the request through: the server
    // itself never saw it. Not a wrong password.
    if is_browser_check(&a) {
        return Err(BrowserCheck(status).into());
    }
    let ok = (200..300).contains(&status);
    let text = match a.body {
        Ok(t) => t,
        // A reply cut off half way mustn't pass for an empty one.
        Err(e) if ok => anyhow::bail!("reading the Standard Notes server's reply: {e}"),
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

/// Cloudflare's browser check turned a request away.
#[derive(Debug)]
pub struct BrowserCheck(pub u16);

impl std::fmt::Display for BrowserCheck {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "Standard Notes' server is behind a Cloudflare browser check that turned this request away before it reached Standard Notes (HTTP {}). This isn't about your email or password.",
            self.0
        )
    }
}

impl std::error::Error for BrowserCheck {}

/// An unauthenticated POST (sign-in).
pub fn post(t: &dyn Transport, server: &str, path: &str, mut body: Value) -> Result<Value> {
    body["api"] = json!(API_VERSION);
    read(t.post(&format!("{}{path}", base(server)), None, &body.to_string())?)
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
    pub transport: std::sync::Arc<dyn Transport>,
    pub server: String,
    pub session: Session,
    /// The session changed (new tokens) since last saved.
    pub renewed: bool,
    /// Told the new secret as soon as the session is renewed.
    pub on_renewed: Option<crate::sync::OnRenewed>,
}

impl Client {
    pub fn new(server: &str, session: Session) -> Client {
        Client { transport: transport_for(server), server: base(server), session, renewed: false, on_renewed: None }
    }

    /// New tokens for the session (`/v1/sessions/refresh`).
    fn refresh(&mut self) -> Result<()> {
        if self.session.refresh_expiration > 0 && self.session.refresh_expiration <= now_ms() {
            return Err(NeedsSignIn("the session expired".into()).into());
        }
        let body = json!({ "access_token": self.session.access_token, "refresh_token": self.session.refresh_token });
        let v = match post(self.transport.as_ref(), &self.server, "/v1/sessions/refresh", body) {
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
            let url = format!("{}{path}", self.server);
            let resp = self.transport.post(&url, Some(&self.session.access_token), &b.to_string())?;
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
