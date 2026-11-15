//! The note editor: a rich-text editor over a [`Doc`], with Apple Notes'
//! paragraph styles, checklists, keyboard shortcuts, undo, find, links and
//! a clipboard that keeps formatting within the app.
//!
//! The note's Markdown is only produced when the app asks for it
//! ([`Editor::markdown`]); while open, the note is a `Doc`.

use std::collections::HashMap;

use eframe::egui::{self, text::CCursor, Event, Key, Modifiers, Pos2, Rect, Sense, Vec2};

use crate::doc::{Attr, Block, Doc, Kind, Pos, Selection, Style};
use crate::markdown;
use crate::theme::Palette;

pub mod layout;
use layout::{Laid, Marks};

/// Things the editor can be asked to do (from the Format menu, toolbar,
/// shortcuts or the right-click menu).
#[derive(Clone, Debug, PartialEq)]
pub enum Command {
    SetKind(Kind),
    ToggleAttr(Attr),
    /// "Mark as Checked".
    ToggleChecked,
    Indent,
    Outdent,
    MoveUp,
    MoveDown,
    /// Open the link sheet for the selection.
    EditLink,
    SetLink(Option<String>),
    Undo,
    Redo,
    SelectAll,
    CopyMarkdown,
}

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
}

/// What happened in a frame, for the app.
#[derive(Default)]
pub struct Output {
    /// The user wants to edit a read-only note.
    pub blocked_edit: bool,
    pub open_url: Option<String>,
}

#[derive(Clone)]
struct State {
    doc: Doc,
    sel: Selection,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Group {
    Typing,
    Deleting,
}

/// Find in note (Ctrl+F).
#[derive(Default)]
pub struct Find {
    pub open: bool,
    pub query: String,
    pub focus: bool,
    /// The current match's index in [`Editor::matches`].
    pub current: usize,
}

/// The "Add Link" sheet.
pub struct LinkSheet {
    pub url: String,
    pub focus: bool,
}

pub struct Editor {
    pub note_id: String,
    pub doc: Doc,
    pub sel: Selection,
    /// The style to type in after Bold etc. was pressed with nothing selected.
    typing_style: Option<Style>,
    undo: Vec<State>,
    redo: Vec<State>,
    group: Option<(Group, f64)>,
    /// The x position vertical moves aim for.
    goal_x: Option<f32>,
    laid: Vec<Laid>,
    /// Changed since the app last took the Markdown.
    changed: bool,
    /// The Markdown the editor was loaded with or last produced.
    pub markdown_seen: String,
    pub find: Find,
    pub link_sheet: Option<LinkSheet>,
    scroll_to_caret: bool,
    /// Last time the caret moved or text changed (the caret shows steadily
    /// right after).
    last_activity: f64,
    dragging: bool,
    /// Formatted copy of what was last copied, matched up with the clipboard
    /// text when pasting.
    clipboard: Option<(String, Vec<Block>)>,
    pub focus_requested: bool,
}

const COALESCE_SECS: f64 = 1.5;

impl Editor {
    pub fn new(note_id: &str, md: &str) -> Editor {
        let doc = markdown::from_markdown(md);
        Editor {
            note_id: note_id.to_owned(),
            doc,
            sel: Selection::default(),
            typing_style: None,
            undo: Vec::new(),
            redo: Vec::new(),
            group: None,
            goal_x: None,
            laid: Vec::new(),
            changed: false,
            markdown_seen: md.to_owned(),
            find: Find::default(),
            link_sheet: None,
            scroll_to_caret: false,
            last_activity: 0.0,
            dragging: false,
            clipboard: None,
            focus_requested: false,
        }
    }

    /// A new note: one empty paragraph in the "New notes start with" style.
    pub fn new_note(note_id: &str, start: Kind) -> Editor {
        let mut e = Editor::new(note_id, "");
        e.doc.blocks[0].kind = start;
        e.markdown_seen = String::new();
        e
    }

    pub fn id(&self) -> egui::Id {
        egui::Id::new(("note-editor", &self.note_id))
    }

    /// The note's Markdown, if it changed since last asked.
    pub fn take_markdown(&mut self) -> Option<String> {
        if !self.changed {
            return None;
        }
        self.changed = false;
        let md = markdown::to_markdown(&self.doc);
        if md == self.markdown_seen {
            return None;
        }
        self.markdown_seen = md.clone();
        Some(md)
    }

    /// The note changed elsewhere (a sync): load it, keeping the caret near
    /// where it was.
    pub fn reload(&mut self, md: &str) {
        if md == self.markdown_seen {
            return;
        }
        self.checkpoint(None);
        self.doc = markdown::from_markdown(md);
        self.markdown_seen = md.to_owned();
        self.sel = Selection { anchor: self.doc.clamp(self.sel.anchor), caret: self.doc.clamp(self.sel.caret) };
    }

    /// The paragraph style at the caret (for the Format menu's checkmark).
    pub fn current_kind(&self) -> Kind {
        self.doc.blocks[self.sel.caret.block.min(self.doc.blocks.len() - 1)].kind
    }

    /// Whether the selection (or typing style) has a character attribute.
    pub fn has_attr(&self, a: Attr) -> bool {
        if self.sel.is_empty() {
            self.typing_style.clone().unwrap_or_else(|| self.doc.style_at(self.sel.caret)).get(a)
        } else {
            self.doc.has_attr(self.sel.start(), self.sel.end(), a)
        }
    }

    // ---- Undo ---------------------------------------------------------

    /// Saves the state for undo before a change. Typing (or deleting) in a
    /// row is undone as one step.
    fn checkpoint(&mut self, group: Option<Group>) {
        let now = self.last_activity;
        let joins = match (group, self.group) {
            (Some(g), Some((last, t))) => g == last && now - t < COALESCE_SECS,
            _ => false,
        };
        if !joins {
            self.undo.push(State { doc: self.doc.clone(), sel: self.sel });
            if self.undo.len() > 500 {
                self.undo.remove(0);
            }
        }
        self.group = group.map(|g| (g, now));
        self.redo.clear();
        self.changed = true;
        self.scroll_to_caret = true;
    }

    fn undo(&mut self) {
        if let Some(s) = self.undo.pop() {
            self.redo.push(State { doc: std::mem::replace(&mut self.doc, s.doc), sel: self.sel });
            self.sel = s.sel;
            self.after_history();
        }
    }

    fn redo(&mut self) {
        if let Some(s) = self.redo.pop() {
            self.undo.push(State { doc: std::mem::replace(&mut self.doc, s.doc), sel: self.sel });
            self.sel = s.sel;
            self.after_history();
        }
    }

    fn after_history(&mut self) {
        self.group = None;
        self.changed = true;
        self.typing_style = None;
        self.scroll_to_caret = true;
    }

    // ---- Editing --------------------------------------------------------

    fn set_caret(&mut self, p: Pos, extend: bool) {
        self.sel.caret = p;
        if !extend {
            self.sel.anchor = p;
        }
        self.typing_style = None;
        self.group = None;
        self.scroll_to_caret = true;
    }

    /// Deletes the selection, if any. Returns where the caret ends up.
    fn delete_selection(&mut self) -> Pos {
        let start = self.doc.delete(self.sel.start(), self.sel.end());
        self.sel = Selection::caret(start);
        start
    }

    pub fn insert_text(&mut self, text: &str) {
        let text: String = text.chars().filter(|&c| c == '\n' || c == '\t' || !c.is_control()).collect();
        if text.is_empty() {
            return;
        }
        self.checkpoint(Some(Group::Typing));
        let style = self.typing_style.clone().unwrap_or_else(|| self.doc.style_at(self.sel.start()));
        let at = self.delete_selection();
        let end = self.doc.insert(at, &text, &style);
        self.sel = Selection::caret(end);
        if text == " " {
            self.auto_list(end);
        }
    }

    /// Typing "- ", "* " or "1. " at the start of body text starts a list,
    /// as in Apple Notes.
    fn auto_list(&mut self, caret: Pos) {
        let b = &self.doc.blocks[caret.block];
        if b.kind != Kind::Body {
            return;
        }
        let before: String = b.text.chars().take(caret.offset).collect();
        let kind = match before.as_str() {
            "- " => Kind::Dashed,
            "* " | "• " => Kind::Bulleted,
            "1. " | "1) " => Kind::Numbered,
            "[] " | "[ ] " => Kind::Checklist(false),
            _ => return,
        };
        self.checkpoint(None);
        let n = before.chars().count();
        self.doc.delete(Pos::new(caret.block, 0), Pos::new(caret.block, n));
        self.doc.blocks[caret.block].kind = kind;
        self.sel = Selection::caret(Pos::new(caret.block, 0));
    }

    fn enter(&mut self) {
        self.checkpoint(None);
        let at = self.delete_selection();
        let p = self.doc.enter(at);
        self.sel = Selection::caret(p);
    }

    fn backspace(&mut self, word: bool) {
        if !self.sel.is_empty() {
            self.checkpoint(Some(Group::Deleting));
            self.delete_selection();
            return;
        }
        let c = self.sel.caret;
        if c.offset == 0 {
            // Nothing to undo: no indent or list/quote style to take off,
            // and nothing above to join (a title stays a title).
            let first = &self.doc.blocks[0];
            if c.block == 0 && first.indent == 0 && !first.kind.is_list() && first.kind != Kind::Quote {
                return;
            }
            self.checkpoint(None);
            let p = self.doc.backspace_at_start(c.block);
            self.sel = Selection::caret(p);
            return;
        }
        self.checkpoint(Some(Group::Deleting));
        let from = if word { self.doc.word_step(c, -1) } else { Pos::new(c.block, c.offset - 1) };
        let p = self.doc.delete(from, c);
        self.sel = Selection::caret(p);
    }

    fn delete_forward(&mut self, word: bool) {
        if !self.sel.is_empty() {
            self.checkpoint(Some(Group::Deleting));
            self.delete_selection();
            return;
        }
        let c = self.sel.caret;
        let to = if word { self.doc.word_step(c, 1) } else { self.doc.char_step(c, 1) };
        if to == c {
            return;
        }
        self.checkpoint(Some(Group::Deleting));
        self.doc.delete(c, to);
    }

    pub fn command(&mut self, cmd: Command) {
        let blocks = self.sel.blocks();
        match cmd {
            Command::SetKind(k) => {
                self.checkpoint(None);
                self.doc.set_kind(blocks, k);
            }
            Command::ToggleAttr(a) => {
                if self.sel.is_empty() {
                    let mut s = self.typing_style.clone().unwrap_or_else(|| self.doc.style_at(self.sel.caret));
                    s.set(a, !s.get(a));
                    self.typing_style = Some(s);
                } else {
                    self.checkpoint(None);
                    self.doc.toggle_attr(self.sel.start(), self.sel.end(), a);
                }
            }
            Command::ToggleChecked => {
                self.checkpoint(None);
                if !self.doc.blocks[blocks.clone()].iter().any(|b| matches!(b.kind, Kind::Checklist(_))) {
                    self.doc.set_kind(blocks, Kind::Checklist(false));
                } else {
                    self.doc.toggle_checked(blocks);
                }
            }
            Command::Indent => {
                self.checkpoint(None);
                self.doc.indent(blocks, 1);
            }
            Command::Outdent => {
                self.checkpoint(None);
                self.doc.indent(blocks, -1);
            }
            Command::MoveUp | Command::MoveDown => {
                let d = if cmd == Command::MoveUp { -1 } else { 1 };
                let before = self.doc.clone();
                if self.doc.move_blocks(blocks, d) {
                    self.undo.push(State { doc: before, sel: self.sel });
                    self.redo.clear();
                    self.changed = true;
                    let shift = |p: Pos| Pos::new((p.block as i32 + d) as usize, p.offset);
                    self.sel = Selection { anchor: shift(self.sel.anchor), caret: shift(self.sel.caret) };
                    self.scroll_to_caret = true;
                }
            }
            Command::EditLink => {
                let start = self.sel.start();
                let url = self.doc.blocks[start.block]
                    .styles
                    .get(start.offset)
                    .and_then(|s| s.link.as_deref().map(str::to_owned))
                    .unwrap_or_default();
                self.link_sheet = Some(LinkSheet { url, focus: true });
            }
            Command::SetLink(url) => {
                self.checkpoint(None);
                let url: Option<std::sync::Arc<str>> = url.filter(|u| !u.trim().is_empty()).map(|u| normalize_url(&u).into());
                if self.sel.is_empty() {
                    if let Some(u) = &url {
                        // Nothing selected: the address itself becomes the link.
                        let style = Style { link: Some(u.clone()), ..self.doc.style_at(self.sel.caret) };
                        let end = self.doc.insert(self.sel.caret, u, &style);
                        self.sel = Selection::caret(end);
                    }
                } else {
                    self.doc.restyle(self.sel.start(), self.sel.end(), |s| s.link = url.clone());
                }
            }
            Command::Undo => self.undo(),
            Command::Redo => self.redo(),
            Command::SelectAll => {
                self.sel = Selection { anchor: Pos::default(), caret: self.doc.end() };
            }
            Command::CopyMarkdown => {}
        }
    }

    /// Clicking a checkbox.
    fn toggle_check_at(&mut self, block: usize, sort: bool) {
        self.checkpoint(None);
        if let Kind::Checklist(c) = self.doc.blocks[block].kind {
            self.doc.blocks[block].kind = Kind::Checklist(!c);
            if sort && !c {
                self.sort_checklist(block);
            }
        }
    }

    /// Moves checked items below the unchecked ones in the checklist around
    /// `block` (Settings → Sort checked items: Automatically).
    fn sort_checklist(&mut self, block: usize) {
        let is_item = |b: &Block| matches!(b.kind, Kind::Checklist(_));
        let mut start = block;
        while start > 0 && is_item(&self.doc.blocks[start - 1]) {
            start -= 1;
        }
        let mut end = block + 1;
        while end < self.doc.blocks.len() && is_item(&self.doc.blocks[end]) {
            end += 1;
        }
        let items: Vec<Block> = self.doc.blocks.drain(start..end).collect();
        let (unchecked, checked): (Vec<Block>, Vec<Block>) =
            items.into_iter().partition(|b| b.kind == Kind::Checklist(false));
        let sorted: Vec<Block> = unchecked.into_iter().chain(checked).collect();
        self.doc.blocks.splice(start..start, sorted);
        self.sel = Selection::caret(self.doc.clamp(self.sel.caret));
    }

    fn selected_blocks(&self) -> Vec<Block> {
        self.doc.fragment(self.sel.start(), self.sel.end())
    }

    fn copy(&mut self, ctx: &egui::Context) {
        if self.sel.is_empty() {
            return;
        }
        let text = self.doc.text_between(self.sel.start(), self.sel.end());
        self.clipboard = Some((text.clone(), self.selected_blocks()));
        ctx.copy_text(text);
    }

    fn copy_markdown(&mut self, ctx: &egui::Context) {
        let blocks = if self.sel.is_empty() { self.doc.blocks.clone() } else { self.selected_blocks() };
        ctx.copy_text(markdown::to_markdown(&Doc::new(blocks)));
    }

    fn paste(&mut self, text: &str) {
        let text = text.replace("\r\n", "\n");
        // Copied from this app: paste with its formatting.
        if let Some((plain, blocks)) = &self.clipboard {
            if *plain == text {
                let blocks = blocks.clone();
                self.checkpoint(None);
                let at = self.delete_selection();
                let end = self.doc.insert_blocks(at, blocks);
                self.sel = Selection::caret(end);
                return;
            }
        }
        self.checkpoint(None);
        let style = self.doc.style_at(self.sel.start());
        let at = self.delete_selection();
        let end = self.doc.insert(at, &text, &Style { link: None, ..style });
        self.sel = Selection::caret(end);
    }

    // ---- Find ------------------------------------------------------------

    /// Matches of `query` (case-insensitive) as (block, char range).
    pub fn matches(&self, query: &str) -> Vec<(usize, std::ops::Range<usize>)> {
        // Query and text are folded the same way, character by character
        // (whole-string lowercasing would turn a final Σ into ς, say).
        let fold = |s: &str| -> Vec<char> {
            s.chars()
                .flat_map(|c| {
                    let l: Vec<char> = c.to_lowercase().collect();
                    // Keep char positions aligned (a few characters lowercase to two).
                    if l.len() == 1 { l } else { vec![c] }
                })
                .collect()
        };
        let q = fold(query);
        if q.is_empty() {
            return vec![];
        }
        let mut out = Vec::new();
        for (bi, b) in self.doc.blocks.iter().enumerate() {
            let t = fold(&b.text);
            let mut i = 0;
            while i + q.len() <= t.len() {
                if t[i..i + q.len()] == q[..] {
                    out.push((bi, i..i + q.len()));
                    i += q.len();
                } else {
                    i += 1;
                }
            }
        }
        out
    }

    /// Selects the next (`1`) or previous (`-1`) match of the find query.
    pub fn find_step(&mut self, dir: i32) {
        let m = self.matches(&self.find.query);
        if m.is_empty() {
            return;
        }
        let n = m.len() as i32;
        self.find.current = ((self.find.current as i32 + dir).rem_euclid(n)) as usize;
        self.select_match(&m);
    }

    /// Selects the first match at or after the caret.
    pub fn find_from_caret(&mut self) {
        let m = self.matches(&self.find.query);
        if m.is_empty() {
            return;
        }
        let caret = self.sel.start();
        self.find.current = m.iter().position(|(b, r)| Pos::new(*b, r.start) >= caret).unwrap_or(0);
        self.select_match(&m);
    }

    fn select_match(&mut self, m: &[(usize, std::ops::Range<usize>)]) {
        let (b, r) = &m[self.find.current.min(m.len() - 1)];
        self.sel = Selection { anchor: Pos::new(*b, r.start), caret: Pos::new(*b, r.end) };
        self.scroll_to_caret = true;
    }

    // ---- Geometry -----------------------------------------------------------

    fn caret_rect(&self, p: Pos) -> Option<Rect> {
        let l = self.laid.get(p.block)?;
        let r = l.galley.pos_from_ccursor(CCursor::new(p.offset));
        Some(r.translate(l.origin.to_vec2()))
    }

    /// The position under a point (screen coordinates).
    fn pos_at(&self, pt: Pos2) -> Pos {
        if self.laid.is_empty() {
            return Pos::default();
        }
        let i = self
            .laid
            .iter()
            .position(|l| pt.y < l.rect.bottom())
            .unwrap_or(self.laid.len() - 1);
        let l = &self.laid[i];
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
        let target = if dir < 0 && row > 0 {
            Some((p.block, row - 1))
        } else if dir > 0 && row + 1 < rows {
            Some((p.block, row + 1))
        } else if dir < 0 && p.block > 0 {
            Some((p.block - 1, self.laid[p.block - 1].galley.rows.len().saturating_sub(1)))
        } else if dir > 0 && p.block + 1 < self.laid.len() {
            Some((p.block + 1, 0))
        } else {
            None
        };
        let Some((b, r)) = target else {
            return if dir < 0 { Pos::new(0, 0) } else { self.doc.end() };
        };
        let l = &self.laid[b];
        let y = l.galley.rows.get(r).map_or(0.0, |row| row.rect.center().y);
        let c = l.galley.cursor_from_pos(Vec2::new(x - l.origin.x, y));
        Pos::new(b, c.ccursor.index.min(self.doc.blocks[b].len()))
    }

    fn row_edge(&self, p: Pos, end: bool) -> Pos {
        if self.laid.len() != self.doc.blocks.len() {
            return Pos::new(p.block, if end { self.doc.blocks[p.block].len() } else { 0 });
        }
        let Some(l) = self.laid.get(p.block) else { return p };
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
        let doc_before_len = (self.doc.blocks.len(), self.undo.len());
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
                        if !opts.read_only && !self.sel.is_empty() {
                            self.checkpoint(None);
                            self.delete_selection();
                        }
                    }
                    Event::Paste(t) if !opts.read_only => {
                        self.last_activity = now;
                        self.paste(&t);
                    }
                    Event::Paste(_) => out.blocked_edit = true,
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
        let painter = ui.painter().clone();
        let mut prev_kind = None;
        for (i, b) in self.doc.blocks.iter().enumerate() {
            y += layout::space_before(b.kind, prev_kind, body);
            prev_kind = Some(b.kind);
            let indent = layout::text_indent(b);
            let marks = Marks {
                highlights: find_marks.remove(&i).unwrap_or_default(),
                current: current.as_ref().filter(|(cb, _)| *cb == i).map(|(_, r)| r.clone()),
            };
            let job = layout::job(b, body, p, width - indent, &marks);
            let galley = ui.fonts(|f| f.layout_job(job));
            let mono_pad = if b.kind == Kind::Monospaced { 2.0 } else { 0.0 };
            let origin = Pos2::new(left + indent, y + mono_pad);
            let h = galley.rect.height().max(layout::size(b.kind, body) * 1.2) + 2.0 * mono_pad;
            let rect = Rect::from_min_size(Pos2::new(left, y), Vec2::new(width, h));
            laid.push(Laid { rect, origin, galley, checkbox: None });
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
        if !self.sel.is_empty() {
            let color = if focused { p.text_selection } else { p.selection_inactive };
            let (s, e) = (self.sel.start(), self.sel.end());
            for (i, l) in laid.iter().enumerate().take(e.block + 1).skip(s.block) {
                let from = if i == s.block { s.offset } else { 0 };
                let to = if i == e.block { e.offset } else { self.doc.blocks[i].len() };
                paint_range(&painter, l, from, to, i != e.block, color);
            }
        }
        painter.galley(header_pos, header_galley, p.secondary);
        for (i, l) in laid.iter_mut().enumerate() {
            let b = &self.doc.blocks[i];
            l.checkbox = layout::paint_margin(&painter, b, numbers[i], l.rect, l.origin, &l.galley, body, p);
            painter.galley(l.origin, l.galley.clone(), p.text);
        }
        self.laid = laid;

        // ---- Pointer ----
        let pointer = ui.input(|i| i.pointer.interact_pos());
        let hover_link = resp.hovered().then(|| pointer.and_then(|pt| self.link_at(pt))).flatten();
        if let Some(_) = &hover_link {
            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        } else if resp.hovered() {
            let over_box = pointer.is_some_and(|pt| self.laid.iter().any(|l| l.checkbox.is_some_and(|r| r.contains(pt))));
            ui.ctx().set_cursor_icon(if over_box { egui::CursorIcon::Default } else { egui::CursorIcon::Text });
        }
        if resp.clicked() || resp.drag_started() || resp.secondary_clicked() {
            ui.memory_mut(|m| m.request_focus(id));
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
                self.scroll_to_caret = true;
            }
        }
        if !ui.input(|i| i.pointer.primary_down()) {
            self.dragging = false;
        }
        if resp.double_clicked() {
            if let Some(pt) = pointer {
                let (a, b) = self.doc.word_at(self.pos_at(pt));
                self.sel = Selection { anchor: a, caret: b };
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
        self.context_menu(&resp, opts, &ctx);

        // ---- Caret ----
        if self.sel != sel_before || (self.doc.blocks.len(), self.undo.len()) != doc_before_len {
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
            if self.scroll_to_caret {
                ui.scroll_to_rect(r.expand(24.0), None);
                self.scroll_to_caret = false;
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

    fn context_menu(&mut self, resp: &egui::Response, opts: &Options, ctx: &egui::Context) {
        let has_sel = !self.sel.is_empty();
        let editable = !opts.read_only;
        resp.context_menu(|ui| {
            ui.set_min_width(200.0);
            let item = |ui: &mut egui::Ui, label: &str, shortcut: &str, enabled: bool| {
                ui.add_enabled(enabled, egui::Button::new(label).shortcut_text(shortcut)).clicked()
            };
            if item(ui, "Cut", cmd_key!("X"), has_sel && editable) {
                self.copy(ctx);
                self.checkpoint(None);
                self.delete_selection();
                ui.close_menu();
            }
            if item(ui, "Copy", cmd_key!("C"), has_sel) {
                self.copy(ctx);
                ui.close_menu();
            }
            if item(ui, "Paste", cmd_key!("V"), editable) {
                ctx.send_viewport_cmd(egui::ViewportCommand::RequestPaste);
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

/// A typed link address made into a URL ("example.com" → "https://example.com").
pub fn normalize_url(u: &str) -> String {
    let u = u.trim();
    if u.contains("://") || u.starts_with("mailto:") {
        u.to_owned()
    } else if u.contains('@') && !u.contains('/') {
        format!("mailto:{u}")
    } else {
        format!("https://{u}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn typing_list_markers_starts_lists() {
        let mut e = Editor::new("n", "");
        e.insert_text("-");
        e.insert_text(" ");
        assert_eq!(e.doc.blocks[0].kind, Kind::Dashed);
        assert_eq!(e.doc.blocks[0].text, "");
        e.insert_text("eggs");
        assert_eq!(e.take_markdown().as_deref(), Some("- eggs\n"));
    }

    #[test]
    fn backspace_at_the_start_of_a_title_changes_nothing() {
        let mut e = Editor::new("n", "# Title\nbody\n");
        e.sel = Selection::caret(Pos::new(0, 0));
        e.backspace(false);
        e.backspace(false);
        assert!(e.undo.is_empty(), "no do-nothing undo steps");
        assert_eq!(e.take_markdown(), None, "the note isn't changed");
        // A list item at the top still turns into body text.
        let mut e = Editor::new("n", "- item\n");
        e.sel = Selection::caret(Pos::new(0, 0));
        e.backspace(false);
        assert_eq!(e.doc.blocks[0].kind, Kind::Body);
    }

    #[test]
    fn the_link_sheet_shows_the_link_where_the_selection_starts() {
        let mut e = Editor::new("n", "[link](https://example.com) text\nsecond line\n");
        // Dragged forwards, from inside the link into the next paragraph.
        e.sel = Selection { anchor: Pos::new(0, 1), caret: Pos::new(1, 3) };
        e.command(Command::EditLink);
        assert_eq!(e.link_sheet.as_ref().map(|s| s.url.as_str()), Some("https://example.com"));
    }

    #[test]
    fn find_folds_case_the_same_way_for_query_and_text() {
        let e = Editor::new("n", "ΟΔΟΣ and Straße\n");
        assert_eq!(e.matches("ΟΔΟΣ").len(), 1);
        assert_eq!(e.matches("οδοσ").len(), 1);
        assert_eq!(e.matches("STRASSE").len(), 0);
        assert_eq!(e.matches("straße").len(), 1);
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
    fn typing_is_undone_as_one_step() {
        let mut e = Editor::new("n", "start");
        e.sel = Selection::caret(Pos::new(0, 5));
        for c in " more text".chars() {
            e.insert_text(&c.to_string());
        }
        e.enter();
        e.insert_text("x");
        assert_eq!(e.doc.plain_text(), "start more text\nx");
        e.command(Command::Undo);
        assert_eq!(e.doc.plain_text(), "start more text\n");
        e.command(Command::Undo);
        assert_eq!(e.doc.plain_text(), "start more text");
        e.command(Command::Undo);
        assert_eq!(e.doc.plain_text(), "start");
        e.command(Command::Redo);
        assert_eq!(e.doc.plain_text(), "start more text");
    }

    #[test]
    fn bold_with_nothing_selected_applies_to_typing() {
        let mut e = Editor::new("n", "a");
        e.sel = Selection::caret(Pos::new(0, 1));
        e.command(Command::ToggleAttr(Attr::Bold));
        e.insert_text("b");
        e.command(Command::ToggleAttr(Attr::Bold));
        e.insert_text("c");
        assert_eq!(e.take_markdown().as_deref(), Some("a**b**c\n"));
    }

    #[test]
    fn copy_and_paste_keep_formatting_inside_the_app() {
        let mut e = Editor::new("n", "**bold** plain\n- [ ] task\n");
        e.sel = Selection { anchor: Pos::new(0, 0), caret: Pos::new(1, 4) };
        let ctx = egui::Context::default();
        e.copy(&ctx);
        let (plain, _) = e.clipboard.clone().unwrap();
        assert_eq!(plain, "bold plain\ntask");
        e.sel = Selection::caret(e.doc.end());
        e.enter();
        e.paste(&plain);
        // Pasted into an empty line, paragraphs keep their own style.
        assert_eq!(e.take_markdown().unwrap(), "**bold** plain\n- [ ] task\n**bold** plain\n- [ ] task\n");
        // Text from elsewhere comes in as plain text.
        e.paste("*x*");
        assert!(e.take_markdown().unwrap().ends_with("task\\*x\\*\n"));
    }

    #[test]
    fn checked_items_sort_to_the_bottom() {
        let mut e = Editor::new("n", "- [ ] a\n- [ ] b\n- [ ] c\nafter\n");
        e.toggle_check_at(0, true);
        assert_eq!(e.take_markdown().unwrap(), "- [ ] b\n- [ ] c\n- [x] a\nafter\n");
    }

    #[test]
    fn find_matches_case_insensitively() {
        let mut e = Editor::new("n", "Apple pie\nan apple a day");
        assert_eq!(e.matches("APPLE"), vec![(0, 0..5), (1, 3..8)]);
        e.find.query = "apple".into();
        e.sel = Selection::caret(Pos::new(0, 2));
        e.find_from_caret();
        assert_eq!(e.sel.start(), Pos::new(1, 3));
        e.find_step(1);
        assert_eq!(e.sel.start(), Pos::new(0, 0));
    }

    #[test]
    fn links() {
        let mut e = Editor::new("n", "see docs");
        e.sel = Selection { anchor: Pos::new(0, 4), caret: Pos::new(0, 8) };
        e.command(Command::SetLink(Some("example.com".into())));
        assert_eq!(e.take_markdown().unwrap(), "see [docs](https://example.com)\n");
        assert_eq!(normalize_url("me@x.org"), "mailto:me@x.org");
    }
}
