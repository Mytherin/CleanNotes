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
//! | Image (an attachment) | `![](attachments/<sha256>.<ext>)` |
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
        if let Kind::Cell { cols, header_row, header_col } = b.kind {
            // A table, written whole at its first cell.
            if i == 0 || !doc.blocks[i - 1].kind.is_cell() {
                let end = doc.blocks[i..].iter().position(|x| !x.kind.is_cell()).map_or(doc.blocks.len(), |n| i + n);
                table_markdown(&doc.blocks[i..end], cols as usize, header_row, header_col, &mut out);
            }
            continue;
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
            Kind::Cell { .. } => String::new(),
        };
        out.push_str(&prefix);
        let inline = inline_markdown(b);
        // Text that would read as a heading, list, quote or code fence gets
        // its first character escaped.
        // (The reader takes a tab for four spaces.)
        let rest = inline.trim_start_matches([' ', '\t']);
        let lead = inline.len() - rest.len();
        let prefix = parse_prefix(rest);
        // (A line starting with `|` could start a table.)
        let reads_as_markup = prefix.is_some() || opens_fence(rest).is_some() || rest.starts_with('|');
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
        if let Some(name) = &s.image {
            // An image stands on its own, outside any other style.
            close_all(&mut out, &mut cur);
            out.push_str(&crate::attachments::markdown(name));
            i += 1;
            continue;
        }
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
        // As written, unless it has spaces, angle brackets or unbalanced
        // parentheses: then in CommonMark's `<…>` form, with `\` escapes.
        // (No percent-encoding, which a URL holding "%20" wouldn't survive.)
        let mut depth = 0i32;
        let mut balanced = true;
        for c in url.chars() {
            depth += match c {
                '(' => 1,
                ')' => -1,
                _ => 0,
            };
            balanced &= depth >= 0;
        }
        balanced &= depth == 0;
        if balanced && !url.contains([' ', '<', '>']) && !url.starts_with('<') {
            out.push_str(&url);
        } else {
            out.push('<');
            for c in url.chars() {
                if matches!(c, '<' | '>' | '\\') {
                    out.push('\\');
                }
                out.push(c);
            }
            out.push('>');
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
        // `!` right before a link to an attachment would make it an image.
        '!' => styles.get(i + 1).is_some_and(|n| {
            n.link != styles[i].link
                && n.link.as_deref().and_then(|u| u.strip_prefix(crate::attachments::URL_PREFIX)).is_some_and(crate::attachments::is_name)
        }),
        // `_` only marks emphasis at the edge of a word.
        '_' => !prev.is_some_and(|p| p.is_alphanumeric()) || !next.is_some_and(|n| n.is_alphanumeric()),
        '<' => {
            let rest: String = chars[i + 1..chars.len().min(i + 6)].iter().collect::<String>().to_ascii_lowercase();
            // (`<br>` is a line break in a table cell.)
            rest.starts_with("u>") || rest.starts_with("/u>") || rest.starts_with("br>") || rest.starts_with("br/>") || rest.starts_with("br />")
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
    let lines: Vec<&str> = md.split('\n').map(|l| l.strip_suffix('\r').unwrap_or(l)).collect();
    let mut li = 0;
    while li < lines.len() {
        let raw = lines[li];
        li += 1;
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
        // A table: a row of cells, then a separator row.
        if trimmed.starts_with('|') && lines.get(li).is_some_and(|l| is_separator(l)) {
            let header = split_row(trimmed);
            let separator = split_row(lines[li]);
            li += 1;
            let mut rows = Vec::new();
            while li < lines.len() && lines[li].trim_start().starts_with('|') {
                rows.push(split_row(lines[li].trim_start()));
                li += 1;
            }
            let header_row = header.iter().any(|c| !c.trim().is_empty());
            let header_col = separator.first().is_some_and(|s| s.starts_with(':') && !s.ends_with(':'));
            if header_row {
                rows.insert(0, header);
            }
            let cols = rows.iter().map(Vec::len).max().unwrap_or(1).max(header_col as usize).max(1);
            let kind = Kind::Cell { cols: cols as u16, header_row, header_col };
            if blocks.last().is_some_and(|b: &Block| b.kind.is_cell()) {
                blocks.push(Block::default());
            }
            for row in &rows {
                for c in 0..cols {
                    let mut b = row.get(c).map_or_else(|| Block::new(kind, ""), |t| cell_from_markdown(t));
                    b.kind = kind;
                    blocks.push(b);
                }
            }
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

/// Whether a line is a table's separator row (`| --- | :---: |`).
fn is_separator(line: &str) -> bool {
    let line = line.trim();
    if !line.contains('-') || !line.chars().all(|c| matches!(c, '|' | '-' | ':' | ' ' | '\t')) {
        return false;
    }
    let cells = split_row(line);
    !cells.is_empty() && cells.iter().all(|c| {
        let c = c.trim().trim_start_matches(':').trim_end_matches(':');
        !c.is_empty() && c.chars().all(|x| x == '-')
    })
}

/// A table row's cells (their Markdown, trimmed), split at unescaped `|`.
fn split_row(line: &str) -> Vec<String> {
    let line = line.trim();
    let line = line.strip_prefix('|').unwrap_or(line);
    let mut cells = Vec::new();
    let mut cur = String::new();
    let mut backslashes = 0;
    for c in line.chars() {
        if c == '|' && backslashes % 2 == 0 {
            cells.push(std::mem::take(&mut cur));
        } else {
            cur.push(c);
        }
        backslashes = if c == '\\' { backslashes + 1 } else { 0 };
    }
    // Text after the last `|` is a cell only if there is any.
    if !cur.trim().is_empty() {
        cells.push(cur);
    }
    cells.into_iter().map(|c| c.trim().to_owned()).collect()
}

/// A cell's paragraph from its Markdown: `<br>` are line breaks in it.
fn cell_from_markdown(md: &str) -> Block {
    let mut b = Block::new(Kind::Body, "");
    let lower = md.to_ascii_lowercase();
    // An unescaped `<br>` (`\<br>` is the text).
    let unescaped = |i: usize| md[..i].bytes().rev().take_while(|&c| c == b'\\').count() % 2 == 0;
    let find_br = |from: usize| -> Option<(usize, usize)> {
        let mut best: Option<(usize, usize)> = None;
        for t in ["<br>", "<br/>", "<br />"] {
            let mut s = from;
            while let Some(i) = lower[s..].find(t).map(|i| s + i) {
                if unescaped(i) {
                    if best.is_none_or(|(bi, _)| i < bi) {
                        best = Some((i, t.len()));
                    }
                    break;
                }
                s = i + 1;
            }
        }
        best
    };
    let mut at = 0;
    let mut first = true;
    loop {
        let next = find_br(at);
        let piece = &md[at..next.map_or(md.len(), |(i, _)| i)];
        if !first {
            push(&mut b, '\n', &Style::default());
        }
        first = false;
        let p = parse_inline(piece);
        b.text.push_str(&p.text);
        b.styles.extend(p.styles);
        match next {
            Some((i, n)) => at = i + n,
            None => break,
        }
    }
    b
}

/// A cell's Markdown: its inline Markdown with `|` escaped and line breaks
/// as `<br>`, on one line.
fn cell_markdown(b: &Block) -> String {
    inline_markdown(b).replace('|', "\\|").replace('\n', "<br>")
}

/// A table (its cells, row by row) as Markdown. A table without a header
/// row gets an empty one (Markdown tables must have one); a header column
/// is marked by the first column's `:---`.
fn table_markdown(cells: &[Block], cols: usize, header_row: bool, header_col: bool, out: &mut String) {
    let cols = cols.max(1);
    let row_line = |row: &[Block]| {
        let mut s = String::from("|");
        for c in 0..cols {
            s.push(' ');
            if let Some(b) = row.get(c) {
                s.push_str(&cell_markdown(b));
            }
            s.push_str(" |");
        }
        s
    };
    let rows: Vec<&[Block]> = cells.chunks(cols).collect();
    let body = if header_row && !rows.is_empty() {
        out.push_str(&row_line(rows[0]));
        &rows[1..]
    } else {
        out.push('|');
        out.push_str(&" |".repeat(cols));
        &rows[..]
    };
    out.push('\n');
    out.push('|');
    for c in 0..cols {
        out.push_str(if c == 0 && header_col { " :--- |" } else { " --- |" });
    }
    out.push('\n');
    for r in body {
        out.push_str(&row_line(r));
        out.push('\n');
    }
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
            // An attached image: `![…](attachments/<name>)`. (Other images
            // are kept as written: a `!` and a link.)
            '!' if chars.get(i + 1) == Some(&'[') && image_at(chars, i).is_some() => {
                let (name, end) = image_at(chars, i).unwrap();
                push(b, crate::attachments::OBJECT, &Style { image: Some(Arc::from(name.as_str())), ..Style::default() });
                i = end;
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

/// For a `!` at `bang` that starts `![…](attachments/<name>)`: the name and
/// the index after it.
fn image_at(chars: &[char], bang: usize) -> Option<(String, usize)> {
    let (_, url, end) = find_link(chars, bang + 1)?;
    let name = url.strip_prefix(crate::attachments::URL_PREFIX)?;
    crate::attachments::is_name(name).then(|| (name.to_owned(), end))
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
    if chars.get(j) == Some(&'<') {
        // `<…>`: anything but a line break, with `\` escapes.
        j += 1;
        loop {
            match *chars.get(j)? {
                '>' => break,
                '\\' if chars.get(j + 1).is_some_and(|c| c.is_ascii_punctuation()) => {
                    url.push(chars[j + 1]);
                    j += 2;
                    continue;
                }
                '<' => return None,
                c => url.push(c),
            }
            j += 1;
        }
        return (chars.get(j + 1) == Some(&')')).then_some((close, url, j + 2));
    }
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
    // Kept as written: a URL may well hold "%20" itself.
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

/// The snippet for a search result: the first line after the title with a
/// match of one of `words`, from a little before the match ("…" where it's
/// cut); the usual snippet if only the title matches.
pub fn search_snippet_of(md: &str, words: &[&str], max_chars: usize) -> String {
    const BEFORE: usize = 24;
    for line in plain_lines(md).skip(1) {
        if let Some(first) = crate::doc::match_ranges(&line, words).first() {
            let chars: Vec<char> = line.chars().collect();
            let start = first.start.saturating_sub(BEFORE);
            // Cut at a word's start.
            let start = if start == 0 {
                0
            } else {
                (start..first.start).find(|&i| chars[i - 1].is_whitespace()).unwrap_or(start)
            };
            let mut s: String = chars[start..].iter().take(max_chars).collect();
            if start > 0 {
                s.insert(0, '…');
            }
            return s;
        }
    }
    snippet_of(md, max_chars)
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
        } else if t.starts_with('|') {
            // A table row: its cells' text (the separator row: nothing).
            if is_separator(t) {
                return None;
            }
            split_row(t).iter().map(|c| cell_from_markdown(c).text.replace('\n', " ")).filter(|c| !c.trim().is_empty()).collect::<Vec<_>>().join("  ")
        } else {
            let start = raw.trim_start_matches([' ', '\t']);
            let body = parse_prefix(start).map_or(start, |p| &start[p.len..]);
            // (Images aren't text.)
            parse_inline(body).text.replace(crate::attachments::OBJECT, " ").trim().to_owned()
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
    fn link_urls_survive_exactly() {
        for url in [
            "https://en.wikipedia.org/wiki/Foo_%28bar%29",
            "https://x.y/a%20b",
            "https://en.wikipedia.org/wiki/Foo_(bar)",
            "file:///C:/My Notes/a b.txt",
            "https://x.y/unbalanced)(",
            "https://x.y/<odd>\\path",
            "",
        ] {
            let b = Block::styled(Kind::Body, &[("link", st(|s| s.link = Some(Arc::from(url))))]);
            doc_round_trip(&Doc::new(vec![b]));
        }
        // Spaces are written in the <…> form.
        let b = Block::styled(Kind::Body, &[("l", st(|s| s.link = Some(Arc::from("a b"))))]);
        assert_eq!(to_markdown(&Doc::new(vec![b])), "[l](<a b>)\n");
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
    fn tables_round_trip() {
        // Apple Notes-style: no header row (an empty one is written).
        round_trip("Before\n| | |\n| --- | --- |\n| a | **b** |\n| c\\|d | e<br>f |\n\nAfter\n");
        let d = from_markdown("| | |\n| --- | --- |\n| a | **b** |\n| c\\|d | e<br>f |\n");
        let cells: Vec<&str> = d.blocks.iter().filter(|b| b.kind.is_cell()).map(|b| b.text.as_str()).collect();
        assert_eq!(cells, ["a", "b", "c|d", "e\nf"]);
        assert_eq!(d.blocks[0].kind, Kind::Cell { cols: 2, header_row: false, header_col: false });
        assert!(d.blocks[1].styles[0].bold);
        // There's always a paragraph after a table to write in.
        assert_eq!(d.blocks.last().unwrap().kind, Kind::Body);
        // Header row and header column (as Super makes them).
        round_trip("| Name | Qty |\n| :--- | --- |\n| eggs | 6 |\n");
        let d = from_markdown("| Name | Qty |\n| :--- | --- |\n| eggs | 6 |\n");
        assert_eq!(d.blocks[0].kind, Kind::Cell { cols: 2, header_row: true, header_col: true });
        // Tables from elsewhere: ragged rows, no outer pipes on the right.
        let d = from_markdown("| a | b | c\n|---|---|---|\n| 1 | 2\n");
        let cells: Vec<&str> = d.blocks.iter().filter(|b| b.kind.is_cell()).map(|b| b.text.as_str()).collect();
        assert_eq!(cells, ["a", "b", "c", "1", "2", ""]);
        // Text that only looks like a table stays text.
        doc_round_trip(&Doc::new(vec![Block::new(Kind::Body, "| not | a table |"), Block::new(Kind::Body, "|---|---|")]));
        doc_round_trip(&Doc::new(vec![Block::new(Kind::Body, "a literal <br> here")]));
        let mut cell = Block::new(Kind::cell(1), "typed <br> and | pipes");
        cell.kind = Kind::cell(1);
        doc_round_trip(&Doc::new(vec![cell, Block::default()]));
        // Titles and snippets show the cells' text.
        assert_eq!(title_of("| Item | Price |\n| --- | --- |\n| Tea | 3 |\n"), "Item  Price");
        assert_eq!(snippet_of("| Item | Price |\n| --- | --- |\n| Tea | 3 |\n", 50), "Tea  3");
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
    fn images_round_trip() {
        let a = crate::attachments::name_for(b"a", "png");
        let md = format!("# Trip\n![](attachments/{a})\nBefore **bold ![](attachments/{a}) still** after\n- ![](attachments/{a})\n");
        let d = from_markdown(&md);
        let o = crate::attachments::OBJECT;
        assert_eq!(d.blocks[1].text, o.to_string());
        assert_eq!(d.blocks[1].styles[0].image.as_deref(), Some(a.as_str()));
        assert_eq!(d.blocks[2].text, format!("Before bold {o} still after"));
        assert_eq!(d.blocks[3].kind, Kind::Dashed);
        // (Bold comes off the spaces next to the image.)
        round_trip(&format!("# Trip\n![](attachments/{a})\nBefore **bold** ![](attachments/{a}) **still** after\n- ![](attachments/{a})\n"));
        // Images that aren't attachments stay as written.
        round_trip("![cat](https://example.com/cat.png)\n");
        round_trip("![x](attachments/not-a-hash.png)\n");
        // A "!" before a link isn't an image.
        let b = Block::styled(Kind::Body, &[("!", Style::default()), ("x", st(|s| s.link = Some(Arc::from(format!("attachments/{a}").as_str()))))]);
        doc_round_trip(&Doc::new(vec![b]));
        // Titles and snippets leave images out.
        assert_eq!(title_of(&format!("![](attachments/{a})\n# Trip\n![](attachments/{a}) photo\n")), "Trip");
        assert_eq!(snippet_of(&format!("# Trip\n![](attachments/{a}) photo\n"), 50), "photo");
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
