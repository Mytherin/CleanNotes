//! Application state, the three panes (folders, notes list, note), keyboard
//! shortcuts, saving and sync scheduling.

use std::collections::{HashMap, HashSet};
use std::time::{Duration as StdDuration, Instant};

use chrono::{DateTime, Local, Utc};
use eframe::egui::{self, Key, KeyboardShortcut, Modifiers};

use crate::db::{self, Db, Secrets};
use crate::doc::Kind;
use crate::editor::{Command, Editor};
use crate::model::{Folder, Note, Store, Tombstone};
use crate::settings::{ListView, Settings, SortBy};
use crate::sync::engine::{self, AccountSnapshot};
use crate::sync::worker::{Reply, Synced, Worker};
use crate::theme::{self, Palette};

mod list;
pub mod locked;
mod note_pane;
pub mod settings_window;
mod sidebar;

/// Wait this long after the last keystroke before uploading.
const SYNC_AFTER_EDIT: StdDuration = StdDuration::from_secs(3);
const RETRY_AFTER_ERROR: StdDuration = StdDuration::from_secs(60);
const SAVE_EVERY: StdDuration = StdDuration::from_millis(800);
/// Height of the toolbar row along the top of every pane.
pub const TOOLBAR_H: f32 = 52.0;
/// `last_folder` when "All Notes" was showing.
const ALL_NOTES: &str = "all-notes";

/// What's selected in the sidebar.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Location {
    /// Every note in every enabled account (when there's more than one
    /// folder).
    All,
    Folder(String),
    /// An account's Recently Deleted.
    Deleted(String),
    Tag(String),
}

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
    pub store: Store,
    pub settings: Settings,
    db: Db,
    worker: Worker,
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
    /// Notes changed since the last save.
    unsaved: HashSet<String>,
    last_save: Instant,
    last_purge: Instant,
    last_edit: Option<Instant>,
    /// When each account last started a sync (this session).
    sync_started: HashMap<String, Instant>,
    /// The secret each running sync was started with: a reply for a
    /// sign-in that was replaced (or an account removed) meanwhile mustn't
    /// save its session over the new one.
    sync_secret: HashMap<String, Option<String>>,
    pub sync_errors: HashMap<String, String>,
    /// Accounts whose saved sign-in stopped working.
    pub needs_sign_in: HashSet<String>,
    /// Messages shown at the bottom of the window for a few seconds.
    notices: Vec<(String, Instant)>,
    pub rename: Option<Rename>,
    pub confirm: Option<Confirm>,
    pub settings_ui: settings_window::SettingsUi,
    /// New notes made this session: left empty, they're thrown away.
    fresh: HashSet<String>,
    /// The notes list as shown last frame (for ↑/↓).
    pub shown: Vec<String>,
    /// Scroll the list to the selected note.
    pub scroll_list: bool,
    pub format_open: bool,
    /// The gallery shows the selected note full size.
    pub gallery_open: bool,
    /// Deletions ⌘Z can take back, newest last, and those it took back
    /// (for ⇧⌘Z).
    undo_deletes: Vec<Deletion>,
    redo_deletes: Vec<Deletion>,
    /// Locked notes: the password this session, unlocked notes' text.
    pub lock: locked::LockState,
    /// The browser engine Standard Notes requests go through (as in its own
    /// apps), if it could be started.
    #[cfg(any(windows, target_os = "macos"))]
    web: Option<crate::webfetch::WebHost>,
}

/// Notes moved to Recently Deleted together, and how things were before.
struct Deletion {
    /// Each note, whether it was pinned and when it was last edited.
    notes: Vec<(String, bool, DateTime<Utc>)>,
    location: Location,
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
        let dir = db::data_dir();
        let (db, store, settings) = Db::open(&dir, Secrets::Keyring)?;
        let ctx = cc.egui_ctx.clone();
        let worker = Worker::spawn(move || ctx.request_repaint());
        let mut app = App::with(db, store, settings, worker);
        // Standard Notes requests go through the system's browser engine, as
        // in Standard Notes' own apps (see webfetch.rs).
        #[cfg(any(windows, target_os = "macos"))]
        match crate::webfetch::WebHost::start(cc) {
            Ok((host, fetch)) => {
                crate::sync::standardnotes::api::use_browser_engine(fetch);
                app.web = Some(host);
            }
            Err(e) => eprintln!("no browser engine for Standard Notes ({e:#}); using plain HTTP"),
        }
        app.restore_location();
        app.purge_expired();
        if std::env::var_os("CLEANNOTES_SEED").is_some() && app.store.notes.is_empty() {
            app.seed();
        }
        Ok(app)
    }

    fn with(db: Db, store: Store, settings: Settings, worker: Worker) -> Self {
        App {
            location: Location::Folder(String::new()),
            store,
            settings,
            db,
            worker,
            selected: None,
            multi: Vec::new(),
            anchor: None,
            editor: None,
            focus: Pane::List,
            search: String::new(),
            search_focus: false,
            search_open: false,
            unsaved: HashSet::new(),
            last_save: Instant::now(),
            last_purge: Instant::now(),
            last_edit: None,
            sync_started: HashMap::new(),
            sync_secret: HashMap::new(),
            sync_errors: HashMap::new(),
            needs_sign_in: HashSet::new(),
            notices: Vec::new(),
            rename: None,
            confirm: None,
            settings_ui: Default::default(),
            fresh: HashSet::new(),
            shown: Vec::new(),
            scroll_list: true,
            format_open: false,
            gallery_open: false,
            undo_deletes: Vec::new(),
            redo_deletes: Vec::new(),
            lock: Default::default(),
            #[cfg(any(windows, target_os = "macos"))]
            web: None,
        }
    }

    /// An app on a database in `dir`, without a window (for tests).
    #[cfg(test)]
    pub(crate) fn for_test(dir: &std::path::Path) -> Self {
        let (db, store, settings) = Db::open(dir, Secrets::Memory).expect("test database");
        let mut app = App::with(db, store, settings, Worker::spawn(|| {}));
        app.restore_location();
        app
    }

    /// Example notes (`CLEANNOTES_SEED=1` with an empty data folder), for
    /// trying the app out and for screenshots.
    fn seed(&mut self) {
        let notes = folder_id_or_default(&self.store);
        let work = self.store.ensure_folder_at(crate::model::LOCAL, &["Work".into()]);
        self.store.ensure_folder_at(crate::model::LOCAL, &["Work".into(), "Projects".into()]);
        let recipes = self.store.ensure_folder_at(crate::model::LOCAL, &["Recipes".into()]);
        let day = chrono::Duration::days(1);
        let samples: [(&str, &str, chrono::Duration, bool); 6] = [
            (&notes, "# Weekend trip\n## Packing\n- [x] Passport\n- [x] Charger\n- [ ] Rain jacket\n- [ ] Snacks for the train\n## Plan\n1. Train at **9:12** from Central\n2. Check in at the *hotel*\n3. Dinner at [Trattoria Roma](https://example.com)\n\nDon't forget #travel insurance.\n", chrono::Duration::minutes(5), true),
            (&notes, "# Book ideas\n* *The Overstory*, Richard Powers\n* *Piranesi*, Susanna Clarke\n    * also her short stories\n* ~~Dune~~ (read it)\n> A reader lives a thousand lives before he dies.\n", day * 2, false),
            (&work, "# Standup notes\nShipped the **sync engine**; next up: conflict UI.\n- Bitwarden backend\n- CloudKit backend\n- <u>Release</u> on Friday #work\n", chrono::Duration::hours(3), false),
            (&work, "# Snippets\n```\ncargo run --release\ncargo test\n```\nRemember to bump the version.\n", day * 12, false),
            (&recipes, "# Pancakes\n### Ingredients\n- [ ] 200 g flour\n- [ ] 2 eggs\n- [ ] 300 ml milk\n### Method\nWhisk everything, rest for 20 minutes, fry in butter. #cooking\n", day * 40, false),
            (&notes, "# Grocery list\nApples, oat milk, coffee beans, basil\n", day * 400, false),
        ];
        for (folder, body, age, pinned) in samples {
            let mut n = Note::new(crate::model::LOCAL, folder, body);
            n.modified = Utc::now() - age;
            n.created = n.modified - chrono::Duration::hours(1);
            n.pinned = pinned;
            self.unsaved.insert(n.id.clone());
            self.store.notes.push(n);
        }
        let first = self.visible_notes().first().cloned();
        self.select(first);
    }

    fn restore_location(&mut self) {
        if self.settings.last_folder.as_deref() == Some(ALL_NOTES) && self.show_all_notes() {
            self.location = Location::All;
            let visible = self.visible_notes();
            let note = self.settings.last_note.clone().filter(|n| visible.contains(n));
            let note = note.or_else(|| visible.first().cloned());
            self.select(note);
            return;
        }
        let folder = self
            .settings
            .last_folder
            .clone()
            .filter(|f| self.store.folder(f).is_some_and(|f| self.account_enabled(&f.account)))
            .or_else(|| self.store.default_folder(&self.settings.default_account).map(|f| f.id.clone()))
            .or_else(|| self.store.default_folder(crate::model::LOCAL).map(|f| f.id.clone()))
            .unwrap_or_default();
        self.location = Location::Folder(folder);
        // Only if the list shows it: the last note may have been a search
        // hit from elsewhere, deleted, or in a disabled account.
        let visible = self.visible_notes();
        let note = self.settings.last_note.clone().filter(|n| visible.contains(n));
        let note = note.or_else(|| visible.first().cloned());
        self.select(note);
    }

    /// "All Notes" is in the sidebar once there's more than one folder.
    pub fn show_all_notes(&self) -> bool {
        self.store.folders.iter().filter(|f| self.account_enabled(&f.account)).count() > 1
    }

    pub fn account_enabled(&self, id: &str) -> bool {
        self.store.account(id).is_some_and(|a| a.enabled)
    }

    // ---- Notes list ---------------------------------------------------------

    /// The notes the list shows, in order.
    pub fn visible_notes(&self) -> Vec<String> {
        self.list_sections().into_iter().flat_map(|(_, ids)| ids).collect()
    }

    /// The notes list in sections ("Pinned", "Today", …), each a list of ids.
    pub fn list_sections(&self) -> Vec<(String, Vec<String>)> {
        let words: Vec<String> = self.search.split_whitespace().map(|w| w.to_lowercase()).collect();
        let searching = !words.is_empty();
        let mut notes: Vec<&Note> = self
            .store
            .notes
            .iter()
            .filter(|n| self.account_enabled(&n.account))
            .filter(|n| {
                if searching {
                    // A locked note is found by its title only.
                    let body = if crate::lock::is_locked(&n.body) { n.title().to_lowercase() } else { n.body.to_lowercase() };
                    n.deleted.is_none() && words.iter().all(|w| body.contains(w.as_str()))
                } else {
                    match &self.location {
                        Location::All => n.deleted.is_none(),
                        Location::Folder(f) => n.deleted.is_none() && &n.folder == f,
                        Location::Deleted(a) => n.deleted.is_some() && &n.account == a,
                        Location::Tag(t) => n.deleted.is_none() && n.tags().contains(t),
                    }
                }
            })
            .collect();
        let deleted_view = matches!(self.location, Location::Deleted(_)) && !searching;
        let key = |n: &Note| -> DateTime<Utc> {
            if deleted_view {
                n.deleted.unwrap_or(n.modified)
            } else if self.settings.sort_by == SortBy::Created {
                n.created
            } else {
                n.modified
            }
        };
        match self.settings.sort_by {
            SortBy::Title if !deleted_view => notes.sort_by(|a, b| {
                crate::model::natural_cmp(&a.title(), &b.title()).then_with(|| b.modified.cmp(&a.modified))
            }),
            _ => notes.sort_by(|a, b| key(b).cmp(&key(a))),
        }
        if searching {
            return vec![(String::new(), notes.iter().map(|n| n.id.clone()).collect())];
        }
        let mut sections: Vec<(String, Vec<String>)> = Vec::new();
        let mut push = |name: String, id: &str| match sections.last_mut() {
            Some((s, ids)) if *s == name => ids.push(id.to_owned()),
            _ => sections.push((name, vec![id.to_owned()])),
        };
        let pinned_first = !deleted_view;
        if pinned_first {
            for n in notes.iter().filter(|n| n.pinned) {
                push("Pinned".into(), &n.id);
            }
        }
        let grouped = self.settings.group_by_date && self.settings.sort_by != SortBy::Title;
        let today = Local::now().date_naive();
        for n in notes.iter().filter(|n| !(pinned_first && n.pinned)) {
            let name = if grouped { date_group(key(n), today) } else if pinned_first && notes.iter().any(|n| n.pinned) { "Notes".into() } else { String::new() };
            push(name, &n.id);
        }
        sections
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
            if self.fresh.contains(&old) && self.store.note(&old).is_some_and(|n| n.body.trim().is_empty()) {
                self.fresh.remove(&old);
                self.store.notes.retain(|n| n.id != old);
                self.unsaved.insert(old);
            }
        }
        self.selected = id.clone();
        self.settings.last_note = id.clone();
        self.editor = id.and_then(|id| self.editor_for(&id));
        self.lock.typed.clear();
        self.lock.error = None;
        self.lock.focus = true;
        self.format_open = false;
        self.scroll_list = true;
    }

    pub fn go_to(&mut self, loc: Location) {
        match &loc {
            Location::Folder(f) => self.settings.last_folder = Some(f.clone()),
            Location::All => self.settings.last_folder = Some(ALL_NOTES.to_owned()),
            _ => {}
        }
        self.location = loc;
        self.search.clear();
        let first = self.visible_notes().first().cloned();
        self.select(first);
    }

    /// The account and folder new notes go to from here.
    pub fn new_note_target(&self) -> (String, String) {
        if let Location::Folder(f) = &self.location {
            if let Some(folder) = self.store.folder(f).filter(|f| self.account_enabled(&f.account)) {
                return (folder.account.clone(), folder.id.clone());
            }
        }
        let account = match &self.location {
            Location::Deleted(a) if self.account_enabled(a) => a.clone(),
            _ => self.settings.default_account.clone(),
        };
        let account = if self.account_enabled(&account) { account } else { crate::model::LOCAL.to_owned() };
        let folder = self.store.default_folder(&account).map(|f| f.id.clone()).unwrap_or_default();
        (account, folder)
    }

    pub fn new_note(&mut self) {
        let (account, folder) = self.new_note_target();
        // In a tag, a new note carries the tag (as in Apple Notes), and the
        // tag stays shown; All Notes shows it too.
        let body_tag = match &self.location {
            Location::Tag(t) => Some(format!("#{t}")),
            _ => None,
        };
        let stays_shown = matches!(self.location, Location::Folder(ref f) if *f == folder) || self.location == Location::All;
        if body_tag.is_none() && !stays_shown {
            self.location = Location::Folder(folder.clone());
        }
        self.search.clear();
        let mut n = Note::new(&account, &folder, "");
        if let Some(t) = &body_tag {
            n.body = format!("\n{t}\n");
        }
        let id = n.id.clone();
        self.store.notes.push(n);
        self.fresh.insert(id.clone());
        self.unsaved.insert(id.clone());
        self.select(Some(id.clone()));
        let kind = self.settings.start_with.kind();
        let mut e = Editor::new_note(&id, kind);
        if let Some(t) = body_tag {
            e = Editor::new(&id, &format!("\n{t}\n"));
            e.doc.blocks[0].kind = kind;
        }
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
        let Some(n) = self.store.note(id).cloned() else { return };
        let mut copy = Note::new(&n.account, &n.folder, &n.body);
        copy.deleted = None;
        let cid = copy.id.clone();
        self.store.notes.push(copy);
        self.unsaved.insert(cid.clone());
        self.select(Some(cid));
    }

    /// Moves notes to Recently Deleted, or out of the app for good if they
    /// were there already (after asking).
    pub fn delete_notes(&mut self, ids: Vec<String>) {
        if ids.is_empty() {
            return;
        }
        let already = ids.iter().all(|id| self.store.note(id).is_some_and(|n| n.deleted.is_some()));
        if already {
            self.confirm = Some(Confirm::DeleteNotesForever(ids));
            return;
        }
        let next = self.neighbour_of(&ids);
        let mut undo = Deletion { notes: Vec::new(), location: self.location.clone() };
        for id in &ids {
            if self.fresh.remove(id) && self.store.note(id).is_some_and(|n| n.body.trim().is_empty()) {
                self.store.notes.retain(|n| &n.id != id);
            } else if let Some(n) = self.store.note_mut(id) {
                undo.notes.push((id.clone(), n.pinned, n.modified));
                n.deleted = Some(Utc::now());
                n.pinned = false;
                n.touch();
            }
            self.unsaved.insert(id.clone());
        }
        if !undo.notes.is_empty() {
            self.undo_deletes.push(undo);
            self.undo_deletes.drain(..self.undo_deletes.len().saturating_sub(50));
            self.redo_deletes.clear();
        }
        self.selected = None;
        self.editor = None;
        self.select(next);
    }

    /// Takes back the last deletion (⌘Z outside a note's text): the notes
    /// come out of Recently Deleted, pinned and dated as they were, shown
    /// where they were.
    pub fn undo_delete(&mut self) -> bool {
        let Some(d) = self.undo_deletes.pop() else { return false };
        // Notes deleted for good since can't come back.
        let notes: Vec<_> = d.notes.into_iter().filter(|(id, ..)| self.store.note(id).is_some_and(|n| n.deleted.is_some())).collect();
        if notes.is_empty() {
            return self.undo_delete();
        }
        for (id, pinned, modified) in &notes {
            self.recover_quietly(id);
            if let Some(n) = self.store.note_mut(id) {
                n.pinned = *pinned;
                // Undoing isn't an edit: the note keeps its date and place.
                n.modified = *modified;
            }
        }
        let still_there = match &d.location {
            Location::Folder(f) => self.store.folder(f).is_some(),
            _ => true,
        };
        if still_there && self.location != d.location {
            self.go_to(d.location.clone());
        }
        if !self.visible_notes().contains(&notes[0].0) {
            let folder = self.store.note(&notes[0].0).map(|n| n.folder.clone()).unwrap_or_default();
            self.go_to(Location::Folder(folder));
        }
        let ids: Vec<String> = notes.iter().map(|(id, ..)| id.clone()).collect();
        // `shown` is last frame's list, so put the notes in it for ordering.
        self.shown = self.visible_notes();
        self.set_multi(ids, &notes[0].0, Some(notes[0].0.clone()));
        self.focus = Pane::List;
        self.redo_deletes.push(Deletion { notes, location: d.location });
        true
    }

    /// Deletes again what ⌘Z brought back (⇧⌘Z).
    pub fn redo_delete(&mut self) -> bool {
        let Some(d) = self.redo_deletes.pop() else { return false };
        let ids: Vec<_> = d.notes.iter().map(|(id, ..)| id.clone()).filter(|id| self.store.note(id).is_some_and(|n| n.deleted.is_none())).collect();
        if ids.is_empty() {
            return self.redo_delete();
        }
        let redo = std::mem::take(&mut self.redo_deletes);
        self.delete_notes(ids);
        self.redo_deletes = redo;
        true
    }

    fn neighbour_of(&self, ids: &[String]) -> Option<String> {
        let shown = &self.shown;
        let i = shown.iter().position(|s| ids.contains(s))?;
        shown.iter().skip(i + 1).find(|s| !ids.contains(s)).or_else(|| shown[..i].iter().rev().find(|s| !ids.contains(s))).cloned()
    }

    pub fn delete_forever(&mut self, ids: &[String]) {
        let next = self.neighbour_of(ids);
        for id in ids {
            self.store.purge_note(id);
            self.unsaved.insert(id.clone());
        }
        let open_gone = self.selected.as_ref().is_some_and(|s| ids.contains(s));
        // Also drops them from a selection of several notes.
        self.prune_missing();
        if open_gone && self.selected.is_none() {
            self.select(next);
        }
    }

    pub fn recover_note(&mut self, id: &str) {
        self.recover_quietly(id);
        self.push_notice("Note recovered.".into());
    }

    /// Takes a note out of Recently Deleted, back into its folder (or the
    /// account's default folder if that's gone).
    fn recover_quietly(&mut self, id: &str) {
        let Some(n) = self.store.note(id) else { return };
        let folder_ok = self.store.folder(&n.folder).is_some();
        let default = self.store.default_folder(&n.account).map(|f| f.id.clone()).unwrap_or_default();
        let n = self.store.note_mut(id).unwrap();
        n.deleted = None;
        if !folder_ok {
            n.folder = default;
        }
        n.touch();
        self.unsaved.insert(id.to_owned());
    }

    pub fn toggle_pin(&mut self, id: &str) {
        if let Some(n) = self.store.note_mut(id) {
            n.pinned = !n.pinned;
            n.touch();
            self.unsaved.insert(id.to_owned());
        }
    }

    /// Moves notes to a folder, possibly in another account. The selection
    /// then moves on to a note still shown, if they've left the list.
    pub fn move_notes(&mut self, ids: &[String], folder: &str) {
        // Worked out before anything moves, so it's never one of them.
        let next = self.neighbour_of(ids);
        for id in ids {
            self.move_one(id, folder);
        }
        self.retain_visible(next);
    }

    fn move_one(&mut self, id: &str, folder: &str) {
        let Some(target) = self.store.folder(folder).cloned() else { return };
        let Some(n) = self.store.note(id).cloned() else { return };
        if n.account == target.account {
            let n = self.store.note_mut(id).unwrap();
            n.folder = target.id.clone();
            n.deleted = None;
            n.touch();
        } else {
            // Into another account: deleted from the old one, made new in
            // the new one.
            if let Some(remote_id) = &n.sync.remote_id {
                self.store.tombstones.push(Tombstone {
                    account: n.account.clone(),
                    note: true,
                    remote_id: remote_id.clone(),
                    path: vec![],
                    revision: n.sync.revision.clone(),
                });
            }
            let n = self.store.note_mut(id).unwrap();
            n.account = target.account.clone();
            n.folder = target.id.clone();
            n.deleted = None;
            n.sync = Default::default();
            n.touch();
        }
        self.unsaved.insert(id.to_owned());
    }

    /// After the search text changed. Notes the search hides drop out of the
    /// selection (so a Delete doesn't reach them). Once the search is
    /// cleared, a note opened from its results stays open: the list moves
    /// to its folder.
    pub fn search_changed(&mut self) {
        if self.search.trim().is_empty() && self.multi.is_empty() {
            if let Some(id) = self.selected.clone().filter(|id| !self.visible_notes().contains(id)) {
                if let Some(n) = self.store.note(&id).filter(|n| n.deleted.is_none() && self.account_enabled(&n.account)) {
                    // Not `go_to`, which would open the folder's first note.
                    let folder = n.folder.clone();
                    self.location = Location::Folder(folder.clone());
                    self.settings.last_folder = Some(folder);
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
        let name = self.store.unique_folder_name(&account, parent.as_deref(), "New Folder");
        let f = Folder::new(&account, parent.as_deref(), &name);
        let id = f.id.clone();
        self.store.folders.push(f);
        self.settings.collapsed.retain(|c| Some(c) != parent.as_ref() && c != &account);
        self.rename = Some(Rename { folder: id.clone(), text: name, focus: true });
        self.go_to(Location::Folder(id));
        self.focus = Pane::Sidebar;
    }

    /// Finishes naming a folder. Returns an error message for a name that
    /// can't be used.
    pub fn finish_rename(&mut self) {
        let Some(r) = self.rename.take() else { return };
        let name = r.text.trim().replace('/', "∕");
        let Some(f) = self.store.folder(&r.folder).cloned() else { return };
        if name.is_empty() || name == f.name {
            return;
        }
        let taken = self.store.folders.iter().any(|o| {
            o.id != f.id && o.account == f.account && o.parent == f.parent && o.name.eq_ignore_ascii_case(&name)
        });
        if taken {
            self.push_notice(format!("The name “{name}” is already taken. Please choose a different name."));
            return;
        }
        self.store.folder_mut(&r.folder).unwrap().name = name;
    }

    /// Deletes a folder and the folders in it; their notes go to Recently
    /// Deleted.
    pub fn delete_folder(&mut self, id: &str) {
        let Some(f) = self.store.folder(id).cloned() else { return };
        if f.default {
            return;
        }
        let ids = self.store.folder_and_descendants(id);
        let default = self.store.default_folder(&f.account).map(|f| f.id.clone()).unwrap_or_default();
        let now = Utc::now();
        for n in self.store.notes.iter_mut().filter(|n| ids.contains(&n.folder)) {
            n.folder = default.clone();
            if n.deleted.is_none() {
                n.deleted = Some(now);
            }
            n.pinned = false;
            n.touch();
            self.unsaved.insert(n.id.clone());
        }
        for fid in &ids {
            let path = self.store.folder_path(fid);
            if let Some(remote_id) = self.store.folder(fid).and_then(|f| f.sync.remote_id.clone()) {
                let path = self.store.folder(fid).and_then(|f| f.sync.synced_path.clone()).unwrap_or(path);
                self.store.tombstones.push(Tombstone { account: f.account.clone(), note: false, remote_id, path, revision: None });
            }
        }
        self.store.folders.retain(|f| !ids.contains(&f.id));
        if matches!(&self.location, Location::Folder(l) if ids.contains(l)) {
            self.go_to(Location::Folder(default));
        }
    }

    pub fn empty_deleted(&mut self, account: &str) {
        let ids: Vec<String> =
            self.store.notes.iter().filter(|n| n.account == account && n.deleted.is_some()).map(|n| n.id.clone()).collect();
        self.delete_forever(&ids);
    }

    fn purge_expired(&mut self) {
        let ids = self.store.expired(Utc::now());
        if !ids.is_empty() {
            self.delete_forever(&ids);
        }
    }

    // ---- Editing ------------------------------------------------------------

    /// Copies the editor's changes into the note.
    pub fn flush_editor(&mut self) {
        let Some(e) = &mut self.editor else { return };
        let Some(md) = e.take_markdown() else { return };
        let id = e.note_id.clone();
        if self.store_locked(&id, &md) {
            return;
        }
        if let Some(n) = self.store.note_mut(&id) {
            if n.body != md {
                n.body = md;
                n.touch();
                self.unsaved.insert(id.clone());
                self.last_edit = Some(Instant::now());
                self.fresh.remove(&id);
            }
        }
    }

    pub fn push_notice(&mut self, s: String) {
        self.notices.push((s, Instant::now()));
    }

    // ---- Saving and syncing -----------------------------------------------

    fn save(&mut self, force: bool) {
        if !force && self.last_save.elapsed() < SAVE_EVERY {
            return;
        }
        let unsaved = std::mem::take(&mut self.unsaved);
        if let Err(e) = self.db.save(&self.store, &self.settings, &unsaved) {
            eprintln!("saving: {e:#}");
            self.unsaved.extend(unsaved);
        }
        self.last_save = Instant::now();
    }

    pub fn sync_now(&mut self, account: &str) {
        self.needs_sign_in.remove(account);
        self.start_sync(account);
    }

    fn start_sync(&mut self, account: &str) {
        let Some(a) = self.store.account(account).cloned() else { return };
        if !a.synced() || !a.enabled || self.worker.busy.contains(account) {
            return;
        }
        self.flush_editor();
        let snapshot = AccountSnapshot::of(&self.store, account);
        let secret = self.db.secret(account);
        self.sync_started.insert(account.to_owned(), Instant::now());
        self.sync_secret.insert(account.to_owned(), secret.clone());
        self.worker.sync(a.backend.clone(), secret, snapshot);
    }

    pub fn syncing(&self, account: &str) -> bool {
        self.worker.busy.contains(account)
    }

    /// Starts the syncs that are due.
    fn schedule_syncs(&mut self) {
        let refresh = self.settings.refresh_minutes;
        let ids: Vec<String> = self.store.accounts.iter().filter(|a| a.synced() && a.enabled).map(|a| a.id.clone()).collect();
        let quiet = self.last_edit.map_or(true, |t| t.elapsed() > SYNC_AFTER_EDIT);
        for id in ids {
            if self.worker.busy.contains(&id) || self.needs_sign_in.contains(&id) {
                continue;
            }
            let since = self.sync_started.get(&id).map(|t| t.elapsed());
            let due = match since {
                None => true,
                Some(s) if self.sync_errors.contains_key(&id) => s > RETRY_AFTER_ERROR,
                Some(s) => {
                    let changed = quiet
                        && s > StdDuration::from_secs(5)
                        && AccountSnapshot::of(&self.store, &id).has_local_changes();
                    // Standard Notes' own apps look for changes every 30
                    // seconds (and are told of them at once, see
                    // `live_changes`); asking costs one small request.
                    let every = match self.store.account(&id).map(|a| &a.backend) {
                        Some(crate::sync::BackendConfig::StandardNotes(_)) => StdDuration::from_secs(30).min(StdDuration::from_secs(refresh as u64 * 60)),
                        _ => StdDuration::from_secs(refresh as u64 * 60),
                    };
                    changed || (refresh > 0 && s > every)
                }
            };
            if due {
                self.start_sync(&id);
            }
        }
    }

    /// A running sync renewed its account's credentials: saved right away,
    /// unless the account was removed or signed in again since the sync
    /// started (then they belong to a sign-in that's gone).
    fn handle_renewed(&mut self, account: &str, secret: String) {
        let current = self.db.secret(account);
        let ours = self.store.account(account).is_some() && self.sync_secret.get(account).is_some_and(|s| *s == current);
        if !ours {
            return;
        }
        if let Err(e) = self.db.set_secret(account, Some(&secret)) {
            eprintln!("saving the renewed sign-in: {e:#}");
            return;
        }
        // Still this sync's sign-in: the end of the sync must see it so.
        self.sync_secret.insert(account.to_owned(), Some(secret));
    }

    fn handle_synced(&mut self, r: Synced) {
        self.worker.busy.remove(&r.account);
        // Signed in again, or removed, while this sync ran: what it says
        // about the old sign-in no longer applies.
        let started_with = self.sync_secret.remove(&r.account);
        let exists = self.store.account(&r.account).is_some();
        let replaced = exists && started_with.is_some_and(|s| s != self.db.secret(&r.account));
        if exists && !replaced {
            if let Some(s) = &r.renewed_secret {
                if let Err(e) = self.db.set_secret(&r.account, Some(s)) {
                    eprintln!("saving the renewed sign-in: {e:#}");
                }
            }
            if r.needs_sign_in {
                self.needs_sign_in.insert(r.account.clone());
            }
        }
        if replaced {
            // The new sign-in's own sync couldn't start while this one ran:
            // due right away. (This one's outcome is still applied.)
            self.sync_started.remove(&r.account);
        }
        let out = match r.result {
            Ok(out) => out,
            Err(e) => {
                self.sync_errors.insert(r.account.clone(), e);
                return;
            }
        };
        if self.store.account(&r.account).is_none() {
            return; // removed meanwhile
        }
        // The open note may have been edited since the snapshot.
        self.flush_editor();
        match &out.error {
            Some(e) => {
                self.sync_errors.insert(r.account.clone(), e.clone());
            }
            None => {
                self.sync_errors.remove(&r.account);
            }
        }
        let unchanged = out.unchanged;
        let notices = engine::apply(&mut self.store, &r.snapshot, out);
        for n in notices {
            self.push_notice(n);
        }
        if unchanged {
            self.save(true);
            return;
        }
        if let Err(e) = self.db.save_all(&self.store, &self.settings) {
            eprintln!("saving after sync: {e:#}");
        }
        // Unlocked notes' text follows the sync too (or they close, if the
        // new version doesn't open with the keys at hand).
        self.refresh_unlocked();
        // Follow the sync in the open note and the selection.
        if let Some(id) = self.editor.as_ref().map(|e| e.note_id.clone()) {
            match self.store.note(&id).map(|n| n.body.clone()) {
                Some(body) if crate::lock::is_locked(&body) => match self.reopen_after_sync(&id) {
                    Some(text) => self.editor.as_mut().unwrap().reload(&text),
                    None => self.editor = None,
                },
                Some(body) => self.editor.as_mut().unwrap().reload(&body),
                None => {
                    self.editor = None;
                    self.selected = None;
                }
            }
        }
        if let Location::Folder(f) = &self.location {
            if self.store.folder(f).is_none() {
                let (_, folder) = self.new_note_target();
                self.location = Location::Folder(folder);
            }
        }
        self.prune_missing();
        // Selected notes the sync moved to Recently Deleted leave the list:
        // don't keep them open (or let Delete/Move reach them).
        let in_deleted = matches!(self.location, Location::Deleted(_));
        if !in_deleted && self.selected_ids().iter().any(|id| self.store.note(id).is_some_and(|n| n.deleted.is_some())) {
            self.retain_visible(None);
        }
    }

    /// Drops notes that no longer exist (purged, deleted by a sync) from
    /// the selection.
    fn prune_missing(&mut self) {
        if self.selected.as_ref().is_some_and(|s| self.store.note(s).is_none()) {
            self.selected = None;
            self.editor = None;
        }
        if self.multi.is_empty() {
            return;
        }
        let store = &self.store;
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
        if pressed(cmd, Key::F) {
            if let Some(e) = &mut self.editor {
                e.find.open = true;
                e.find.focus = true;
            } else {
                self.search_focus = true;
            }
        }
        if pressed(cmd, Key::G) {
            if let Some(e) = &mut self.editor {
                e.find_step(1);
            }
        }
        if pressed(shift_cmd, Key::G) {
            if let Some(e) = &mut self.editor {
                e.find_step(-1);
            }
        }
        if pressed(cmd, Key::Comma) {
            self.settings_ui.open = true;
        }
        if pressed(cmd, Key::Num1) {
            self.settings.view = ListView::List;
        }
        if pressed(cmd, Key::Num2) {
            self.settings.view = ListView::Gallery;
        }
        if pressed(alt_cmd, Key::S) {
            self.settings.sidebar_visible = !self.settings.sidebar_visible;
        }
        if pressed(cmd, Key::D) {
            if let Some(id) = self.selected.clone() {
                self.duplicate_note(&id);
            }
        }
        // Text size: ⌘+ / ⌘- / ⌘0.
        let sizes = crate::settings::TEXT_SIZES;
        if pressed_loose(cmd, Key::Plus) || pressed_loose(cmd, Key::Equals) {
            self.settings.text_size = sizes.iter().copied().find(|&s| s > self.settings.text_size).unwrap_or(sizes[sizes.len() - 1]);
        }
        if pressed_loose(cmd, Key::Minus) {
            self.settings.text_size = sizes.iter().rev().copied().find(|&s| s < self.settings.text_size).unwrap_or(sizes[0]);
        }
        if pressed(cmd, Key::Num0) {
            self.settings.text_size = Settings::default().text_size;
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
        if let Some(e) = &mut self.editor {
            if self.store.note(&e.note_id).is_some_and(|n| n.deleted.is_some()) {
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
        let (title, text, button) = match c {
            Confirm::DeleteNotesForever(ids) => (
                if ids.len() == 1 { "Delete this note?".to_owned() } else { format!("Delete {} notes?", ids.len()) },
                "This action can't be undone.".to_owned(),
                "Delete",
            ),
            Confirm::DeleteFolder(id) => (
                format!("Delete the folder “{}”?", self.store.folder(id).map_or("", |f| f.name.as_str())),
                "All notes and subfolders in this folder will be deleted. Notes will be kept in Recently Deleted for 30 days.".to_owned(),
                "Delete Folder",
            ),
            Confirm::EmptyDeleted(_) => (
                "Delete all notes in Recently Deleted?".to_owned(),
                "This action can't be undone.".to_owned(),
                "Delete All",
            ),
            Confirm::RemoveAccount(id) => (
                format!("Remove the account “{}”?", self.store.account(id).map_or("", |a| a.name.as_str())),
                "Its notes are removed from this computer. They stay where they're synced to.".to_owned(),
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
            Some(true) => {
                match self.confirm.take().unwrap() {
                    Confirm::DeleteNotesForever(ids) => self.delete_forever(&ids),
                    Confirm::DeleteFolder(id) => self.delete_folder(&id),
                    Confirm::EmptyDeleted(a) => self.empty_deleted(&a),
                    Confirm::RemoveAccount(a) => self.remove_account(&a),
                }
            }
            Some(false) => self.confirm = None,
            None => {}
        }
    }

    pub fn remove_account(&mut self, id: &str) {
        if id == crate::model::LOCAL {
            return;
        }
        self.worker.forget(id);
        let _ = self.db.set_secret(id, None);
        let notes: Vec<String> = self.store.notes.iter().filter(|n| n.account == id).map(|n| n.id.clone()).collect();
        self.store.notes.retain(|n| n.account != id);
        self.unsaved.extend(notes);
        self.store.folders.retain(|f| f.account != id);
        self.store.tombstones.retain(|t| t.account != id);
        self.store.accounts.retain(|a| a.id != id);
        if self.settings.default_account == id {
            self.settings.default_account = crate::model::LOCAL.to_owned();
        }
        self.sync_errors.remove(id);
        self.needs_sign_in.remove(id);
        let loc_gone = match &self.location {
            Location::Folder(f) => self.store.folder(f).is_none(),
            Location::Deleted(a) => a == id,
            Location::Tag(_) | Location::All => false,
        };
        if loc_gone {
            let f = self.store.default_folder(crate::model::LOCAL).map(|f| f.id.clone()).unwrap_or_default();
            self.go_to(Location::Folder(f));
        }
        self.save(true);
    }

    /// Adds a signed-in account (from the Accounts settings).
    pub fn add_account(&mut self, account: crate::model::Account, secret: Option<String>) {
        if let Some(s) = &secret {
            if let Err(e) = self.db.set_secret(&account.id, Some(s)) {
                self.push_notice(format!("Couldn't save the sign-in: {e:#}"));
            } else if self.db.secret_in_memory_only(&account.id) {
                self.push_notice(
                    "The system's credential store isn't available, so this sign-in is kept only until CleanNotes quits (it's never written to disk unencrypted). You'll sign in again next time.".into(),
                );
            }
        }
        let id = account.id.clone();
        self.store.accounts.retain(|a| a.id != id);
        self.store.accounts.push(account);
        self.store.ensure_local();
        self.needs_sign_in.remove(&id);
        self.sync_errors.remove(&id);
        self.sync_started.remove(&id);
        self.worker.forget(&id);
        self.save(true);
        self.start_sync(&id);
    }

    fn notices_bar(&mut self, ctx: &egui::Context, p: &Palette) {
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

/// The notes list section a date falls in: "Today", "Yesterday",
/// "Previous 7 Days", "Previous 30 Days", a month of this year, or a year.
pub fn date_group(t: DateTime<Utc>, today: chrono::NaiveDate) -> String {
    let d = t.with_timezone(&Local).date_naive();
    let days = (today - d).num_days();
    use chrono::Datelike;
    match days {
        i64::MIN..=0 => "Today".into(),
        1 => "Yesterday".into(),
        2..=7 => "Previous 7 Days".into(),
        8..=30 => "Previous 30 Days".into(),
        _ if d.year() == today.year() => d.format("%B").to_string(),
        _ => d.year().to_string(),
    }
}

/// A note's time in the list: "11:32" today, "Yesterday", a weekday within a
/// week, else the date.
pub fn list_time(t: DateTime<Utc>) -> String {
    let lt = t.with_timezone(&Local);
    let today = Local::now().date_naive();
    let days = (today - lt.date_naive()).num_days();
    match days {
        i64::MIN..=0 => lt.format("%H:%M").to_string(),
        1 => "Yesterday".into(),
        2..=6 => lt.format("%A").to_string(),
        _ => lt.format("%-d %b %Y").to_string(),
    }
}

/// The date at the top of a note: "15 November 2026 at 11:32".
pub fn note_header_time(t: DateTime<Utc>) -> String {
    t.with_timezone(&Local).format("%-d %B %Y at %H:%M").to_string()
}

impl App {
    /// Passes Standard Notes requests to the browser engine, and if
    /// Cloudflare asks for its browser check, shows it (as a browser would)
    /// below a bar saying what it is, until the user is done.
    #[cfg(any(windows, target_os = "macos"))]
    fn browser_check(&mut self, ctx: &egui::Context) {
        let Some(web) = &mut self.web else { return };
        web.pump();
        if web.take_changed() {
            // Told of a change on Standard Notes' server: those accounts
            // sync now (as soon as they aren't syncing already).
            let ids: Vec<String> = self
                .store
                .accounts
                .iter()
                .filter(|a| matches!(&a.backend, crate::sync::BackendConfig::StandardNotes(c) if crate::sync::standardnotes::api::base(&c.server) == crate::sync::standardnotes::api::base(crate::sync::standardnotes::DEFAULT_SERVER)))
                .map(|a| a.id.clone())
                .collect();
            for id in ids {
                self.sync_started.remove(&id);
            }
        }
        let Some(web) = &mut self.web else { return };
        if !web.check_needed() && !web.checking() {
            return;
        }
        let mut done = false;
        let bar = egui::TopBottomPanel::top("browser-check").show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.label("Standard Notes' server wants a quick browser check before it lets CleanNotes sync. Complete it below, then click Done.");
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    done = ui.button("Done").clicked();
                });
            });
        });
        if done {
            web.end_check();
            // Try again now.
            self.sync_started.clear();
            return;
        }
        let screen = ctx.screen_rect();
        web.show_check(egui::Rect::from_min_max(egui::pos2(screen.min.x, bar.response.rect.max.y), screen.max));
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
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        theme::apply_style(ctx);
        let p = Palette::get(ctx);
        #[cfg(any(windows, target_os = "macos"))]
        self.browser_check(ctx);
        while let Ok(r) = self.worker.rx.try_recv() {
            match r {
                Reply::Synced(r) => self.handle_synced(r),
                Reply::Renewed { account, secret } => self.handle_renewed(&account, secret),
            }
        }
        self.schedule_syncs();
        self.shortcuts(ctx);
        self.auto_lock(ctx);

        if self.settings.sidebar_visible {
            egui::SidePanel::left("sidebar")
                .resizable(true)
                .default_width(240.0)
                .width_range(190.0..=380.0)
                .show_separator_line(false)
                .frame(egui::Frame::none().fill(p.list_bg))
                .show(ctx, |ui| self.sidebar(ui, &p));
        }
        let list_w = if self.settings.view == ListView::Gallery { 0.0 } else { 300.0 };
        if self.settings.view == ListView::List {
            egui::SidePanel::left("notes-list")
                .resizable(true)
                .default_width(list_w)
                .width_range(220.0..=480.0)
                .show_separator_line(false)
                .frame(egui::Frame::none().fill(p.list_bg))
                .show(ctx, |ui| self.notes_list(ui, &p));
        }
        egui::CentralPanel::default().frame(egui::Frame::none().fill(p.bg)).show(ctx, |ui| {
            if self.settings.view == ListView::Gallery {
                self.gallery(ui, &p);
            } else {
                self.note_pane(ui, &p);
            }
        });

        self.flush_editor();
        self.confirm_dialog(ctx, &p);
        self.lock_sheets(ctx, &p);
        self.settings_window(ctx, &p);
        self.notices_bar(ctx, &p);
        if self.last_purge.elapsed() > StdDuration::from_secs(3600) {
            self.last_purge = Instant::now();
            self.purge_expired();
        }
        self.save(false);
        ctx.request_repaint_after(StdDuration::from_secs(10));
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        self.flush_editor();
        if let Some(sel) = self.selected.clone() {
            // An empty new note goes on quitting, too.
            if self.fresh.contains(&sel) && self.store.note(&sel).is_some_and(|n| n.body.trim().is_empty()) {
                self.store.notes.retain(|n| n.id != sel);
                self.unsaved.insert(sel);
            }
        }
        self.save(true);
    }
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
    use chrono::TimeZone;

    #[test]
    fn all_notes_shows_every_folder_once_there_are_two() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = App::for_test(dir.path());
        assert!(!app.show_all_notes(), "one folder: no All Notes");
        let notes = app.store.default_folder(crate::model::LOCAL).unwrap().id.clone();
        let work = app.store.ensure_folder_at(crate::model::LOCAL, &["Work".into()]);
        assert!(app.show_all_notes());
        let a = Note::new(crate::model::LOCAL, &notes, "# A\n");
        let b = Note::new(crate::model::LOCAL, &work, "# B\n");
        let mut gone = Note::new(crate::model::LOCAL, &work, "# Gone\n");
        gone.deleted = Some(Utc::now());
        let ids = [a.id.clone(), b.id.clone(), gone.id.clone()];
        app.store.notes.extend([a, b, gone]);
        app.go_to(Location::All);
        let shown = app.visible_notes();
        assert!(shown.contains(&ids[0]) && shown.contains(&ids[1]));
        assert!(!shown.contains(&ids[2]), "deleted notes stay in Recently Deleted");
        // A new note made in All Notes stays in view.
        app.new_note();
        assert_eq!(app.location, Location::All);
        // It's remembered for next time.
        assert_eq!(app.settings.last_folder.as_deref(), Some(ALL_NOTES));
    }

    #[test]
    fn date_groups() {
        let today = chrono::NaiveDate::from_ymd_opt(2026, 11, 15).unwrap();
        let at = |y, m, d| Local.with_ymd_and_hms(y, m, d, 12, 0, 0).unwrap().with_timezone(&Utc);
        assert_eq!(date_group(at(2026, 11, 15), today), "Today");
        assert_eq!(date_group(at(2026, 11, 14), today), "Yesterday");
        assert_eq!(date_group(at(2026, 11, 10), today), "Previous 7 Days");
        assert_eq!(date_group(at(2026, 10, 25), today), "Previous 30 Days");
        assert_eq!(date_group(at(2026, 3, 1), today), "March");
        assert_eq!(date_group(at(2025, 12, 1), today), "2025");
    }

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
            n.modified = Utc::now() + chrono::Duration::seconds(self.store.notes.len() as i64);
            let id = n.id.clone();
            self.store.notes.push(n);
            id
        }

        fn folder_named(&mut self, name: &str) -> String {
            self.store.ensure_folder_at(crate::model::LOCAL, &[name.to_owned()])
        }

        /// What the list would show this frame.
        fn draw(&mut self) {
            self.shown = self.visible_notes();
        }
    }

    #[test]
    fn moving_several_notes_leaves_a_shown_note_selected() {
        let (mut app, _dir) = app();
        let home = folder_id_or_default(&app.store);
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
        let home = folder_id_or_default(&app.store);
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
    fn a_sync_from_before_signing_in_again_doesnt_undo_it() {
        let (mut app, _dir) = app();
        let mut acc = app.store.accounts[0].clone();
        acc.id = "sn".into();
        let stale = |app: &App| Synced {
            account: "sn".into(),
            snapshot: AccountSnapshot::of(&app.store, "sn"),
            result: Err("the password was changed".into()),
            needs_sign_in: true,
            renewed_secret: Some("old session, renewed".into()),
        };
        // A sync starts with the old sign-in…
        app.store.accounts.push(acc.clone());
        app.db.set_secret("sn", Some("old session")).unwrap();
        app.sync_secret.insert("sn".into(), Some("old session".into()));
        // …the user signs in again meanwhile…
        app.add_account(acc, Some("new session".into()));
        // …and the old sync's reply arrives.
        let r = stale(&app);
        app.handle_synced(r);
        assert_eq!(app.db.secret("sn").as_deref(), Some("new session"));
        assert!(!app.needs_sign_in.contains("sn"));
        // Removed while syncing: its secret isn't written back.
        app.sync_secret.insert("sn".into(), Some("new session".into()));
        app.remove_account("sn");
        let r = stale(&app);
        app.handle_synced(r);
        assert_eq!(app.db.secret("sn"), None);
        assert!(!app.needs_sign_in.contains("sn"));
    }

    #[test]
    fn credentials_renewed_mid_sync_are_saved_at_once_unless_replaced() {
        let (mut app, _dir) = app();
        let mut acc = app.store.accounts[0].clone();
        acc.id = "sn".into();
        app.store.accounts.push(acc.clone());
        app.db.set_secret("sn", Some("s0")).unwrap();
        app.sync_secret.insert("sn".into(), Some("s0".into()));
        // Renewed twice during one sync: each saved as it comes.
        app.handle_renewed("sn", "s1".into());
        assert_eq!(app.db.secret("sn").as_deref(), Some("s1"));
        app.handle_renewed("sn", "s2".into());
        assert_eq!(app.db.secret("sn").as_deref(), Some("s2"));
        // The sync's end, with the last one, isn't taken for a sign-in
        // that changed meanwhile.
        let r = Synced {
            account: "sn".into(),
            snapshot: AccountSnapshot::of(&app.store, "sn"),
            result: Err("offline".into()),
            needs_sign_in: false,
            renewed_secret: Some("s2".into()),
        };
        app.handle_synced(r);
        assert_eq!(app.db.secret("sn").as_deref(), Some("s2"));
        // Signed in again during a sync: its renewals are dropped.
        app.sync_secret.insert("sn".into(), Some("s2".into()));
        app.add_account(acc, Some("fresh".into()));
        app.handle_renewed("sn", "s3".into());
        assert_eq!(app.db.secret("sn").as_deref(), Some("fresh"));
    }

    #[test]
    fn clearing_the_search_keeps_the_found_note_open_in_its_folder() {
        let (mut app, _dir) = app();
        let home = folder_id_or_default(&app.store);
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
        let home = folder_id_or_default(&app.store);
        app.add(&home, "# Plan\n#work\n");
        app.go_to(Location::Tag("work".into()));
        app.new_note();
        assert_eq!(app.location, Location::Tag("work".into()));
        let id = app.selected.clone().unwrap();
        assert!(app.store.note(&id).unwrap().tags().contains(&"work".to_owned()));
        assert!(app.visible_notes().contains(&id));
    }

    #[test]
    fn new_notes_never_go_to_a_disabled_account() {
        let (mut app, _dir) = app();
        let mut acc = app.store.accounts[0].clone();
        acc.id = "other".into();
        acc.name = "Other".into();
        app.store.accounts.push(acc);
        app.store.ensure_local();
        let f = app.store.ensure_folder_at("other", &["Stuff".into()]);
        app.go_to(Location::Folder(f.clone()));
        assert_eq!(app.new_note_target().1, f);
        app.store.account_mut("other").unwrap().enabled = false;
        let (account, _) = app.new_note_target();
        assert_ne!(account, "other");
    }

    #[test]
    fn startup_only_reopens_a_note_the_list_shows() {
        let (mut app, _dir) = app();
        let home = folder_id_or_default(&app.store);
        let elsewhere = app.folder_named("Elsewhere");
        let here = app.add(&home, "# Here\n");
        let there = app.add(&elsewhere, "# There\n");
        app.settings.last_folder = Some(home.clone());
        app.settings.last_note = Some(there);
        app.restore_location();
        assert_eq!(app.selected, Some(here));
    }

    #[test]
    fn shift_arrows_on_an_empty_list_do_nothing() {
        let (mut app, _dir) = app();
        let home = folder_id_or_default(&app.store);
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
        let home = folder_id_or_default(&app.store);
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

fn folder_id_or_default(store: &Store) -> String {
    store.default_folder(crate::model::LOCAL).map(|f| f.id.clone()).unwrap_or_default()
}
