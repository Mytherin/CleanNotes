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

/// The local page requests are made from. Answers are kept on the page until
/// collected: wry drops messages sent from `file://` pages (it can't parse
/// their address as an `http::Uri`), so the app asks for them instead.
const PAGE: &str = r#"<!doctype html>
<meta charset="utf-8">
<title>CleanNotes</title>
<script>
window.__cleannotesAnswers = {};
// As FetchRequestHandler in standardnotes/app: a POST with the client's
// headers, cookies included.
window.__cleannotesFetch = async (id, url, headers, body) => {
  let answer;
  try {
    const r = await fetch(url, { method: 'POST', headers, body, credentials: 'include' });
    const text = await r.text();
    const h = {};
    r.headers.forEach((v, k) => { h[k] = v; });
    answer = { status: r.status, headers: h, body: text };
  } catch (e) {
    answer = { error: String((e && e.message) || e) };
  }
  window.__cleannotesAnswers[id] = answer;
};
// The answers that came in since last asked, as JSON text.
window.__cleannotesCollect = () => {
  const a = window.__cleannotesAnswers;
  window.__cleannotesAnswers = {};
  return JSON.stringify(a);
};
</script>
"#;

/// How often the page is asked for answers while requests are out.
const COLLECT_EVERY: Duration = Duration::from_millis(40);

/// Hands answers collected from the page to the requests waiting for them.
/// `result` is what the collecting script returned, as JSON: a string
/// holding a JSON object of answers by request id.
fn deliver(result: &str, waiting: &Waiting) {
    let Ok(text) = serde_json::from_str::<String>(result) else { return };
    let Ok(Value::Object(answers)) = serde_json::from_str::<Value>(&text) else { return };
    for (id, v) in answers {
        let Ok(id) = id.parse::<u64>() else { continue };
        let Some(tx) = waiting.lock().unwrap().remove(&id) else { continue };
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
}

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

/// Notes how requests go, with timings, to `webfetch.log` in the data
/// folder (started afresh each run). Only paths, statuses and times: never
/// tokens or contents.
fn log(line: &str) {
    use std::io::Write;
    static START: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();
    let t = START.get_or_init(std::time::Instant::now).elapsed();
    let path = crate::db::data_dir().join("webfetch.log");
    let first = !LOGGING.swap(true, Ordering::Relaxed);
    let file = std::fs::OpenOptions::new().create(true).write(true).append(!first).truncate(first).open(path);
    if let Ok(mut f) = file {
        let _ = writeln!(f, "{:>8.3}s  {line}", t.as_secs_f64());
    }
}

static LOGGING: AtomicBool = AtomicBool::new(false);

/// The path of a URL, for the log.
fn path_of(url: &str) -> &str {
    url.find("://").and_then(|i| url[i + 3..].find('/').map(|j| &url[i + 3 + j..])).unwrap_or(url)
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
        let asked = std::time::Instant::now();
        log(&format!("request {id}: POST {}", path_of(url)));
        let reply = rx.recv_timeout(TIMEOUT);
        self.waiting.lock().unwrap().remove(&id);
        let took = asked.elapsed().as_secs_f64();
        match &reply {
            Ok(Ok(a)) => log(&format!("request {id}: HTTP {} after {took:.3}s{}", a.status, if api::is_browser_check(a) { " (Cloudflare browser check)" } else { "" })),
            Ok(Err(e)) => log(&format!("request {id}: failed after {took:.3}s: {e}")),
            Err(_) => log(&format!("request {id}: no answer after {took:.3}s")),
        }
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
    /// Kept for as long as the WebView.
    _context: wry::WebContext,
    page_url: String,
    jobs: Receiver<Job>,
    queued: Vec<Job>,
    waiting: Waiting,
    ready: Arc<AtomicBool>,
    check_needed: Arc<AtomicBool>,
    /// The WebView is showing Standard Notes' server for the browser check.
    checking: bool,
    /// Give the keyboard back to the app's window once the page has loaded
    /// (WebView2 takes it when it loads a page).
    give_back_focus: bool,
    /// When the page was last asked for answers.
    last_collect: std::time::Instant,
    ctx: egui::Context,
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
        // Ready once the engine says the local page loaded.
        let on_load = {
            let (ready, ctx) = (ready.clone(), ctx.clone());
            move |event: wry::PageLoadEvent, url: String| {
                let what = if matches!(event, wry::PageLoadEvent::Finished) { "finished" } else { "started" };
                log(&format!("page load {what}: {}", if url.starts_with("file:") { "the local page" } else { path_of(&url) }));
                if matches!(event, wry::PageLoadEvent::Finished) && url.starts_with("file:") && !ready.swap(true, Ordering::Relaxed) {
                    log("page ready (load finished)");
                    ctx.request_repaint();
                }
            }
        };
        log("starting the browser engine");
        let started = std::time::Instant::now();
        // The engine's own data (cookies, cache) in the app's data folder, not
        // next to the exe (which may be somewhere read-only, like Program Files).
        let mut context = wry::WebContext::new(Some(dir.join("WebView")));
        let webview = wry::WebViewBuilder::new_with_web_context(&mut context)
            .with_on_page_load_handler(on_load)
            .with_url(&page_url)
            .with_visible(false)
            // Hidden, it mustn't take the keyboard from the app's window.
            .with_focused(false)
            .with_bounds(wry::Rect { position: LogicalPosition::new(0.0, 0.0).into(), size: LogicalSize::new(1.0, 1.0).into() })
            .build_as_child(cc)
            .map_err(|e| {
                log(&format!("the browser engine didn't start: {e}"));
                anyhow!("starting the browser engine: {e}")
            })?;
        log(&format!("browser engine started in {:.3}s (the window waited for this)", started.elapsed().as_secs_f64()));

        let (tx, jobs) = mpsc::channel();
        let check_needed = Arc::new(AtomicBool::new(false));
        let fetch = Arc::new(WebFetch {
            jobs: Mutex::new(tx),
            waiting: waiting.clone(),
            next: AtomicU64::new(1),
            check_needed: check_needed.clone(),
            ctx: ctx.clone(),
        });
        let host = WebHost {
            webview,
            _context: context,
            page_url,
            jobs,
            queued: Vec::new(),
            waiting,
            ready,
            check_needed,
            checking: false,
            give_back_focus: true,
            last_collect: std::time::Instant::now(),
            ctx,
        };
        let _ = host.webview.focus_parent();
        Ok((host, fetch))
    }

    /// Hands queued requests to the page, and collects its answers (each
    /// frame; frames keep coming while requests are out).
    pub fn pump(&mut self) {
        self.send_queued();
        if self.waiting.lock().unwrap().is_empty() || self.checking {
            return;
        }
        if self.last_collect.elapsed() >= COLLECT_EVERY {
            self.last_collect = std::time::Instant::now();
            let waiting = self.waiting.clone();
            let ctx = self.ctx.clone();
            let collect = self.webview.evaluate_script_with_callback("window.__cleannotesCollect()", move |result| {
                deliver(&result, &waiting);
                ctx.request_repaint();
            });
            if let Err(e) = collect {
                log(&format!("couldn't ask the page for answers: {e}"));
            }
        }
        self.ctx.request_repaint_after(COLLECT_EVERY);
    }

    fn send_queued(&mut self) {
        self.queued.extend(self.jobs.try_iter());
        if !self.ready.load(Ordering::Relaxed) || self.checking {
            return;
        }
        if self.give_back_focus {
            self.give_back_focus = false;
            let _ = self.webview.focus_parent();
        }
        for job in self.queued.drain(..) {
            log(&format!("request {}: handed to the page", job.id));
            if let Err(e) = self.webview.evaluate_script(&job.script) {
                log(&format!("request {}: the page wouldn't run it: {e}", job.id));
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
        let _ = self.webview.focus_parent();
        let _ = self.webview.load_url(&self.page_url);
        self.give_back_focus = true;
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
    fn answers_collected_from_the_page_reach_their_requests() {
        let waiting: Waiting = Arc::new(Mutex::new(HashMap::new()));
        let (tx1, rx1) = mpsc::channel();
        let (tx2, rx2) = mpsc::channel();
        waiting.lock().unwrap().insert(1, tx1);
        waiting.lock().unwrap().insert(2, tx2);
        // What WebView2 hands the callback: the script's result (a string)
        // as JSON.
        let page = json!({
            "1": {"status": 200, "headers": {"Content-Type": "application/json"}, "body": "{\"ok\":true}"},
            "2": {"error": "Failed to fetch"},
            "9": {"status": 200, "headers": {}, "body": ""},
        });
        deliver(&json!(page.to_string()).to_string(), &waiting);
        let a = rx1.try_recv().unwrap().unwrap();
        assert_eq!((a.status, a.body.unwrap()), (200, "{\"ok\":true}".to_owned()));
        assert_eq!(a.headers, vec![("content-type".to_owned(), "application/json".to_owned())]);
        assert_eq!(rx2.try_recv().unwrap().err().unwrap(), "Failed to fetch");
        assert!(waiting.lock().unwrap().is_empty());
        // Nothing yet, or junk: nothing happens.
        deliver(&json!("{}").to_string(), &waiting);
        deliver("not json", &waiting);
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
