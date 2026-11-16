//! Importing Evernote export files (`.enex`).
//!
//! An ENEX file is XML: one `<note>` per note, each with a title, its text
//! as ENML (Evernote's XHTML, in a CDATA section), dates, tags, and its
//! attachments (`<resource>`s, base64-encoded, referred to from the text by
//! the MD5 hash of their data). Tools that export Apple Notes to ENEX write
//! the same format, with the note's folder as its one tag, and HTML that
//! isn't always well-formed XML (`<br>` without a closing tag), so the text
//! is read with a lenient tokenizer rather than an XML parser.
//!
//! ENML is converted to the app's [`Doc`] and saved as Markdown like any
//! note:
//!
//! | ENML | Notes |
//! |---|---|
//! | `<div>`, `<p>` | a paragraph (`<br>` starts a new one) |
//! | `<h1>`, `<h2>`, `<h3>`–`<h6>` | Title, Heading, Subheading |
//! | `<ul>`, `<ul class="Apple-dash-list">`, `<ol>` (nested) | bulleted, dashed and numbered lists |
//! | `<en-todo>` | a checklist item |
//! | `<blockquote>` | a block quote |
//! | a paragraph all in `<tt>` / `<pre>` / a monospaced font | Monospaced |
//! | `<table>` | a table |
//! | `<b>`, `<i>`, `<u>`, `<s>`, `<a href>` | bold, italic, underline, strikethrough, a link |
//! | `<en-media>` (an attachment) | a link to the attachment, saved as a file |
//!
//! Colours, fonts and sizes are dropped. The app can't show images, so
//! attachments are saved as files (see [`Importer::save`]) and linked from
//! the note where they were.

use std::sync::Arc;

use anyhow::{bail, Context, Result};
use base64::Engine;
use chrono::{DateTime, NaiveDateTime, Utc};

use crate::doc::{Block, Doc, Kind, Style, MAX_INDENT};

// ---- Tokenizer ------------------------------------------------------------

#[derive(Debug, PartialEq)]
enum Tok<'a> {
    Start { name: String, attrs: Vec<(String, String)>, empty: bool },
    End(String),
    /// Text with its entities decoded.
    Text(String),
    CData(&'a str),
}

/// Tags, text and CDATA of XML or HTML, read leniently: anything that
/// isn't a well-formed tag is text, names are lowercased, and comments,
/// `<?…?>` and `<!DOCTYPE …>` are skipped.
struct Lexer<'a> {
    s: &'a str,
    i: usize,
}

impl<'a> Lexer<'a> {
    fn new(s: &'a str) -> Self {
        Lexer { s, i: 0 }
    }

    fn skip_past(&mut self, end: &str) {
        self.i = self.s[self.i..].find(end).map_or(self.s.len(), |n| self.i + n + end.len());
    }

    fn text_until_tag(&mut self) -> Tok<'a> {
        let start = self.i;
        // Past this '<' (which isn't a tag), up to the next one.
        let from = if self.s[start..].starts_with('<') { start + 1 } else { start };
        self.i = self.s[from..].find('<').map_or(self.s.len(), |n| from + n);
        Tok::Text(decode_entities(&self.s[start..self.i]))
    }

    /// A start or end tag at `self.i` (a '<'), if it is one.
    fn tag(&mut self) -> Option<Tok<'a>> {
        let b = self.s.as_bytes();
        let mut j = self.i + 1;
        let end = b.get(j) == Some(&b'/');
        if end {
            j += 1;
        }
        if !b.get(j).is_some_and(u8::is_ascii_alphabetic) {
            return None;
        }
        let name_start = j;
        while j < b.len() && (b[j].is_ascii_alphanumeric() || matches!(b[j], b'-' | b'_' | b':' | b'.')) {
            j += 1;
        }
        let name = self.s[name_start..j].to_ascii_lowercase();
        let mut attrs = Vec::new();
        let mut empty = false;
        loop {
            while j < b.len() && b[j].is_ascii_whitespace() {
                j += 1;
            }
            match b.get(j)? {
                b'>' => {
                    j += 1;
                    break;
                }
                b'/' => {
                    empty = true;
                    j += 1;
                    continue;
                }
                _ => {}
            }
            let a = j;
            while j < b.len() && !b[j].is_ascii_whitespace() && !matches!(b[j], b'=' | b'>' | b'/') {
                j += 1;
            }
            if j == a {
                // A stray character ('=' or a quote): skip it.
                j += 1;
                continue;
            }
            let attr = self.s[a..j].to_ascii_lowercase();
            while j < b.len() && b[j].is_ascii_whitespace() {
                j += 1;
            }
            let mut value = String::new();
            if b.get(j) == Some(&b'=') {
                j += 1;
                while j < b.len() && b[j].is_ascii_whitespace() {
                    j += 1;
                }
                match b.get(j)? {
                    &q @ (b'"' | b'\'') => {
                        let close = self.s[j + 1..].find(q as char)? + j + 1;
                        value = decode_entities(&self.s[j + 1..close]);
                        j = close + 1;
                    }
                    _ => {
                        let v = j;
                        while j < b.len() && !b[j].is_ascii_whitespace() && b[j] != b'>' {
                            j += 1;
                        }
                        value = decode_entities(&self.s[v..j]);
                    }
                }
            }
            attrs.push((attr, value));
        }
        self.i = j;
        Some(if end { Tok::End(name) } else { Tok::Start { name, attrs, empty } })
    }
}

impl<'a> Iterator for Lexer<'a> {
    type Item = Tok<'a>;

    fn next(&mut self) -> Option<Tok<'a>> {
        loop {
            let rest = &self.s[self.i..];
            if rest.is_empty() {
                return None;
            }
            if !rest.starts_with('<') {
                return Some(self.text_until_tag());
            }
            if let Some(body) = rest.strip_prefix("<![CDATA[") {
                let n = body.find("]]>").unwrap_or(body.len());
                let start = self.i + "<![CDATA[".len();
                self.i = (start + n + 3).min(self.s.len());
                return Some(Tok::CData(&self.s[start..start + n]));
            }
            if rest.starts_with("<!--") {
                self.skip_past("-->");
                continue;
            }
            if rest.starts_with("<!") || rest.starts_with("<?") {
                self.skip_past(">");
                continue;
            }
            return Some(match self.tag() {
                Some(t) => t,
                None => self.text_until_tag(),
            });
        }
    }
}

/// Decodes `&amp;`, `&#39;`, `&#x27;`, … As in HTML, the common entities are
/// also read without their semicolon (`info-&gtnext` is "info->next");
/// anything else stays as written.
fn decode_entities(s: &str) -> String {
    if !s.contains('&') {
        return s.to_owned();
    }
    const NAMED: &[(&str, char)] = &[
        ("amp", '&'),
        ("lt", '<'),
        ("gt", '>'),
        ("quot", '"'),
        ("apos", '\''),
        ("nbsp", '\u{a0}'),
        ("ndash", '–'),
        ("mdash", '—'),
        ("hellip", '…'),
        ("lsquo", '‘'),
        ("rsquo", '’'),
        ("ldquo", '“'),
        ("rdquo", '”'),
        ("bull", '•'),
        ("copy", '©'),
        ("reg", '®'),
        ("euro", '€'),
    ];
    // Read without a semicolon, as browsers do.
    const LEGACY: &[&str] = &["amp", "lt", "gt", "quot", "nbsp"];
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(at) = rest.find('&') {
        out.push_str(&rest[..at]);
        rest = &rest[at..];
        let after = &rest[1..];
        let mut done = false;
        if let Some(num) = after.strip_prefix('#') {
            let (digits, radix) = match num.strip_prefix(['x', 'X']) {
                Some(h) => (h, 16),
                None => (num, 10),
            };
            let n = digits.find(|c: char| !c.is_digit(radix)).unwrap_or(digits.len());
            if n > 0 {
                if let Some(c) = u32::from_str_radix(&digits[..n], radix).ok().and_then(char::from_u32) {
                    out.push(c);
                    let used = 1 + (num.len() - digits.len()) + n;
                    let semi = after[used..].starts_with(';') as usize;
                    rest = &rest[1 + used + semi..];
                    done = true;
                }
            }
        } else {
            let n = after.find(|c: char| !c.is_ascii_alphanumeric()).unwrap_or(after.len());
            let name = &after[..n];
            if after[n..].starts_with(';') {
                if let Some((_, c)) = NAMED.iter().find(|(e, _)| *e == name) {
                    out.push(*c);
                    rest = &rest[1 + n + 1..];
                    done = true;
                }
            }
            if !done {
                if let Some(e) = LEGACY.iter().find(|e| name.starts_with(**e)) {
                    out.push(NAMED.iter().find(|(x, _)| x == e).unwrap().1);
                    rest = &rest[1 + e.len()..];
                    done = true;
                }
            }
        }
        if !done {
            out.push('&');
            rest = &rest[1..];
        }
    }
    out.push_str(rest);
    out
}

// ---- The export file ------------------------------------------------------

/// An attachment of a note.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Resource {
    pub data: Vec<u8>,
    pub mime: String,
    pub file_name: Option<String>,
    /// The MD5 hash of `data`, in hex: how the note's text refers to it.
    pub hash: String,
}

impl Resource {
    /// A name for the file: its own (without any folder), or one made from
    /// its type.
    pub fn name(&self) -> String {
        let own = self.file_name.as_deref().map(|f| f.rsplit(['/', '\\']).next().unwrap_or(f).trim()).filter(|f| !f.is_empty());
        match own {
            Some(f) => f.to_owned(),
            None => format!("Attachment.{}", self.extension()),
        }
    }

    /// The file's extension: from its name, else from its type.
    pub fn extension(&self) -> String {
        let from_name = self
            .file_name
            .as_deref()
            .and_then(|f| f.rsplit(['/', '\\']).next())
            .and_then(|f| f.rsplit_once('.'))
            .map(|(_, e)| e.to_ascii_lowercase())
            .filter(|e| !e.is_empty() && e.len() <= 8 && e.chars().all(|c| c.is_ascii_alphanumeric()));
        from_name.unwrap_or_else(|| {
            match self.mime.as_str() {
                "image/jpeg" => "jpg",
                "image/png" => "png",
                "image/gif" => "gif",
                "image/heic" => "heic",
                "image/webp" => "webp",
                "image/svg+xml" => "svg",
                "application/pdf" => "pdf",
                "audio/mpeg" => "mp3",
                "audio/x-m4a" | "audio/mp4" => "m4a",
                "audio/wav" | "audio/x-wav" => "wav",
                "video/mp4" => "mp4",
                "video/quicktime" => "mov",
                "text/plain" => "txt",
                _ => "bin",
            }
            .to_owned()
        })
    }
}

/// A note as the export file has it.
#[derive(Clone, Debug, Default)]
struct RawNote {
    title: String,
    content: String,
    created: Option<DateTime<Utc>>,
    updated: Option<DateTime<Utc>>,
    tags: Vec<String>,
    resources: Vec<Resource>,
}

fn parse_date(s: &str) -> Option<DateTime<Utc>> {
    NaiveDateTime::parse_from_str(s.trim(), "%Y%m%dT%H%M%SZ").ok().map(|d| d.and_utc())
}

/// The notes of an ENEX file.
fn read_notes(xml: &str) -> Result<Vec<RawNote>> {
    let mut notes = Vec::new();
    let mut note: Option<RawNote> = None;
    let mut resource: Option<Resource> = None;
    let mut seen_export = false;
    let mut buf = String::new();
    for t in Lexer::new(xml) {
        match t {
            Tok::Start { name, empty, .. } => {
                buf.clear();
                match name.as_str() {
                    "en-export" => seen_export = true,
                    "note" if !empty => note = Some(RawNote::default()),
                    "resource" if note.is_some() && !empty => resource = Some(Resource::default()),
                    _ => {}
                }
            }
            Tok::Text(s) => buf.push_str(&s),
            Tok::CData(s) => buf.push_str(s),
            Tok::End(name) => {
                let value = std::mem::take(&mut buf);
                let Some(n) = note.as_mut() else { continue };
                if let Some(r) = resource.as_mut() {
                    match name.as_str() {
                        "data" => {
                            let b64: String = value.chars().filter(|c| !c.is_whitespace()).collect();
                            r.data = base64::engine::general_purpose::STANDARD
                                .decode(b64.as_bytes())
                                .with_context(|| format!("an attachment of \"{}\" isn't valid base64", n.title))?;
                        }
                        "mime" => r.mime = value.trim().to_ascii_lowercase(),
                        "file-name" => r.file_name = Some(value.trim().to_owned()),
                        "resource" => {
                            let mut r = resource.take().unwrap();
                            use md5::Digest;
                            r.hash = md5::Md5::digest(&r.data).iter().map(|b| format!("{b:02x}")).collect();
                            n.resources.push(r);
                        }
                        _ => {}
                    }
                    continue;
                }
                match name.as_str() {
                    "title" => n.title = value.trim().to_owned(),
                    "content" => n.content = value,
                    "created" => n.created = parse_date(&value),
                    "updated" => n.updated = parse_date(&value),
                    "tag" => {
                        let tag = value.trim();
                        if !tag.is_empty() {
                            n.tags.push(tag.to_owned());
                        }
                    }
                    "note" => notes.push(note.take().unwrap()),
                    _ => {}
                }
            }
        }
    }
    if !seen_export {
        bail!("not an Evernote export (no <en-export>)");
    }
    Ok(notes)
}

// ---- ENML to a Doc --------------------------------------------------------

/// An open element of the note's text.
struct El {
    name: String,
    link: Option<Arc<str>>,
    /// A monospaced font (`<font face="Menlo">`, …).
    mono: bool,
    /// Apple Notes' dashed list.
    dash: bool,
}

#[derive(Default)]
struct Table {
    rows: Vec<Vec<Block>>,
    header_row: bool,
    /// The cell being read.
    cell: Option<Block>,
}

struct Converter<'a> {
    blocks: Vec<Block>,
    /// The paragraph being read (its kind is decided when it ends).
    cur: Block,
    stack: Vec<El>,
    /// The table being read; tables inside it are read as its cells' text.
    table: Option<Table>,
    table_depth: usize,
    /// The paragraph started with a to-do box (checked or not).
    todo: Option<bool>,
    /// The link for an attachment, by its hash.
    media: &'a mut dyn FnMut(&str) -> Option<(String, String)>,
}

fn is_block(name: &str) -> bool {
    matches!(
        name,
        "div" | "p" | "h1" | "h2" | "h3" | "h4" | "h5" | "h6" | "li" | "ul" | "ol" | "blockquote" | "pre" | "table" | "tr" | "td" | "th"
            | "hr" | "en-note" | "object" | "dl" | "dt" | "dd" | "center" | "section" | "article" | "header" | "footer"
    )
}

fn is_void(name: &str) -> bool {
    matches!(name, "br" | "hr" | "img" | "en-media" | "en-todo" | "en-crypt" | "meta" | "link" | "input" | "wbr" | "col")
}

fn attr<'v>(attrs: &'v [(String, String)], name: &str) -> Option<&'v str> {
    attrs.iter().find(|(a, _)| a == name).map(|(_, v)| v.as_str())
}

fn monospaced_face(face: &str) -> bool {
    let f = face.to_ascii_lowercase();
    ["mono", "courier", "menlo", "consolas", "monaco"].iter().any(|m| f.contains(m))
}

impl Converter<'_> {
    fn has(&self, names: &[&str]) -> bool {
        self.stack.iter().any(|e| names.contains(&e.name.as_str()))
    }

    fn style(&self) -> Style {
        Style {
            bold: self.has(&["b", "strong"]),
            italic: self.has(&["i", "em"]),
            underline: self.has(&["u", "ins"]),
            strike: self.has(&["s", "strike", "del"]),
            code: self.has(&["tt", "code", "kbd", "samp", "pre"]) || self.stack.iter().any(|e| e.mono),
            link: self.stack.iter().rev().find_map(|e| e.link.clone()),
        }
    }

    fn text(&mut self, s: &str) {
        // Line breaks in the source are layout, not text.
        if s.contains('\n') && s.trim().is_empty() {
            return;
        }
        let s: String = s.chars().filter(|&c| c != '\r').map(|c| if c == '\n' || c == '\u{a0}' { ' ' } else { c }).collect();
        if s.is_empty() || self.table_depth > 0 && self.table.as_ref().is_some_and(|t| t.cell.is_none()) {
            return;
        }
        let style = self.style();
        let n = s.chars().count();
        let b = self.target();
        b.text.push_str(&s);
        b.styles.extend(std::iter::repeat(style).take(n));
    }

    /// Where text goes: the table cell being read, or the paragraph.
    fn target(&mut self) -> &mut Block {
        match self.table.as_mut().and_then(|t| t.cell.as_mut()) {
            Some(c) => c,
            None => &mut self.cur,
        }
    }

    fn in_cell(&self) -> bool {
        self.table.as_ref().is_some_and(|t| t.cell.is_some())
    }

    /// Ends the paragraph; an empty one is kept if `empty` (it ended with a
    /// `<br>`: Apple Notes' empty line). In a table cell, a new line.
    fn end_paragraph(&mut self, empty: bool) {
        if self.in_cell() {
            let c = self.target();
            if empty || (!c.text.is_empty() && !c.text.ends_with('\n')) {
                c.text.push('\n');
                c.styles.push(Style::default());
            }
            return;
        }
        if self.cur.is_empty() && !empty && self.todo.is_none() {
            return;
        }
        let mut b = std::mem::take(&mut self.cur);
        b.kind = self.kind();
        b.indent = self.indent(b.kind);
        if let Some(checked) = self.todo.take() {
            b.kind = Kind::Checklist(checked);
            b.indent = self.list_depth().saturating_sub(1);
            // The space after the box.
            if b.text.starts_with(' ') {
                b.text.remove(0);
                b.styles.remove(0);
            }
        }
        let all_code = b.styles.iter().all(|s| s.code);
        if b.kind == Kind::Body && ((!b.is_empty() && all_code) || (b.is_empty() && self.style().code)) {
            b.kind = Kind::Monospaced;
        }
        if b.kind == Kind::Monospaced {
            // Monospaced paragraphs are plain text.
            for s in &mut b.styles {
                *s = Style::default();
            }
        }
        if matches!(b.kind, Kind::Title | Kind::Heading | Kind::Subheading) {
            if b.text.trim().is_empty() {
                b = Block::new(Kind::Body, "");
            } else {
                // Headings are bold already.
                for s in &mut b.styles {
                    s.bold = false;
                }
            }
        }
        self.blocks.push(b);
    }

    fn list_depth(&self) -> u8 {
        self.stack.iter().filter(|e| e.name == "ul" || e.name == "ol").count().min(MAX_INDENT as usize + 1) as u8
    }

    /// The paragraph style the open elements give.
    fn kind(&self) -> Kind {
        for e in self.stack.iter().rev() {
            match e.name.as_str() {
                "h1" => return Kind::Title,
                "h2" => return Kind::Heading,
                "h3" | "h4" | "h5" | "h6" => return Kind::Subheading,
                "li" | "ul" | "ol" => {
                    let list = self.stack.iter().rev().find(|e| e.name == "ul" || e.name == "ol");
                    return match list {
                        Some(l) if l.name == "ol" => Kind::Numbered,
                        Some(l) if l.dash => Kind::Dashed,
                        _ => Kind::Bulleted,
                    };
                }
                "blockquote" => return Kind::Quote,
                "pre" => return Kind::Monospaced,
                _ => {}
            }
        }
        Kind::Body
    }

    fn indent(&self, kind: Kind) -> u8 {
        if kind.is_list() {
            self.list_depth().saturating_sub(1)
        } else if kind == Kind::Quote {
            (self.stack.iter().filter(|e| e.name == "blockquote").count().saturating_sub(1)).min(MAX_INDENT as usize) as u8
        } else {
            0
        }
    }

    fn start(&mut self, name: String, attrs: Vec<(String, String)>, empty: bool) {
        if is_block(&name) {
            self.end_paragraph(false);
        }
        match name.as_str() {
            "br" => {
                // A line break: a new paragraph, and an empty line if the
                // paragraph had nothing yet.
                let had = !self.target().is_empty();
                if self.in_cell() {
                    let after_break = self.target().text.ends_with('\n');
                    self.end_paragraph(!had || after_break);
                } else {
                    self.end_paragraph(!had);
                }
                return;
            }
            "en-todo" => {
                if !self.in_cell() {
                    self.todo = Some(attr(&attrs, "checked").is_some_and(|v| v.eq_ignore_ascii_case("true")));
                } else {
                    let checked = attr(&attrs, "checked").is_some_and(|v| v.eq_ignore_ascii_case("true"));
                    self.text(if checked { "☑ " } else { "☐ " });
                }
                return;
            }
            "en-media" => {
                let hash = attr(&attrs, "hash").unwrap_or("").to_ascii_lowercase();
                if let Some((label, url)) = (self.media)(&hash) {
                    self.link_text(&label, &url);
                }
                return;
            }
            "img" => {
                if let Some(src) = attr(&attrs, "src").filter(|s| !s.starts_with("data:")) {
                    let label = attr(&attrs, "alt").filter(|a| !a.trim().is_empty()).unwrap_or(src).to_owned();
                    self.link_text(&label, src);
                }
                return;
            }
            "table" => {
                self.table_depth += 1;
                if self.table_depth == 1 {
                    self.table = Some(Table::default());
                }
            }
            "tr" if self.table_depth == 1 => {
                if let Some(t) = self.table.as_mut() {
                    t.rows.push(Vec::new());
                }
            }
            "td" | "th" if self.table_depth == 1 => {
                if let Some(t) = self.table.as_mut() {
                    if t.rows.is_empty() {
                        t.rows.push(Vec::new());
                    }
                    if name == "th" && t.rows.len() == 1 {
                        t.header_row = true;
                    }
                    t.cell = Some(Block::default());
                }
            }
            _ => {}
        }
        if empty || is_void(&name) {
            return;
        }
        let link = if name == "a" { attr(&attrs, "href").filter(|h| !h.trim().is_empty()).map(|h| Arc::from(h.trim())) } else { None };
        let mono = name == "font" && attr(&attrs, "face").is_some_and(monospaced_face)
            || attr(&attrs, "style").is_some_and(|s| s.contains("font-family") && monospaced_face(s));
        let dash = attr(&attrs, "class").is_some_and(|c| c.contains("Apple-dash-list"));
        self.stack.push(El { name, link, mono, dash });
    }

    fn link_text(&mut self, label: &str, url: &str) {
        let mut style = self.style();
        style.link = Some(Arc::from(url));
        style.code = false;
        let n = label.chars().count();
        let b = self.target();
        b.text.push_str(label);
        b.styles.extend(std::iter::repeat(style).take(n));
    }

    fn end(&mut self, name: &str) {
        let Some(at) = self.stack.iter().rposition(|e| e.name == name) else { return };
        if is_block(name) {
            self.end_paragraph(false);
        }
        self.stack.truncate(at);
        match name {
            "td" | "th" if self.table_depth == 1 => {
                if let Some(t) = self.table.as_mut() {
                    if let Some(mut c) = t.cell.take() {
                        trim_newlines(&mut c);
                        if let Some(row) = t.rows.last_mut() {
                            row.push(c);
                        }
                    }
                }
            }
            "table" => {
                self.table_depth = self.table_depth.saturating_sub(1);
                if self.table_depth == 0 {
                    if let Some(t) = self.table.take() {
                        self.emit_table(t);
                    }
                } else if self.in_cell() {
                    self.end_paragraph(false);
                }
            }
            _ => {}
        }
    }

    fn emit_table(&mut self, t: Table) {
        let rows: Vec<Vec<Block>> = t.rows.into_iter().filter(|r| !r.is_empty()).collect();
        let cols = rows.iter().map(Vec::len).max().unwrap_or(0);
        if cols == 0 {
            return;
        }
        let kind = Kind::Cell { cols: cols as u16, header_row: t.header_row && rows.len() > 1, header_col: false };
        for mut row in rows {
            row.resize_with(cols, Block::default);
            for mut c in row {
                c.kind = kind;
                c.indent = 0;
                self.blocks.push(c);
            }
        }
    }

    fn finish(mut self) -> Vec<Block> {
        if let Some(t) = self.table.take() {
            self.emit_table(t);
        }
        self.end_paragraph(false);
        self.blocks
    }
}

fn trim_newlines(b: &mut Block) {
    while b.text.ends_with('\n') {
        b.text.pop();
        b.styles.pop();
    }
    while b.text.starts_with('\n') {
        b.text.remove(0);
        b.styles.remove(0);
    }
}

/// A note's ENML as paragraphs. `media` gives the link (its text and URL)
/// for an attachment's hash.
fn enml_blocks(enml: &str, media: &mut dyn FnMut(&str) -> Option<(String, String)>) -> Vec<Block> {
    let mut c = Converter { blocks: Vec::new(), cur: Block::default(), stack: Vec::new(), table: None, table_depth: 0, todo: None, media };
    let mut in_note = !enml.contains("<en-note");
    for t in Lexer::new(enml) {
        match t {
            Tok::Start { name, .. } if name == "en-note" => in_note = true,
            Tok::End(name) if name == "en-note" => {
                in_note = false;
            }
            _ if !in_note => {}
            Tok::Start { name, attrs, empty } => c.start(name, attrs, empty),
            Tok::End(name) => c.end(&name),
            Tok::Text(s) => c.text(&s),
            Tok::CData(s) => c.text(s),
        }
    }
    c.finish()
}

// ---- Importing ------------------------------------------------------------

/// A note read from an export file, ready to be added.
#[derive(Clone, Debug, PartialEq)]
pub struct ImportedNote {
    /// Its folder's name.
    pub folder: String,
    /// Its text as Markdown.
    pub body: String,
    pub created: DateTime<Utc>,
    pub modified: DateTime<Utc>,
    pub attachments: usize,
}

/// Reads export files; `save` keeps an attachment somewhere and returns
/// the URL of the copy, which the note links to.
pub struct Importer<'a> {
    pub save: &'a mut dyn FnMut(&Resource) -> Result<String>,
}

impl Importer<'_> {
    /// The notes of an export file. `notebook` (the file's name) is the
    /// folder, unless every note has exactly one tag: then (as in exports of
    /// Apple Notes) that tag is its folder. Otherwise tags become #tags at
    /// the end of the note.
    pub fn read(&mut self, xml: &str, notebook: &str) -> Result<Vec<ImportedNote>> {
        let raw = read_notes(xml)?;
        let tags_are_folders = !raw.is_empty() && raw.iter().all(|n| n.tags.len() == 1);
        let now = Utc::now();
        let mut out = Vec::new();
        for n in raw {
            let folder = if tags_are_folders { n.tags[0].clone() } else { notebook.to_owned() };
            let folder = folder.trim().replace('/', "∕");
            let folder = if folder.is_empty() { crate::model::DEFAULT_FOLDER.to_owned() } else { folder };
            let mut saved: Vec<Option<(String, String)>> = vec![None; n.resources.len()];
            let mut used = vec![false; n.resources.len()];
            let mut error = None;
            let mut media = |hash: &str| -> Option<(String, String)> {
                let i = n.resources.iter().position(|r| r.hash == hash)?;
                used[i] = true;
                link_for(&n.resources[i], &mut saved[i], self.save, &mut error)
            };
            let mut blocks = enml_blocks(&n.content, &mut media);
            // Attachments the text doesn't show go at the end.
            for i in 0..n.resources.len() {
                if !used[i] {
                    if let Some((label, url)) = link_for(&n.resources[i], &mut saved[i], self.save, &mut error) {
                        blocks.push(Block::styled(Kind::Body, &[(&label, Style { link: Some(Arc::from(url.as_str())), ..Style::default() })]));
                    }
                }
            }
            if let Some(e) = error {
                return Err(e.context(format!("saving an attachment of \"{}\"", n.title)));
            }
            add_title(&mut blocks, &n.title);
            if !tags_are_folders && !n.tags.is_empty() {
                let tags: Vec<String> = n.tags.iter().map(|t| hashtag(t)).filter(|t| t.len() > 1).collect();
                if !tags.is_empty() {
                    blocks.push(Block::new(Kind::Body, &tags.join(" ")));
                }
            }
            while blocks.len() > 1 && blocks.last().is_some_and(|b| b.is_empty() && !b.kind.is_cell() && !b.kind.is_list()) {
                blocks.pop();
            }
            let body = crate::markdown::to_markdown(&Doc::new(blocks));
            let created = n.created.or(n.updated).unwrap_or(now);
            let modified = n.updated.unwrap_or(created).max(created);
            out.push(ImportedNote { folder, body, created, modified, attachments: n.resources.len() });
        }
        Ok(out)
    }
}

fn link_for(
    r: &Resource,
    saved: &mut Option<(String, String)>,
    save: &mut dyn FnMut(&Resource) -> Result<String>,
    error: &mut Option<anyhow::Error>,
) -> Option<(String, String)> {
    if saved.is_none() && error.is_none() {
        match save(r) {
            Ok(url) => *saved = Some((r.name(), url)),
            Err(e) => *error = Some(e),
        }
    }
    saved.clone()
}

/// The note's title as its first line, unless its text starts with it
/// already (exports of Apple Notes leave it out of the text; Evernote's
/// keep it, and a long title may be cut short with "…").
fn add_title(blocks: &mut Vec<Block>, title: &str) {
    let short = title.trim_end_matches('…').trim_end_matches("...").trim();
    let first = blocks.iter().position(|b| !b.text.trim().is_empty() || b.kind.is_cell());
    let squash = |s: &str| s.split_whitespace().collect::<Vec<_>>().join(" ");
    let has_title = first.is_some_and(|i| squash(&blocks[i].text).starts_with(&squash(short)));
    if short.is_empty() || has_title {
        // The note starts at its first line with text.
        blocks.drain(..first.unwrap_or(0));
        return;
    }
    blocks.insert(0, Block::new(Kind::Title, title.trim()));
}

/// An Evernote tag as a #tag ("Work Stuff" → "#Work-Stuff").
fn hashtag(tag: &str) -> String {
    let mut t = String::from("#");
    for c in tag.trim().chars() {
        if c.is_alphanumeric() || c == '_' || c == '/' {
            t.push(c);
        } else if !t.ends_with('-') && t.len() > 1 {
            t.push('-');
        }
    }
    t.trim_end_matches('-').to_owned()
}

/// A `file:` URL for a path, with the characters a Markdown link or a URL
/// can't hold escaped.
pub fn file_url(path: &std::path::Path) -> String {
    let s = path.to_string_lossy().replace('\\', "/");
    let mut out = String::from(if s.starts_with('/') { "file://" } else { "file:///" });
    for c in s.chars() {
        match c {
            ' ' | '(' | ')' | '%' | '#' | '?' | '<' | '>' | '[' | ']' | '"' | '`' | '^' | '{' | '}' | '|' => {
                let mut b = [0u8; 4];
                for byte in c.encode_utf8(&mut b).bytes() {
                    out.push_str(&format!("%{byte:02X}"));
                }
            }
            c => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn note(title: &str, enml: &str, tags: &[&str]) -> String {
        let tags: String = tags.iter().map(|t| format!("<tag>{t}</tag>")).collect();
        format!(
            "<note><title>{title}</title><content><![CDATA[<?xml version=\"1.0\" encoding=\"UTF-8\"?><!DOCTYPE en-note SYSTEM \"http://xml.evernote.com/pub/enml2.dtd\"><en-note>{enml}</en-note>]]></content><created>20190424T204749Z</created><updated>20190517T172450Z</updated>{tags}</note>"
        )
    }

    fn export(notes: &[String]) -> String {
        format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<!DOCTYPE en-export SYSTEM \"http://xml.evernote.com/pub/evernote-export3.dtd\">\n<en-export application=\"Test\">\n{}\n</en-export>\n",
            notes.join("\n")
        )
    }

    fn import(xml: &str) -> Vec<ImportedNote> {
        let mut save = |r: &Resource| Ok(format!("file:///att/{}.{}", r.hash, r.extension()));
        Importer { save: &mut save }.read(xml, "Notebook").unwrap()
    }

    fn md(title: &str, enml: &str) -> String {
        import(&export(&[note(title, enml, &["F"])])).remove(0).body
    }

    #[test]
    fn entities() {
        assert_eq!(decode_entities("a &amp; b &lt;c&gt; &#x27;&#39; &quot;"), "a & b <c> '' \"");
        assert_eq!(decode_entities("info-&gtnext; i&gt4999 &lt=l"), "info->next; i>4999 <=l");
        assert_eq!(decode_entities("AT&T &foo; & &#xZZ;"), "AT&T &foo; & &#xZZ;");
        assert_eq!(decode_entities("&nbsp;&hellip;&#128512;"), "\u{a0}…😀");
    }

    #[test]
    fn lexer_is_lenient() {
        let toks: Vec<Tok> = Lexer::new("<div class=\"x\" a=b><br>1 < 2<!-- c --><a href='u'>t</a></div>").collect();
        assert_eq!(
            toks,
            vec![
                Tok::Start { name: "div".into(), attrs: vec![("class".into(), "x".into()), ("a".into(), "b".into())], empty: false },
                Tok::Start { name: "br".into(), attrs: vec![], empty: false },
                Tok::Text("1 ".into()),
                Tok::Text("< 2".into()),
                Tok::Start { name: "a".into(), attrs: vec![("href".into(), "u".into())], empty: false },
                Tok::Text("t".into()),
                Tok::End("a".into()),
                Tok::End("div".into()),
            ]
        );
    }

    #[test]
    fn paragraphs_and_empty_lines() {
        let enml = "\n<div>74cm x 28cm</div>\n<div>84cm x 14cm</div>\n<div><br></div>\n<div>Kast:</div>\n<div>80cm x 60cm<br></div>\n";
        assert_eq!(md("Wc:", enml), "# Wc:\n74cm x 28cm\n84cm x 14cm\n\nKast:\n80cm x 60cm\n");
        // A <br> inside a paragraph splits it; trailing empty lines go.
        assert_eq!(md("T", "<div>T</div><div>a<br>b</div><div><br></div><div><b><br></b></div>"), "T\na\nb\n");
    }

    #[test]
    fn styles_headings_and_code() {
        let enml = "<div><h1>Big</h1></div><div><h2>Mid</h2></div><div><h3><b>Small</b></h3></div>\
                    <div><b>bold</b> <i>it</i> <u>under</u> <strike>gone</strike> <a href=\"https://x.org\">link</a></div>\
                    <div><tt>\tcode()</tt></div><div><font face=\"Menlo\">more</font></div><div>inline <tt>x</tt></div>";
        assert_eq!(
            md("T", enml),
            "# T\n# Big\n## Mid\n### Small\n**bold** *it* <u>under</u> ~~gone~~ [link](https://x.org)\n```\n\tcode()\nmore\n```\ninline `x`\n"
        );
    }

    #[test]
    fn lists_nest_and_dash() {
        let enml = "<ul class=\"Apple-dash-list\">\n<li>door</li>\n<li><br></li>\n</ul>\
                    <ul>\n<li>Japan</li>\n<ul>\n<li>Geisha</li>\n</ul>\n<li>Train<br></li>\n</ul>\
                    <ol><li>one</li><li>two</li></ol>\
                    <div><en-todo checked=\"true\"/>done</div><div><en-todo/>open</div>";
        assert_eq!(md("T", enml), "# T\n- door\n- \n* Japan\n    * Geisha\n* Train\n1. one\n2. two\n- [x] done\n- [ ] open\n");
    }

    #[test]
    fn tables() {
        let enml = "<div><object><table cellspacing=\"0\"><tbody>\n<tr><td valign=\"top\"><div>Amsterdam</div>\n</td><td><div>AVL</div><div>second line</div></td></tr>\n<tr><td><div>Delft</div></td></tr>\n</tbody></table></object></div><div>after</div>";
        let body = md("T", enml);
        let doc = crate::markdown::from_markdown(&body);
        let cells: Vec<(&str, Kind)> = doc.blocks.iter().map(|b| (b.text.as_str(), b.kind)).collect();
        let k = Kind::cell(2);
        assert_eq!(
            cells,
            vec![
                ("T", Kind::Title),
                ("Amsterdam", k),
                ("AVL\nsecond line", k),
                ("Delft", k),
                ("", k),
                ("after", Kind::Body),
            ]
        );
    }

    #[test]
    fn titles() {
        // In the text already (perhaps cut short): not added again.
        assert_eq!(md("In all cases, the…", "<ol><li>In all cases, the names</li></ol>"), "1. In all cases, the names\n");
        assert_eq!(md("Hi", "<div><br></div><div><h1>Hi</h1></div><div>x</div>"), "# Hi\nx\n");
        // Left out: the empty line after it stays, as it was under the title.
        assert_eq!(md("Pi", "<div><br></div><div>x</div>"), "# Pi\n\nx\n");
        // An empty note keeps its title.
        assert_eq!(md("Woonmall", "\n"), "# Woonmall\n");
        assert_eq!(md("", "<div>x</div>"), "x\n");
    }

    #[test]
    fn attachments_are_linked_where_they_were() {
        let data = b"fake image";
        use md5::Digest;
        let hash: String = md5::Md5::digest(data).iter().map(|b| format!("{b:02x}")).collect();
        let b64 = base64::engine::general_purpose::STANDARD.encode(data);
        let other = base64::engine::general_purpose::STANDARD.encode(b"a pdf");
        let n = format!(
            "<note><title>Pics</title><content><![CDATA[<en-note><div>see</div><div><en-media type=\"image/jpeg\" hash=\"{hash}\"></en-media></div></en-note>]]></content>\
             <tag>Cats</tag>\
             <resource><data encoding=\"base64\">\n{b64}\n</data><mime>image/jpeg</mime><resource-attributes><file-name>te/p1832-1.jpg</file-name></resource-attributes></resource>\
             <resource><data encoding=\"base64\">{other}</data><mime>application/pdf</mime></resource></note>"
        );
        let notes = import(&export(&[n]));
        assert_eq!(notes[0].attachments, 2);
        let pdf_hash: String = md5::Md5::digest(b"a pdf").iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(
            notes[0].body,
            format!("# Pics\nsee\n[p1832-1.jpg](file:///att/{hash}.jpg)\n[Attachment.pdf](file:///att/{pdf_hash}.pdf)\n")
        );
    }

    #[test]
    fn folders_from_tags_or_the_notebook() {
        let xml = export(&[note("A", "<div>a</div>", &["House &amp; DIY"]), note("B", "<div>b</div>", &["Money"])]);
        let notes = import(&xml);
        assert_eq!(notes.iter().map(|n| n.folder.as_str()).collect::<Vec<_>>(), ["House & DIY", "Money"]);
        assert_eq!(notes[0].created, Utc.with_ymd_and_hms(2019, 4, 24, 20, 47, 49).unwrap());
        assert_eq!(notes[0].modified, Utc.with_ymd_and_hms(2019, 5, 17, 17, 24, 50).unwrap());

        // Evernote: a notebook per file, tags as tags.
        let xml = export(&[note("A", "<div>a</div>", &["work stuff", "x"]), note("B", "<div>b</div>", &[])]);
        let notes = import(&xml);
        assert_eq!(notes[0].folder, "Notebook");
        assert_eq!(notes[0].body, "# A\na\n#work-stuff #x\n");
        assert_eq!(notes[1].body, "# B\nb\n");
    }

    #[test]
    fn not_an_export() {
        let mut save = |_: &Resource| Ok(String::new());
        assert!(Importer { save: &mut save }.read("<html></html>", "x").is_err());
    }

    #[test]
    fn file_urls() {
        assert_eq!(file_url(std::path::Path::new(r"C:\Users\A B\x (1).jpg")), "file:///C:/Users/A%20B/x%20%281%29.jpg");
        assert_eq!(file_url(std::path::Path::new("/home/a/x.jpg")), "file:///home/a/x.jpg");
    }

    use chrono::TimeZone;

    /// Converts real export files for a look at the result:
    /// `ENEX_IN=<file or folder> ENEX_OUT=<folder> cargo test enex::tests::real -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn real() {
        let input = std::path::PathBuf::from(std::env::var("ENEX_IN").unwrap());
        let out = std::path::PathBuf::from(std::env::var("ENEX_OUT").unwrap());
        let files: Vec<std::path::PathBuf> = if input.is_dir() {
            let mut v: Vec<_> =
                std::fs::read_dir(&input).unwrap().map(|e| e.unwrap().path()).filter(|p| p.extension().is_some_and(|e| e == "enex")).collect();
            v.sort();
            v
        } else {
            vec![input]
        };
        let t = std::time::Instant::now();
        let mut count = std::collections::BTreeMap::<String, usize>::new();
        for f in files {
            let xml = std::fs::read_to_string(&f).unwrap();
            let mut save = |r: &Resource| Ok(format!("file:///att/{}.{}", r.hash, r.extension()));
            let notes = Importer { save: &mut save }.read(&xml, f.file_stem().unwrap().to_str().unwrap()).unwrap();
            for (i, n) in notes.iter().enumerate() {
                *count.entry(n.folder.clone()).or_default() += 1;
                let dir = out.join(&n.folder);
                std::fs::create_dir_all(&dir).unwrap();
                std::fs::write(dir.join(format!("{i:04}.md")), &n.body).unwrap();
            }
        }
        println!("{:?} {count:#?}", t.elapsed());
    }
}
