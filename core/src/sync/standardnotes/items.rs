//! Standard Notes' item contents (decrypted) and how notes map onto them.
//!
//! A `Note` item's content is `{title, text, references, appData, noteType,
//! editorIdentifier, trashed, preview_plain, …}`; a `Tag`'s is `{title,
//! references, expanded, …}`, its references naming the notes in it and,
//! for a nested tag, its parent (`reference_type: "TagToParentTag"`).
//! Pins and the user's modification date live in
//! `appData["org.standardnotes.sn"]` (`pinned`, `client_updated_at`).
//!
//! Everything in a content that the app doesn't own is kept when it's
//! updated: the content is changed in place, never rebuilt.

use chrono::{DateTime, SecondsFormat, Utc};
use serde_json::{json, Map, Value};

use crate::markdown::{snippet_of, title_of};
use crate::sync::NoteData;

pub const APP_DOMAIN: &str = "org.standardnotes.sn";
/// The app's own appData domain: when a note went to the trash, which
/// Standard Notes doesn't record.
pub const OUR_DOMAIN: &str = "com.cleannotes";
pub const PLAIN_EDITOR: &str = "com.standardnotes.plain-text";
pub const PARENT_TAG: &str = "TagToParentTag";

/// The line put under the title of a note shown read-only.
pub const READ_ONLY_BANNER: &str = "> Shown read-only:";

pub fn iso(t: DateTime<Utc>) -> String {
    t.to_rfc3339_opts(SecondsFormat::Millis, true)
}

/// A date as Standard Notes writes it: an ISO string (a `Date` through
/// `JSON.stringify`), `Date.toString()` text, or milliseconds.
pub fn parse_date(v: &Value) -> Option<DateTime<Utc>> {
    match v {
        Value::Number(n) => n.as_i64().and_then(DateTime::from_timestamp_millis),
        Value::String(s) => {
            if let Ok(t) = DateTime::parse_from_rfc3339(s) {
                return Some(t.with_timezone(&Utc));
            }
            // "Sun Nov 15 2026 12:03:00 GMT+0100 (Central European Standard Time)"
            let head = s.split(" (").next().unwrap_or(s);
            DateTime::parse_from_str(head, "%a %b %d %Y %H:%M:%S GMT%z").ok().map(|t| t.with_timezone(&Utc))
        }
        _ => None,
    }
}

fn app_data<'a>(content: &'a Value, domain: &str) -> Option<&'a Value> {
    content.get("appData")?.get(domain)
}

fn app_data_mut<'a>(content: &'a mut Value, domain: &str) -> &'a mut Map<String, Value> {
    if !content["appData"].is_object() {
        content["appData"] = json!({});
    }
    let ad = content["appData"].as_object_mut().unwrap();
    let d = ad.entry(domain.to_owned()).or_insert_with(|| json!({}));
    if !d.is_object() {
        *d = json!({});
    }
    d.as_object_mut().unwrap()
}

pub fn str_field<'a>(content: &'a Value, k: &str) -> &'a str {
    content.get(k).and_then(Value::as_str).unwrap_or("")
}

/// What kind of note a content is, as far as the app is concerned.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// Plain text or Markdown: the text is the note.
    Text,
    /// A Super note the app can show exactly: edited here as Markdown and
    /// saved back as a Super note (see [`super::lexical`]).
    Super,
    /// Written with another editor (Super, rich text, code, a spreadsheet,
    /// …) or locked: shown read-only, never overwritten.
    ReadOnly,
}

pub fn kind(content: &Value) -> Kind {
    if app_data(content, APP_DOMAIN).and_then(|d| d.get("locked")).and_then(Value::as_bool) == Some(true) {
        return Kind::ReadOnly;
    }
    let editor = content.get("editorIdentifier").and_then(Value::as_str);
    match content.get("noteType").and_then(Value::as_str) {
        Some("plain-text") | Some("markdown") => Kind::Text,
        Some("super") if super_markdown(str_field(content, "text")).is_ok() => Kind::Super,
        None | Some("") => match editor {
            None | Some("") | Some(PLAIN_EDITOR) => Kind::Text,
            Some(_) => Kind::ReadOnly,
        },
        Some(_) => Kind::ReadOnly,
    }
}

fn kind_name(content: &Value) -> String {
    match content.get("noteType").and_then(Value::as_str) {
        Some("super") => "a Super note".into(),
        Some("rich-text") => "a rich text note".into(),
        Some("code") => "a code note".into(),
        Some("task") => "a task list".into(),
        Some("spreadsheet") => "a spreadsheet".into(),
        Some("authentication") => "an authenticator note".into(),
        _ if app_data(content, APP_DOMAIN).and_then(|d| d.get("locked")).and_then(Value::as_bool) == Some(true) => {
            "a locked note".into()
        }
        _ => "a note for another editor".into(),
    }
}

/// A note's content as the app sees it.
#[derive(Clone, Debug, PartialEq)]
pub struct ReadNote {
    pub body: String,
    pub kind: Kind,
    /// The title isn't the text's first line, so it was put on top of the
    /// body as a heading (and is taken off again when saving).
    pub title_prepended: bool,
    pub pinned: bool,
    pub trashed: bool,
    pub trashed_at: Option<DateTime<Utc>>,
    pub modified: Option<DateTime<Utc>>,
}

pub fn read_note(content: &Value) -> ReadNote {
    let title = str_field(content, "title").trim().to_owned();
    let text = str_field(content, "text");
    let kind = kind(content);
    let with_title = |text: &str| {
        if !title.is_empty() && title_of(text) != title {
            (format!("# {title}\n{text}"), true)
        } else {
            (text.to_owned(), false)
        }
    };
    let (body, title_prepended) = match kind {
        Kind::Text => with_title(text),
        Kind::Super => with_title(&super_markdown(text).unwrap_or_default()),
        Kind::ReadOnly => (read_only_body(content, &title), false),
    };
    let sn = app_data(content, APP_DOMAIN);
    ReadNote {
        body,
        kind,
        title_prepended,
        pinned: sn.and_then(|d| d.get("pinned")).and_then(Value::as_bool).unwrap_or(false),
        trashed: content.get("trashed").and_then(Value::as_bool).unwrap_or(false),
        trashed_at: app_data(content, OUR_DOMAIN).and_then(|d| d.get("trashed_at")).and_then(parse_date),
        modified: sn.and_then(|d| d.get("client_updated_at")).and_then(parse_date),
    }
}

/// The body shown for a note the app can't edit: its title, a line saying
/// so, and its text as plain as it can be made.
fn read_only_body(content: &Value, title: &str) -> String {
    let text = str_field(content, "text");
    let shown = match content.get("noteType").and_then(Value::as_str) {
        Some("super") => lexical_text(text).unwrap_or_else(|| str_field(content, "preview_plain").to_owned()),
        Some("rich-text") => html_text(text),
        _ if kind_is_locked_text(content) => text.to_owned(),
        _ => format!("```\n{}\n```", text.trim_end_matches('\n')),
    };
    let title = if title.is_empty() { "Untitled" } else { title };
    // For a Super note: what in it the app can't show yet.
    let why = match content.get("noteType").and_then(Value::as_str) {
        Some("super") => match super_markdown(text) {
            Err(reason) => format!(" with {reason}, which CleanNotes can't show yet"),
            Ok(_) => String::new(),
        },
        _ => String::new(),
    };
    format!(
        "# {title}\n{READ_ONLY_BANNER} this is {}{why} in Standard Notes. Edit it there; changes to its text made here are saved as a new note.\n\n{shown}",
        kind_name(content)
    )
}

/// A Super note's text as the app's Markdown, if it converts exactly: to the
/// app's document and back to Super gives the same document, and so does
/// going through Markdown (what the editor reads and writes). `Err` says
/// what the app can't show.
pub fn super_markdown(text: &str) -> Result<String, String> {
    use super::lexical;
    use crate::markdown::{from_markdown, to_markdown};
    // Empty paragraphs at the end aren't kept by the Markdown: not a loss.
    let trimmed = |d: &crate::doc::Doc| {
        let mut b = d.blocks.clone();
        while b.last().is_some_and(|l| l.kind == crate::doc::Kind::Body && l.text.is_empty()) {
            b.pop();
        }
        b
    };
    let doc = lexical::to_doc(text)?;
    let md = to_markdown(&doc);
    let exact = lexical::to_doc(&lexical::from_doc(&doc)).is_ok_and(|d| trimmed(&d) == trimmed(&doc))
        && trimmed(&from_markdown(&md)) == trimmed(&doc);
    if !exact {
        return Err("formatting that doesn't convert exactly".into());
    }
    Ok(md)
}

fn kind_is_locked_text(content: &Value) -> bool {
    matches!(content.get("noteType").and_then(Value::as_str), None | Some("") | Some("plain-text") | Some("markdown"))
        && matches!(content.get("editorIdentifier").and_then(Value::as_str), None | Some("") | Some(PLAIN_EDITOR))
}

/// The body of a new note made from an edited read-only note: without the
/// read-only line.
pub fn without_banner(body: &str) -> String {
    let mut lines: Vec<&str> = body.split('\n').collect();
    if let Some(i) = lines.iter().position(|l| l.starts_with(READ_ONLY_BANNER)) {
        lines.remove(i);
        if lines.get(i).is_some_and(|l| l.is_empty()) {
            lines.remove(i);
        }
    }
    lines.join("\n")
}

/// Title and text for a body.
fn split_body(body: &str, title_prepended: bool) -> (String, String) {
    let title = title_of(body);
    if title_prepended {
        if let Some((first, rest)) = body.split_once('\n') {
            if first.trim_start().starts_with('#') {
                return (title, rest.to_owned());
            }
        }
    }
    (title, body.to_owned())
}

fn preview(text: &str) -> String {
    snippet_of(text, 90)
}

/// Writes a note into a content, in place. The text is only written for
/// [`Kind::Text`] notes. Returns whether anything changed.
pub fn update_note(content: &mut Value, note: &NoteData, read: &ReadNote) -> bool {
    let before = content.clone();
    if read.kind == Kind::Text {
        let (title, text) = split_body(&note.body, read.title_prepended);
        if str_field(content, "title") != title || str_field(content, "text") != text {
            content["title"] = json!(title);
            content["text"] = json!(text);
            content["preview_plain"] = json!(preview(&note.body));
        }
    }
    if read.kind == Kind::Super {
        let (title, md) = split_body(&note.body, read.title_prepended);
        if str_field(content, "title") != title {
            content["title"] = json!(title);
        }
        // Only rewritten when the text changed, so a note edited only in
        // Super keeps exactly what Super wrote.
        if super_markdown(str_field(content, "text")).ok().as_deref() != Some(md.as_str()) {
            content["text"] = json!(super::lexical::from_doc(&crate::markdown::from_markdown(&md)));
            content["preview_plain"] = json!(preview(&note.body));
        }
    }
    if read.pinned != note.pinned {
        app_data_mut(content, APP_DOMAIN).insert("pinned".into(), json!(note.pinned));
    }
    if read.trashed != note.deleted.is_some() {
        content["trashed"] = json!(note.deleted.is_some());
    }
    match note.deleted {
        Some(t) if read.trashed_at != Some(t) => {
            app_data_mut(content, OUR_DOMAIN).insert("trashed_at".into(), json!(iso(t)));
        }
        None if read.trashed_at.is_some() => {
            app_data_mut(content, OUR_DOMAIN).remove("trashed_at");
        }
        _ => {}
    }
    let changed = *content != before;
    let modified_differs = read.modified.map(|m| m.timestamp_millis()) != Some(note.modified.timestamp_millis());
    if changed || modified_differs {
        app_data_mut(content, APP_DOMAIN).insert("client_updated_at".into(), json!(iso(note.modified)));
    }
    *content != before
}

/// The content of a new note.
pub fn new_note(note: &NoteData) -> Value {
    let (title, text) = split_body(&note.body, false);
    let mut c = json!({
        "title": title,
        "text": text,
        "preview_plain": preview(&note.body),
        "noteType": "plain-text",
        "editorIdentifier": PLAIN_EDITOR,
        "references": [],
        "appData": { APP_DOMAIN: { "client_updated_at": iso(note.modified) } },
    });
    if note.pinned {
        app_data_mut(&mut c, APP_DOMAIN).insert("pinned".into(), json!(true));
    }
    if let Some(t) = note.deleted {
        c["trashed"] = json!(true);
        app_data_mut(&mut c, OUR_DOMAIN).insert("trashed_at".into(), json!(iso(t)));
    }
    c
}

/// Records a change by the user now (`client_updated_at`).
pub fn touch(content: &mut Value, now: DateTime<Utc>) {
    app_data_mut(content, APP_DOMAIN).insert("client_updated_at".into(), json!(iso(now)));
}

/// The content of a new tag.
pub fn new_tag(title: &str, parent: Option<&str>, now: DateTime<Utc>) -> Value {
    let mut c = json!({
        "title": title,
        "expanded": false,
        "references": [],
        "appData": { APP_DOMAIN: { "client_updated_at": iso(now) } },
    });
    if let Some(p) = parent {
        c["references"] = json!([{ "uuid": p, "content_type": "Tag", "reference_type": PARENT_TAG }]);
    }
    c
}

pub fn references(content: &Value) -> &[Value] {
    content.get("references").and_then(Value::as_array).map(Vec::as_slice).unwrap_or(&[])
}

/// A tag's parent tag.
pub fn parent_of(tag: &Value) -> Option<&str> {
    references(tag)
        .iter()
        .find(|r| r.get("reference_type").and_then(Value::as_str) == Some(PARENT_TAG))
        .and_then(|r| r.get("uuid")?.as_str())
}

/// Sets (or clears) a tag's parent, keeping its other references.
pub fn set_parent(tag: &mut Value, parent: Option<&str>) {
    let mut refs: Vec<Value> =
        references(tag).iter().filter(|r| r.get("reference_type").and_then(Value::as_str) != Some(PARENT_TAG)).cloned().collect();
    if let Some(p) = parent {
        refs.push(json!({ "uuid": p, "content_type": "Tag", "reference_type": PARENT_TAG }));
    }
    tag["references"] = json!(refs);
}

/// Whether a tag holds a note.
pub fn tag_has_note(tag: &Value, note: &str) -> bool {
    references(tag).iter().any(|r| r.get("uuid").and_then(Value::as_str) == Some(note) && r.get("content_type").and_then(Value::as_str) == Some("Note"))
}

pub fn add_note_to_tag(tag: &mut Value, note: &str) {
    let mut refs = references(tag).to_vec();
    refs.push(json!({ "uuid": note, "content_type": "Note" }));
    tag["references"] = json!(refs);
}

pub fn remove_note_from_tag(tag: &mut Value, note: &str) {
    let refs: Vec<Value> = references(tag).iter().filter(|r| r.get("uuid").and_then(Value::as_str) != Some(note)).cloned().collect();
    tag["references"] = json!(refs);
}

/// The text of a Super note (Lexical's editor state as JSON), a line per
/// paragraph, list item, heading, quote line or code line.
pub fn lexical_text(json: &str) -> Option<String> {
    let v: Value = serde_json::from_str(json).ok()?;
    let mut lines = Vec::new();
    lexical_blocks(v.get("root")?, &mut lines);
    Some(lines.join("\n"))
}

fn node_type(n: &Value) -> &str {
    n.get("type").and_then(Value::as_str).unwrap_or("")
}

fn node_children(n: &Value) -> &[Value] {
    n.get("children").and_then(Value::as_array).map(Vec::as_slice).unwrap_or(&[])
}

const LEXICAL_BLOCKS: [&str; 6] = ["paragraph", "heading", "quote", "listitem", "code", "tablecell"];

/// The lines of a container (the root, a list, a table, …).
fn lexical_blocks(node: &Value, lines: &mut Vec<String>) {
    for c in node_children(node) {
        if LEXICAL_BLOCKS.contains(&node_type(c)) {
            lexical_block(c, lines);
        } else if let Some(t) = c.get("text").and_then(Value::as_str) {
            lines.push(t.to_owned());
        } else {
            lexical_blocks(c, lines);
        }
    }
}

/// The lines of one block.
fn lexical_block(node: &Value, lines: &mut Vec<String>) {
    let prefix = match node_type(node) {
        "heading" => {
            let level = node.get("tag").and_then(Value::as_str).and_then(|t| t.strip_prefix('h')).and_then(|n| n.parse::<usize>().ok());
            format!("{} ", "#".repeat(level.unwrap_or(1).clamp(1, 6)))
        }
        "quote" => "> ".to_owned(),
        "listitem" => match node.get("checked").and_then(Value::as_bool) {
            Some(true) => "- [x] ".to_owned(),
            Some(false) => "- [ ] ".to_owned(),
            None => "- ".to_owned(),
        },
        _ => String::new(),
    };
    let mut cur = prefix.clone();
    let mut nested = false;
    for c in node_children(node) {
        match node_type(c) {
            "linebreak" => lines.push(std::mem::take(&mut cur)),
            "list" => {
                // A list inside a list item: its own lines.
                if cur != prefix {
                    lines.push(std::mem::take(&mut cur));
                }
                cur.clear();
                nested = true;
                lexical_blocks(c, lines);
            }
            _ => lexical_inline(c, &mut cur),
        }
    }
    if !(nested && (cur.is_empty() || cur == prefix)) {
        lines.push(cur);
    }
}

/// The text of an inline node (text, or a link and the like around text).
fn lexical_inline(node: &Value, cur: &mut String) {
    match node.get("text").and_then(Value::as_str) {
        Some(t) => cur.push_str(t),
        None if node_type(node) == "tab" => cur.push('\t'),
        None => {
            for c in node_children(node) {
                lexical_inline(c, cur);
            }
        }
    }
}

/// The text of HTML, roughly: tags dropped, block ends as line breaks.
pub fn html_text(html: &str) -> String {
    let mut out = String::new();
    let mut rest = html;
    while let Some(i) = rest.find('<') {
        out.push_str(&rest[..i]);
        let Some(j) = rest[i..].find('>') else {
            rest = &rest[i..];
            break;
        };
        let tag = rest[i + 1..i + j].trim().to_lowercase();
        let name = tag.trim_start_matches('/').split(|c: char| c.is_whitespace() || c == '/').next().unwrap_or("");
        if matches!(name, "br" | "p" | "div" | "li" | "h1" | "h2" | "h3" | "h4" | "h5" | "h6" | "tr" | "blockquote" | "pre")
            && (name == "br" || tag.starts_with('/'))
        {
            out.push('\n');
        }
        rest = &rest[i + j + 1..];
    }
    out.push_str(rest);
    let out = out
        .replace("&nbsp;", " ")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&amp;", "&");
    out.trim_end().to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn data(body: &str) -> NoteData {
        NoteData {
            id: "n".into(),
            folder: vec!["Notes".into()],
            body: body.into(),
            created: Utc::now(),
            modified: DateTime::from_timestamp_millis(1_763_000_000_000).unwrap(),
            pinned: false,
            deleted: None,
        }
    }

    #[test]
    fn dates_in_all_the_shapes_the_client_writes() {
        let t = DateTime::parse_from_rfc3339("2026-11-15T11:03:00Z").unwrap().with_timezone(&Utc);
        assert_eq!(parse_date(&json!("2026-11-15T11:03:00.000Z")), Some(t));
        assert_eq!(parse_date(&json!("Sun Nov 15 2026 12:03:00 GMT+0100 (Central European Standard Time)")), Some(t));
        assert_eq!(parse_date(&json!(t.timestamp_millis())), Some(t));
        assert_eq!(parse_date(&json!("nonsense")), None);
    }

    #[test]
    fn note_kinds() {
        assert_eq!(kind(&json!({"text": "x"})), Kind::Text);
        assert_eq!(kind(&json!({"noteType": "markdown", "editorIdentifier": "org.standardnotes.advanced-markdown-editor"})), Kind::Text);
        assert_eq!(kind(&json!({"noteType": "plain-text"})), Kind::Text);
        assert_eq!(kind(&json!({"noteType": "super"})), Kind::Super);
        assert_eq!(kind(&json!({"noteType": "code"})), Kind::ReadOnly);
        assert_eq!(kind(&json!({"editorIdentifier": "org.standardnotes.plus-editor"})), Kind::ReadOnly);
        assert_eq!(kind(&json!({"text": "x", "appData": {APP_DOMAIN: {"locked": true}}})), Kind::ReadOnly);
    }

    #[test]
    fn a_separate_title_goes_on_top_and_comes_off_again() {
        let mut c = json!({"title": "Groceries", "text": "eggs\nmilk", "references": [], "spellcheck": false});
        let r = read_note(&c);
        assert_eq!(r.body, "# Groceries\neggs\nmilk");
        assert!(r.title_prepended);
        assert!(update_note(&mut c, &data("# Groceries\neggs\nmilk\nbread"), &r));
        assert_eq!(c["title"], "Groceries");
        assert_eq!(c["text"], "eggs\nmilk\nbread");
        assert_eq!(c["spellcheck"], false);
        // A title that is the first line isn't repeated.
        let c = json!({"title": "Plan", "text": "# Plan\n- [ ] x"});
        assert_eq!(read_note(&c).body, "# Plan\n- [ ] x");
    }

    #[test]
    fn unchanged_notes_stay_unchanged() {
        let mut c = new_note(&data("# A\nb"));
        let r = read_note(&c);
        assert!(!update_note(&mut c, &data("# A\nb"), &r));
    }

    #[test]
    fn super_notes_as_text() {
        let lexical = r#"{"root":{"type":"root","children":[
            {"type":"heading","tag":"h2","children":[{"type":"text","text":"Trip"}]},
            {"type":"paragraph","children":[{"type":"text","text":"Pack "},{"type":"text","text":"light","format":1},{"type":"linebreak"},{"type":"text","text":"really"}]},
            {"type":"list","listType":"check","children":[
                {"type":"listitem","checked":true,"children":[{"type":"text","text":"Passport"}]},
                {"type":"listitem","checked":false,"children":[{"type":"text","text":"Tickets"}]}]},
            {"type":"quote","children":[{"type":"text","text":"Go"}]}
        ]}}"#;
        assert_eq!(lexical_text(lexical).unwrap(), "## Trip\nPack light\nreally\n- [x] Passport\n- [ ] Tickets\n> Go");
        let c = json!({"title": "Trip", "text": lexical, "noteType": "super", "editorIdentifier": "com.standardnotes.super-editor"});
        let r = read_note(&c);
        assert_eq!(r.kind, Kind::ReadOnly);
        assert!(r.body.starts_with("# Trip\n> Shown read-only: this is a Super note"));
        assert!(r.body.ends_with("- [ ] Tickets\n> Go"));
        assert_eq!(without_banner(&r.body), format!("# Trip\n{}", lexical_text(lexical).unwrap()));
    }

    #[test]
    fn html_as_text() {
        assert_eq!(html_text("<p>Hello &amp; <b>bye</b></p><ul><li>one</li><li>two</li></ul>"), "Hello & bye\none\ntwo");
    }

    #[test]
    fn tag_references() {
        let mut t = new_tag("Work", Some("p1"), Utc::now());
        assert_eq!(parent_of(&t), Some("p1"));
        add_note_to_tag(&mut t, "n1");
        assert!(tag_has_note(&t, "n1"));
        set_parent(&mut t, None);
        assert_eq!(parent_of(&t), None);
        assert!(tag_has_note(&t, "n1"));
        remove_note_from_tag(&mut t, "n1");
        assert!(references(&t).is_empty());
    }
}
