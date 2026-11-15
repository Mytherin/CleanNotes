//! Standard Notes requests from a browser engine, as Standard Notes' own
//! apps make them.
//!
//! Every official Standard Notes client is its web app in a browser engine
//! (Electron on desktop, WKWebView on iPhone, Android's WebView), loaded from
//! a local `file://` page and calling the API with `fetch()`. The Cloudflare
//! front of `api.standardnotes.com` lets those through and turns away other
//! HTTP clients. So, for the official server, CleanNotes does the same: a
//! hidden system WebView (WebView2 on Windows, WKWebView on macOS) loads a
//! local page, and each request is a `fetch()` there, with the headers and
//! options of the official `FetchRequestHandler`.
//!
//! The WebView lives on the window's thread; syncs run on their own thread.
//! [`WebFetch`] (the transport the sync thread uses) hands each request to
//! [`WebHost`] (on the window's thread) and waits for the page's answer.
//!
//! If Cloudflare still asks for its browser check, [`WebHost`] can show the
//! WebView on Standard Notes' server for the user to complete it, as in a
//! browser; the clearance it gives is kept by the WebView for later
//! requests.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use eframe::egui;
use serde_json::{json, Value};
use wry::dpi::{LogicalPosition, LogicalSize};

use crate::sync::standardnotes::api::{self, Answer, Transport};

/// How long a request may take before the sync gives up on it.
const TIMEOUT: Duration = Duration::from_secs(90);

/// The local page requests are made from.
const PAGE: &str = r#"<!doctype html>
<meta charset="utf-8">
<title>CleanNotes</title>
<script>
// As FetchRequestHandler in standardnotes/app: a POST with the client's
// headers, cookies included.
window.__cleannotesFetch = async (id, url, headers, body) => {
  try {
    const r = await fetch(url, { method: 'POST', headers, body, credentials: 'include' });
    const text = await r.text();
    const h = {};
    r.headers.forEach((v, k) => { h[k] = v; });
    window.ipc.postMessage(JSON.stringify({ id, status: r.status, headers: h, body: text }));
  } catch (e) {
    window.ipc.postMessage(JSON.stringify({ id, error: String((e && e.message) || e) }));
  }
};
window.addEventListener('load', () => window.ipc.postMessage(JSON.stringify({ ready: true })));
</script>
"#;

/// The script making one request: the official clients' headers, and every
/// argument a JSON literal, so nothing in a URL, token or body can break out
/// of it.
fn fetch_script(id: u64, url: &str, bearer: Option<&str>, body: &str) -> String {
    let mut headers = json!({
        "X-SNJS-Version": api::SNJS_VERSION,
        "X-Application-Version": api::application_version(),
        "Content-Type": "application/json",
    });
    if let Some(t) = bearer {
        headers["Authorization"] = json!(format!("Bearer {t}"));
    }
    format!("window.__cleannotesFetch({id}, {}, {headers}, {});", json!(url), json!(body))
}

type Reply = std::result::Result<Answer, String>;
type Waiting = Arc<Mutex<HashMap<u64, Sender<Reply>>>>;

/// A request for the page: a script calling `__cleannotesFetch`.
struct Job {
    id: u64,
    script: String,
}

/// The transport the sync thread uses: requests go to the window's thread.
pub struct WebFetch {
    jobs: Mutex<Sender<Job>>,
    waiting: Waiting,
    next: AtomicU64,
    /// Set when Cloudflare asked for its browser check.
    check_needed: Arc<AtomicBool>,
    ctx: egui::Context,
}

impl Transport for WebFetch {
    fn post(&self, url: &str, bearer: Option<&str>, body: &str) -> Result<Answer> {
        let id = self.next.fetch_add(1, Ordering::Relaxed);
        let script = fetch_script(id, url, bearer, body);
        let (tx, rx) = mpsc::channel();
        self.waiting.lock().unwrap().insert(id, tx);
        self.jobs.lock().unwrap().send(Job { id, script }).map_err(|_| anyhow!("the app is closing"))?;
        self.ctx.request_repaint();
        let reply = rx.recv_timeout(TIMEOUT);
        self.waiting.lock().unwrap().remove(&id);
        match reply {
            Ok(Ok(answer)) => {
                if api::is_browser_check(&answer) {
                    self.check_needed.store(true, Ordering::Relaxed);
                    self.ctx.request_repaint();
                }
                Ok(answer)
            }
            Ok(Err(e)) => bail!("contacting the Standard Notes server: {e}"),
            Err(_) => bail!("the Standard Notes server didn't answer in time"),
        }
    }
}

/// The WebView, on the window's thread.
pub struct WebHost {
    webview: wry::WebView,
    page_url: String,
    jobs: Receiver<Job>,
    queued: Vec<Job>,
    waiting: Waiting,
    ready: Arc<AtomicBool>,
    check_needed: Arc<AtomicBool>,
    /// The WebView is showing Standard Notes' server for the browser check.
    checking: bool,
}

impl WebHost {
    /// Makes the WebView (hidden) in the app's window, and the transport for
    /// the sync thread.
    pub fn start(cc: &eframe::CreationContext<'_>) -> Result<(WebHost, Arc<WebFetch>)> {
        // A local file, as the official apps load theirs.
        let dir = crate::db::data_dir();
        std::fs::create_dir_all(&dir)?;
        let page = dir.join("standardnotes.html");
        std::fs::write(&page, PAGE).with_context(|| format!("writing {}", page.display()))?;
        let page_url = format!("file:///{}", page.display().to_string().replace('\\', "/").trim_start_matches('/'));

        let waiting: Waiting = Arc::new(Mutex::new(HashMap::new()));
        let ready = Arc::new(AtomicBool::new(false));
        let ctx = cc.egui_ctx.clone();
        let handler = {
            let (waiting, ready, ctx) = (waiting.clone(), ready.clone(), ctx.clone());
            move |req: wry::http::Request<String>| {
                let Ok(v) = serde_json::from_str::<Value>(req.body()) else { return };
                if v["ready"] == true {
                    ready.store(true, Ordering::Relaxed);
                    ctx.request_repaint();
                    return;
                }
                let Some(id) = v["id"].as_u64() else { return };
                let Some(tx) = waiting.lock().unwrap().remove(&id) else { return };
                let reply = match v["error"].as_str() {
                    Some(e) => Err(e.to_owned()),
                    None => Ok(Answer {
                        status: v["status"].as_u64().unwrap_or(0) as u16,
                        headers: v["headers"]
                            .as_object()
                            .map(|h| h.iter().map(|(k, v)| (k.to_lowercase(), v.as_str().unwrap_or("").to_owned())).collect())
                            .unwrap_or_default(),
                        body: Ok(v["body"].as_str().unwrap_or("").to_owned()),
                    }),
                };
                let _ = tx.send(reply);
            }
        };
        let webview = wry::WebViewBuilder::new()
            .with_url(&page_url)
            .with_visible(false)
            .with_bounds(wry::Rect { position: LogicalPosition::new(0.0, 0.0).into(), size: LogicalSize::new(1.0, 1.0).into() })
            .with_ipc_handler(handler)
            .build_as_child(cc)
            .map_err(|e| anyhow!("starting the browser engine: {e}"))?;

        let (tx, jobs) = mpsc::channel();
        let check_needed = Arc::new(AtomicBool::new(false));
        let fetch = Arc::new(WebFetch {
            jobs: Mutex::new(tx),
            waiting: waiting.clone(),
            next: AtomicU64::new(1),
            check_needed: check_needed.clone(),
            ctx,
        });
        let host = WebHost { webview, page_url, jobs, queued: Vec::new(), waiting, ready, check_needed, checking: false };
        Ok((host, fetch))
    }

    /// Hands queued requests to the page (each frame).
    pub fn pump(&mut self) {
        self.queued.extend(self.jobs.try_iter());
        if !self.ready.load(Ordering::Relaxed) || self.checking {
            return;
        }
        for job in self.queued.drain(..) {
            if let Err(e) = self.webview.evaluate_script(&job.script) {
                if let Some(tx) = self.waiting.lock().unwrap().remove(&job.id) {
                    let _ = tx.send(Err(e.to_string()));
                }
            }
        }
    }

    /// Cloudflare asked for its browser check (and it isn't showing yet).
    pub fn check_needed(&self) -> bool {
        self.check_needed.load(Ordering::Relaxed) && !self.checking
    }

    /// Shows Standard Notes' server in the WebView, over `area` (in points),
    /// for the user to complete Cloudflare's check.
    pub fn show_check(&mut self, area: egui::Rect) {
        if !self.checking {
            self.checking = true;
            self.ready.store(false, Ordering::Relaxed);
            let _ = self.webview.load_url(api::base(crate::sync::standardnotes::DEFAULT_SERVER).as_str());
        }
        let _ = self.webview.set_bounds(wry::Rect {
            position: LogicalPosition::new(area.min.x as f64, area.min.y as f64).into(),
            size: LogicalSize::new(area.width() as f64, area.height() as f64).into(),
        });
        let _ = self.webview.set_visible(true);
    }

    /// Hides the WebView again after the check, back on the local page.
    pub fn end_check(&mut self) {
        self.checking = false;
        self.check_needed.store(false, Ordering::Relaxed);
        let _ = self.webview.set_visible(false);
        let _ = self.webview.load_url(&self.page_url);
    }

    pub fn checking(&self) -> bool {
        self.checking
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requests_carry_the_official_headers_and_cant_break_out_of_the_script() {
        let body = r#"{"text":"a \"quote\", a </script>, a 
 and \u2028"}"#;
        let s = fetch_script(7, "https://api.standardnotes.com/v1/items", Some("tok\"en"), body);
        assert!(s.starts_with("window.__cleannotesFetch(7, \"https://api.standardnotes.com/v1/items\", {"));
        // The arguments parse back as JSON values, exactly.
        let args = s.trim_start_matches("window.__cleannotesFetch(").trim_end_matches(");");
        let v: Value = serde_json::from_str(&format!("[{args}]")).unwrap();
        assert_eq!(v[3], body);
        assert_eq!(v[2]["Authorization"], "Bearer tok\"en");
        assert_eq!(v[2]["X-SNJS-Version"], api::SNJS_VERSION);
        assert!(v[2]["X-Application-Version"].as_str().unwrap().starts_with("Desktop-"));
        assert_eq!(v[2]["Content-Type"], "application/json");
    }

    #[test]
    fn the_browser_check_is_recognised_from_what_fetch_can_see() {
        // Cross-origin, fetch() can't read cf-mitigated: the page tells.
        let a = Answer { status: 403, headers: vec![("content-type".into(), "text/html; charset=UTF-8".into())], body: Ok("<title>Just a moment...</title>".into()) };
        assert!(api::is_browser_check(&a));
        let json403 = Answer { status: 403, headers: vec![("content-type".into(), "application/json".into())], body: Ok(r#"{"error":{"message":"Just a moment"}}"#.into()) };
        assert!(!api::is_browser_check(&json403));
    }
}
