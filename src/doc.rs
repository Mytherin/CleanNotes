//! The rich-text document the editor works on: a list of paragraphs
//! ([`Block`]s), each with a paragraph style (Title, Heading, list, …), an
//! indent level for lists, and text whose every character carries its own
//! character style (bold, italic, …).
//!
//! Notes are stored as Markdown (see [`crate::markdown`]); a `Doc` only lives
//! while a note is open. All editing operations are here, without any UI, so
//! they can be tested on their own.

use std::sync::Arc;

/// A paragraph style, as in the Aa menu of Apple Notes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Kind {
    Title,
    Heading,
    Subheading,
    #[default]
    Body,
    Monospaced,
    /// "•"
    Bulleted,
    /// "–"
    Dashed,
    /// "1."
    Numbered,
    /// A checklist item, checked or not.
    Checklist(bool),
    /// A block quote (a bar down the left).
    Quote,
}

impl Kind {
    pub fn is_list(self) -> bool {
        matches!(self, Kind::Bulleted | Kind::Dashed | Kind::Numbered | Kind::Checklist(_))
    }

    /// Whether `self` and `other` are the same style, ignoring whether a
    /// checklist item is checked.
    pub fn same(self, other: Kind) -> bool {
        match (self, other) {
            (Kind::Checklist(_), Kind::Checklist(_)) => true,
            _ => self == other,
        }
    }

    /// Paragraphs that can be indented (Apple Notes indents lists and quotes).
    pub fn indents(self) -> bool {
        self.is_list() || self == Kind::Quote
    }
}

/// A character style.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub struct Style {
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    pub strike: bool,
    /// Inline code (only from Markdown written elsewhere; Notes has no button
    /// for it).
    pub code: bool,
    pub link: Option<Arc<str>>,
}

/// A character attribute that can be switched on and off.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Attr {
    Bold,
    Italic,
    Underline,
    Strike,
}

impl Style {
    pub fn get(&self, a: Attr) -> bool {
        match a {
            Attr::Bold => self.bold,
            Attr::Italic => self.italic,
            Attr::Underline => self.underline,
            Attr::Strike => self.strike,
        }
    }

    pub fn set(&mut self, a: Attr, on: bool) {
        match a {
            Attr::Bold => self.bold = on,
            Attr::Italic => self.italic = on,
            Attr::Underline => self.underline = on,
            Attr::Strike => self.strike = on,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Block {
    pub kind: Kind,
    /// Indent level (lists and quotes only).
    pub indent: u8,
    pub text: String,
    /// One style per character of `text`.
    pub styles: Vec<Style>,
}

pub const MAX_INDENT: u8 = 8;

impl Block {
    pub fn new(kind: Kind, text: &str) -> Block {
        Block { kind, indent: 0, text: text.to_owned(), styles: vec![Style::default(); text.chars().count()] }
    }

    pub fn styled(kind: Kind, runs: &[(&str, Style)]) -> Block {
        let mut b = Block::new(kind, "");
        for (t, s) in runs {
            b.text.push_str(t);
            b.styles.extend(std::iter::repeat(s.clone()).take(t.chars().count()));
        }
        b
    }

    /// Length in characters.
    pub fn len(&self) -> usize {
        self.styles.len()
    }

    pub fn is_empty(&self) -> bool {
        self.styles.is_empty()
    }

    /// The byte offset of character `c`.
    pub fn byte(&self, c: usize) -> usize {
        self.text.char_indices().nth(c).map_or(self.text.len(), |(b, _)| b)
    }

    /// Characters `[from, to)` as a new block of the same kind.
    fn slice(&self, from: usize, to: usize) -> Block {
        Block {
            kind: self.kind,
            indent: self.indent,
            text: self.text[self.byte(from)..self.byte(to)].to_owned(),
            styles: self.styles[from..to].to_vec(),
        }
    }

    fn insert(&mut self, at: usize, text: &str, style: &Style) {
        let b = self.byte(at);
        self.text.insert_str(b, text);
        let n = text.chars().count();
        self.styles.splice(at..at, std::iter::repeat(style.clone()).take(n));
    }

    fn remove(&mut self, from: usize, to: usize) {
        let (a, b) = (self.byte(from), self.byte(to));
        self.text.replace_range(a..b, "");
        self.styles.drain(from..to);
    }

    fn append(&mut self, other: Block) {
        self.text.push_str(&other.text);
        self.styles.extend(other.styles);
    }

    /// Runs of equally styled text, as (char range, style).
    pub fn runs(&self) -> Vec<(std::ops::Range<usize>, &Style)> {
        let mut out: Vec<(std::ops::Range<usize>, &Style)> = Vec::new();
        for (i, s) in self.styles.iter().enumerate() {
            match out.last_mut() {
                Some((r, last)) if *last == s => r.end = i + 1,
                _ => out.push((i..i + 1, s)),
            }
        }
        out
    }
}

/// A place in the document: a paragraph and a character offset into it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Pos {
    pub block: usize,
    pub offset: usize,
}

impl Pos {
    pub fn new(block: usize, offset: usize) -> Pos {
        Pos { block, offset }
    }
}

/// A selection: where it started and where the caret is now. Empty when
/// both are the same.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Selection {
    pub anchor: Pos,
    pub caret: Pos,
}

impl Selection {
    pub fn caret(p: Pos) -> Selection {
        Selection { anchor: p, caret: p }
    }

    pub fn is_empty(&self) -> bool {
        self.anchor == self.caret
    }

    pub fn start(&self) -> Pos {
        self.anchor.min(self.caret)
    }

    pub fn end(&self) -> Pos {
        self.anchor.max(self.caret)
    }

    /// The paragraphs the selection touches.
    pub fn blocks(&self) -> std::ops::RangeInclusive<usize> {
        self.start().block..=self.end().block
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Doc {
    /// Never empty.
    pub blocks: Vec<Block>,
}

impl Default for Doc {
    fn default() -> Self {
        Doc { blocks: vec![Block::default()] }
    }
}

impl Doc {
    pub fn new(blocks: Vec<Block>) -> Doc {
        if blocks.is_empty() {
            Doc::default()
        } else {
            Doc { blocks }
        }
    }

    pub fn end(&self) -> Pos {
        let b = self.blocks.len() - 1;
        Pos::new(b, self.blocks[b].len())
    }

    /// `p` moved inside the document.
    pub fn clamp(&self, p: Pos) -> Pos {
        let block = p.block.min(self.blocks.len() - 1);
        Pos::new(block, p.offset.min(self.blocks[block].len()))
    }

    /// The text of the whole note, one line per paragraph.
    pub fn plain_text(&self) -> String {
        self.blocks.iter().map(|b| b.text.as_str()).collect::<Vec<_>>().join("\n")
    }

    /// The paragraphs between two positions (partial ones cut).
    pub fn fragment(&self, start: Pos, end: Pos) -> Vec<Block> {
        if start.block == end.block {
            return vec![self.blocks[start.block].slice(start.offset, end.offset)];
        }
        let mut out = vec![];
        let first = &self.blocks[start.block];
        out.push(first.slice(start.offset, first.len()));
        out.extend(self.blocks[start.block + 1..end.block].iter().cloned());
        out.push(self.blocks[end.block].slice(0, end.offset));
        out
    }

    /// Plain text between two positions, paragraphs separated by newlines.
    pub fn text_between(&self, start: Pos, end: Pos) -> String {
        self.fragment(start, end).iter().map(|b| b.text.as_str()).collect::<Vec<_>>().join("\n")
    }

    /// The style of the character before `p` (after it at the start of a
    /// paragraph): what typing at `p` continues with. Links don't continue.
    pub fn style_at(&self, p: Pos) -> Style {
        let b = &self.blocks[p.block];
        let i = if p.offset > 0 { p.offset - 1 } else { 0 };
        let mut s = b.styles.get(i).cloned().unwrap_or_default();
        if p.offset == 0 || p.offset == b.len() {
            s.link = None;
        }
        s
    }

    /// Deletes `[start, end)`, joining the first and last paragraph. The first
    /// paragraph keeps its style. Returns `start`.
    pub fn delete(&mut self, start: Pos, end: Pos) -> Pos {
        if start >= end {
            return start;
        }
        if start.block == end.block {
            self.blocks[start.block].remove(start.offset, end.offset);
        } else {
            let tail = {
                let last = &self.blocks[end.block];
                last.slice(end.offset, last.len())
            };
            let first = &mut self.blocks[start.block];
            let n = first.len();
            first.remove(start.offset, n);
            first.append(tail);
            self.blocks.drain(start.block + 1..=end.block);
        }
        start
    }

    /// Inserts text (which may hold newlines: new paragraphs of the same
    /// kind) at `p` in `style`. Returns the position after it.
    pub fn insert(&mut self, p: Pos, text: &str, style: &Style) -> Pos {
        let mut p = p;
        for (i, line) in text.split('\n').enumerate() {
            if i > 0 {
                p = self.split(p);
            }
            let line = line.strip_suffix('\r').unwrap_or(line);
            self.blocks[p.block].insert(p.offset, line, style);
            p.offset += line.chars().count();
        }
        p
    }

    /// Inserts paragraphs (from the clipboard) at `p`. The first one's text
    /// goes into the paragraph at `p`; the following ones keep their own
    /// style. Returns the position after the inserted text.
    pub fn insert_blocks(&mut self, p: Pos, blocks: Vec<Block>) -> Pos {
        let mut blocks = blocks.into_iter();
        let Some(first) = blocks.next() else { return p };
        let rest: Vec<Block> = blocks.collect();
        let target = &mut self.blocks[p.block];
        // A paste into an empty paragraph takes the pasted paragraph's style.
        if target.is_empty() && !rest.is_empty() {
            target.kind = first.kind;
            target.indent = first.indent;
        }
        let tail = target.slice(p.offset, target.len());
        let n = target.len();
        target.remove(p.offset, n);
        let first_len = first.len();
        target.append(Block { kind: target.kind, indent: target.indent, ..first });
        if rest.is_empty() {
            target.append(tail);
            return Pos::new(p.block, p.offset + first_len);
        }
        let count = rest.len();
        let at = p.block + 1;
        self.blocks.splice(at..at, rest);
        let last = at + count - 1;
        let end = Pos::new(last, self.blocks[last].len());
        self.blocks[last].append(tail);
        end
    }

    /// Splits the paragraph at `p` (Return). The new paragraph continues a
    /// list or quote (an unchecked item after a checklist item); after a
    /// heading it's body text. Returns the start of the new paragraph.
    pub fn split(&mut self, p: Pos) -> Pos {
        let b = &mut self.blocks[p.block];
        let mut rest = b.slice(p.offset, b.len());
        let n = b.len();
        b.remove(p.offset, n);
        rest.kind = match b.kind {
            Kind::Title | Kind::Heading | Kind::Subheading if !rest.is_empty() || p.offset == 0 => b.kind,
            Kind::Title | Kind::Heading | Kind::Subheading => Kind::Body,
            Kind::Checklist(checked) if p.offset == 0 => Kind::Checklist(checked),
            Kind::Checklist(_) => Kind::Checklist(false),
            k => k,
        };
        if p.offset == 0 && matches!(b.kind, Kind::Title | Kind::Heading | Kind::Subheading) {
            // Return at the start of a heading pushes it down and leaves an
            // empty body line above.
            b.kind = Kind::Body;
        }
        self.blocks.insert(p.block + 1, rest);
        Pos::new(p.block + 1, 0)
    }

    /// Return as typed: on an empty list item or quote it ends the list (the
    /// item becomes body text, or moves out a level), otherwise it splits.
    pub fn enter(&mut self, p: Pos) -> Pos {
        let b = &mut self.blocks[p.block];
        if b.is_empty() && (b.kind.is_list() || b.kind == Kind::Quote) {
            if b.indent > 0 {
                b.indent -= 1;
            } else {
                b.kind = Kind::Body;
            }
            return p;
        }
        self.split(p)
    }

    /// Backspace at the start of a paragraph: an indented one moves out a
    /// level, a list item or quote becomes body text, and body text joins the
    /// paragraph above. Returns the new caret.
    pub fn backspace_at_start(&mut self, block: usize) -> Pos {
        let b = &mut self.blocks[block];
        if b.indent > 0 {
            b.indent -= 1;
            return Pos::new(block, 0);
        }
        if b.kind.is_list() || b.kind == Kind::Quote {
            b.kind = Kind::Body;
            return Pos::new(block, 0);
        }
        if block == 0 {
            return Pos::new(0, 0);
        }
        let prev_len = self.blocks[block - 1].len();
        let end = Pos::new(block - 1, prev_len);
        // Joining onto an empty paragraph keeps the lower paragraph's style.
        if prev_len == 0 {
            self.blocks.remove(block - 1);
            return Pos::new(block - 1, 0);
        }
        self.delete(end, Pos::new(block, 0))
    }

    /// Sets the paragraph style of the given paragraphs. Choosing a list or
    /// quote style they all already have turns them back into body text, as
    /// the Aa menu and its shortcuts do.
    pub fn set_kind(&mut self, blocks: std::ops::RangeInclusive<usize>, kind: Kind) {
        let all = self.blocks[blocks.clone()].iter().all(|b| b.kind.same(kind));
        let toggle_off = all && (kind.is_list() || kind == Kind::Quote || kind == Kind::Monospaced);
        for b in &mut self.blocks[blocks] {
            if toggle_off {
                b.kind = Kind::Body;
                b.indent = 0;
            } else if !b.kind.same(kind) {
                b.kind = kind;
                if !kind.indents() {
                    b.indent = 0;
                }
            }
        }
    }

    /// Checks or unchecks checklist items: all checked if any wasn't.
    pub fn toggle_checked(&mut self, blocks: std::ops::RangeInclusive<usize>) {
        let items: Vec<usize> = blocks.filter(|&i| matches!(self.blocks[i].kind, Kind::Checklist(_))).collect();
        let all = items.iter().all(|&i| self.blocks[i].kind == Kind::Checklist(true));
        for i in items {
            self.blocks[i].kind = Kind::Checklist(!all);
        }
    }

    /// Whether every character in `[start, end)` has `attr`.
    pub fn has_attr(&self, start: Pos, end: Pos, attr: Attr) -> bool {
        let mut any = false;
        for (i, b) in self.blocks.iter().enumerate().take(end.block + 1).skip(start.block) {
            let from = if i == start.block { start.offset } else { 0 };
            let to = if i == end.block { end.offset } else { b.len() };
            for s in &b.styles[from..to] {
                if !s.get(attr) {
                    return false;
                }
                any = true;
            }
        }
        any
    }

    /// Changes the style of every character in `[start, end)`.
    pub fn restyle(&mut self, start: Pos, end: Pos, mut f: impl FnMut(&mut Style)) {
        for i in start.block..=end.block {
            let b = &mut self.blocks[i];
            let from = if i == start.block { start.offset } else { 0 };
            let to = if i == end.block { end.offset } else { b.len() };
            for s in &mut b.styles[from..to] {
                f(s);
            }
        }
    }

    /// Bold, italic, … on or off for `[start, end)`: on unless it's all on.
    pub fn toggle_attr(&mut self, start: Pos, end: Pos, attr: Attr) {
        let on = !self.has_attr(start, end, attr);
        self.restyle(start, end, |s| s.set(attr, on));
    }

    /// Indents (`delta` 1) or outdents (-1) the paragraphs that can be.
    pub fn indent(&mut self, blocks: std::ops::RangeInclusive<usize>, delta: i32) {
        for b in &mut self.blocks[blocks] {
            if b.kind.indents() {
                b.indent = (b.indent as i32 + delta).clamp(0, MAX_INDENT as i32) as u8;
            }
        }
    }

    /// Moves paragraphs up (`-1`) or down (`1`) past their neighbour. Returns
    /// whether they moved.
    pub fn move_blocks(&mut self, blocks: std::ops::RangeInclusive<usize>, delta: i32) -> bool {
        let (a, b) = (*blocks.start(), *blocks.end());
        if delta < 0 && a > 0 {
            let above = self.blocks.remove(a - 1);
            self.blocks.insert(b, above);
            true
        } else if delta > 0 && b + 1 < self.blocks.len() {
            let below = self.blocks.remove(b + 1);
            self.blocks.insert(a, below);
            true
        } else {
            false
        }
    }

    /// The number shown before each numbered list item (0 for others).
    /// Numbering restarts after anything that isn't a deeper-indented
    /// paragraph.
    pub fn list_numbers(&self) -> Vec<u32> {
        let mut out = vec![0; self.blocks.len()];
        let mut counters: Vec<u32> = vec![0; MAX_INDENT as usize + 1];
        for (i, b) in self.blocks.iter().enumerate() {
            let level = b.indent as usize;
            // Deeper levels restart below any shallower paragraph.
            for c in &mut counters[level + 1..] {
                *c = 0;
            }
            if b.kind == Kind::Numbered {
                counters[level] += 1;
                out[i] = counters[level];
            } else if !(b.kind.is_list() && level > 0) {
                counters[level] = 0;
            }
        }
        out
    }

    /// Word boundaries around offset `c` in a paragraph (for double-click).
    pub fn word_at(&self, p: Pos) -> (Pos, Pos) {
        let chars: Vec<char> = self.blocks[p.block].text.chars().collect();
        let is_word = |c: char| c.is_alphanumeric() || c == '_' || c == '\'';
        let at = p.offset.min(chars.len());
        let probe = if at < chars.len() && is_word(chars[at]) {
            Some(at)
        } else if at > 0 && is_word(chars[at - 1]) {
            Some(at - 1)
        } else {
            None
        };
        let Some(i) = probe else {
            let end = (at + 1).min(chars.len());
            return (Pos::new(p.block, at), Pos::new(p.block, end));
        };
        let mut s = i;
        while s > 0 && is_word(chars[s - 1]) {
            s -= 1;
        }
        let mut e = i;
        while e < chars.len() && is_word(chars[e]) {
            e += 1;
        }
        (Pos::new(p.block, s), Pos::new(p.block, e))
    }

    /// The position one word to the left (`-1`) or right (`1`) of `p`.
    pub fn word_step(&self, p: Pos, dir: i32) -> Pos {
        let chars: Vec<char> = self.blocks[p.block].text.chars().collect();
        let is_word = |c: char| c.is_alphanumeric() || c == '_';
        if dir < 0 {
            if p.offset == 0 {
                return if p.block > 0 { Pos::new(p.block - 1, self.blocks[p.block - 1].len()) } else { p };
            }
            let mut i = p.offset;
            while i > 0 && !is_word(chars[i - 1]) {
                i -= 1;
            }
            while i > 0 && is_word(chars[i - 1]) {
                i -= 1;
            }
            Pos::new(p.block, i)
        } else {
            if p.offset >= chars.len() {
                return if p.block + 1 < self.blocks.len() { Pos::new(p.block + 1, 0) } else { p };
            }
            let mut i = p.offset;
            while i < chars.len() && !is_word(chars[i]) {
                i += 1;
            }
            while i < chars.len() && is_word(chars[i]) {
                i += 1;
            }
            Pos::new(p.block, i)
        }
    }

    /// One character left (`-1`) or right (`1`), across paragraphs.
    pub fn char_step(&self, p: Pos, dir: i32) -> Pos {
        if dir < 0 {
            if p.offset > 0 {
                Pos::new(p.block, p.offset - 1)
            } else if p.block > 0 {
                Pos::new(p.block - 1, self.blocks[p.block - 1].len())
            } else {
                p
            }
        } else if p.offset < self.blocks[p.block].len() {
            Pos::new(p.block, p.offset + 1)
        } else if p.block + 1 < self.blocks.len() {
            Pos::new(p.block + 1, 0)
        } else {
            p
        }
    }

    /// Hashtags in the note (`#word`, without the `#`), lowercased, in order
    /// of first appearance.
    pub fn tags(&self) -> Vec<String> {
        let mut out = Vec::new();
        for b in &self.blocks {
            if b.kind == Kind::Monospaced {
                continue;
            }
            for t in tags_in(&b.text) {
                if !out.contains(&t) {
                    out.push(t);
                }
            }
        }
        out
    }
}

/// Hashtags in a line of text: `#` at the start of a word, followed by
/// letters, digits, `-`, `_` or `/`, at least one of them a non-digit.
pub fn tags_in(text: &str) -> Vec<String> {
    let chars: Vec<char> = text.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '#' && (i == 0 || chars[i - 1].is_whitespace() || chars[i - 1] == '(') {
            let mut j = i + 1;
            while j < chars.len() && (chars[j].is_alphanumeric() || matches!(chars[j], '-' | '_' | '/')) {
                j += 1;
            }
            let tag: String = chars[i + 1..j].iter().collect();
            let tag = tag.trim_end_matches(['-', '/']).to_lowercase();
            if !tag.is_empty() && tag.chars().any(|c| !c.is_ascii_digit()) {
                out.push(tag);
            }
            i = j;
        } else {
            i += 1;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn doc(lines: &[(Kind, &str)]) -> Doc {
        Doc::new(lines.iter().map(|(k, t)| Block::new(*k, t)).collect())
    }

    fn texts(d: &Doc) -> Vec<(Kind, String)> {
        d.blocks.iter().map(|b| (b.kind, b.text.clone())).collect()
    }

    fn bold() -> Style {
        Style { bold: true, ..Style::default() }
    }

    #[test]
    fn typing_and_deleting_across_paragraphs() {
        let mut d = doc(&[(Kind::Title, "Shopping"), (Kind::Body, "milk")]);
        let p = d.insert(Pos::new(1, 4), " and eggs\nbread", &Style::default());
        assert_eq!(p, Pos::new(2, 5));
        assert_eq!(d.plain_text(), "Shopping\nmilk and eggs\nbread");
        d.delete(Pos::new(0, 4), Pos::new(2, 0));
        assert_eq!(texts(&d), vec![(Kind::Title, "Shopbread".into())]);
        assert_eq!(d.blocks[0].styles.len(), 9);
    }

    #[test]
    fn return_continues_lists_and_ends_them_when_empty() {
        let mut d = doc(&[(Kind::Checklist(true), "eggs")]);
        let p = d.enter(Pos::new(0, 4));
        assert_eq!(d.blocks[1].kind, Kind::Checklist(false), "a new item starts unchecked");
        let p = d.enter(p);
        assert_eq!(p, Pos::new(1, 0));
        assert_eq!(texts(&d), vec![(Kind::Checklist(true), "eggs".into()), (Kind::Body, "".into())]);
    }

    #[test]
    fn return_on_an_indented_empty_item_moves_it_out() {
        let mut d = doc(&[(Kind::Bulleted, "a"), (Kind::Bulleted, "")]);
        d.blocks[1].indent = 1;
        d.enter(Pos::new(1, 0));
        assert_eq!((d.blocks[1].kind, d.blocks[1].indent), (Kind::Bulleted, 0));
    }

    #[test]
    fn return_after_a_heading_gives_body_text() {
        let mut d = doc(&[(Kind::Title, "Trip")]);
        let p = d.enter(Pos::new(0, 4));
        assert_eq!(d.blocks[p.block].kind, Kind::Body);
        // In the middle the rest stays a heading.
        let mut d = doc(&[(Kind::Heading, "Trip plan")]);
        d.enter(Pos::new(0, 4));
        assert_eq!(texts(&d), vec![(Kind::Heading, "Trip".into()), (Kind::Heading, " plan".into())]);
    }

    #[test]
    fn backspace_at_the_start_removes_the_bullet_then_joins() {
        let mut d = doc(&[(Kind::Body, "one"), (Kind::Bulleted, "two")]);
        d.blocks[1].indent = 1;
        assert_eq!(d.backspace_at_start(1), Pos::new(1, 0));
        assert_eq!(d.blocks[1].indent, 0);
        d.backspace_at_start(1);
        assert_eq!(d.blocks[1].kind, Kind::Body);
        assert_eq!(d.backspace_at_start(1), Pos::new(0, 3));
        assert_eq!(texts(&d), vec![(Kind::Body, "onetwo".into())]);
    }

    #[test]
    fn list_styles_toggle_off() {
        let mut d = doc(&[(Kind::Body, "a"), (Kind::Body, "b")]);
        d.set_kind(0..=1, Kind::Checklist(false));
        assert!(d.blocks.iter().all(|b| b.kind == Kind::Checklist(false)));
        d.blocks[0].kind = Kind::Checklist(true);
        d.set_kind(0..=1, Kind::Checklist(false));
        assert!(d.blocks.iter().all(|b| b.kind == Kind::Body));
        // Headings don't toggle.
        d.set_kind(0..=0, Kind::Title);
        d.set_kind(0..=0, Kind::Title);
        assert_eq!(d.blocks[0].kind, Kind::Title);
    }

    #[test]
    fn bold_toggles_on_unless_all_bold() {
        let mut d = Doc::new(vec![Block::styled(Kind::Body, &[("ab", bold()), ("cd", Style::default())])]);
        d.toggle_attr(Pos::new(0, 0), Pos::new(0, 4), Attr::Bold);
        assert!(d.has_attr(Pos::new(0, 0), Pos::new(0, 4), Attr::Bold));
        d.toggle_attr(Pos::new(0, 1), Pos::new(0, 3), Attr::Bold);
        let bolds: Vec<bool> = d.blocks[0].styles.iter().map(|s| s.bold).collect();
        assert_eq!(bolds, [true, false, false, true]);
    }

    #[test]
    fn typing_continues_the_style_before_the_caret() {
        let d = Doc::new(vec![Block::styled(Kind::Body, &[("ab", bold()), ("cd", Style::default())])]);
        assert!(d.style_at(Pos::new(0, 2)).bold);
        assert!(!d.style_at(Pos::new(0, 3)).bold);
        assert!(d.style_at(Pos::new(0, 0)).bold);
    }

    #[test]
    fn pasting_paragraphs() {
        let mut d = doc(&[(Kind::Body, "start end")]);
        let frag = vec![Block::new(Kind::Body, "one"), Block::new(Kind::Checklist(false), "two")];
        let p = d.insert_blocks(Pos::new(0, 6), frag);
        assert_eq!(p, Pos::new(1, 3));
        assert_eq!(
            texts(&d),
            vec![(Kind::Body, "start one".into()), (Kind::Checklist(false), "twoend".into())]
        );
    }

    #[test]
    fn numbering_restarts_after_other_paragraphs() {
        let mut d = doc(&[
            (Kind::Numbered, "a"),
            (Kind::Numbered, "b"),
            (Kind::Numbered, "b1"),
            (Kind::Numbered, "c"),
            (Kind::Body, "x"),
            (Kind::Numbered, "a"),
        ]);
        d.blocks[2].indent = 1;
        assert_eq!(d.list_numbers(), vec![1, 2, 1, 3, 0, 1]);
    }

    #[test]
    fn moving_list_items() {
        let mut d = doc(&[(Kind::Bulleted, "a"), (Kind::Bulleted, "b"), (Kind::Bulleted, "c")]);
        assert!(d.move_blocks(2..=2, -1));
        assert_eq!(d.plain_text(), "a\nc\nb");
        assert!(!d.move_blocks(0..=0, -1));
        assert!(d.move_blocks(0..=1, 1));
        assert_eq!(d.plain_text(), "b\na\nc");
    }

    #[test]
    fn words() {
        let d = doc(&[(Kind::Body, "hello, big world")]);
        assert_eq!(d.word_at(Pos::new(0, 8)), (Pos::new(0, 7), Pos::new(0, 10)));
        assert_eq!(d.word_step(Pos::new(0, 16), -1), Pos::new(0, 11));
        assert_eq!(d.word_step(Pos::new(0, 0), 1), Pos::new(0, 5));
        assert_eq!(d.word_step(Pos::new(0, 5), 1), Pos::new(0, 10));
    }

    #[test]
    fn hashtags() {
        assert_eq!(tags_in("#Work notes about #home-office and #1 or a#b (#x)"), vec!["work", "home-office", "x"]);
        let d = doc(&[(Kind::Body, "#a #b"), (Kind::Monospaced, "#code"), (Kind::Body, "#A")]);
        assert_eq!(d.tags(), vec!["a", "b"]);
    }
}
