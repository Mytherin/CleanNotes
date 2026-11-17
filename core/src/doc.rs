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
    /// A table cell. A table is a run of cells, row by row; every cell says
    /// how many columns its table has and whether its first row and first
    /// column are headers. A cell's text may hold line breaks.
    Cell { cols: u16, header_row: bool, header_col: bool },
}

impl Kind {
    pub fn is_list(self) -> bool {
        matches!(self, Kind::Bulleted | Kind::Dashed | Kind::Numbered | Kind::Checklist(_))
    }

    pub fn is_cell(self) -> bool {
        matches!(self, Kind::Cell { .. })
    }

    /// What a list item shows before its text, as text: "•", "–", "3.",
    /// "☐" or "☑" (`number` is a numbered item's); empty for other kinds.
    /// The apps draw a checklist's circle themselves; this is its text form
    /// (sharing, printing).
    pub fn list_marker(self, number: u32) -> String {
        match self {
            Kind::Bulleted => "•".to_owned(),
            Kind::Dashed => "–".to_owned(),
            Kind::Numbered => format!("{number}."),
            Kind::Checklist(true) => "☑".to_owned(),
            Kind::Checklist(false) => "☐".to_owned(),
            _ => String::new(),
        }
    }

    /// A table cell of an Apple Notes-style table (no headers).
    pub fn cell(cols: usize) -> Kind {
        Kind::Cell { cols: cols as u16, header_row: false, header_col: false }
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
    /// An image: the character is [`crate::attachments::OBJECT`], shown as
    /// the attachment of this name.
    pub image: Option<Arc<str>>,
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
            let mut d = Doc { blocks };
            // There's always a paragraph after a table to go on writing in
            // (as in Apple Notes); the Markdown doesn't keep it if empty.
            if d.blocks.last().is_some_and(|b| b.kind.is_cell()) {
                d.blocks.push(Block::default());
            }
            d
        }
    }

    // ---- Tables ---------------------------------------------------------

    /// The table a block is a cell of: its blocks and its number of columns.
    pub fn table_of(&self, block: usize) -> Option<(std::ops::Range<usize>, usize)> {
        let Kind::Cell { cols, .. } = self.blocks.get(block)?.kind else { return None };
        let mut a = block;
        while a > 0 && self.blocks[a - 1].kind.is_cell() {
            a -= 1;
        }
        let mut b = block + 1;
        while b < self.blocks.len() && self.blocks[b].kind.is_cell() {
            b += 1;
        }
        Some((a..b, (cols as usize).max(1)))
    }

    /// A cell's row and column.
    pub fn cell_at(&self, block: usize) -> Option<(usize, usize)> {
        let (r, cols) = self.table_of(block)?;
        let i = block - r.start;
        Some((i / cols, i % cols))
    }

    fn set_table_kind(&mut self, range: std::ops::Range<usize>, kind: Kind) {
        for b in &mut self.blocks[range] {
            b.kind = kind;
        }
    }

    /// Inserts an empty table at `p` (splitting its paragraph, or in place of
    /// an empty one), with a paragraph after it. Returns its first cell.
    pub fn insert_table(&mut self, p: Pos, rows: usize, cols: usize) -> Pos {
        if self.blocks[p.block].kind.is_cell() || rows == 0 || cols == 0 {
            return p;
        }
        let len = self.blocks[p.block].len();
        let at = if self.blocks[p.block].is_empty() {
            self.blocks.remove(p.block);
            p.block
        } else if p.offset == 0 {
            p.block
        } else if p.offset == len {
            p.block + 1
        } else {
            let rest = self.blocks[p.block].slice(p.offset, len);
            self.blocks[p.block].remove(p.offset, len);
            self.blocks.insert(p.block + 1, rest);
            p.block + 1
        };
        let n = rows * cols;
        self.blocks.splice(at..at, (0..n).map(|_| Block::new(Kind::cell(cols), "")));
        if self.blocks.get(at + n).map_or(true, |b| b.kind.is_cell()) {
            self.blocks.insert(at + n, Block::default());
        }
        Pos::new(at, 0)
    }

    /// Adds a row above or below the cell's. Returns the new row's cell in
    /// the same column.
    pub fn add_row(&mut self, block: usize, below: bool) -> Pos {
        let Some((r, cols)) = self.table_of(block) else { return Pos::new(block, 0) };
        let (row, col) = ((block - r.start) / cols, (block - r.start) % cols);
        let kind = self.blocks[block].kind;
        let at = r.start + (row + below as usize) * cols;
        self.blocks.splice(at..at, (0..cols).map(|_| Block::new(kind, "")));
        Pos::new(at + col, 0)
    }

    /// Adds a column before or after the cell's. Returns the new column's
    /// cell in the same row.
    pub fn add_column(&mut self, block: usize, after: bool) -> Pos {
        let Some((r, cols)) = self.table_of(block) else { return Pos::new(block, 0) };
        let Kind::Cell { header_row, header_col, .. } = self.blocks[block].kind else { return Pos::new(block, 0) };
        let (row, col) = ((block - r.start) / cols, (block - r.start) % cols);
        let new_col = col + after as usize;
        let rows = r.len() / cols;
        let kind = Kind::Cell { cols: cols as u16 + 1, header_row, header_col };
        for rw in (0..rows).rev() {
            self.blocks.insert(r.start + rw * cols + new_col, Block::new(kind, ""));
        }
        self.set_table_kind(r.start..r.start + rows * (cols + 1), kind);
        Pos::new(r.start + row * (cols + 1) + new_col, 0)
    }

    /// Deletes the cell's row (the table, if it's the only one). Returns
    /// where the caret goes.
    pub fn delete_row(&mut self, block: usize) -> Pos {
        let Some((r, cols)) = self.table_of(block) else { return Pos::new(block, 0) };
        let rows = r.len() / cols;
        if rows <= 1 {
            return self.delete_table(block);
        }
        let (row, col) = ((block - r.start) / cols, (block - r.start) % cols);
        let at = r.start + row * cols;
        self.blocks.drain(at..at + cols);
        Pos::new(r.start + row.min(rows - 2) * cols + col, 0)
    }

    /// Deletes the cell's column (the table, if it's the only one).
    pub fn delete_column(&mut self, block: usize) -> Pos {
        let Some((r, cols)) = self.table_of(block) else { return Pos::new(block, 0) };
        if cols <= 1 {
            return self.delete_table(block);
        }
        let Kind::Cell { header_row, header_col, .. } = self.blocks[block].kind else { return Pos::new(block, 0) };
        let (row, col) = ((block - r.start) / cols, (block - r.start) % cols);
        let rows = r.len() / cols;
        for rw in (0..rows).rev() {
            self.blocks.remove(r.start + rw * cols + col);
        }
        self.set_table_kind(r.start..r.start + rows * (cols - 1), Kind::Cell { cols: cols as u16 - 1, header_row, header_col });
        Pos::new(r.start + row * (cols - 1) + col.min(cols - 2), 0)
    }

    /// Deletes the whole table. Returns the paragraph after it.
    pub fn delete_table(&mut self, block: usize) -> Pos {
        let Some((r, _)) = self.table_of(block) else { return Pos::new(block, 0) };
        let at = r.start;
        self.blocks.drain(r);
        if self.blocks.is_empty() {
            self.blocks.push(Block::default());
        }
        // Two tables that now touch stay apart.
        if at > 0 && at < self.blocks.len() && self.blocks[at - 1].kind.is_cell() && self.blocks[at].kind.is_cell() {
            self.blocks.insert(at, Block::default());
        }
        let at = at.min(self.blocks.len() - 1);
        Pos::new(at, 0)
    }

    /// Turns the header row or column on or off for the cell's table.
    pub fn toggle_header(&mut self, block: usize, column: bool) {
        let Some((r, _)) = self.table_of(block) else { return };
        let Kind::Cell { cols, header_row, header_col } = self.blocks[block].kind else { return };
        let kind = if column {
            Kind::Cell { cols, header_row, header_col: !header_col }
        } else {
            Kind::Cell { cols, header_row: !header_row, header_col }
        };
        self.set_table_kind(r, kind);
    }

    /// The cell after (`dir` 1) or before (-1) a cell, row by row.
    pub fn next_cell(&self, block: usize, dir: i32) -> Option<usize> {
        let (r, _) = self.table_of(block)?;
        let i = block as i64 + dir as i64;
        (i >= r.start as i64 && i < r.end as i64).then_some(i as usize)
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

    /// The note as text to send elsewhere (Messages, Mail; Apple Notes'
    /// "Send a Copy"): list markers, numbers and checkboxes written out,
    /// nested items indented, a table's rows as lines of tab-separated cells.
    pub fn shared_text(&self) -> String {
        let numbers = self.list_numbers();
        let mut lines: Vec<String> = Vec::new();
        let mut i = 0;
        while i < self.blocks.len() {
            let b = &self.blocks[i];
            if let Kind::Cell { cols, .. } = b.kind {
                let cols = (cols as usize).max(1);
                let mut end = i;
                while end < self.blocks.len() && self.blocks[end].kind.is_cell() {
                    end += 1;
                }
                for row in self.blocks[i..end].chunks(cols) {
                    lines.push(row.iter().map(|c| c.text.as_str()).collect::<Vec<_>>().join("\t"));
                }
                i = end;
                continue;
            }
            let pad = "    ".repeat(b.indent as usize);
            let marker = b.kind.list_marker(numbers[i]);
            let sep = if marker.is_empty() { "" } else { " " };
            lines.push(format!("{pad}{marker}{sep}{}", b.text));
            i += 1;
        }
        lines.join("\n").trim_end_matches('\n').to_owned()
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
        // Typing next to an image is text.
        s.image = None;
        s
    }

    /// Inserts an image at `p`. It gets a paragraph of its own (the text
    /// after `p` moves below it; in a table, it goes below the table), and
    /// the caret goes to the paragraph after it, as in Apple Notes. Returns
    /// the new caret.
    pub fn insert_image(&mut self, p: Pos, name: &str) -> Pos {
        let style = Style { image: Some(Arc::from(name)), ..Style::default() };
        self.insert_line_of(p, &crate::attachments::OBJECT.to_string(), &style)
    }

    /// Inserts an attached file at `p`, as [`Doc::insert_image`] does a
    /// picture: its name (`label`) on a line of its own, linked to the
    /// attachment (`attachments/<reference>`). Returns the new caret.
    pub fn insert_file(&mut self, p: Pos, reference: &str, label: &str) -> Pos {
        let style = Style { link: Some(Arc::from(format!("{}{reference}", crate::attachments::URL_PREFIX))), ..Style::default() };
        let label: String = label.chars().map(|c| if c.is_control() || c == crate::attachments::OBJECT { ' ' } else { c }).collect();
        let label = if label.trim().is_empty() { "Attachment" } else { label.trim() };
        self.insert_line_of(p, label, &style)
    }

    /// `text` in `style` at `p`, in a body paragraph of its own (below the
    /// table if `p` is in one). Returns the start of the paragraph after it.
    fn insert_line_of(&mut self, p: Pos, text: &str, style: &Style) -> Pos {
        let mut p = p;
        if let Some((r, _)) = self.table_of(p.block) {
            // (There's always a paragraph after a table.)
            if r.end >= self.blocks.len() {
                self.blocks.push(Block::default());
            }
            p = Pos::new(r.end, 0);
        }
        if p.offset > 0 {
            p = self.split(p);
        }
        let rest = !self.blocks[p.block].is_empty();
        if rest {
            self.split(p);
            p = Pos::new(p.block, 0);
        }
        let b = &mut self.blocks[p.block];
        // Headings and code blocks can't hold an image: it's body text.
        if matches!(b.kind, Kind::Title | Kind::Heading | Kind::Subheading | Kind::Monospaced) {
            b.kind = Kind::Body;
        }
        b.insert(0, text, style);
        if !rest {
            self.blocks.insert(p.block + 1, Block::default());
        }
        Pos::new(p.block + 1, 0)
    }

    /// The attachments the document shows, in order, each once.
    pub fn images(&self) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for s in self.blocks.iter().flat_map(|b| &b.styles) {
            if let Some(n) = &s.image {
                if !out.iter().any(|x| **x == **n) {
                    out.push(n.to_string());
                }
            }
        }
        out
    }

    /// Deletes `[start, end)`, joining the first and last paragraph. The first
    /// paragraph keeps its style. Returns `start`.
    pub fn delete(&mut self, start: Pos, end: Pos) -> Pos {
        if start >= end {
            return start;
        }
        if start.block == end.block {
            self.blocks[start.block].remove(start.offset, end.offset);
        } else if (start.block..=end.block).any(|i| self.blocks[i].kind.is_cell()) {
            self.delete_around_cells(start, end);
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

    /// A deletion touching table cells: cells are emptied, never removed or
    /// joined to anything (the grid stays), unless their whole table is
    /// inside the range; other paragraphs inside go; the ends join only if
    /// neither is a cell and nothing is left between them.
    fn delete_around_cells(&mut self, start: Pos, end: Pos) {
        let (first, last) = (start.block, end.block);
        self.blocks[last].remove(0, end.offset);
        let n = self.blocks[first].len();
        self.blocks[first].remove(start.offset, n);
        let (mut kept, mut removed) = (0, 0);
        for i in (first + 1..last).rev() {
            // Where the range's last paragraph is now.
            let last_now = last - removed;
            let whole_table = self.table_of(i).is_some_and(|(r, _)| r.start > first && r.end <= last_now);
            if self.blocks[i].kind.is_cell() && !whole_table {
                let n = self.blocks[i].len();
                self.blocks[i].remove(0, n);
                kept += 1;
            } else {
                self.blocks.remove(i);
                removed += 1;
            }
        }
        let last_now = first + 1 + kept;
        if kept == 0 && !self.blocks[first].kind.is_cell() && !self.blocks[last_now].kind.is_cell() {
            let tail = self.blocks.remove(last_now);
            self.blocks[first].append(tail);
        }
    }

    /// Inserts text (which may hold newlines: new paragraphs of the same
    /// kind; line breaks in a table cell) at `p` in `style`. Returns the
    /// position after it.
    pub fn insert(&mut self, p: Pos, text: &str, style: &Style) -> Pos {
        if self.blocks[p.block].kind.is_cell() {
            let text = text.replace("\r\n", "\n").replace('\r', "\n");
            self.blocks[p.block].insert(p.offset, &text, style);
            return Pos::new(p.block, p.offset + text.chars().count());
        }
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
        // Into a cell: the pasted paragraphs as lines of it.
        if self.blocks[p.block].kind.is_cell() {
            let mut at = p.offset;
            for (i, b) in blocks.into_iter().enumerate() {
                if i > 0 {
                    self.blocks[p.block].insert(at, "\n", &Style::default());
                    at += 1;
                }
                let n = b.len();
                let cell = &mut self.blocks[p.block];
                let (bi, tail) = (cell.byte(at), cell.slice(at, cell.len()));
                cell.text.truncate(bi);
                cell.styles.truncate(at);
                cell.append(b);
                cell.append(tail);
                at += n;
            }
            return Pos::new(p.block, at);
        }
        // A pasted table goes in whole, between the two halves of the
        // paragraph at `p`.
        if blocks.iter().any(|b| b.kind.is_cell()) {
            let target = &mut self.blocks[p.block];
            let tail = target.slice(p.offset, target.len());
            let n = target.len();
            target.remove(p.offset, n);
            let count = blocks.len();
            let at = p.block + 1;
            self.blocks.splice(at..at, blocks);
            let last = at + count - 1;
            let end = Pos::new(last, self.blocks[last].len());
            if self.blocks[last].kind.is_cell() || !tail.is_empty() {
                self.blocks.insert(last + 1, Block { kind: if tail.kind.is_cell() { Kind::Body } else { tail.kind }, ..tail });
            }
            return end;
        }
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
        // Return in a table cell: a line break in it (as in Apple Notes).
        if self.blocks[p.block].kind.is_cell() {
            let style = self.style_at(p);
            self.blocks[p.block].insert(p.offset, "\n", &style);
            return Pos::new(p.block, p.offset + 1);
        }
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
        // At the start of a cell: nothing to join it to.
        if self.blocks[block].kind.is_cell() {
            return Pos::new(block, 0);
        }
        // Right after a table: never joined into its last cell. An empty
        // paragraph goes (unless it's the last one), else the caret moves
        // into the table.
        if block > 0 && self.blocks[block - 1].kind.is_cell() && self.blocks[block].indent == 0 && !self.blocks[block].kind.is_list() {
            let end = Pos::new(block - 1, self.blocks[block - 1].len());
            if self.blocks[block].is_empty() && block + 1 < self.blocks.len() && !self.blocks[block + 1].kind.is_cell() {
                self.blocks.remove(block);
            }
            return end;
        }
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
        // Table cells keep being cells.
        let all = self.blocks[blocks.clone()].iter().filter(|b| !b.kind.is_cell()).all(|b| b.kind.same(kind));
        let toggle_off = all && (kind.is_list() || kind == Kind::Quote || kind == Kind::Monospaced);
        for b in self.blocks[blocks].iter_mut().filter(|b| !b.kind.is_cell()) {
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
        // Tables aren't moved, nor paragraphs into or past them.
        let touches_cell = |i: usize| self.blocks.get(i).is_some_and(|x| x.kind.is_cell());
        if (a..=b).any(touches_cell) || (delta < 0 && a > 0 && touches_cell(a - 1)) || (delta > 0 && touches_cell(b + 1)) {
            return false;
        }
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
    /// One character (as seen: a whole emoji, flag or accented letter)
    /// back (`dir < 0`) or forward, across paragraph ends.
    pub fn char_step(&self, p: Pos, dir: i32) -> Pos {
        if dir < 0 {
            if p.offset > 0 {
                Pos::new(p.block, grapheme_step(&self.blocks[p.block].text, p.offset, -1))
            } else if p.block > 0 {
                Pos::new(p.block - 1, self.blocks[p.block - 1].len())
            } else {
                p
            }
        } else if p.offset < self.blocks[p.block].len() {
            Pos::new(p.block, grapheme_step(&self.blocks[p.block].text, p.offset, 1))
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

/// The char offset of the next (`dir > 0`) or previous grapheme boundary
/// from `offset` (a char offset) in `text`.
fn grapheme_step(text: &str, offset: usize, dir: i32) -> usize {
    use unicode_segmentation::UnicodeSegmentation;
    let mut at = 0;
    let mut prev = 0;
    for g in text.graphemes(true) {
        let next = at + g.chars().count();
        if dir > 0 && next > offset {
            return next;
        }
        if dir < 0 && next >= offset {
            return if at < offset { at } else { prev };
        }
        prev = at;
        at = next;
    }
    if dir > 0 { at } else { prev.min(offset.saturating_sub(1)) }
}

/// Font size of a paragraph style, for a body size.
pub fn font_size(kind: Kind, body: f32) -> f32 {
    match kind {
        Kind::Title => (body * 1.6).round(),
        Kind::Heading => (body * 1.33).round(),
        Kind::Subheading => (body * 1.13).round(),
        Kind::Monospaced => body * 0.93,
        _ => body,
    }
}

/// Plain URLs in text (Apple Notes makes them links), as char ranges.
pub fn auto_links(text: &str) -> Vec<(std::ops::Range<usize>, String)> {
    let chars: Vec<char> = text.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        let at_word = i == 0 || !chars[i - 1].is_alphanumeric();
        let rest: String = chars[i..chars.len().min(i + 8)].iter().collect::<String>().to_lowercase();
        let scheme = ["https://", "http://", "www."].into_iter().find(|s| rest.starts_with(s));
        if let (true, Some(s)) = (at_word, scheme) {
            let mut j = i + s.chars().count();
            while j < chars.len() && !chars[j].is_whitespace() && !matches!(chars[j], '<' | '>' | '"') {
                j += 1;
            }
            // Trailing punctuation isn't part of the link.
            while j > i && matches!(chars[j - 1], '.' | ',' | ';' | ':' | '!' | '?' | ')' | '\'') {
                j -= 1;
            }
            if j > i + s.chars().count() {
                let url: String = chars[i..j].iter().collect();
                let url = if s == "www." { format!("https://{url}") } else { url };
                out.push((i..j, url));
            }
            i = j.max(i + 1);
        } else {
            i += 1;
        }
    }
    out
}

/// Hashtag ranges in a paragraph (chars), drawn in the accent color.
pub fn tag_ranges(text: &str) -> Vec<std::ops::Range<usize>> {
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
            if !tag.is_empty() && tag.chars().any(|c| !c.is_ascii_digit()) {
                out.push(i..j);
            }
            i = j;
        } else {
            i += 1;
        }
    }
    out
}

/// Text folded for case-insensitive search, character by character, so
/// positions in it are positions in `s` (whole-string lowercasing would
/// turn a final Σ into ς; the few characters that lowercase to two are
/// kept as they are).
pub fn fold_case(s: &str) -> Vec<char> {
    s.chars()
        .flat_map(|c| {
            let l: Vec<char> = c.to_lowercase().collect();
            if l.len() == 1 {
                l
            } else {
                vec![c]
            }
        })
        .collect()
}

/// The char ranges of `text` where any of `words` matches (case
/// insensitive), merged where they touch or overlap.
pub fn match_ranges(text: &str, words: &[&str]) -> Vec<std::ops::Range<usize>> {
    let t = fold_case(text);
    let mut hits: Vec<std::ops::Range<usize>> = Vec::new();
    for w in words {
        let q = fold_case(w);
        if q.is_empty() || q.len() > t.len() {
            continue;
        }
        for i in 0..=t.len() - q.len() {
            if t[i..i + q.len()] == q[..] {
                hits.push(i..i + q.len());
            }
        }
    }
    hits.sort_by_key(|r| r.start);
    let mut merged: Vec<std::ops::Range<usize>> = Vec::new();
    for r in hits {
        match merged.last_mut() {
            Some(last) if r.start <= last.end => last.end = last.end.max(r.end),
            _ => merged.push(r),
        }
    }
    merged
}

/// `text` in parts, each marked if it's a match of one of `words`: for
/// showing search results with the matches in bold.
pub fn highlight_parts(text: &str, words: &[&str]) -> Vec<(String, bool)> {
    let chars: Vec<char> = text.chars().collect();
    let mut out = Vec::new();
    let mut at = 0;
    for r in match_ranges(text, words) {
        if r.start > at {
            out.push((chars[at..r.start].iter().collect(), false));
        }
        out.push((chars[r.clone()].iter().collect(), true));
        at = r.end;
    }
    if at < chars.len() || out.is_empty() {
        out.push((chars[at..].iter().collect(), false));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn search_matches_are_found_case_insensitively_and_merged() {
        assert_eq!(match_ranges("Straße and STRASSE", &["straße"]), [0..6]);
        assert_eq!(match_ranges("ΟΔΟΣ", &["οδοσ"]), [0..4]);
        // Overlapping and touching matches of different words become one.
        assert_eq!(match_ranges("milkshake", &["milk", "shake", "lks"]), [0..9]);
        assert_eq!(
            highlight_parts("Buy Milk and more milk", &["milk"]),
            [
                ("Buy ".to_owned(), false),
                ("Milk".to_owned(), true),
                (" and more ".to_owned(), false),
                ("milk".to_owned(), true)
            ]
        );
        assert_eq!(highlight_parts("nothing", &["x"]), [("nothing".to_owned(), false)]);
        assert_eq!(highlight_parts("", &["x"]), [(String::new(), false)]);
    }

    #[test]
    fn shared_text_writes_out_lists_checkboxes_and_tables() {
        let d = crate::markdown::from_markdown(
            "# Trip\n- [ ] passport\n- [x] tickets\n1. first\n   1. nested\n2. second\n* dot\n- dash\n> quoted\n\n| a | b |\n|---|---|\n| 1 | 2 |\n",
        );
        assert_eq!(
            d.shared_text(),
            "Trip\n☐ passport\n☑ tickets\n1. first\n    1. nested\n2. second\n• dot\n– dash\nquoted\n\na\tb\n1\t2"
        );
    }

    #[test]
    fn the_caret_steps_over_whole_characters() {
        let d = doc(&[(Kind::Body, "a👍🏽b🇩🇪e\u{301}")]);
        // a | 👍🏽 (2 chars) | b | 🇩🇪 (2 chars) | é (2 chars)
        let stops: Vec<usize> = std::iter::successors(Some(Pos::new(0, 0)), |&p| {
            let n = d.char_step(p, 1);
            (n != p).then_some(n)
        })
        .map(|p| p.offset)
        .collect();
        assert_eq!(stops, [0, 1, 3, 4, 6, 8]);
        let back: Vec<usize> = std::iter::successors(Some(Pos::new(0, 8)), |&p| {
            let n = d.char_step(p, -1);
            (n != p).then_some(n)
        })
        .map(|p| p.offset)
        .collect();
        assert_eq!(back, [8, 6, 4, 3, 1, 0]);
    }

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
    fn images_get_a_paragraph_of_their_own() {
        let mut d = doc(&[(Kind::Title, "Trip"), (Kind::Body, "before after")]);
        let p = d.insert_image(Pos::new(1, 7), "x.png");
        assert_eq!(p, Pos::new(3, 0));
        let o = crate::attachments::OBJECT.to_string();
        assert_eq!(texts(&d)[1..], [(Kind::Body, "before ".into()), (Kind::Body, o.clone()), (Kind::Body, "after".into())]);
        assert_eq!(d.blocks[2].styles[0].image.as_deref(), Some("x.png"));
        // At the end of a note: an empty paragraph to go on in.
        let mut d = doc(&[(Kind::Title, "Trip")]);
        let p = d.insert_image(Pos::new(0, 4), "y.png");
        assert_eq!(texts(&d), [(Kind::Title, "Trip".into()), (Kind::Body, o.clone()), (Kind::Body, String::new())]);
        assert_eq!(p, Pos::new(2, 0));
        // Typing after an image isn't an image.
        assert_eq!(d.style_at(Pos::new(1, 1)).image, None);
        assert_eq!(d.images(), vec!["y.png"]);
        // In a table: below it.
        let mut d = doc(&[(Kind::Body, "")]);
        d.insert_table(Pos::new(0, 0), 1, 2);
        let p = d.insert_image(Pos::new(1, 0), "z.png");
        assert!(d.blocks[0].text.is_empty() && d.blocks[1].text.is_empty());
        assert_eq!(d.blocks[2].text, o);
        assert_eq!(p, Pos::new(3, 0));
    }

    #[test]
    fn hashtags() {
        assert_eq!(tags_in("#Work notes about #home-office and #1 or a#b (#x)"), vec!["work", "home-office", "x"]);
        let d = doc(&[(Kind::Body, "#a #b"), (Kind::Monospaced, "#code"), (Kind::Body, "#A")]);
        assert_eq!(d.tags(), vec!["a", "b"]);
    }

    fn grid(d: &Doc) -> Vec<Vec<String>> {
        let (r, cols) = d.blocks.iter().position(|b| b.kind.is_cell()).and_then(|i| d.table_of(i)).unwrap();
        d.blocks[r].chunks(cols).map(|row| row.iter().map(|b| b.text.clone()).collect()).collect()
    }

    #[test]
    fn inserting_a_table_and_editing_its_rows_and_columns() {
        let mut d = doc(&[(Kind::Body, "Before"), (Kind::Body, "")]);
        let p = d.insert_table(Pos::new(1, 0), 2, 2);
        // In place of the empty paragraph, with one after it to go on in.
        assert_eq!(p, Pos::new(1, 0));
        assert_eq!(d.blocks.len(), 1 + 4 + 1);
        assert_eq!(d.cell_at(4), Some((1, 1)));
        let p = d.insert(p, "a", &Style::default());
        let p = d.insert(Pos::new(p.block + 1, 0), "b", &Style::default());
        let _ = d.insert(Pos::new(p.block + 2, 0), "d", &Style::default());
        assert_eq!(grid(&d), [["a", "b"], ["", "d"]]);
        let p = d.add_row(2, true);
        assert_eq!(p, Pos::new(4, 0));
        assert_eq!(grid(&d), [["a", "b"], ["", ""], ["", "d"]]);
        let p = d.add_column(1, false);
        assert_eq!(p, Pos::new(1, 0));
        assert_eq!(grid(&d), [["", "a", "b"], ["", "", ""], ["", "", "d"]]);
        d.delete_column(1);
        d.delete_row(4);
        assert_eq!(grid(&d), [["a", "b"], ["", "d"]]);
        let p = d.delete_table(2);
        assert_eq!(texts(&d), [(Kind::Body, "Before".into()), (Kind::Body, String::new())]);
        assert_eq!(p, Pos::new(1, 0));
    }

    #[test]
    fn typing_in_cells_never_breaks_the_grid() {
        let mut d = doc(&[(Kind::Body, "Intro"), (Kind::Body, "")]);
        d.insert_table(Pos::new(1, 0), 2, 2);
        // Return is a line break in the cell; pasted lines too.
        let p = d.enter(Pos::new(1, 0));
        assert_eq!((p, d.blocks[1].text.as_str()), (Pos::new(1, 1), "\n"));
        d.insert(Pos::new(2, 0), "x\ny", &Style::default());
        assert_eq!(d.blocks[2].text, "x\ny");
        // Backspace at a cell's start, and Delete at the end of the
        // paragraph before the table, join nothing.
        assert_eq!(d.backspace_at_start(3), Pos::new(3, 0));
        d.delete(Pos::new(0, 5), Pos::new(1, 0));
        assert_eq!(grid(&d), [["\n", "x\ny"], ["", ""]]);
        // A selection from the intro into the table empties the cells it
        // covers but keeps them.
        d.delete(Pos::new(0, 2), Pos::new(2, 1));
        assert_eq!(d.blocks[0].text, "In");
        assert_eq!(grid(&d), [["", "\ny"], ["", ""]]);
        // Selecting over the whole table removes it.
        let last = d.blocks.len() - 1;
        d.delete(Pos::new(0, 2), Pos::new(last, 0));
        assert_eq!(texts(&d), [(Kind::Body, "In".into())]);
        // Paragraph styles and moves leave cells alone.
        let mut d = doc(&[(Kind::Body, "")]);
        d.insert_table(Pos::new(0, 0), 1, 2);
        d.set_kind(0..=2, Kind::Bulleted);
        assert!(d.blocks[0].kind.is_cell() && d.blocks[1].kind.is_cell());
        assert_eq!(d.blocks[2].kind, Kind::Bulleted);
        assert!(!d.move_blocks(2..=2, -1));
        // Backspace after a table: a bullet goes first, then the caret
        // moves into the last cell (nothing is joined into it).
        assert_eq!(d.backspace_at_start(2), Pos::new(2, 0));
        assert_eq!(d.blocks[2].kind, Kind::Body);
        assert_eq!(d.backspace_at_start(2), Pos::new(1, 0));
        assert_eq!(d.blocks.len(), 3);
    }
}
