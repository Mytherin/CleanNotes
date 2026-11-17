//! The desktop app: the three panes (folders, notes list, note), what's
//! selected, keyboard shortcuts and dialogs. The notes themselves, and what's
//! done to them (listing, deleting, moving, folders, accounts, saving,
//! syncing, locked notes), are the shared [`Library`]'s.

use std::time::{Duration as StdDuration, Instant};

use chrono::Local;


use eframe::egui::{self, Key, KeyboardShortcut, Modifiers};

use crate::db::{self, Secrets};
use crate::doc::Kind;
use crate::editor::images::ImageCache;
use crate::editor::{Command, Editor};
use crate::settings::{ListView, Settings};
use crate::theme::{self, Palette};
use cleannotes_core::library::{BodySet, Library};

pub use cleannotes_core::dates::{list_time, note_header_time};
pub use cleannotes_core::library::Location;

mod attach;
mod import;
mod list;
pub mod locked;
mod note_pane;
pub mod settings_window;
mod sidebar;

/// Height of the toolbar row along the top of every pane.
pub const TOOLBAR_H: f32 = 52.0;

/// Which pane has the keyboard.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pane {
    Sidebar,
    List,
    Note,
}

/// A question waiting for an answer.
pub enum Confirm {
    DeleteNotesForever(Vec<String>),
    DeleteFolder(String),
    EmptyDeleted(String),
    RemoveAccount(String),
}

/// A folder being named in the sidebar.
pub struct Rename {
    pub folder: String,
    pub text: String,
    pub focus: bool,
}

pub struct App {
    /// The notes, settings, storage and sync (shared with the iPhone app).
    pub lib: Library,
    pub location: Location,
    pub selected: Option<String>,
    /// Notes selected together with Ctrl+Click or Shift+Click (including
    /// `selected`); empty when just one note is selected.
    pub multi: Vec<String>,
    /// Where a Shift+Click range starts.
    anchor: Option<String>,
    pub editor: Option<Editor>,
    pub focus: Pane,
    /// The toolbar search (searches all notes).
    pub search: String,
    pub search_focus: bool,
    /// The round search button has opened into a field.
    pub search_open: bool,
    /// Messages shown at the bottom of the window for a few seconds.
    notices: Vec<(String, Instant)>,
    pub rename: Option<Rename>,
    pub confirm: Option<Confirm>,
    pub settings_ui: settings_window::SettingsUi,
    /// The Import Notes sheet, when open.
    pub import: Option<import::ImportSheet>,
    /// The notes list as shown last frame (for ↑/↓).
    pub shown: Vec<String>,
    /// Scroll the list to the selected note.
    pub scroll_list: bool,
    pub format_open: bool,
    /// The gallery shows the selected note full size.
    pub gallery_open: bool,
    /// "Edit as Markdown": notes show (and are edited as) their Markdown.
    pub markdown_mode: bool,
    /// The open note's Markdown as being edited there: its id and text.
    pub markdown_source: Option<(String, String)>,
    /// The lock screen and password sheets.
    pub lock: locked::LockUi,
    /// The tags in the notes, and the notes' fingerprint they were found
    /// for (finding them reads every note).
    tag_cache: Option<(u64, Vec<String>)>,
    /// The notes list's sections, and what they were worked out for.
    sections_cache: Option<(u64, Vec<(String, Vec<String>)>)>,
    /// Pictures in notes, loaded.
    pub images: ImageCache,
}

impl App {
    /// The app, or (if the notes database can't be opened, say because
    /// another program holds it) a window saying why.
    pub fn start(cc: &eframe::CreationContext<'_>) -> Box<dyn eframe::App> {
        match App::new(cc) {
            Ok(app) => Box::new(app),
            Err(e) => Box::new(StartupError(format!("{e:#}"))),
        }
    }

    fn new(cc: &eframe::CreationContext<'_>) -> anyhow::Result<Self> {
        theme::install_fonts(&cc.egui_ctx);
        cc.egui_ctx.options_mut(|o| o.zoom_with_keyboard = false);
        // The system's light/dark mode, unless CLEANNOTES_THEME says otherwise.
        match std::env::var("CLEANNOTES_THEME").as_deref() {
            Ok("light") => cc.egui_ctx.options_mut(|o| o.theme_preference = egui::ThemePreference::Light),
            Ok("dark") => cc.egui_ctx.options_mut(|o| o.theme_preference = egui::ThemePreference::Dark),
            _ => {}
        }
        let ctx = cc.egui_ctx.clone();
        let mut lib = Library::open(&db::data_dir(), Secrets::Keyring, move || ctx.request_repaint())?;
        // Example notes (`CLEANNOTES_SEED=1` with an empty data folder), for
        // trying the app out and for screenshots.
        let seed = std::env::var_os("CLEANNOTES_SEED").is_some() && lib.store.notes.is_empty();
        if seed {
            lib.seed();
        }
        let mut app = App::with(lib);
        app.restore_location();
        Ok(app)
    }

    fn with(lib: Library) -> Self {
        App {
            images: ImageCache::new(lib.attachments_dir()),
            lib,
            location: Location::Folder(String::new()),
            selected: None,
            multi: Vec::new(),
            anchor: None,
            editor: None,
            focus: Pane::List,
            search: String::new(),
            search_focus: false,
            search_open: false,
            notices: Vec::new(),
            rename: None,
            confirm: None,
            settings_ui: Default::default(),
            import: None,
            shown: Vec::new(),
            scroll_list: true,
            format_open: false,
            gallery_open: false,
            markdown_mode: false,
            markdown_source: None,
            lock: Default::default(),
            tag_cache: None,
            sections_cache: None,
        }
    }

    /// An app on a database in `dir`, without a window (for tests).
    #[cfg(test)]
    pub(crate) fn for_test(dir: &std::path::Path) -> Self {
        let mut app = App::with(Library::for_test(dir));
        app.restore_location();
        app
    }

    fn restore_location(&mut self) {
        let (loc, note) = self.lib.initial_location();
        self.location = loc;
        self.select(note);
    }

    /// "All Notes" is in the sidebar once there's more than one folder.
    pub fn show_all_notes(&self) -> bool {
        self.lib.show_all_notes()
    }

    pub fn account_enabled(&self, id: &str) -> bool {
        self.lib.account_enabled(id)
    }

    /// A cheap fingerprint of the notes (no text is read): what's derived
    /// from all of them is worked out again only when it changes.
    fn notes_fingerprint(&self) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        self.lib.store.notes.len().hash(&mut h);
        for a in &self.lib.store.accounts {
            (&a.id, a.enabled).hash(&mut h);
        }
        for n in &self.lib.store.notes {
            (&n.id, n.version, n.body.len(), n.modified, n.created, n.deleted, n.pinned, &n.folder, &n.account).hash(&mut h);
        }
        h.finish()
    }

    /// [`App::list_sections`], from a cache while nothing it depends on
    /// changes (the list is drawn every frame).
    pub fn cached_sections(&mut self) -> Vec<(String, Vec<String>)> {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        self.notes_fingerprint().hash(&mut h);
        self.search.hash(&mut h);
        format!("{:?}", self.location).hash(&mut h);
        format!("{:?}", self.lib.settings.sort_by).hash(&mut h);
        self.lib.settings.group_by_date.hash(&mut h);
        Local::now().date_naive().hash(&mut h);
        let key = h.finish();
        if let Some((k, sections)) = &self.sections_cache {
            if *k == key {
                return sections.clone();
            }
        }
        let sections = self.list_sections();
        self.sections_cache = Some((key, sections.clone()));
        sections
    }

    /// Every tag in the notes shown (sorted), from a cache.
    pub fn all_tags(&mut self) -> Vec<String> {
        let fp = self.notes_fingerprint();
        if let Some((cached, tags)) = &self.tag_cache {
            if *cached == fp {
                return tags.clone();
            }
        }
        let tags = self.lib.all_tags();
        self.tag_cache = Some((fp, tags.clone()));
        tags
    }

    // ---- Notes list ---------------------------------------------------------

    /// The notes the list shows, in order.
    pub fn visible_notes(&self) -> Vec<String> {
        self.lib.visible_notes(&self.location, &self.search)
    }

    /// The notes list in sections ("Pinned", "Today", …), each a list of ids.
    pub fn list_sections(&self) -> Vec<(String, Vec<String>)> {
        self.lib.list_sections(&self.location, &self.search)
    }

    pub fn select(&mut self, id: Option<String>) {
        self.anchor = id.clone();
        if self.selected == id {
            if !self.multi.is_empty() {
                self.multi.clear();
                self.editor = id.and_then(|id| self.editor_for(&id));
            }
            return;
        }
        self.multi.clear();
        self.flush_editor();
        // A new note left empty goes away.
        if let Some(old) = self.selected.take() {
            self.lib.leave_note(&old);
        }
        self.selected = id.clone();
        self.lib.remember_note(id.as_deref());
        self.editor = id.and_then(|id| self.editor_for(&id));
        self.lock.typed.clear();
        self.lock.error = None;
        self.lock.focus = true;
        self.format_open = false;
        self.scroll_list = true;
    }

    pub fn go_to(&mut self, loc: Location) {
        self.lib.remember_location(&loc);
        self.location = loc;
        self.search.clear();
        let first = self.visible_notes().first().cloned();
        self.select(first);
    }

    /// The account and folder new notes go to from here.
    pub fn new_note_target(&self) -> (String, String) {
        self.lib.new_note_target(&self.location)
    }

    pub fn new_note(&mut self) {
        self.flush_editor();
        let (id, body, shown) = self.lib.new_note(&self.location);
        if shown != self.location {
            self.location = shown;
        }
        self.search.clear();
        self.select(Some(id.clone()));
        let mut e = Editor::new_note(&id, &body, self.lib.settings.start_with.kind());
        e.focus_requested = true;
        self.editor = Some(e);
        self.focus = Pane::Note;
    }

    /// The selected notes: several after Ctrl/Shift+Click, else the one.
    pub fn selected_ids(&self) -> Vec<String> {
        if self.multi.len() > 1 {
            self.multi.clone()
        } else {
            self.selected.iter().cloned().collect()
        }
    }

    pub fn is_selected(&self, id: &str) -> bool {
        if self.multi.len() > 1 {
            self.multi.iter().any(|m| m == id)
        } else {
            self.selected.as_deref() == Some(id)
        }
    }

    /// A click on a note in the list or gallery: Ctrl+Click adds or removes
    /// it, Shift+Click selects everything from the last plain click to it.
    pub fn click_note(&mut self, id: &str, m: egui::Modifiers) {
        if m.shift {
            let anchor = self.anchor.clone().or_else(|| self.selected.clone()).unwrap_or_else(|| id.to_owned());
            let a = self.shown.iter().position(|s| *s == anchor);
            let b = self.shown.iter().position(|s| s == id);
            let range = match (a, b) {
                (Some(a), Some(b)) => self.shown[a.min(b)..=a.max(b)].to_vec(),
                _ => vec![id.to_owned()],
            };
            self.set_multi(range, id, Some(anchor));
        } else if m.command {
            let mut set = self.selected_ids();
            if let Some(i) = set.iter().position(|s| s == id) {
                if set.len() > 1 {
                    set.remove(i);
                }
            } else {
                set.push(id.to_owned());
            }
            let primary = if set.iter().any(|s| s == id) { id.to_owned() } else { set.last().cloned().unwrap_or_default() };
            self.set_multi(set, &primary, Some(id.to_owned()));
        } else {
            self.select(Some(id.to_owned()));
        }
    }

    /// Selects `set` (in list order), showing `primary` if it's just one.
    fn set_multi(&mut self, mut set: Vec<String>, primary: &str, anchor: Option<String>) {
        set.sort_by_key(|s| self.shown.iter().position(|x| x == s).unwrap_or(usize::MAX));
        set.dedup();
        self.select(Some(primary.to_owned()));
        self.anchor = anchor;
        if set.len() > 1 {
            self.flush_editor();
            self.editor = None;
            self.format_open = false;
            self.multi = set;
        }
    }

    pub fn duplicate_note(&mut self, id: &str) {
        self.flush_editor();
        let copy = self.lib.duplicate_note(id);
        if copy.is_some() {
            self.select(copy);
        }
    }

    /// Moves notes to Recently Deleted, or out of the app for good if they
    /// were there already (after asking).
    pub fn delete_notes(&mut self, ids: Vec<String>) {
        if ids.is_empty() {
            return;
        }
        if self.lib.all_in_deleted(&ids) {
            self.confirm = Some(Confirm::DeleteNotesForever(ids));
            return;
        }
        self.flush_editor();
        let next = self.neighbour_of(&ids);
        self.lib.trash_notes(&ids, &self.location);
        self.selected = None;
        self.editor = None;
        self.select(next);
    }

    /// Takes back the last deletion (⌘Z outside a note's text): the notes
    /// come out of Recently Deleted, pinned and dated as they were, shown
    /// where they were.
    pub fn undo_delete(&mut self) -> bool {
        let Some(r) = self.lib.undo_delete() else { return false };
        if self.lib.location_exists(&r.location) && self.location != r.location {
            self.go_to(r.location.clone());
        }
        if !self.visible_notes().contains(&r.ids[0]) {
            let folder = self.lib.store.note(&r.ids[0]).map(|n| n.folder.clone()).unwrap_or_default();
            self.go_to(Location::Folder(folder));
        }
        // `shown` is last frame's list, so put the notes in it for ordering.
        self.shown = self.visible_notes();
        let first = r.ids[0].clone();
        self.set_multi(r.ids, &first, Some(first.clone()));
        self.focus = Pane::List;
        true
    }

    /// Deletes again what ⌘Z brought back (⇧⌘Z).
    pub fn redo_delete(&mut self) -> bool {
        self.flush_editor();
        let Some(ids) = self.lib.redo_delete() else { return false };
        let next = self.neighbour_of(&ids);
        if self.selected_ids().iter().any(|s| ids.contains(s)) {
            self.selected = None;
            self.editor = None;
            self.multi.clear();
            self.select(next);
        }
        true
    }

    fn neighbour_of(&self, ids: &[String]) -> Option<String> {
        let shown = &self.shown;
        let i = shown.iter().position(|s| ids.contains(s))?;
        shown.iter().skip(i + 1).find(|s| !ids.contains(s)).or_else(|| shown[..i].iter().rev().find(|s| !ids.contains(s))).cloned()
    }

    pub fn delete_forever(&mut self, ids: &[String]) {
        let next = self.neighbour_of(ids);
        self.lib.delete_forever(ids);
        let open_gone = self.selected.as_ref().is_some_and(|s| ids.contains(s));
        // Also drops them from a selection of several notes.
        self.prune_missing();
        if open_gone && self.selected.is_none() {
            self.select(next);
        }
    }

    pub fn recover_note(&mut self, id: &str) {
        self.lib.recover_note(id);
    }

    pub fn toggle_pin(&mut self, id: &str) {
        self.lib.toggle_pin(id);
    }

    /// Moves notes to a folder, possibly in another account. The selection
    /// then moves on to a note still shown, if they've left the list.
    pub fn move_notes(&mut self, ids: &[String], folder: &str) {
        self.flush_editor();
        // Worked out before anything moves, so it's never one of them.
        let next = self.neighbour_of(ids);
        self.lib.move_notes(ids, folder);
        self.retain_visible(next);
    }

    /// After the search text changed. Notes the search hides drop out of the
    /// selection (so a Delete doesn't reach them). Once the search is
    /// cleared, a note opened from its results stays open: the list moves
    /// to its folder.
    pub fn search_changed(&mut self) {
        if self.search.trim().is_empty() && self.multi.is_empty() {
            if let Some(id) = self.selected.clone().filter(|id| !self.visible_notes().contains(id)) {
                if let Some(n) = self.lib.store.note(&id).filter(|n| n.deleted.is_none() && self.account_enabled(&n.account)) {
                    // Not `go_to`, which would open the folder's first note.
                    let loc = Location::Folder(n.folder.clone());
                    self.lib.remember_location(&loc);
                    self.location = loc;
                    self.scroll_list = true;
                    return;
                }
            }
        }
        self.retain_visible(None);
    }

    /// Drops notes the list no longer shows from the selection. If the open
    /// note is one of them, `fallback` (if shown) or the first shown note is
    /// opened instead.
    pub fn retain_visible(&mut self, fallback: Option<String>) {
        let visible = self.visible_notes();
        let pick = |fallback: Option<String>| fallback.filter(|f| visible.contains(f)).or_else(|| visible.first().cloned());
        if !self.multi.is_empty() {
            self.multi.retain(|id| visible.contains(id));
            if self.multi.len() > 1 {
                if !self.selected.as_ref().is_some_and(|s| self.multi.contains(s)) {
                    self.selected = self.multi.first().cloned();
                }
                return;
            }
            // Down to one (or none): show it on its own.
            let keep = self.multi.pop().or_else(|| pick(fallback));
            self.selected = None;
            self.editor = None;
            self.select(keep);
            return;
        }
        if self.selected.as_ref().is_some_and(|s| !visible.contains(s)) {
            self.select(pick(fallback));
        }
    }

    // ---- Folders ------------------------------------------------------------

    pub fn new_folder(&mut self, account: Option<String>, parent: Option<String>) {
        let account = account.unwrap_or_else(|| self.new_note_target().0);
        let id = self.lib.new_folder(&account, parent.as_deref(), None);
        let name = self.lib.store.folder(&id).map(|f| f.name.clone()).unwrap_or_default();
        self.rename = Some(Rename { folder: id.clone(), text: name, focus: true });
        self.go_to(Location::Folder(id));
        self.focus = Pane::Sidebar;
    }

    /// Moves a folder (and what's in it) into another folder, or to the top
    /// level of an account.
    pub fn move_folder(&mut self, id: &str, account: &str, parent: Option<&str>) {
        self.flush_editor();
        self.lib.move_folder(id, account, parent);
    }

    /// Moves a folder to just before `before` among `parent`'s folders (or
    /// after the last), dragged there in the sidebar.
    pub fn place_folder(&mut self, id: &str, account: &str, parent: Option<&str>, before: Option<&str>) {
        self.flush_editor();
        self.lib.place_folder(id, account, parent, before);
    }

    /// Finishes naming a folder (a name that's taken says so instead).
    pub fn finish_rename(&mut self) {
        let Some(r) = self.rename.take() else { return };
        if let Err(msg) = self.lib.rename_folder(&r.folder, &r.text) {
            self.push_notice(msg);
        }
    }

    /// Deletes a folder and the folders in it; their notes go to Recently
    /// Deleted.
    pub fn delete_folder(&mut self, id: &str) {
        self.flush_editor();
        let account = self.lib.store.folder(id).map(|f| f.account.clone());
        let gone = self.lib.delete_folder(id);
        if matches!(&self.location, Location::Folder(l) if gone.contains(l)) {
            let home = account.map(|a| self.lib.home_folder_id(&a)).unwrap_or_default();
            self.go_to(Location::Folder(home));
        } else {
            self.retain_visible(None);
        }
    }

    pub fn empty_deleted(&mut self, account: &str) {
        let ids: Vec<String> =
            self.lib.store.notes.iter().filter(|n| n.account == account && n.deleted.is_some()).map(|n| n.id.clone()).collect();
        self.delete_forever(&ids);
    }

    // ---- Editing ------------------------------------------------------------

    /// Copies the editor's changes into the note.
    pub fn flush_editor(&mut self) {
        let Some(e) = &mut self.editor else { return };
        let Some(md) = e.take_markdown() else { return };
        let id = e.note_id.clone();
        if self.lib.set_body(&id, &md) == BodySet::Closed {
            self.editor = None;
        }
    }

    /// Turns "Edit as Markdown" on or off (for every note, until turned
    /// off again).
    pub fn toggle_markdown_mode(&mut self) {
        self.flush_editor();
        self.markdown_mode = !self.markdown_mode;
        self.markdown_source = None;
        self.format_open = false;
        if let Some(e) = &mut self.editor {
            e.find.open = false;
            e.focus_requested = true;
            self.focus = Pane::Note;
        }
    }

    /// The open note's Markdown, edited by hand: stored as written.
    pub fn set_markdown(&mut self, md: &str) {
        let Some(e) = &mut self.editor else { return };
        if self.lib.store.note(&e.note_id).is_some_and(|n| n.deleted.is_some()) {
            return;
        }
        e.set_markdown(md);
        let id = e.note_id.clone();
        if self.lib.set_body(&id, md) == BodySet::Closed {
            self.editor = None;
        }
    }

    pub fn push_notice(&mut self, s: String) {
        self.notices.push((s, Instant::now()));
    }

    // ---- Syncing ------------------------------------------------------------

    pub fn sync_now(&mut self, account: &str) {
        self.flush_editor();
        self.lib.sync_now(account);
    }

    pub fn syncing(&self, account: &str) -> bool {
        self.lib.syncing(account)
    }

    /// After a sync (or the purge of Recently Deleted) changed notes: the
    /// open note and the selection follow.
    fn follow_changes(&mut self) {
        if let Some(id) = self.editor.as_ref().map(|e| e.note_id.clone()) {
            if self.lib.store.note(&id).is_none() {
                self.editor = None;
                self.selected = None;
            } else {
                // A locked note's text, if it still opens.
                match self.lib.plain_body_now(&id) {
                    Some(text) => {
                        if self.editor.as_mut().unwrap().reload(&text) {
                            // "Edit as Markdown" shows the new text.
                            self.markdown_source = None;
                        }
                    }
                    None => self.editor = None,
                }
            }
        }
        if !self.lib.location_exists(&self.location) {
            if let Location::Folder(_) = self.location {
                let (_, folder) = self.new_note_target();
                self.location = Location::Folder(folder);
            }
        }
        self.prune_missing();
        // Selected notes the sync moved to Recently Deleted leave the list:
        // don't keep them open (or let Delete/Move reach them).
        let in_deleted = matches!(self.location, Location::Deleted(_));
        if !in_deleted && self.selected_ids().iter().any(|id| self.lib.store.note(id).is_some_and(|n| n.deleted.is_some())) {
            self.retain_visible(None);
        }
    }

    /// What happens over time, each frame: sync replies, syncs that are
    /// due, purging Recently Deleted, auto-lock.
    fn background(&mut self, ctx: &egui::Context) {
        // Syncs must see the open note as it is now.
        self.flush_editor();
        let ticked = self.lib.tick();
        if ticked.auto_locked {
            self.after_lock_now();
        }
        if ticked.changed || ticked.auto_locked {
            self.follow_changes();
        }
        if ticked.attachments {
            self.images.retry_missing();
        }
        // Checked (in `tick`) before this frame's input counts: coming back
        // to a window left minimized for an hour brings input with it.
        if ctx.input(|i| !i.events.is_empty() || i.pointer.is_moving()) {
            self.lib.touch_lock();
        }
    }

    /// Drops notes that no longer exist (purged, deleted by a sync) from
    /// the selection.
    fn prune_missing(&mut self) {
        if self.selected.as_ref().is_some_and(|s| self.lib.store.note(s).is_none()) {
            self.selected = None;
            self.editor = None;
        }
        if self.multi.is_empty() {
            return;
        }
        let store = &self.lib.store;
        self.multi.retain(|id| store.note(id).is_some());
        if self.multi.len() < 2 {
            let keep = self.multi.pop();
            self.selected = None;
            self.editor = None;
            self.select(keep);
        } else if self.selected.is_none() {
            self.selected = self.multi.first().cloned();
        }
    }

    // ---- Keyboard -------------------------------------------------------------

    fn shortcuts(&mut self, ctx: &egui::Context) {
        let cmd = Modifiers::COMMAND;
        let shift_cmd = Modifiers::COMMAND | Modifiers::SHIFT;
        let alt_cmd = Modifiers::COMMAND | Modifiers::ALT;
        // A modal question takes the keyboard (Enter, Esc) for itself.
        if self.confirm.is_some() {
            return;
        }
        // Modifiers must match exactly: egui's own matching ignores extra
        // Shift/Alt, so ↓ would swallow Shift+↓ and ⌘N would swallow ⇧⌘N.
        let pressed = |m: Modifiers, k: Key| ctx.input_mut(|i| consume_exact(i, m, k));
        // For keys whose Shift depends on the keyboard layout (+, =).
        let pressed_loose = |m: Modifiers, k: Key| ctx.input_mut(|i| i.consume_shortcut(&KeyboardShortcut::new(m, k)));
        if pressed(cmd, Key::N) {
            self.new_note();
        }
        if pressed(shift_cmd, Key::N) {
            self.new_folder(None, None);
        }
        if pressed(alt_cmd, Key::F) {
            self.search_focus = true;
        }
        if pressed(shift_cmd, Key::E) && self.selected.is_some() && self.multi.len() < 2 {
            self.toggle_markdown_mode();
        }
        // Find in Note finds in the formatted note, not its Markdown.
        let find_in_note = !self.markdown_mode;
        if pressed(cmd, Key::F) {
            match &mut self.editor {
                Some(e) if find_in_note => {
                    e.find.open = true;
                    e.find.focus = true;
                }
                _ => self.search_focus = true,
            }
        }
        if pressed(cmd, Key::G) && find_in_note {
            if let Some(e) = &mut self.editor {
                e.find_step(1);
            }
        }
        if pressed(shift_cmd, Key::G) && find_in_note {
            if let Some(e) = &mut self.editor {
                e.find_step(-1);
            }
        }
        if pressed(cmd, Key::Comma) {
            self.settings_ui.open = true;
        }
        if pressed(cmd, Key::Num1) {
            self.lib.settings.view = ListView::List;
        }
        if pressed(cmd, Key::Num2) {
            self.lib.settings.view = ListView::Gallery;
        }
        if pressed(alt_cmd, Key::S) {
            self.lib.settings.sidebar_visible = !self.lib.settings.sidebar_visible;
        }
        if pressed(cmd, Key::D) {
            if let Some(id) = self.selected.clone() {
                self.duplicate_note(&id);
            }
        }
        // Text size: ⌘+ / ⌘- / ⌘0.
        let sizes = crate::settings::TEXT_SIZES;
        let size = &mut self.lib.settings.text_size;
        if pressed_loose(cmd, Key::Plus) || pressed_loose(cmd, Key::Equals) {
            *size = sizes.iter().copied().find(|&s| s > *size).unwrap_or(sizes[sizes.len() - 1]);
        }
        if pressed_loose(cmd, Key::Minus) {
            *size = sizes.iter().rev().copied().find(|&s| s < *size).unwrap_or(sizes[0]);
        }
        if pressed(cmd, Key::Num0) {
            *size = Settings::default().text_size;
        }
        // ⌘Z / ⇧⌘Z take back note deletions, unless something with text
        // (a note, the search field) has the keyboard and undoes its own.
        if !ctx.wants_keyboard_input() {
            if pressed(shift_cmd, Key::Z) || (!cfg!(target_os = "macos") && pressed(cmd, Key::Y)) {
                self.redo_delete();
            } else if pressed(cmd, Key::Z) {
                self.undo_delete();
            }
        }
        // Not while typing: there Ctrl+Backspace deletes a word.
        if !ctx.wants_keyboard_input() && (pressed(cmd, Key::Backspace) || pressed(cmd, Key::Delete)) {
            self.delete_notes(self.selected_ids());
        }
        // The notes list.
        if self.focus == Pane::List && self.rename.is_none() && !ctx.wants_keyboard_input() {
            if pressed(Modifiers::NONE, Key::ArrowDown) {
                self.step_selection(1);
            }
            if pressed(Modifiers::NONE, Key::ArrowUp) {
                self.step_selection(-1);
            }
            // Shift+↑/↓ select a range, like Shift+Click.
            if pressed(Modifiers::SHIFT, Key::ArrowDown) {
                self.extend_selection(1);
            }
            if pressed(Modifiers::SHIFT, Key::ArrowUp) {
                self.extend_selection(-1);
            }
            if pressed(cmd, Key::A) && !self.shown.is_empty() {
                let all = self.shown.clone();
                let primary = self.selected.clone().unwrap_or_else(|| all[0].clone());
                let anchor = self.anchor.clone();
                self.set_multi(all, &primary, anchor);
            }
            if pressed(Modifiers::NONE, Key::Delete) || pressed(Modifiers::NONE, Key::Backspace) {
                self.delete_notes(self.selected_ids());
            }
            if pressed(Modifiers::NONE, Key::Enter) || pressed(Modifiers::NONE, Key::Tab) {
                if let Some(e) = &mut self.editor {
                    e.focus_requested = true;
                    self.focus = Pane::Note;
                }
            }
        }
    }

    /// Grows or shrinks a Shift selection by one note.
    fn extend_selection(&mut self, d: i32) {
        let shown = self.shown.clone();
        if shown.is_empty() {
            return;
        }
        let Some(cur) = self.selected.clone().filter(|s| shown.contains(s)) else { return self.step_selection(d) };
        let anchor = self.anchor.clone().unwrap_or_else(|| cur.clone());
        // The moving end is the selected note farthest from the anchor.
        let ids = self.selected_ids();
        let a = shown.iter().position(|s| *s == anchor).unwrap_or(0);
        let end = ids.iter().filter_map(|s| shown.iter().position(|x| x == s)).max_by_key(|&i| (i as i64 - a as i64).abs()).unwrap_or(a);
        let next = (end as i64 + d as i64).clamp(0, shown.len() as i64 - 1) as usize;
        let range = shown[a.min(next)..=a.max(next)].to_vec();
        self.set_multi(range, &shown[next].clone(), Some(anchor));
        self.scroll_list = true;
    }

    fn step_selection(&mut self, d: i32) {
        let shown = self.shown.clone();
        if shown.is_empty() {
            return;
        }
        let i = self.selected.as_ref().and_then(|s| shown.iter().position(|x| x == s));
        let next = match i {
            Some(i) => (i as i32 + d).clamp(0, shown.len() as i32 - 1) as usize,
            None => 0,
        };
        self.select(Some(shown[next].clone()));
    }

    pub fn editor_command(&mut self, c: Command) {
        if self.markdown_mode {
            return;
        }
        if let Some(e) = &mut self.editor {
            if self.lib.store.note(&e.note_id).is_some_and(|n| n.deleted.is_some()) {
                return;
            }
            e.command(c);
            e.focus_requested = true;
            self.focus = Pane::Note;
        }
    }

    pub fn note_kind(&self) -> Kind {
        self.editor.as_ref().map_or(Kind::Body, |e| e.current_kind())
    }

    // ---- Dialogs ---------------------------------------------------------

    fn confirm_dialog(&mut self, ctx: &egui::Context, p: &Palette) {
        let Some(c) = &self.confirm else { return };
        let store = &self.lib.store;
        let (title, text, button) = match c {
            Confirm::DeleteNotesForever(ids) => (
                if ids.len() == 1 { "Delete this note?".to_owned() } else { format!("Delete {} notes?", ids.len()) },
                "This action can't be undone.".to_owned(),
                "Delete",
            ),
            Confirm::DeleteFolder(id) => (
                format!("Delete the folder “{}”?", store.folder(id).map_or("", |f| f.name.as_str())),
                "All notes and subfolders in this folder will be deleted. Notes will be kept in Recently Deleted for 30 days.".to_owned(),
                "Delete Folder",
            ),
            Confirm::EmptyDeleted(_) => (
                "Delete all notes in Recently Deleted?".to_owned(),
                "This action can't be undone.".to_owned(),
                "Delete All",
            ),
            Confirm::RemoveAccount(id) => (
                format!("Remove the account “{}”?", store.account(id).map_or("", |a| a.name.as_str())),
                if id == crate::model::LOCAL {
                    let n = self.lib.account_note_count(id);
                    let notes = if n == 1 { "Its 1 note is".to_owned() } else { format!("Its {n} notes are") };
                    format!("{notes} deleted for good: they're only on this computer. You can add the account back from Settings → Accounts → +, empty.")
                } else {
                    "Its notes are removed from this computer. They stay where they're synced to.".to_owned()
                },
                "Remove",
            ),
        };
        let mut answer = None;
        modal(ctx, "confirm", p, |ui| {
            ui.set_width(300.0);
            ui.vertical_centered(|ui| {
                ui.add_space(4.0);
                ui.label(egui::RichText::new(&title).font(theme::bold_font(14.0)).color(p.text));
                ui.add_space(2.0);
                ui.label(egui::RichText::new(&text).size(12.0).color(p.secondary));
                ui.add_space(10.0);
            });
            ui.horizontal(|ui| {
                let w = (ui.available_width() - 8.0) / 2.0;
                if ui.add_sized([w, 26.0], egui::Button::new("Cancel")).clicked() || ui.input(|i| i.key_pressed(Key::Escape)) {
                    answer = Some(false);
                }
                let b = egui::Button::new(egui::RichText::new(button).color(egui::Color32::WHITE)).fill(theme::RED);
                if ui.add_sized([w, 26.0], b).clicked() || ui.input(|i| i.key_pressed(Key::Enter)) {
                    answer = Some(true);
                }
            });
        });
        match answer {
            Some(true) => match self.confirm.take().unwrap() {
                Confirm::DeleteNotesForever(ids) => self.delete_forever(&ids),
                Confirm::DeleteFolder(id) => self.delete_folder(&id),
                Confirm::EmptyDeleted(a) => self.empty_deleted(&a),
                Confirm::RemoveAccount(a) => self.remove_account(&a),
            },
            Some(false) => self.confirm = None,
            None => {}
        }
    }

    pub fn remove_account(&mut self, id: &str) {
        self.flush_editor();
        self.lib.remove_account(id);
        if !self.lib.location_exists(&self.location) {
            let f = self.lib.default_folder_id(&self.lib.fallback_account());
            self.go_to(Location::Folder(f));
        } else {
            self.prune_missing();
        }
    }

    /// Adds a signed-in account (from the Accounts settings).
    pub fn add_account(&mut self, account: crate::model::Account, secret: Option<String>) {
        self.flush_editor();
        self.lib.add_account(account, secret);
    }

    fn notices_bar(&mut self, ctx: &egui::Context, p: &Palette) {
        for n in self.lib.take_notices() {
            self.push_notice(n);
        }
        self.notices.retain(|(_, t)| t.elapsed() < StdDuration::from_secs(6));
        let Some((text, _)) = self.notices.last() else { return };
        let text = text.clone();
        egui::Area::new(egui::Id::new("notice"))
            .anchor(egui::Align2::CENTER_BOTTOM, egui::vec2(0.0, -18.0))
            .order(egui::Order::Foreground)
            .show(ctx, |ui| {
                egui::Frame::none()
                    .fill(p.popover)
                    .rounding(10.0)
                    .stroke(egui::Stroke::new(1.0_f32, p.separator))
                    .shadow(egui::epaint::Shadow { offset: egui::vec2(0.0, 4.0), blur: 16.0, spread: 0.0, color: egui::Color32::from_black_alpha(40) })
                    .inner_margin(egui::Margin::symmetric(16.0, 10.0))
                    .show(ui, |ui| {
                        ui.set_max_width(420.0);
                        ui.label(egui::RichText::new(text).color(p.text));
                    });
            });
        ctx.request_repaint_after(StdDuration::from_secs(1));
    }
}

/// Shown instead of the app when the notes database can't be opened. (The
/// app has no console, so a panic would just mean no window at all.)
pub struct StartupError(String);

impl eframe::App for StartupError {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        egui::CentralPanel::default().show(ctx, |ui| {
            ui.vertical_centered(|ui| {
                ui.add_space(ui.available_height() * 0.3);
                ui.heading("Your notes can't be opened right now");
                ui.add_space(8.0);
                ui.label("Another program (a backup or sync tool, or CleanNotes itself) may be using them. Close it and start CleanNotes again.");
                ui.add_space(8.0);
                ui.label(egui::RichText::new(&self.0).weak().small());
            });
        });
    }
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, frame: &mut eframe::Frame) {
        let started = std::time::Instant::now();
        self.frame(ctx, frame);
        slow_frame(started.elapsed(), self.lib.store.notes.len());
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        self.flush_editor();
        if let Some(sel) = self.selected.clone() {
            // An empty new note goes on quitting, too.
            self.lib.leave_note(&sel);
        }
        self.lib.save(true);
    }
}

/// Notes slow frames (they make moving the window lag) in `frames.log`
/// in the data folder, at most once a second.
fn slow_frame(took: std::time::Duration, notes: usize) {
    static LAST: std::sync::Mutex<Option<std::time::Instant>> = std::sync::Mutex::new(None);
    if took < std::time::Duration::from_millis(30) {
        return;
    }
    let mut last = LAST.lock().unwrap();
    if last.is_some_and(|l| l.elapsed() < std::time::Duration::from_secs(1)) {
        return;
    }
    *last = Some(std::time::Instant::now());
    let path = crate::db::data_dir().join("frames.log");
    if std::fs::metadata(&path).is_ok_and(|m| m.len() > 1_000_000) {
        let _ = std::fs::remove_file(&path);
    }
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&path) {
        use std::io::Write;
        let _ = writeln!(f, "{} slow frame: {:.1} ms ({notes} notes)", Local::now().format("%Y-%m-%d %H:%M:%S"), took.as_secs_f64() * 1000.0);
    }
}

impl App {
    fn frame(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        theme::apply_style(ctx);
        let p = Palette::get(ctx);
        self.background(ctx);
        self.shortcuts(ctx);

        if self.lib.settings.sidebar_visible {
            egui::SidePanel::left("sidebar")
                .resizable(true)
                .default_width(240.0)
                .width_range(190.0..=380.0)
                .show_separator_line(false)
                .frame(egui::Frame::none().fill(p.list_bg))
                .show(ctx, |ui| self.sidebar(ui, &p));
        }
        let list_w = if self.lib.settings.view == ListView::Gallery { 0.0 } else { 300.0 };
        if self.lib.settings.view == ListView::List {
            egui::SidePanel::left("notes-list")
                .resizable(true)
                .default_width(list_w)
                .width_range(220.0..=480.0)
                .show_separator_line(false)
                .frame(egui::Frame::none().fill(p.list_bg))
                .show(ctx, |ui| self.notes_list(ui, &p));
        }
        egui::CentralPanel::default().frame(egui::Frame::none().fill(p.bg)).show(ctx, |ui| {
            if self.lib.settings.view == ListView::Gallery {
                self.gallery(ui, &p);
            } else {
                self.note_pane(ui, &p);
            }
        });

        self.flush_editor();
        self.confirm_dialog(ctx, &p);
        self.lock_sheets(ctx, &p);
        self.settings_window(ctx, &p);
        self.dropped_files(ctx);
        self.import_sheet(ctx, &p);
        self.notices_bar(ctx, &p);
        self.lib.save(false);
        ctx.request_repaint_after(StdDuration::from_secs(10));
    }
}

/// The search field's id, so others can tell when it has the keyboard.
pub fn search_field_id() -> egui::Id {
    egui::Id::new("search-field")
}

/// Consumes a press of `key` with exactly these modifiers (⌘ meaning Ctrl
/// off macOS). Unlike egui's `consume_shortcut`, extra Shift/Alt don't match.
pub fn consume_exact(i: &mut egui::InputState, modifiers: Modifiers, key: Key) -> bool {
    let mut found = false;
    i.events.retain(|e| {
        let hit = matches!(e, egui::Event::Key { key: k, pressed: true, modifiers: m, .. } if *k == key && m.matches_exact(modifiers));
        found |= hit;
        !hit
    });
    found
}

/// A dialog in the middle of the window, dimming everything behind it.
pub fn modal<R>(ctx: &egui::Context, id: &str, p: &Palette, add: impl FnOnce(&mut egui::Ui) -> R) -> Option<R> {
    let screen = ctx.screen_rect();
    egui::Area::new(egui::Id::new((id, "dim")))
        .order(egui::Order::Middle)
        .fixed_pos(screen.min)
        .show(ctx, |ui| {
            let (r, resp) = ui.allocate_exact_size(screen.size(), egui::Sense::click_and_drag());
            let _ = resp;
            ui.painter().rect_filled(r, 0.0, egui::Color32::from_black_alpha(if p.dark { 110 } else { 60 }));
        });
    egui::Window::new(id)
        .id(egui::Id::new((id, "window")))
        .title_bar(false)
        .resizable(false)
        .collapsible(false)
        .order(egui::Order::Foreground)
        .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
        .frame(
            egui::Frame::window(&ctx.style())
                .fill(p.popover)
                .inner_margin(egui::Margin::same(18.0))
                .rounding(12.0),
        )
        .show(ctx, add)
        .and_then(|r| r.inner)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cached_list_and_tags_follow_every_change() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = App::for_test(dir.path());
        let folder = app.lib.store.default_folder(crate::model::LOCAL).unwrap().id.clone();
        let a = Note::new(crate::model::LOCAL, &folder, "# A\n#one\n");
        let b = Note::new(crate::model::LOCAL, &folder, "# B\n");
        let (ia, ib) = (a.id.clone(), b.id.clone());
        app.lib.store.notes.extend([a, b]);
        app.go_to(Location::Folder(folder.clone()));
        let check = |app: &mut App| assert_eq!(app.cached_sections(), app.list_sections());
        check(&mut app);
        assert_eq!(app.all_tags(), vec!["one".to_owned()]);
        // Pinned, deleted, edited, searched, moved: never a stale list.
        app.toggle_pin(&ib);
        check(&mut app);
        app.lib.store.notes.iter_mut().find(|n| n.id == ia).unwrap().deleted = Some(Utc::now());
        check(&mut app);
        assert!(app.all_tags().is_empty(), "a deleted note's tags go");
        let n = app.lib.store.notes.iter_mut().find(|n| n.id == ib).unwrap();
        n.body = "# B\n#two\n".into();
        n.touch();
        check(&mut app);
        assert_eq!(app.all_tags(), vec!["two".to_owned()]);
        app.search = "zzz".into();
        check(&mut app);
        assert!(app.cached_sections().iter().all(|(_, ids)| ids.is_empty()));
        app.search.clear();
        let other = app.lib.store.ensure_folder_at(crate::model::LOCAL, &["Other".into()]);
        app.lib.store.notes.iter_mut().find(|n| n.id == ib).unwrap().folder = other;
        check(&mut app);
    }

    use crate::model::Note;
    use chrono::Utc;

    // ---- The app's selection logic, without a window -----------------------

    fn app() -> (App, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let app = App::for_test(dir.path());
        (app, dir)
    }

    impl App {
        /// A note in `folder`, saved as if typed (not a fresh empty one).
        fn add(&mut self, folder: &str, body: &str) -> String {
            let mut n = Note::new(crate::model::LOCAL, folder, body);
            // Distinct dates keep the list order fixed: later adds first.
            n.modified = Utc::now() + chrono::Duration::seconds(self.lib.store.notes.len() as i64);
            let id = n.id.clone();
            self.lib.store.notes.push(n);
            id
        }

        fn folder_named(&mut self, name: &str) -> String {
            self.lib.store.ensure_folder_at(crate::model::LOCAL, &[name.to_owned()])
        }

        fn home(&self) -> String {
            self.lib.default_folder_id(crate::model::LOCAL)
        }

        /// What the list would show this frame.
        fn draw(&mut self) {
            self.shown = self.visible_notes();
        }
    }

    #[test]
    fn a_new_note_in_all_notes_stays_in_view_and_is_remembered() {
        let (mut app, _dir) = app();
        app.folder_named("Work");
        app.go_to(Location::All);
        app.new_note();
        assert_eq!(app.location, Location::All);
        assert!(app.visible_notes().contains(app.selected.as_ref().unwrap()));
        assert_eq!(app.lib.initial_location().0, Location::All);
    }

    #[test]
    fn moving_several_notes_leaves_a_shown_note_selected() {
        let (mut app, _dir) = app();
        let home = app.home();
        let other = app.folder_named("Other");
        let a = app.add(&home, "# A\n");
        let b = app.add(&home, "# B\n");
        let c = app.add(&home, "# C\n");
        let keep = app.add(&home, "# Keep\n");
        app.go_to(Location::Folder(home.clone()));
        app.draw();
        app.click_note(&c, Modifiers::NONE);
        app.click_note(&a, Modifiers::SHIFT);
        assert_eq!(app.selected_ids().len(), 3);
        app.move_notes(&app.selected_ids(), &other);
        app.draw();
        assert_eq!(app.shown, vec![keep.clone()]);
        assert_eq!(app.selected.as_deref(), Some(keep.as_str()));
        assert!(app.multi.is_empty());
        assert_eq!(app.editor.as_ref().map(|e| e.note_id.clone()), Some(keep));
        // All of them moved: nothing (rather than a hidden note) is open.
        let _ = b;
        app.click_note(&app.shown[0].clone(), Modifiers::NONE);
        app.move_notes(&app.selected_ids(), &other);
        assert!(app.selected.is_none() && app.editor.is_none());
    }

    #[test]
    fn a_search_drops_hidden_notes_from_the_selection() {
        let (mut app, _dir) = app();
        let home = app.home();
        for i in 0..5 {
            app.add(&home, &format!("# Note {i}\n"));
        }
        let apple = app.add(&home, "# Apple pie\n");
        app.go_to(Location::Folder(home.clone()));
        app.draw();
        app.click_note(&apple, Modifiers::NONE);
        app.shown = app.visible_notes();
        let all = app.shown.clone();
        app.set_multi(all, &apple, None);
        assert_eq!(app.selected_ids().len(), 6);
        app.search = "apple".into();
        app.retain_visible(None);
        assert_eq!(app.selected_ids(), vec![apple.clone()], "Delete must only reach what's shown");
        assert!(app.editor.is_some());
        // A search that hides the open note opens the first hit instead.
        app.search = "note 3".into();
        app.retain_visible(None);
        assert_eq!(app.selected_ids(), app.visible_notes());
    }

    #[test]
    fn clearing_the_search_keeps_the_found_note_open_in_its_folder() {
        let (mut app, _dir) = app();
        let home = app.home();
        let elsewhere = app.folder_named("Elsewhere");
        app.add(&home, "# Here\n");
        let found = app.add(&elsewhere, "# Found it\n");
        app.go_to(Location::Folder(home));
        app.search = "found".into();
        app.search_changed();
        app.select(Some(found.clone()));
        app.search.clear();
        app.search_changed();
        assert_eq!(app.selected, Some(found.clone()));
        assert_eq!(app.location, Location::Folder(elsewhere));
        assert!(app.visible_notes().contains(&found));
    }

    #[test]
    fn a_new_note_in_a_tag_carries_the_tag() {
        let (mut app, _dir) = app();
        let home = app.home();
        app.add(&home, "# Plan\n#work\n");
        app.go_to(Location::Tag("work".into()));
        app.new_note();
        assert_eq!(app.location, Location::Tag("work".into()));
        let id = app.selected.clone().unwrap();
        assert!(app.lib.store.note(&id).unwrap().tags().contains(&"work".to_owned()));
        assert!(app.visible_notes().contains(&id));
    }

    #[test]
    fn undoing_a_deletion_shows_and_selects_the_notes_again() {
        let (mut app, _dir) = app();
        let home = app.home();
        let a = app.add(&home, "# A\n");
        let b = app.add(&home, "# B\n");
        app.go_to(Location::Folder(home.clone()));
        app.draw();
        app.click_note(&a, Modifiers::NONE);
        app.click_note(&b, Modifiers::COMMAND);
        app.delete_notes(app.selected_ids());
        app.draw();
        assert!(app.shown.is_empty());
        app.go_to(Location::Deleted(crate::model::LOCAL.into()));
        assert!(app.undo_delete());
        assert_eq!(app.location, Location::Folder(home));
        let mut sel = app.selected_ids();
        sel.sort();
        let mut want = vec![a, b];
        want.sort();
        assert_eq!(sel, want);
        assert!(app.redo_delete());
        assert!(app.selected_ids().is_empty());
    }

    #[test]
    fn shift_arrows_on_an_empty_list_do_nothing() {
        let (mut app, _dir) = app();
        let home = app.home();
        let n = app.add(&home, "# Gone from view\n");
        app.selected = Some(n);
        app.shown.clear();
        app.extend_selection(1);
        app.extend_selection(-1);
        app.step_selection(1);
    }

    #[test]
    fn purging_drops_notes_from_a_selection_of_several() {
        let (mut app, _dir) = app();
        let home = app.home();
        let ids: Vec<String> = (0..3).map(|i| app.add(&home, &format!("# {i}\n"))).collect();
        app.go_to(Location::Folder(home));
        app.draw();
        let primary = app.shown[0].clone();
        app.set_multi(ids.clone(), &primary, None);
        let others: Vec<String> = ids.iter().filter(|i| **i != primary).cloned().collect();
        app.delete_forever(&others);
        assert!(app.multi.is_empty());
        assert_eq!(app.selected_ids(), vec![primary]);
        assert!(app.editor.is_some());
    }

    fn key(k: Key, modifiers: Modifiers) -> egui::Event {
        egui::Event::Key { key: k, physical_key: None, pressed: true, repeat: false, modifiers }
    }

    #[test]
    fn markdown_typed_in_edit_as_markdown_is_the_note() {
        let (mut app, _dir) = app();
        let home = app.home();
        let a = app.add(&home, "# A\nplain\n");
        app.go_to(Location::Folder(home));
        app.select(Some(a.clone()));
        app.toggle_markdown_mode();
        assert!(app.markdown_mode);
        app.set_markdown("# A\n_slanted_\n");
        // Stored as typed, and the formatted note follows.
        assert_eq!(app.lib.store.note(&a).unwrap().body, "# A\n_slanted_\n");
        app.flush_editor();
        assert_eq!(app.lib.store.note(&a).unwrap().body, "# A\n_slanted_\n");
        let e = app.editor.as_ref().unwrap();
        assert!(e.doc.blocks[1].runs().iter().all(|(_, s)| s.italic));
        // Formatting commands are for the formatted note only.
        app.editor_command(Command::SetKind(Kind::Title));
        assert_eq!(app.editor.as_ref().unwrap().doc.blocks[1].kind, Kind::Body);
        app.toggle_markdown_mode();
        assert!(!app.markdown_mode && app.markdown_source.is_none());
    }

    #[test]
    fn shortcuts_match_modifiers_exactly() {
        let mut i = egui::InputState::default();
        i.events = vec![key(Key::ArrowDown, Modifiers::SHIFT)];
        assert!(!consume_exact(&mut i, Modifiers::NONE, Key::ArrowDown), "↓ mustn't take Shift+↓");
        assert!(consume_exact(&mut i, Modifiers::SHIFT, Key::ArrowDown));
        assert!(i.events.is_empty());
        let shift_cmd = Modifiers::COMMAND | Modifiers::SHIFT;
        i.events = vec![key(Key::N, shift_cmd), key(Key::G, shift_cmd)];
        assert!(!consume_exact(&mut i, Modifiers::COMMAND, Key::N), "⌘N mustn't take ⇧⌘N");
        assert!(!consume_exact(&mut i, Modifiers::COMMAND, Key::G), "⌘G mustn't take ⇧⌘G");
        assert!(consume_exact(&mut i, shift_cmd, Key::N));
        assert!(consume_exact(&mut i, shift_cmd, Key::G));
        // Key releases aren't presses.
        i.events = vec![egui::Event::Key { key: Key::Z, physical_key: None, pressed: false, repeat: false, modifiers: Modifiers::COMMAND }];
        assert!(!consume_exact(&mut i, Modifiers::COMMAND, Key::Z));
    }
}
