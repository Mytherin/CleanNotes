//! Tab notes: browser tabs that CleanTabs (a Chrome extension) saved as
//! notes, which CleanNotes shows on their own, as previews of their web
//! pages, apart from the account's notes.
//!
//! They're kept in a sub-folder of the S3 account's folder, [`DIR`]
//! (`<bucket>/CleanNotes/tabs/`), laid out exactly as a CleanNotes folder
//! is (see [`super`]: `tabs/head`, `tabs/notes/…`, `tabs/folders/…`,
//! `tabs/attachments/…`). CleanNotes reads and syncs it as a second, linked
//! source of the account, so tabs never mix with the notes, and its notes
//! sync (which reads only `notes/` and `folders/`) never sees them. It's
//! encrypted when the account's folder is, with the same key; it has no
//! `encryption.json` of its own.
//!
//! A tab note's id starts with [`ID_PREFIX`], and its folder is the
//! collection it was saved to: `[Research]`, and a tab group saved with its
//! window `[Research, Docs]`. (The first versions kept tab notes among the
//! account's notes, under `Tabs` or `CleanTabs`; CleanTabs moves those, see
//! [`legacy_folder`].) Its Markdown is:
//!
//! ```text
//! # <the page's title>
//!
//! [<the address, shortened>](<the address>)
//!
//! ![](attachments/<sha256>.jpg)     a small picture of the page, if taken
//!
//! <anything written about it since, in CleanNotes>
//! ```
//!
//! Tabs are put back in the order the notes were made in (`created`), so
//! CleanTabs makes a saved window's notes a millisecond apart.

use super::NoteData;

/// What a tab note's id starts with.
pub const ID_PREFIX: &str = "tab-";
/// The sub-folder of an account's folder that tabs are kept in.
pub const DIR: &str = "tabs";
/// Where tabs saved without a collection go.
pub const DEFAULT_COLLECTION: &str = "Saved Tabs";

/// The S3 prefix of the tabs of an account whose folder is
/// `account_prefix` (`CleanNotes` → `CleanNotes/tabs`).
pub fn dir_prefix(account_prefix: &str) -> String {
    let p = account_prefix.trim().trim_matches('/');
    if p.is_empty() {
        DIR.to_owned()
    } else {
        format!("{p}/{DIR}")
    }
}

/// Folders the first versions kept tab notes in, among the account's notes.
const LEGACY_ROOTS: [&str; 2] = ["Tabs", "CleanTabs"];

/// For a tab note the first versions saved among the account's notes: its
/// collection in [`DIR`] (its folder below `Tabs` or `CleanTabs`, else
/// [`DEFAULT_COLLECTION`]).
pub fn legacy_folder(folder: &[String]) -> Vec<String> {
    let rel = match folder.first() {
        Some(r) if LEGACY_ROOTS.contains(&r.as_str()) => &folder[1..],
        _ => &[][..],
    };
    if rel.is_empty() {
        vec![DEFAULT_COLLECTION.to_owned()]
    } else {
        rel.to_vec()
    }
}

/// A tab note's contents.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TabNote {
    pub title: String,
    pub url: String,
    /// The page's picture: an attachment's name (`<sha256>.jpg`).
    pub thumbnail: Option<String>,
    /// What else the note says.
    pub text: String,
}

/// Whether a note is a tab note.
pub fn is_tab(n: &NoteData) -> bool {
    n.id.starts_with(ID_PREFIX) && TabNote::parse(&n.body).is_some()
}

/// A new tab note's id.
pub fn new_id() -> String {
    format!("{ID_PREFIX}{}", super::new_id())
}

fn escape(text: &str) -> String {
    let mut out = String::new();
    for c in text.chars() {
        if matches!(c, '\\' | '[' | ']' | '*' | '_' | '`' | '<' | '>' | '#' | '~' | '!') {
            out.push('\\');
        }
        out.push(if c == '\n' || c == '\r' { ' ' } else { c });
    }
    out
}

fn unescape(text: &str) -> String {
    let mut out = String::new();
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\\' && chars.peek().is_some_and(|n| n.is_ascii_punctuation()) {
            out.push(chars.next().unwrap());
        } else {
            out.push(c);
        }
    }
    out
}

/// A link's destination as Markdown: in `<…>` when it has spaces,
/// parentheses or angle brackets.
fn destination(url: &str) -> String {
    if url.contains([' ', '(', ')', '<', '>']) {
        format!("<{}>", url.replace('<', "%3C").replace('>', "%3E"))
    } else {
        url.to_owned()
    }
}

/// The address shown for a link: without the scheme and `www.`, at most
/// 80 characters.
pub fn short_url(url: &str) -> String {
    let rest = url.split_once("://").map_or(url, |(_, r)| r);
    let rest = rest.strip_prefix("www.").unwrap_or(rest).trim_end_matches('/');
    let mut s: String = rest.chars().take(80).collect();
    if rest.chars().count() > 80 {
        s.push('…');
    }
    s
}

/// The address's host, without `www.`.
pub fn host(url: &str) -> Option<String> {
    let rest = url.split_once("://")?.1;
    let host = rest.split(['/', '?', '#']).next()?.rsplit('@').next()?;
    let host = host.split(':').next()?.trim_start_matches("www.");
    (!host.is_empty()).then(|| host.to_lowercase())
}

/// A link line, `[text](dest)` (and nothing else): its destination.
fn link_target(line: &str) -> Option<String> {
    let line = line.trim();
    let rest = line.strip_prefix('[')?;
    // The text: up to the `]` that isn't escaped.
    let mut i = 0;
    let b = rest.as_bytes();
    let close = loop {
        match b.get(i)? {
            b'\\' => i += 2,
            b']' => break i,
            _ => i += 1,
        }
    };
    let dest = rest[close + 1..].strip_prefix('(')?.strip_suffix(')')?;
    let dest = match dest.strip_prefix('<') {
        Some(d) => d.strip_suffix('>')?.to_owned(),
        None => dest.split_whitespace().next()?.to_owned(),
    };
    (!dest.is_empty()).then_some(dest)
}

/// An image line, `![…](attachments/<name>)`: the attachment's name.
fn image_name(line: &str) -> Option<String> {
    let rest = line.trim().strip_prefix('!')?;
    let target = link_target(rest)?;
    let name = target.strip_prefix(super::ATTACHMENTS)?;
    (!name.contains('/') && !name.contains('#')).then(|| name.to_owned())
}

impl TabNote {
    pub fn to_markdown(&self) -> String {
        let title = if self.title.trim().is_empty() { short_url(&self.url) } else { self.title.trim().to_owned() };
        let mut md = format!("# {}\n\n[{}]({})\n", escape(&title), escape(&short_url(&self.url)), destination(&self.url));
        if let Some(t) = &self.thumbnail {
            md.push_str(&format!("\n![]({}{t})\n", super::ATTACHMENTS));
        }
        if !self.text.trim().is_empty() {
            md.push('\n');
            md.push_str(self.text.trim_end());
            md.push('\n');
        }
        md
    }

    /// A tab note's contents, or `None` if `body` isn't one: a `# ` title,
    /// then (past blank lines) a line that's just a link.
    pub fn parse(body: &str) -> Option<TabNote> {
        let mut lines = body.lines().peekable();
        let title = unescape(lines.next()?.strip_prefix("# ")?.trim());
        let mut next = || loop {
            let l = lines.next()?;
            if !l.trim().is_empty() {
                return Some(l);
            }
        };
        let url = link_target(next()?)?;
        let mut tab = TabNote { title, url, thumbnail: None, text: String::new() };
        let rest: Vec<&str> = lines.collect();
        let mut at = rest.iter().position(|l| !l.trim().is_empty()).unwrap_or(rest.len());
        if let Some(name) = rest.get(at).and_then(|l| image_name(l)) {
            tab.thumbnail = Some(name);
            at += 1;
        }
        let text = rest[at.min(rest.len())..].join("\n");
        tab.text = text.trim_matches('\n').to_owned();
        Some(tab)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        let t = TabNote {
            title: "Rust *is* [great] # yes".into(),
            url: "https://en.wikipedia.org/wiki/Foo_(bar)?q=a b".into(),
            thumbnail: Some(format!("{}.jpg", "a".repeat(64))),
            text: "Read this later.\n\n- point".into(),
        };
        let md = t.to_markdown();
        assert!(md.starts_with("# Rust \\*is\\* \\[great\\] \\# yes\n\n[en.wikipedia.org/wiki/Foo\\_(bar)?q=a b](<https://en.wikipedia.org/wiki/Foo_(bar)?q=a b>)\n\n![](attachments/"), "{md}");
        assert_eq!(TabNote::parse(&md), Some(t));
        let plain = TabNote { title: "Example".into(), url: "https://example.com/".into(), thumbnail: None, text: String::new() };
        assert_eq!(plain.to_markdown(), "# Example\n\n[example.com](https://example.com/)\n");
        assert_eq!(TabNote::parse(&plain.to_markdown()), Some(plain));
    }

    #[test]
    fn untitled_tabs_are_named_by_their_address() {
        let t = TabNote { url: "https://www.example.com/a".into(), ..Default::default() };
        assert_eq!(TabNote::parse(&t.to_markdown()).unwrap().title, "example.com/a");
    }

    #[test]
    fn other_notes_are_not_tabs() {
        assert_eq!(TabNote::parse("# Shopping\n\neggs\n"), None);
        assert_eq!(TabNote::parse("Just text"), None);
        assert_eq!(TabNote::parse("# T\n\n[a](https://a.b) and more\n"), None);
        let t = chrono::Utc::now();
        let n = |id: &str| NoteData { id: id.into(), folder: vec![DEFAULT_COLLECTION.into()], body: "# A\n\n[a.b](https://a.b)\n".into(), created: t, modified: t, pinned: false, deleted: None };
        assert!(is_tab(&n("tab-1")));
        assert!(!is_tab(&n("6f1c0d3e")));
    }

    #[test]
    fn folders() {
        let p = |s: &[&str]| s.iter().map(|x| x.to_string()).collect::<Vec<_>>();
        assert_eq!(dir_prefix("CleanNotes"), "CleanNotes/tabs");
        assert_eq!(dir_prefix("/notes/work/"), "notes/work/tabs");
        assert_eq!(dir_prefix(""), "tabs");
        assert_eq!(legacy_folder(&p(&["CleanTabs", "Research", "Docs"])), p(&["Research", "Docs"]));
        assert_eq!(legacy_folder(&p(&["Tabs", "Research"])), p(&["Research"]));
        assert_eq!(legacy_folder(&p(&["Tabs"])), p(&["Saved Tabs"]));
        assert_eq!(legacy_folder(&p(&["Notes"])), p(&["Saved Tabs"]));
    }

    #[test]
    fn hosts() {
        assert_eq!(host("https://www.Example.com:8080/x").as_deref(), Some("example.com"));
        assert_eq!(host("https://user@a.b/"), Some("a.b".into()));
        assert_eq!(host("about:blank"), None);
        assert_eq!(short_url(&format!("https://x.y/{}", "a".repeat(100))).chars().count(), 81);
    }
}
