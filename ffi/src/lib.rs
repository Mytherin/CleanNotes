//! CleanNotes' core for Swift: the iPhone app runs on the same notes
//! library and editing engine as the desktop app, through these bindings
//! (made with UniFFI; `ios/build-rust.sh` builds the library and generates
//! the Swift).
//!
//! - [`NotesCore`]: the notes, folders, accounts and settings, and
//!   everything done to them ([`cleannotes_core::library::Library`]).
//! - [`NoteEditor`]: an open note ([`cleannotes_core::editing::Editing`]),
//!   with positions in UTF-16 code units as `UITextView` counts them.
//! - [`check_s3`]: checking an S3 bucket before adding it as a sync account
//!   (blocking: call it off the main thread).
//!
//! Only what Swift needs to see crosses over: plain records and enums, made
//! from the core's own types here.

use std::sync::{Arc, Mutex, MutexGuard, OnceLock};
use std::time::Instant;

use cleannotes_core::db::Secrets;
use cleannotes_core::doc::{self, Attr, Doc, Kind, Pos};
use cleannotes_core::editing::{Command, Editing};
use cleannotes_core::library::{self, BodySet, Library, Location};
use cleannotes_core::model::{new_id, LOCAL};
use cleannotes_core::sync::{s3, BackendConfig};
use cleannotes_core::settings::{SortBy, StartWith, TEXT_SIZES};
use cleannotes_core::{dates, lock};

uniffi::setup_scaffolding!();

// ---- Errors and callbacks -----------------------------------------------------

/// Something that couldn't be done, with a message to show.
#[derive(Debug, uniffi::Error)]
pub enum CoreError {
    Failed { message: String },
}

impl std::fmt::Display for CoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CoreError::Failed { message } => f.write_str(message),
        }
    }
}

impl std::error::Error for CoreError {}

fn failed(message: impl Into<String>) -> CoreError {
    CoreError::Failed { message: message.into() }
}

/// The app, told when a sync has something to report. Called on the sync
/// thread: the app should then call [`NotesCore::tick`] on the main thread.
#[uniffi::export(with_foreign)]
pub trait CoreListener: Send + Sync {
    fn sync_replied(&self);
}

// ---- Where the notes list is --------------------------------------------------

/// What's chosen among the folders (the notes list's place).
#[derive(Clone, Debug, PartialEq, Eq, Hash, uniffi::Enum)]
pub enum Place {
    AllNotes,
    Folder { id: String },
    /// An account's Recently Deleted.
    Deleted { account: String },
    Tag { name: String },
}

impl From<&Place> for Location {
    fn from(p: &Place) -> Location {
        match p {
            Place::AllNotes => Location::All,
            Place::Folder { id } => Location::Folder(id.clone()),
            Place::Deleted { account } => Location::Deleted(account.clone()),
            Place::Tag { name } => Location::Tag(name.clone()),
        }
    }
}

impl From<Location> for Place {
    fn from(l: Location) -> Place {
        match l {
            Location::All => Place::AllNotes,
            Location::Folder(id) => Place::Folder { id },
            Location::Deleted(account) => Place::Deleted { account },
            Location::Tag(name) => Place::Tag { name },
        }
    }
}

// ---- Records for the screens --------------------------------------------------

/// A note in the notes list.
#[derive(Clone, Debug, PartialEq, uniffi::Record)]
pub struct NoteItem {
    pub id: String,
    pub title: String,
    pub snippet: String,
    pub time: String,
    /// The note's folder, when the list mixes folders.
    pub folder: Option<String>,
    pub pinned: bool,
    pub locked: bool,
    pub deleted: bool,
}

/// A section of the notes list ("Pinned", "Today", …; the title is empty
/// for an untitled section, such as search results).
#[derive(Clone, Debug, PartialEq, uniffi::Record)]
pub struct NoteSection {
    pub title: String,
    pub notes: Vec<NoteItem>,
}

#[derive(Clone, Debug, PartialEq, uniffi::Record)]
pub struct FolderItem {
    pub id: String,
    pub name: String,
    /// How deep it's nested (0 at the top of its account).
    pub depth: u32,
    pub parent: Option<String>,
    pub count: u32,
    /// The account's "Notes" folder: can't be renamed or deleted.
    pub is_default: bool,
    pub has_children: bool,
}

#[derive(Clone, Debug, PartialEq, uniffi::Record)]
pub struct AccountItem {
    pub id: String,
    pub name: String,
    /// "Local", "SQLite", "S3".
    pub kind: String,
    pub synced: bool,
    pub syncing: bool,
    /// Why the last sync failed, if it did.
    pub problem: Option<String>,
    pub needs_sign_in: bool,
    /// Its folders, depth first.
    pub folders: Vec<FolderItem>,
    pub deleted_count: u32,
    /// An S3 account's bucket and folder.
    pub s3: Option<S3Settings>,
}

/// Where an S3 account keeps its notes (no keys: those are in the
/// keychain).
#[derive(Clone, Debug, PartialEq, uniffi::Record)]
pub struct S3Settings {
    pub bucket: String,
    pub region: String,
    /// The folder in the bucket; empty for its top.
    pub folder: String,
    /// An S3-compatible service's address; `None` for Amazon S3.
    pub endpoint: Option<String>,
    /// The folder's notes are encrypted with a key of the user's own.
    pub encrypted: bool,
}

impl S3Settings {
    fn of(c: &s3::Config) -> S3Settings {
        S3Settings { bucket: c.bucket.clone(), region: c.region.clone(), folder: c.prefix.clone(), endpoint: c.endpoint.clone(), encrypted: c.encrypted }
    }

    fn config(&self) -> s3::Config {
        let endpoint = self.endpoint.as_deref().map(|e| e.trim().trim_end_matches('/')).filter(|e| !e.is_empty());
        s3::Config {
            bucket: self.bucket.trim().to_owned(),
            region: if self.region.trim().is_empty() { s3::DEFAULT_REGION.to_owned() } else { self.region.trim().to_owned() },
            prefix: self.folder.trim().trim_matches('/').to_owned(),
            endpoint: endpoint.map(str::to_owned),
            encrypted: self.encrypted,
        }
    }
}

/// A bucket that passed [`check_s3`]: the settings to add the account with,
/// and the secret for the keychain.
#[derive(Clone, Debug, PartialEq, uniffi::Record)]
pub struct S3Checked {
    pub settings: S3Settings,
    pub secret: String,
}

/// Everything the folders screen shows.
#[derive(Clone, Debug, PartialEq, uniffi::Record)]
pub struct Overview {
    /// "All Notes" is shown (there's more than one folder).
    pub show_all_notes: bool,
    pub all_count: u32,
    pub accounts: Vec<AccountItem>,
    pub tags: Vec<String>,
    pub show_note_count: bool,
}

#[derive(Clone, Debug, PartialEq, uniffi::Record)]
pub struct NewNote {
    pub id: String,
    /// Where the list should be to show it.
    pub place: Place,
}

#[derive(Clone, Debug, PartialEq, uniffi::Record)]
pub struct RestoredNotes {
    pub ids: Vec<String>,
    pub place: Place,
}

#[derive(Clone, Debug, PartialEq, uniffi::Record)]
pub struct Ticked {
    /// Notes or folders changed (a sync): screens should refresh, and an
    /// open note reload ([`NotesCore::reload_editor`]).
    pub changed: bool,
    /// Unlocked notes locked again after a while without use.
    pub auto_locked: bool,
    /// Messages to show.
    pub notices: Vec<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum SortOrder {
    DateEdited,
    DateCreated,
    Title,
}

/// A paragraph style (Apple Notes' Aa menu).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, uniffi::Enum)]
pub enum ParagraphStyle {
    Title,
    Heading,
    Subheading,
    Body,
    Monospaced,
    Bulleted,
    Dashed,
    Numbered,
    Checklist,
    Quote,
}

impl ParagraphStyle {
    fn of(k: Kind) -> ParagraphStyle {
        match k {
            Kind::Title => ParagraphStyle::Title,
            Kind::Heading => ParagraphStyle::Heading,
            Kind::Subheading => ParagraphStyle::Subheading,
            Kind::Body => ParagraphStyle::Body,
            Kind::Monospaced => ParagraphStyle::Monospaced,
            Kind::Bulleted => ParagraphStyle::Bulleted,
            Kind::Dashed => ParagraphStyle::Dashed,
            Kind::Numbered => ParagraphStyle::Numbered,
            Kind::Checklist(_) => ParagraphStyle::Checklist,
            Kind::Quote => ParagraphStyle::Quote,
            // Tables aren't edited on iPhone yet: a cell shows as text.
            Kind::Cell { .. } => ParagraphStyle::Body,
        }
    }

    fn kind(self) -> Kind {
        match self {
            ParagraphStyle::Title => Kind::Title,
            ParagraphStyle::Heading => Kind::Heading,
            ParagraphStyle::Subheading => Kind::Subheading,
            ParagraphStyle::Body => Kind::Body,
            ParagraphStyle::Monospaced => Kind::Monospaced,
            ParagraphStyle::Bulleted => Kind::Bulleted,
            ParagraphStyle::Dashed => Kind::Dashed,
            ParagraphStyle::Numbered => Kind::Numbered,
            ParagraphStyle::Checklist => Kind::Checklist(false),
            ParagraphStyle::Quote => Kind::Quote,
        }
    }
}

/// The settings the iPhone's Settings screen shows.
#[derive(Clone, Debug, PartialEq, uniffi::Record)]
pub struct AppSettings {
    pub default_account: String,
    pub sort_by: SortOrder,
    /// Title, Heading, Subheading or Body.
    pub new_notes_start_with: ParagraphStyle,
    /// Checked items move to the bottom of their checklist.
    pub sort_checked: bool,
    pub text_size: f32,
    pub group_by_date: bool,
    pub show_note_count: bool,
    /// 0: only when asked.
    pub refresh_minutes: u32,
    /// Notes show as a gallery of cards.
    pub gallery: bool,
    /// A password for locked notes has been chosen.
    pub has_lock_password: bool,
    pub lock_hint: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum LockOutcome {
    Locked,
    /// No password chosen yet: ask for one, [`NotesCore::set_password`],
    /// then lock again.
    ChoosePassword,
    /// Ask for the password, [`NotesCore::enter_password`], then lock again.
    EnterPassword,
}

// ---- The notes library ----------------------------------------------------------

#[derive(uniffi::Object)]
pub struct NotesCore {
    lib: Mutex<Library>,
}

impl NotesCore {
    fn lib(&self) -> MutexGuard<'_, Library> {
        // A panic while holding the lock doesn't lose the notes: carry on.
        self.lib.lock().unwrap_or_else(|e| e.into_inner())
    }
}

#[uniffi::export]
impl NotesCore {
    /// The notes in `dir` (the app's Application Support folder). Sign-ins
    /// go to the keychain. `seed` adds example notes to an empty library.
    #[uniffi::constructor]
    pub fn new(dir: String, listener: Arc<dyn CoreListener>, seed: bool) -> Result<Arc<NotesCore>, CoreError> {
        let mut lib = Library::open(std::path::Path::new(&dir), Secrets::Keyring, move || listener.sync_replied())
            .map_err(|e| failed(format!("{e:#}")))?;
        if seed && lib.store.notes.is_empty() {
            lib.seed();
        }
        Ok(Arc::new(NotesCore { lib: Mutex::new(lib) }))
    }

    /// Sync replies, syncs that are due, the purge of Recently Deleted,
    /// saving, auto-lock. Call it on a timer, after
    /// [`CoreListener::sync_replied`], and when the app comes back. Save the
    /// open editor first ([`NotesCore::save_editor`]).
    pub fn tick(&self) -> Ticked {
        let mut lib = self.lib();
        let t = lib.tick();
        Ticked { changed: t.changed, auto_locked: t.auto_locked, notices: lib.take_notices() }
    }

    /// Saves everything now (going to the background, quitting).
    pub fn save_now(&self) {
        self.lib().save(true);
    }

    /// Messages waiting to be shown.
    pub fn take_notices(&self) -> Vec<String> {
        self.lib().take_notices()
    }

    // ---- Folders screen ----

    pub fn overview(&self) -> Overview {
        let lib = self.lib();
        let accounts = lib
            .store
            .accounts
            .iter()
            .filter(|a| a.enabled)
            .map(|a| {
                let folders = lib
                    .store
                    .folder_tree(&a.id)
                    .into_iter()
                    .map(|(f, depth)| FolderItem {
                        id: f.id.clone(),
                        name: f.name.clone(),
                        depth: depth as u32,
                        parent: f.parent.clone(),
                        count: lib.folder_count(&f.id) as u32,
                        is_default: f.default,
                        has_children: lib.store.folders.iter().any(|c| c.parent.as_deref() == Some(f.id.as_str())),
                    })
                    .collect();
                AccountItem {
                    id: a.id.clone(),
                    name: a.name.clone(),
                    kind: a.backend.kind_name().to_owned(),
                    synced: a.synced(),
                    syncing: lib.syncing(&a.id),
                    problem: lib.sync_errors.get(&a.id).cloned(),
                    needs_sign_in: lib.needs_sign_in.contains(&a.id),
                    folders,
                    deleted_count: lib.deleted_count(&a.id) as u32,
                    s3: match &a.backend {
                        BackendConfig::S3(c) => Some(S3Settings::of(c)),
                        _ => None,
                    },
                }
            })
            .collect();
        Overview {
            show_all_notes: lib.show_all_notes(),
            all_count: lib.all_count() as u32,
            accounts,
            tags: lib.all_tags(),
            show_note_count: lib.settings.show_note_count,
        }
    }

    /// Where the app was when last used, and the note open then.
    pub fn initial_place(&self) -> Place {
        self.lib().initial_location().0.into()
    }

    /// Remembers where the list is, for next time.
    pub fn remember_place(&self, place: Place) {
        self.lib().remember_location(&(&place).into());
    }

    pub fn place_exists(&self, place: Place) -> bool {
        self.lib().location_exists(&(&place).into())
    }

    pub fn new_folder(&self, account: String, parent: Option<String>, name: String) -> String {
        self.lib().new_folder(&account, parent.as_deref(), Some(&name))
    }

    pub fn rename_folder(&self, id: String, name: String) -> Result<(), CoreError> {
        self.lib().rename_folder(&id, &name).map_err(failed)
    }

    /// Deletes a folder and its subfolders; their notes go to Recently
    /// Deleted.
    pub fn delete_folder(&self, id: String) {
        self.lib().delete_folder(&id);
    }

    pub fn empty_deleted(&self, account: String) {
        self.lib().empty_deleted(&account);
    }

    // ---- Notes list ----

    /// The list's title: the folder's name, "All Notes", "Search", …
    pub fn title(&self, place: Place, search: String) -> String {
        self.lib().location_title(&(&place).into(), &search)
    }

    /// The notes list, in sections. A search looks through every note.
    pub fn sections(&self, place: Place, search: String) -> Vec<NoteSection> {
        let lib = self.lib();
        let loc: Location = (&place).into();
        let show_folder = lib.list_shows_folders(&loc, &search);
        lib.list_sections(&loc, &search)
            .into_iter()
            .map(|(title, ids)| NoteSection {
                title,
                notes: ids.iter().filter_map(|id| lib.note_row(id, &loc, show_folder)).map(item).collect(),
            })
            .collect()
    }

    pub fn note(&self, id: String, place: Place) -> Option<NoteItem> {
        self.lib().note_row(&id, &(&place).into(), false).map(item)
    }

    /// "15 November 2026 at 11:32", for the top of the note.
    pub fn note_date(&self, id: String) -> String {
        self.lib().store.note(&id).map(|n| dates::note_header_time(n.modified)).unwrap_or_default()
    }

    /// A new note from `place`, with its editor (the first paragraph in the
    /// "New notes start with" style).
    pub fn new_note(&self, place: Place) -> NewNote {
        let (id, _, shown) = self.lib().new_note(&(&place).into());
        NewNote { id, place: shown.into() }
    }

    /// Leaving a note: a new note left empty goes away (returns true).
    pub fn leave_note(&self, id: String) -> bool {
        self.lib().leave_note(&id)
    }

    pub fn toggle_pin(&self, id: String) {
        self.lib().toggle_pin(&id);
    }

    pub fn duplicate_note(&self, id: String) -> Option<String> {
        self.lib().duplicate_note(&id)
    }

    /// Whether deleting these deletes them for good (they're all in Recently
    /// Deleted already): ask first.
    pub fn deletes_forever(&self, ids: Vec<String>) -> bool {
        self.lib().all_in_deleted(&ids)
    }

    /// Moves notes to Recently Deleted (the list was at `place`).
    pub fn trash_notes(&self, ids: Vec<String>, place: Place) {
        self.lib().trash_notes(&ids, &(&place).into());
    }

    pub fn delete_forever(&self, ids: Vec<String>) {
        self.lib().delete_forever(&ids);
    }

    pub fn can_undo_delete(&self) -> bool {
        self.lib().can_undo_delete()
    }

    /// Takes back the last deletion (shake to undo).
    pub fn undo_delete(&self) -> Option<RestoredNotes> {
        self.lib().undo_delete().map(|r| RestoredNotes { ids: r.ids, place: r.location.into() })
    }

    pub fn redo_delete(&self) -> Option<Vec<String>> {
        self.lib().redo_delete()
    }

    pub fn recover_note(&self, id: String) {
        self.lib().recover_note(&id);
    }

    pub fn move_notes(&self, ids: Vec<String>, folder: String) {
        self.lib().move_notes(&ids, &folder);
    }

    /// A note's Markdown (a locked note's once unlocked).
    pub fn markdown(&self, id: String) -> Option<String> {
        self.lib().plain_body(&id)
    }

    // ---- The open note ----

    /// The editor for a note: none if it doesn't exist or is locked and
    /// closed.
    pub fn open_editor(&self, id: String) -> Option<Arc<NoteEditor>> {
        let md = self.lib().plain_body(&id)?;
        Some(NoteEditor::wrap(Editing::new(&id, &md)))
    }

    /// The editor for a note just made with [`NotesCore::new_note`].
    pub fn new_note_editor(&self, id: String) -> Arc<NoteEditor> {
        let lib = self.lib();
        let md = lib.store.note(&id).map(|n| n.body.clone()).unwrap_or_default();
        NoteEditor::wrap(Editing::new_note(&id, &md, lib.settings.start_with.kind()))
    }

    /// Copies an editor's changes into its note. Returns false if the note
    /// had to close (a locked note locked elsewhere with another password).
    pub fn save_editor(&self, editor: Arc<NoteEditor>) -> bool {
        let mut e = editor.ed();
        let Some(md) = e.take_markdown() else { return true };
        let id = e.note_id.clone();
        drop(e);
        self.lib().set_body(&id, &md) != BodySet::Closed
    }

    /// After a sync: the editor follows its note. Returns false if the note
    /// is gone or closed (locked), so the editor should close.
    pub fn reload_editor(&self, editor: Arc<NoteEditor>) -> bool {
        let id = editor.ed().note_id.clone();
        match self.lib().plain_body_now(&id) {
            Some(md) => {
                editor.ed().reload(&md);
                true
            }
            None => false,
        }
    }

    /// Recently Deleted notes can be read, not edited.
    pub fn is_deleted(&self, id: String) -> bool {
        self.lib().store.note(&id).is_some_and(|n| n.deleted.is_some())
    }

    pub fn exists(&self, id: String) -> bool {
        self.lib().store.note(&id).is_some()
    }

    // ---- Locked notes ----

    pub fn is_locked(&self, id: String) -> bool {
        self.lib().is_locked(&id)
    }

    /// Locked and not unlocked: show the lock screen.
    pub fn is_closed(&self, id: String) -> bool {
        self.lib().is_closed(&id)
    }

    pub fn has_unlocked(&self) -> bool {
        self.lib().has_unlocked()
    }

    /// The lock screen's "View Note". Errors with a message (and the hint).
    pub fn unlock(&self, id: String, password: String) -> Result<(), CoreError> {
        self.lib().unlock(&id, &password).map_err(failed)
    }

    /// "Lock Note" (save the open editor first).
    pub fn lock_note(&self, id: String) -> LockOutcome {
        match self.lib().lock_note(&id) {
            library::LockNote::Locked => LockOutcome::Locked,
            library::LockNote::ChoosePassword => LockOutcome::ChoosePassword,
            library::LockNote::EnterPassword => LockOutcome::EnterPassword,
        }
    }

    pub fn remove_lock(&self, id: String) {
        self.lib().remove_lock(&id);
    }

    /// Closes every unlocked note (save the open editor first).
    pub fn lock_now(&self) {
        self.lib().lock_now();
    }

    pub fn set_password(&self, password: String, hint: String) -> Result<(), CoreError> {
        self.lib().set_password(&password, &hint).map_err(failed)
    }

    pub fn enter_password(&self, password: String) -> Result<(), CoreError> {
        self.lib().enter_password(&password).map_err(failed)
    }

    /// Returns how many notes now open with the new password.
    pub fn change_password(&self, old: String, password: String, hint: String) -> Result<u32, CoreError> {
        self.lib().change_password(&old, &password, &hint).map(|n| n as u32).map_err(failed)
    }

    /// The user did something: unlocked notes stay open another while.
    pub fn touch_lock(&self) {
        self.lib().touch_lock();
    }

    // ---- Settings ----

    pub fn settings(&self) -> AppSettings {
        let lib = self.lib();
        let s = &lib.settings;
        AppSettings {
            default_account: s.default_account.clone(),
            sort_by: match s.sort_by {
                SortBy::Edited => SortOrder::DateEdited,
                SortBy::Created => SortOrder::DateCreated,
                SortBy::Title => SortOrder::Title,
            },
            new_notes_start_with: ParagraphStyle::of(s.start_with.kind()),
            sort_checked: s.sort_checked,
            text_size: s.text_size,
            group_by_date: s.group_by_date,
            show_note_count: s.show_note_count,
            refresh_minutes: s.refresh_minutes,
            gallery: s.view == cleannotes_core::settings::ListView::Gallery,
            has_lock_password: lib.has_lock_password(),
            lock_hint: s.lock_hint.clone(),
        }
    }

    pub fn set_settings(&self, new: AppSettings) {
        let mut lib = self.lib();
        let s = &mut lib.settings;
        if s.default_account != new.default_account && !new.default_account.is_empty() {
            s.default_account = new.default_account;
        }
        s.sort_by = match new.sort_by {
            SortOrder::DateEdited => SortBy::Edited,
            SortOrder::DateCreated => SortBy::Created,
            SortOrder::Title => SortBy::Title,
        };
        s.start_with = match new.new_notes_start_with {
            ParagraphStyle::Title => StartWith::Title,
            ParagraphStyle::Heading => StartWith::Heading,
            ParagraphStyle::Subheading => StartWith::Subheading,
            _ => StartWith::Body,
        };
        s.sort_checked = new.sort_checked;
        s.text_size = new.text_size.clamp(TEXT_SIZES[0], TEXT_SIZES[TEXT_SIZES.len() - 1]);
        s.group_by_date = new.group_by_date;
        s.show_note_count = new.show_note_count;
        s.refresh_minutes = new.refresh_minutes;
        s.view = if new.gallery { cleannotes_core::settings::ListView::Gallery } else { cleannotes_core::settings::ListView::List };
        lib.save(true);
    }

    // ---- Accounts ----

    /// Syncs every account now (pull to refresh).
    pub fn sync_all(&self) {
        self.lib().sync_all();
    }

    pub fn sync_now(&self, account: String) {
        self.lib().sync_now(&account);
    }

    pub fn any_syncing(&self) -> bool {
        self.lib().any_syncing()
    }

    pub fn remove_account(&self, id: String) {
        self.lib().remove_account(&id);
    }

    /// Adds an S3 account after [`check_s3`], or with `again` (an
    /// account's id) gives it the new keys, keeping its notes and sync
    /// state. Returns its id.
    pub fn add_s3_account(&self, again: Option<String>, name: String, checked: S3Checked) -> String {
        let mut lib = self.lib();
        let id = again.unwrap_or_else(new_id);
        let name = match (name.trim(), lib.store.account(&id)) {
            ("", Some(a)) => a.name.clone(),
            ("", None) => "S3".to_owned(),
            (n, _) => n.to_owned(),
        };
        let account = lib.account_for(&id, name, BackendConfig::S3(checked.settings.config()));
        lib.add_account(account, Some(checked.secret));
        id
    }
}

/// Checks an S3 bucket and access key (blocking: call it off the main
/// thread): it must be writable and honour conditional writes. With a
/// passphrase, the folder's notes are encrypted (set up now, or unlocked if
/// they are already); a folder that is encrypted needs it.
#[uniffi::export]
pub fn check_s3(settings: S3Settings, access_key_id: String, secret_access_key: String, passphrase: Option<String>) -> Result<S3Checked, CoreError> {
    let keys = s3::Keys { access_key_id: access_key_id.trim().to_owned(), secret_access_key: secret_access_key.trim().to_owned(), encryption_key: None };
    let (config, secret) = s3::check(&settings.config(), &keys, passphrase.as_deref()).map_err(|e| failed(format!("{e:#}")))?;
    Ok(S3Checked { settings: S3Settings::of(&config), secret })
}

/// The region new S3 accounts start with.
#[uniffi::export]
pub fn default_s3_region() -> String {
    s3::DEFAULT_REGION.to_owned()
}

/// The folder in the bucket new S3 accounts start with.
#[uniffi::export]
pub fn default_s3_folder() -> String {
    s3::DEFAULT_PREFIX.to_owned()
}

fn item(r: library::NoteRow) -> NoteItem {
    NoteItem {
        id: r.id,
        title: r.title,
        snippet: r.snippet,
        time: r.time,
        folder: r.folder,
        pinned: r.pinned,
        locked: r.locked,
        deleted: r.deleted,
    }
}

/// The local account's id (notes on this iPhone only).
#[uniffi::export]
pub fn local_account_id() -> String {
    LOCAL.to_owned()
}

/// The text sizes Settings offers.
#[uniffi::export]
pub fn text_sizes() -> Vec<f32> {
    TEXT_SIZES.to_vec()
}

/// The font size of a paragraph style for a body text size.
#[uniffi::export]
pub fn font_size(style: ParagraphStyle, body: f32) -> f32 {
    doc::font_size(style.kind(), body)
}

/// Whether Markdown is a locked note's (for previews).
#[uniffi::export]
pub fn is_locked_markdown(md: String) -> bool {
    lock::is_locked(&md)
}

// ---- The note editor ------------------------------------------------------------

/// A run of text in one style.
#[derive(Clone, Debug, PartialEq, uniffi::Record)]
pub struct TextRun {
    pub text: String,
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    pub strike: bool,
    /// Inline code (from Markdown written elsewhere).
    pub code: bool,
    /// A link (set, or a plain URL in the text).
    pub link: Option<String>,
    /// A #tag.
    pub tag: bool,
}

/// A paragraph to lay out.
#[derive(Clone, Debug, PartialEq, uniffi::Record)]
pub struct Paragraph {
    pub style: ParagraphStyle,
    /// Indent level (lists and quotes).
    pub indent: u32,
    /// A ticked checklist item.
    pub checked: bool,
    /// A numbered item's number.
    pub number: u32,
    pub runs: Vec<TextRun>,
}

/// The whole note to show: its paragraphs (joined by line breaks, they're
/// the text view's text) and the selection.
#[derive(Clone, Debug, PartialEq, uniffi::Record)]
pub struct EditorState {
    pub paragraphs: Vec<Paragraph>,
    /// The selection, in UTF-16 offsets into the text.
    pub selection_start: u32,
    pub selection_end: u32,
}

/// What the Aa sheet and the keyboard bar show as on.
#[derive(Clone, Debug, PartialEq, uniffi::Record)]
pub struct Formatting {
    pub style: ParagraphStyle,
    pub checked: bool,
    pub indent: u32,
    pub number: u32,
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    pub strike: bool,
    pub code: bool,
    pub link: Option<String>,
    pub can_undo: bool,
    pub can_redo: bool,
}

#[derive(Clone, Debug, PartialEq, uniffi::Enum)]
pub enum EditorCommand {
    SetStyle { style: ParagraphStyle },
    Bold,
    Italic,
    Underline,
    Strikethrough,
    /// Make the paragraphs a checklist, or tick/untick them.
    ToggleChecklist,
    Indent,
    Outdent,
    MoveUp,
    MoveDown,
    /// Link the selection (or insert the address); `None` removes links.
    SetLink { url: Option<String> },
    Undo,
    Redo,
    SelectAll,
}

#[derive(uniffi::Object)]
pub struct NoteEditor {
    ed: Mutex<Editing>,
}

/// A clock for grouping typing into undo steps.
fn clock() -> f64 {
    static START: OnceLock<Instant> = OnceLock::new();
    START.get_or_init(Instant::now).elapsed().as_secs_f64()
}

impl NoteEditor {
    fn wrap(ed: Editing) -> Arc<NoteEditor> {
        Arc::new(NoteEditor { ed: Mutex::new(ed) })
    }

    fn ed(&self) -> MutexGuard<'_, Editing> {
        let mut e = self.ed.lock().unwrap_or_else(|e| e.into_inner());
        e.clock = clock();
        e
    }
}

#[uniffi::export]
impl NoteEditor {
    pub fn note_id(&self) -> String {
        self.ed().note_id.clone()
    }

    /// Everything to show.
    pub fn state(&self) -> EditorState {
        let e = self.ed();
        let numbers = e.doc.list_numbers();
        let paragraphs = e.doc.blocks.iter().enumerate().map(|(i, b)| paragraph(b, numbers[i])).collect();
        let (a, b) = (to_utf16(&e.doc, e.sel.start()), to_utf16(&e.doc, e.sel.end()));
        EditorState { paragraphs, selection_start: a, selection_end: b }
    }

    /// One paragraph (to restyle it after typing in it).
    pub fn paragraph(&self, index: u32) -> Option<Paragraph> {
        let e = self.ed();
        let i = index as usize;
        let b = e.doc.blocks.get(i)?;
        Some(paragraph(b, e.doc.list_numbers()[i]))
    }

    /// The paragraph the UTF-16 offset is in.
    pub fn paragraph_at(&self, offset: u32) -> u32 {
        let e = self.ed();
        from_utf16(&e.doc, offset).block as u32
    }

    /// Where a paragraph starts (UTF-16 offset).
    pub fn paragraph_start(&self, index: u32) -> u32 {
        let e = self.ed();
        to_utf16(&e.doc, Pos::new((index as usize).min(e.doc.blocks.len() - 1), 0))
    }

    pub fn paragraph_count(&self) -> u32 {
        self.ed().doc.blocks.len() as u32
    }

    /// The text view replaced `start..end` (UTF-16) with `text`. Returns
    /// whether the text view may make the change itself (plain typing): if
    /// not (Return in a list, "- " starting one, Backspace taking a list
    /// style off, …), it should refuse it and show [`NoteEditor::state`].
    pub fn replace(&self, start: u32, end: u32, text: String) -> bool {
        let mut e = self.ed();
        let before = e.doc.plain_text();
        let shape = shape(&e.doc);
        let (s, t) = (from_utf16(&e.doc, start), from_utf16(&e.doc, end));
        e.replace_range(s, t, &text);
        let expected = splice_utf16(&before, start as usize, end as usize, &text);
        !text.contains('\n') && !text.contains('\r') && e.doc.plain_text() == expected && shape == crate::shape(&e.doc)
    }

    /// The text view's text, after it changed without saying how (dictation,
    /// a keyboard's own edits): the note is brought in line, without list
    /// handling. Returns whether anything differed.
    pub fn sync_text(&self, text: String) -> bool {
        let mut e = self.ed();
        let mine = e.doc.plain_text();
        if mine == text {
            return false;
        }
        let (a, b): (Vec<char>, Vec<char>) = (mine.chars().collect(), text.chars().collect());
        let prefix = a.iter().zip(&b).take_while(|(x, y)| x == y).count();
        let max_suffix = a.len().min(b.len()) - prefix;
        let suffix = a.iter().rev().zip(b.iter().rev()).take(max_suffix).take_while(|(x, y)| x == y).count();
        let start = char_pos(&e.doc, prefix);
        let end = char_pos(&e.doc, a.len() - suffix);
        let new: String = b[prefix..b.len() - suffix].iter().collect();
        e.replace_raw(start, end, &new);
        true
    }

    /// The text view's selection changed (UTF-16 offsets).
    pub fn select(&self, start: u32, end: u32) {
        let mut e = self.ed();
        let (a, b) = (from_utf16(&e.doc, start), from_utf16(&e.doc, end));
        e.set_selection(a, b);
    }

    pub fn command(&self, command: EditorCommand) {
        let c = match command {
            EditorCommand::SetStyle { style } => Command::SetKind(style.kind()),
            EditorCommand::Bold => Command::ToggleAttr(Attr::Bold),
            EditorCommand::Italic => Command::ToggleAttr(Attr::Italic),
            EditorCommand::Underline => Command::ToggleAttr(Attr::Underline),
            EditorCommand::Strikethrough => Command::ToggleAttr(Attr::Strike),
            EditorCommand::ToggleChecklist => Command::ToggleChecked,
            EditorCommand::Indent => Command::Indent,
            EditorCommand::Outdent => Command::Outdent,
            EditorCommand::MoveUp => Command::MoveUp,
            EditorCommand::MoveDown => Command::MoveDown,
            EditorCommand::SetLink { url } => Command::SetLink(url),
            EditorCommand::Undo => Command::Undo,
            EditorCommand::Redo => Command::Redo,
            EditorCommand::SelectAll => Command::SelectAll,
        };
        self.ed().command(c);
    }

    /// Tapping a checklist item's circle.
    pub fn toggle_check(&self, paragraph: u32, sort_checked: bool) {
        self.ed().toggle_check_at(paragraph as usize, sort_checked);
    }

    /// The style at the caret (or of the selection), and undo's state.
    pub fn formatting(&self) -> Formatting {
        let e = self.ed();
        let b = &e.doc.blocks[e.sel.caret.block.min(e.doc.blocks.len() - 1)];
        let typing = e.typing_style();
        let on = |a: Attr| e.has_attr(a);
        Formatting {
            style: ParagraphStyle::of(b.kind),
            checked: b.kind == Kind::Checklist(true),
            indent: b.indent as u32,
            number: e.doc.list_numbers()[e.sel.caret.block.min(e.doc.blocks.len() - 1)],
            bold: on(Attr::Bold),
            italic: on(Attr::Italic),
            underline: on(Attr::Underline),
            strike: on(Attr::Strike),
            code: typing.code,
            link: typing.link.as_deref().map(str::to_owned),
            can_undo: e.can_undo(),
            can_redo: e.can_redo(),
        }
    }

    /// The link where the selection starts (for the link sheet).
    pub fn link_at_selection(&self) -> String {
        self.ed().link_at_selection()
    }

    /// Copy: the selection's text for the pasteboard (pasting it back here
    /// keeps its formatting).
    pub fn copy(&self) -> Option<String> {
        self.ed().copy()
    }

    pub fn cut(&self) -> Option<String> {
        self.ed().cut()
    }

    pub fn paste(&self, text: String) {
        self.ed().paste(&text);
    }

    /// The selection (or the whole note) as Markdown.
    pub fn markdown(&self) -> String {
        self.ed().selection_markdown()
    }

    /// The text without formatting.
    pub fn plain_text(&self) -> String {
        self.ed().doc.plain_text()
    }
}

/// Paragraph kinds and indents, to tell plain typing from a change of
/// structure.
fn shape(d: &Doc) -> Vec<(Kind, u8)> {
    d.blocks.iter().map(|b| (b.kind, b.indent)).collect()
}

fn paragraph(b: &doc::Block, number: u32) -> Paragraph {
    let mono = b.kind == Kind::Monospaced;
    let chars: Vec<char> = b.text.chars().collect();
    // Plain URLs and #tags are shown as links and tags (not in code).
    let mut auto: Vec<Option<String>> = vec![None; chars.len()];
    let mut tag = vec![false; chars.len()];
    if !mono {
        for (r, url) in doc::auto_links(&b.text) {
            for slot in &mut auto[r] {
                *slot = Some(url.clone());
            }
        }
        for r in doc::tag_ranges(&b.text) {
            for t in &mut tag[r] {
                *t = true;
            }
        }
    }
    let mut runs: Vec<TextRun> = Vec::new();
    for (i, c) in chars.iter().enumerate() {
        let s = b.styles.get(i).cloned().unwrap_or_default();
        let link = s.link.as_deref().map(str::to_owned).or_else(|| auto[i].clone());
        let run = TextRun {
            text: String::new(),
            bold: s.bold,
            italic: s.italic,
            underline: s.underline,
            strike: s.strike,
            code: s.code,
            link,
            tag: tag[i],
        };
        match runs.last_mut() {
            Some(last) if TextRun { text: String::new(), ..last.clone() } == run => last.text.push(*c),
            _ => runs.push(TextRun { text: c.to_string(), ..run }),
        }
    }
    Paragraph {
        style: ParagraphStyle::of(b.kind),
        indent: b.indent as u32,
        checked: b.kind == Kind::Checklist(true),
        number,
        runs,
    }
}

/// A position as a UTF-16 offset into the paragraphs joined by "\n".
fn to_utf16(d: &Doc, p: Pos) -> u32 {
    let p = d.clamp(p);
    let before: usize = d.blocks[..p.block].iter().map(|b| b.text.encode_utf16().count() + 1).sum();
    let within: usize = d.blocks[p.block].text.chars().take(p.offset).map(char::len_utf16).sum();
    (before + within) as u32
}

/// A UTF-16 offset as a position (inside a surrogate pair: before it).
fn from_utf16(d: &Doc, offset: u32) -> Pos {
    let mut rest = offset as usize;
    let last = d.blocks.len() - 1;
    for (i, b) in d.blocks.iter().enumerate() {
        let len = b.text.encode_utf16().count();
        if rest <= len || i == last {
            let (mut units, mut chars) = (0, 0);
            for c in b.text.chars() {
                if units + c.len_utf16() > rest {
                    break;
                }
                units += c.len_utf16();
                chars += 1;
            }
            return Pos::new(i, chars);
        }
        rest -= len + 1;
    }
    d.end()
}

/// A char offset into the paragraphs joined by "\n" as a position.
fn char_pos(d: &Doc, mut offset: usize) -> Pos {
    for (i, b) in d.blocks.iter().enumerate() {
        let len = b.len();
        if offset <= len {
            return Pos::new(i, offset);
        }
        offset -= len + 1;
    }
    d.end()
}

fn splice_utf16(s: &str, start: usize, end: usize, text: &str) -> String {
    let units: Vec<u16> = s.encode_utf16().collect();
    let end = end.min(units.len());
    let start = start.min(end);
    let mut out: Vec<u16> = units[..start].to_vec();
    out.extend(text.encode_utf16());
    out.extend_from_slice(&units[end..]);
    String::from_utf16_lossy(&out)
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Quiet;
    impl CoreListener for Quiet {
        fn sync_replied(&self) {}
    }

    fn core() -> (Arc<NotesCore>, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let lib = Library::open(dir.path(), Secrets::Memory, || {}).unwrap();
        let _ = Quiet.sync_replied();
        (Arc::new(NotesCore { lib: Mutex::new(lib) }), dir)
    }

    #[test]
    fn s3_accounts_are_added_and_get_new_keys() {
        let (core, _dir) = core();
        let settings = |folder: &str| S3Settings {
            bucket: " notes ".into(),
            region: "".into(),
            folder: format!("/{folder}/"),
            // Nothing listens there: the first sync just fails.
            endpoint: Some("http://127.0.0.1:9/".into()),
            encrypted: true,
        };
        let id = core.add_s3_account(None, "  ".into(), S3Checked { settings: settings("Work"), secret: "k1".into() });
        let s3_of = |core: &NotesCore| core.overview().accounts.into_iter().find(|a| a.id == id).unwrap();
        let a = s3_of(&core);
        assert_eq!((a.name.as_str(), a.kind.as_str(), a.synced), ("S3", "S3", true));
        assert_eq!(
            a.s3,
            Some(S3Settings { bucket: "notes".into(), region: s3::DEFAULT_REGION.into(), folder: "Work".into(), endpoint: Some("http://127.0.0.1:9".into()), encrypted: true })
        );
        assert_eq!(core.lib().db.secret(&id).as_deref(), Some("k1"));
        assert!(core.overview().accounts.iter().find(|a| a.id == local_account_id()).unwrap().s3.is_none());

        // New keys: the same account, its name kept.
        core.lib().store.accounts.iter_mut().find(|a| a.id == id).unwrap().name = "Work notes".into();
        let again = core.add_s3_account(Some(id.clone()), "".into(), S3Checked { settings: settings("Work"), secret: "k2".into() });
        assert_eq!(again, id);
        assert_eq!(s3_of(&core).name, "Work notes");
        assert_eq!(core.lib().db.secret(&id).as_deref(), Some("k2"));
        assert_eq!(core.overview().accounts.len(), 2);
    }

    fn editor(md: &str) -> Arc<NoteEditor> {
        NoteEditor::wrap(Editing::new("n", md))
    }

    #[test]
    fn utf16_offsets_count_as_ios_does() {
        let e = editor("a😀b\nsecond\n");
        let ed = e.ed();
        assert_eq!(to_utf16(&ed.doc, Pos::new(0, 2)), 3, "the emoji is two UTF-16 units");
        assert_eq!(to_utf16(&ed.doc, Pos::new(1, 0)), 5);
        assert_eq!(from_utf16(&ed.doc, 3), Pos::new(0, 2));
        assert_eq!(from_utf16(&ed.doc, 2), Pos::new(0, 1), "inside the pair: before it");
        assert_eq!(from_utf16(&ed.doc, 5), Pos::new(1, 0));
        assert_eq!(from_utf16(&ed.doc, 99), Pos::new(1, 6));
    }

    #[test]
    fn plain_typing_is_left_to_the_text_view() {
        let e = editor("# Title\nbody\n");
        e.select(12, 12);
        assert!(e.replace(12, 12, "!".into()), "typing at the end of a line");
        assert_eq!(e.plain_text(), "Title\nbody!");
        // Return makes a paragraph: the editor shows it itself.
        assert!(!e.replace(13, 13, "\n".into()));
        // "- " starts a list: not plain typing.
        let s = e.state();
        let at = s.selection_start;
        assert!(e.replace(at, at, "-".into()));
        assert!(!e.replace(at + 1, at + 1, " ".into()));
        assert_eq!(e.state().paragraphs[2].style, ParagraphStyle::Dashed);
    }

    #[test]
    fn a_text_view_out_of_step_is_brought_in_line() {
        let e = editor("hello world\n");
        assert!(e.sync_text("hello brave world".into()));
        assert_eq!(e.plain_text(), "hello brave world");
        assert!(!e.sync_text("hello brave world".into()));
    }

    #[test]
    fn paragraphs_carry_styles_links_and_tags() {
        let e = editor("- [x] **buy** milk #shop at https://x.org\n1. one\n");
        let s = e.state();
        let p = &s.paragraphs[0];
        assert_eq!((p.style, p.checked), (ParagraphStyle::Checklist, true));
        assert!(p.runs[0].bold && p.runs[0].text == "buy");
        assert!(p.runs.iter().any(|r| r.tag && r.text == "#shop"));
        assert!(p.runs.iter().any(|r| r.link.as_deref() == Some("https://x.org")));
        assert_eq!(s.paragraphs[1].number, 1);
    }

    #[test]
    fn notes_made_edited_and_listed_through_the_bindings() {
        let (core, _dir) = core();
        let home = Place::Folder { id: core.lib().default_folder_id(LOCAL) };
        let n = core.new_note(home.clone());
        let e = core.new_note_editor(n.id.clone());
        assert_eq!(e.formatting().style, ParagraphStyle::Title, "new notes start with a title");
        for c in "Groceries".chars() {
            let at = e.state().selection_start;
            e.replace(at, at, c.to_string());
        }
        assert!(core.save_editor(e.clone()));
        let sections = core.sections(home.clone(), String::new());
        assert_eq!(sections[0].notes[0].title, "Groceries");
        core.trash_notes(vec![n.id.clone()], home.clone());
        assert!(core.sections(home.clone(), String::new()).is_empty());
        let back = core.undo_delete().unwrap();
        assert_eq!(back.place, home);
        // An empty new note left goes away.
        let m = core.new_note(home.clone());
        assert!(core.leave_note(m.id));
        let ov = core.overview();
        assert_eq!(ov.accounts[0].folders[0].count, 1);
    }

    #[test]
    fn locking_through_the_bindings() {
        let (core, _dir) = core();
        let home = Place::Folder { id: core.lib().default_folder_id(LOCAL) };
        let n = core.new_note(home);
        core.lib().set_body(&n.id, "# Secret\nPIN\n");
        assert_eq!(core.lock_note(n.id.clone()), LockOutcome::ChoosePassword);
        core.set_password("pw".into(), "hint".into()).unwrap();
        assert_eq!(core.lock_note(n.id.clone()), LockOutcome::Locked);
        core.lock_now();
        assert!(core.is_closed(n.id.clone()) && core.open_editor(n.id.clone()).is_none());
        let err = core.unlock(n.id.clone(), "nope".into()).unwrap_err();
        assert!(err.to_string().contains("hint"));
        core.unlock(n.id.clone(), "pw".into()).unwrap();
        assert!(core.open_editor(n.id).is_some());
    }
}
