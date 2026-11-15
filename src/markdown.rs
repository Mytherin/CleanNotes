//! Notes are stored as Markdown. This converts between Markdown and the
//! editor's [`Doc`].
//!
//! Each paragraph of a note is one line (an empty line is an empty
//! paragraph), so a note reads in a plain text editor the way it looks in the
//! app:
//!
//! | Notes | Markdown |
//! |---|---|
//! | Title / Heading / Subheading | `# `, `## `, `### ` |
//! | Body | the text |
//! | Monospaced | a fenced code block (```` ``` ````) |
//! | Bulleted list (•) | `* ` |
//! | Dashed list (–) | `- ` |
//! | Numbered list | `1. ` |
//! | Checklist | `- [ ] ` and `- [x] ` |
//! | Block quote | `> ` |
//! | Bold, italic, strikethrough | `**…**`, `*…*`, `~~…~~` |
//! | Underline | `<u>…</u>` |
//! | Link | `[text](url)` |
//!
//! List items are indented four spaces per level. Characters that would
//! otherwise be read as markup are escaped with a backslash. Markdown written
//! elsewhere is read leniently: `+` bullets, `_italic_`, `__bold__`, deeper
//! headings, `1)` numbers, and any indentation.

use std::sync::Arc;

use crate::doc::{Block, Doc, Kind, Style};

const INDENT: &str = "    ";

pub fn to_markdown(doc: &Doc) -> String {
    let mut out = String::new();
    let numbers = doc.list_numbers();
    // The fence of the open code block.
    let mut fence: Option<String> = None;
    for (i, b) in doc.blocks.iter().enumerate() {
        if b.kind == Kind::Monospaced {
            if fence.is_none() {
                let f = code_fence(&doc.blocks[i..]);
                out.push_str(&f);
                out.push('\n');
                fence = Some(f);
            }
            out.push_str(&b.text);
            out.push('\n');
            continue;
        }
        if let Some(f) = fence.take() {
            out.push_str(&f);
            out.push('\n');
        }
        if b.kind.indents() {
            for _ in 0..b.indent {
                out.push_str(INDENT);
            }
        }
        let prefix = match b.kind {
            Kind::Title => "# ".to_owned(),
            Kind::Heading => "## ".to_owned(),
            Kind::Subheading => "### ".to_owned(),
            Kind::Body | Kind::Monospaced => String::new(),
            Kind::Bulleted => "* ".to_owned(),
            Kind::Dashed => "- ".to_owned(),
            Kind::Numbered => format!("{}. ", numbers[i].max(1)),
            Kind::Checklist(false) => "- [ ] ".to_owned(),
            Kind::Checklist(true) => "- [x] ".to_owned(),
            Kind::Quote => "> ".to_owned(),
        };
        out.push_str(&prefix);
        let inline = inline_markdown(b);
        // Text that would read as a heading, list, quote or code fence gets
        // its first character escaped.
        // (The reader takes a tab for four spaces.)
        let rest = inline.trim_start_matches([' ', '\t']);
        let lead = inline.len() - rest.len();
        let prefix = parse_prefix(rest);
        let reads_as_markup = prefix.is_some() || opens_fence(rest).is_some();
        if matches!(b.kind, Kind::Body | Kind::Quote) && reads_as_markup {
            // Digits can't be escaped: "1\. text".
            let at = match prefix {
                Some(Prefix { kind: Kind::Numbered, .. }) => rest.bytes().take_while(u8::is_ascii_digit).count(),
                _ => 0,
            };
            out.push_str(&inline[..lead + at]);
            out.push('\\');
            out.push_str(&rest[at..]);
        } else {
            out.push_str(&inline);
        }
        out.push('\n');
    }
    if let Some(f) = fence {
        out.push_str(&f);
        out.push('\n');
    }
    // One trailing newline, however the note ends.
    while out.ends_with("\n\n") {
        out.pop();
    }
    if out.trim().is_empty() {
        return String::new();
    }
    out
}

/// The fence for a code block starting at `blocks[0]`: longer than any run
/// of backticks a line of it starts with, so the block can hold fences.
fn code_fence(blocks: &[Block]) -> String {
    let longest = blocks
        .iter()
        .take_while(|b| b.kind == Kind::Monospaced)
        .map(|b| b.text.trim_start().chars().take_while(|&c| c == '`').count())
        .max()
        .unwrap_or(0);
    "`".repeat((longest + 1).max(3))
}

/// A paragraph's text with its character styles as Markdown.
fn inline_markdown(b: &Block) -> String {
    let chars: Vec<char> = b.text.chars().collect();
    let styles = tidy_styles(&chars, &b.styles);
    let mut out = String::new();
    let mut cur = Style::default();
    let mut i = 0;
    while i < chars.len() {
        let s = &styles[i];
        if s.link != cur.link {
            close_all(&mut out, &mut cur);
            if let Some(url) = &s.link {
                out.push('[');
                cur.link = Some(url.clone());
            }
        }
        if s.code {
            // A code span runs to the end of the code style (within the link).
            let mut j = i;
            while j < chars.len() && styles[j].code && styles[j].link == s.link {
                j += 1;
            }
            switch(&mut out, &mut cur, &Style { code: false, ..s.clone() });
            let text: String = chars[i..j].iter().collect();
            // Fenced by more backticks than any run inside, and padded with a
            // space (which the reader takes off again) when it holds a
            // backtick, or has spaces on both ends.
            let mut longest = 0;
            let mut run = 0;
            for ch in text.chars() {
                run = if ch == '`' { run + 1 } else { 0 };
                longest = longest.max(run);
            }
            let fence = "`".repeat(longest + 1);
            let pad = text.contains('`') || (text.starts_with(' ') && text.ends_with(' ') && !text.trim().is_empty());
            let pad = if pad { " " } else { "" };
            out.push_str(&format!("{fence}{pad}{text}{pad}{fence}"));
            i = j;
            continue;
        }
        switch(&mut out, &mut cur, s);
        escape_char(&mut out, &chars, &styles, i);
        i += 1;
    }
    close_all(&mut out, &mut cur);
    out
}

/// Bold and italic can't start or end on a space in Markdown (`** a**`
/// isn't bold), and look the same on a space anyway, so they're taken off
/// spaces at the edges of a run.
fn tidy_styles(chars: &[char], styles: &[Style]) -> Vec<Style> {
    let mut out = styles.to_vec();
    for attr in [crate::doc::Attr::Bold, crate::doc::Attr::Italic] {
        let mut i = 0;
        while i < out.len() {
            if !out[i].get(attr) {
                i += 1;
                continue;
            }
            let mut j = i;
            while j < out.len() && out[j].get(attr) {
                j += 1;
            }
            let mut a = i;
            while a < j && chars[a].is_whitespace() {
                out[a].set(attr, false);
                a += 1;
            }
            let mut b = j;
            while b > a && chars[b - 1].is_whitespace() {
                out[b - 1].set(attr, false);
                b -= 1;
            }
            i = j;
        }
    }
    out
}

/// Writes the markers that change the open character styles from `cur` to
/// `to` (links and code are handled by the caller). Markers toggle, so the
/// order they're written in doesn't matter to [`parse_inline`].
fn switch(out: &mut String, cur: &mut Style, to: &Style) {
    if cur.underline && !to.underline {
        out.push_str("</u>");
    }
    if cur.strike != to.strike {
        out.push_str("~~");
    }
    let stars = 2 * (cur.bold != to.bold) as usize + (cur.italic != to.italic) as usize;
    for _ in 0..stars {
        out.push('*');
    }
    if !cur.underline && to.underline {
        out.push_str("<u>");
    }
    cur.bold = to.bold;
    cur.italic = to.italic;
    cur.strike = to.strike;
    cur.underline = to.underline;
}

fn close_all(out: &mut String, cur: &mut Style) {
    let link = cur.link.take();
    switch(out, cur, &Style::default());
    if let Some(url) = link {
        out.push_str("](");
        for c in url.chars() {
            match c {
                ' ' => out.push_str("%20"),
                '(' => out.push_str("%28"),
                ')' => out.push_str("%29"),
                c => out.push(c),
            }
        }
        out.push(')');
    }
}

fn escape_char(out: &mut String, chars: &[char], styles: &[Style], i: usize) {
    let c = chars[i];
    // A neighbour in another style has markup (`**`, `[`, …) between it and
    // this character, so for the reader it isn't part of the same word.
    let prev = if i > 0 && styles[i - 1] == styles[i] { Some(chars[i - 1]) } else { None };
    let next = chars.get(i + 1).copied().filter(|_| styles.get(i + 1) == Some(&styles[i]));
    let needs = match c {
        '\\' | '*' | '`' | '[' | ']' | '~' => true,
        // `_` only marks emphasis at the edge of a word.
        '_' => !prev.is_some_and(|p| p.is_alphanumeric()) || !next.is_some_and(|n| n.is_alphanumeric()),
        '<' => {
            let rest: String = chars[i + 1..chars.len().min(i + 4)].iter().collect();
            rest.starts_with("u>") || rest.starts_with("/u>")
        }
        _ => false,
    };
    if needs {
        out.push('\\');
    }
    out.push(c);
}

/// What a line's start says it is.
#[derive(Debug, PartialEq)]
struct Prefix {
    kind: Kind,
    /// Bytes taken up by the marker (after any indentation).
    len: usize,
}

/// The paragraph marker at the start of `line` (indentation removed), if any.
fn parse_prefix(line: &str) -> Option<Prefix> {
    let b = line.as_bytes();
    // Headings: 1 to 6 `#`, then a space or the end of the line.
    let hashes = b.iter().take_while(|&&c| c == b'#').count();
    if (1..=6).contains(&hashes) && (b.len() == hashes || b[hashes] == b' ') {
        let kind = match hashes {
            1 => Kind::Title,
            2 => Kind::Heading,
            _ => Kind::Subheading,
        };
        return Some(Prefix { kind, len: (hashes + 1).min(b.len()) });
    }
    if let Some(&c) = b.first() {
        if matches!(c, b'-' | b'*' | b'+') && (b.len() == 1 || b[1] == b' ') {
            let after = &line[(2).min(line.len())..];
            for (mark, checked) in [("[ ]", false), ("[x]", true), ("[X]", true)] {
                if let Some(rest) = after.strip_prefix(mark) {
                    if rest.is_empty() || rest.starts_with(' ') {
                        return Some(Prefix { kind: Kind::Checklist(checked), len: (2 + 4).min(line.len()) });
                    }
                }
            }
            // A thematic break (`---`, `***`) isn't a list.
            let kind = if c == b'-' { Kind::Dashed } else { Kind::Bulleted };
            return Some(Prefix { kind, len: 2.min(b.len()) });
        }
        if c == b'>' {
            let len = if b.get(1) == Some(&b' ') { 2 } else { 1 };
            return Some(Prefix { kind: Kind::Quote, len });
        }
    }
    let digits = b.iter().take_while(|c| c.is_ascii_digit()).count();
    if (1..=9).contains(&digits) && matches!(b.get(digits), Some(b'.') | Some(b')')) {
        if b.len() == digits + 1 || b[digits + 1] == b' ' {
            return Some(Prefix { kind: Kind::Numbered, len: (digits + 2).min(b.len()) });
        }
    }
    None
}

/// The fence a line opens a code block with, if it does: three or more
/// backticks or tildes. As in CommonMark, a backtick fence line holds no
/// other backticks, so a line starting with a code span (```` ```a``b``` ````)
/// isn't one.
fn opens_fence(line: &str) -> Option<String> {
    let first = line.chars().next().filter(|&c| c == '`' || c == '~')?;
    let mark: String = line.chars().take_while(|&c| c == first).collect();
    if mark.len() < 3 || (first == '`' && line[mark.len()..].contains('`')) {
        return None;
    }
    Some(mark)
}

pub fn from_markdown(md: &str) -> Doc {
    let mut blocks = Vec::new();
    let mut fence: Option<String> = None;
    // Indentation of the list items above, to work out nesting however the
    // file is indented.
    let mut list_stack: Vec<usize> = Vec::new();
    let md = md.strip_suffix('\n').unwrap_or(md);
    for raw in md.split('\n') {
        let raw = raw.strip_suffix('\r').unwrap_or(raw);
        let expanded = raw.replace('\t', INDENT);
        let trimmed = expanded.trim_start_matches(' ');
        let spaces = expanded.len() - trimmed.len();
        if let Some(f) = &fence {
            if trimmed.starts_with(f.as_str()) && trimmed.trim_end().chars().all(|c| c == '`' || c == '~') {
                fence = None;
            } else {
                blocks.push(Block::new(Kind::Monospaced, raw));
            }
            continue;
        }
        if let Some(mark) = opens_fence(trimmed) {
            fence = Some(mark);
            list_stack.clear();
            continue;
        }
        let Some(prefix) = parse_prefix(trimmed) else {
            list_stack.clear();
            // Leading spaces in body text are kept.
            let mut b = parse_inline(raw);
            b.kind = Kind::Body;
            blocks.push(b);
            continue;
        };
        let mut indent = 0;
        if prefix.kind.is_list() || prefix.kind == Kind::Quote {
            while list_stack.last().is_some_and(|&top| top > spaces) {
                list_stack.pop();
            }
            if list_stack.last() != Some(&spaces) {
                list_stack.push(spaces);
            }
            // The app writes four spaces per level, which also holds for an
            // indented item with nothing above it to nest under.
            let nesting = (list_stack.len() - 1).max(spaces / INDENT.len());
            indent = nesting.min(crate::doc::MAX_INDENT as usize) as u8;
        } else {
            list_stack.clear();
        }
        let mut text = &trimmed[prefix.len..];
        let mut kind = prefix.kind;
        if kind == Kind::Quote {
            // `> - item`: a list inside a quote is read as the list.
            if let Some(inner) = parse_prefix(text) {
                if inner.kind.is_list() {
                    kind = inner.kind;
                    text = &text[inner.len..];
                }
            }
        }
        let mut b = parse_inline(text);
        b.kind = kind;
        b.indent = indent;
        blocks.push(b);
    }
    Doc::new(blocks)
}

/// Parses a line's inline Markdown into a body paragraph.
pub fn parse_inline(text: &str) -> Block {
    let mut b = Block::new(Kind::Body, "");
    let chars: Vec<char> = text.chars().collect();
    let mut style = Style::default();
    inline_into(&mut b, &chars, &mut style);
    b
}

fn push(b: &mut Block, c: char, s: &Style) {
    b.text.push(c);
    b.styles.push(s.clone());
}

fn inline_into(b: &mut Block, chars: &[char], style: &mut Style) {
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        let rest = || chars[i..].iter().collect::<String>();
        match c {
            '\\' if i + 1 < chars.len() && chars[i + 1].is_ascii_punctuation() => {
                push(b, chars[i + 1], style);
                i += 2;
            }
            '*' | '_' => {
                let n = chars[i..].iter().take_while(|&&x| x == c).count();
                let prev = if i > 0 { Some(chars[i - 1]) } else { None };
                let next = chars.get(i + n).copied();
                // `_` inside a word (snake_case) is just a character.
                let intraword = prev.is_some_and(|p| p.is_alphanumeric()) && next.is_some_and(|x| x.is_alphanumeric());
                // A lone `*` between spaces ("2 * 3") is just a character.
                let spaced = prev.map_or(true, char::is_whitespace) && next.map_or(true, char::is_whitespace);
                if (c == '_' && intraword) || spaced {
                    for _ in 0..n {
                        push(b, c, style);
                    }
                } else {
                    let mut left = n;
                    if left >= 2 {
                        style.bold = !style.bold;
                        left -= 2;
                    }
                    if left >= 1 {
                        style.italic = !style.italic;
                        left -= 1;
                    }
                    for _ in 0..left {
                        push(b, c, style);
                    }
                }
                i += n;
            }
            '~' if chars.get(i + 1) == Some(&'~') => {
                style.strike = !style.strike;
                i += 2;
            }
            '<' if rest().starts_with("<u>") => {
                style.underline = true;
                i += 3;
            }
            '<' if rest().starts_with("</u>") => {
                style.underline = false;
                i += 4;
            }
            '`' => {
                let n = chars[i..].iter().take_while(|&&x| x == '`').count();
                // Find the closing run of exactly n backticks.
                let mut j = i + n;
                let mut close = None;
                while j < chars.len() {
                    if chars[j] == '`' {
                        let m = chars[j..].iter().take_while(|&&x| x == '`').count();
                        if m == n {
                            close = Some(j);
                            break;
                        }
                        j += m;
                    } else {
                        j += 1;
                    }
                }
                let Some(close) = close else {
                    for _ in 0..n {
                        push(b, '`', style);
                    }
                    i += n;
                    continue;
                };
                let mut code: String = chars[i + n..close].iter().collect();
                if code.len() >= 2 && code.starts_with(' ') && code.ends_with(' ') && code.trim() != "" {
                    code = code[1..code.len() - 1].to_owned();
                }
                let cs = Style { code: true, ..style.clone() };
                for ch in code.chars() {
                    push(b, ch, &cs);
                }
                i = close + n;
            }
            '[' => {
                if let Some((text_end, url, end)) = find_link(chars, i) {
                    let saved = style.clone();
                    style.link = Some(Arc::from(url.as_str()));
                    inline_into(b, &chars[i + 1..text_end], style);
                    // Styles left open inside the link end with it.
                    *style = saved;
                    i = end;
                } else {
                    push(b, c, style);
                    i += 1;
                }
            }
            _ => {
                push(b, c, style);
                i += 1;
            }
        }
    }
}

/// For a `[` at `open`: the index of the matching `]`, the URL, and the
/// index after the closing `)` — if a whole `[text](url)` starts there.
fn find_link(chars: &[char], open: usize) -> Option<(usize, String, usize)> {
    let mut depth = 0;
    let mut i = open;
    let close = loop {
        match chars.get(i)? {
            '\\' => i += 1,
            '[' => depth += 1,
            ']' => {
                depth -= 1;
                if depth == 0 {
                    break i;
                }
            }
            _ => {}
        }
        i += 1;
    };
    if chars.get(close + 1) != Some(&'(') {
        return None;
    }
    let mut j = close + 2;
    let mut url = String::new();
    let mut parens = 0;
    loop {
        match *chars.get(j)? {
            ')' if parens == 0 => break,
            ')' => parens -= 1,
            '(' => parens += 1,
            ' ' => return None,
            _ => {}
        }
        url.push(chars[j]);
        j += 1;
    }
    let url = url.replace("%20", " ").replace("%28", "(").replace("%29", ")");
    Some((close, url, j + 1))
}

/// The note's title: its first line with text, without markup.
pub fn title_of(md: &str) -> String {
    plain_lines(md).next().unwrap_or_default()
}

/// The text after the title, for the notes list: the following lines with
/// text, without markup, joined with spaces.
pub fn snippet_of(md: &str, max_chars: usize) -> String {
    let mut s = String::new();
    for line in plain_lines(md).skip(1) {
        if !s.is_empty() {
            s.push(' ');
        }
        s.push_str(&line);
        if s.chars().count() >= max_chars {
            break;
        }
    }
    s.chars().take(max_chars).collect()
}

/// A note's lines with text, as plain text, trimmed.
fn plain_lines(md: &str) -> impl Iterator<Item = String> + '_ {
    // The open code block's fence, closed (as in `from_markdown`) only by a
    // line of at least as many of the same character.
    let mut fence: Option<String> = None;
    md.lines().filter_map(move |raw| {
        let t = raw.trim();
        match &fence {
            Some(f) if t.starts_with(f.as_str()) && t.chars().all(|c| c == '`' || c == '~') => {
                fence = None;
                return None;
            }
            None => {
                if let Some(f) = opens_fence(t) {
                    fence = Some(f);
                    return None;
                }
            }
            _ => {}
        }
        let text = if fence.is_some() {
            t.to_owned()
        } else {
            let start = raw.trim_start_matches([' ', '\t']);
            let body = parse_prefix(start).map_or(start, |p| &start[p.len..]);
            parse_inline(body).text.trim().to_owned()
        };
        (!text.is_empty()).then_some(text)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::doc::Attr;

    fn st(f: impl Fn(&mut Style)) -> Style {
        let mut s = Style::default();
        f(&mut s);
        s
    }

    fn round_trip(md: &str) {
        let doc = from_markdown(md);
        assert_eq!(to_markdown(&doc), md, "{doc:#?}");
    }

    #[test]
    fn paragraph_styles_round_trip() {
        round_trip(
            "# Groceries\n## Fruit\n### Soft\nPlain text\n\n* bullet\n- dash\n1. one\n2. two\n- [ ] eggs\n- [x] milk\n> quote\n",
        );
    }

    #[test]
    fn code_blocks_keep_their_text() {
        round_trip("Before\n```\nfn main() {\n    let x = *y; // **not bold**\n}\n```\nAfter\n");
        // A code block that itself holds a fence gets a longer one.
        let d = Doc::new(vec![Block::new(Kind::Monospaced, "```"), Block::new(Kind::Monospaced, "x")]);
        assert_eq!(to_markdown(&d), "````\n```\nx\n````\n");
        assert_eq!(from_markdown(&to_markdown(&d)), d);
        // Quotes can hold text that looks like a list.
        let d = Doc::new(vec![Block::new(Kind::Quote, "- not a list")]);
        assert_eq!(from_markdown(&to_markdown(&d)), d);
        let d = from_markdown("```\n  a\n```\n");
        assert_eq!(d.blocks.len(), 1);
        assert_eq!((d.blocks[0].kind, d.blocks[0].text.as_str()), (Kind::Monospaced, "  a"));
    }

    #[test]
    fn nested_lists() {
        round_trip("* a\n    * b\n        - [ ] c\n    * d\n* e\n");
        let d = from_markdown("- a\n  - b\n    - c\n  - d\n- e");
        let indents: Vec<u8> = d.blocks.iter().map(|b| b.indent).collect();
        assert_eq!(indents, [0, 1, 2, 1, 0]);
    }

    #[test]
    fn numbers_are_rewritten_in_order() {
        let d = from_markdown("1. a\n1. b\n7) c\n");
        assert!(d.blocks.iter().all(|b| b.kind == Kind::Numbered));
        assert_eq!(to_markdown(&d), "1. a\n2. b\n3. c\n");
    }

    #[test]
    fn character_styles_round_trip() {
        round_trip("**bold** *italic* ***both*** ~~gone~~ <u>under</u> `code` [link](https://x.y/a%20b)\n");
        round_trip("**bold *and italic* inside**\n");
        round_trip("[**bold link** text](https://example.com)\n");
    }

    #[test]
    fn character_styles_parse() {
        let b = parse_inline("a **b *c*** d");
        let bold: Vec<bool> = b.styles.iter().map(|s| s.bold).collect();
        let italic: Vec<bool> = b.styles.iter().map(|s| s.italic).collect();
        assert_eq!(b.text, "a b c d");
        assert_eq!(bold, [false, false, true, true, true, false, false]);
        assert_eq!(italic, [false, false, false, false, true, false, false]);
        let b = parse_inline("[go](http://a.b) now");
        assert_eq!(b.text, "go now");
        assert_eq!(b.styles[0].link.as_deref(), Some("http://a.b"));
        assert_eq!(b.styles[3].link, None);
    }

    #[test]
    fn overlapping_styles_survive() {
        // Bold "ab", italic "bc": the markers interleave but toggle back.
        let doc = Doc::new(vec![Block::styled(
            Kind::Body,
            &[
                ("a", st(|s| s.bold = true)),
                ("b", st(|s| {
                    s.bold = true;
                    s.italic = true
                })),
                ("c", st(|s| s.italic = true)),
                ("d", Style::default()),
            ],
        )]);
        let md = to_markdown(&doc);
        assert_eq!(from_markdown(&md), doc, "{md}");
    }

    #[test]
    fn spaces_at_the_edge_of_bold_go_outside_the_markers() {
        let doc = Doc::new(vec![Block::styled(Kind::Body, &[("x", Style::default()), (" bold ", st(|s| s.bold = true)), ("y", Style::default())])]);
        assert_eq!(to_markdown(&doc), "x **bold** y\n");
    }

    #[test]
    fn markup_characters_are_escaped() {
        for text in [
            "2 * 3 = 6",
            "a*b*c",
            "snake_case and _edge",
            "[not a link](x)",
            "back\\slash ~tilde~ `tick`",
            "<u> literally",
            "# not a heading",
            "- not a list",
            "1. not a list",
            "> not a quote",
            "```",
            "    leading spaces",
            "    - indented dash",
            "---",
            "#tag at the start",
        ] {
            let doc = Doc::new(vec![Block::new(Kind::Body, text)]);
            let md = to_markdown(&doc);
            assert_eq!(from_markdown(&md), doc, "{text:?} was written as {md:?}");
        }
    }

    #[test]
    fn code_with_backticks() {
        let doc = Doc::new(vec![Block::styled(Kind::Body, &[("a`b", st(|s| s.code = true))])]);
        assert_eq!(to_markdown(&doc), "`` a`b ``\n");
        assert_eq!(from_markdown(&to_markdown(&doc)), doc);
    }

    #[test]
    fn empty_lines_are_empty_paragraphs() {
        round_trip("a\n\n\nb\n");
        assert_eq!(to_markdown(&Doc::default()), "");
        assert_eq!(from_markdown(""), Doc::default());
    }

    #[test]
    fn markdown_from_elsewhere() {
        let d = from_markdown("#### Deep\n+ plus\n__bold__ and _it_\n\t- tabbed\n* [X] done\n> - quoted item\n---\n");
        let kinds: Vec<Kind> = d.blocks.iter().map(|b| b.kind).collect();
        assert_eq!(
            kinds,
            [
                Kind::Subheading,
                Kind::Bulleted,
                Kind::Body,
                Kind::Dashed,
                Kind::Checklist(true),
                Kind::Dashed,
                // A thematic break is just text.
                Kind::Body
            ]
        );
        assert!(d.has_attr(crate::doc::Pos::new(2, 0), crate::doc::Pos::new(2, 4), Attr::Bold));
        assert!(d.has_attr(crate::doc::Pos::new(2, 9), crate::doc::Pos::new(2, 11), Attr::Italic));
        // Four spaces (or a tab) are a level, even for the first item: the
        // app writes an indented item after body text that way.
        assert_eq!(d.blocks[3].indent, 1);
    }

    fn doc_round_trip(d: &Doc) {
        let md = to_markdown(d);
        assert_eq!(&from_markdown(&md), d, "written as {md:?}");
    }

    fn styled(text: &str, from: usize, to: usize, f: impl Fn(&mut Style)) -> Block {
        let mut b = Block::new(Kind::Body, text);
        for s in &mut b.styles[from..to] {
            f(s);
        }
        b
    }

    #[test]
    fn underscores_next_to_a_style_change_survive() {
        for b in [
            styled("snake_case", 6, 10, |s| s.bold = true),
            styled("snake_case", 0, 5, |s| s.bold = true),
            styled("snake_case", 0, 6, |s| s.italic = true),
            styled("foo_bar", 4, 7, |s| s.link = Some(Arc::from("https://example.com"))),
            styled("foo_bar", 4, 7, |s| s.code = true),
            styled("foo_bar", 0, 7, |s| s.bold = true),
        ] {
            doc_round_trip(&Doc::new(vec![b]));
        }
    }

    #[test]
    fn lines_that_are_just_a_number_survive() {
        for t in ["1.", "2024.", "3)", "12) x", "1. step", "  3.", "x 1."] {
            doc_round_trip(&Doc::new(vec![Block::new(Kind::Body, t)]));
            doc_round_trip(&Doc::new(vec![Block::new(Kind::Quote, t)]));
        }
    }

    #[test]
    fn body_text_after_a_tab_stays_body_text() {
        for t in ["\t1. step", "\t- x", "\t# x", "\t> x", "\t+ x", "\t```", " \t* y"] {
            doc_round_trip(&Doc::new(vec![Block::new(Kind::Body, t)]));
        }
    }

    #[test]
    fn indentation_survives() {
        let item = |kind, text: &str, indent| {
            let mut b = Block::new(kind, text);
            b.indent = indent;
            b
        };
        // An indented first item, a skipped level, and an indented item after
        // an empty line.
        doc_round_trip(&Doc::new(vec![Block::new(Kind::Body, "x"), item(Kind::Bulleted, "y", 1)]));
        doc_round_trip(&Doc::new(vec![item(Kind::Bulleted, "a", 0), item(Kind::Bulleted, "b", 2)]));
        doc_round_trip(&Doc::new(vec![
            item(Kind::Bulleted, "a", 0),
            item(Kind::Bulleted, "b", 1),
            Block::new(Kind::Body, ""),
            item(Kind::Bulleted, "c", 1),
        ]));
        doc_round_trip(&Doc::new(vec![item(Kind::Quote, "q", 2)]));
        // Two-space nesting from other editors still nests.
        let d = from_markdown("- a\n  - b\n    - c\n");
        assert_eq!(d.blocks.iter().map(|b| b.indent).collect::<Vec<_>>(), [0, 1, 2]);
    }

    #[test]
    fn code_spans_with_backticks_and_spaces_survive() {
        for t in ["a``b", "`x", "x`", " x ", "``", " ", "a ` b", "a```b``c"] {
            let b = styled(t, 0, t.chars().count(), |s| s.code = true);
            let mut d = Doc::new(vec![b]);
            // Text around it, too.
            let mut around = Block::new(Kind::Body, &format!("p {t} q"));
            for s in &mut around.styles[2..2 + t.chars().count()] {
                s.code = true;
            }
            d.blocks.push(around);
            doc_round_trip(&d);
        }
    }

    #[test]
    fn titles_and_snippets() {
        let md = "\n# **Trip** to Rome\n\n- [ ] book *hotel*\n```\ncode\n```\nlast";
        assert_eq!(title_of(md), "Trip to Rome");
        assert_eq!(snippet_of(md, 100), "book hotel code last");
        assert_eq!(snippet_of(md, 6), "book h");
        assert_eq!(title_of(""), "");
        // A longer fence holding a shorter one, and a line starting with a
        // code span, don't flip in and out of code.
        let md = "````\n```\ninside\n````\n```a``b``` text\nlast";
        assert_eq!(title_of(md), "```");
        assert_eq!(snippet_of(md, 100), "inside a``b text last");
    }
}
