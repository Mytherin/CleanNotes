//! A minimal HTTP/1.1 server on 127.0.0.1 for the backends' fake servers.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::Arc;

pub struct Request {
    pub method: String,
    /// The path without the query.
    pub path: String,
    pub query: HashMap<String, String>,
    /// Header names lowercased.
    pub headers: HashMap<String, String>,
    pub body: Vec<u8>,
}

pub struct Response {
    pub status: u16,
    pub body: Vec<u8>,
    pub content_type: &'static str,
    pub headers: Vec<(&'static str, String)>,
}

impl Response {
    pub fn json(status: u16, v: &serde_json::Value) -> Response {
        let body = if v.is_null() { Vec::new() } else { v.to_string().into_bytes() };
        Response { status, body, content_type: "application/json", headers: vec![] }
    }
}

pub fn url_decode(s: &str) -> String {
    let s = s.replace('+', " ");
    let b = s.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 3 <= b.len() {
            if let Ok(v) = u8::from_str_radix(&s[i + 1..i + 3], 16) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// `a=1&b=2` as a map.
pub fn parse_form(s: &str) -> HashMap<String, String> {
    s.split('&')
        .filter_map(|kv| kv.split_once('=').map(|(k, v)| (url_decode(k), url_decode(v))))
        .collect()
}

fn read_request(r: &mut BufReader<TcpStream>) -> Option<Request> {
    let mut line = String::new();
    if r.read_line(&mut line).ok()? == 0 {
        return None;
    }
    let mut parts = line.split_whitespace();
    let method = parts.next()?.to_owned();
    let target = parts.next()?.to_owned();
    let (path, query) = match target.split_once('?') {
        Some((p, q)) => (p.to_owned(), parse_form(q)),
        None => (target, HashMap::new()),
    };
    let mut headers = HashMap::new();
    loop {
        let mut h = String::new();
        r.read_line(&mut h).ok()?;
        let h = h.trim_end();
        if h.is_empty() {
            break;
        }
        if let Some((k, v)) = h.split_once(':') {
            headers.insert(k.trim().to_lowercase(), v.trim().to_owned());
        }
    }
    let len: usize = headers.get("content-length").and_then(|l| l.parse().ok()).unwrap_or(0);
    let mut body = vec![0; len];
    r.read_exact(&mut body).ok()?;
    Some(Request { method, path, query, headers, body })
}

/// Serves requests with `handler` on a free port. Returns the base URL.
pub fn spawn(handler: impl Fn(&Request) -> Response + Send + Sync + 'static) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let handler = Arc::new(handler);
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let handler = handler.clone();
            std::thread::spawn(move || {
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut out = stream;
                while let Some(req) = read_request(&mut reader) {
                    let resp = handler(&req);
                    let extra: String = resp.headers.iter().map(|(k, v)| format!("{k}: {v}\r\n")).collect();
                    let head = format!(
                        "HTTP/1.1 {} X\r\nContent-Type: {}\r\nContent-Length: {}\r\n{extra}\r\n",
                        resp.status,
                        resp.content_type,
                        resp.body.len(),
                    );
                    if out.write_all(head.as_bytes()).and_then(|_| out.write_all(&resp.body)).is_err() {
                        return;
                    }
                }
            });
        }
    });
    url
}
