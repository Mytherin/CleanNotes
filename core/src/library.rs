//! The notes session both apps run on: the notes, folders and accounts with
//! their settings, and everything done to them that isn't drawing. That
//! covers the notes list and its sections, new notes, deleting and undoing
//! it, moving, pinning, folders, accounts, saving, syncing and locked notes.
//!
//! What's selected, which editor is open and how things are laid out belong
//! to each app. The rule for a host: before anything that syncs ([`Library::tick`],
//! [`Library::sync_now`]), copy the open editor's changes in with
//! [`Library::set_body`], so a sync never works from an outdated note.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use chrono::{DateTime, Local, Utc};

use crate::dates;
use crate::db::{Db, Secrets};
use crate::lock::{self, Keys};
use crate::markdown;
use crate::model::{self, Account, Folder, Note, Store, Tombstone, LOCAL};
use crate::settings::{NoteFilter, Settings, SmartFolder, SortBy};

/// A Smart Folder's name, trimmed ("Smart Folder" if empty).
fn smart_name(name: &str) -> String {
    let name = name.trim();
    if name.is_empty() { "Smart Folder".to_owned() } else { name.to_owned() }
}
use crate::sync::engine::{self, AccountSnapshot};
use crate::sync::worker::{Reply, Synced, Worker};

/// Wait this long after the last edit before uploading.
const SYNC_AFTER_EDIT: Duration = Duration::from_secs(3);
const RETRY_AFTER_ERROR: Duration = Duration::from_secs(60);
const SAVE_EVERY: Duration = Duration::from_millis(800);
const PURGE_EVERY: Duration = Duration::from_secs(3600);
/// `last_folder` when "All Notes" was showing.
const ALL_NOTES: &str = "all-notes";
/// What the password check in Settings decrypts to.
const CHECK: &str = "CleanNotes locked notes";
/// Unlocked notes lock again after this long without use.
pub const AUTO_LOCK: Duration = Duration::from_secs(15 * 60);

/// Where the notes list is: what's chosen among the folders.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Location {
    /// Every note in every enabled account (when there's more than one
    /// folder).
    All,
    Folder(String),
    /// An account's Recently Deleted.
    Deleted(String),
    Tag(String),
    /// A Smart Folder (its id in the settings).
    Smart(String),
}

/// A note as the notes list shows it.
#[derive(Clone, Debug, PartialEq)]
pub struct NoteRow {
    pub id: String,
    /// The title, or "New Note".
    pub title: String,
    /// The first line after the title, "No additional text", or "Locked".
    pub snippet: String,
    /// Nothing after the title (the snippet says "No additional text").
    pub empty: bool,
    /// "11:32", "Yesterday", "Monday" or a date.
    pub time: String,
    /// The note's folder, when the list shows notes from several.
    pub folder: Option<String>,
    pub pinned: bool,
    pub locked: bool,
    pub deleted: bool,
}

/// What became of an editor's changes ([`Library::set_body`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BodySet {
    Unchanged,
    Changed,
    /// A locked note that can't be saved any more (locked again elsewhere
    /// with another password): its editor must close.
    Closed,
}

/// "Lock Note" ([`Library::lock_note`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LockNote {
    Locked,
    /// No password chosen yet: ask for one (and a hint), then
    /// [`Library::set_password`] and lock again.
    ChoosePassword,
    /// Not entered this session: ask for it, then
    /// [`Library::enter_password`] and lock again.
    EnterPassword,
}

/// Notes a deletion undo brought back, and where they were shown.
#[derive(Clone, Debug, PartialEq)]
pub struct Restored {
    pub ids: Vec<String>,
    pub location: Location,
}

/// What [`Library::tick`] did.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Ticked {
    /// A sync (or the hourly purge of Recently Deleted) changed notes or
    /// folders: open editors should reload, and selections drop notes that
    /// are gone.
    pub changed: bool,
    /// Unlocked notes locked again after a while without use: password
    /// sheets left open should close, and locked notes' editors too.
    pub auto_locked: bool,
    /// A sync brought pictures notes show: images shown as missing should
    /// be loaded again.
    pub attachments: bool,
}

/// Notes moved to Recently Deleted together, and how things were before.
struct Deletion {
    /// Each note, whether it was pinned and when it was last edited.
    notes: Vec<(String, bool, DateTime<Utc>)>,
    location: Location,
}

/// Locked notes this session. As in Apple Notes, one password unlocks every
/// note locked with it, and they stay unlocked until "Lock Now", a while
/// without use, or quitting. Passwords are only ever kept in memory.
#[derive(Default)]
struct LockKeys {
    /// The current password, once entered (and checked) this session.
    current: Option<String>,
    /// Keys made this session, by salt (base64).
    keys: HashMap<String, Keys>,
    /// The text of each unlocked note.
    unlocked: HashMap<String, String>,
    last_use: Option<Instant>,
    /// Pictures encrypted or decrypted this session, each way (a plain
    /// picture's name and its encrypted copy's reference, with its key), so
    /// an editor still showing one gets the same other one each time.
    refs: HashMap<String, String>,
}

pub struct Library {
    pub store: Store,
    pub settings: Settings,
    pub db: Db,
    worker: Worker,
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
    /// Messages for the user, waiting to be shown.
    notices: Vec<String>,
    /// New notes made this session: left empty, they're thrown away.
    fresh: HashSet<String>,
    /// Deletions undo can take back, newest last, and those it took back.
    undo_deletes: Vec<Deletion>,
    redo_deletes: Vec<Deletion>,
    lock: LockKeys,
    /// A sync downloaded attachments since the last [`Library::tick`].
    attachments_arrived: bool,
}

impl Library {
    /// The notes in `dir`. `on_sync_reply` is called (from the sync thread)
    /// whenever a sync has something to report: the host should then call
    /// [`Library::tick`] soon, on its own thread.
    pub fn open(dir: &Path, secrets: Secrets, on_sync_reply: impl Fn() + Send + Sync + 'static) -> anyhow::Result<Library> {
        let (db, store, settings) = Db::open(dir, secrets)?;
        let mut lib = Library::with(db, store, settings, Worker::spawn(on_sync_reply));
        lib.upgrade_file_links();
        lib.purge_expired();
        lib.clear_opened_files();
        Ok(lib)
    }

    fn with(db: Db, store: Store, settings: Settings, worker: Worker) -> Library {
        Library {
            store,
            settings,
            db,
            worker,
            unsaved: HashSet::new(),
            last_save: Instant::now(),
            last_purge: Instant::now(),
            last_edit: None,
            sync_started: HashMap::new(),
            sync_secret: HashMap::new(),
            sync_errors: HashMap::new(),
            needs_sign_in: HashSet::new(),
            notices: Vec::new(),
            fresh: HashSet::new(),
            undo_deletes: Vec::new(),
            redo_deletes: Vec::new(),
            lock: LockKeys::default(),
            attachments_arrived: false,
        }
    }

    /// A library on a database in `dir`, secrets in memory, no sync
    /// callback (for tests).
    pub fn for_test(dir: &Path) -> Library {
        Library::open(dir, Secrets::Memory, || {}).expect("test database")
    }

    /// Example notes, for trying the app out and for screenshots.
    pub fn seed(&mut self) {
        let notes = self.default_folder_id(LOCAL);
        let work = self.store.ensure_folder_at(LOCAL, &["Work".into()]);
        self.store.ensure_folder_at(LOCAL, &["Work".into(), "Projects".into()]);
        let recipes = self.store.ensure_folder_at(LOCAL, &["Recipes".into()]);
        let day = chrono::Duration::days(1);
        let samples: [(&str, &str, chrono::Duration, bool); 6] = [
            (&notes, "# Weekend trip\n## Packing\n- [x] Passport\n- [x] Charger\n- [ ] Rain jacket\n- [ ] Snacks for the train\n## Plan\n1. Train at **9:12** from Central\n2. Check in at the *hotel*\n3. Dinner at [Trattoria Roma](https://example.com)\n\nDon't forget #travel insurance.\n", chrono::Duration::minutes(5), true),
            (&notes, "# Book ideas\n* *The Overstory*, Richard Powers\n* *Piranesi*, Susanna Clarke\n    * also her short stories\n* ~~Dune~~ (read it)\n> A reader lives a thousand lives before he dies.\n", day * 2, false),
            (&work, "# Standup notes\nShipped the **sync engine**; next up: conflict UI.\n- S3 backend\n- SQLite backend\n- <u>Release</u> on Friday #work\n", chrono::Duration::hours(3), false),
            (&work, "# Snippets\n```\ncargo run --release\ncargo test\n```\nRemember to bump the version.\n", day * 12, false),
            (&recipes, "# Pancakes\n### Ingredients\n- [ ] 200 g flour\n- [ ] 2 eggs\n- [ ] 300 ml milk\n### Method\nWhisk everything, rest for 20 minutes, fry in butter. #cooking\n", day * 40, false),
            (&notes, "# Grocery list\nApples, oat milk, coffee beans, basil\n", day * 400, false),
        ];
        for (folder, body, age, pinned) in samples {
            let mut n = Note::new(LOCAL, folder, body);
            n.modified = Utc::now() - age;
            n.created = n.modified - chrono::Duration::hours(1);
            n.pinned = pinned;
            self.unsaved.insert(n.id.clone());
            self.store.notes.push(n);
        }
    }

    /// The local account's (or another account's) default folder.
    pub fn default_folder_id(&self, account: &str) -> String {
        self.store.default_folder(account).map(|f| f.id.clone()).unwrap_or_default()
    }

    /// The folder an account's notes go to and its list opens on when none
    /// is chosen: its Notes folder, unless that was deleted.
    pub fn home_folder_id(&self, account: &str) -> String {
        self.store.home_folder(account).map(|f| f.id.clone()).unwrap_or_default()
    }

    /// Marks a note as changed outside the library's own methods (so it's
    /// saved).
    pub fn mark_changed(&mut self, id: &str) {
        self.unsaved.insert(id.to_owned());
    }

    /// Messages for the user since last asked ("Note recovered.", sync
    /// conflicts, …).
    pub fn take_notices(&mut self) -> Vec<String> {
        std::mem::take(&mut self.notices)
    }

    pub fn push_notice(&mut self, s: String) {
        self.notices.push(s);
    }

    // ---- Where the list is ----------------------------------------------------

    /// Where the list was when the app was last used (and the note open
    /// then, if that list still shows it), else the default account's
    /// Notes folder and its first note.
    pub fn initial_location(&self) -> (Location, Option<String>) {
        let loc = if self.settings.last_folder.as_deref() == Some(ALL_NOTES) && self.show_all_notes() {
            Location::All
        } else {
            let folder = self
                .settings
                .last_folder
                .clone()
                .filter(|f| self.store.folder(f).is_some_and(|f| self.account_enabled(&f.account) && self.store.folder_shown(f)))
                .or_else(|| self.store.home_folder(&self.settings.default_account).map(|f| f.id.clone()))
                .or_else(|| self.store.home_folder(&self.fallback_account()).map(|f| f.id.clone()))
                .unwrap_or_default();
            Location::Folder(folder)
        };
        // Only if the list shows it: the last note may have been a search
        // hit from elsewhere, deleted, or in a disabled account.
        let visible = self.visible_notes(&loc, "");
        let note = self.settings.last_note.clone().filter(|n| visible.contains(n));
        (loc.clone(), note.or_else(|| visible.first().cloned()))
    }

    /// Remembers where the list is, for next time.
    pub fn remember_location(&mut self, loc: &Location) {
        match loc {
            Location::Folder(f) => self.settings.last_folder = Some(f.clone()),
            Location::All => self.settings.last_folder = Some(ALL_NOTES.to_owned()),
            _ => {}
        }
    }

    /// Remembers the open note, for next time.
    pub fn remember_note(&mut self, id: Option<&str>) {
        self.settings.last_note = id.map(str::to_owned);
    }

    /// "All Notes" is offered once there's more than one folder.
    pub fn show_all_notes(&self) -> bool {
        self.store.folders.iter().filter(|f| self.account_enabled(&f.account)).count() > 1
    }

    pub fn account_enabled(&self, id: &str) -> bool {
        self.store.account(id).is_some_and(|a| a.enabled)
    }

    /// Whether a location still exists (a folder may have been deleted, an
    /// account removed or switched off).
    pub fn location_exists(&self, loc: &Location) -> bool {
        match loc {
            Location::All => true,
            Location::Folder(f) => self.store.folder(f).is_some_and(|f| self.account_enabled(&f.account)),
            Location::Deleted(a) => self.account_enabled(a),
            Location::Tag(_) => true,
            Location::Smart(s) => self.smart_folder(s).is_some(),
        }
    }

    // ---- Smart Folders --------------------------------------------------------

    pub fn smart_folder(&self, id: &str) -> Option<&SmartFolder> {
        self.settings.smart_folders.iter().find(|s| s.id == id)
    }

    /// Whether a note is in a Smart Folder (`now` for the date filter).
    fn in_smart_folder(&self, s: &SmartFolder, n: &Note, now: DateTime<Utc>) -> bool {
        let locked = lock::is_locked(&n.body);
        let days = (now - n.modified).num_days();
        s.filter.matches(&n.body, n.pinned, locked, days)
    }

    /// A new Smart Folder; returns its id.
    pub fn add_smart_folder(&mut self, name: &str, filter: NoteFilter) -> String {
        let id = model::new_id();
        self.settings.smart_folders.push(SmartFolder { id: id.clone(), name: smart_name(name), filter });
        self.save(true);
        id
    }

    pub fn update_smart_folder(&mut self, id: &str, name: &str, filter: NoteFilter) {
        if let Some(s) = self.settings.smart_folders.iter_mut().find(|s| s.id == id) {
            s.name = smart_name(name);
            s.filter = filter;
            self.save(true);
        }
    }

    pub fn delete_smart_folder(&mut self, id: &str) {
        self.settings.smart_folders.retain(|s| s.id != id);
        self.save(true);
    }

    /// The list's title: the folder's name, "All Notes", "Recently Deleted",
    /// "#tag", or "Search" while searching.
    pub fn location_title(&self, loc: &Location, search: &str) -> String {
        if !search.trim().is_empty() {
            return "Search".into();
        }
        match loc {
            Location::All => "All Notes".into(),
            Location::Folder(f) => self.store.folder(f).map(|f| f.name.clone()).unwrap_or_default(),
            Location::Deleted(_) => "Recently Deleted".into(),
            Location::Tag(t) => format!("#{t}"),
            Location::Smart(s) => self.smart_folder(s).map(|s| s.name.clone()).unwrap_or_default(),
        }
    }

    // ---- The notes list -------------------------------------------------------

    /// The notes the list shows, in order.
    pub fn visible_notes(&self, loc: &Location, search: &str) -> Vec<String> {
        self.list_sections(loc, search).into_iter().flat_map(|(_, ids)| ids).collect()
    }

    /// The notes list in sections ("Pinned", "Today", …), each a list of
    /// ids. A search (every word must match) looks through every note.
    pub fn list_sections(&self, loc: &Location, search: &str) -> Vec<(String, Vec<String>)> {
        let words: Vec<String> = search.split_whitespace().map(|w| w.to_lowercase()).collect();
        let searching = !words.is_empty();
        let smart = match loc {
            Location::Smart(s) => self.smart_folder(s),
            _ => None,
        };
        let now = Utc::now();
        let mut notes: Vec<&Note> = self
            .store
            .notes
            .iter()
            .filter(|n| self.account_enabled(&n.account))
            .filter(|n| {
                if searching {
                    // A locked note is found by its title only.
                    let body = if lock::is_locked(&n.body) { n.title().to_lowercase() } else { n.body.to_lowercase() };
                    n.deleted.is_none() && words.iter().all(|w| body.contains(w.as_str()))
                } else {
                    match loc {
                        Location::All => n.deleted.is_none(),
                        Location::Folder(f) => n.deleted.is_none() && &n.folder == f,
                        Location::Deleted(a) => n.deleted.is_some() && &n.account == a,
                        Location::Tag(t) => n.deleted.is_none() && n.tags().contains(t),
                        Location::Smart(_) => n.deleted.is_none() && smart.is_some_and(|f| self.in_smart_folder(f, n, now)),
                    }
                }
            })
            .collect();
        let deleted_view = matches!(loc, Location::Deleted(_)) && !searching;
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
            SortBy::Title if !deleted_view => {
                // Each title read once, not at every comparison.
                let mut titled: Vec<(String, &Note)> = notes.iter().map(|n| (n.title(), *n)).collect();
                titled.sort_by(|(ta, a), (tb, b)| model::natural_cmp(ta, tb).then_with(|| b.modified.cmp(&a.modified)));
                notes = titled.into_iter().map(|(_, n)| n).collect();
            }
            _ => notes.sort_by(|a, b| key(b).cmp(&key(a))),
        }
        if searching {
            // As in Apple Notes: notes whose title matches first, as Top Hits.
            let title_hit = |n: &Note| {
                let t = n.title().to_lowercase();
                words.iter().all(|w| t.contains(w.as_str()))
            };
            let (top, rest): (Vec<&Note>, Vec<&Note>) = notes.iter().copied().partition(|n| title_hit(n));
            let mut out = Vec::new();
            for (name, group) in [("Top Hits", top), ("Notes", rest)] {
                if !group.is_empty() {
                    out.push((name.to_owned(), group.iter().map(|n| n.id.clone()).collect()));
                }
            }
            return out;
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
        let any_pinned = pinned_first && notes.iter().any(|n| n.pinned);
        for n in notes.iter().filter(|n| !(pinned_first && n.pinned)) {
            let name = if grouped {
                dates::date_group(key(n), today)
            } else if any_pinned {
                "Notes".into()
            } else {
                String::new()
            };
            push(name, &n.id);
        }
        sections
    }

    /// A note as the list at `loc` shows it (with its folder's name if
    /// `show_folder`).
    pub fn note_row(&self, id: &str, loc: &Location, show_folder: bool) -> Option<NoteRow> {
        let n = self.store.note(id)?;
        let title = n.title();
        let title = if title.is_empty() { "New Note".to_owned() } else { title };
        let locked = lock::is_locked(&n.body);
        let snippet = if locked { "Locked".to_owned() } else { markdown::snippet_of(&n.body, 120) };
        let empty = snippet.is_empty();
        let snippet = if empty { "No additional text".to_owned() } else { snippet };
        let when = match n.deleted {
            Some(d) if matches!(loc, Location::Deleted(_)) => d,
            _ if self.settings.sort_by == SortBy::Created => n.created,
            _ => n.modified,
        };
        let folder = show_folder.then(|| self.store.folder(&n.folder).map(|f| f.name.clone())).flatten();
        Some(NoteRow {
            id: id.to_owned(),
            title,
            snippet,
            empty,
            time: dates::list_time(when),
            folder,
            pinned: n.pinned,
            locked,
            deleted: n.deleted.is_some(),
        })
    }

    /// A search result's title and snippet, in parts with the matches of
    /// the search's words marked (to show them in bold); the snippet is the
    /// line where the words are found. `None` when not searching.
    pub fn search_parts(&self, id: &str, search: &str) -> Option<(Vec<(String, bool)>, Vec<(String, bool)>)> {
        let words: Vec<&str> = search.split_whitespace().collect();
        if words.is_empty() {
            return None;
        }
        let n = self.store.note(id)?;
        let row = self.note_row(id, &Location::All, false)?;
        let snippet = if row.locked || row.empty { row.snippet } else { markdown::search_snippet_of(&n.body, &words, 120) };
        Some((crate::doc::highlight_parts(&row.title, &words), crate::doc::highlight_parts(&snippet, &words)))
    }

    /// Whether a list at `loc` shows each note's folder (All Notes, tags and
    /// search results mix folders).
    pub fn list_shows_folders(&self, loc: &Location, search: &str) -> bool {
        !search.trim().is_empty() || matches!(loc, Location::All | Location::Tag(_) | Location::Smart(_))
    }

    /// Notes (not deleted) in a folder.
    pub fn folder_count(&self, folder: &str) -> usize {
        self.store.notes.iter().filter(|n| n.folder == folder && n.deleted.is_none()).count()
    }

    /// Notes in All Notes.
    pub fn all_count(&self) -> usize {
        self.store.notes.iter().filter(|n| n.deleted.is_none() && self.account_enabled(&n.account)).count()
    }

    /// Notes in an account's Recently Deleted.
    pub fn deleted_count(&self, account: &str) -> usize {
        self.store.notes.iter().filter(|n| n.account == account && n.deleted.is_some()).count()
    }

    /// Every #tag in the notes shown, sorted.
    pub fn all_tags(&self) -> Vec<String> {
        let mut tags: Vec<String> = Vec::new();
        let mut seen = HashSet::new();
        for n in self.store.notes.iter().filter(|n| n.deleted.is_none() && self.account_enabled(&n.account)) {
            for t in n.tags() {
                if seen.insert(t.clone()) {
                    tags.push(t);
                }
            }
        }
        tags.sort();
        tags
    }

    // ---- Notes ------------------------------------------------------------------

    /// The account and folder new notes go to from `loc`.
    pub fn new_note_target(&self, loc: &Location) -> (String, String) {
        if let Location::Folder(f) = loc {
            if let Some(folder) = self.store.folder(f).filter(|f| self.account_enabled(&f.account)) {
                return (folder.account.clone(), folder.id.clone());
            }
        }
        let account = match loc {
            Location::Deleted(a) if self.account_enabled(a) => a.clone(),
            _ => self.settings.default_account.clone(),
        };
        let account = if self.account_enabled(&account) { account } else { self.fallback_account() };
        let folder = self.home_folder_id(&account);
        (account, folder)
    }

    /// Makes a new, empty note from `loc`. Returns its id, its Markdown (a
    /// note made in a tag carries the tag, as in Apple Notes) and where the
    /// list should be to show it.
    pub fn new_note(&mut self, loc: &Location) -> (String, String, Location) {
        let (account, folder) = self.new_note_target(loc);
        let body = match loc {
            Location::Tag(t) => format!("\n#{t}\n"),
            // A Smart Folder's tags, so the note is in it.
            Location::Smart(s) => match self.smart_folder(s).map(|s| &s.filter.tags) {
                Some(tags) if !tags.is_empty() => {
                    format!("\n{}\n", tags.iter().map(|t| format!("#{t}")).collect::<Vec<_>>().join(" "))
                }
                _ => String::new(),
            },
            _ => String::new(),
        };
        // The tag stays shown, and All Notes shows it too; a Smart Folder
        // if the new note is in it.
        let in_smart = match loc {
            Location::Smart(s) => self.smart_folder(s).is_some_and(|s| s.filter.matches(&body, false, false, 0)),
            _ => false,
        };
        let stays = in_smart || matches!(loc, Location::Tag(_) | Location::All) || matches!(loc, Location::Folder(f) if *f == folder);
        let shown = if stays { loc.clone() } else { Location::Folder(folder.clone()) };
        let n = Note::new(&account, &folder, &body);
        let id = n.id.clone();
        self.store.notes.push(n);
        self.fresh.insert(id.clone());
        self.unsaved.insert(id.clone());
        (id, body, shown)
    }

    /// Leaving a note: a new note left empty goes away. Returns whether it
    /// did.
    pub fn leave_note(&mut self, id: &str) -> bool {
        if self.fresh.contains(id) && self.store.note(id).is_some_and(|n| n.body.trim().is_empty()) {
            self.fresh.remove(id);
            self.store.notes.retain(|n| n.id != id);
            self.unsaved.insert(id.to_owned());
            return true;
        }
        false
    }

    /// An editor's Markdown for a note: stored if it changed (sealed again,
    /// if the note is locked).
    pub fn set_body(&mut self, id: &str, md: &str) -> BodySet {
        if let Some(r) = self.store_locked(id, md) {
            return r;
        }
        // Encrypted pictures (an editor opened before "Remove Lock", or
        // pasted from a locked note) are kept plainly in a plain note.
        let (md, _) = self.convert_refs(md, false);
        let md = md.as_str();
        let Some(n) = self.store.note_mut(id) else { return BodySet::Unchanged };
        if n.body == md {
            return BodySet::Unchanged;
        }
        n.body = md.to_owned();
        n.touch();
        self.unsaved.insert(id.to_owned());
        self.last_edit = Some(Instant::now());
        self.fresh.remove(id);
        BodySet::Changed
    }

    /// A copy of a note, in the same folder. Returns its id.
    pub fn duplicate_note(&mut self, id: &str) -> Option<String> {
        let n = self.store.note(id).cloned()?;
        let mut copy = Note::new(&n.account, &n.folder, &n.body);
        copy.deleted = None;
        let cid = copy.id.clone();
        self.store.notes.push(copy);
        self.unsaved.insert(cid.clone());
        Some(cid)
    }

    /// Whether every one of these notes is in Recently Deleted already (so
    /// deleting them deletes them for good, after asking).
    pub fn all_in_deleted(&self, ids: &[String]) -> bool {
        !ids.is_empty() && ids.iter().all(|id| self.store.note(id).is_some_and(|n| n.deleted.is_some()))
    }

    /// Moves notes to Recently Deleted (new notes left empty just go). The
    /// list was at `loc`: undoing goes back there.
    pub fn trash_notes(&mut self, ids: &[String], loc: &Location) {
        let mut undo = Deletion { notes: Vec::new(), location: loc.clone() };
        for id in ids {
            if self.fresh.remove(id) && self.store.note(id).is_some_and(|n| n.body.trim().is_empty()) {
                self.store.notes.retain(|n| &n.id != id);
            } else if let Some(n) = self.store.note_mut(id) {
                if n.deleted.is_some() {
                    continue;
                }
                undo.notes.push((id.clone(), n.pinned, n.modified));
                n.deleted = Some(Utc::now());
                n.pinned = false;
                n.touch();
            }
            self.unsaved.insert(id.clone());
        }
        if !undo.notes.is_empty() {
            self.undo_deletes.push(undo);
            let extra = self.undo_deletes.len().saturating_sub(50);
            self.undo_deletes.drain(..extra);
            self.redo_deletes.clear();
        }
    }

    pub fn can_undo_delete(&self) -> bool {
        !self.undo_deletes.is_empty()
    }

    /// Takes back the last deletion: the notes come out of Recently Deleted,
    /// pinned and dated as they were.
    pub fn undo_delete(&mut self) -> Option<Restored> {
        loop {
            let d = self.undo_deletes.pop()?;
            // Notes deleted for good since can't come back.
            let notes: Vec<_> =
                d.notes.into_iter().filter(|(id, ..)| self.store.note(id).is_some_and(|n| n.deleted.is_some())).collect();
            if notes.is_empty() {
                continue;
            }
            for (id, pinned, modified) in &notes {
                self.recover_quietly(id);
                if let Some(n) = self.store.note_mut(id) {
                    n.pinned = *pinned;
                    // Undoing isn't an edit: the note keeps its date and place.
                    n.modified = *modified;
                }
            }
            let ids = notes.iter().map(|(id, ..)| id.clone()).collect();
            let location = d.location.clone();
            self.redo_deletes.push(Deletion { notes, location: d.location });
            return Some(Restored { ids, location });
        }
    }

    /// Deletes again what undo brought back. Returns the notes deleted.
    pub fn redo_delete(&mut self) -> Option<Vec<String>> {
        loop {
            let d = self.redo_deletes.pop()?;
            let ids: Vec<_> = d
                .notes
                .iter()
                .map(|(id, ..)| id.clone())
                .filter(|id| self.store.note(id).is_some_and(|n| n.deleted.is_none()))
                .collect();
            if ids.is_empty() {
                continue;
            }
            let redo = std::mem::take(&mut self.redo_deletes);
            self.trash_notes(&ids, &d.location);
            self.redo_deletes = redo;
            return Some(ids);
        }
    }

    /// Deletes notes for good.
    pub fn delete_forever(&mut self, ids: &[String]) {
        for id in ids {
            self.store.purge_note(id);
            self.lock.unlocked.remove(id);
            self.unsaved.insert(id.clone());
        }
    }

    /// Empties an account's Recently Deleted. Returns the notes deleted.
    pub fn empty_deleted(&mut self, account: &str) -> Vec<String> {
        let ids: Vec<String> =
            self.store.notes.iter().filter(|n| n.account == account && n.deleted.is_some()).map(|n| n.id.clone()).collect();
        self.delete_forever(&ids);
        ids
    }

    fn purge_expired(&mut self) -> bool {
        let ids = self.store.expired(Utc::now());
        self.delete_forever(&ids);
        !ids.is_empty()
    }

    /// "Recover": out of Recently Deleted, back into its folder.
    pub fn recover_note(&mut self, id: &str) {
        self.recover_quietly(id);
        self.push_notice("Note recovered.".into());
    }

    /// Takes a note out of Recently Deleted, back into its folder (or the
    /// account's default folder if that's gone).
    fn recover_quietly(&mut self, id: &str) {
        let Some(n) = self.store.note(id) else { return };
        let folder_ok = self.store.folder(&n.folder).is_some();
        let default = self.default_folder_id(&n.account);
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

    /// Moves notes to a folder, possibly in another account.
    pub fn move_notes(&mut self, ids: &[String], folder: &str) {
        for id in ids {
            self.move_one(id, folder);
        }
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

    // ---- Folders --------------------------------------------------------------

    /// Makes a folder ("New Folder", or `name` if given and free) in an
    /// account, inside `parent` if given. Returns its id.
    pub fn new_folder(&mut self, account: &str, parent: Option<&str>, name: Option<&str>) -> String {
        let base = name.map(|n| n.trim().replace('/', "∕")).filter(|n| !n.is_empty()).unwrap_or_else(|| "New Folder".into());
        // "Notes" where the deleted default folder was brings that back.
        if let Some(d) = self.store.default_folder(account).filter(|d| d.hidden && parent.is_none() && d.name.eq_ignore_ascii_case(&base)) {
            let id = d.id.clone();
            self.store.folder_mut(&id).unwrap().hidden = false;
            self.settings.collapsed.retain(|c| c != account);
            return id;
        }
        let name = self.store.unique_folder_name(account, parent, &base);
        let f = Folder::new(account, parent, &name);
        let id = f.id.clone();
        self.store.folders.push(f);
        // Its account and parent open up to show it.
        self.settings.collapsed.retain(|c| Some(c.as_str()) != parent && c != account);
        id
    }

    /// Renames a folder. Errors with a message for a name that's taken.
    /// Returns the id of the folder that now has the name: naming a folder
    /// at the top level after the account's deleted "Notes" folder brings
    /// that back, with what was in this folder moved into it.
    pub fn rename_folder(&mut self, id: &str, name: &str) -> Result<String, String> {
        let name = name.trim().replace('/', "∕");
        let Some(f) = self.store.folder(id).cloned() else { return Ok(id.to_owned()) };
        if name.is_empty() || name == f.name {
            return Ok(id.to_owned());
        }
        if let Some(d) = self.store.default_folder(&f.account).filter(|d| {
            d.id != f.id && f.parent.is_none() && !self.store.folder_shown(d) && d.name.eq_ignore_ascii_case(&name)
        }) {
            let default = d.id.clone();
            self.merge_into_default(&f.id, &default);
            return Ok(default);
        }
        let taken = self.store.folders.iter().any(|o| {
            o.id != f.id && o.account == f.account && o.parent == f.parent && o.name.eq_ignore_ascii_case(&name)
        });
        if taken {
            return Err(format!("The name “{name}” is already taken. Please choose a different name."));
        }
        self.store.folder_mut(id).unwrap().name = name;
        Ok(id.to_owned())
    }

    /// Brings back an account's deleted default folder in place of folder
    /// `id` (in the same account): its notes and folders move into the
    /// default folder, and `id` itself goes.
    fn merge_into_default(&mut self, id: &str, default: &str) {
        for n in self.store.notes.iter_mut().filter(|n| n.folder == id) {
            n.folder = default.to_owned();
            n.touch();
            self.unsaved.insert(n.id.clone());
        }
        let kids: Vec<(String, String)> = self
            .store
            .folders
            .iter()
            .filter(|c| c.parent.as_deref() == Some(id))
            .map(|c| (c.id.clone(), c.name.clone()))
            .collect();
        for (kid, kid_name) in kids {
            let account = self.store.folder(kid.as_str()).unwrap().account.clone();
            let name = self.store.unique_folder_name(&account, Some(default), &kid_name);
            let k = self.store.folder_mut(&kid).unwrap();
            k.parent = Some(default.to_owned());
            k.name = name;
        }
        if let Some(f) = self.store.folder(id).cloned() {
            if let Some(remote_id) = f.sync.remote_id.clone() {
                let path = f.sync.synced_path.clone().unwrap_or_else(|| self.store.folder_path(id));
                self.store.tombstones.push(Tombstone { account: f.account.clone(), note: false, remote_id, path, revision: None });
            }
            self.store.folders.retain(|o| o.id != id);
            self.settings.collapsed.retain(|c| c != id && *c != f.account && c != default);
        }
        self.store.folder_mut(default).unwrap().hidden = false;
    }

    /// Whether a folder can be moved into `parent` (a folder, or `None` for
    /// the top level of `account`): not an account's "Notes" folder, not
    /// into itself or a folder inside it, and not to where it already is.
    pub fn can_move_folder(&self, id: &str, account: &str, parent: Option<&str>) -> bool {
        let Some(f) = self.store.folder(id) else { return false };
        if f.default || self.store.account(account).is_none() {
            return false;
        }
        if let Some(p) = parent {
            match self.store.folder(p) {
                Some(pf) if pf.account == account => {}
                _ => return false,
            }
            if self.store.folder_and_descendants(id).iter().any(|d| d == p) {
                return false;
            }
        }
        !(f.account == account && f.parent.as_deref() == parent)
    }

    /// Moves a folder, with the folders and notes in it, into `parent` (or
    /// the top level of `account`), possibly in another account. A name
    /// already used there gets a number ("Work 2"). Returns whether it moved.
    pub fn move_folder(&mut self, id: &str, account: &str, parent: Option<&str>) -> bool {
        if !self.can_move_folder(id, account, parent) {
            return false;
        }
        let f = self.store.folder(id).cloned().unwrap();
        // A free name among its new siblings (not counting itself).
        let name = {
            let taken = |n: &str| {
                self.store.folders.iter().any(|o| {
                    o.id != f.id && o.account == account && o.parent.as_deref() == parent && o.name.eq_ignore_ascii_case(n)
                })
            };
            if taken(&f.name) {
                (2..).map(|i| format!("{} {i}", f.name)).find(|n| !taken(n)).unwrap()
            } else {
                f.name.clone()
            }
        };
        if f.account == account {
            // In the same account it's a change of path, synced like a rename.
            let m = self.store.folder_mut(id).unwrap();
            m.parent = parent.map(str::to_owned);
            m.name = name;
            m.position = None;
        } else {
            // Into another account: its notes move as notes moved there do
            // (gone from the old account, new in the new one), and its folders
            // are deleted from the old account and made new in the new one.
            let ids = self.store.folder_and_descendants(id);
            let old_default = self.default_folder_id(&f.account);
            let notes: Vec<(String, bool)> =
                self.store.notes.iter().filter(|n| ids.contains(&n.folder)).map(|n| (n.id.clone(), n.deleted.is_some())).collect();
            // Folders first, so the notes find them in the new account.
            for fid in &ids {
                let path = self.store.folder_path(fid);
                let fo = self.store.folder(fid).unwrap();
                if let Some(remote_id) = fo.sync.remote_id.clone() {
                    let path = fo.sync.synced_path.clone().unwrap_or(path);
                    self.store.tombstones.push(Tombstone { account: f.account.clone(), note: false, remote_id, path, revision: None });
                }
            }
            for fid in &ids {
                let m = self.store.folder_mut(fid).unwrap();
                m.account = account.to_owned();
                m.sync = Default::default();
                if fid == id {
                    m.parent = parent.map(str::to_owned);
                    m.name = name.clone();
                    m.position = None;
                }
            }
            for (nid, deleted) in notes {
                if deleted {
                    // Recently Deleted stays with the old account.
                    let n = self.store.note_mut(&nid).unwrap();
                    n.folder = old_default.clone();
                    n.touch();
                    self.unsaved.insert(nid);
                } else {
                    let folder = self.store.note(&nid).unwrap().folder.clone();
                    self.move_one(&nid, &folder);
                }
            }
        }
        // Where it went opens up to show it.
        self.settings.collapsed.retain(|c| Some(c.as_str()) != parent && c != account);
        true
    }

    /// Whether a folder can be dragged to just before `before` (one of
    /// `parent`'s folders, or `None` for after the last) among `parent`'s
    /// folders (or the top level of `account`): somewhere it can move to,
    /// or a new place among its own siblings. Not the default folder.
    pub fn can_place_folder(&self, id: &str, account: &str, parent: Option<&str>, before: Option<&str>) -> bool {
        let Some(f) = self.store.folder(id) else { return false };
        if f.default {
            return false;
        }
        let kids: Vec<&str> = self.store.child_folders(account, parent).iter().map(|k| k.id.as_str()).collect();
        if before.is_some_and(|b| !kids.contains(&b)) {
            return false;
        }
        if f.account != account || f.parent.as_deref() != parent {
            return self.can_move_folder(id, account, parent);
        }
        // Among its siblings: not right where it is already.
        let at = kids.iter().position(|k| *k == id);
        let next = at.and_then(|i| kids.get(i + 1).copied());
        before != Some(id) && before != next
    }

    /// Moves a folder (as [`Self::move_folder`] does, if `parent` isn't
    /// where it is) and puts it just before `before` among `parent`'s
    /// folders, or after the last if `None`. Returns whether it moved.
    pub fn place_folder(&mut self, id: &str, account: &str, parent: Option<&str>, before: Option<&str>) -> bool {
        if !self.can_place_folder(id, account, parent, before) {
            return false;
        }
        let f = self.store.folder(id).unwrap();
        if (f.account != account || f.parent.as_deref() != parent) && !self.move_folder(id, account, parent) {
            return false;
        }
        let mut order: Vec<String> =
            self.store.child_folders(account, parent).into_iter().map(|k| k.id.clone()).filter(|k| k != id).collect();
        let at = before.and_then(|b| order.iter().position(|k| k == b)).unwrap_or(order.len());
        order.insert(at, id.to_owned());
        for (i, k) in order.iter().enumerate() {
            self.store.folder_mut(k).unwrap().position = Some(i as u32);
        }
        true
    }

    /// Deletes a folder and the folders in it; their notes go to Recently
    /// Deleted. Returns the folders deleted. A default folder itself stays
    /// (notes with nowhere else to go still land in it), just out of the
    /// sidebar while it's empty.
    pub fn delete_folder(&mut self, id: &str) -> Vec<String> {
        let Some(f) = self.store.folder(id).cloned() else { return vec![] };
        let ids = self.store.folder_and_descendants(id);
        let default = self.default_folder_id(&f.account);
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
        for fid in ids.iter().filter(|fid| **fid != default) {
            let path = self.store.folder_path(fid);
            if let Some(remote_id) = self.store.folder(fid).and_then(|f| f.sync.remote_id.clone()) {
                let path = self.store.folder(fid).and_then(|f| f.sync.synced_path.clone()).unwrap_or(path);
                self.store.tombstones.push(Tombstone { account: f.account.clone(), note: false, remote_id, path, revision: None });
            }
        }
        self.store.folders.retain(|f| f.id == default || !ids.contains(&f.id));
        if f.default {
            self.store.folder_mut(id).unwrap().hidden = true;
        }
        ids
    }

    // ---- Accounts ---------------------------------------------------------------

    /// Whether an account can be removed: any but the last one.
    pub fn can_remove_account(&self, id: &str) -> bool {
        self.store.account(id).is_some() && self.store.accounts.len() > 1
    }

    /// How many notes (Recently Deleted included) an account has here.
    pub fn account_note_count(&self, id: &str) -> usize {
        self.store.notes.iter().filter(|n| n.account == id).count()
    }

    /// The account new notes fall back on: the default account if it's on,
    /// else the local one or the first account that's on.
    pub fn fallback_account(&self) -> String {
        if self.account_enabled(&self.settings.default_account) {
            return self.settings.default_account.clone();
        }
        let home = self.store.home_account();
        if self.account_enabled(&home) {
            return home;
        }
        self.store.accounts.iter().find(|a| a.enabled).map(|a| a.id.clone()).unwrap_or(home)
    }

    /// Adds "On My PC" back after it was removed.
    pub fn add_local_account(&mut self) {
        self.store.add_local();
        self.save(true);
    }

    /// Removes an account and its notes from this device: a synced one's
    /// stay where they're synced to; the local account's are gone for good.
    /// The last account can't be removed.
    pub fn remove_account(&mut self, id: &str) {
        if !self.can_remove_account(id) {
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
            self.settings.default_account = self.store.home_account();
        }
        self.sync_errors.remove(id);
        self.needs_sign_in.remove(id);
        self.save(true);
    }

    /// The account to save after signing in: the existing one (signing in
    /// again keeps its sync state) or a new one.
    pub fn account_for(&self, id: &str, name: String, backend: crate::sync::BackendConfig) -> Account {
        match self.store.account(id) {
            Some(a) => Account { name, backend, enabled: true, ..a.clone() },
            None => Account { id: id.to_owned(), name, backend, enabled: true, cursor: None, last_sync: None },
        }
    }

    /// Adds a signed-in account (or replaces it, signed in again), and
    /// starts its first sync.
    pub fn add_account(&mut self, account: Account, secret: Option<String>) {
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

    // ---- Saving and syncing -------------------------------------------------

    /// Saves what changed (at most every 0.8 s, unless `force`).
    pub fn save(&mut self, force: bool) {
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

    /// Everything that happens over time: sync replies, starting the syncs
    /// that are due, the purge of Recently Deleted, saving, auto-lock. Call
    /// it often (every frame, or on a timer and after `on_sync_reply`),
    /// with the open editor's changes copied in first.
    pub fn tick(&mut self) -> Ticked {
        let mut t = Ticked::default();
        while let Ok(r) = self.worker.rx.try_recv() {
            match r {
                Reply::Synced(r) => t.changed |= self.handle_synced(r),
                Reply::Renewed { account, secret } => self.handle_renewed(&account, secret),
            }
        }
        self.schedule_syncs();
        if self.last_purge.elapsed() > PURGE_EVERY {
            self.last_purge = Instant::now();
            t.changed |= self.purge_expired();
        }
        if self.has_unlocked() && self.lock.last_use.is_some_and(|u| u.elapsed() > AUTO_LOCK) {
            self.lock_now();
            t.auto_locked = true;
        }
        self.save(false);
        t.attachments = std::mem::take(&mut self.attachments_arrived);
        t
    }

    // ---- Attachments -------------------------------------------------------------

    /// The folder attachments are kept in (see [`crate::attachments`]).
    pub fn attachments_dir(&self) -> PathBuf {
        self.db.dir().join(crate::attachments::DIR)
    }

    /// Saves an image (its file's bytes) to attach to a note: returns its
    /// name, for [`crate::editing::Command::InsertImage`].
    pub fn add_image(&self, data: &[u8]) -> anyhow::Result<String> {
        crate::attachments::save(&self.attachments_dir(), data)
    }

    /// Saves an image to attach to the note `id`: encrypted, with a key of
    /// its own, if the note is locked. Returns its reference, for
    /// [`crate::editing::Command::InsertImage`].
    pub fn add_image_to(&self, id: &str, data: &[u8]) -> anyhow::Result<String> {
        if self.is_locked(id) {
            crate::attachments::save_sealed(&self.attachments_dir(), data)
        } else {
            self.add_image(data)
        }
    }

    /// Saves a file (not shown as a picture: a PDF, …) to attach to the
    /// note `id`, `file_name` being the name it had: encrypted, with a key
    /// of its own, if the note is locked. Returns its reference, for
    /// [`crate::editing::Command::InsertFile`].
    pub fn add_file_to(&self, id: &str, data: &[u8], file_name: &str) -> anyhow::Result<String> {
        if self.is_locked(id) {
            crate::attachments::save_sealed_file(&self.attachments_dir(), data)
        } else {
            crate::attachments::save_file(&self.attachments_dir(), data, file_name)
        }
    }

    /// Where attached files are copied to be opened in other apps.
    fn opened_files_dir() -> PathBuf {
        std::env::temp_dir().join("CleanNotes Attachments")
    }

    /// An attached file (its reference and the name it's shown with) copied
    /// out to open in the system's app for it; see
    /// [`crate::attachments::open_copy`]. A locked note's file is decrypted
    /// into the copy, which is removed again on [`Library::lock_now`] and
    /// the next start.
    pub fn open_attachment(&self, reference: &str, label: &str) -> anyhow::Result<PathBuf> {
        crate::attachments::open_copy(&self.attachments_dir(), reference, label, &Self::opened_files_dir())
    }

    /// Removes the copies [`Library::open_attachment`] made (those still
    /// open in an app may stay).
    pub fn clear_opened_files(&self) {
        let _ = std::fs::remove_dir_all(Self::opened_files_dir());
    }

    /// An image's bytes by reference (decrypted in memory if it's a locked
    /// note's), if this device has it.
    pub fn attachment_data(&self, reference: &str) -> Option<Vec<u8>> {
        crate::attachments::load(&self.attachments_dir(), reference)
    }

    /// `md` with its attachments encrypted (`seal`: for a locked note) or
    /// decrypted (for a plain one), as files of their own; and whether all
    /// of them could be (those not on this device yet are left as they are).
    fn convert_refs(&mut self, md: &str, seal: bool) -> (String, bool) {
        let dir = self.attachments_dir();
        let cache = &mut self.lock.refs;
        let mut all = true;
        let out = crate::attachments::replace_refs(md, |r, label| {
            if crate::attachments::is_sealed_ref(r) == seal {
                return None;
            }
            if let Some(other) = cache.get(r) {
                return Some(other.clone());
            }
            let made = if seal { crate::attachments::seal(&dir, r) } else { crate::attachments::unseal(&dir, r, label) };
            match made {
                Ok(other) => {
                    cache.insert(r.to_owned(), other.clone());
                    cache.insert(other.clone(), r.to_owned());
                    Some(other)
                }
                Err(_) => {
                    all = false;
                    None
                }
            }
        });
        (out, all)
    }

    /// Notes imported from Evernote before pictures were supported link to
    /// their pictures as files: those links become the notes' own images
    /// (saved and synced, their dates kept). Locked notes are done when
    /// they're opened ([`Library::upgrade_unlocked`]).
    fn upgrade_file_links(&mut self) {
        let dir = self.attachments_dir();
        for n in &mut self.store.notes {
            if lock::is_locked(&n.body) {
                continue;
            }
            if let Some(md) = crate::attachments::upgrade_file_links(&n.body, &dir) {
                n.body = md;
                n.touch_keep_date();
                self.unsaved.insert(n.id.clone());
            }
        }
        if !self.unsaved.is_empty() {
            self.save(true);
        }
    }

    /// [`Library::upgrade_file_links`] for a locked note just opened: its
    /// text is sealed again with the pictures (encrypted for it).
    fn upgrade_unlocked(&mut self, id: &str) {
        let Some(text) = self.lock.unlocked.get(id) else { return };
        let Some(md) = crate::attachments::upgrade_file_links(text, &self.attachments_dir()) else { return };
        let modified = self.store.note(id).map(|n| n.modified);
        if self.store_locked(id, &md) == Some(BodySet::Changed) {
            if let (Some(n), Some(m)) = (self.store.note_mut(id), modified) {
                n.modified = m;
            }
            self.last_edit = None;
        }
    }

    /// Where an attachment's file is, if this device has it (a synced
    /// note's pictures may still be on their way).
    pub fn attachment_path(&self, name: &str) -> Option<PathBuf> {
        let dir = self.attachments_dir();
        crate::attachments::exists(&dir, name).then(|| crate::attachments::path(&dir, name))
    }

    /// Syncs an account now (the user asked to).
    pub fn sync_now(&mut self, account: &str) {
        self.needs_sign_in.remove(account);
        self.start_sync(account);
    }

    /// Syncs every account now (pull to refresh, coming back to the app).
    pub fn sync_all(&mut self) {
        let ids: Vec<String> = self.store.accounts.iter().filter(|a| a.synced() && a.enabled).map(|a| a.id.clone()).collect();
        for id in ids {
            if !self.needs_sign_in.contains(&id) {
                self.start_sync(&id);
            }
        }
    }

    fn start_sync(&mut self, account: &str) {
        let Some(a) = self.store.account(account).cloned() else { return };
        if !a.synced() || !a.enabled || self.worker.busy.contains(account) {
            return;
        }
        let snapshot = AccountSnapshot::of(&self.store, account).with_attachments(&self.attachments_dir());
        let secret = self.db.secret(account);
        self.sync_started.insert(account.to_owned(), Instant::now());
        self.sync_secret.insert(account.to_owned(), secret.clone());
        self.worker.sync(a.backend.clone(), secret, snapshot);
    }

    /// Makes accounts' syncs due on the next [`Library::tick`] (as soon as
    /// they aren't syncing already): those `which` picks, or every account.
    pub fn sync_soon(&mut self, which: impl Fn(&Account) -> bool) {
        let ids: Vec<String> = self.store.accounts.iter().filter(|a| which(a)).map(|a| a.id.clone()).collect();
        for id in ids {
            self.sync_started.remove(&id);
        }
    }

    pub fn syncing(&self, account: &str) -> bool {
        self.worker.busy.contains(account)
    }

    pub fn any_syncing(&self) -> bool {
        !self.worker.busy.is_empty()
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
                    let changed = quiet && s > Duration::from_secs(5) && engine::has_local_changes(&self.store, &id);
                    let every = Duration::from_secs(refresh as u64 * 60);
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

    /// A sync ended. Returns whether it changed notes or folders.
    fn handle_synced(&mut self, r: Synced) -> bool {
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
                return false;
            }
        };
        if !exists {
            return false; // removed meanwhile
        }
        match &out.error {
            Some(e) => {
                self.sync_errors.insert(r.account.clone(), e.clone());
            }
            None => {
                self.sync_errors.remove(&r.account);
            }
        }
        let unchanged = out.unchanged;
        self.attachments_arrived |= out.attachments_fetched > 0;
        let notices = engine::apply(&mut self.store, &r.snapshot, out);
        self.notices.extend(notices);
        if unchanged {
            self.save(true);
            return false;
        }
        if let Err(e) = self.db.save_all(&self.store, &self.settings) {
            eprintln!("saving after sync: {e:#}");
        }
        // Unlocked notes' text follows the sync too (or they close, if the
        // new version doesn't open with the keys at hand).
        self.refresh_unlocked();
        true
    }

    // ---- Locked notes -----------------------------------------------------------

    pub fn is_locked(&self, id: &str) -> bool {
        self.store.note(id).is_some_and(|n| lock::is_locked(&n.body))
    }

    /// A locked note that hasn't been unlocked.
    pub fn is_closed(&self, id: &str) -> bool {
        self.is_locked(id) && !self.lock.unlocked.contains_key(id)
    }

    /// A note's Markdown: its text, or a locked note's once unlocked (opening
    /// it with keys from this session if it can).
    pub fn plain_body(&mut self, id: &str) -> Option<String> {
        if self.is_closed(id) {
            self.try_open(id);
        }
        self.plain_body_now(id)
    }

    /// [`Library::plain_body`] without trying to open a locked note.
    pub fn plain_body_now(&self, id: &str) -> Option<String> {
        let n = self.store.note(id)?;
        if lock::is_locked(&n.body) {
            self.lock.unlocked.get(id).cloned()
        } else {
            Some(n.body.clone())
        }
    }

    pub fn has_unlocked(&self) -> bool {
        !self.lock.unlocked.is_empty()
    }

    /// Whether a password for locked notes has been chosen.
    pub fn has_lock_password(&self) -> bool {
        self.settings.lock_check.is_some()
    }

    /// The user did something: unlocked notes stay open another while.
    pub fn touch_lock(&mut self) {
        self.lock.last_use = Some(Instant::now());
    }

    /// Opens a locked note with keys already made this session.
    fn try_open(&mut self, id: &str) -> bool {
        let Some(env) = self.store.note(id).and_then(|n| lock::parse(&n.body)) else { return false };
        let Some(k) = self.lock.keys.get(&env.salt).cloned() else { return false };
        match lock::open(&k, &env) {
            Ok(text) => {
                self.lock.unlocked.insert(id.to_owned(), text);
                self.upgrade_unlocked(id);
                true
            }
            Err(_) => false,
        }
    }

    /// Opens every locked note the keys made this session open.
    fn open_all(&mut self) {
        let ids: Vec<String> = self.store.notes.iter().filter(|n| lock::is_locked(&n.body)).map(|n| n.id.clone()).collect();
        for id in ids {
            if !self.lock.unlocked.contains_key(&id) {
                self.try_open(&id);
            }
        }
    }

    fn hint_suffix(&self) -> String {
        if self.settings.lock_hint.is_empty() {
            String::new()
        } else {
            format!(" Hint: {}", self.settings.lock_hint)
        }
    }

    /// The lock screen's "View Note": opens a locked note (and every other
    /// one the password opens). Errors with a message to show.
    pub fn unlock(&mut self, id: &str, password: &str) -> Result<(), String> {
        let Some(env) = self.store.note(id).and_then(|n| lock::parse(&n.body)) else { return Ok(()) };
        let opened = lock::derive_for(password, &env).and_then(|k| lock::open(&k, &env).map(|t| (k, t)));
        match opened {
            Ok((k, text)) => {
                self.lock.keys.insert(env.salt.clone(), k.clone());
                self.lock.unlocked.insert(id.to_owned(), text);
                self.upgrade_unlocked(id);
                self.touch_lock();
                // The settings' check: this device learns the password's
                // salt from the note if it hasn't got one, and the password
                // is the current one if it opens the check.
                match self.settings.lock_check.as_deref().and_then(|c| lock::decode(c).ok()) {
                    None => {
                        let check = lock::seal(&k, &env.salt(), CHECK);
                        self.settings.lock_check = Some(lock::encode(&check));
                        self.lock.current = Some(password.to_owned());
                    }
                    Some(check) => {
                        let k2 = if check.salt == env.salt { Ok(k) } else { lock::derive_for(password, &check) };
                        if let Ok(k2) = k2 {
                            if lock::open(&k2, &check).is_ok() {
                                self.lock.keys.insert(check.salt.clone(), k2);
                                self.lock.current = Some(password.to_owned());
                            }
                        }
                    }
                }
                // Other notes locked with it open now too.
                self.open_all();
                Ok(())
            }
            Err(e) => {
                let msg = if e.to_string() == "wrong password" { "Wrong password.".to_owned() } else { e.to_string() };
                Err(format!("{msg}{}", self.hint_suffix()))
            }
        }
    }

    /// "Lock Note" (with the open editor's changes copied in first).
    pub fn lock_note(&mut self, id: &str) -> LockNote {
        self.lock_notes(&[id.to_owned()])
    }

    /// A note "Lock Note" applies to: there, not deleted, not locked yet.
    pub fn can_lock(&self, id: &str) -> bool {
        self.store.note(id).is_some_and(|n| n.deleted.is_none() && !lock::is_locked(&n.body))
    }

    /// "Lock Notes" with several notes selected: the password is asked for
    /// once, then every one of them not locked yet (nor deleted) is locked.
    pub fn lock_notes(&mut self, ids: &[String]) -> LockNote {
        let ids: Vec<String> = ids.iter().filter(|id| self.can_lock(id)).cloned().collect();
        if ids.is_empty() {
            return LockNote::Locked;
        }
        if self.settings.lock_check.is_none() {
            return LockNote::ChoosePassword;
        }
        let Some((k, salt)) = self.current_keys() else { return LockNote::EnterPassword };
        let mut locked = 0;
        // Notes whose pictures aren't all on this device yet (a sync is
        // bringing them): they can't be encrypted, so the note isn't locked.
        let mut missing = 0;
        for id in ids {
            let Some(text) = self.plain_body_now(&id) else { continue };
            // Its pictures are encrypted too, each with a key of its own
            // that goes in the sealed text.
            let (text, all) = self.convert_refs(&text, true);
            if !all {
                missing += 1;
                continue;
            }
            let env = lock::seal(&k, &salt, &text);
            if let Some(n) = self.store.note_mut(&id) {
                n.body = lock::locked_body(&text, &env);
                n.touch_keep_date();
            }
            self.lock.unlocked.insert(id.clone(), text);
            self.unsaved.insert(id);
            locked += 1;
        }
        if locked > 0 {
            self.touch_lock();
            let what = if locked == 1 { "Note locked".to_owned() } else { format!("{locked} notes locked") };
            let skipped = match missing {
                0 => String::new(),
                1 => " A note whose images aren't on this device yet wasn't locked.".to_owned(),
                n => format!(" {n} notes whose images aren't on this device yet weren't locked."),
            };
            self.push_notice(format!("{what}. Choose Lock Now to close locked notes.{skipped}"));
        } else if missing > 0 {
            self.push_notice("This note's images aren't on this device yet, so it can't be locked. Try again once they've synced.".into());
        }
        LockNote::Locked
    }

    /// The current password's keys and salt (those of the settings' check).
    fn current_keys(&mut self) -> Option<(Keys, Vec<u8>)> {
        let password = self.lock.current.clone()?;
        let check = lock::decode(self.settings.lock_check.as_deref()?).ok()?;
        let k = match self.lock.keys.get(&check.salt) {
            Some(k) => k.clone(),
            None => lock::derive_for(&password, &check).ok()?,
        };
        self.lock.keys.insert(check.salt.clone(), k.clone());
        Some((k, check.salt()))
    }

    /// "Remove Lock": the note is stored as plain text again (with the open
    /// editor's changes copied in first).
    pub fn remove_lock(&mut self, id: &str) {
        let Some(text) = self.lock.unlocked.remove(id) else { return };
        // Its pictures are kept plainly again (any not on this device yet
        // keep their keys in the text, and still show once they arrive).
        let (text, _) = self.convert_refs(&text, false);
        if let Some(n) = self.store.note_mut(id) {
            n.body = text;
            n.touch_keep_date();
        }
        self.unsaved.insert(id.to_owned());
    }

    /// Closes every unlocked note and forgets the password (with the open
    /// editor's changes copied in first). Returns whether any note was open.
    pub fn lock_now(&mut self) -> bool {
        let any = !self.lock.unlocked.is_empty();
        self.lock.unlocked.clear();
        self.lock.keys.clear();
        self.lock.refs.clear();
        self.lock.current = None;
        // Locked notes' files opened in other apps were copied out decrypted.
        self.clear_opened_files();
        any
    }

    /// Chooses the password for locked notes (the first time).
    pub fn set_password(&mut self, password: &str, hint: &str) -> Result<(), String> {
        if password.is_empty() {
            return Err("Enter a password.".into());
        }
        let salt = lock::new_salt();
        let k = lock::derive(password, &salt).map_err(|e| e.to_string())?;
        let check = lock::seal(&k, &salt, CHECK);
        self.lock.keys.insert(check.salt.clone(), k);
        self.settings.lock_check = Some(lock::encode(&check));
        self.settings.lock_hint = hint.trim().to_owned();
        self.lock.current = Some(password.to_owned());
        self.touch_lock();
        Ok(())
    }

    /// Enters the password (to lock a note with it).
    pub fn enter_password(&mut self, password: &str) -> Result<(), String> {
        self.check_password(password)?;
        self.lock.current = Some(password.to_owned());
        self.touch_lock();
        Ok(())
    }

    /// Changes the password: every locked note the old one opens is sealed
    /// again with the new one. Returns how many notes changed.
    pub fn change_password(&mut self, old: &str, password: &str, hint: &str) -> Result<usize, String> {
        if password.is_empty() {
            return Err("Enter a password.".into());
        }
        self.check_password(old)?;
        let salt = lock::new_salt();
        let k = lock::derive(password, &salt).map_err(|e| e.to_string())?;
        let mut by_salt: HashMap<String, Option<Keys>> = HashMap::new();
        let mut changed = 0;
        let ids: Vec<String> = self.store.notes.iter().filter(|n| lock::is_locked(&n.body)).map(|n| n.id.clone()).collect();
        for id in ids {
            let Some(env) = self.store.note(&id).and_then(|n| lock::parse(&n.body)) else { continue };
            let ok = by_salt.entry(env.salt.clone()).or_insert_with(|| lock::derive_for(old, &env).ok()).clone();
            let Some(ok) = ok else { continue };
            let Ok(text) = lock::open(&ok, &env) else { continue };
            let sealed = lock::seal(&k, &salt, &text);
            if let Some(n) = self.store.note_mut(&id) {
                n.body = lock::locked_body(&text, &sealed);
                n.touch_keep_date();
            }
            self.unsaved.insert(id);
            changed += 1;
        }
        let check = lock::seal(&k, &salt, CHECK);
        self.lock.keys.insert(check.salt.clone(), k);
        self.settings.lock_check = Some(lock::encode(&check));
        self.settings.lock_hint = hint.trim().to_owned();
        self.lock.current = Some(password.to_owned());
        self.lock.unlocked.clear();
        self.open_all();
        self.push_notice(format!("Password changed for {changed} locked note{}.", if changed == 1 { "" } else { "s" }));
        Ok(changed)
    }

    /// Checks a password against the settings' check.
    fn check_password(&mut self, password: &str) -> Result<Keys, String> {
        let check = self.settings.lock_check.as_deref().and_then(|c| lock::decode(c).ok()).ok_or("No password is set.")?;
        let k = lock::derive_for(password, &check).map_err(|e| e.to_string())?;
        lock::open(&k, &check).map_err(|_| format!("Wrong password.{}", self.hint_suffix()))?;
        self.lock.keys.insert(check.salt.clone(), k.clone());
        Ok(k)
    }

    /// Re-encrypts an unlocked note's edited text. `None` if `id` isn't a
    /// locked note.
    fn store_locked(&mut self, id: &str, md: &str) -> Option<BodySet> {
        let env = self.store.note(id).and_then(|n| lock::parse(&n.body))?;
        if !self.lock.unlocked.contains_key(id) {
            // Never write a locked note's text out in the clear.
            return Some(BodySet::Unchanged);
        }
        // Plain pictures (an editor opened before "Lock Note", or pasted
        // from another note) are encrypted for it.
        let (md, _) = self.convert_refs(md, true);
        let md = md.as_str();
        if self.lock.unlocked.get(id).is_some_and(|t| t == md) {
            return Some(BodySet::Unchanged);
        }
        let Some(k) = self.lock.keys.get(&env.salt).cloned() else {
            // Locked again elsewhere with another password: the edit can't
            // be sealed, so say so and close the note rather than drop it
            // silently (never write it out in the clear).
            self.lock.unlocked.remove(id);
            self.push_notice("A change to a locked note couldn't be saved: it was locked with another password on another device.".into());
            return Some(BodySet::Closed);
        };
        let sealed = lock::seal(&k, &env.salt(), md);
        if let Some(n) = self.store.note_mut(id) {
            n.body = lock::locked_body(md, &sealed);
            n.touch();
        }
        self.lock.unlocked.insert(id.to_owned(), md.to_owned());
        self.unsaved.insert(id.to_owned());
        self.last_edit = Some(Instant::now());
        self.touch_lock();
        Some(BodySet::Changed)
    }

    /// After a sync: every unlocked note's text is read again from its
    /// (possibly new) sealed text, so a stale copy is never shown or sealed
    /// over another device's edit. Notes gone, no longer locked, or locked
    /// with keys not at hand are closed.
    fn refresh_unlocked(&mut self) {
        let ids: Vec<String> = self.lock.unlocked.keys().cloned().collect();
        for id in ids {
            self.lock.unlocked.remove(&id);
            self.try_open(&id);
        }
    }

    // ---- For tests ----------------------------------------------------------

    /// Feeds a sync's reply in as if it came from the sync thread.
    #[doc(hidden)]
    pub fn test_handle_synced(&mut self, r: Synced) -> bool {
        self.handle_synced(r)
    }

    #[doc(hidden)]
    pub fn test_handle_renewed(&mut self, account: &str, secret: String) {
        self.handle_renewed(account, secret)
    }

    /// Pretends a sync started with `secret`.
    #[doc(hidden)]
    pub fn test_sync_started_with(&mut self, account: &str, secret: Option<&str>) {
        self.sync_secret.insert(account.to_owned(), secret.map(str::to_owned));
    }

    /// Pretends the last use of unlocked notes was `ago`.
    #[doc(hidden)]
    pub fn test_last_lock_use(&mut self, ago: Duration) {
        self.lock.last_use = Instant::now().checked_sub(ago);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lib() -> (Library, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        (Library::for_test(dir.path()), dir)
    }

    fn add(lib: &mut Library, folder: &str, body: &str) -> String {
        let mut n = Note::new(LOCAL, folder, body);
        n.modified = Utc::now() + chrono::Duration::seconds(lib.store.notes.len() as i64);
        let id = n.id.clone();
        lib.store.notes.push(n);
        id
    }

    #[test]
    fn all_notes_shows_every_folder_once_there_are_two() {
        let (mut lib, _d) = lib();
        assert!(!lib.show_all_notes(), "one folder: no All Notes");
        let notes = lib.default_folder_id(LOCAL);
        let work = lib.store.ensure_folder_at(LOCAL, &["Work".into()]);
        assert!(lib.show_all_notes());
        let a = add(&mut lib, &notes, "# A\n");
        let b = add(&mut lib, &work, "# B\n");
        let gone = add(&mut lib, &work, "# Gone\n");
        lib.store.note_mut(&gone).unwrap().deleted = Some(Utc::now());
        let shown = lib.visible_notes(&Location::All, "");
        assert!(shown.contains(&a) && shown.contains(&b));
        assert!(!shown.contains(&gone), "deleted notes stay in Recently Deleted");
        // A new note made in All Notes stays in view.
        let (_, _, shown) = lib.new_note(&Location::All);
        assert_eq!(shown, Location::All);
        lib.remember_location(&Location::All);
        assert_eq!(lib.initial_location().0, Location::All);
    }

    #[test]
    fn a_new_note_in_a_tag_carries_the_tag() {
        let (mut lib, _d) = lib();
        let home = lib.default_folder_id(LOCAL);
        add(&mut lib, &home, "# Plan\n#work\n");
        let tag = Location::Tag("work".into());
        let (id, body, shown) = lib.new_note(&tag);
        assert_eq!(shown, tag);
        assert_eq!(lib.store.note(&id).unwrap().body, body);
        assert!(lib.store.note(&id).unwrap().tags().contains(&"work".to_owned()));
        assert!(lib.visible_notes(&tag, "").contains(&id));
    }

    #[test]
    fn on_my_pc_can_be_removed_and_added_back() {
        let dir = tempfile::tempdir().unwrap();
        let mut lib = Library::for_test(dir.path());
        let mut acc = lib.store.accounts[0].clone();
        acc.id = "other".into();
        acc.name = "Other".into();
        lib.store.accounts.push(acc);
        lib.store.ensure_local();
        let home = lib.default_folder_id(LOCAL);
        add(&mut lib, &home, "# Only here\n");
        let theirs = lib.default_folder_id("other");
        let mut n = Note::new("other", &theirs, "# Kept\n");
        let kept = n.id.clone();
        n.modified = Utc::now();
        lib.store.notes.push(n);
        lib.mark_changed(&kept);
        lib.save(true);
        assert_eq!(lib.account_note_count(LOCAL), 1);
        // Removed: its notes and folders are gone, the other account's stay.
        assert!(lib.can_remove_account(LOCAL));
        lib.remove_account(LOCAL);
        assert!(lib.store.account(LOCAL).is_none());
        assert!(lib.store.notes.iter().all(|n| n.account != LOCAL));
        assert!(lib.store.folders.iter().all(|f| f.account != LOCAL));
        assert!(lib.store.note(&kept).is_some());
        // New notes and startup fall back on the account that's left.
        assert_eq!(lib.fallback_account(), "other");
        assert_eq!(lib.new_note_target(&Location::All), ("other".to_owned(), theirs.clone()));
        // It stays gone after a restart.
        drop(lib);
        let mut lib = Library::for_test(dir.path());
        assert!(lib.store.account(LOCAL).is_none());
        assert_eq!(lib.initial_location().0, Location::Folder(theirs));
        // The last account can't be removed.
        assert!(!lib.can_remove_account("other"));
        lib.remove_account("other");
        assert!(lib.store.account("other").is_some());
        // Added back, empty, with its Notes folder.
        lib.add_local_account();
        assert!(lib.store.account(LOCAL).is_some());
        assert_eq!(lib.account_note_count(LOCAL), 0);
        assert!(lib.store.default_folder(LOCAL).is_some());
    }

    #[test]
    fn new_notes_never_go_to_a_disabled_account() {
        let (mut lib, _d) = lib();
        let mut acc = lib.store.accounts[0].clone();
        acc.id = "other".into();
        acc.name = "Other".into();
        lib.store.accounts.push(acc);
        lib.store.ensure_local();
        let f = lib.store.ensure_folder_at("other", &["Stuff".into()]);
        assert_eq!(lib.new_note_target(&Location::Folder(f.clone())).1, f);
        lib.store.account_mut("other").unwrap().enabled = false;
        let (account, _) = lib.new_note_target(&Location::Folder(f));
        assert_ne!(account, "other");
    }

    #[test]
    fn startup_only_reopens_a_note_the_list_shows() {
        let (mut lib, _d) = lib();
        let home = lib.default_folder_id(LOCAL);
        let elsewhere = lib.store.ensure_folder_at(LOCAL, &["Elsewhere".into()]);
        let here = add(&mut lib, &home, "# Here\n");
        let there = add(&mut lib, &elsewhere, "# There\n");
        lib.settings.last_folder = Some(home.clone());
        lib.settings.last_note = Some(there);
        assert_eq!(lib.initial_location(), (Location::Folder(home), Some(here)));
    }

    #[test]
    fn smart_folders_gather_notes_from_every_folder() {
        let (mut lib, _d) = lib();
        let home = lib.default_folder_id(LOCAL);
        let other = lib.new_folder(LOCAL, None, Some("Other"));
        let shop = add(&mut lib, &home, "# Shop #errands\n- [ ] milk\n");
        let done = add(&mut lib, &other, "# Done #errands\n- [x] post\n");
        add(&mut lib, &other, "# Plain\nno tags\n");
        let filter = NoteFilter { tags: vec!["errands".into()], ..Default::default() };
        let id = lib.add_smart_folder("  ", filter.clone());
        let smart = Location::Smart(id.clone());
        assert!(lib.location_exists(&smart));
        assert_eq!(lib.location_title(&smart, ""), "Smart Folder");
        let mut found = lib.visible_notes(&smart, "");
        found.sort();
        let mut want = vec![shop.clone(), done.clone()];
        want.sort();
        assert_eq!(found, want);
        assert!(lib.list_shows_folders(&smart, ""));

        lib.update_smart_folder(&id, "To Do", NoteFilter { checklists: crate::settings::ChecklistFilter::Unchecked, ..filter });
        assert_eq!(lib.location_title(&smart, ""), "To Do");
        assert_eq!(lib.visible_notes(&smart, ""), [shop.clone()]);

        // A new note made there carries its tags, and stays in it if it can.
        lib.update_smart_folder(&id, "Errands", NoteFilter { tags: vec!["errands".into()], ..Default::default() });
        let (n, body, shown) = lib.new_note(&smart);
        assert_eq!(body, "\n#errands\n");
        assert_eq!(shown, smart);
        assert!(lib.visible_notes(&smart, "").contains(&n));
        let pinned_only = lib.add_smart_folder("Pinned", NoteFilter { pinned: true, ..Default::default() });
        let (_, _, shown) = lib.new_note(&Location::Smart(pinned_only));
        assert!(matches!(shown, Location::Folder(_)), "a new note isn't pinned: shown in its folder");

        lib.delete_smart_folder(&id);
        assert!(!lib.location_exists(&smart));
    }

    #[test]
    fn search_puts_title_matches_first_and_shows_where_the_words_are() {
        let (mut lib, _d) = lib();
        let home = lib.default_folder_id(LOCAL);
        let shopping = add(&mut lib, &home, "# Shopping\nmilk, bread\n");
        let recipe = add(&mut lib, &home, "# Pancakes\nBeat the eggs in a large bowl with a fork, then pour in the milk slowly\n");
        add(&mut lib, &home, "# Other\nnothing here\n");
        let loc = Location::Folder(home);
        let sections = lib.list_sections(&loc, "MILK");
        assert_eq!(sections, [("Notes".to_owned(), vec![recipe.clone(), shopping.clone()])]);
        let sections = lib.list_sections(&loc, "shop milk");
        assert_eq!(sections, [("Notes".to_owned(), vec![shopping.clone()])], "every word must match");
        let sections = lib.list_sections(&loc, "pan");
        assert_eq!(sections, [("Top Hits".to_owned(), vec![recipe.clone()])]);

        let (title, snippet) = lib.search_parts(&recipe, "milk pour").unwrap();
        assert_eq!(title, [("Pancakes".to_owned(), false)]);
        let text: String = snippet.iter().map(|(t, _)| t.as_str()).collect();
        assert!(text.starts_with("…bowl with a fork, then pour"), "cut a little before the match: {text}");
        let bold: Vec<&str> = snippet.iter().filter(|(_, m)| *m).map(|(t, _)| t.as_str()).collect();
        assert_eq!(bold, ["pour", "milk"]);
        assert!(lib.search_parts(&recipe, "  ").is_none());
    }

    #[test]
    fn empty_new_notes_go_away_when_left() {
        let (mut lib, _d) = lib();
        let home = Location::Folder(lib.default_folder_id(LOCAL));
        let (id, ..) = lib.new_note(&home);
        assert!(lib.leave_note(&id));
        assert!(lib.store.note(&id).is_none());
        let (id, ..) = lib.new_note(&home);
        assert_eq!(lib.set_body(&id, "# Kept\n"), BodySet::Changed);
        assert!(!lib.leave_note(&id));
    }

    #[test]
    fn undoing_a_deletion_brings_notes_back_as_they_were() {
        let (mut lib, _d) = lib();
        let home = lib.default_folder_id(LOCAL);
        let loc = Location::Folder(home.clone());
        let a = add(&mut lib, &home, "# A\n");
        lib.toggle_pin(&a);
        let when = lib.store.note(&a).unwrap().modified;
        lib.trash_notes(&[a.clone()], &loc);
        assert!(lib.store.note(&a).unwrap().deleted.is_some());
        let r = lib.undo_delete().unwrap();
        assert_eq!(r, Restored { ids: vec![a.clone()], location: loc });
        let n = lib.store.note(&a).unwrap();
        assert!(n.deleted.is_none() && n.pinned && n.modified == when);
        assert_eq!(lib.redo_delete(), Some(vec![a.clone()]));
        assert!(lib.store.note(&a).unwrap().deleted.is_some());
        // Deleted for good meanwhile: nothing to bring back.
        lib.undo_delete();
        lib.trash_notes(&[a.clone()], &Location::All);
        lib.delete_forever(&[a]);
        assert_eq!(lib.undo_delete(), None);
    }

    #[test]
    fn images_are_saved_once() {
        let (lib, _d) = lib();
        let name = lib.add_image(b"\x89PNG\r\n\x1a\npicture").unwrap();
        assert_eq!(lib.add_image(b"\x89PNG\r\n\x1a\npicture").unwrap(), name);
        assert!(lib.add_image(b"not a picture").is_err());
        assert_eq!(std::fs::read(lib.attachment_path(&name).unwrap()).unwrap(), b"\x89PNG\r\n\x1a\npicture");
        assert_eq!(lib.attachment_data(&name).unwrap(), b"\x89PNG\r\n\x1a\npicture");
        assert_eq!(lib.attachment_path(&crate::attachments::name_for(b"x", "png")), None);
    }

    #[test]
    fn locked_notes_keep_their_pictures_encrypted() {
        use crate::attachments;
        const PIC: &[u8] = b"\x89PNG\r\n\x1a\nholiday";
        const PIC2: &[u8] = b"\x89PNG\r\n\x1a\nboarding pass";
        let (mut lib, _d) = lib();
        let home = lib.default_folder_id(LOCAL);
        let name = lib.add_image(PIC).unwrap();
        let plain_md = format!("# Trip\n![](attachments/{name})\n");
        let id = add(&mut lib, &home, &plain_md);
        lib.set_password("pw", "").unwrap();
        assert_eq!(lib.lock_note(&id), LockNote::Locked);
        assert!(lib.is_locked(&id));

        // Stored: no picture in the clear, only its encrypted file's name
        // beside the seal (for syncing it), never its key.
        let body = lib.store.note(&id).unwrap().body.clone();
        assert!(!body.contains(&name), "{body}");
        let refs = attachments::refs_in_body(&body);
        assert_eq!(refs.len(), 1);
        assert!(refs[0].ends_with(".enc"));
        let blob = std::fs::read(attachments::path(&lib.attachments_dir(), &refs[0])).unwrap();
        assert!(attachments::verify(&refs[0], &blob) && !blob.windows(PIC.len()).any(|w| w == PIC));

        // Open, the note shows the encrypted picture, decrypted in memory.
        let md = lib.plain_body_now(&id).unwrap();
        let sealed = attachments::image_refs_in(&md).pop().unwrap();
        assert!(attachments::is_sealed_ref(&sealed) && !body.contains(sealed.split('#').nth(1).unwrap()));
        assert_eq!(lib.attachment_data(&sealed).unwrap(), PIC);
        assert_eq!(lib.attachment_path(&sealed), None, "never handed out as a file");

        // The editor still showing the plain picture saves the same
        // encrypted one (not a new copy each time).
        assert_eq!(lib.set_body(&id, &plain_md), BodySet::Unchanged);
        // Pictures added to a locked note are encrypted from the start.
        let added = lib.add_image_to(&id, PIC2).unwrap();
        assert!(attachments::is_sealed_ref(&added));
        assert!(!attachments::exists(&lib.attachments_dir(), &attachments::name_for(PIC2, "png")));
        assert_eq!(lib.set_body(&id, &format!("{md}{}\n", attachments::markdown(&added))), BodySet::Changed);
        assert_eq!(attachments::refs_in_body(&lib.store.note(&id).unwrap().body).len(), 2);

        // A new password: the pictures' keys are in the text, so they open.
        lib.change_password("pw", "pw2", "").unwrap();
        lib.lock_now();
        assert_eq!(lib.attachment_data(&added).unwrap(), PIC2, "a reference holds its key");
        lib.unlock(&id, "pw2").unwrap();
        let md = lib.plain_body_now(&id).unwrap();
        assert_eq!(attachments::image_refs_in(&md), vec![sealed.clone(), added.clone()]);

        // Remove Lock: plain pictures again.
        lib.remove_lock(&id);
        let body = lib.store.note(&id).unwrap().body.clone();
        assert_eq!(body, format!("# Trip\n![](attachments/{name})\n![](attachments/{})\n", attachments::name_for(PIC2, "png")));
        assert_eq!(lib.attachment_data(&attachments::name_for(PIC2, "png")).unwrap(), PIC2);
        // An editor still showing the encrypted ones saves the plain ones.
        assert_eq!(lib.set_body(&id, &md), BodySet::Unchanged);
    }

    #[test]
    fn locked_notes_keep_their_files_encrypted() {
        use crate::attachments;
        const PDF: &[u8] = b"%PDF-1.7 boarding pass";
        const DOC: &[u8] = b"PK\x03\x04 a document";
        let (mut lib, _d) = lib();
        let home = lib.default_folder_id(LOCAL);
        let id = add(&mut lib, &home, "# Trip\n");
        let name = lib.add_file_to(&id, PDF, "boarding-pass.pdf").unwrap();
        assert_eq!(name, attachments::name_for(PDF, "pdf"));
        let plain_md = format!("# Trip\n[boarding-pass.pdf](attachments/{name})\n");
        assert_eq!(lib.set_body(&id, &plain_md), BodySet::Changed);
        lib.set_password("pw", "").unwrap();
        assert_eq!(lib.lock_note(&id), LockNote::Locked);

        // Stored encrypted, as pictures are.
        let body = lib.store.note(&id).unwrap().body.clone();
        assert!(!body.contains(&name) && !body.contains("boarding"), "{body}");
        let refs = attachments::refs_in_body(&body);
        assert!(refs.len() == 1 && refs[0].ends_with(".enc"));
        let md = lib.plain_body_now(&id).unwrap();
        let sealed = attachments::image_refs_in(&md).pop().unwrap();
        assert!(md.contains(&format!("[boarding-pass.pdf](attachments/{sealed})")), "{md}");
        assert_eq!(lib.attachment_data(&sealed).unwrap(), PDF);

        // Files added to a locked note are encrypted from the start.
        let added = lib.add_file_to(&id, DOC, "plan.docx").unwrap();
        assert!(attachments::is_sealed_ref(&added));
        assert!(!attachments::exists(&lib.attachments_dir(), &attachments::name_for(DOC, "docx")));
        let md = format!("{md}[plan.docx](attachments/{added})\n");
        assert_eq!(lib.set_body(&id, &md), BodySet::Changed);

        // Remove Lock: plain files again, under their names' extensions.
        lib.remove_lock(&id);
        let body = lib.store.note(&id).unwrap().body.clone();
        let docx = attachments::name_for(DOC, "docx");
        assert_eq!(body, format!("# Trip\n[boarding-pass.pdf](attachments/{name})\n[plan.docx](attachments/{docx})\n"));
        assert_eq!(lib.attachment_data(&docx).unwrap(), DOC);
    }

    #[test]
    fn a_note_whose_pictures_havent_arrived_isnt_locked() {
        let (mut lib, _d) = lib();
        let home = lib.default_folder_id(LOCAL);
        let lost = crate::attachments::name_for(b"\x89PNG\r\n\x1a\nelsewhere", "png");
        let a = add(&mut lib, &home, &format!("# A\n![](attachments/{lost})\n"));
        let b = add(&mut lib, &home, "# B\n");
        lib.set_password("pw", "").unwrap();
        lib.take_notices();
        assert_eq!(lib.lock_notes(&[a.clone(), b.clone()]), LockNote::Locked);
        assert!(!lib.is_locked(&a) && lib.is_locked(&b));
        assert_eq!(
            lib.take_notices(),
            vec!["Note locked. Choose Lock Now to close locked notes. A note whose images aren't on this device yet wasn't locked.".to_owned()]
        );
    }

    #[test]
    fn locking_a_note_keeps_its_last_modified_date() {
        let (mut lib, _d) = lib();
        let home = lib.default_folder_id(LOCAL);
        let a = add(&mut lib, &home, "# Secret\n");
        let b = add(&mut lib, &home, "# Other\n");
        let when = |lib: &Library, id: &str| lib.store.note(id).unwrap().modified;
        let (wa, wb) = (when(&lib, &a), when(&lib, &b));
        lib.set_password("pw", "").unwrap();
        assert_eq!(lib.lock_note(&a), LockNote::Locked);
        assert_eq!(lib.lock_note(&b), LockNote::Locked);
        let n = lib.store.note(&a).unwrap();
        assert!(lock::is_locked(&n.body) && n.sync.dirty);
        assert_eq!((when(&lib, &a), when(&lib, &b)), (wa, wb));
        // A new password seals them again: still not a change to the text.
        assert_eq!(lib.change_password("pw", "pw2", ""), Ok(2));
        assert_eq!((when(&lib, &a), when(&lib, &b)), (wa, wb));
        lib.remove_lock(&b);
        assert!(!lock::is_locked(&lib.store.note(&b).unwrap().body));
        assert_eq!(when(&lib, &b), wb);
        // Editing a locked note is a change.
        assert_eq!(lib.set_body(&a, "# Secret\n\nmore\n"), BodySet::Changed);
        assert!(when(&lib, &a) > wa);
    }

    #[test]
    fn a_sync_from_before_signing_in_again_doesnt_undo_it() {
        let (mut lib, _d) = lib();
        let mut acc = lib.store.accounts[0].clone();
        acc.id = "sn".into();
        let stale = |lib: &Library| Synced {
            account: "sn".into(),
            snapshot: AccountSnapshot::of(&lib.store, "sn"),
            result: Err("the password was changed".into()),
            needs_sign_in: true,
            renewed_secret: Some("old session, renewed".into()),
        };
        // A sync starts with the old sign-in…
        lib.store.accounts.push(acc.clone());
        lib.db.set_secret("sn", Some("old session")).unwrap();
        lib.test_sync_started_with("sn", Some("old session"));
        // …the user signs in again meanwhile…
        lib.add_account(acc, Some("new session".into()));
        // …and the old sync's reply arrives.
        let r = stale(&lib);
        lib.handle_synced(r);
        assert_eq!(lib.db.secret("sn").as_deref(), Some("new session"));
        assert!(!lib.needs_sign_in.contains("sn"));
        // Removed while syncing: its secret isn't written back.
        lib.test_sync_started_with("sn", Some("new session"));
        lib.remove_account("sn");
        let r = stale(&lib);
        lib.handle_synced(r);
        assert_eq!(lib.db.secret("sn"), None);
        assert!(!lib.needs_sign_in.contains("sn"));
    }

    #[test]
    fn credentials_renewed_mid_sync_are_saved_at_once_unless_replaced() {
        let (mut lib, _d) = lib();
        let mut acc = lib.store.accounts[0].clone();
        acc.id = "sn".into();
        lib.store.accounts.push(acc.clone());
        lib.db.set_secret("sn", Some("s0")).unwrap();
        lib.test_sync_started_with("sn", Some("s0"));
        // Renewed twice during one sync: each saved as it comes.
        lib.handle_renewed("sn", "s1".into());
        assert_eq!(lib.db.secret("sn").as_deref(), Some("s1"));
        lib.handle_renewed("sn", "s2".into());
        assert_eq!(lib.db.secret("sn").as_deref(), Some("s2"));
        // The sync's end, with the last one, isn't taken for a sign-in
        // that changed meanwhile.
        let r = Synced {
            account: "sn".into(),
            snapshot: AccountSnapshot::of(&lib.store, "sn"),
            result: Err("offline".into()),
            needs_sign_in: false,
            renewed_secret: Some("s2".into()),
        };
        lib.handle_synced(r);
        assert_eq!(lib.db.secret("sn").as_deref(), Some("s2"));
        // Signed in again during a sync: its renewals are dropped.
        lib.test_sync_started_with("sn", Some("s2"));
        lib.add_account(acc, Some("fresh".into()));
        lib.handle_renewed("sn", "s3".into());
        assert_eq!(lib.db.secret("sn").as_deref(), Some("fresh"));
    }

    #[test]
    fn folders_rename_and_delete() {
        let (mut lib, _d) = lib();
        let a = lib.new_folder(LOCAL, None, Some("Work"));
        let b = lib.new_folder(LOCAL, None, None);
        assert_eq!(lib.store.folder(&b).unwrap().name, "New Folder");
        assert!(lib.rename_folder(&b, "work").is_err(), "names are unique, ignoring case");
        lib.rename_folder(&b, "Home").unwrap();
        let n = add(&mut lib, &a, "# In work\n");
        let sub = lib.new_folder(LOCAL, Some(&a), None);
        assert_eq!(lib.delete_folder(&a).len(), 2);
        assert!(lib.store.folder(&sub).is_none());
        assert!(lib.store.note(&n).unwrap().deleted.is_some());
    }

    #[test]
    fn the_notes_folder_can_be_deleted_and_comes_back_when_used() {
        let (mut lib, _d) = lib();
        let notes = lib.default_folder_id(LOCAL);
        let work = lib.new_folder(LOCAL, None, Some("Work"));
        let n = add(&mut lib, &notes, "# Old\n");
        let sub = lib.new_folder(LOCAL, Some(&notes), Some("Inside"));
        let shown = |lib: &Library| lib.store.folder_tree(LOCAL).iter().map(|(f, _)| f.name.clone()).collect::<Vec<_>>();
        assert_eq!(shown(&lib), ["Notes", "Inside", "Work"]);

        assert_eq!(lib.delete_folder(&notes).len(), 2);
        assert!(lib.store.note(&n).unwrap().deleted.is_some(), "its notes go to Recently Deleted");
        assert!(lib.store.folder(&sub).is_none(), "so do its folders");
        assert_eq!(shown(&lib), ["Work"], "Notes is out of the sidebar");
        assert_eq!(lib.default_folder_id(LOCAL), notes, "but still there for notes with nowhere else to go");
        assert_eq!(lib.new_note_target(&Location::All), (LOCAL.to_owned(), work.clone()), "new notes go elsewhere");
        assert_eq!(lib.initial_location().0, Location::Folder(work.clone()));

        // A note recovered into it brings it back while it has notes.
        lib.recover_note(&n);
        assert_eq!(shown(&lib), ["Notes", "Work"]);
        lib.move_notes(&[n.clone()], &work);
        assert_eq!(shown(&lib), ["Work"]);

        // As does making a folder called Notes, for good.
        assert_eq!(lib.new_folder(LOCAL, None, Some("notes")), notes);
        assert_eq!(shown(&lib), ["Notes", "Work"]);
    }

    #[test]
    fn naming_a_new_folder_notes_brings_the_deleted_notes_folder_back() {
        let (mut lib, _d) = lib();
        let notes = lib.default_folder_id(LOCAL);
        lib.new_folder(LOCAL, None, Some("Work"));
        lib.delete_folder(&notes);
        let shown = |lib: &Library| lib.store.folder_tree(LOCAL).iter().map(|(f, _)| f.name.clone()).collect::<Vec<_>>();
        assert_eq!(shown(&lib), ["Work"]);

        // The desktop app makes "New Folder", then renames it.
        let fresh = lib.new_folder(LOCAL, None, None);
        let n = add(&mut lib, &fresh, "# Kept\n");
        let inner = lib.new_folder(LOCAL, Some(&fresh), Some("Inner"));
        assert_eq!(lib.rename_folder(&fresh, "Notes"), Ok(notes.clone()));
        assert!(lib.store.folder(&fresh).is_none(), "the new folder gives way to Notes");
        assert_eq!(lib.store.note(&n).unwrap().folder, notes, "with its notes");
        assert_eq!(lib.store.folder(&inner).unwrap().parent.as_deref(), Some(notes.as_str()), "and folders");
        assert_eq!(shown(&lib), ["Notes", "Inner", "Work"]);

        // Once it's back, the name is taken as usual.
        let other = lib.new_folder(LOCAL, None, None);
        assert!(lib.rename_folder(&other, "Notes").is_err());
    }

    #[test]
    fn folders_move_into_other_folders() {
        let (mut lib, _d) = lib();
        let work = lib.new_folder(LOCAL, None, Some("Work"));
        let plans = lib.new_folder(LOCAL, None, Some("Plans"));
        let inner = lib.new_folder(LOCAL, Some(&plans), Some("2026"));
        let n = add(&mut lib, &inner, "# Trip\n");
        lib.settings.collapsed.push(work.clone());
        assert!(lib.move_folder(&plans, LOCAL, Some(&work)));
        assert_eq!(lib.store.folder_path(&inner), ["Work", "Plans", "2026"]);
        assert_eq!(lib.store.note(&n).unwrap().folder, inner, "notes stay in their folder");
        assert!(!lib.settings.collapsed.contains(&work), "the new parent opens up");
        // Not into itself or a folder inside it, nor where it already is.
        assert!(!lib.move_folder(&plans, LOCAL, Some(&plans)));
        assert!(!lib.move_folder(&plans, LOCAL, Some(&inner)));
        assert!(!lib.move_folder(&plans, LOCAL, Some(&work)));
        // The default folder stays put.
        let home = lib.default_folder_id(LOCAL);
        assert!(!lib.move_folder(&home, LOCAL, Some(&work)));
        // Back to the top, where a folder of the same name gets a number.
        let other = lib.new_folder(LOCAL, None, Some("plans"));
        assert!(lib.move_folder(&plans, LOCAL, None));
        assert_eq!(lib.store.folder(&plans).unwrap().parent, None);
        assert_eq!(lib.store.folder(&plans).unwrap().name, "Plans 2");
        assert_eq!(lib.store.folder(&other).unwrap().name, "plans");
    }

    #[test]
    fn folders_can_be_put_in_order() {
        let (mut lib, _d) = lib();
        let names = |lib: &Library, parent: Option<&str>| {
            lib.store.child_folders(LOCAL, parent).iter().map(|f| f.name.clone()).collect::<Vec<_>>()
        };
        let a = lib.new_folder(LOCAL, None, Some("A"));
        let b = lib.new_folder(LOCAL, None, Some("B"));
        let c = lib.new_folder(LOCAL, None, Some("C"));
        let home = lib.default_folder_id(LOCAL);
        assert_eq!(names(&lib, None), ["Notes", "A", "B", "C"]);
        // C to the top, then A to the end.
        assert!(lib.place_folder(&c, LOCAL, None, Some(&home)));
        assert_eq!(names(&lib, None), ["C", "Notes", "A", "B"]);
        assert!(lib.place_folder(&a, LOCAL, None, None));
        assert_eq!(names(&lib, None), ["C", "Notes", "B", "A"]);
        // Not to where it is already, and not the default folder.
        assert!(!lib.can_place_folder(&a, LOCAL, None, None));
        assert!(!lib.can_place_folder(&b, LOCAL, None, Some(&a)));
        assert!(!lib.can_place_folder(&b, LOCAL, None, Some(&b)));
        assert!(!lib.can_place_folder(&home, LOCAL, None, None));
        // New folders go after the ones put in order.
        lib.new_folder(LOCAL, None, Some("0"));
        assert_eq!(names(&lib, None), ["C", "Notes", "B", "A", "0"]);
        // Into another folder, at a place among its folders.
        let x = lib.new_folder(LOCAL, Some(&b), Some("X"));
        let y = lib.new_folder(LOCAL, Some(&b), Some("Y"));
        assert!(lib.place_folder(&c, LOCAL, Some(&b), Some(&y)));
        assert_eq!(names(&lib, Some(&b)), ["X", "C", "Y"]);
        assert_eq!(names(&lib, None), ["Notes", "B", "A", "0"]);
        // Not into itself.
        assert!(!lib.can_place_folder(&b, LOCAL, Some(&b), Some(&x)));
        // Moved elsewhere without a place, it goes after the ones in order.
        assert!(lib.move_folder(&c, LOCAL, None));
        assert_eq!(names(&lib, None), ["Notes", "B", "A", "0", "C"]);
    }

    #[test]
    fn a_folder_moved_to_another_account_takes_its_notes() {
        let (mut lib, _d) = lib();
        let mut acc = lib.store.accounts[0].clone();
        acc.id = "other".into();
        acc.name = "Other".into();
        lib.store.accounts.push(acc);
        lib.store.ensure_local();
        let plans = lib.new_folder(LOCAL, None, Some("Plans"));
        let inner = lib.new_folder(LOCAL, Some(&plans), Some("2026"));
        lib.store.folder_mut(&plans).unwrap().sync =
            model::FolderSync { remote_id: Some("r1".into()), synced_path: Some(vec!["Plans".into()]), revision: None };
        let kept = add(&mut lib, &inner, "# Trip\n");
        lib.store.note_mut(&kept).unwrap().sync.remote_id = Some("n1".into());
        let gone = add(&mut lib, &plans, "# Old\n");
        lib.store.note_mut(&gone).unwrap().deleted = Some(Utc::now());
        assert!(lib.move_folder(&plans, "other", None));
        for f in [&plans, &inner] {
            let f = lib.store.folder(f).unwrap();
            assert_eq!(f.account, "other");
            assert!(f.sync.remote_id.is_none(), "made new in the new account");
        }
        let n = lib.store.note(&kept).unwrap();
        assert_eq!((n.account.as_str(), n.folder.as_str()), ("other", inner.as_str()));
        assert!(n.sync.remote_id.is_none());
        let t = &lib.store.tombstones;
        assert!(t.iter().any(|t| t.account == LOCAL && !t.note && t.remote_id == "r1"), "the folder goes from the old account");
        assert!(t.iter().any(|t| t.account == LOCAL && t.note && t.remote_id == "n1"), "so does the note");
        // Recently Deleted stays behind.
        let g = lib.store.note(&gone).unwrap();
        assert_eq!((g.account.as_str(), g.folder.clone()), (LOCAL, lib.default_folder_id(LOCAL)));
    }

    #[test]
    fn rows_say_what_the_list_shows() {
        let (mut lib, _d) = lib();
        let home = lib.default_folder_id(LOCAL);
        let id = add(&mut lib, &home, "# Trip\nPack the bags\n");
        let empty = add(&mut lib, &home, "");
        let loc = Location::Folder(home);
        let r = lib.note_row(&id, &loc, false).unwrap();
        assert_eq!((r.title.as_str(), r.snippet.as_str()), ("Trip", "Pack the bags"));
        let r = lib.note_row(&empty, &loc, false).unwrap();
        assert_eq!((r.title.as_str(), r.snippet.as_str()), ("New Note", "No additional text"));
    }
}
