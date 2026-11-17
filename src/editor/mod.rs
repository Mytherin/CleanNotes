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

use crate::doc::{Attr, FileView, Kind, Pos, Selection};
use crate::icons::Icon;
use crate::menu;
use crate::theme::{self, Palette};
pub use cleannotes_core::editing::{Command, Editing};

pub mod images;
pub mod layout;
pub mod cards;
pub mod pdfs;
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
    /// The PDFs notes show.
    pub pdfs: &'a pdfs::PdfCache,
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
    /// An attached file's link was clicked: its reference and name.
    pub open_file: Option<(String, String)>,
    /// "Attach Image…" was chosen.
    pub attach_image: bool,
    /// "Attach File…" was chosen.
    pub attach_file: bool,
    /// Paste was asked for: a picture on the clipboard is the app's to
    /// paste (egui only brings text).
    pub paste_image: bool,
    /// Something to do with an attached file (its reference and name),
    /// from its card or menu.
    pub file_action: Option<(FileAction, String, String)>,
    /// Pictures (and nothing else) were copied: their references, for the
    /// app to put on the system clipboard as pictures and files.
    pub copy_images: Vec<String>,
}

/// What can be done with an attached file, by the app.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FileAction {
    /// In the system's app for it.
    Open,
    /// In CleanNotes' own viewer, all its pages (a PDF).
    QuickLook,
    /// The file on the clipboard (to paste in Explorer, a mail, …).
    Copy,
    /// A copy saved where the user picks ("Share…" in Apple Notes).
    SaveAs,
}

/// The "Rename Attachment" sheet: the attached file of paragraph `block`.
pub struct RenameSheet {
    pub block: usize,
    pub label: String,
    pub focus: bool,
}

/// A choice in an attached file's menu.
#[derive(Clone, Copy, Debug, PartialEq)]
enum MenuChoice {
    File(FileAction),
    View(FileView),
    Rename,
    Cut,
    Paste,
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
    /// How far each PDF's pages are scrolled sideways, and where to (‹ ›
    /// ease them there), by reference.
    pdf_scroll: std::collections::HashMap<String, (f32, f32)>,
    /// The PDFs whose thumbnails are shown, by reference.
    pdf_thumbs: std::collections::HashSet<String>,
    /// The attached file (its paragraph) right-clicked: its menu is shown.
    file_menu: Option<usize>,
    pub rename_sheet: Option<RenameSheet>,
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
        Editor { ed, goal_x: None, laid: Vec::new(), link_sheet: None, last_activity: 0.0, dragging: false, focus_requested: false, pdf_scroll: Default::default(), pdf_thumbs: Default::default(), file_menu: None, rename_sheet: None }
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

    fn copy(&mut self, ctx: &egui::Context, out: &mut Output) {
        let images = self.ed.selected_images();
        let Some(text) = self.ed.copy() else { return };
        if images.is_empty() {
            ctx.copy_text(text);
        } else {
            // (egui's clipboard only carries text: the app copies these.)
            out.copy_images = images;
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

    /// The attached file's card under a point, if any, and its paragraph.
    fn card_at(&self, pt: Pos2) -> Option<(usize, &cards::LaidFile)> {
        self.laid.iter().enumerate().find_map(|(i, l)| l.file.as_ref().filter(|f| f.card.contains(pt)).map(|f| (i, f)))
    }

    /// The card under a point, if it's on its pages (a small card's
    /// picture) or controls: something a click does something with.
    fn card_control_at(&self, pt: Pos2) -> Option<&cards::LaidFile> {
        self.card_at(pt).map(|(_, f)| f).filter(|f| f.pages_at(pt) || f.arrow_at(pt).is_some() || f.toggle_at(pt) || f.thumb_at(pt).is_some() || f.menu.contains(pt))
    }

    /// Sideways scrolling (a trackpad's, or Shift and the wheel) over a
    /// PDF's pages scrolls them; and scrolling set going by ‹ › eases on.
    fn scroll_pdfs(&mut self, ui: &egui::Ui) {
        let pointer = ui.input(|i| i.pointer.hover_pos());
        let dx = ui.input(|i| i.smooth_scroll_delta.x);
        let under = pointer.and_then(|pt| self.card_at(pt)).map(|(_, f)| f).filter(|f| f.max_scroll > 0.0);
        if let (Some(pdf), true) = (under, dx != 0.0) {
            let (reference, max) = (pdf.reference.clone(), pdf.max_scroll);
            let s = self.pdf_scroll.entry(reference).or_default();
            s.0 = (s.0 - dx).clamp(0.0, max);
            s.1 = s.0;
            // (Taken: the note doesn't scroll sideways too.)
            ui.ctx().input_mut(|i| i.smooth_scroll_delta.x = 0.0);
        }
        let dt = ui.input(|i| i.stable_dt).min(0.1);
        for s in self.pdf_scroll.values_mut() {
            if (s.1 - s.0).abs() > 0.5 {
                s.0 += (s.1 - s.0) * (1.0 - (-dt * 14.0).exp());
                ui.ctx().request_repaint();
            } else {
                s.0 = s.1;
            }
        }
    }

    /// Eases the pages of the card of paragraph `block` to where `to`
    /// says (from where they're headed now).
    fn scroll_card(&mut self, ctx: &egui::Context, block: usize, to: impl FnOnce(&cards::LaidFile, f32) -> f32) {
        let Some(f) = self.laid.get(block).and_then(|l| l.file.as_ref()) else { return };
        let s = self.pdf_scroll.get(&f.reference).copied().unwrap_or_default();
        let target = to(f, s.1);
        let reference = f.reference.clone();
        self.pdf_scroll.insert(reference, (s.0, target));
        ctx.request_repaint();
    }

    /// A press on a card's controls (‹ ›, "Show Thumbnails", a thumbnail):
    /// whether there was one.
    fn press_card(&mut self, ctx: &egui::Context, pt: Pos2) -> bool {
        let Some((block, f)) = self.card_at(pt) else { return false };
        if let Some(dir) = f.arrow_at(pt) {
            self.scroll_card(ctx, block, |f, t| f.step(t, dir));
        } else if f.toggle_at(pt) {
            let r = f.reference.clone();
            if !self.pdf_thumbs.remove(&r) {
                self.pdf_thumbs.insert(r);
            }
        } else if let Some(page) = f.thumb_at(pt) {
            self.scroll_card(ctx, block, |f, _| f.scroll_to(page));
        } else {
            return false;
        }
        true
    }

    /// What a click on a card's pages (or a small card's picture) does:
    /// Quick Look for a PDF that can be read, else open it.
    fn click_card(f: &cards::LaidFile) -> (FileAction, String, String) {
        let action = if f.pdf && (f.view != FileView::Small || !f.pages.is_empty()) { FileAction::QuickLook } else { FileAction::Open };
        (action, f.reference.clone(), f.label.clone())
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
        self.scroll_pdfs(ui);
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
                    Event::Copy => self.copy(&ctx, &mut out),
                    // egui reports Ctrl+Shift+X as Cut: it's Strikethrough.
                    Event::Cut if shift_down => {
                        if !opts.read_only {
                            self.command(Command::ToggleAttr(Attr::Strike));
                        }
                    }
                    Event::Cut => {
                        self.copy(&ctx, &mut out);
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
                        laid.push(Laid { rect, origin: rect.min + layout::CELL_PAD, galley, checkbox: None, images: Vec::new(), file: None });
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
                laid.push(Laid { rect, origin: Pos2::new(left + indent, y), galley, checkbox: None, images, file: None });
                y += h + 1.0;
                continue;
            }
            // An attached file: a card, its name the card's title.
            if let Some(reference) = self.doc.file_at(i) {
                let reference = reference.to_owned();
                let label = b.text.trim().to_owned();
                let col = (width - indent).max(160.0);
                let ext = cards::extension(&reference, &label);
                let pdf = cards::is_pdf_name(&reference, &label);
                let state = if pdf { opts.pdfs.get(&ctx, &reference) } else { pdfs::Pdf::Gone };
                let size = opts.pdfs.file_size(&reference);
                let kind = cards::kind_name(&ext);
                let view = match state {
                    pdfs::Pdf::Gone => FileView::Small,
                    _ => self.doc.file_view(i),
                };
                let details = match (size, view) {
                    (None, _) => format!("{kind} · Not on this device yet"),
                    (Some(n), FileView::Small) => format!("{kind} · {}", cards::size_text(n)),
                    (Some(n), _) => cards::size_text(n),
                };
                // The name is plain text, bold, in the card (the card says
                // it opens).
                let (wrap, font) = match view {
                    FileView::Small => (cards::LaidFile::small_text_w(col), theme::bold_font(body)),
                    _ => (cards::LaidFile::pages_text_w(col), theme::bold_font(cards::TITLE_SIZE)),
                };
                let mut job = layout::job(b, body, p, wrap.max(40.0), &marks, false);
                for s in &mut job.sections {
                    s.format.color = p.text;
                    s.format.underline = egui::Stroke::NONE;
                    s.format.font_id = font.clone();
                }
                let galley = ui.fonts(|f| f.layout_job(job));
                let pos = Pos2::new(left + indent, y + cards::MARGIN);
                let first_row = galley.rows.first().map_or(Vec2::ZERO, |r| r.rect.size());
                let mut card = match (&state, view) {
                    (_, FileView::Small) => {
                        let first = match &state {
                            pdfs::Pdf::Ready(sizes) => sizes.first().copied(),
                            _ => None,
                        };
                        cards::LaidFile::small(reference, label, pdf, details, ext, pos, col, galley.rect.size(), first)
                    }
                    (state, view) => {
                        let sizes = match state {
                            pdfs::Pdf::Ready(sizes) => Some(sizes.clone()),
                            _ => None,
                        };
                        let scroll = self.pdf_scroll.get(&reference).map_or(0.0, |s| s.0);
                        let thumbs = self.pdf_thumbs.contains(&reference);
                        cards::LaidFile::pages(reference, label, details, view, pos, col, sizes.as_deref(), first_row, galley.rect.height(), scroll, thumbs)
                    }
                };
                let res = match view {
                    FileView::Small => pdfs::Res::Thumb,
                    FileView::Medium => pdfs::Res::Card,
                    FileView::Large => pdfs::Res::Full,
                };
                for pg in &mut card.pages {
                    pg.tex = opts.pdfs.page(&ctx, &card.reference, pg.index, res);
                }
                for t in &mut card.thumbs {
                    t.tex = opts.pdfs.page(&ctx, &card.reference, t.index, pdfs::Res::Thumb);
                }
                let h = card.card.height() + 2.0 * cards::MARGIN;
                let rect = Rect::from_min_size(Pos2::new(left, y), Vec2::new(width, h));
                laid.push(Laid { rect, origin: card.text_origin, galley, checkbox: None, images: Vec::new(), file: Some(card) });
                y += h + 1.0;
                continue;
            }
            let job = layout::job(b, body, p, width - indent, &marks, false);
            let galley = ui.fonts(|f| f.layout_job(job));
            let mono_pad = if b.kind == Kind::Monospaced { 2.0 } else { 0.0 };
            let origin = Pos2::new(left + indent, y + mono_pad);
            let h = galley.rect.height().max(layout::size(b.kind, body) * 1.2) + 2.0 * mono_pad;
            let rect = Rect::from_min_size(Pos2::new(left, y), Vec2::new(width, h));
            laid.push(Laid { rect, origin, galley, checkbox: None, images: Vec::new(), file: None });
            y += h + 1.0;
        }
        // Scrolled no further than the pages reach (the window narrowed,
        // say).
        // (Not by a small card of the same PDF: it doesn't scroll.)
        for card in laid.iter().filter_map(|l| l.file.as_ref()).filter(|f| f.view != FileView::Small) {
            if let Some(s) = self.pdf_scroll.get_mut(&card.reference) {
                s.0 = s.0.clamp(0.0, card.max_scroll);
                s.1 = s.1.clamp(0.0, card.max_scroll);
            }
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
        // Attached files' cards, under their names (and the selection of
        // them). (Hover by the pointer itself: the ⌄ button takes it from
        // the editor's response.)
        let hover = ui.ctx().pointer_hover_pos().filter(|pt| ui.clip_rect().contains(*pt));
        for (i, l) in laid.iter().enumerate() {
            let Some(file) = &l.file else { continue };
            cards::paint(&painter, file, p, hover);
            // Selected with all of its name: the card is tinted too.
            let b = &self.doc.blocks[i];
            if !self.sel.is_empty() && self.sel.start() <= Pos::new(i, 0) && Pos::new(i, b.len()) <= self.sel.end() {
                let c = if focused { p.text_selection } else { p.selection_inactive };
                painter.rect_filled(file.card, 12.0, c.gamma_multiply(0.35));
            }
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
        if hover_link.is_some() || (resp.hovered() && pointer.is_some_and(|pt| self.card_control_at(pt).is_some())) {
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
            // On an attached file's card: its menu, not the text's.
            self.file_menu = pointer.and_then(|pt| self.card_at(pt)).map(|(i, _)| i);
            if let Some(pt) = pointer.filter(|_| self.file_menu.is_none()) {
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
                if self.press_card(&ctx, pt) || self.card_control_at(pt).is_some() {
                    // ‹ ›, "Show Thumbnails", a thumbnail: done; pages: a
                    // click opens them (the caret stays).
                } else if let Some(b) = checkbox {
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
                match cleannotes_core::attachments::file_link_ref(&url) {
                    // An attached file: opened by the app, with its name.
                    Some(r) => {
                        let label = pointer.map(|pt| self.link_text_at(pt)).unwrap_or_default();
                        out.open_file = Some((r.to_owned(), label));
                    }
                    None => out.open_url = Some(url),
                }
            } else if let Some(pt) = pointer {
                // A card's pages (a small card's picture): Quick Look.
                if let Some((_, f)) = self.card_at(pt).filter(|(_, f)| f.pages_at(pt)) {
                    out.file_action = Some(Self::click_card(f));
                }
            }
        }
        self.card_menus(ui, opts, &mut out);
        self.context_menu(&resp, opts, &ctx, &mut out);

        // ---- Caret ----
        if self.sel != sel_before || (self.doc.blocks.len(), self.undo_depth()) != doc_before_len {
            self.last_activity = now;
        }
        if let Some(r) = self.caret_rect(self.sel.caret) {
            let r = Rect::from_min_size(r.min, Vec2::new(2.0, r.height()));
            // In a window that isn't active the caret isn't shown (as on
            // the Mac), so nothing is drawn again just to blink it.
            let active_window = ui.input(|i| i.focused);
            if focused && active_window {
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
    /// The paragraph and character under the pointer, if it's over text.
    fn char_at(&self, pt: Pos2) -> Option<(usize, usize)> {
        let i = self.laid.iter().position(|l| l.rect.contains(pt))?;
        let l = &self.laid[i];
        if !l.galley.rect.translate(l.origin.to_vec2()).contains(pt) {
            return None;
        }
        let c = l.galley.cursor_from_pos(pt - l.origin).ccursor.index;
        // The character under the pointer is left or right of the cursor.
        let r = l.galley.pos_from_ccursor(CCursor::new(c)).translate(l.origin.to_vec2());
        Some((i, if pt.x < r.left() && c > 0 { c - 1 } else { c }))
    }

    /// The text of the link under the pointer (an attached file's name).
    fn link_text_at(&self, pt: Pos2) -> String {
        let Some((i, ci)) = self.char_at(pt) else { return String::new() };
        let b = &self.doc.blocks[i];
        let Some(link) = b.styles.get(ci).and_then(|s| s.link.clone()) else { return String::new() };
        let same = |j: &usize| b.styles[*j].link.as_ref() == Some(&link);
        let start = (0..=ci).rev().take_while(same).last().unwrap_or(ci);
        let end = (ci..b.styles.len()).take_while(same).last().map_or(ci, |j| j + 1);
        b.text.chars().skip(start).take(end - start).collect()
    }

    fn link_at(&self, pt: Pos2) -> Option<String> {
        let (i, ci) = self.char_at(pt)?;
        let b = &self.doc.blocks[i];
        if let Some(url) = b.styles.get(ci).and_then(|s| s.link.as_deref()) {
            return Some(url.to_owned());
        }
        layout::auto_links(&b.text).into_iter().find(|(range, _)| range.contains(&ci)).map(|(_, u)| u)
    }

    /// What an attached file's menu shows for the card of paragraph
    /// `block`: whether Quick Look is there, and its view (`None`: no View
    /// As, for a file that isn't a PDF).
    fn menu_info(&self, block: usize) -> Option<(bool, Option<FileView>)> {
        let f = self.laid.get(block)?.file.as_ref()?;
        let readable = f.pdf && (f.view != FileView::Small || !f.pages.is_empty());
        let view = f.pdf.then(|| self.doc.file_view(block));
        Some((readable, view))
    }

    /// An attached file's menu, as in Apple Notes (less Markup; Share… is
    /// Save As…: Windows has no share sheet to hand a file to).
    fn attachment_menu(ui: &mut egui::Ui, quick_look: bool, view: Option<FileView>, editable: bool) -> Option<MenuChoice> {
        menu::begin(ui, 220.0);
        let mut choice = None;
        let mut item = |ui: &mut egui::Ui, label: &str, icon: Icon, enabled: bool, c: MenuChoice| {
            if menu::item(label).icon(icon).enabled(enabled).show(ui).clicked() {
                choice = Some(c);
            }
        };
        item(ui, "Cut", Icon::Cut, editable, MenuChoice::Cut);
        item(ui, "Copy", Icon::Copy, true, MenuChoice::File(FileAction::Copy));
        item(ui, "Paste", Icon::Paste, editable, MenuChoice::Paste);
        menu::separator(ui);
        item(ui, "Save As…", Icon::Share, true, MenuChoice::File(FileAction::SaveAs));
        menu::separator(ui);
        if let Some(current) = view {
            let chosen = menu::submenu(ui, "View As", Some(Icon::ViewAs), editable, |ui| {
                menu::begin(ui, 120.0);
                let mut chosen = None;
                for (label, v) in [("Small", FileView::Small), ("Medium", FileView::Medium), ("Large", FileView::Large)] {
                    if menu::item(label).checked(v == current).show(ui).clicked() {
                        chosen = Some(MenuChoice::View(v));
                    }
                }
                chosen
            });
            if let Some(Some(c)) = chosen {
                choice = Some(c);
            }
        }
        let mut item = |ui: &mut egui::Ui, label: &str, icon: Icon, enabled: bool, c: MenuChoice| {
            if menu::item(label).icon(icon).enabled(enabled).show(ui).clicked() {
                choice = Some(c);
            }
        };
        item(ui, "Rename Attachment…", Icon::Rename, editable, MenuChoice::Rename);
        menu::separator(ui);
        item(ui, "Quick Look Attachment", Icon::Eye, quick_look, MenuChoice::File(FileAction::QuickLook));
        item(ui, "Open Attachment", Icon::OpenApp, true, MenuChoice::File(FileAction::Open));
        choice
    }

    /// Does what was chosen in the menu of the attached file of paragraph
    /// `block`.
    fn apply_menu_choice(&mut self, ctx: &egui::Context, block: usize, choice: MenuChoice, opts: &Options, out: &mut Output) {
        let Some(f) = self.laid.get(block).and_then(|l| l.file.as_ref()) else { return };
        let (reference, label) = (f.reference.clone(), f.label.clone());
        let editable = !opts.read_only;
        match choice {
            MenuChoice::File(action) => out.file_action = Some((action, reference, label)),
            // The file on the clipboard (pasting it attaches it again), and
            // gone from the note.
            MenuChoice::Cut if editable => {
                out.file_action = Some((FileAction::Copy, reference, label));
                self.command(Command::DeleteFile { block });
            }
            // On the line after the card, where the caret goes after
            // attaching a file.
            MenuChoice::Paste if editable => {
                let at = if block + 1 < self.doc.blocks.len() { Pos::new(block + 1, 0) } else { Pos::new(block, self.doc.blocks[block].len()) };
                self.sel = Selection::caret(at);
                ctx.send_viewport_cmd(egui::ViewportCommand::RequestPaste);
                out.paste_image = opts.can_attach;
            }
            MenuChoice::View(view) if editable => self.command(Command::SetFileView { block, view }),
            MenuChoice::Rename if editable => self.rename_sheet = Some(RenameSheet { block, label, focus: true }),
            _ => out.blocked_edit = true,
        }
    }

    /// The ⌄ on each card: its menu, on a click.
    fn card_menus(&mut self, ui: &mut egui::Ui, opts: &Options, out: &mut Output) {
        let editable = !opts.read_only;
        let buttons: Vec<(usize, Rect)> = self.laid.iter().enumerate().filter_map(|(i, l)| l.file.as_ref().map(|f| (i, f.menu))).collect();
        for (block, rect) in buttons {
            let Some((quick_look, view)) = self.menu_info(block) else { continue };
            // (Drawn by the card: the button is only where it's clicked.)
            let mut child = ui.new_child(egui::UiBuilder::new().max_rect(rect).id_salt(("file-menu", block)));
            let button = egui::Button::new("").frame(false).min_size(rect.size());
            let chosen = egui::menu::menu_custom_button(&mut child, button, |ui| Self::attachment_menu(ui, quick_look, view, editable));
            if let Some(Some(choice)) = chosen.inner {
                let ctx = ui.ctx().clone();
                self.apply_menu_choice(&ctx, block, choice, opts, out);
            }
        }
    }

    fn context_menu(&mut self, resp: &egui::Response, opts: &Options, ctx: &egui::Context, out: &mut Output) {
        // Right-clicked on an attached file's card: its menu.
        if let Some(block) = self.file_menu {
            let Some((quick_look, view)) = self.menu_info(block) else {
                self.file_menu = None;
                return;
            };
            let mut chosen = None;
            resp.context_menu(|ui| chosen = Self::attachment_menu(ui, quick_look, view, !opts.read_only));
            if let Some(choice) = chosen {
                self.apply_menu_choice(ctx, block, choice, opts, out);
                self.file_menu = None;
            }
            return;
        }
        let has_sel = !self.sel.is_empty();
        let editable = !opts.read_only;
        resp.context_menu(|ui| {
            menu::begin(ui, 220.0);
            if menu::item("Cut").icon(Icon::Cut).shortcut(cmd_key!("X")).enabled(has_sel && editable).show(ui).clicked() {
                self.copy(ctx, out);
                self.delete_selected();
            }
            if menu::item("Copy").icon(Icon::Copy).shortcut(cmd_key!("C")).enabled(has_sel).show(ui).clicked() {
                self.copy(ctx, out);
            }
            if menu::item("Paste").icon(Icon::Paste).shortcut(cmd_key!("V")).enabled(editable).show(ui).clicked() {
                ctx.send_viewport_cmd(egui::ViewportCommand::RequestPaste);
                // (A picture on the clipboard is the app's to paste.)
                out.paste_image = opts.can_attach;
            }
            if menu::item("Copy as Markdown").show(ui).clicked() {
                self.copy_markdown(ctx);
            }
            menu::separator(ui);
            if menu::item("Select All").shortcut(cmd_key!("A")).show(ui).clicked() {
                self.command(Command::SelectAll);
            }
            menu::separator(ui);
            menu::submenu(ui, "Font", Some(Icon::Format), editable, |ui| {
                menu::begin(ui, 180.0);
                for (label, a, sc) in [
                    ("Bold", Attr::Bold, cmd_key!("B")),
                    ("Italic", Attr::Italic, cmd_key!("I")),
                    ("Underline", Attr::Underline, cmd_key!("U")),
                    ("Strikethrough", Attr::Strike, cmd_key!("Shift+X")),
                ] {
                    if menu::item(label).shortcut(sc).checked(self.has_attr(a)).show(ui).clicked() {
                        self.command(Command::ToggleAttr(a));
                    }
                }
            });
            if menu::item("Add Link…").icon(Icon::Link).shortcut(cmd_key!("K")).enabled(editable).show(ui).clicked() {
                self.command(Command::EditLink);
            }
            if menu::item("Attach Image…").icon(Icon::Photo).enabled(editable && opts.can_attach).show(ui).clicked() {
                out.attach_image = true;
            }
            if menu::item("Attach File…").icon(Icon::Paperclip).enabled(editable && opts.can_attach).show(ui).clicked() {
                out.attach_file = true;
            }
            let block = self.sel.caret.block;
            if let Kind::Cell { header_row, header_col, .. } = self.doc.blocks[block].kind {
                menu::separator(ui);
                for (label, c) in [
                    ("Add Row Above", Command::AddRowAbove),
                    ("Add Row Below", Command::AddRowBelow),
                    ("Add Column Before", Command::AddColumnBefore),
                    ("Add Column After", Command::AddColumnAfter),
                ] {
                    if menu::item(label).enabled(editable).show(ui).clicked() {
                        self.command(c);
                    }
                }
                menu::separator(ui);
                for (label, c) in [("Delete Row", Command::DeleteRow), ("Delete Column", Command::DeleteColumn), ("Delete Table", Command::DeleteTable)] {
                    if menu::item(label).enabled(editable).show(ui).clicked() {
                        self.command(c);
                    }
                }
                menu::separator(ui);
                if menu::item("Header Row").checked(header_row).enabled(editable).show(ui).clicked() {
                    self.command(Command::ToggleHeaderRow);
                }
                if menu::item("Header Column").checked(header_col).enabled(editable).show(ui).clicked() {
                    self.command(Command::ToggleHeaderColumn);
                }
            } else if menu::item("Table").icon(Icon::Table).shortcut(cmd_key!("Alt+T")).enabled(editable).show(ui).clicked() {
                self.command(Command::InsertTable);
            }
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
    fn typing_star_space_starts_a_bulleted_list() {
        let dir = tempfile::tempdir().unwrap();
        let cache = images::ImageCache::new(dir.path().to_owned());
        let mut e = Editor::new("n", "Title\n\n");
        e.sel = Selection::caret(Pos::new(1, 0));
        e.focus_requested = true;
        let ctx = egui::Context::default();
        crate::theme::install_fonts(&ctx);
        let shift = Modifiers::SHIFT;
        let key = |key, modifiers| Event::Key { key, physical_key: None, pressed: true, repeat: false, modifiers };
        let frames = [
            vec![],
            vec![key(Key::Num8, shift), Event::Text("*".into())],
            vec![key(Key::Space, Modifiers::NONE), Event::Text(" ".into())],
        ];
        for events in frames {
            let input = egui::RawInput { screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(800.0, 600.0))), events, ..Default::default() };
            let _ = ctx.run(input, |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    let p = crate::theme::Palette::get(ctx);
                    let opts = Options { text_size: 15.0, sort_checked: false, read_only: false, highlight: "", images: &cache, pdfs: &pdfs::PdfCache::new(cache.dir().to_owned()), can_attach: true };
                    e.show(ui, &p, &opts, "");
                });
            });
        }
        assert_eq!((e.doc.blocks[1].kind, e.doc.blocks[1].text.as_str()), (Kind::Bulleted, ""));
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
                    let opts = Options { text_size: 15.0, sort_checked: false, read_only: false, highlight: "", images: &cache, pdfs: &pdfs::PdfCache::new(cache.dir().to_owned()), can_attach: true };
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
    fn attached_files_are_cards_as_in_apple_notes() {
        let dir = tempfile::tempdir().unwrap();
        let pdf = pdfs::tests::test_pdf(&[(612, 792); 12]);
        let name = cleannotes_core::attachments::save_file(dir.path(), &pdf, "trip.pdf").unwrap();
        let doc = cleannotes_core::attachments::save_file(dir.path(), b"PK\x03\x04", "notes.docx").unwrap();
        let cache = images::ImageCache::new(dir.path().to_owned());
        let pdf_cache = pdfs::PdfCache::new(dir.path().to_owned());
        let md = format!("# Trip\n[trip.pdf](attachments/{name})\n[notes.docx](attachments/{doc})\nsee [trip.pdf](attachments/{name})\n[pass.pdf](attachments/{name} \"small\")\nafter\n");
        let mut e = Editor::new("n", &md);
        let ctx = egui::Context::default();
        crate::theme::install_fonts(&ctx);
        let input = |events: Vec<egui::Event>| egui::RawInput { screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(800.0, 1400.0))), events, ..Default::default() };
        let frame = |e: &mut Editor, input: egui::RawInput| {
            let mut out = Output::default();
            let _ = ctx.run(input, |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    let p = crate::theme::Palette::get(ctx);
                    let opts = Options { text_size: 15.0, sort_checked: false, read_only: false, highlight: "", images: &cache, pdfs: &pdf_cache, can_attach: true };
                    out = e.show(ui, &p, &opts, "");
                });
            });
            out
        };
        let card = |e: &Editor, i: usize| e.laid[i].file.as_ref().unwrap().clone();
        // Read and drawn on threads of their own: shown a few frames on.
        pdfs::tests::wait(|| {
            frame(&mut e, input(vec![]));
            let ready = |i: usize| e.laid[i].file.as_ref().is_some_and(|f| !f.pages.is_empty() && f.pages.iter().all(|pg| pg.tex.is_some()));
            (ready(1) && ready(4)).then_some(())
        });
        // Medium, unless set: the name at the top, big pages below.
        let c = card(&e, 1);
        assert_eq!((c.view, c.reference.as_str(), c.label.as_str()), (FileView::Medium, name.as_str(), "trip.pdf"));
        assert_eq!(c.strip.height(), cards::MEDIUM_PAGE_H);
        assert_eq!(e.laid[1].origin, c.text_origin);
        assert!(c.text_origin.y < c.strip.top() && e.laid[2].rect.top() > c.card.bottom());
        assert!(c.pages.len() < 12 && c.arrow(-1).is_none() && c.arrow(1).is_some());
        // Other files: small cards, saying what they are; a PDF linked in
        // text is a link; one set small shows its first page in the card.
        let d = card(&e, 2);
        assert_eq!(d.view, FileView::Small);
        assert!(d.details.starts_with("Word Document · ") && d.pages.is_empty(), "{}", d.details);
        assert!(e.laid[3].file.is_none());
        let s = card(&e, 4);
        assert_eq!((s.view, s.pages.len()), (FileView::Small, 1));
        assert!(s.details.starts_with("PDF Document · ") && s.details.ends_with(" KB"), "{}", s.details);
        assert!(s.strip.contains(s.pages[0].rect.center()) && s.menu.right() < s.strip.left());

        // Shift and the wheel over the pages scroll them sideways.
        let wheel = egui::Event::MouseWheel { unit: egui::MouseWheelUnit::Point, delta: Vec2::new(0.0, -120.0), modifiers: Modifiers::SHIFT };
        frame(&mut e, input(vec![egui::Event::PointerMoved(c.strip.center()), wheel]));
        for _ in 0..30 {
            frame(&mut e, input(vec![]));
        }
        let scrolled = card(&e, 1).scroll;
        assert!(scrolled > 50.0, "{scrolled}");
        let click = |pos, pressed| egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed, modifiers: Default::default() };
        let press = |e: &mut Editor, at: Pos2| {
            frame(e, input(vec![egui::Event::PointerMoved(at), click(at, true)]));
            let out = frame(e, input(vec![click(at, false)]));
            for _ in 0..120 {
                frame(e, input(vec![]));
            }
            out
        };
        // › scrolls on to a page's start, without opening it or moving the
        // caret.
        let at = card(&e, 1).arrow(1).unwrap().center();
        let out = press(&mut e, at);
        assert!(out.file_action.is_none());
        let c = card(&e, 1);
        assert!(c.scroll > scrolled + 200.0 && c.starts.contains(&c.scroll), "{}", c.scroll);
        assert_eq!(e.sel, Selection::caret(Pos::new(0, 0)));
        // Show Thumbnails, and one clicked goes to its page.
        let at = card(&e, 1).toggle.unwrap().center();
        press(&mut e, at);
        let c = card(&e, 1);
        assert!(c.thumbs_shown && !c.thumbs.is_empty());
        let t = c.thumbs.iter().find(|t| t.index == 1).unwrap().rect.center();
        press(&mut e, t);
        let c = card(&e, 1);
        assert_eq!((c.scroll, c.current), (c.starts[1], 1));
        // A click on the pages: Quick Look.
        let page = c.pages.iter().map(|pg| pg.rect.center()).find(|pt| c.pages_at(*pt)).unwrap();
        let out = press(&mut e, page);
        assert_eq!(out.file_action, Some((FileAction::QuickLook, name.clone(), "trip.pdf".to_owned())));
        // The ⌄ takes the click (its menu): nothing else happens.
        let at = card(&e, 1).menu.center();
        let out = press(&mut e, at);
        assert!(out.file_action.is_none() && out.open_file.is_none());

        // The menu's choices.
        let opts_cache = (images::ImageCache::new(dir.path().to_owned()), pdfs::PdfCache::new(dir.path().to_owned()));
        let opts = Options { text_size: 15.0, sort_checked: false, read_only: false, highlight: "", images: &opts_cache.0, pdfs: &opts_cache.1, can_attach: true };
        let mut out = Output::default();
        e.apply_menu_choice(&egui::Context::default(), 1, MenuChoice::View(FileView::Large), &opts, &mut out);
        assert!(e.take_markdown().unwrap().contains(&format!("[trip.pdf](attachments/{name} \"large\")")));
        e.apply_menu_choice(&egui::Context::default(), 2, MenuChoice::File(FileAction::Copy), &opts, &mut out);
        assert_eq!(out.file_action, Some((FileAction::Copy, doc.clone(), "notes.docx".to_owned())));
        e.apply_menu_choice(&egui::Context::default(), 2, MenuChoice::Rename, &opts, &mut out);
        assert!(e.rename_sheet.as_ref().is_some_and(|s| s.block == 2 && s.label == "notes.docx"));
        // Paste: on the line after the card.
        let ctx = egui::Context::default();
        let mut out = Output::default();
        e.apply_menu_choice(&ctx, 1, MenuChoice::Paste, &opts, &mut out);
        assert!(out.paste_image && e.sel == Selection::caret(Pos::new(2, 0)));
        // Cut: the file on the clipboard, and gone.
        let mut out = Output::default();
        e.apply_menu_choice(&ctx, 2, MenuChoice::Cut, &opts, &mut out);
        assert_eq!(out.file_action, Some((FileAction::Copy, doc.clone(), "notes.docx".to_owned())));
        assert!(!e.take_markdown().unwrap().contains("notes.docx"));
        // Not in a deleted note.
        let ro = Options { read_only: true, ..opts };
        let mut out = Output::default();
        e.apply_menu_choice(&ctx, 1, MenuChoice::Cut, &ro, &mut out);
        assert!(out.blocked_edit && out.file_action.is_none() && e.take_markdown().is_none());
    }

    #[test]
    fn copy_puts_the_text_on_the_system_clipboard() {
        let mut e = Editor::new("n", "**bold** plain\n");
        e.sel = Selection { anchor: Pos::new(0, 0), caret: Pos::new(0, 4) };
        let ctx = egui::Context::default();
        let mut copied = Output::default();
        let out = ctx.run(egui::RawInput::default(), |ctx| e.copy(ctx, &mut copied));
        assert!(out.platform_output.copied_text == "bold");
        assert!(copied.copy_images.is_empty());
    }

    #[test]
    fn copied_pictures_are_the_apps_to_put_on_the_clipboard() {
        let name = cleannotes_core::attachments::name_for(b"pic", "png");
        let mut e = Editor::new("n", &format!("day\n![](attachments/{name})\n"));
        e.sel = Selection { anchor: Pos::new(1, 0), caret: Pos::new(1, 1) };
        let ctx = egui::Context::default();
        let mut copied = Output::default();
        let out = ctx.run(egui::RawInput::default(), |ctx| e.copy(ctx, &mut copied));
        assert_eq!(copied.copy_images, vec![name]);
        // (Not as text: that would replace them.)
        assert!(out.platform_output.copied_text.is_empty());
    }
}
