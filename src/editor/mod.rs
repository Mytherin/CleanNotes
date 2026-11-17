//! The note editor on the desktop: lays out, draws and takes keyboard and
//! pointer input for the shared editing engine ([`Editing`]), which holds the
//! note as a [`Doc`] and does what typing, Return, the formatting commands,
//! undo and find do to it. Here are the egui parts: layout, the caret,
//! selection painting, arrows by line, clicks, the context menu, the system
//! clipboard and the link sheet.
//!
//! The note's Markdown is only produced when the app asks for it
//! ([`Editing::take_markdown`]); while open, the note is a `Doc`.

use std::collections::HashMap;
use std::ops::{Deref, DerefMut};

use eframe::egui::{self, text::CCursor, Event, Key, Modifiers, Pos2, Rect, Sense, Vec2};

use crate::doc::{Attr, Kind, Pos, Selection};
use crate::theme::Palette;
pub use cleannotes_core::editing::{Command, Editing};

pub mod images;
pub mod layout;
use layout::{Laid, Marks};

/// Settings the editor needs from the app.
pub struct Options<'a> {
    /// Body text size.
    pub text_size: f32,
    /// Move checked items to the bottom of their checklist.
    pub sort_checked: bool,
    /// Recently Deleted notes can be read but not changed.
    pub read_only: bool,
    /// Words to highlight (the search in the toolbar).
    pub highlight: &'a str,
    /// The pictures notes show.
    pub images: &'a images::ImageCache,
    /// Pictures can be added to this note (not locked, not deleted).
    pub can_attach: bool,
}

/// What happened in a frame, for the app.
#[derive(Default)]
pub struct Output {
    /// The user wants to edit a read-only note.
    pub blocked_edit: bool,
    pub open_url: Option<String>,
    /// A picture was double-clicked: its file, to open.
    pub open_image: Option<std::path::PathBuf>,
    /// "Attach Image…" was chosen.
    pub attach_image: bool,
    /// Paste was asked for: a picture on the clipboard is the app's to
    /// paste (egui only brings text).
    pub paste_image: bool,
}

/// The "Add Link" sheet.
pub struct LinkSheet {
    pub url: String,
    pub focus: bool,
}

pub struct Editor {
    /// The note being edited (shared with the iPhone app).
    pub ed: Editing,
    /// The x position vertical moves aim for.
    goal_x: Option<f32>,
    laid: Vec<Laid>,
    pub link_sheet: Option<LinkSheet>,
    /// Last time the caret moved or text changed (the caret shows steadily
    /// right after).
    last_activity: f64,
    dragging: bool,
    pub focus_requested: bool,
}

impl Deref for Editor {
    type Target = Editing;
    fn deref(&self) -> &Editing {
        &self.ed
    }
}

impl DerefMut for Editor {
    fn deref_mut(&mut self) -> &mut Editing {
        &mut self.ed
    }
}

impl Editor {
    pub fn new(note_id: &str, md: &str) -> Editor {
        Editor::wrap(Editing::new(note_id, md))
    }

    /// A new note: its Markdown from the library (empty, or a tag), the
    /// first paragraph in the "New notes start with" style.
    pub fn new_note(note_id: &str, md: &str, start: Kind) -> Editor {
        Editor::wrap(Editing::new_note(note_id, md, start))
    }

    fn wrap(ed: Editing) -> Editor {
        Editor { ed, goal_x: None, laid: Vec::new(), link_sheet: None, last_activity: 0.0, dragging: false, focus_requested: false }
    }

    pub fn id(&self) -> egui::Id {
        egui::Id::new(("note-editor", &self.note_id))
    }

    pub fn command(&mut self, cmd: Command) {
        match cmd {
            Command::EditLink => self.link_sheet = Some(LinkSheet { url: self.ed.link_at_selection(), focus: true }),
            c => self.ed.command(c),
        }
    }

    fn copy(&mut self, ctx: &egui::Context) {
        if let Some(text) = self.ed.copy() {
            ctx.copy_text(text);
        }
    }

    fn copy_markdown(&mut self, ctx: &egui::Context) {
        ctx.copy_text(self.ed.selection_markdown());
    }

    // ---- Geometry -----------------------------------------------------------

    fn caret_rect(&self, p: Pos) -> Option<Rect> {
        let l = self.laid.get(p.block)?;
        if !l.images.is_empty() {
            // Before a picture: at its left edge; after the last: its right.
            let (img, right) = match l.images.get(p.offset) {
                Some(img) => (img, false),
                None => (l.images.last()?, true),
            };
            let x = if right { img.rect.right() + 1.0 } else { img.rect.left() - 3.0 };
            return Some(Rect::from_min_max(Pos2::new(x, img.rect.top()), Pos2::new(x, img.rect.bottom())));
        }
        let r = l.galley.pos_from_ccursor(CCursor::new(p.offset));
        Some(r.translate(l.origin.to_vec2()))
    }

    /// In a paragraph of pictures: the offset nearest a point (before or
    /// after the picture at its height).
    fn image_offset(l: &Laid, pt: Pos2) -> usize {
        let i = l.images.iter().position(|img| pt.y < img.rect.bottom() + layout::IMAGE_GAP / 2.0).unwrap_or(l.images.len() - 1);
        i + (pt.x > l.images[i].rect.center().x) as usize
    }

    /// The picture under a point, if any.
    fn image_at(&self, pt: Pos2) -> Option<&layout::LaidImage> {
        self.laid.iter().flat_map(|l| &l.images).find(|img| img.rect.contains(pt))
    }

    /// The position under a point (screen coordinates).
    fn pos_at(&self, pt: Pos2) -> Pos {
        if self.laid.is_empty() {
            return Pos::default();
        }
        // The paragraph (or table cell) under the point, else the first
        // whose bottom is below it.
        let i = self
            .laid
            .iter()
            .position(|l| l.rect.contains(pt))
            .or_else(|| self.laid.iter().position(|l| pt.y < l.rect.bottom()))
            .unwrap_or(self.laid.len() - 1);
        let l = &self.laid[i];
        if !l.images.is_empty() {
            return Pos::new(i, Self::image_offset(l, pt).min(self.doc.blocks[i].len()));
        }
        let c = l.galley.cursor_from_pos(pt - l.origin);
        Pos::new(i, c.ccursor.index.min(self.doc.blocks[i].len()))
    }

    /// One line up (`-1`) or down (`1`) from `p`, aiming for `x`.
    fn line_step(&self, p: Pos, dir: i32, x: f32) -> Pos {
        // The layout is last frame's: after an edit that added or removed
        // paragraphs this frame, step by paragraph instead.
        if self.laid.len() != self.doc.blocks.len() {
            return match dir {
                d if d < 0 && p.block > 0 => Pos::new(p.block - 1, 0),
                d if d < 0 => Pos::new(0, 0),
                _ if p.block + 1 < self.doc.blocks.len() => Pos::new(p.block + 1, 0),
                _ => self.doc.end(),
            };
        }
        let Some(l) = self.laid.get(p.block) else { return p };
        let cur = l.galley.from_ccursor(CCursor::new(p.offset));
        let row = cur.rcursor.row;
        let rows = l.galley.rows.len();
        let last_row = |b: usize| self.laid[b].galley.rows.len().saturating_sub(1);
        let target = if dir < 0 && row > 0 {
            Some((p.block, row - 1))
        } else if dir > 0 && row + 1 < rows {
            Some((p.block, row + 1))
        } else if let Some((r, cols)) = self.doc.table_of(p.block) {
            // Out of a cell: the cell above or below, else past the table.
            let b = if dir < 0 {
                if p.block >= r.start + cols {
                    Some(p.block - cols)
                } else {
                    r.start.checked_sub(1).map(|b| self.entry_cell(b, x, true))
                }
            } else if p.block + cols < r.end {
                Some(p.block + cols)
            } else {
                (r.end < self.laid.len()).then(|| self.entry_cell(r.end, x, false))
            };
            b.map(|b| (b, if dir < 0 { last_row(b) } else { 0 }))
        } else if dir < 0 && p.block > 0 {
            let b = self.entry_cell(p.block - 1, x, true);
            Some((b, last_row(b)))
        } else if dir > 0 && p.block + 1 < self.laid.len() {
            Some((self.entry_cell(p.block + 1, x, false), 0))
        } else {
            None
        };
        let Some((b, r)) = target else {
            return if dir < 0 { Pos::new(0, 0) } else { self.doc.end() };
        };
        let l = &self.laid[b];
        if !l.images.is_empty() {
            // Into a paragraph of pictures: before or after them, by `x`.
            let first = &l.images[0].rect;
            return Pos::new(b, if x > first.center().x { self.doc.blocks[b].len() } else { 0 });
        }
        let y = l.galley.rows.get(r).map_or(0.0, |row| row.rect.center().y);
        let c = l.galley.cursor_from_pos(Vec2::new(x - l.origin.x, y));
        Pos::new(b, c.ccursor.index.min(self.doc.blocks[b].len()))
    }

    /// Moving up or down into `block`: if it's in a table, the cell under
    /// `x` in the table's last row (coming from below) or first row.
    fn entry_cell(&self, block: usize, x: f32, from_below: bool) -> usize {
        let Some((r, cols)) = self.doc.table_of(block) else { return block };
        let row_start = if from_below { r.start + (r.len() - 1) / cols * cols } else { r.start };
        (row_start..(row_start + cols).min(r.end))
            .find(|&b| self.laid.get(b).is_some_and(|l| x < l.rect.right()))
            .unwrap_or((row_start + cols).min(r.end) - 1)
    }

    fn row_edge(&self, p: Pos, end: bool) -> Pos {
        if self.laid.len() != self.doc.blocks.len() {
            return Pos::new(p.block, if end { self.doc.blocks[p.block].len() } else { 0 });
        }
        let Some(l) = self.laid.get(p.block) else { return p };
        if !l.images.is_empty() {
            return Pos::new(p.block, if end { self.doc.blocks[p.block].len() } else { 0 });
        }
        let cur = l.galley.from_ccursor(CCursor::new(p.offset));
        let c = if end { l.galley.cursor_end_of_row(&cur) } else { l.galley.cursor_begin_of_row(&cur) };
        Pos::new(p.block, c.ccursor.index.min(self.doc.blocks[p.block].len()))
    }

    // ---- Keyboard -------------------------------------------------------

    /// Handles a key press. Returns whether the editor used it.
    fn key(&mut self, key: Key, m: Modifiers, opts: &Options, ctx: &egui::Context) -> bool {
        let cmd = m.command;
        let mac = cfg!(target_os = "macos");
        let word = if mac { m.alt } else { m.ctrl };
        let line = mac && m.mac_cmd;
        let extend = m.shift;
        let editable = !opts.read_only;

        // Formatting shortcuts (Apple Notes'), Ctrl for ⌘ off macOS.
        if cmd && m.shift && !m.alt {
            let k = match key {
                Key::T => Some(Command::SetKind(Kind::Title)),
                Key::H => Some(Command::SetKind(Kind::Heading)),
                Key::J => Some(Command::SetKind(Kind::Subheading)),
                Key::B => Some(Command::SetKind(Kind::Body)),
                Key::M => Some(Command::SetKind(Kind::Monospaced)),
                Key::Num7 => Some(Command::SetKind(Kind::Bulleted)),
                Key::Num8 => Some(Command::SetKind(Kind::Dashed)),
                Key::Num9 => Some(Command::SetKind(Kind::Numbered)),
                Key::L => Some(Command::SetKind(Kind::Checklist(false))),
                Key::U => Some(Command::ToggleChecked),
                Key::Z => Some(Command::Redo),
                _ => None,
            };
            if let Some(c) = k {
                if editable || matches!(c, Command::Redo) {
                    self.command(c);
                }
                return true;
            }
        }
        if cmd && !m.shift && !m.alt {
            let k = match key {
                Key::B => Some(Command::ToggleAttr(Attr::Bold)),
                Key::I => Some(Command::ToggleAttr(Attr::Italic)),
                Key::U => Some(Command::ToggleAttr(Attr::Underline)),
                Key::Quote => Some(Command::SetKind(Kind::Quote)),
                Key::CloseBracket => Some(Command::Indent),
                Key::OpenBracket => Some(Command::Outdent),
                Key::K => Some(Command::EditLink),
                Key::Z => Some(Command::Undo),
                Key::Y if !mac => Some(Command::Redo),
                Key::A => Some(Command::SelectAll),
                _ => None,
            };
            if let Some(c) = k {
                let safe = matches!(c, Command::Undo | Command::Redo | Command::SelectAll);
                if editable || safe {
                    self.command(c);
                }
                return true;
            }
        }
        // Move list items: ⌃⌘↑/↓ (Ctrl+Alt+↑/↓ elsewhere).
        let move_mods = if mac { m.ctrl && m.mac_cmd } else { m.ctrl && m.alt };
        if move_mods && matches!(key, Key::ArrowUp | Key::ArrowDown) {
            if editable {
                self.command(if key == Key::ArrowUp { Command::MoveUp } else { Command::MoveDown });
            }
            return true;
        }
        if cmd && m.alt && m.shift && key == Key::C {
            self.copy_markdown(ctx);
            return true;
        }
        // Table: ⌥⌘T, as in Apple Notes.
        if cmd && m.alt && !m.shift && key == Key::T {
            if editable {
                self.command(Command::InsertTable);
            }
            return true;
        }

        let caret = self.sel.caret;
        let x = || self.goal_x.or_else(|| self.caret_rect(caret).map(|r| r.left())).unwrap_or(0.0);
        match key {
            Key::ArrowLeft | Key::ArrowRight => {
                let dir = if key == Key::ArrowLeft { -1 } else { 1 };
                let p = if line {
                    self.row_edge(caret, dir > 0)
                } else if word {
                    self.doc.word_step(caret, dir)
                } else if !self.sel.is_empty() && !extend {
                    if dir < 0 { self.sel.start() } else { self.sel.end() }
                } else {
                    self.doc.char_step(caret, dir)
                };
                self.goal_x = None;
                self.set_caret(p, extend);
            }
            Key::ArrowUp | Key::ArrowDown => {
                let dir = if key == Key::ArrowUp { -1 } else { 1 };
                let gx = x();
                let p = if line || (cmd && !mac) {
                    if dir < 0 { Pos::default() } else { self.doc.end() }
                } else {
                    self.line_step(caret, dir, gx)
                };
                self.set_caret(p, extend);
                self.goal_x = Some(gx);
            }
            Key::Home | Key::End => {
                let end = key == Key::End;
                let p = if cmd {
                    if end { self.doc.end() } else { Pos::default() }
                } else {
                    self.row_edge(caret, end)
                };
                self.goal_x = None;
                self.set_caret(p, extend);
            }
            Key::PageUp | Key::PageDown => {
                let dir = if key == Key::PageUp { -1 } else { 1 };
                let gx = x();
                let mut p = caret;
                for _ in 0..20 {
                    p = self.line_step(p, dir, gx);
                }
                self.set_caret(p, extend);
                self.goal_x = Some(gx);
            }
            Key::Backspace if editable => self.backspace(word),
            Key::Delete if editable => self.delete_forward(word),
            Key::Enter if editable => {
                if m.shift {
                    // A line break inside a paragraph isn't a thing in
                    // Markdown-per-line notes; Shift+Return starts a new
                    // paragraph like Return.
                }
                self.enter();
            }
            Key::Tab if editable && self.doc.blocks[self.sel.caret.block].kind.is_cell() => self.tab_in_table(m.shift),
            Key::Tab if editable => {
                let blocks = self.sel.blocks();
                let in_list = self.doc.blocks[blocks.clone()].iter().any(|b| b.kind.indents());
                if in_list {
                    self.command(if m.shift { Command::Outdent } else { Command::Indent });
                } else if !m.shift {
                    self.insert_text("\t");
                }
            }
            Key::Escape => {
                if self.find.open {
                    self.find.open = false;
                } else {
                    return false;
                }
            }
            Key::Backspace | Key::Delete | Key::Enter | Key::Tab => {}
            _ => return false,
        }
        if !matches!(key, Key::ArrowUp | Key::ArrowDown | Key::PageUp | Key::PageDown) {
            self.goal_x = None;
        }
        true
    }

    // ---- Drawing and input ----------------------------------------------

    /// Draws the note (inside the caller's scroll area) and handles input.
    pub fn show(&mut self, ui: &mut egui::Ui, p: &Palette, opts: &Options, header: &str) -> Output {
        let mut out = Output::default();
        let id = self.id();
        let ctx = ui.ctx().clone();
        let now = ui.input(|i| i.time);
        if self.last_activity == 0.0 {
            self.last_activity = now;
        }
        // Typing within a moment of the last is one undo step.
        self.ed.clock = now;
        if self.focus_requested {
            ui.memory_mut(|m| m.request_focus(id));
            self.focus_requested = false;
        }
        let focused = ui.memory(|m| m.has_focus(id));
        if focused {
            ui.memory_mut(|m| {
                m.set_focus_lock_filter(
                    id,
                    egui::EventFilter { tab: true, horizontal_arrows: true, vertical_arrows: true, escape: false },
                )
            });
        }

        // ---- Input (applies to last frame's layout) ----
        let sel_before = self.sel;
        let doc_before_len = (self.doc.blocks.len(), self.undo_depth());
        if focused {
            let events = ui.input(|i| i.events.clone());
            let shift_down = ui.input(|i| i.modifiers.shift);
            for ev in events {
                match ev {
                    Event::Text(t) if !opts.read_only => {
                        self.last_activity = now;
                        self.insert_text(&t);
                    }
                    Event::Ime(egui::ImeEvent::Commit(t)) if !opts.read_only => {
                        self.last_activity = now;
                        self.insert_text(&t);
                    }
                    Event::Text(_) | Event::Ime(egui::ImeEvent::Commit(_)) => out.blocked_edit = true,
                    Event::Copy => self.copy(&ctx),
                    // egui reports Ctrl+Shift+X as Cut: it's Strikethrough.
                    Event::Cut if shift_down => {
                        if !opts.read_only {
                            self.command(Command::ToggleAttr(Attr::Strike));
                        }
                    }
                    Event::Cut => {
                        self.copy(&ctx);
                        if !opts.read_only {
                            self.delete_selected();
                        }
                    }
                    Event::Paste(t) if !opts.read_only => {
                        self.last_activity = now;
                        self.paste(&t);
                    }
                    Event::Paste(_) => out.blocked_edit = true,
                    // egui sends no Paste for a clipboard without text (a
                    // copied picture), only Ctrl+V's release: the app looks
                    // for a picture then.
                    Event::Key { key: Key::V, pressed: false, modifiers, .. } if modifiers.command && opts.can_attach => {
                        out.paste_image = true;
                    }
                    Event::Key { key, pressed: true, modifiers, .. } => {
                        if self.key(key, modifiers, opts, &ctx) {
                            self.last_activity = now;
                        }
                    }
                    _ => {}
                }
            }
        }

        // ---- Layout ----
        let body = opts.text_size;
        let full = ui.available_rect_before_wrap();
        let pad_x = 22.0_f32.max((full.width() - 900.0) / 2.0).min(full.width() / 6.0);
        let left = full.left() + pad_x;
        let width = (full.width() - 2.0 * pad_x).max(80.0);

        // The date at the top, centered.
        let header_galley =
            ui.painter().layout_no_wrap(header.to_owned(), crate::theme::font(12.0), p.secondary);
        let mut y = full.top() + 14.0;
        let header_pos = Pos2::new(full.center().x - header_galley.size().x / 2.0, y);
        y += header_galley.size().y + 12.0;

        let mut find_marks: HashMap<usize, Vec<std::ops::Range<usize>>> = HashMap::new();
        let mut current: Option<(usize, std::ops::Range<usize>)> = None;
        let highlight_query = if self.find.open && !self.find.query.is_empty() { self.find.query.clone() } else { opts.highlight.to_owned() };
        if !highlight_query.trim().is_empty() {
            let words: Vec<&str> = if self.find.open { vec![highlight_query.as_str()] } else { highlight_query.split_whitespace().collect() };
            for w in words {
                let m = self.matches(w);
                if self.find.open && !m.is_empty() {
                    current = m.get(self.find.current.min(m.len() - 1)).cloned();
                }
                for (b, r) in m {
                    find_marks.entry(b).or_default().push(r);
                }
            }
        }

        let numbers = self.doc.list_numbers();
        let mut laid = Vec::with_capacity(self.doc.blocks.len());
        let mut tables: Vec<layout::LaidTable> = Vec::new();
        let painter = ui.painter().clone();
        let mut prev_kind = None;
        let mut next = 0;
        for (i, b) in self.doc.blocks.iter().enumerate() {
            if i < next {
                continue; // a cell, laid out with its table
            }
            if let (Kind::Cell { header_row, header_col, .. }, Some((r, cols))) = (b.kind, self.doc.table_of(i)) {
                // A table: columns of equal width across the text, each row
                // as tall as its tallest cell.
                y += if prev_kind.is_some() { body * 0.5 } else { 0.0 };
                let col_w = width / cols as f32;
                let mut ys = vec![y];
                for row_start in r.clone().step_by(cols) {
                    let row_end = (row_start + cols).min(r.end);
                    let mut row = Vec::new();
                    let mut h = layout::size(Kind::Body, body) * 1.2;
                    for (col, bi) in (row_start..row_end).enumerate() {
                        let cell = &self.doc.blocks[bi];
                        let row_i = (row_start - r.start) / cols;
                        let strong = (header_row && row_i == 0) || (header_col && col == 0);
                        let marks = Marks {
                            highlights: find_marks.remove(&bi).unwrap_or_default(),
                            current: current.as_ref().filter(|(cb, _)| *cb == bi).map(|(_, r)| r.clone()),
                        };
                        let job = layout::job(cell, body, p, col_w - 2.0 * layout::CELL_PAD.x, &marks, strong);
                        let galley = ui.fonts(|f| f.layout_job(job));
                        h = h.max(galley.rect.height());
                        row.push(galley);
                    }
                    let row_h = h + 2.0 * layout::CELL_PAD.y;
                    for (col, galley) in row.into_iter().enumerate() {
                        let rect = Rect::from_min_size(Pos2::new(left + col as f32 * col_w, y), Vec2::new(col_w, row_h));
                        laid.push(Laid { rect, origin: rect.min + layout::CELL_PAD, galley, checkbox: None, images: Vec::new() });
                    }
                    y += row_h;
                    ys.push(y);
                }
                let xs: Vec<f32> = (0..=cols).map(|c| left + c as f32 * col_w).collect();
                tables.push(layout::LaidTable {
                    rect: Rect::from_min_max(Pos2::new(left, ys[0]), Pos2::new(left + width, y)),
                    xs,
                    ys,
                    header_row,
                    header_col,
                });
                y += body * 0.5;
                prev_kind = Some(b.kind);
                next = r.end;
                continue;
            }
            y += layout::space_before(b.kind, prev_kind, body);
            prev_kind = Some(b.kind);
            let indent = layout::text_indent(b);
            let marks = Marks {
                highlights: find_marks.remove(&i).unwrap_or_default(),
                current: current.as_ref().filter(|(cb, _)| *cb == i).map(|(_, r)| r.clone()),
            };
            if layout::is_image_block(b) {
                // Pictures, one below the other, as wide as they are (in
                // points) but no wider than the text.
                let ppp = ui.ctx().pixels_per_point();
                let mut images = Vec::new();
                let mut iy = y + layout::IMAGE_GAP / 2.0;
                for s in &b.styles {
                    let name = s.image.as_deref().unwrap_or_default();
                    let shown = opts.images.get(&ctx, name);
                    let size = images::display_size(shown, (width - indent - 4.0).max(40.0), ppp);
                    let rect = Rect::from_min_size(Pos2::new(left + indent, iy), size);
                    images.push(layout::LaidImage { name: name.to_owned(), shown, rect });
                    iy += size.y + layout::IMAGE_GAP;
                }
                let empty = crate::doc::Block::new(b.kind, "");
                let galley = ui.fonts(|f| f.layout_job(layout::job(&empty, body, p, width - indent, &Marks::default(), false)));
                let h = iy - y - layout::IMAGE_GAP / 2.0;
                let rect = Rect::from_min_size(Pos2::new(left, y), Vec2::new(width, h));
                laid.push(Laid { rect, origin: Pos2::new(left + indent, y), galley, checkbox: None, images });
                y += h + 1.0;
                continue;
            }
            let job = layout::job(b, body, p, width - indent, &marks, false);
            let galley = ui.fonts(|f| f.layout_job(job));
            let mono_pad = if b.kind == Kind::Monospaced { 2.0 } else { 0.0 };
            let origin = Pos2::new(left + indent, y + mono_pad);
            let h = galley.rect.height().max(layout::size(b.kind, body) * 1.2) + 2.0 * mono_pad;
            let rect = Rect::from_min_size(Pos2::new(left, y), Vec2::new(width, h));
            laid.push(Laid { rect, origin, galley, checkbox: None, images: Vec::new() });
            y += h + 1.0;
        }
        let content = Rect::from_min_max(full.min, Pos2::new(full.right(), (y + 120.0).max(full.bottom())));
        // The interaction goes under the editor's own id, so keyboard focus
        // given to that id sticks.
        ui.allocate_rect(content, Sense::hover());
        let resp = ui.interact(content, id, Sense::click_and_drag());

        // Code block backgrounds, then selection, then text.
        let mut run_start: Option<Rect> = None;
        for (i, l) in laid.iter().enumerate() {
            if self.doc.blocks[i].kind == Kind::Monospaced {
                run_start = Some(run_start.map_or(l.rect, |r| r.union(l.rect)));
            }
            let next_mono = self.doc.blocks.get(i + 1).is_some_and(|b| b.kind == Kind::Monospaced);
            if let (Some(r), false) = (run_start, next_mono) {
                painter.rect_filled(r.expand2(Vec2::new(8.0, 4.0)), 6.0, p.code_bg);
                run_start = None;
            }
        }
        for t in &tables {
            layout::paint_table(&painter, t, p);
        }
        if !self.sel.is_empty() {
            let color = if focused { p.text_selection } else { p.selection_inactive };
            let (s, e) = (self.sel.start(), self.sel.end());
            for (i, l) in laid.iter().enumerate().take(e.block + 1).skip(s.block) {
                let from = if i == s.block { s.offset } else { 0 };
                let to = if i == e.block { e.offset } else { self.doc.blocks[i].len() };
                if l.images.is_empty() {
                    paint_range(&painter, l, from, to, i != e.block, color);
                }
            }
        }
        painter.galley(header_pos, header_galley, p.secondary);
        let (sel_start, sel_end) = (self.sel.start(), self.sel.end());
        for (i, l) in laid.iter_mut().enumerate() {
            let b = &self.doc.blocks[i];
            l.checkbox = layout::paint_margin(&painter, b, numbers[i], l.rect, l.origin, &l.galley, body, p);
            painter.galley(l.origin, l.galley.clone(), p.text);
            for (k, img) in l.images.iter().enumerate() {
                layout::paint_image(&painter, img, p);
                // A selected picture is tinted with the selection.
                let at = Pos::new(i, k);
                if !self.sel.is_empty() && sel_start <= at && Pos::new(i, k + 1) <= sel_end {
                    let c = if focused { p.text_selection } else { p.selection_inactive };
                    painter.rect_filled(img.rect, 0.0, c.gamma_multiply(0.6));
                }
            }
        }
        self.laid = laid;

        // ---- Pointer ----
        let pointer = ui.input(|i| i.pointer.interact_pos());
        let hover_link = resp.hovered().then(|| pointer.and_then(|pt| self.link_at(pt))).flatten();
        if let Some(_) = &hover_link {
            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        } else if resp.hovered() {
            let over_box = pointer.is_some_and(|pt| self.laid.iter().any(|l| l.checkbox.is_some_and(|r| r.contains(pt))) || self.image_at(pt).is_some());
            ui.ctx().set_cursor_icon(if over_box { egui::CursorIcon::Default } else { egui::CursorIcon::Text });
        }
        if resp.clicked() || resp.drag_started() || resp.secondary_clicked() {
            ui.memory_mut(|m| m.request_focus(id));
        }
        // A right-click puts the caret there (unless on the selection), so
        // its menu acts on what was clicked: the table cell, say.
        if resp.secondary_clicked() {
            if let Some(pt) = pointer {
                let at = self.pos_at(pt);
                if self.sel.is_empty() || at < self.sel.start() || at > self.sel.end() {
                    self.sel = Selection::caret(at);
                }
            }
        }
        let shift = ui.input(|i| i.modifiers.shift);
        if let Some(pt) = pointer {
            let pressed = ui.input(|i| i.pointer.primary_pressed());
            if pressed && resp.hovered() {
                let checkbox = self.laid.iter().position(|l| l.checkbox.is_some_and(|r| r.contains(pt)));
                if let Some(b) = checkbox {
                    if opts.read_only {
                        out.blocked_edit = true;
                    } else {
                        self.toggle_check_at(b, opts.sort_checked);
                    }
                } else {
                    let p = self.pos_at(pt);
                    self.set_caret(p, shift);
                    self.dragging = true;
                    self.goal_x = None;
                }
                self.last_activity = now;
            }
            if self.dragging && ui.input(|i| i.pointer.primary_down()) && resp.dragged() {
                let p = self.pos_at(pt);
                self.sel.caret = p;
                self.reveal = true;
            }
        }
        if !ui.input(|i| i.pointer.primary_down()) {
            self.dragging = false;
        }
        if resp.double_clicked() {
            if let Some(pt) = pointer {
                if let Some(img) = self.image_at(pt) {
                    // A picture opens in the system's viewer (not a locked
                    // note's: it would have to be written out decrypted).
                    let path = cleannotes_core::attachments::path(opts.images.dir(), &img.name);
                    if cleannotes_core::attachments::is_name(&img.name) && path.is_file() {
                        out.open_image = Some(path);
                    }
                } else {
                    let (a, b) = self.doc.word_at(self.pos_at(pt));
                    self.sel = Selection { anchor: a, caret: b };
                }
            }
        }
        if resp.triple_clicked() {
            let b = self.sel.caret.block;
            self.sel = Selection { anchor: Pos::new(b, 0), caret: Pos::new(b, self.doc.blocks[b].len()) };
        }
        if resp.clicked() && !shift {
            if let (Some(url), true) = (hover_link, self.sel.is_empty()) {
                out.open_url = Some(url);
            }
        }
        self.context_menu(&resp, opts, &ctx, &mut out);

        // ---- Caret ----
        if self.sel != sel_before || (self.doc.blocks.len(), self.undo_depth()) != doc_before_len {
            self.last_activity = now;
        }
        if let Some(r) = self.caret_rect(self.sel.caret) {
            let r = Rect::from_min_size(r.min, Vec2::new(2.0, r.height()));
            if focused {
                let since = now - self.last_activity;
                let visible = since < 0.5 || (since * 1.0).fract() < 0.55;
                if visible {
                    painter.rect_filled(r, 1.0, p.accent);
                }
                ui.ctx().request_repaint_after(std::time::Duration::from_millis(250));
                ctx.output_mut(|o| o.ime = Some(egui::output::IMEOutput { rect: resp.rect, cursor_rect: r }));
            }
            if self.reveal {
                ui.scroll_to_rect(r.expand(24.0), None);
                self.reveal = false;
            }
        }
        out
    }

    /// The link (explicit or a plain URL) under a point.
    fn link_at(&self, pt: Pos2) -> Option<String> {
        let i = self.laid.iter().position(|l| l.rect.contains(pt))?;
        let l = &self.laid[i];
        if !l.galley.rect.translate(l.origin.to_vec2()).contains(pt) {
            return None;
        }
        let c = l.galley.cursor_from_pos(pt - l.origin).ccursor.index;
        let b = &self.doc.blocks[i];
        // The character under the pointer is left or right of the cursor.
        let r = l.galley.pos_from_ccursor(CCursor::new(c)).translate(l.origin.to_vec2());
        let ci = if pt.x < r.left() && c > 0 { c - 1 } else { c };
        if let Some(url) = b.styles.get(ci).and_then(|s| s.link.as_deref()) {
            return Some(url.to_owned());
        }
        layout::auto_links(&b.text).into_iter().find(|(range, _)| range.contains(&ci)).map(|(_, u)| u)
    }

    fn context_menu(&mut self, resp: &egui::Response, opts: &Options, ctx: &egui::Context, out: &mut Output) {
        let has_sel = !self.sel.is_empty();
        let editable = !opts.read_only;
        resp.context_menu(|ui| {
            ui.set_min_width(200.0);
            let item = |ui: &mut egui::Ui, label: &str, shortcut: &str, enabled: bool| {
                ui.add_enabled(enabled, egui::Button::new(label).shortcut_text(shortcut)).clicked()
            };
            if item(ui, "Cut", cmd_key!("X"), has_sel && editable) {
                self.copy(ctx);
                self.delete_selected();
                ui.close_menu();
            }
            if item(ui, "Copy", cmd_key!("C"), has_sel) {
                self.copy(ctx);
                ui.close_menu();
            }
            if item(ui, "Paste", cmd_key!("V"), editable) {
                ctx.send_viewport_cmd(egui::ViewportCommand::RequestPaste);
                // (A picture on the clipboard is the app's to paste.)
                out.paste_image = opts.can_attach;
                ui.close_menu();
            }
            if item(ui, "Copy as Markdown", "", true) {
                self.copy_markdown(ctx);
                ui.close_menu();
            }
            ui.separator();
            if item(ui, "Select All", cmd_key!("A"), true) {
                self.command(Command::SelectAll);
                ui.close_menu();
            }
            ui.separator();
            ui.add_enabled_ui(editable, |ui| {
                ui.menu_button("Font", |ui| {
                    for (label, a, sc) in [
                        ("Bold", Attr::Bold, cmd_key!("B")),
                        ("Italic", Attr::Italic, cmd_key!("I")),
                        ("Underline", Attr::Underline, cmd_key!("U")),
                        ("Strikethrough", Attr::Strike, cmd_key!("Shift+X")),
                    ] {
                        if ui.add(egui::Button::new(label).shortcut_text(sc).selected(self.has_attr(a))).clicked() {
                            self.command(Command::ToggleAttr(a));
                            ui.close_menu();
                        }
                    }
                });
                if item(ui, "Add Link…", cmd_key!("K"), true) {
                    self.command(Command::EditLink);
                    ui.close_menu();
                }
                if item(ui, "Attach Image…", "", opts.can_attach) {
                    out.attach_image = true;
                    ui.close_menu();
                }
                let block = self.sel.caret.block;
                if let Kind::Cell { header_row, header_col, .. } = self.doc.blocks[block].kind {
                    ui.separator();
                    for (label, c) in [
                        ("Add Row Above", Command::AddRowAbove),
                        ("Add Row Below", Command::AddRowBelow),
                        ("Add Column Before", Command::AddColumnBefore),
                        ("Add Column After", Command::AddColumnAfter),
                    ] {
                        if item(ui, label, "", true) {
                            self.command(c);
                            ui.close_menu();
                        }
                    }
                    ui.separator();
                    for (label, c) in [("Delete Row", Command::DeleteRow), ("Delete Column", Command::DeleteColumn), ("Delete Table", Command::DeleteTable)] {
                        if item(ui, label, "", true) {
                            self.command(c);
                            ui.close_menu();
                        }
                    }
                    ui.separator();
                    if ui.add(egui::Button::new("Header Row").selected(header_row)).clicked() {
                        self.command(Command::ToggleHeaderRow);
                        ui.close_menu();
                    }
                    if ui.add(egui::Button::new("Header Column").selected(header_col)).clicked() {
                        self.command(Command::ToggleHeaderColumn);
                        ui.close_menu();
                    }
                } else if item(ui, "Table", cmd_key!("Alt+T"), true) {
                    self.command(Command::InsertTable);
                    ui.close_menu();
                }
            });
        });
    }
}

/// Paints the selection over characters `[from, to)` of a paragraph,
/// extending past the end of the text when the selection continues.
fn paint_range(painter: &egui::Painter, l: &Laid, from: usize, to: usize, past_end: bool, color: egui::Color32) {
    let mut start = 0;
    let rows = l.galley.rows.len();
    for (ri, row) in l.galley.rows.iter().enumerate() {
        let n = row.char_count_excluding_newline();
        let end = start + n;
        let (a, b) = (from.max(start), to.min(end));
        let last = ri + 1 == rows;
        // The selection goes on past this row (onto the next row or paragraph).
        let continues = (last && past_end) || (!last && to > end);
        if a < b || (continues && from <= end) {
            let xa = row.rect.left() + row.x_offset(a - start);
            let mut xb = row.rect.left() + row.x_offset(b.max(a) - start);
            if continues {
                xb += 6.0;
            }
            let r = Rect::from_min_max(Pos2::new(xa, row.rect.top()), Pos2::new(xb, row.rect.bottom()))
                .translate(l.origin.to_vec2());
            painter.rect_filled(r, 0.0, color);
        }
        start = end;
    }
}


#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_link_sheet_shows_the_link_where_the_selection_starts() {
        let mut e = Editor::new("n", "[link](https://example.com) text\nsecond line\n");
        // Dragged forwards, from inside the link into the next paragraph.
        e.sel = Selection { anchor: Pos::new(0, 1), caret: Pos::new(1, 3) };
        e.command(Command::EditLink);
        assert_eq!(e.link_sheet.as_ref().map(|s| s.url.as_str()), Some("https://example.com"));
    }

    #[test]
    fn arrows_without_an_up_to_date_layout_step_by_paragraph() {
        // No layout yet (or one from before paragraphs were removed).
        let e = Editor::new("n", "one\ntwo\nthree\n");
        assert_eq!(e.line_step(Pos::new(2, 2), 1, 0.0), e.doc.end());
        assert_eq!(e.line_step(Pos::new(1, 2), 1, 0.0), Pos::new(2, 0));
        assert_eq!(e.line_step(Pos::new(1, 2), -1, 0.0), Pos::new(0, 0));
        assert_eq!(e.row_edge(Pos::new(2, 1), true), Pos::new(2, 5));
    }

    #[test]
    fn tables_as_in_apple_notes() {
        let mut e = Editor::new("n", "Shopping\n");
        e.sel = Selection::caret(Pos::new(0, 8));
        e.command(Command::InsertTable);
        // After the paragraph, 3×3, the caret in the first cell.
        assert_eq!(e.sel.caret, Pos::new(1, 0));
        assert_eq!(e.doc.table_of(1), Some((1..10, 3)));
        e.insert_text("Eggs");
        // Tab moves along the row; Return is a line break in the cell.
        e.tab_in_table(false);
        e.insert_text("6");
        e.enter();
        e.insert_text("large");
        assert_eq!(e.doc.blocks[2].text, "6\nlarge");
        // Shift+Tab goes back; Tab in the last cell adds a row.
        e.tab_in_table(true);
        assert_eq!(e.sel.caret, Pos::new(1, 4));
        e.sel = Selection::caret(Pos::new(9, 0));
        e.tab_in_table(false);
        assert_eq!(e.doc.table_of(1), Some((1..13, 3)));
        assert_eq!(e.sel.caret, Pos::new(10, 0));
        // Row and column commands act on the caret's cell: the new row goes,
        // the caret moves up to the first column, a column after it comes.
        e.command(Command::DeleteRow);
        e.command(Command::AddColumnAfter);
        assert_eq!(e.doc.table_of(1), Some((1..13, 4)));
        // Saved as a Markdown table; undo takes it all back.
        let md = e.take_markdown().unwrap();
        assert!(md.starts_with("Shopping\n| | | | |\n| --- | --- | --- | --- |\n| Eggs |  | 6<br>large |  |\n"), "{md}");
        for _ in 0..20 {
            e.command(Command::Undo);
        }
        assert_eq!(e.doc.blocks.len(), 1);
    }

    #[test]
    fn pictures_are_laid_out_and_pointed_at() {
        let dir = tempfile::tempdir().unwrap();
        let png = images::png_from_rgba(40, 20, &[200; 40 * 20 * 4]).unwrap();
        let name = cleannotes_core::attachments::save(dir.path(), &png).unwrap();
        let cache = images::ImageCache::new(dir.path().to_owned());
        let mut e = Editor::new("n", &format!("# Pics\n![](attachments/{name})\nafter\n"));
        let ctx = egui::Context::default();
        crate::theme::install_fonts(&ctx);
        let input = || egui::RawInput { screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(800.0, 600.0))), ..Default::default() };
        // Decoded on a thread of its own: shown a few frames on.
        for _ in 0..300 {
            let _ = ctx.run(input(), |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    let p = crate::theme::Palette::get(ctx);
                    let opts = Options { text_size: 15.0, sort_checked: false, read_only: false, highlight: "", images: &cache, can_attach: true };
                    e.show(ui, &p, &opts, "");
                });
            });
            if matches!(e.laid[1].images[0].shown, images::Shown::Ready(..)) {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        let img = &e.laid[1].images[0];
        assert!(matches!(img.shown, images::Shown::Ready(_, s) if s == Vec2::new(40.0, 20.0)), "{:?}", img.shown);
        let r = img.rect;
        assert_eq!(r.size(), Vec2::new(40.0, 20.0));
        assert!(e.laid[2].rect.top() >= r.bottom(), "the text after it is below it");
        // Its left half puts the caret before it, its right half after.
        assert_eq!(e.pos_at(r.left_center() + Vec2::new(2.0, 0.0)), Pos::new(1, 0));
        assert_eq!(e.pos_at(r.right_center() - Vec2::new(2.0, 0.0)), Pos::new(1, 1));
        assert_eq!(e.caret_rect(Pos::new(1, 1)).unwrap().left(), r.right() + 1.0);
        assert_eq!(e.caret_rect(Pos::new(1, 0)).unwrap().height(), r.height());
        assert_eq!(e.row_edge(Pos::new(1, 0), true), Pos::new(1, 1));
        assert!(e.image_at(r.center()).is_some_and(|i| i.name == name));
        // A picture that isn't here shows as a box, waiting for it.
        let cache = images::ImageCache::new(dir.path().join("elsewhere"));
        assert_eq!(cache.get(&ctx, &name), images::Shown::Missing);
    }

    #[test]
    fn copy_puts_the_text_on_the_system_clipboard() {
        let mut e = Editor::new("n", "**bold** plain\n");
        e.sel = Selection { anchor: Pos::new(0, 0), caret: Pos::new(0, 4) };
        let ctx = egui::Context::default();
        let out = ctx.run(egui::RawInput::default(), |ctx| e.copy(ctx));
        assert!(out.platform_output.copied_text == "bold");
    }
}
