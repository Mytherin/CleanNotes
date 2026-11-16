//! How a note is written into an S3 object: YAML front matter with its
//! metadata, then its Markdown.
//!
//! ```text
//! ---
//! id: 6f1c0d3e-…
//! folder: [Notes, Recipes]
//! created: 2026-11-15T12:03:00.000Z
//! modified: 2026-11-15T12:30:12.500Z
//! pinned: true
//! tags: [baking]
//! ---
//! # Pancakes
//! …
//! ```
//!
//! A note deleted for good leaves a tombstone (`gone:` instead of the
//! note), so a delete can be refused when the note changed since, and other
//! devices see the deletion. A `.md` file put into the folder by hand,
//! without front matter, is read as a note too.

use chrono::{DateTime, Utc};

use crate::sync::NoteData;

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

/// A YAML scalar's value: unquoted (`"…"` with backslash escapes, or
/// `'…'`), else as it is.
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

fn ts(t: DateTime<Utc>) -> String {
    t.to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

fn list(items: &[String]) -> String {
    let v: Vec<String> = items.iter().map(|s| scalar(s)).collect();
    format!("[{}]", v.join(", "))
}

pub fn write(n: &NoteData) -> String {
    let mut fm = String::from("---\n");
    fm.push_str(&format!("id: {}\n", scalar(&n.id)));
    fm.push_str(&format!("folder: {}\n", list(&n.folder)));
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
        fm.push_str(&format!("tags: {}\n", list(&tags)));
    }
    fm.push_str("---\n");
    fm.push_str(&n.body);
    fm
}

pub fn tombstone(id: &str, now: DateTime<Utc>) -> String {
    format!("---\nid: {}\ngone: {}\n---\n", scalar(id), ts(now))
}

/// What an object holds.
#[derive(Debug, PartialEq)]
pub enum Object {
    Note(NoteData),
    /// A note deleted for good.
    Gone,
}

/// Reads an object. `stem` is its name without `.md`, `modified` its last
/// change in S3 (used for files without front matter).
pub fn read(text: &str, stem: &str, modified: DateTime<Utc>) -> Object {
    let fallback = |body: String| NoteData {
        id: format!("s3-{stem}"),
        folder: vec![crate::model::DEFAULT_FOLDER.to_owned()],
        body,
        created: modified,
        modified,
        pinned: false,
        deleted: None,
    };
    // A hand-written file is read the same on every device, so its line
    // endings may be tidied; a note's own text is kept byte for byte.
    let hand_written = || Object::Note(fallback(text.replace("\r\n", "\n")));
    let Some(rest) = text.strip_prefix("---\n").or_else(|| text.strip_prefix("---\r\n")) else { return hand_written() };
    // The front matter ends at a line that is just `---` (in whatever line
    // endings it was saved with).
    let mut at = 0;
    let mut end = None;
    for line in rest.split_inclusive('\n') {
        if line.trim_end_matches('\n').trim_end_matches('\r') == "---" {
            end = Some((at, at + line.len()));
            break;
        }
        at += line.len();
    }
    let Some((header_end, body_start)) = end else { return hand_written() };
    let header = &rest[..header_end];
    let body = rest[body_start..].to_owned();
    // Ours starts with an id; anything else is part of a hand-written file.
    let has_id = header.lines().next().and_then(|l| l.split_once(':')).is_some_and(|(k, v)| k.trim() == "id" && !unquote(v.trim()).is_empty());
    if !has_id {
        return hand_written();
    }
    let mut n = fallback(body);
    for line in header.lines() {
        let Some((k, v)) = line.split_once(':') else { continue };
        let raw = v.trim();
        let v = unquote(raw);
        let time = || DateTime::parse_from_rfc3339(&v).ok().map(|t| t.with_timezone(&Utc));
        match k.trim() {
            "id" => n.id = v.clone(),
            "gone" => return Object::Gone,
            "folder" => {
                let f = parse_list(raw);
                if !f.is_empty() {
                    n.folder = f;
                }
            }
            "created" => n.created = time().unwrap_or(n.created),
            "modified" => n.modified = time().unwrap_or(n.modified),
            "pinned" => n.pinned = v == "true",
            "deleted" => n.deleted = time(),
            _ => {}
        }
    }
    Object::Note(n)
}

/// A YAML flow sequence of scalars: `[a, "b, c", 'd']`.
fn parse_list(s: &str) -> Vec<String> {
    let Some(inner) = s.trim().strip_prefix('[').and_then(|r| r.strip_suffix(']')) else {
        let v = unquote(s.trim());
        return if v.is_empty() { vec![] } else { vec![v] };
    };
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quote: Option<char> = None;
    let mut esc = false;
    for c in inner.chars() {
        match quote {
            Some(q) => {
                cur.push(c);
                if esc {
                    esc = false;
                } else if c == '\\' && q == '"' {
                    esc = true;
                } else if c == q {
                    quote = None;
                }
            }
            None if c == ',' => {
                out.push(unquote(cur.trim()));
                cur.clear();
            }
            None => {
                if (c == '"' || c == '\'') && cur.trim().is_empty() {
                    quote = Some(c);
                }
                cur.push(c);
            }
        }
    }
    if !cur.trim().is_empty() {
        out.push(unquote(cur.trim()));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn note(folder: &[&str], body: &str) -> NoteData {
        NoteData {
            id: "6f1c0d3e-0000-4000-8000-000000000001".into(),
            folder: folder.iter().map(|s| s.to_string()).collect(),
            body: body.into(),
            created: DateTime::parse_from_rfc3339("2026-11-15T12:03:00Z").unwrap().into(),
            modified: DateTime::parse_from_rfc3339("2026-11-15T12:30:12.5Z").unwrap().into(),
            pinned: true,
            deleted: None,
        }
    }

    #[test]
    fn round_trip() {
        let n = note(&["Notes", "Recipes, \"best\"", "true"], "# Pancakes\nflour #baking\n");
        let text = write(&n);
        assert!(text.starts_with("---\nid: 6f1c0d3e-0000-4000-8000-000000000001\nfolder: [Notes, \"Recipes, \\\"best\\\"\", \"true\"]\n"));
        assert!(text.contains("tags: [baking]\n"));
        assert_eq!(read(&text, "x", Utc::now()), Object::Note(n));
    }

    #[test]
    fn the_text_comes_back_exactly() {
        // Windows line endings in a note's text stay as they are (else
        // every line differs from the device that saved it, and merges
        // fail); front matter edited on Windows is still read.
        let n = note(&["Notes"], "# A\r\nb\r\n");
        assert_eq!(read(&write(&n), "x", Utc::now()), Object::Note(n.clone()));
        let crlf = write(&n).replacen("---\n", "---\r\n", 1).replace("Z\n", "Z\r\n");
        let Object::Note(m) = read(&crlf, "x", Utc::now()) else { panic!() };
        assert_eq!((m.id, m.created), (n.id, n.created));
    }

    #[test]
    fn deleted_and_empty() {
        let n = NoteData { deleted: Some(DateTime::parse_from_rfc3339("2026-11-16T00:00:00Z").unwrap().into()), pinned: false, ..note(&["Notes"], "") };
        assert_eq!(read(&write(&n), "x", Utc::now()), Object::Note(n));
    }

    #[test]
    fn tombstones() {
        assert_eq!(read(&tombstone("abc", Utc::now()), "abc", Utc::now()), Object::Gone);
    }

    #[test]
    fn hand_written_files() {
        let t = Utc::now();
        let Object::Note(n) = read("# Shopping\r\neggs\r\n", "shopping", t) else { panic!() };
        assert_eq!(n.id, "s3-shopping");
        assert_eq!(n.body, "# Shopping\neggs\n");
        assert_eq!(n.folder, vec!["Notes"]);
        assert_eq!(n.modified, t);
        let Object::Note(n) = read("---\ntitle: x\n---\nbody", "y", t) else { panic!() };
        assert_eq!(n.body, "---\ntitle: x\n---\nbody");
    }

    #[test]
    fn lists() {
        assert_eq!(parse_list("[a, 'b, c', \"d\\\"e\"]"), vec!["a", "b, c", "d\"e"]);
        assert_eq!(parse_list("[]"), Vec::<String>::new());
        assert_eq!(parse_list("Notes"), vec!["Notes"]);
    }
}
