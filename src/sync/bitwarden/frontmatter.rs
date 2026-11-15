//! How a note is written into a Bitwarden secure note: Jekyll style, YAML
//! front matter with the note's metadata, then its Markdown.
//!
//! ```text
//! ---
//! id: 6f1c0d3e-…
//! created: 2026-11-15T12:03:00.000Z
//! modified: 2026-11-15T12:30:12.500Z
//! pinned: true
//! tags: [travel, work]
//! ---
//! # Weekend trip
//! …
//! ```
//!
//! The item's name is the note's title, so the vault lists notes the way the
//! app does. Bitwarden caps an item's notes at 10,000 encrypted characters;
//! a longer note continues in hidden custom fields `cleannotes-part-2`,
//! `-3`, … (`parts:` in the front matter says how many there are).
//!
//! A secure note typed into the vault directly, without front matter, is
//! read as a note too.

use chrono::{DateTime, Utc};

use super::crypto::{encrypted_len, max_plain_len};
use crate::sync::NoteData;

/// Bitwarden's limits, in encrypted characters.
pub const NOTES_LIMIT: usize = 10_000;
pub const FIELD_LIMIT: usize = 5_000;
pub const NAME_LIMIT: usize = 1_000;
pub const PART_FIELD: &str = "cleannotes-part-";

/// A note's text split up for a cipher.
#[derive(Debug, PartialEq)]
pub struct Written {
    pub name: String,
    pub notes: String,
    /// (field name, value) for the hidden fields holding the rest.
    pub parts: Vec<(String, String)>,
}

fn ts(t: DateTime<Utc>) -> String {
    t.to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

/// A YAML scalar, quoted when it would otherwise read as something else.
fn scalar(s: &str) -> String {
    let plain = !s.is_empty()
        && s.chars().all(|c| c.is_alphanumeric() || matches!(c, '-' | '_' | '.' | ':' | '/' | '+'))
        && !s.starts_with(['-', ':'])
        && !matches!(s, "true" | "false" | "null" | "~" | "yes" | "no");
    if plain {
        s.to_owned()
    } else {
        format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
    }
}

fn front_matter(n: &NoteData, parts: usize) -> String {
    let mut fm = String::from("---\n");
    fm.push_str(&format!("id: {}\n", scalar(&n.id)));
    fm.push_str(&format!("created: {}\n", ts(n.created)));
    fm.push_str(&format!("modified: {}\n", ts(n.modified)));
    if n.pinned {
        fm.push_str("pinned: true\n");
    }
    if let Some(d) = n.deleted {
        fm.push_str(&format!("deleted: {}\n", ts(d)));
    }
    let mut tags: Vec<String> = Vec::new();
    for line in n.body.lines() {
        for t in crate::doc::tags_in(line) {
            if !tags.contains(&t) {
                tags.push(t);
            }
        }
    }
    if !tags.is_empty() {
        let list: Vec<String> = tags.iter().map(|t| scalar(t)).collect();
        fm.push_str(&format!("tags: [{}]\n", list.join(", ")));
    }
    if parts > 1 {
        fm.push_str(&format!("parts: {parts}\n"));
    }
    fm.push_str("---\n");
    fm
}

/// The longest prefix of `s` of at most `max` bytes, on a char boundary.
fn take(s: &str, max: usize) -> &str {
    if s.len() <= max {
        return s;
    }
    let mut i = max;
    while !s.is_char_boundary(i) {
        i -= 1;
    }
    &s[..i]
}

pub fn write(n: &NoteData) -> Written {
    let title = crate::markdown::title_of(&n.body);
    let title = if title.is_empty() { "New Note".to_owned() } else { title };
    let name = take(&title, max_plain_len(NAME_LIMIT)).to_owned();
    let notes_max = max_plain_len(NOTES_LIMIT);
    let field_max = max_plain_len(FIELD_LIMIT);
    // The number of parts changes the front matter's length, so settle it.
    let mut parts = 1;
    loop {
        let fm = front_matter(n, parts);
        let first = take(&n.body, notes_max.saturating_sub(fm.len()));
        let mut rest = &n.body[first.len()..];
        let mut chunks = Vec::new();
        while !rest.is_empty() {
            let c = take(rest, field_max);
            chunks.push(c);
            rest = &rest[c.len()..];
        }
        if chunks.len() + 1 == parts {
            let notes = format!("{fm}{first}");
            debug_assert!(encrypted_len(notes.len()) <= NOTES_LIMIT);
            let parts = chunks.iter().enumerate().map(|(i, c)| (format!("{PART_FIELD}{}", i + 2), (*c).to_owned())).collect();
            return Written { name, notes, parts };
        }
        parts = chunks.len() + 1;
    }
}

/// What a cipher's item says, decrypted.
pub struct Item<'a> {
    pub cipher_id: &'a str,
    pub name: &'a str,
    pub notes: &'a str,
    /// Custom fields (name, value).
    pub fields: &'a [(String, String)],
    pub created: DateTime<Utc>,
    pub revised: DateTime<Utc>,
}

/// The note in a cipher. `folder` is filled in by the caller.
pub fn read(item: &Item) -> NoteData {
    let fallback = || NoteData {
        // A stable id for an item the app didn't write.
        id: format!("bw-{}", item.cipher_id),
        folder: vec![],
        body: plain_body(item.name, item.notes),
        created: item.created,
        modified: item.revised,
        pinned: false,
        deleted: None,
    };
    let text = item.notes.replace("\r\n", "\n");
    let Some(rest) = text.strip_prefix("---\n") else { return fallback() };
    let Some(end) = rest.find("\n---\n").map(|i| i + 1).or_else(|| rest.strip_suffix("---").map(|r| r.len()))
    else {
        return fallback();
    };
    let header = &rest[..end];
    let mut body = rest[end..].strip_prefix("---").unwrap_or("").strip_prefix('\n').unwrap_or("").to_owned();
    let mut n = fallback();
    let mut parts = 1;
    for line in header.lines() {
        let Some((k, v)) = line.split_once(':') else { continue };
        let v = unquote(v.trim());
        let time = || DateTime::parse_from_rfc3339(&v).ok().map(|t| t.with_timezone(&Utc));
        match k.trim() {
            "id" if !v.is_empty() => n.id = v.clone(),
            "created" => n.created = time().unwrap_or(n.created),
            "modified" => n.modified = time().unwrap_or(n.modified),
            "pinned" => n.pinned = v == "true",
            "deleted" => n.deleted = time(),
            "parts" => parts = v.parse().unwrap_or(1),
            _ => {}
        }
    }
    for i in 2..=parts {
        let name = format!("{PART_FIELD}{i}");
        if let Some((_, v)) = item.fields.iter().find(|(k, _)| *k == name) {
            body.push_str(v);
        }
    }
    n.body = body;
    n
}

/// The body of an item typed into the vault: its notes, headed by its name
/// unless the notes start with it already.
fn plain_body(name: &str, notes: &str) -> String {
    let notes = notes.replace("\r\n", "\n");
    let first = notes.lines().next().unwrap_or("").trim().trim_start_matches('#').trim();
    if name.trim().is_empty() || first == name.trim() {
        notes
    } else if notes.is_empty() {
        format!("# {}\n", name.trim())
    } else {
        format!("# {}\n{notes}", name.trim())
    }
}

fn unquote(v: &str) -> String {
    if v.len() >= 2 && v.starts_with('"') && v.ends_with('"') {
        let inner = &v[1..v.len() - 1];
        let mut out = String::new();
        let mut esc = false;
        for c in inner.chars() {
            if esc {
                out.push(c);
                esc = false;
            } else if c == '\\' {
                esc = true;
            } else {
                out.push(c);
            }
        }
        out
    } else if v.len() >= 2 && v.starts_with('\'') && v.ends_with('\'') {
        v[1..v.len() - 1].replace("''", "'")
    } else {
        v.to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn note(body: &str) -> NoteData {
        NoteData {
            id: "6f1c0d3e-0000-4000-8000-000000000001".into(),
            folder: vec![],
            body: body.into(),
            created: DateTime::parse_from_rfc3339("2026-11-15T12:03:00Z").unwrap().into(),
            modified: DateTime::parse_from_rfc3339("2026-11-15T12:30:12.5Z").unwrap().into(),
            pinned: true,
            deleted: None,
        }
    }

    fn item<'a>(w: &'a Written, fields: &'a [(String, String)]) -> Item<'a> {
        Item { cipher_id: "c1", name: &w.name, notes: &w.notes, fields, created: Utc::now(), revised: Utc::now() }
    }

    #[test]
    fn jekyll_style() {
        let n = note("# Weekend trip\n- [ ] pack #travel\n");
        let w = write(&n);
        assert_eq!(w.name, "Weekend trip");
        assert_eq!(
            w.notes,
            "---\nid: 6f1c0d3e-0000-4000-8000-000000000001\ncreated: 2026-11-15T12:03:00.000Z\nmodified: 2026-11-15T12:30:12.500Z\npinned: true\ntags: [travel]\n---\n# Weekend trip\n- [ ] pack #travel\n"
        );
        assert!(w.parts.is_empty());
        assert_eq!(read(&item(&w, &[])), n);
    }

    #[test]
    fn long_notes_continue_in_fields() {
        let line = "Ünïcode text that goes on and on, with **some** markup — ✓\n";
        let body = line.repeat(100_000 / line.len() + 1);
        let n = NoteData { deleted: Some(Utc::now()), ..note(&body) };
        let w = write(&n);
        assert!(w.parts.len() > 10);
        assert!(encrypted_len(w.notes.len()) <= NOTES_LIMIT);
        assert!(w.parts.iter().all(|(_, v)| encrypted_len(v.len()) <= FIELD_LIMIT));
        assert!(w.notes.contains(&format!("parts: {}\n", w.parts.len() + 1)));
        let back = read(&item(&w, &w.parts));
        assert_eq!(back.body, body);
        assert_eq!(back.deleted.map(|d| d.timestamp_millis()), n.deleted.map(|d| d.timestamp_millis()));
    }

    #[test]
    fn notes_typed_into_the_vault() {
        let w = Written { name: "Wi-Fi".into(), notes: "password is hunter2".into(), parts: vec![] };
        let n = read(&item(&w, &[]));
        assert_eq!(n.body, "# Wi-Fi\npassword is hunter2");
        assert_eq!(n.id, "bw-c1");
        let w = Written { name: "Ideas".into(), notes: "Ideas\n- one".into(), parts: vec![] };
        assert_eq!(read(&item(&w, &[])).body, "Ideas\n- one");
        // Something that only looks like front matter.
        let w = Written { name: "X".into(), notes: "---\nnot closed".into(), parts: vec![] };
        assert_eq!(read(&item(&w, &[])).body, "# X\n---\nnot closed");
    }

    #[test]
    fn odd_values_are_quoted() {
        assert_eq!(scalar("true"), "\"true\"");
        assert_eq!(scalar("a \"b\""), "\"a \\\"b\\\"\"");
        assert_eq!(unquote(&scalar("a \"b\" \\ c")), "a \"b\" \\ c");
        assert_eq!(scalar("home-office"), "home-office");
    }

    #[test]
    fn an_empty_note_and_crlf() {
        let n = note("");
        let w = write(&n);
        assert_eq!(w.name, "New Note");
        assert_eq!(read(&item(&w, &[])).body, "");
        let crlf = w.notes.replace('\n', "\r\n");
        let w2 = Written { notes: crlf, ..w };
        assert_eq!(read(&item(&w2, &[])).id, n.id);
    }
}
