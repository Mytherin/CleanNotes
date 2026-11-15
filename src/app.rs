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
use crate::sync::worker::{Synced, Worker};
use crate::theme::{self, Palette};

mod list;
mod note_pane;
pub mod settings_window;
mod sidebar;

/// Wait this long after the last keystroke before uploading.
const SYNC_AFTER_EDIT: StdDuration = StdDuration::from_secs(3);
const RETRY_AFTER_ERROR: StdDuration = StdDuration::from_secs(60);
const SAVE_EVERY: StdDuration = StdDuration::from_millis(800);
/// Height of the toolbar row along the top of every pane.
pub const TOOLBAR_H: f32 = 52.0;

/// What's selected in the sidebar.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Location {
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
    pub editor: Option<Editor>,
    pub focus: Pane,
    /// The toolbar search (searches all notes).
    pub search: String,
    pub search_focus: bool,
    /// Notes changed since the last save.
    unsaved: HashSet<String>,
    last_save: Instant,
    last_purge: Instant,
    last_edit: Option<Instant>,
    /// When each account last started a sync (this session).
    sync_started: HashMap<String, Instant>,
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
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        theme::install_fonts(&cc.egui_ctx);
        cc.egui_ctx.options_mut(|o| o.zoom_with_keyboard = false);
        let dir = db::data_dir();
        let (db, store, settings) = Db::open(&dir, Secrets::Keyring).expect("opening the notes database");
        let ctx = cc.egui_ctx.clone();
        let worker = Worker::spawn(move || ctx.request_repaint());
        let mut app = App {
            location: Location::Folder(String::new()),
            store,
            settings,
            db,
            worker,
            selected: None,
            editor: None,
            focus: Pane::List,
            search: String::new(),
            search_focus: false,
            unsaved: HashSet::new(),
            last_save: Instant::now(),
            last_purge: Instant::now(),
            last_edit: None,
            sync_started: HashMap::new(),
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
        };
        app.restore_location();
        app.purge_expired();
        if std::env::var_os("CLEANNOTES_SEED").is_some() && app.store.notes.is_empty() {
            app.seed();
        }
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
        let folder = self
            .settings
            .last_folder
            .clone()
            .filter(|f| self.store.folder(f).is_some_and(|f| self.account_enabled(&f.account)))
            .or_else(|| self.store.default_folder(&self.settings.default_account).map(|f| f.id.clone()))
            .or_else(|| self.store.default_folder(crate::model::LOCAL).map(|f| f.id.clone()))
            .unwrap_or_default();
        self.location = Location::Folder(folder);
        let note = self.settings.last_note.clone().filter(|n| self.store.note(n).is_some());
        let note = note.or_else(|| self.visible_notes().first().cloned());
        self.select(note);
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
                    let body = n.body.to_lowercase();
                    n.deleted.is_none() && words.iter().all(|w| body.contains(w.as_str()))
                } else {
                    match &self.location {
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
        if self.selected == id {
            return;
        }
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
        self.editor = id.and_then(|id| self.store.note(&id).map(|n| Editor::new(&id, &n.body)));
        self.format_open = false;
        self.scroll_list = true;
    }

    pub fn go_to(&mut self, loc: Location) {
        if let Location::Folder(f) = &loc {
            self.settings.last_folder = Some(f.clone());
        }
        self.location = loc;
        self.search.clear();
        let first = self.visible_notes().first().cloned();
        self.select(first);
    }

    /// The account and folder new notes go to from here.
    pub fn new_note_target(&self) -> (String, String) {
        if let Location::Folder(f) = &self.location {
            if let Some(folder) = self.store.folder(f) {
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
        if !matches!(self.location, Location::Folder(_)) || !self.search.is_empty() {
            self.location = Location::Folder(folder.clone());
            self.search.clear();
        }
        let mut n = Note::new(&account, &folder, "");
        // In a tag, a new note carries the tag (as in Apple Notes).
        let mut body_tag = None;
        if let Location::Tag(t) = &self.location {
            body_tag = Some(format!("#{t}"));
        }
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
        for id in &ids {
            if self.fresh.remove(id) && self.store.note(id).is_some_and(|n| n.body.trim().is_empty()) {
                self.store.notes.retain(|n| &n.id != id);
            } else if let Some(n) = self.store.note_mut(id) {
                n.deleted = Some(Utc::now());
                n.pinned = false;
                n.touch();
            }
            self.unsaved.insert(id.clone());
        }
        self.selected = None;
        self.editor = None;
        self.select(next);
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
        if self.selected.as_ref().is_some_and(|s| ids.contains(s)) {
            self.selected = None;
            self.editor = None;
            self.select(next);
        }
    }

    pub fn recover_note(&mut self, id: &str) {
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
        self.push_notice("Note recovered.".into());
    }

    pub fn toggle_pin(&mut self, id: &str) {
        if let Some(n) = self.store.note_mut(id) {
            n.pinned = !n.pinned;
            n.touch();
            self.unsaved.insert(id.to_owned());
        }
    }

    /// Moves a note to a folder, possibly in another account.
    pub fn move_note(&mut self, id: &str, folder: &str) {
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
        if !self.visible_notes().contains(&id.to_owned()) && self.selected.as_deref() == Some(id) {
            let next = self.neighbour_of(&[id.to_owned()]);
            self.select(next);
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
                self.store.tombstones.push(Tombstone { account: f.account.clone(), note: false, remote_id, path });
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
                    changed || (refresh > 0 && s > StdDuration::from_secs(refresh as u64 * 60))
                }
            };
            if due {
                self.start_sync(&id);
            }
        }
    }

    fn handle_synced(&mut self, r: Synced) {
        self.worker.busy.remove(&r.account);
        if let Some(s) = &r.renewed_secret {
            if let Err(e) = self.db.set_secret(&r.account, Some(s)) {
                eprintln!("saving the renewed sign-in: {e:#}");
            }
        }
        if r.needs_sign_in {
            self.needs_sign_in.insert(r.account.clone());
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
        // Follow the sync in the open note and the selection.
        if let Some(e) = &mut self.editor {
            match self.store.note(&e.note_id) {
                Some(n) => e.reload(&n.body.clone()),
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
    }

    // ---- Keyboard -------------------------------------------------------------

    fn shortcuts(&mut self, ctx: &egui::Context) {
        let cmd = Modifiers::COMMAND;
        let shift_cmd = Modifiers::COMMAND | Modifiers::SHIFT;
        let alt_cmd = Modifiers::COMMAND | Modifiers::ALT;
        let pressed = |m: Modifiers, k: Key| ctx.input_mut(|i| i.consume_shortcut(&KeyboardShortcut::new(m, k)));
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
        if pressed(cmd, Key::Plus) || pressed(cmd, Key::Equals) || pressed(shift_cmd, Key::Equals) {
            self.settings.text_size = sizes.iter().copied().find(|&s| s > self.settings.text_size).unwrap_or(sizes[sizes.len() - 1]);
        }
        if pressed(cmd, Key::Minus) {
            self.settings.text_size = sizes.iter().rev().copied().find(|&s| s < self.settings.text_size).unwrap_or(sizes[0]);
        }
        if pressed(cmd, Key::Num0) {
            self.settings.text_size = Settings::default().text_size;
        }
        if pressed(cmd, Key::Backspace) || pressed(cmd, Key::Delete) {
            if let Some(id) = self.selected.clone() {
                self.delete_notes(vec![id]);
            }
        }
        // The notes list.
        if self.focus == Pane::List && self.rename.is_none() && !ctx.wants_keyboard_input() {
            if pressed(Modifiers::NONE, Key::ArrowDown) {
                self.step_selection(1);
            }
            if pressed(Modifiers::NONE, Key::ArrowUp) {
                self.step_selection(-1);
            }
            if pressed(Modifiers::NONE, Key::Delete) || pressed(Modifiers::NONE, Key::Backspace) {
                if let Some(id) = self.selected.clone() {
                    self.delete_notes(vec![id]);
                }
            }
            if pressed(Modifiers::NONE, Key::Enter) || pressed(Modifiers::NONE, Key::Tab) {
                if let Some(e) = &mut self.editor {
                    e.focus_requested = true;
                    self.focus = Pane::Note;
                }
            }
        }
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
            Location::Tag(_) => false,
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

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        theme::apply_style(ctx);
        let p = Palette::get(ctx);
        while let Ok(r) = self.worker.rx.try_recv() {
            self.handle_synced(r);
        }
        self.schedule_syncs();
        self.shortcuts(ctx);

        if self.settings.sidebar_visible {
            egui::SidePanel::left("sidebar")
                .resizable(true)
                .default_width(220.0)
                .width_range(170.0..=360.0)
                .frame(egui::Frame::none().fill(p.sidebar))
                .show(ctx, |ui| self.sidebar(ui, &p));
        }
        let list_w = if self.settings.view == ListView::Gallery { 0.0 } else { 300.0 };
        if self.settings.view == ListView::List {
            egui::SidePanel::left("notes-list")
                .resizable(true)
                .default_width(list_w)
                .width_range(220.0..=480.0)
                .frame(egui::Frame::none().fill(p.bg).stroke(egui::Stroke::new(1.0_f32, p.separator)))
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
}

fn folder_id_or_default(store: &Store) -> String {
    store.default_folder(crate::model::LOCAL).map(|f| f.id.clone()).unwrap_or_default()
}
