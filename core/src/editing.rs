//! The editing engine both apps' note editors drive: a [`Doc`] with a
//! selection, and what typing, Return, Backspace and the formatting commands
//! do to it, as in Apple Notes (lists continue and end, "- " starts a list,
//! Backspace at a list item's start turns it into text, checked items sort
//! down). It also does undo with typing grouped into steps, find in note, and
//! copy and paste that keep formatting within the app.
//!
//! How the text is laid out, drawn and pointed at is each app's own: the
//! desktop's egui editor wraps this, and the iPhone's text view reports its
//! edits through [`Editing::replace_range`].

use crate::doc::{Attr, Block, Doc, Kind, Pos, Selection, Style};
use crate::markdown;

/// Things the editor can be asked to do (from the Format menu, toolbar,
/// shortcuts or the right-click menu).
#[derive(Clone, Debug, PartialEq)]
pub enum Command {
    SetKind(Kind),
    ToggleAttr(Attr),
    /// "Mark as Checked" (or make the paragraphs a checklist).
    ToggleChecked,
    Indent,
    Outdent,
    MoveUp,
    MoveDown,
    /// The checklist at the caret: tick every item (Apple Notes' Format →
    /// More → Check All).
    CheckAll,
    UncheckAll,
    /// Remove the checklist's ticked items.
    DeleteChecked,
    /// Ticked items go below the others, keeping their order.
    MoveCheckedToBottom,
    /// Open the link sheet for the selection (the host's to show; see
    /// [`Editing::link_at_selection`]).
    EditLink,
    SetLink(Option<String>),
    Undo,
    Redo,
    SelectAll,
    /// Copy as Markdown (the host's clipboard; see
    /// [`Editing::selection_markdown`]).
    CopyMarkdown,
    /// "Table" (⌥⌘T in Apple Notes): a 3×3 table at the caret.
    InsertTable,
    AddRowAbove,
    AddRowBelow,
    AddColumnBefore,
    AddColumnAfter,
    DeleteRow,
    DeleteColumn,
    DeleteTable,
    /// A table's first row (or column) can be a header.
    ToggleHeaderRow,
    ToggleHeaderColumn,
    /// Puts an image (an attachment's name, saved already) at the caret,
    /// in place of the selection.
    InsertImage(String),
}

/// Text from outside (typed, pasted from another app) can't hold images:
/// a stray object character would show as one without a picture.
fn without_objects(text: &str) -> String {
    text.replace(crate::attachments::OBJECT, "")
}

/// Find in note.
#[derive(Default, Clone, Debug)]
pub struct Find {
    pub open: bool,
    pub query: String,
    /// The find field should take the keyboard (desktop).
    pub focus: bool,
    /// The current match's index in [`Editing::matches`].
    pub current: usize,
}

#[derive(Clone)]
struct State {
    doc: Doc,
    sel: Selection,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Group {
    Typing,
    Deleting,
}

/// Typing (or deleting) within this many seconds of the last is one undo
/// step.
const COALESCE_SECS: f64 = 1.5;
const MAX_UNDO: usize = 500;

pub struct Editing {
    pub note_id: String,
    pub doc: Doc,
    pub sel: Selection,
    /// The style to type in after Bold etc. was pressed with nothing selected.
    pub typing_style: Option<Style>,
    undo: Vec<State>,
    redo: Vec<State>,
    group: Option<(Group, f64)>,
    /// The time now, in seconds (any clock), set by the host before input:
    /// undo groups typing by it.
    pub clock: f64,
    /// Changed since the app last took the Markdown.
    changed: bool,
    /// The Markdown the editor was loaded with or last produced.
    pub markdown_seen: String,
    pub find: Find,
    /// The caret moved or the text changed: the host should scroll it into
    /// view (and clear this).
    pub reveal: bool,
    /// Formatted copy of what was last copied, matched up with the clipboard
    /// text when pasting.
    clipboard: Option<(String, Vec<Block>)>,
}

impl Editing {
    pub fn new(note_id: &str, md: &str) -> Editing {
        Editing {
            note_id: note_id.to_owned(),
            doc: markdown::from_markdown(md),
            sel: Selection::default(),
            typing_style: None,
            undo: Vec::new(),
            redo: Vec::new(),
            group: None,
            clock: 0.0,
            changed: false,
            markdown_seen: md.to_owned(),
            find: Find::default(),
            reveal: false,
            clipboard: None,
        }
    }

    /// A new note from [`crate::library::Library::new_note`]: its Markdown
    /// (empty, or a tag), the first paragraph in the "New notes start with"
    /// style.
    pub fn new_note(note_id: &str, md: &str, start: Kind) -> Editing {
        let mut e = Editing::new(note_id, md);
        e.doc.blocks[0].kind = start;
        e
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
    /// where it was. Returns whether anything changed.
    pub fn reload(&mut self, md: &str) -> bool {
        if md == self.markdown_seen {
            return false;
        }
        // Not an edit here: the note isn't marked changed (which would write
        // the other device's text back in this app's own Markdown style),
        // and undo history from before can't be stepped back into, as that
        // would quietly revert the other device's edit.
        self.undo.clear();
        self.redo.clear();
        self.group = None;
        self.typing_style = None;
        self.doc = markdown::from_markdown(md);
        self.markdown_seen = md.to_owned();
        self.sel = Selection { anchor: self.doc.clamp(self.sel.anchor), caret: self.doc.clamp(self.sel.caret) };
        true
    }

    /// The paragraph style at the caret (for the Format menu's checkmark).
    pub fn current_kind(&self) -> Kind {
        self.doc.blocks[self.sel.caret.block.min(self.doc.blocks.len() - 1)].kind
    }

    /// Whether the selection (or typing style) has a character attribute.
    pub fn has_attr(&self, a: Attr) -> bool {
        if self.sel.is_empty() {
            self.typing_style().get(a)
        } else {
            self.doc.has_attr(self.sel.start(), self.sel.end(), a)
        }
    }

    /// The style typing would use now.
    pub fn typing_style(&self) -> Style {
        self.typing_style.clone().unwrap_or_else(|| self.doc.style_at(self.sel.start()))
    }

    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }

    /// How many steps undo can take (it changes with every edit step).
    pub fn undo_depth(&self) -> usize {
        self.undo.len()
    }

    // ---- Undo -----------------------------------------------------------------

    /// Saves the state for undo before a change. Typing (or deleting) in a
    /// row is undone as one step.
    fn checkpoint(&mut self, group: Option<Group>) {
        let now = self.clock;
        let joins = match (group, self.group) {
            (Some(g), Some((last, t))) => g == last && now - t < COALESCE_SECS,
            _ => false,
        };
        if !joins {
            self.undo.push(State { doc: self.doc.clone(), sel: self.sel });
            if self.undo.len() > MAX_UNDO {
                self.undo.remove(0);
            }
        }
        self.group = group.map(|g| (g, now));
        self.redo.clear();
        self.changed = true;
        self.reveal = true;
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
        self.reveal = true;
    }

    // ---- Editing --------------------------------------------------------------

    /// Moves the caret (extending the selection if `extend`).
    pub fn set_caret(&mut self, p: Pos, extend: bool) {
        self.sel.caret = p;
        if !extend {
            self.sel.anchor = p;
        }
        self.typing_style = None;
        self.group = None;
        self.reveal = true;
    }

    /// Selects from `anchor` to `caret`. The typing style is kept if the
    /// selection didn't actually change (iOS reports the selection again
    /// after every edit).
    pub fn set_selection(&mut self, anchor: Pos, caret: Pos) {
        let sel = Selection { anchor: self.doc.clamp(anchor), caret: self.doc.clamp(caret) };
        if sel != self.sel {
            self.sel = sel;
            self.typing_style = None;
            self.group = None;
        }
    }

    /// Deletes the selection, if any. Returns where the caret ends up.
    fn delete_selection(&mut self) -> Pos {
        let start = self.doc.delete(self.sel.start(), self.sel.end());
        self.sel = Selection::caret(start);
        start
    }

    /// Cut: deletes the selection with an undo step.
    pub fn delete_selected(&mut self) {
        if !self.sel.is_empty() {
            self.checkpoint(None);
            self.delete_selection();
        }
    }

    /// Typing (one character or an IME's commit) over the selection.
    pub fn insert_text(&mut self, text: &str) {
        let text: String = without_objects(text).chars().filter(|&c| c == '\n' || c == '\t' || !c.is_control()).collect();
        if text.is_empty() {
            return;
        }
        self.checkpoint(Some(Group::Typing));
        let style = self.typing_style();
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

    /// Return: a new paragraph (a list continues; Return on an empty item
    /// ends the list).
    pub fn enter(&mut self) {
        self.checkpoint(None);
        let at = self.delete_selection();
        let p = self.doc.enter(at);
        self.sel = Selection::caret(p);
    }

    /// Backspace (a word with `word`).
    pub fn backspace(&mut self, word: bool) {
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
        let from = if word { self.doc.word_step(c, -1) } else { self.doc.char_step(c, -1) };
        let p = self.doc.delete(from, c);
        self.sel = Selection::caret(p);
    }

    /// Forward delete (a word with `word`).
    pub fn delete_forward(&mut self, word: bool) {
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

    /// An edit as a system text view reports it: the text from `start` to
    /// `end` is replaced with `text`. Return, Backspace at a paragraph's
    /// start and pasting several lines get the same treatment as when typed
    /// on the desktop (lists continue and end, "- " starts a list, …).
    pub fn replace_range(&mut self, start: Pos, end: Pos, text: &str) {
        let (start, end) = (self.doc.clamp(start), self.doc.clamp(end));
        let (start, end) = if end < start { (end, start) } else { (start, end) };
        let text = text.replace("\r\n", "\n").replace('\r', "\n");
        let was_caret = self.sel.is_empty() && self.sel.caret == end;
        if self.sel.start() != start || self.sel.end() != end {
            self.set_selection(start, end);
        }
        if text.is_empty() {
            // One character back from the caret (Backspace) at a
            // paragraph's start: take the list style off first, as typed.
            let joins_paragraphs = end.offset == 0 && end.block == start.block + 1 && start.offset == self.doc.blocks[start.block].len();
            if joins_paragraphs && was_caret {
                self.sel = Selection::caret(end);
                self.backspace(false);
            } else if start != end {
                self.checkpoint(Some(Group::Deleting));
                self.delete_selection();
            }
            return;
        }
        if text == "\n" {
            self.enter();
            return;
        }
        if text.contains('\n') {
            self.paste(&text);
            return;
        }
        self.insert_text(&text);
    }

    /// Replaces text as it is, with none of typing's list handling: for
    /// bringing the document back in line with a system text view that
    /// changed its text by itself.
    pub fn replace_raw(&mut self, start: Pos, end: Pos, text: &str) {
        let (start, end) = (self.doc.clamp(start), self.doc.clamp(end));
        let (start, end) = if end < start { (end, start) } else { (start, end) };
        self.checkpoint(Some(Group::Typing));
        self.sel = Selection { anchor: start, caret: end };
        let style = self.typing_style();
        let at = self.delete_selection();
        let end = self.doc.insert(at, &without_objects(&text.replace("\r\n", "\n")), &style);
        self.sel = Selection::caret(end);
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
                    let mut s = self.typing_style();
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
                    // Typing after this is a step of its own.
                    self.group = None;
                    self.changed = true;
                    let shift = |p: Pos| Pos::new((p.block as i32 + d) as usize, p.offset);
                    self.sel = Selection { anchor: shift(self.sel.anchor), caret: shift(self.sel.caret) };
                    self.reveal = true;
                }
            }
            Command::CheckAll | Command::UncheckAll | Command::DeleteChecked | Command::MoveCheckedToBottom => {
                self.checklist_command(&cmd);
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
            Command::EditLink | Command::CopyMarkdown => {}
            Command::InsertTable => {
                if self.doc.blocks[self.sel.caret.block].kind.is_cell() {
                    return;
                }
                self.checkpoint(None);
                let at = self.delete_selection();
                let p = self.doc.insert_table(at, 3, 3);
                self.sel = Selection::caret(p);
            }
            Command::InsertImage(name) => {
                if !crate::attachments::is_name(&name) {
                    return;
                }
                self.checkpoint(None);
                let at = self.delete_selection();
                let p = self.doc.insert_image(at, &name);
                self.sel = Selection::caret(p);
            }
            Command::AddRowAbove
            | Command::AddRowBelow
            | Command::AddColumnBefore
            | Command::AddColumnAfter
            | Command::DeleteRow
            | Command::DeleteColumn
            | Command::DeleteTable
            | Command::ToggleHeaderRow
            | Command::ToggleHeaderColumn => {
                let block = self.sel.caret.block;
                if !self.doc.blocks[block].kind.is_cell() {
                    return;
                }
                self.checkpoint(None);
                let p = match cmd {
                    Command::AddRowAbove => self.doc.add_row(block, false),
                    Command::AddRowBelow => self.doc.add_row(block, true),
                    Command::AddColumnBefore => self.doc.add_column(block, false),
                    Command::AddColumnAfter => self.doc.add_column(block, true),
                    Command::DeleteRow => self.doc.delete_row(block),
                    Command::DeleteColumn => self.doc.delete_column(block),
                    Command::DeleteTable => self.doc.delete_table(block),
                    Command::ToggleHeaderRow => {
                        self.doc.toggle_header(block, false);
                        self.sel.caret
                    }
                    _ => {
                        self.doc.toggle_header(block, true);
                        self.sel.caret
                    }
                };
                self.sel = Selection::caret(self.doc.clamp(p));
                self.reveal = true;
            }
        }
    }

    /// Tab (or Shift+Tab) in a table cell: the caret goes to the end of the
    /// next (previous) cell; Tab in the last cell adds a row, as in Apple
    /// Notes.
    pub fn tab_in_table(&mut self, back: bool) {
        let block = self.sel.caret.block;
        let target = match self.doc.next_cell(block, if back { -1 } else { 1 }) {
            Some(b) => b,
            None if back => return,
            None => {
                self.checkpoint(None);
                // Below the last row; its first cell.
                let (r, cols) = self.doc.table_of(block).unwrap();
                self.doc.add_row(r.end - cols, true).block
            }
        };
        let end = self.doc.blocks[target].len();
        self.sel = Selection::caret(Pos::new(target, end));
        self.reveal = true;
    }

    /// The link where the selection starts (for the link sheet).
    pub fn link_at_selection(&self) -> String {
        let start = self.sel.start();
        self.doc.blocks[start.block]
            .styles
            .get(start.offset)
            .and_then(|s| s.link.as_deref().map(str::to_owned))
            .unwrap_or_default()
    }

    /// Ticks or unticks a checklist item (tapping its circle).
    pub fn toggle_check_at(&mut self, block: usize, sort: bool) {
        if block >= self.doc.blocks.len() {
            return;
        }
        self.checkpoint(None);
        if let Kind::Checklist(c) = self.doc.blocks[block].kind {
            self.doc.blocks[block].kind = Kind::Checklist(!c);
            if sort && !c {
                self.sort_checklist(block);
            }
        }
    }

    /// The checklist (its items in a row) around the caret, if the caret is
    /// in one.
    fn checklist_at_caret(&self) -> Option<std::ops::Range<usize>> {
        let is_item = |b: &Block| matches!(b.kind, Kind::Checklist(_));
        let block = self.sel.caret.block.min(self.doc.blocks.len() - 1);
        if !is_item(&self.doc.blocks[block]) {
            return None;
        }
        let mut start = block;
        while start > 0 && is_item(&self.doc.blocks[start - 1]) {
            start -= 1;
        }
        let mut end = block + 1;
        while end < self.doc.blocks.len() && is_item(&self.doc.blocks[end]) {
            end += 1;
        }
        Some(start..end)
    }

    /// Whether the checklist at the caret has a ticked item, and an
    /// unticked one; `None` outside a checklist.
    pub fn checklist_state(&self) -> Option<(bool, bool)> {
        let r = self.checklist_at_caret()?;
        let items = &self.doc.blocks[r];
        Some((items.iter().any(|b| b.kind == Kind::Checklist(true)), items.iter().any(|b| b.kind == Kind::Checklist(false))))
    }

    /// Check All, Uncheck All, Delete Checked, Move Checked to Bottom.
    fn checklist_command(&mut self, cmd: &Command) {
        let Some(range) = self.checklist_at_caret() else { return };
        let (any_checked, any_unchecked) = self.checklist_state().unwrap_or((false, false));
        let does_something = match cmd {
            Command::CheckAll => any_unchecked,
            Command::UncheckAll | Command::DeleteChecked => any_checked,
            _ => {
                // Some ticked item has an unticked one after it.
                let items = &self.doc.blocks[range.clone()];
                let first_checked = items.iter().position(|b| b.kind == Kind::Checklist(true));
                first_checked.is_some_and(|i| items[i..].iter().any(|b| b.kind == Kind::Checklist(false)))
            }
        };
        if !does_something {
            return;
        }
        self.checkpoint(None);
        match cmd {
            Command::CheckAll | Command::UncheckAll => {
                let on = *cmd == Command::CheckAll;
                for b in &mut self.doc.blocks[range] {
                    b.kind = Kind::Checklist(on);
                }
            }
            Command::DeleteChecked => {
                let start = range.start;
                let kept: Vec<Block> =
                    self.doc.blocks.drain(range).filter(|b| b.kind != Kind::Checklist(true)).collect();
                let n = kept.len();
                self.doc.blocks.splice(start..start, kept);
                if self.doc.blocks.is_empty() {
                    self.doc.blocks.push(Block::new(Kind::Checklist(false), ""));
                }
                // The caret goes to the end of the last item kept, or to the
                // start of what followed the checklist.
                let at = if n > 0 {
                    let b = start + n - 1;
                    Pos::new(b, self.doc.blocks[b].len())
                } else {
                    Pos::new(start.min(self.doc.blocks.len() - 1), 0)
                };
                self.sel = Selection::caret(self.doc.clamp(at));
            }
            _ => self.sort_checklist(range.start),
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

    // ---- Clipboard ------------------------------------------------------------

    fn selected_blocks(&self) -> Vec<Block> {
        self.doc.fragment(self.sel.start(), self.sel.end())
    }

    /// Copy: the selection's text for the system clipboard (its formatting
    /// is kept here, for pasting back into the app).
    pub fn copy(&mut self) -> Option<String> {
        if self.sel.is_empty() {
            return None;
        }
        let text = self.doc.text_between(self.sel.start(), self.sel.end());
        self.clipboard = Some((text.clone(), self.selected_blocks()));
        Some(text)
    }

    /// Cut: [`Editing::copy`], then delete the selection.
    pub fn cut(&mut self) -> Option<String> {
        let text = self.copy()?;
        self.delete_selected();
        Some(text)
    }

    /// The selection (or the whole note) as Markdown.
    pub fn selection_markdown(&self) -> String {
        let blocks = if self.sel.is_empty() { self.doc.blocks.clone() } else { self.selected_blocks() };
        markdown::to_markdown(&Doc::new(blocks))
    }

    /// Pastes text: with its formatting if it was copied from this note
    /// editor, else as plain text.
    pub fn paste(&mut self, text: &str) {
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
        let text = without_objects(&text);
        if text.is_empty() && self.sel.is_empty() {
            return;
        }
        self.checkpoint(None);
        let style = self.doc.style_at(self.sel.start());
        let at = self.delete_selection();
        let end = self.doc.insert(at, &text, &Style { link: None, ..style });
        self.sel = Selection::caret(end);
    }

    // ---- Find ---------------------------------------------------------------------

    /// Matches of `query` (case-insensitive) as (block, char range).
    pub fn matches(&self, query: &str) -> Vec<(usize, std::ops::Range<usize>)> {
        // Query and text are folded the same way, character by character
        // (whole-string lowercasing would turn a final Σ into ς, say).
        let fold = |s: &str| -> Vec<char> {
            s.chars()
                .flat_map(|c| {
                    let l: Vec<char> = c.to_lowercase().collect();
                    // Keep char positions aligned (a few characters lowercase to two).
                    if l.len() == 1 {
                        l
                    } else {
                        vec![c]
                    }
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
        self.reveal = true;
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
    fn checklist_commands_act_on_the_checklist_at_the_caret() {
        let md = "Shopping\n- [ ] eggs\n- [x] milk\n- [ ] bread\n- [x] jam\n\nOther\n- [x] done\n";
        let mut e = Editing::new("n", md);
        e.sel = Selection::caret(Pos::new(1, 2));
        assert_eq!(e.checklist_state(), Some((true, true)));
        e.command(Command::CheckAll);
        assert!(e.doc.blocks[1..5].iter().all(|b| b.kind == Kind::Checklist(true)));
        let last = e.doc.blocks.len() - 1;
        assert_eq!(e.doc.blocks[last].kind, Kind::Checklist(true), "another checklist is left alone");
        e.command(Command::UncheckAll);
        assert!(e.doc.blocks[1..5].iter().all(|b| b.kind == Kind::Checklist(false)));
        assert_eq!(e.doc.blocks[last].kind, Kind::Checklist(true));
        e.command(Command::Undo);
        e.command(Command::Undo);
        assert_eq!(e.doc.blocks[2].kind, Kind::Checklist(true), "undone step by step");
        assert_eq!(e.doc.blocks[1].kind, Kind::Checklist(false));

        e.command(Command::MoveCheckedToBottom);
        let texts: Vec<&str> = e.doc.blocks[1..5].iter().map(|b| b.text.as_str()).collect();
        assert_eq!(texts, ["eggs", "bread", "milk", "jam"]);
        let depth = e.undo_depth();
        e.command(Command::MoveCheckedToBottom);
        assert_eq!(e.undo_depth(), depth, "already sorted: no undo step");

        e.sel = Selection::caret(Pos::new(4, 1));
        e.command(Command::DeleteChecked);
        let texts: Vec<&str> = e.doc.blocks[..3].iter().map(|b| b.text.as_str()).collect();
        assert_eq!(texts, ["Shopping", "eggs", "bread"]);
        assert_eq!(e.doc.blocks[last - 2].text, "done", "the other checklist keeps its items");
        assert_eq!(e.sel.caret, Pos::new(2, 5), "after the last item kept");

        // Outside a checklist nothing happens.
        e.sel = Selection::caret(Pos::new(0, 0));
        assert_eq!(e.checklist_state(), None);
        let depth = e.undo_depth();
        e.command(Command::CheckAll);
        assert_eq!(e.undo_depth(), depth);
    }

    #[test]
    fn deleting_every_item_of_the_only_checklist_leaves_an_empty_item() {
        let mut e = Editing::new("n", "- [x] a\n- [x] b\n");
        e.sel = Selection::caret(Pos::new(0, 0));
        e.command(Command::DeleteChecked);
        assert_eq!(e.doc.blocks.len(), 1);
        assert_eq!(e.doc.blocks[0].kind, Kind::Checklist(false));
        assert_eq!(e.sel.caret, Pos::new(0, 0));
    }

    #[test]
    fn typing_list_markers_starts_lists() {
        let mut e = Editing::new("n", "");
        e.insert_text("-");
        e.insert_text(" ");
        assert_eq!(e.doc.blocks[0].kind, Kind::Dashed);
        assert_eq!(e.doc.blocks[0].text, "");
        e.insert_text("eggs");
        assert_eq!(e.take_markdown().as_deref(), Some("- eggs\n"));
    }

    #[test]
    fn backspace_at_the_start_of_a_title_changes_nothing() {
        let mut e = Editing::new("n", "# Title\nbody\n");
        e.sel = Selection::caret(Pos::new(0, 0));
        e.backspace(false);
        e.backspace(false);
        assert!(e.undo.is_empty(), "no do-nothing undo steps");
        assert_eq!(e.take_markdown(), None, "the note isn't changed");
        // A list item at the top still turns into body text.
        let mut e = Editing::new("n", "- item\n");
        e.sel = Selection::caret(Pos::new(0, 0));
        e.backspace(false);
        assert_eq!(e.doc.blocks[0].kind, Kind::Body);
    }

    #[test]
    fn the_link_sheet_shows_the_link_where_the_selection_starts() {
        let mut e = Editing::new("n", "[link](https://example.com) text\nsecond line\n");
        // Dragged forwards, from inside the link into the next paragraph.
        e.sel = Selection { anchor: Pos::new(0, 1), caret: Pos::new(1, 3) };
        assert_eq!(e.link_at_selection(), "https://example.com");
    }

    #[test]
    fn find_folds_case_the_same_way_for_query_and_text() {
        let e = Editing::new("n", "ΟΔΟΣ and Straße\n");
        assert_eq!(e.matches("ΟΔΟΣ").len(), 1);
        assert_eq!(e.matches("οδοσ").len(), 1);
        assert_eq!(e.matches("STRASSE").len(), 0);
        assert_eq!(e.matches("straße").len(), 1);
    }

    #[test]
    fn typing_after_moving_a_paragraph_is_its_own_undo_step() {
        let mut e = Editing::new("n", "one\ntwo\n");
        e.sel = Selection::caret(Pos::new(0, 3));
        e.insert_text("!");
        e.command(Command::MoveDown);
        e.insert_text("?");
        assert_eq!(e.doc.plain_text(), "two\none!?");
        e.command(Command::Undo);
        assert_eq!(e.doc.plain_text(), "two\none!", "only the typing is undone");
        e.command(Command::Undo);
        assert_eq!(e.doc.plain_text(), "one!\ntwo");
    }

    #[test]
    fn backspace_and_delete_take_whole_emoji() {
        let mut e = Editing::new("n", "ok 👍🏽🇩🇪\n");
        e.sel = Selection::caret(Pos::new(0, 7));
        e.backspace(false);
        assert_eq!(e.doc.blocks[0].text, "ok 👍🏽");
        e.backspace(false);
        assert_eq!(e.doc.blocks[0].text, "ok ");
        let mut e = Editing::new("n", "👍🏽x\n");
        e.sel = Selection::caret(Pos::new(0, 0));
        e.delete_forward(false);
        assert_eq!(e.doc.blocks[0].text, "x");
    }

    #[test]
    fn a_reload_from_a_sync_is_not_an_edit() {
        let mut e = Editing::new("n", "# Note\nmine\n");
        e.sel = Selection::caret(Pos::new(1, 4));
        e.insert_text("!");
        assert_eq!(e.take_markdown().as_deref(), Some("# Note\nmine!\n"));
        // Markdown from another device, not in this app's own style.
        e.reload("# Note\nmine!\n+ theirs\n");
        assert_eq!(e.take_markdown(), None, "not written back reformatted");
        e.command(Command::Undo);
        assert_eq!(e.doc.plain_text(), "Note\nmine!\ntheirs", "undo can't revert the other device's edit");
        assert_eq!((e.doc.clamp(e.sel.anchor), e.doc.clamp(e.sel.caret)), (e.sel.anchor, e.sel.caret));
    }

    #[test]
    fn typing_is_undone_as_one_step() {
        let mut e = Editing::new("n", "start");
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
        let mut e = Editing::new("n", "a");
        e.sel = Selection::caret(Pos::new(0, 1));
        e.command(Command::ToggleAttr(Attr::Bold));
        e.insert_text("b");
        e.command(Command::ToggleAttr(Attr::Bold));
        e.insert_text("c");
        assert_eq!(e.take_markdown().as_deref(), Some("a**b**c\n"));
    }

    #[test]
    fn copy_and_paste_keep_formatting_inside_the_app() {
        let mut e = Editing::new("n", "**bold** plain\n- [ ] task\n");
        e.sel = Selection { anchor: Pos::new(0, 0), caret: Pos::new(1, 4) };
        let plain = e.copy().unwrap();
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
    fn inserting_copying_and_undoing_images() {
        let name = crate::attachments::name_for(b"pic", "png");
        let mut e = Editing::new("n", "# Trip\nday one\n");
        e.set_caret(Pos::new(1, 3), false);
        e.command(Command::InsertImage(name.clone()));
        assert_eq!(e.take_markdown().unwrap(), format!("# Trip\nday\n![](attachments/{name})\n one\n"));
        assert_eq!(e.sel.caret, Pos::new(3, 0));
        // Typing goes on as text.
        e.insert_text("x");
        assert_eq!(e.doc.blocks[3].styles[0].image, None);
        // Copied and pasted inside the app, the image comes along.
        e.set_selection(Pos::new(2, 0), Pos::new(2, 1));
        let plain = e.copy().unwrap();
        e.set_caret(e.doc.end(), false);
        e.paste(&plain);
        assert_eq!(e.doc.images(), vec![name.clone()]);
        assert_eq!(e.doc.blocks.iter().flat_map(|b| &b.styles).filter(|s| s.image.is_some()).count(), 2);
        // From another app, the object character is dropped.
        e.paste("a\u{FFFC}b");
        assert!(e.doc.blocks.last().unwrap().text.ends_with("ab"));
        // Undo takes the image back out.
        for _ in 0..4 {
            e.command(Command::Undo);
        }
        assert_eq!(e.doc.plain_text(), "Trip\nday one");
        // A name that isn't an attachment's does nothing.
        e.command(Command::InsertImage("../../x.png".into()));
        assert!(e.doc.images().is_empty());
    }

    #[test]
    fn checked_items_sort_to_the_bottom() {
        let mut e = Editing::new("n", "- [ ] a\n- [ ] b\n- [ ] c\nafter\n");
        e.toggle_check_at(0, true);
        assert_eq!(e.take_markdown().unwrap(), "- [ ] b\n- [ ] c\n- [x] a\nafter\n");
    }

    #[test]
    fn find_matches_case_insensitively() {
        let mut e = Editing::new("n", "Apple pie\nan apple a day");
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
        let mut e = Editing::new("n", "see docs");
        e.sel = Selection { anchor: Pos::new(0, 4), caret: Pos::new(0, 8) };
        e.command(Command::SetLink(Some("example.com".into())));
        assert_eq!(e.take_markdown().unwrap(), "see [docs](https://example.com)\n");
        assert_eq!(normalize_url("me@x.org"), "mailto:me@x.org");
    }

    // ---- Edits as a system text view reports them -----------------------------

    #[test]
    fn return_through_replace_range_continues_and_ends_lists() {
        let mut e = Editing::new("n", "- [ ] milk\n");
        let end = Pos::new(0, 4);
        e.set_selection(end, end);
        e.replace_range(end, end, "\n");
        assert_eq!(e.doc.blocks[1].kind, Kind::Checklist(false), "the checklist continues");
        let p = e.sel.caret;
        e.replace_range(p, p, "\n");
        assert_eq!(e.doc.blocks.len(), 2, "Return on an empty item ends the list");
        assert_eq!(e.doc.blocks[1].kind, Kind::Body);
    }

    #[test]
    fn backspace_through_replace_range_takes_the_list_style_off_first() {
        let mut e = Editing::new("n", "text\n- item\n");
        let caret = Pos::new(1, 0);
        e.set_selection(caret, caret);
        // The text view deletes the line break before the caret.
        e.replace_range(Pos::new(0, 4), caret, "");
        assert_eq!(e.doc.blocks.len(), 2, "not joined yet");
        assert_eq!(e.doc.blocks[1].kind, Kind::Body);
        let caret = e.sel.caret;
        e.replace_range(Pos::new(0, 4), caret, "");
        assert_eq!(e.doc.plain_text(), "textitem");
    }

    #[test]
    fn typing_through_replace_range_starts_lists_and_replaces_selections() {
        let mut e = Editing::new("n", "");
        for c in ["-", " ", "a"] {
            let p = e.sel.caret;
            e.replace_range(p, p, c);
        }
        assert_eq!(e.take_markdown().as_deref(), Some("- a\n"));
        let mut e = Editing::new("n", "hello world\n");
        e.replace_range(Pos::new(0, 6), Pos::new(0, 11), "there");
        assert_eq!(e.doc.plain_text(), "hello there");
        // An autocorrect replacing a word elsewhere, multi-line paste.
        e.replace_range(Pos::new(0, 0), Pos::new(0, 5), "hi\nyou");
        assert_eq!(e.doc.plain_text(), "hi\nyou there");
    }
}
