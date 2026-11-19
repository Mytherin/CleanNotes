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
use cleannotes_core::settings::{self, SortBy, StartWith, TEXT_SIZES};
use cleannotes_core::{attachments, dates, lock};

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
    /// A Smart Folder.
    Smart { id: String },
    /// CleanTabs' saved tabs (a tabs account's id): a folder of them (with
    /// those in its folders), or All Tabs when `folder` is `None`.
    Tabs { account: String, folder: Option<String> },
    /// CleanTabs' Recently Deleted.
    TabsDeleted { account: String },
}

impl From<&Place> for Location {
    fn from(p: &Place) -> Location {
        match p {
            Place::AllNotes => Location::All,
            Place::Folder { id } => Location::Folder(id.clone()),
            Place::Deleted { account } => Location::Deleted(account.clone()),
            Place::Tag { name } => Location::Tag(name.clone()),
            Place::Smart { id } => Location::Smart(id.clone()),
            Place::Tabs { account, folder } => Location::Tabs(account.clone(), folder.clone()),
            Place::TabsDeleted { account } => Location::TabsDeleted(account.clone()),
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
            Location::Smart(id) => Place::Smart { id },
            Location::Tabs(account, folder) => Place::Tabs { account, folder },
            Location::TabsDeleted(account) => Place::TabsDeleted { account },
        }
    }
}

/// A Smart Folder's filter: every part that's set must pass.
#[derive(Clone, Debug, PartialEq, uniffi::Record)]
pub struct SmartFilter {
    /// #tags, without the #; none: any note.
    pub tags: Vec<String>,
    /// Every one of the tags, rather than any.
    pub all_tags: bool,
    pub checklists: ChecklistFilter,
    pub pinned: bool,
    pub locked: bool,
    /// Edited in the last this many days (0: any time).
    pub edited_within_days: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum ChecklistFilter {
    /// Checklists don't matter.
    Off,
    /// Notes with a checklist.
    HasChecklist,
    /// Notes with an unticked item.
    Unchecked,
    /// Notes whose items are all ticked.
    AllChecked,
}

impl From<settings::NoteFilter> for SmartFilter {
    fn from(f: settings::NoteFilter) -> SmartFilter {
        SmartFilter {
            tags: f.tags,
            all_tags: f.all_tags,
            checklists: match f.checklists {
                settings::ChecklistFilter::Off => ChecklistFilter::Off,
                settings::ChecklistFilter::Any => ChecklistFilter::HasChecklist,
                settings::ChecklistFilter::Unchecked => ChecklistFilter::Unchecked,
                settings::ChecklistFilter::AllChecked => ChecklistFilter::AllChecked,
            },
            pinned: f.pinned,
            locked: f.locked,
            edited_within_days: f.edited_within_days,
        }
    }
}

impl From<SmartFilter> for settings::NoteFilter {
    fn from(f: SmartFilter) -> settings::NoteFilter {
        settings::NoteFilter {
            tags: f.tags.into_iter().map(|t| t.trim().trim_start_matches('#').to_owned()).filter(|t| !t.is_empty()).collect(),
            all_tags: f.all_tags,
            checklists: match f.checklists {
                ChecklistFilter::Off => settings::ChecklistFilter::Off,
                ChecklistFilter::HasChecklist => settings::ChecklistFilter::Any,
                ChecklistFilter::Unchecked => settings::ChecklistFilter::Unchecked,
                ChecklistFilter::AllChecked => settings::ChecklistFilter::AllChecked,
            },
            pinned: f.pinned,
            locked: f.locked,
            edited_within_days: f.edited_within_days,
        }
    }
}

/// A Smart Folder on the Folders screen.
#[derive(Clone, Debug, PartialEq, uniffi::Record)]
pub struct SmartFolderItem {
    pub id: String,
    pub name: String,
    pub count: u32,
    pub filter: SmartFilter,
}

// ---- Records for the screens --------------------------------------------------

/// A note in the notes list.
#[derive(Clone, Debug, PartialEq, uniffi::Record)]
pub struct NoteItem {
    pub id: String,
    pub title: String,
    pub snippet: String,
    /// Nothing after the title (the snippet is "No additional text").
    pub empty: bool,
    pub time: String,
    /// The note's folder, when the list mixes folders.
    pub folder: Option<String>,
    pub pinned: bool,
    pub locked: bool,
    pub deleted: bool,
    /// The note's first picture (an attachment's name, for
    /// `attachment_path`), shown beside the row. Never a locked note's.
    pub thumbnail: Option<String>,
    /// While searching: the title, and the snippet from the line where the
    /// words are, in parts with the matches marked (shown in bold). Empty
    /// otherwise.
    pub title_parts: Vec<TextPart>,
    pub snippet_parts: Vec<TextPart>,
    /// A tab note's web page (then `title` is the page's and `snippet` its
    /// site). See [`TabPage`].
    pub tab: Option<TabPage>,
}

/// Part of a search result's text: a match of the search, or not.
#[derive(Clone, Debug, PartialEq, uniffi::Record)]
pub struct TextPart {
    pub text: String,
    pub matched: bool,
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
    /// Any account but the last one can be removed.
    pub can_remove: bool,
    /// Its notes here, Recently Deleted included (what removing the local
    /// account deletes).
    pub note_count: u32,
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
    /// Smart Folders, with their note counts.
    pub smart_folders: Vec<SmartFolderItem>,
    /// CleanTabs' sections: an S3 account's saved tabs each, when its
    /// folder has them.
    pub tabs: Vec<TabsItem>,
}

/// CleanTabs' section for an S3 account's saved tabs (a hidden account of
/// their own): All Tabs, their folders, Recently Deleted.
#[derive(Clone, Debug, PartialEq, uniffi::Record)]
pub struct TabsItem {
    /// The tabs account's id, for [`Place::Tabs`].
    pub id: String,
    /// The S3 account's name.
    pub account_name: String,
    pub all_count: u32,
    pub deleted_count: u32,
    /// Its folders (CleanTabs' collections), depth first.
    pub folders: Vec<FolderItem>,
}

/// A note for the system's search (Spotlight). A locked note's text and
/// tags are empty.
#[derive(Clone, Debug, PartialEq, uniffi::Record)]
pub struct IndexedNote {
    pub id: String,
    pub title: String,
    pub text: String,
    pub folder: String,
    pub tags: Vec<String>,
    /// Seconds since 1970.
    pub modified: i64,
    pub locked: bool,
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
    /// A sync brought pictures: an open note showing ones that were missing
    /// should show itself again.
    pub attachments: bool,
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
        // CleanTabs' saved tabs, in the S3 accounts' folders.
        lib.find_tabs = true;
        Ok(Arc::new(NotesCore { lib: Mutex::new(lib) }))
    }

    /// Sync replies, syncs that are due, the purge of Recently Deleted,
    /// saving, auto-lock. Call it on a timer, after
    /// [`CoreListener::sync_replied`], and when the app comes back. Save the
    /// open editor first ([`NotesCore::save_editor`]).
    pub fn tick(&self) -> Ticked {
        let mut lib = self.lib();
        let t = lib.tick();
        Ticked { changed: t.changed, auto_locked: t.auto_locked, attachments: t.attachments, notices: lib.take_notices() }
    }

    /// Saves a picture (a PNG, JPEG, GIF, WebP, BMP or HEIC file's bytes) to
    /// attach to a note. Returns its name, for
    /// [`EditorCommand::InsertImage`].
    pub fn add_image(&self, data: Vec<u8>) -> Result<String, CoreError> {
        self.lib().add_image(&data).map_err(|e| failed(format!("{e:#}")))
    }

    /// Saves a picture to attach to the note `id`: encrypted, with a key of
    /// its own, if the note is locked. Returns its reference, for
    /// [`EditorCommand::InsertImage`].
    pub fn add_note_image(&self, id: String, data: Vec<u8>) -> Result<String, CoreError> {
        self.lib().add_image_to(&id, &data).map_err(|e| failed(format!("{e:#}")))
    }

    /// Saves a file (a PDF, a document, …; `file_name` is the name it had)
    /// to attach to the note `id`: encrypted, with a key of its own, if the
    /// note is locked. Returns its reference, for
    /// [`EditorCommand::InsertFile`].
    pub fn add_note_file(&self, id: String, data: Vec<u8>, file_name: String) -> Result<String, CoreError> {
        self.lib().add_file_to(&id, &data, &file_name).map_err(|e| failed(format!("{e:#}")))
    }

    /// An attached file (the reference its link names after `attachments/`,
    /// and the name it's shown with) copied out to open in another app: the
    /// copy's path. A locked note's file is decrypted into the copy, which
    /// is removed again on Lock Now.
    pub fn open_attachment(&self, reference: String, label: String) -> Result<String, CoreError> {
        self.lib()
            .open_attachment(&reference, &label)
            .map(|p| p.to_string_lossy().into_owned())
            .map_err(|e| failed(format!("{e:#}")))
    }

    /// Where an attachment's file is, if this device has it yet (a synced
    /// note's pictures may still be on their way). Never a locked note's
    /// picture: see [`NotesCore::attachment_data`].
    pub fn attachment_path(&self, name: String) -> Option<String> {
        self.lib().attachment_path(&name).map(|p| p.to_string_lossy().into_owned())
    }

    /// A picture's bytes by its reference (a locked note's decrypted, in
    /// memory), if this device has it yet.
    pub fn attachment_data(&self, name: String) -> Option<Vec<u8>> {
        self.lib().attachment_data(&name)
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
            .filter(|a| lib.account_enabled(&a.id))
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
                    can_remove: lib.can_remove_account(&a.id),
                    note_count: lib.account_note_count(&a.id) as u32,
                }
            })
            .collect();
        let smart_folders = lib
            .settings
            .smart_folders
            .iter()
            .map(|s| SmartFolderItem {
                id: s.id.clone(),
                name: s.name.clone(),
                count: lib.visible_notes(&Location::Smart(s.id.clone()), "").len() as u32,
                filter: s.filter.clone().into(),
            })
            .collect();
        let tabs = lib
            .tabs_accounts()
            .into_iter()
            .map(|(id, account_name)| {
                let (counts, all, deleted) = lib.tab_counts(&id);
                let folders = lib
                    .tab_folders(&id)
                    .into_iter()
                    .map(|(fid, name, depth)| {
                        let f = lib.store.folder(&fid);
                        FolderItem {
                            count: counts.get(&fid).copied().unwrap_or(0) as u32,
                            parent: f.and_then(|f| f.parent.clone()),
                            is_default: f.is_some_and(|f| f.default),
                            has_children: lib.store.folders.iter().any(|c| c.parent.as_deref() == Some(fid.as_str())),
                            id: fid,
                            name,
                            depth: depth as u32,
                        }
                    })
                    .collect();
                TabsItem { id, account_name, all_count: all as u32, deleted_count: deleted as u32, folders }
            })
            .collect();
        Overview {
            tabs,
            show_all_notes: lib.show_all_notes(),
            all_count: lib.all_count() as u32,
            accounts,
            tags: lib.all_tags(),
            show_note_count: lib.settings.show_note_count,
            smart_folders,
        }
    }

    /// A new Smart Folder; returns its id.
    pub fn add_smart_folder(&self, name: String, filter: SmartFilter) -> String {
        self.lib().add_smart_folder(&name, filter.into())
    }

    pub fn update_smart_folder(&self, id: String, name: String, filter: SmartFilter) {
        self.lib().update_smart_folder(&id, &name, filter.into());
    }

    pub fn delete_smart_folder(&self, id: String) {
        self.lib().delete_smart_folder(&id);
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
        self.lib().rename_folder(&id, &name).map(|_| ()).map_err(failed)
    }

    /// Whether a folder can go into `parent` (`None`: the top level of
    /// `account`): not a "Notes" folder, not into itself or its subfolders.
    pub fn can_move_folder(&self, id: String, account: String, parent: Option<String>) -> bool {
        self.lib().can_move_folder(&id, &account, parent.as_deref())
    }

    /// Moves a folder, with its subfolders and notes, into `parent` (`None`:
    /// the top level of `account`), possibly in another account. Returns
    /// whether it moved.
    pub fn move_folder(&self, id: String, account: String, parent: Option<String>) -> bool {
        self.lib().move_folder(&id, &account, parent.as_deref())
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
                notes: ids
                    .iter()
                    .filter_map(|id| {
                        let mut it = item(lib.note_row(id, &loc, show_folder)?);
                        tab_row(&lib, &mut it);
                        if let Some((t, s)) = lib.search_parts(id, &search) {
                            it.snippet = s.iter().map(|(text, _)| text.as_str()).collect();
                            it.title_parts = parts(t);
                            it.snippet_parts = parts(s);
                        }
                        Some(it)
                    })
                    .collect(),
            })
            .collect()
    }

    pub fn note(&self, id: String, place: Place) -> Option<NoteItem> {
        let lib = self.lib();
        let mut it = item(lib.note_row(&id, &(&place).into(), false)?);
        tab_row(&lib, &mut it);
        Some(it)
    }

    /// "15 November 2026 at 11:32", for the top of the note.
    pub fn note_date(&self, id: String) -> String {
        self.lib().store.note(&id).map(|n| dates::note_header_time(n.modified)).unwrap_or_default()
    }

    /// The folder new notes go to when none is chosen (the compose button on
    /// the Folders screen): the default account's Notes folder, or the
    /// local one if that account is unavailable.
    pub fn default_folder(&self) -> String {
        self.lib().new_note_target(&Location::All).1
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

    /// A note as text to share (Apple Notes' "Send a Copy"): list markers
    /// and checkboxes written out. None if it's locked and closed.
    pub fn shared_text(&self, id: String) -> Option<String> {
        let md = self.lib().plain_body_now(&id)?;
        if let Some((tab, text)) = split_tab(&id, &md) {
            return Some(tab_shared_text(&tab, &cleannotes_core::markdown::from_markdown(&text)));
        }
        Some(cleannotes_core::markdown::from_markdown(&md).shared_text())
    }

    /// A note's Markdown (a locked note's once unlocked).
    pub fn markdown(&self, id: String) -> Option<String> {
        self.lib().plain_body(&id)
    }

    // ---- From outside the app (Spotlight, Shortcuts, Siri) ----

    /// The notes Spotlight should find (see `Library::indexed_notes`).
    pub fn indexed_notes(&self) -> Vec<IndexedNote> {
        self.lib()
            .indexed_notes()
            .into_iter()
            .map(|n| IndexedNote {
                id: n.id,
                title: n.title,
                text: n.text,
                folder: n.folder,
                tags: n.tags,
                modified: n.modified.timestamp(),
                locked: n.locked,
            })
            .collect()
    }

    /// Where a note is listed (its folder, or Recently Deleted), to open it
    /// over that list. None if it's gone.
    pub fn note_place(&self, id: String) -> Option<Place> {
        self.lib().note_location(&id).map(Place::from)
    }

    /// A note with `text` (plain text; the first line is its title) in
    /// `folder`, or where new notes go.
    pub fn create_note(&self, folder: Option<String>, text: String) -> NewNote {
        let (id, folder) = self.lib().create_note(folder.as_deref(), &text);
        NewNote { id, place: Place::Folder { id: folder } }
    }

    /// Adds plain text to the end of a note. Not to a locked note, nor one in
    /// Recently Deleted. Save the open editor first.
    pub fn add_text(&self, id: String, text: String) -> Result<(), CoreError> {
        self.lib().add_text(&id, &text).map_err(|e| failed(e.to_string()))
    }

    // ---- The open note ----

    /// The editor for a note: none if it doesn't exist or is locked and
    /// closed.
    pub fn open_editor(&self, id: String) -> Option<Arc<NoteEditor>> {
        let md = self.lib().plain_body(&id)?;
        Some(NoteEditor::for_note(&id, &md))
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
        let md = join_tab(editor.tab().as_ref(), md);
        self.lib().set_body(&id, &md) != BodySet::Closed
    }

    /// After a sync: the editor follows its note. Returns false if the note
    /// is gone or closed (locked), so the editor should close.
    pub fn reload_editor(&self, editor: Arc<NoteEditor>) -> bool {
        let id = editor.ed().note_id.clone();
        match self.lib().plain_body_now(&id) {
            Some(md) => {
                editor.ed().reload_with_tab(&mut editor.tab(), &md);
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

    /// Removes an account (any but the last): a synced one's notes stay
    /// where they're synced to; the local account's are deleted for good.
    pub fn remove_account(&self, id: String) {
        self.lib().remove_account(&id);
    }

    /// Adds "On My iPhone" back, empty, after it was removed.
    pub fn add_local_account(&self) {
        self.lib().add_local_account();
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
        empty: r.empty,
        time: r.time,
        folder: r.folder,
        pinned: r.pinned,
        locked: r.locked,
        deleted: r.deleted,
        thumbnail: r.thumbnail,
        title_parts: Vec::new(),
        snippet_parts: Vec::new(),
        tab: None,
    }
}

fn parts(p: Vec<(String, bool)>) -> Vec<TextPart> {
    p.into_iter().map(|(text, matched)| TextPart { text, matched }).collect()
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
    /// A picture: the run is one attachment character (U+FFFC), shown as
    /// the attachment of this name ([`NotesCore::attachment_path`]).
    pub image: Option<String>,
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
    /// The list marker as text ("•", "–", "3.", "☐", "☑"; empty if not a
    /// list item), for drawing it and for printing.
    pub marker: String,
    /// A table cell (its style is Body): where it is in its table.
    pub cell: Option<TableCell>,
    /// An attached file on a line of its own (only a link to it): shown as
    /// a card, a PDF with its pages.
    pub file: Option<AttachedFile>,
    pub runs: Vec<TextRun>,
}

/// An attached file a paragraph is (see [`Paragraph::file`]).
#[derive(Clone, Debug, PartialEq, uniffi::Record)]
pub struct AttachedFile {
    /// Its attachment (for [`NotesCore::attachment_data`]).
    pub reference: String,
    /// A PDF (by its name; a locked note's by the name it's shown with).
    pub pdf: bool,
    pub view: FileView,
}

/// How an attached file is shown (Apple Notes' View As). A PDF is Medium
/// unless set; other files are always Small.
#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum FileView {
    /// Its name beside a small picture of it.
    Small,
    /// Its pages side by side.
    Medium,
    /// Its pages as wide as the note.
    Large,
}

/// Where a table cell is. A table's cells are paragraphs in a row, row by
/// row; Return in a cell is a line break in it.
#[derive(Clone, Debug, PartialEq, uniffi::Record)]
pub struct TableCell {
    pub row: u32,
    pub column: u32,
    pub rows: u32,
    pub columns: u32,
    /// In the table's header row or header column.
    pub header: bool,
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
    /// A list item (bulleted, dashed, numbered or checklist): it can move
    /// up and down.
    pub is_list: bool,
    /// The caret is in a table cell (the Table menu's row and column
    /// commands), and whether that table has a header row and column.
    pub in_table: bool,
    pub header_row: bool,
    pub header_column: bool,
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
    /// The caret is in a checklist with ticked items (Uncheck All, Delete
    /// Checked).
    pub checklist_has_checked: bool,
    /// The caret is in a checklist with unticked items (Check All).
    pub checklist_has_unchecked: bool,
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
    /// The checklist at the caret: tick every item, untick every item,
    /// remove the ticked ones, or move them below the others.
    CheckAll,
    UncheckAll,
    DeleteChecked,
    MoveCheckedToBottom,
    /// Link the selection (or insert the address); `None` removes links.
    SetLink { url: Option<String> },
    Undo,
    Redo,
    SelectAll,
    /// Puts a picture (saved with [`NotesCore::add_image`]) at the caret,
    /// on a line of its own.
    InsertImage { name: String },
    /// Puts an attached file (saved with [`NotesCore::add_note_file`]) at
    /// the caret: its name, linked to it, on a line of its own.
    InsertFile { reference: String, label: String },
    /// A 3×3 table at the caret (not inside a table).
    InsertTable,
    /// The table at the caret: rows, columns, headers, or all of it.
    AddRowAbove,
    AddRowBelow,
    AddColumnBefore,
    AddColumnAfter,
    DeleteRow,
    DeleteColumn,
    DeleteTable,
    ToggleHeaderRow,
    ToggleHeaderColumn,
    /// Tab and Shift+Tab in a table: the next or previous cell (Tab in the
    /// last cell adds a row).
    NextCell,
    PreviousCell,
}

#[derive(uniffi::Object)]
pub struct NoteEditor {
    ed: Mutex<Editing>,
    /// A tab note's page: `ed` has only the text below it (see
    /// [`NoteEditor::tab_page`]).
    tab: Mutex<Option<TabNote>>,
}

/// A clock for grouping typing into undo steps.
fn clock() -> f64 {
    static START: OnceLock<Instant> = OnceLock::new();
    START.get_or_init(Instant::now).elapsed().as_secs_f64()
}

impl NoteEditor {
    fn wrap(ed: Editing) -> Arc<NoteEditor> {
        Arc::new(NoteEditor { ed: Mutex::new(ed), tab: Mutex::new(None) })
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
        let paragraphs = e.doc.blocks.iter().enumerate().map(|(i, b)| paragraph(b, numbers[i], table_cell(&e.doc, i), attached_file(&e.doc, i))).collect();
        let (a, b) = (to_utf16(&e.doc, e.sel.start()), to_utf16(&e.doc, e.sel.end()));
        EditorState { paragraphs, selection_start: a, selection_end: b }
    }

    /// One paragraph (to restyle it after typing in it).
    pub fn paragraph(&self, index: u32) -> Option<Paragraph> {
        let e = self.ed();
        let i = index as usize;
        let b = e.doc.blocks.get(i)?;
        Some(paragraph(b, e.doc.list_numbers()[i], table_cell(&e.doc, i), attached_file(&e.doc, i)))
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
            EditorCommand::CheckAll => Command::CheckAll,
            EditorCommand::UncheckAll => Command::UncheckAll,
            EditorCommand::DeleteChecked => Command::DeleteChecked,
            EditorCommand::MoveCheckedToBottom => Command::MoveCheckedToBottom,
            EditorCommand::SetLink { url } => Command::SetLink(url),
            EditorCommand::Undo => Command::Undo,
            EditorCommand::Redo => Command::Redo,
            EditorCommand::SelectAll => Command::SelectAll,
            EditorCommand::InsertImage { name } => Command::InsertImage(name),
            EditorCommand::InsertFile { reference, label } => Command::InsertFile { reference, label },
            EditorCommand::InsertTable => Command::InsertTable,
            EditorCommand::AddRowAbove => Command::AddRowAbove,
            EditorCommand::AddRowBelow => Command::AddRowBelow,
            EditorCommand::AddColumnBefore => Command::AddColumnBefore,
            EditorCommand::AddColumnAfter => Command::AddColumnAfter,
            EditorCommand::DeleteRow => Command::DeleteRow,
            EditorCommand::DeleteColumn => Command::DeleteColumn,
            EditorCommand::DeleteTable => Command::DeleteTable,
            EditorCommand::ToggleHeaderRow => Command::ToggleHeaderRow,
            EditorCommand::ToggleHeaderColumn => Command::ToggleHeaderColumn,
            EditorCommand::NextCell => {
                self.ed().tab_in_table(false);
                return;
            }
            EditorCommand::PreviousCell => {
                self.ed().tab_in_table(true);
                return;
            }
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
        let (checklist_has_checked, checklist_has_unchecked) = e.checklist_state().unwrap_or((false, false));
        Formatting {
            style: ParagraphStyle::of(b.kind),
            is_list: b.kind.is_list(),
            in_table: b.kind.is_cell(),
            header_row: matches!(b.kind, Kind::Cell { header_row: true, .. }),
            header_column: matches!(b.kind, Kind::Cell { header_col: true, .. }),
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
            checklist_has_checked,
            checklist_has_unchecked,
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

    /// The note as text to share: list markers and checkboxes written out
    /// (with changes not saved yet).
    pub fn shared_text(&self) -> String {
        let e = self.ed();
        match &*self.tab() {
            Some(tab) => tab_shared_text(tab, &e.doc),
            None => e.doc.shared_text(),
        }
    }
}

/// Paragraph kinds and indents, to tell plain typing from a change of
/// structure.
fn shape(d: &Doc) -> Vec<(Kind, u8)> {
    d.blocks.iter().map(|b| (b.kind, b.indent)).collect()
}

/// Where a block is in its table, if it's a table cell.
fn table_cell(d: &Doc, block: usize) -> Option<TableCell> {
    let Kind::Cell { header_row, header_col, .. } = d.blocks.get(block)?.kind else { return None };
    let (range, columns) = d.table_of(block)?;
    let (row, column) = d.cell_at(block)?;
    let rows = range.len().div_ceil(columns);
    Some(TableCell {
        row: row as u32,
        column: column as u32,
        rows: rows as u32,
        columns: columns as u32,
        header: (header_row && row == 0) || (header_col && column == 0),
    })
}

/// Paragraph `i` as an attached file, if it's one.
fn attached_file(d: &doc::Doc, i: usize) -> Option<AttachedFile> {
    let reference = d.file_at(i)?.to_owned();
    let label = &d.blocks[i].text;
    let (name, _) = attachments::split_ref(&reference);
    let pdf_name = |s: &str| s.len() > 4 && s.is_char_boundary(s.len() - 4) && s[s.len() - 4..].eq_ignore_ascii_case(".pdf");
    let sealed = name.ends_with(&format!(".{}", attachments::SEALED_EXT));
    let pdf = pdf_name(name) || (sealed && pdf_name(label.trim()));
    let view = match d.file_view(i) {
        _ if !pdf => FileView::Small,
        doc::FileView::Small => FileView::Small,
        doc::FileView::Medium => FileView::Medium,
        doc::FileView::Large => FileView::Large,
    };
    Some(AttachedFile { reference, pdf, view })
}

fn paragraph(b: &doc::Block, number: u32, cell: Option<TableCell>, file: Option<AttachedFile>) -> Paragraph {
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
        let image = s.image.as_deref().map(str::to_owned);
        let run = TextRun {
            text: String::new(),
            bold: s.bold,
            italic: s.italic,
            underline: s.underline,
            strike: s.strike,
            code: s.code,
            link,
            tag: tag[i],
            image: image.clone(),
        };
        match runs.last_mut() {
            // A picture is a run of its own (one attachment character).
            Some(last) if image.is_none() && TextRun { text: String::new(), ..last.clone() } == run => last.text.push(*c),
            _ => runs.push(TextRun { text: c.to_string(), ..run }),
        }
    }
    Paragraph {
        style: ParagraphStyle::of(b.kind),
        indent: b.indent as u32,
        checked: b.kind == Kind::Checklist(true),
        number,
        marker: b.kind.list_marker(number),
        cell,
        file,
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

// ==== CleanPassword's vault =====================================================
//
// The iPhone app's CleanPassword section, as the desktop app's
// (`src/app/passwords.rs`): CleanPassword's vault, when an S3 account's
// bucket holds one, found with the accounts' access keys, unlocked with the
// master password, its items read, and deleted, recovered and deleted
// permanently. Editing items is CleanPassword's. Everything is
// [`cleannotes_core::passwords`]; this is what the screens see of it.

use cleannotes_core::passwords::{self as vault, ItemExt};

/// The vault is looked for again this often (a vault made since, or one
/// that was unreachable), as in the desktop app.
const VAULT_SEARCH_EVERY: std::time::Duration = std::time::Duration::from_secs(10 * 60);

/// The app, told when something about the vault changed in the background
/// (found or gone, unlocked, synced, a change saved). Called on a background
/// thread: the app should then read [`PasswordsCore::status`] again on the
/// main thread.
#[uniffi::export(with_foreign)]
pub trait PasswordsListener: Send + Sync {
    fn vault_changed(&self);
}

/// What's going on in the background.
#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum VaultBusy {
    Unlocking,
    Refreshing,
}

/// What a CleanPassword list shows.
#[derive(Clone, Debug, PartialEq, Eq, Hash, uniffi::Enum)]
pub enum VaultPlace {
    /// All Passwords (not those in Recently Deleted).
    All,
    /// Items with a verification code.
    Codes,
    /// A folder (`Work`, or `Work/Email` inside it), with the folders
    /// inside it.
    Folder { path: String },
    /// CleanPassword's Recently Deleted.
    Deleted,
}

/// One of the vault's folders, for the sidebar.
#[derive(Clone, Debug, PartialEq, uniffi::Record)]
pub struct VaultFolder {
    /// `Money/Banks`, for [`VaultPlace::Folder`].
    pub path: String,
    /// `Banks`.
    pub name: String,
    /// How deep inside other folders it is (0 at the top).
    pub depth: u32,
    /// Its items, with those of the folders inside it.
    pub count: u32,
}

/// The vault, as the folders screen shows it.
#[derive(Clone, Debug, PartialEq, uniffi::Record)]
pub struct VaultStatus {
    /// An S3 account's bucket holds a vault: the CleanPassword section is
    /// shown.
    pub found: bool,
    /// Where: "bucket/folder".
    pub place: String,
    /// The account whose access key reads it.
    pub account: String,
    /// Looking for a vault.
    pub searching: bool,
    pub unlocked: bool,
    pub busy: Option<VaultBusy>,
    /// Changes are being saved.
    pub saving: bool,
    /// Why unlocking or syncing last failed (shown under the master
    /// password field, or on top of the item).
    pub error: Option<String>,
    /// Counts, while unlocked (0 while locked).
    pub all_count: u32,
    pub codes_count: u32,
    pub deleted_count: u32,
    /// The items' folders and those around them, sorted, while unlocked.
    pub folders: Vec<VaultFolder>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum VaultItemKind {
    Login,
    /// A secure note.
    Note,
}

/// A verification code as it is now.
#[derive(Clone, Debug, PartialEq, uniffi::Record)]
pub struct VaultCode {
    /// "287082", to copy.
    pub code: String,
    /// "287 082", to show.
    pub spaced: String,
    /// Seconds until the next code.
    pub remaining: u32,
    /// Seconds each code lasts.
    pub period: u32,
}

/// An item in a CleanPassword list.
#[derive(Clone, Debug, PartialEq, uniffi::Record)]
pub struct VaultRow {
    pub id: String,
    /// Its name, or its first website's host, or its user name.
    pub title: String,
    /// The user name, "Secure Note", or (in Recently Deleted) when it was
    /// deleted.
    pub subtitle: String,
    pub kind: VaultItemKind,
    pub username: String,
    /// It has a password (to copy: [`PasswordsCore::item`]).
    pub has_password: bool,
    /// Its verification code now, if it has one.
    pub code: Option<VaultCode>,
    /// Its first website, to open (`https://…`).
    pub website: Option<String>,
    /// In Recently Deleted.
    pub deleted: bool,
}

#[derive(Clone, Debug, PartialEq, uniffi::Record)]
pub struct VaultWebsite {
    /// As saved.
    pub uri: String,
    /// Its host ("github.com"), or as saved.
    pub label: String,
    /// To open: `https://` added to a bare host.
    pub url: String,
}

#[derive(Clone, Debug, PartialEq, uniffi::Record)]
pub struct VaultPasskey {
    /// The site it signs in to ("github.com").
    pub site: String,
    /// The account's name on the site, if known.
    pub user: String,
}

/// A custom field.
#[derive(Clone, Debug, PartialEq, uniffi::Record)]
pub struct VaultField {
    pub name: String,
    pub value: String,
    /// Shown as dots until revealed (a PIN, a security answer).
    pub hidden: bool,
}

/// A password the item had before.
#[derive(Clone, Debug, PartialEq, uniffi::Record)]
pub struct VaultHistoryEntry {
    pub password: String,
    /// When it stopped being the password ("Yesterday", "3 Mar 2026").
    pub retired: String,
}

/// An item's details, as Apple's Passwords app shows them.
#[derive(Clone, Debug, PartialEq, uniffi::Record)]
pub struct VaultItem {
    pub id: String,
    pub title: String,
    pub kind: VaultItemKind,
    pub username: String,
    pub password: String,
    /// Its verification code now, if it has one.
    pub code: Option<VaultCode>,
    pub websites: Vec<VaultWebsite>,
    pub passkeys: Vec<VaultPasskey>,
    pub fields: Vec<VaultField>,
    pub notes: String,
    /// Earlier passwords, most recent first.
    pub history: Vec<VaultHistoryEntry>,
    pub folder: String,
    pub favorite: bool,
    /// "15 November 2026 at 11:32".
    pub modified: String,
    /// In Recently Deleted.
    pub deleted: bool,
}

/// CleanPassword's vault for the iPhone app ([`vault::Passwords`]): found,
/// unlocked and synced on background threads, which tell the app through
/// [`PasswordsListener`]; the screens read it while they draw.
#[derive(uniffi::Object)]
pub struct PasswordsCore {
    service: vault::Passwords,
    /// The S3 accounts last looked with.
    searched_with: Mutex<Option<String>>,
}

#[uniffi::export]
impl PasswordsCore {
    /// The vault is kept in `dir` (the app's Application Support folder)
    /// between unlocks, sealed with the vault key, as CleanPassword's
    /// devices keep it.
    #[uniffi::constructor]
    pub fn new(dir: String, listener: Arc<dyn PasswordsListener>) -> Arc<PasswordsCore> {
        let service = vault::Passwords::new(move || listener.vault_changed());
        service.set_cache_dir(std::path::PathBuf::from(dir));
        Arc::new(PasswordsCore { service, searched_with: Mutex::new(None) })
    }

    /// Looks for a vault with `notes`' S3 accounts' access keys, in the
    /// background, when the accounts changed and every 10 minutes (else does
    /// nothing): call it on the app's timer. A vault no account can see any
    /// more is locked and forgotten.
    pub fn look_for_vault(&self, notes: Arc<NotesCore>) {
        let lib = notes.lib();
        let accounts: Vec<(String, s3::Config)> = lib
            .store
            .accounts
            .iter()
            .filter(|a| lib.account_enabled(&a.id))
            .filter_map(|a| match &a.backend {
                BackendConfig::S3(c) => Some((a.id.clone(), c.clone())),
                _ => None,
            })
            .collect();
        let fingerprint = format!("{accounts:?}");
        let stale = self.service.last_search().is_none_or(|t| t.elapsed() > VAULT_SEARCH_EVERY);
        let mut searched = self.searched_with.lock().unwrap_or_else(|e| e.into_inner());
        if searched.as_deref() == Some(fingerprint.as_str()) && !stale {
            return;
        }
        // The access keys come from the keychain: only now.
        let candidates: Vec<vault::Candidate> = accounts.into_iter().filter_map(|(id, c)| Some((id.clone(), c, lib.db.secret(&id)?))).collect();
        drop(lib);
        *searched = Some(fingerprint);
        if !candidates.is_empty() {
            self.service.search(candidates);
        } else if self.service.state().found.is_some() {
            // The account was removed: so is its vault.
            self.service.search(Vec::new());
        }
    }

    /// Call now and then (the app's timer): locks the vault after 15
    /// minutes without use (returns true then), and syncs it every minute.
    pub fn tick(&self) -> bool {
        self.service.tick()
    }

    pub fn status(&self) -> VaultStatus {
        let s = self.service.state();
        let found = s.found.as_ref();
        let folders = s
            .folders()
            .into_iter()
            .map(|path| VaultFolder {
                name: path.rsplit('/').next().unwrap_or(&path).to_owned(),
                depth: path.matches('/').count() as u32,
                count: vault_items(&s, &VaultPlace::Folder { path: path.clone() }, "").len() as u32,
                path,
            })
            .collect();
        VaultStatus {
            found: found.is_some(),
            place: found.map(|f| f.place()).unwrap_or_default(),
            account: found.map(|f| f.account.clone()).unwrap_or_default(),
            searching: s.searching,
            unlocked: s.unlocked(),
            busy: s.busy.map(|b| match b {
                vault::Busy::Unlocking => VaultBusy::Unlocking,
                vault::Busy::Refreshing => VaultBusy::Refreshing,
            }),
            saving: s.saving(),
            error: s.error.clone(),
            all_count: s.items().len() as u32,
            codes_count: vault_items(&s, &VaultPlace::Codes, "").len() as u32,
            deleted_count: s.deleted().len() as u32,
            folders,
        }
    }

    /// Unlocks the vault with the master password and syncs it, in the
    /// background: [`VaultStatus::busy`] until it's unlocked, or
    /// [`VaultStatus::error`] says why not.
    pub fn unlock(&self, password: String) {
        self.service.unlock(password);
    }

    /// Lock Now: the vault and its keys are dropped (the keys wiped).
    pub fn lock(&self) {
        self.service.lock();
    }

    /// Syncs the unlocked vault now, in the background (pull to refresh).
    pub fn refresh(&self) {
        self.service.refresh();
    }

    /// The vault was used: it stays unlocked another 15 minutes.
    pub fn touch(&self) {
        self.service.touch();
    }

    /// The items `place` shows that have every word of `search` (all, when
    /// it's empty), by name; Recently Deleted's most recent first.
    pub fn items(&self, place: VaultPlace, search: String) -> Vec<VaultRow> {
        let s = self.service.state();
        vault_items(&s, &place, &search).into_iter().map(|i| vault_row(i, &place)).collect()
    }

    /// An item's details, while unlocked.
    pub fn item(&self, id: String) -> Option<VaultItem> {
        self.service.state().item(&id).map(vault_item)
    }

    /// An item's verification code now (for its countdown).
    pub fn code(&self, id: String) -> Option<VaultCode> {
        self.service.state().item(&id).and_then(|i| vault_code(&i.totp))
    }

    /// To CleanPassword's Recently Deleted: shown at once, saved to the
    /// vault in the background (a change that can't be saved now is kept
    /// and tried again, and [`VaultStatus::error`] says so).
    pub fn delete(&self, ids: Vec<String>) {
        self.service.change(ids, vault::Change::Delete);
    }

    /// Back from Recently Deleted.
    pub fn recover(&self, ids: Vec<String>) {
        self.service.change(ids, vault::Change::Recover);
    }

    /// For good, from every device (unless another device edits it first).
    pub fn delete_permanently(&self, ids: Vec<String>) {
        self.service.change(ids, vault::Change::DeleteForever);
    }
}

/// The items `place` shows, matching `search`.
fn vault_items<'a>(s: &'a vault::State, place: &VaultPlace, search: &str) -> Vec<&'a vault::Item> {
    let items = if *place == VaultPlace::Deleted { s.deleted() } else { s.items() };
    items
        .into_iter()
        .filter(|i| match place {
            VaultPlace::All | VaultPlace::Deleted => true,
            VaultPlace::Codes => vault::totp::code_now(&i.totp).is_some(),
            VaultPlace::Folder { path } => i.in_folder(path),
        })
        .filter(|i| search.trim().is_empty() || i.matches(search))
        .collect()
}

fn vault_kind(i: &vault::Item) -> VaultItemKind {
    if i.kind == vault::ItemKind::Note {
        VaultItemKind::Note
    } else {
        VaultItemKind::Login
    }
}

fn vault_code(totp: &str) -> Option<VaultCode> {
    vault::totp::code_now(totp).map(|c| VaultCode {
        spaced: vault::totp::spaced(&c.code),
        code: c.code,
        remaining: c.remaining as u32,
        period: c.period as u32,
    })
}

/// A saved website as a URL to open (`github.com` → `https://github.com`).
fn website_url(uri: &str) -> String {
    let u = uri.trim();
    if u.contains("://") {
        u.to_owned()
    } else {
        format!("https://{u}")
    }
}

fn vault_row(i: &vault::Item, place: &VaultPlace) -> VaultRow {
    let subtitle = match place {
        VaultPlace::Deleted => i.deleted.map(dates::list_time).unwrap_or_default(),
        _ if i.kind == vault::ItemKind::Note => "Secure Note".into(),
        _ => i.username.clone(),
    };
    VaultRow {
        id: i.id.clone(),
        title: i.title(),
        subtitle,
        kind: vault_kind(i),
        username: i.username.clone(),
        has_password: !i.password.is_empty(),
        code: vault_code(&i.totp),
        website: i.uris.first().map(|u| website_url(&u.uri)),
        deleted: i.deleted.is_some(),
    }
}

fn vault_item(i: &vault::Item) -> VaultItem {
    VaultItem {
        id: i.id.clone(),
        title: i.title(),
        kind: vault_kind(i),
        username: i.username.clone(),
        password: i.password.clone(),
        code: vault_code(&i.totp),
        websites: i
            .uris
            .iter()
            .map(|u| VaultWebsite { uri: u.uri.clone(), label: vault::host(&u.uri).unwrap_or_else(|| u.uri.clone()), url: website_url(&u.uri) })
            .collect(),
        passkeys: i
            .passkeys
            .iter()
            .map(|k| VaultPasskey {
                site: k.rp_id.clone(),
                user: if k.user_name.is_empty() { k.user_display_name.clone() } else { k.user_name.clone() },
            })
            .collect(),
        fields: i.fields.iter().map(|f| VaultField { name: f.name.clone(), value: f.value.clone(), hidden: f.hidden }).collect(),
        notes: i.notes.trim_end().to_owned(),
        history: i.history.iter().rev().map(|h| VaultHistoryEntry { password: h.password.clone(), retired: dates::list_time(h.retired) }).collect(),
        folder: i.folder.clone(),
        favorite: i.favorite,
        modified: dates::note_header_time(i.modified),
        deleted: i.deleted.is_some(),
    }
}

#[cfg(test)]
mod passwords_tests {
    use super::*;

    struct Quiet;
    impl PasswordsListener for Quiet {
        fn vault_changed(&self) {}
    }

    fn passwords() -> (Arc<PasswordsCore>, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        (PasswordsCore::new(dir.path().to_string_lossy().into_owned(), Arc::new(Quiet)), dir)
    }

    /// Where the vault was found (the desktop app's tests' vault).
    fn found() -> vault::Found {
        let header = vault::VaultHeader {
            format: "cleanpassword-vault".into(),
            version: 2,
            vault_id: "v1".into(),
            kdf: vault::KdfParams::with_cost(8, 1),
            wrapped_key: String::new(),
            created: chrono::Utc::now(),
        };
        let config = s3::Config { bucket: "b".into(), region: "us-east-1".into(), prefix: "CleanPassword".into(), endpoint: None, encrypted: false };
        vault::Found { account: "acct".into(), config, header, header_etag: String::new() }
    }

    /// A vault made by CleanPassword's core, holding GitHub, Bank, Wi-Fi and
    /// Old (in Recently Deleted). Its S3 is nowhere (a closed port), so its
    /// syncs fail at once.
    fn made_vault() -> vault::Vault {
        use vault::{Field, HistoryEntry, ImportedItem, ItemData, ItemUri, Passkey};
        let now = chrono::Utc::now();
        let config = vault::S3Config { bucket: "b".into(), region: "us-east-1".into(), prefix: "CleanPassword".into(), endpoint: Some("http://127.0.0.1:9".into()) };
        let creds = vault::AwsCredentials { access_key_id: "AKIA".into(), secret_access_key: "secret".into(), session_token: None };
        let mut v = vault::Vault::create(config, creds, "correct horse battery").unwrap();
        let login = |name: &str| ItemData { name: name.into(), username: "me".into(), ..Default::default() };
        let item = |data: ItemData| ImportedItem { data, history: vec![], passkeys: vec![] };
        v.import(vec![
            ImportedItem {
                data: ItemData {
                    password: "s3cret-pw".into(),
                    username: "octocat".into(),
                    uris: vec![ItemUri::new("https://github.com/login"), ItemUri::new("gist.github.com")],
                    totp: "JBSWY3DPEHPK3PXP".into(),
                    notes: "Recovery codes in the safe.\n".into(),
                    folder: "Work".into(),
                    fields: vec![Field { name: "PIN".into(), value: "4321".into(), hidden: true }],
                    ..login("GitHub")
                },
                history: vec![HistoryEntry { password: "old-pw".into(), retired: now }],
                passkeys: vec![Passkey {
                    credential_id: "Y3JlZA".into(),
                    rp_id: "github.com".into(),
                    rp_name: String::new(),
                    user_handle: "dQ".into(),
                    user_name: "octocat".into(),
                    user_display_name: String::new(),
                    key: String::new(),
                    counter: 0,
                    created: now,
                }],
            },
            item(ItemData { password: "x".into(), folder: "Money/Banks".into(), ..login("Bank") }),
            item(ItemData { kind: vault::ItemKind::Note, name: "Wi-Fi".into(), notes: "swordfish".into(), ..Default::default() }),
            item(login("Old")),
        ]);
        let old = v.items().iter().find(|i| i.name == "Old").unwrap().id.clone();
        v.delete(&old).unwrap();
        v
    }

    fn titles(rows: &[VaultRow]) -> Vec<&str> {
        rows.iter().map(|r| r.title.as_str()).collect()
    }

    fn id_of(p: &PasswordsCore, title: &str) -> String {
        let all = p.items(VaultPlace::All, String::new()).into_iter().chain(p.items(VaultPlace::Deleted, String::new()));
        all.into_iter().find(|r| r.title == title).unwrap_or_else(|| panic!("no {title}")).id
    }

    fn wait(what: impl Fn() -> bool) {
        let start = Instant::now();
        while !what() {
            assert!(start.elapsed() < std::time::Duration::from_secs(60), "timed out");
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }

    #[test]
    fn the_vault_is_shown_locked_then_unlocked() {
        let (p, _dir) = passwords();
        assert!(!p.status().found, "no vault yet: no section");

        // Found, locked: the section, nothing in it.
        p.service.set_for_test(found(), None);
        let s = p.status();
        assert!(s.found && !s.unlocked && !s.searching);
        assert_eq!((s.place.as_str(), s.account.as_str()), ("b/CleanPassword", "acct"));
        assert_eq!((s.all_count, s.codes_count, s.deleted_count), (0, 0, 0));
        assert!(s.folders.is_empty() && s.busy.is_none() && s.error.is_none());
        assert!(p.items(VaultPlace::All, String::new()).is_empty());

        // Unlocked: counts, folders (with those around them), and the items
        // in each list.
        p.service.set_for_test(found(), Some(made_vault()));
        let s = p.status();
        assert!(s.unlocked);
        assert_eq!((s.all_count, s.codes_count, s.deleted_count), (3, 1, 1));
        let folder = |path: &str, name: &str, depth: u32, count: u32| VaultFolder { path: path.into(), name: name.into(), depth, count };
        assert_eq!(s.folders, [folder("Money", "Money", 0, 1), folder("Money/Banks", "Banks", 1, 1), folder("Work", "Work", 0, 1)]);

        let all = p.items(VaultPlace::All, String::new());
        assert_eq!(titles(&all), ["Bank", "GitHub", "Wi-Fi"]);
        assert_eq!(all.iter().map(|r| r.subtitle.as_str()).collect::<Vec<_>>(), ["me", "octocat", "Secure Note"]);
        assert_eq!(all.iter().map(|r| r.kind).collect::<Vec<_>>(), [VaultItemKind::Login, VaultItemKind::Login, VaultItemKind::Note]);
        let gh = &all[1];
        assert!(gh.has_password && !gh.deleted && gh.code.is_some());
        assert_eq!(gh.website.as_deref(), Some("https://github.com/login"));
        assert!(all[2].website.is_none() && all[2].code.is_none() && !all[2].has_password);

        assert_eq!(titles(&p.items(VaultPlace::Codes, String::new())), ["GitHub"]);
        assert_eq!(titles(&p.items(VaultPlace::Folder { path: "Money".into() }, String::new())), ["Bank"]);
        assert_eq!(titles(&p.items(VaultPlace::Folder { path: "Money/Banks".into() }, String::new())), ["Bank"]);
        assert_eq!(titles(&p.items(VaultPlace::All, "  GIT ".into())), ["GitHub"]);
        assert!(p.items(VaultPlace::All, "git money".into()).is_empty());
        let deleted = p.items(VaultPlace::Deleted, String::new());
        assert_eq!(titles(&deleted), ["Old"]);
        assert!(deleted[0].deleted);
        assert!(!deleted[0].subtitle.is_empty(), "when it was deleted");

        // An item's details.
        let item = p.item(gh.id.clone()).unwrap();
        assert_eq!((item.title.as_str(), item.kind, item.username.as_str(), item.password.as_str()), ("GitHub", VaultItemKind::Login, "octocat", "s3cret-pw"));
        let code = item.code.unwrap();
        assert_eq!(code.code.len(), 6);
        assert_eq!(code.spaced, format!("{} {}", &code.code[..3], &code.code[3..]));
        assert_eq!(code.period, 30);
        assert!((1..=30).contains(&code.remaining));
        assert_eq!(
            item.websites,
            [
                VaultWebsite { uri: "https://github.com/login".into(), label: "github.com".into(), url: "https://github.com/login".into() },
                VaultWebsite { uri: "gist.github.com".into(), label: "gist.github.com".into(), url: "https://gist.github.com".into() },
            ]
        );
        assert_eq!(item.passkeys, [VaultPasskey { site: "github.com".into(), user: "octocat".into() }]);
        assert_eq!(item.fields, [VaultField { name: "PIN".into(), value: "4321".into(), hidden: true }]);
        assert_eq!(item.notes, "Recovery codes in the safe.");
        assert_eq!(item.history.iter().map(|h| h.password.as_str()).collect::<Vec<_>>(), ["old-pw"]);
        assert!(!item.history[0].retired.is_empty() && !item.modified.is_empty());
        assert_eq!((item.folder.as_str(), item.deleted), ("Work", false));
        assert!(p.code(gh.id.clone()).is_some());
        assert!(p.code(all[0].id.clone()).is_none());
        assert_eq!(p.item(all[2].id.clone()).unwrap().kind, VaultItemKind::Note);
        assert!(p.item(deleted[0].id.clone()).unwrap().deleted);
        assert!(p.item("nope".into()).is_none());

        // Lock Now: nothing is shown any more.
        p.touch();
        p.lock();
        let s = p.status();
        assert!(s.found && !s.unlocked);
        assert_eq!((s.all_count, s.deleted_count), (0, 0));
        assert!(p.items(VaultPlace::All, String::new()).is_empty());
        assert!(p.item(gh.id.clone()).is_none() && p.code(gh.id.clone()).is_none());
        assert!(!p.tick(), "nothing to lock");
    }

    #[test]
    fn deleting_recovering_and_deleting_permanently() {
        let (p, _dir) = passwords();
        p.service.set_for_test(found(), Some(made_vault()));
        let (gh, wifi, old) = (id_of(&p, "GitHub"), id_of(&p, "Wi-Fi"), id_of(&p, "Old"));

        // Shown at once.
        p.delete(vec![gh.clone(), wifi.clone()]);
        assert_eq!(titles(&p.items(VaultPlace::All, String::new())), ["Bank"]);
        assert_eq!(p.status().deleted_count, 3);
        assert!(p.item(gh.clone()).unwrap().deleted);
        p.recover(vec![gh.clone()]);
        assert_eq!(titles(&p.items(VaultPlace::All, String::new())), ["Bank", "GitHub"]);
        p.delete_permanently(vec![old.clone()]);
        assert!(p.item(old.clone()).is_none());
        assert_eq!(titles(&p.items(VaultPlace::Deleted, String::new())), ["Wi-Fi"]);

        // The vault's S3 can't be reached: the changes stay, and that's said.
        wait(|| !p.status().saving);
        let s = p.status();
        assert!(s.error.as_deref().unwrap_or_default().starts_with("Couldn't save the change"), "{:?}", s.error);
        assert_eq!((s.all_count, s.deleted_count), (2, 1));
        assert!(p.item(old).is_none());
    }

    #[test]
    fn unlocking_says_why_it_failed() {
        let (p, _dir) = passwords();
        // Not found: nothing happens.
        p.unlock("correct horse battery".into());
        assert!(p.status().busy.is_none());

        // Found, but its access key can't be read.
        p.service.set_for_test(found(), None);
        p.unlock("correct horse battery".into());
        wait(|| p.status().busy.is_none());
        let s = p.status();
        assert!(!s.unlocked);
        assert!(s.error.is_some());
    }

    #[test]
    fn the_vault_is_looked_for_when_the_s3_accounts_change() {
        let (notes, _notes_dir) = tests_core();
        let (p, _dir) = passwords();

        // Only "On My iPhone": nothing to look with.
        p.look_for_vault(notes.clone());
        assert!(p.service.last_search().is_none());

        // An S3 account (nowhere: a closed port) is looked in once, and not
        // again until the accounts change (or 10 minutes pass).
        let settings = S3Settings { bucket: "notes".into(), region: "".into(), folder: "".into(), endpoint: Some("http://127.0.0.1:9/".into()), encrypted: false };
        notes.add_s3_account(None, "Work".into(), S3Checked { settings: settings.clone(), secret: "k".into() });
        p.look_for_vault(notes.clone());
        let first = p.service.last_search();
        assert!(first.is_some());
        wait(|| !p.status().searching);
        assert!(!p.status().found);
        p.look_for_vault(notes.clone());
        assert_eq!(p.service.last_search(), first);
        notes.add_s3_account(None, "Home".into(), S3Checked { settings, secret: "k".into() });
        p.look_for_vault(notes.clone());
        assert_ne!(p.service.last_search(), first);
        wait(|| !p.status().searching);

        // A vault found with an account since removed is forgotten.
        let (alone, _alone_dir) = tests_core();
        let (q, _qdir) = passwords();
        q.service.set_for_test(found(), None);
        q.look_for_vault(alone);
        wait(|| !q.status().searching);
        assert!(!q.status().found);
    }

    /// A notes library with only "On My iPhone" (as `tests::core`).
    fn tests_core() -> (Arc<NotesCore>, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let lib = Library::open(dir.path(), Secrets::Memory, || {}).unwrap();
        (Arc::new(NotesCore { lib: Mutex::new(lib) }), dir)
    }
}

// ---- Tab notes (browser tabs CleanTabs saved) ----------------------------------
//
// A tab note (id `tab-…`, in CleanTabs' hidden account; see
// `cleannotes_core::model::tabs`) shows as a preview of its web page: in the
// list, the page's title, its site in place of the text and its picture; in
// the gallery, the picture as the card; in the note, a card with the page
// above the text. Its editor holds only that text (as the desktop's does,
// with `Editing::for_note`), and saving puts the page back on top, so the
// note keeps CleanTabs' format. A locked tab note isn't one until opened.

use cleannotes_core::editing::{join_tab, split_tab};
use cleannotes_core::model::tabs::{self, TabNote};

/// A tab note's web page.
#[derive(Clone, Debug, PartialEq, uniffi::Record)]
pub struct TabPage {
    pub title: String,
    pub url: String,
    /// The address as shown: without the scheme and `www.`, shortened.
    pub short_url: String,
    /// The site ("example.com"): in the list in place of the text, and on
    /// the card when there's no picture.
    pub site: String,
    /// The page's picture: an attachment's name, for
    /// [`NotesCore::attachment_data`]. None if CleanTabs took none.
    pub thumbnail: Option<String>,
}

impl From<&TabNote> for TabPage {
    fn from(t: &TabNote) -> TabPage {
        TabPage {
            title: t.title.clone(),
            url: t.url.clone(),
            short_url: tabs::short_url(&t.url),
            site: tabs::host(&t.url).unwrap_or_else(|| tabs::short_url(&t.url)),
            thumbnail: t.thumbnail.clone(),
        }
    }
}

/// A tab note's row in the list: the page's title, and its site in place of
/// the first line.
fn tab_row(lib: &Library, it: &mut NoteItem) {
    let Some(n) = lib.store.note(&it.id) else { return };
    if lock::is_locked(&n.body) {
        return;
    }
    let Some(tab) = n.tab() else { return };
    let page = TabPage::from(&tab);
    it.title = if page.title.is_empty() { "New Note".to_owned() } else { page.title.clone() };
    it.empty = page.site.is_empty();
    it.snippet = if it.empty { "No additional text".to_owned() } else { page.site.clone() };
    it.tab = Some(page);
}

/// A tab note as text to share: the page's title and address, then what's
/// written about it.
fn tab_shared_text(tab: &TabNote, text: &Doc) -> String {
    let mut s = format!("{}\n{}", tab.title, tab.url);
    let text = text.shared_text();
    if !text.trim().is_empty() {
        s.push_str("\n\n");
        s.push_str(&text);
    }
    s
}

impl NoteEditor {
    /// The editor for a note: a tab note's page apart, its text to edit.
    fn for_note(id: &str, md: &str) -> Arc<NoteEditor> {
        let (ed, tab) = Editing::for_note(id, md);
        Arc::new(NoteEditor { ed: Mutex::new(ed), tab: Mutex::new(tab) })
    }

    fn tab(&self) -> MutexGuard<'_, Option<TabNote>> {
        self.tab.lock().unwrap_or_else(|e| e.into_inner())
    }
}

#[uniffi::export]
impl NoteEditor {
    /// A tab note's page, shown as a card above the text (the editor has
    /// only the text below it). Ask again after [`NotesCore::reload_editor`].
    pub fn tab_page(&self) -> Option<TabPage> {
        self.tab().as_ref().map(TabPage::from)
    }
}

#[cfg(test)]
mod tab_tests {
    use super::*;
    use cleannotes_core::model::Note;

    const PIC: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa.jpg";

    fn core() -> (Arc<NotesCore>, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let lib = Library::open(dir.path(), Secrets::Memory, || {}).unwrap();
        (Arc::new(NotesCore { lib: Mutex::new(lib) }), dir)
    }

    fn page() -> TabNote {
        TabNote { title: "Rust *is* great".into(), url: "https://www.example.com/rust".into(), thumbnail: Some(PIC.into()), text: "Read this later.\n\n- point".into() }
    }

    /// A note with this id and Markdown in `Reading`: its place.
    fn add(core: &NotesCore, id: &str, body: &str) -> Place {
        let mut lib = core.lib();
        let folder = lib.store.ensure_folder_at(LOCAL, &["Reading".to_owned()]);
        let mut n = Note::new(LOCAL, &folder, body);
        n.id = id.to_owned();
        lib.store.notes.push(n);
        Place::Folder { id: folder }
    }

    fn row(core: &NotesCore, place: &Place, id: &str) -> NoteItem {
        core.sections(place.clone(), String::new()).into_iter().flat_map(|s| s.notes).find(|n| n.id == id).unwrap()
    }

    #[test]
    fn cleantabs_has_a_section_of_its_own_on_the_folders_screen() {
        use cleannotes_core::model::{self, Account};
        let (core, _dir) = core();
        let tabs = {
            let mut lib = core.lib();
            let config = s3::Config { bucket: "b".into(), region: "eu-west-1".into(), prefix: "CleanNotes".into(), endpoint: None, encrypted: false };
            lib.store.accounts.push(Account {
                id: "work".into(),
                name: "Work".into(),
                backend: BackendConfig::S3(config),
                enabled: true,
                cursor: None,
                last_sync: None,
                tabs_of: None,
            });
            lib.store.ensure_local();
            assert!(lib.set_tabs_found("work", true));
            let tabs = model::tabs_account_id("work");
            let research = lib.store.ensure_folder_at(&tabs, &["Research".to_owned()]);
            for (i, title) in ["Rust book", "Rust docs"].iter().enumerate() {
                let body = TabNote { title: (*title).into(), url: format!("https://example.com/{i}"), thumbnail: None, text: String::new() }.to_markdown();
                let mut n = Note::new(&tabs, &research, &body);
                n.id = format!("tab-{i}");
                lib.store.notes.push(n);
            }
            tabs
        };
        let o = core.overview();
        // Not an account of notes, but CleanTabs' section, named for its S3 account.
        assert!(o.accounts.iter().all(|a| a.id != tabs));
        assert_eq!(o.tabs.len(), 1);
        let t = &o.tabs[0];
        assert_eq!((t.id.as_str(), t.account_name.as_str(), t.all_count, t.deleted_count), (tabs.as_str(), "Work", 2, 0));
        assert_eq!(t.folders.iter().map(|f| (f.name.as_str(), f.count)).collect::<Vec<_>>(), [("Research", 2)]);

        let all = Place::Tabs { account: tabs.clone(), folder: None };
        assert_eq!(core.title(all.clone(), String::new()), "All Tabs");
        let shown: Vec<_> = core.sections(all.clone(), String::new()).into_iter().flat_map(|s| s.notes).collect();
        assert_eq!(shown.iter().map(|n| n.title.as_str()).collect::<Vec<_>>(), ["Rust book", "Rust docs"]);
        assert!(shown.iter().all(|n| n.tab.is_some()));
        assert!(core.place_exists(all.clone()));

        // Deleted, it's in CleanTabs' Recently Deleted.
        core.trash_notes(vec!["tab-0".into()], all.clone());
        assert_eq!(core.overview().tabs[0].deleted_count, 1);
        let deleted = Place::TabsDeleted { account: tabs.clone() };
        let ids: Vec<_> = core.sections(deleted, String::new()).into_iter().flat_map(|s| s.notes).map(|n| n.id).collect();
        assert_eq!(ids, ["tab-0"]);
    }

    #[test]
    fn tab_notes_in_the_list_show_their_page() {
        let (core, _dir) = core();
        let body = page().to_markdown();
        let place = add(&core, "tab-1", &body);
        add(&core, "tab-2", &TabNote { thumbnail: None, url: "about:blank".into(), ..page() }.to_markdown());
        add(&core, "6f1c0d3e", &body);
        add(&core, "6f1c0d3f", "# Shopping\neggs\n");

        let r = row(&core, &place, "tab-1");
        assert_eq!((r.title.as_str(), r.snippet.as_str(), r.empty), ("Rust *is* great", "example.com", false));
        let tab = r.tab.unwrap();
        assert_eq!(
            tab,
            TabPage {
                title: "Rust *is* great".into(),
                url: "https://www.example.com/rust".into(),
                short_url: "example.com/rust".into(),
                site: "example.com".into(),
                thumbnail: Some(PIC.into()),
            }
        );
        assert_eq!(core.note("tab-1".into(), place.clone()).unwrap().tab, Some(tab));
        // No picture, no host: the address stands in for the site.
        let r = row(&core, &place, "tab-2");
        assert_eq!((r.snippet.as_str(), r.tab.unwrap().thumbnail), ("about:blank", None));
        // Not made by CleanTabs, or not shaped like one: notes as ever.
        let r = row(&core, &place, "6f1c0d3e");
        assert!(r.tab.is_none());
        assert!(r.snippet.starts_with("example.com/rust"), "{}", r.snippet);
        let r = row(&core, &place, "6f1c0d3f");
        assert_eq!((r.title.as_str(), r.snippet.as_str(), r.tab), ("Shopping", "eggs", None));
        // Searching still shows where the words are.
        let found = core.sections(place, "later".into()).into_iter().flat_map(|s| s.notes).find(|n| n.id == "tab-1").unwrap();
        assert!(found.tab.is_some() && found.snippet.contains("later"), "{found:?}");
    }

    #[test]
    fn editing_a_tab_notes_text_keeps_its_page() {
        let (core, _dir) = core();
        let body = page().to_markdown();
        add(&core, "tab-1", &body);
        let e = core.open_editor("tab-1".into()).unwrap();
        assert_eq!(e.tab_page().map(|p| p.title), Some("Rust *is* great".into()));
        assert_eq!(e.plain_text(), "Read this later.\n\npoint", "only the text is edited");
        assert!(core.save_editor(e.clone()), "nothing changed");
        assert_eq!(core.markdown("tab-1".into()).unwrap(), body);

        // Typing at the start of the text.
        e.select(0, 0);
        assert!(e.replace(0, 0, "Really. ".into()));
        assert!(core.save_editor(e.clone()));
        let saved = core.markdown("tab-1".into()).unwrap();
        assert_eq!(saved, TabNote { text: "Really. Read this later.\n\n- point".into(), ..page() }.to_markdown());
        assert_eq!(saved, body.replace("Read this", "Really. Read this"));
        assert!(saved.contains(&format!("![](attachments/{PIC})")));
        assert_eq!(e.shared_text(), "Rust *is* great\nhttps://www.example.com/rust\n\nReally. Read this later.\n\n– point");
        assert_eq!(core.shared_text("tab-1".into()).unwrap(), e.shared_text());

        // Changed elsewhere: the editor follows, page and text.
        core.lib().set_body("tab-1", &saved.replace("Rust", "Go").replace("Really. ", ""));
        assert!(core.reload_editor(e.clone()));
        assert_eq!(e.tab_page().unwrap().title, "Go *is* great");
        assert_eq!(e.plain_text(), "Read this later.\n\npoint");

        // Emptied: the page stays.
        let n = e.plain_text().encode_utf16().count() as u32;
        e.select(0, n);
        e.replace(0, n, String::new());
        assert!(core.save_editor(e.clone()));
        let tab = TabNote::parse(&core.markdown("tab-1".into()).unwrap()).unwrap();
        assert_eq!((tab.title.as_str(), tab.thumbnail.as_deref(), tab.text.as_str()), ("Go *is* great", Some(PIC), ""));
    }

    #[test]
    fn other_notes_are_edited_whole() {
        let (core, _dir) = core();
        let body = page().to_markdown();
        add(&core, "6f1c0d3e", &body);
        let e = core.open_editor("6f1c0d3e".into()).unwrap();
        assert!(e.tab_page().is_none());
        assert!(e.plain_text().starts_with("Rust *is* great\n\nexample.com/rust\n"), "{}", e.plain_text());
        e.select(0, 0);
        assert!(e.replace(0, 0, "x".into()));
        assert!(core.save_editor(e.clone()));
        let md = core.markdown("6f1c0d3e".into()).unwrap();
        assert!(md.starts_with("# xRust"), "{md}");
        assert_eq!(e.shared_text().lines().next(), Some("xRust *is* great"));
        // A new note isn't a tab note either.
        let n = core.new_note(Place::AllNotes);
        assert!(core.new_note_editor(n.id).tab_page().is_none());
    }

    #[test]
    fn a_locked_tab_note_is_a_tab_note_once_opened() {
        let (core, _dir) = core();
        // (A note can't be locked while its pictures are on their way.)
        let body = TabNote { thumbnail: None, ..page() }.to_markdown();
        let place = add(&core, "tab-1", &body);
        core.set_password("pw".into(), String::new()).unwrap();
        assert_eq!(core.lock_note("tab-1".into()), LockOutcome::Locked);
        core.lock_now();
        let r = row(&core, &place, "tab-1");
        assert!(r.locked && r.tab.is_none(), "{r:?}");
        assert_eq!(r.snippet, "Locked");
        assert!(core.open_editor("tab-1".into()).is_none());

        core.unlock("tab-1".into(), "pw".into()).unwrap();
        assert!(row(&core, &place, "tab-1").tab.is_none(), "the list never shows a locked note's page");
        let e = core.open_editor("tab-1".into()).unwrap();
        assert_eq!(e.tab_page().unwrap().url, "https://www.example.com/rust");
        e.select(0, 0);
        assert!(e.replace(0, 0, "Really. ".into()));
        assert!(core.save_editor(e.clone()));
        assert!(core.is_locked("tab-1".into()), "still locked");
        assert_eq!(core.markdown("tab-1".into()).unwrap(), body.replace("Read this", "Really. Read this"));
        assert!(!core.lib().store.note("tab-1").unwrap().body.contains("example.com"), "stored sealed");
    }
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
    fn notes_come_and_go_from_outside_the_app() {
        let (core, _dir) = core();
        let n = core.create_note(None, "Ideas\nA garden".into());
        assert_eq!(n.place, Place::Folder { id: core.default_folder() });
        core.add_text(n.id.clone(), "A pond".into()).unwrap();
        let found = core.indexed_notes();
        assert_eq!(found.len(), 1);
        assert_eq!((found[0].title.as_str(), found[0].text.as_str()), ("Ideas", "A garden\nA pond"));
        assert!(found[0].modified > 0);
        assert_eq!(core.note_place(n.id.clone()), Some(n.place.clone()));
        core.trash_notes(vec![n.id.clone()], n.place);
        assert!(core.indexed_notes().is_empty());
        assert!(core.add_text(n.id.clone(), "x".into()).is_err());
        assert_eq!(core.note_place(n.id), Some(Place::Deleted { account: local_account_id() }));
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

    #[test]
    fn on_my_iphone_can_be_removed_and_added_back() {
        let (core, _dir) = core();
        let local = |core: &NotesCore| core.overview().accounts.into_iter().find(|a| a.id == local_account_id());
        // Alone, it can't be removed.
        assert!(!local(&core).unwrap().can_remove);
        let s3 = S3Settings { bucket: "notes".into(), region: "".into(), folder: "".into(), endpoint: Some("http://127.0.0.1:9/".into()), encrypted: false };
        let id = core.add_s3_account(None, "Work".into(), S3Checked { settings: s3, secret: "k".into() });
        let home = core.lib().default_folder_id(LOCAL);
        let n = cleannotes_core::model::Note::new(LOCAL, &home, "# Only here\n");
        core.lib().store.notes.push(n);
        let a = local(&core).unwrap();
        assert!(a.can_remove);
        assert_eq!(a.note_count, 1);
        core.remove_account(local_account_id());
        assert!(local(&core).is_none());
        // The S3 account is now the last one.
        assert!(!core.overview().accounts[0].can_remove);
        assert_eq!(core.overview().accounts[0].id, id);
        core.add_local_account();
        let a = local(&core).unwrap();
        assert_eq!(a.note_count, 0);
        assert!(a.can_remove);
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
    fn notes_are_shared_as_text_and_locked_ones_only_when_open() {
        let (core, _dir) = core();
        let home = Place::Folder { id: core.lib().default_folder_id(LOCAL) };
        let n = core.new_note(home);
        core.lib().set_body(&n.id, "# List\n- [ ] eggs\n- [x] milk\n");
        assert_eq!(core.shared_text(n.id.clone()).as_deref(), Some("List\n☐ eggs\n☑ milk"));
        // The open editor shares what's on screen, saved or not.
        let e = core.open_editor(n.id.clone()).unwrap();
        e.command(EditorCommand::SelectAll);
        e.command(EditorCommand::SetStyle { style: ParagraphStyle::Bulleted });
        assert_eq!(e.shared_text(), "• List\n• eggs\n• milk");

        core.set_password("pw".into(), "".into()).unwrap();
        assert_eq!(core.lock_note(n.id.clone()), LockOutcome::Locked);
        core.lock_now();
        assert_eq!(core.shared_text(n.id.clone()), None, "a closed locked note isn't shared");
    }

    /// Types as the iPhone's text view does: each character goes to
    /// `replace` at the caret; plain typing is then applied by the text view,
    /// which reports the caret after it.
    fn type_like_the_text_view(e: &NoteEditor, text: &str) {
        for ch in text.chars() {
            let at = e.state().selection_start;
            let s = ch.to_string();
            if e.replace(at, at, s.clone()) {
                let after = at + s.encode_utf16().count() as u32;
                e.select(after, after);
            }
        }
    }

    #[test]
    fn search_results_come_in_sections_with_matches_marked() {
        let (core, _dir) = core();
        let home = Place::Folder { id: core.default_folder() };
        let a = core.new_note(home.clone()).id;
        core.lib().set_body(&a, "# Milk run\nshop\n");
        let b = core.new_note(home.clone()).id;
        core.lib().set_body(&b, "# Pancakes\nflour and milk\n");
        let sections = core.sections(Place::AllNotes, "milk".into());
        let titles: Vec<&str> = sections.iter().map(|s| s.title.as_str()).collect();
        assert_eq!(titles, ["Top Hits", "Notes"]);
        let top = &sections[0].notes[0];
        assert_eq!(top.id, a);
        assert_eq!(top.title_parts, [TextPart { text: "Milk".into(), matched: true }, TextPart { text: " run".into(), matched: false }]);
        let other = &sections[1].notes[0];
        assert_eq!(other.snippet, "flour and milk");
        assert!(other.snippet_parts.iter().any(|p| p.matched && p.text == "milk"));
        // Not searching: no parts.
        assert!(core.sections(home, String::new()).iter().flat_map(|s| &s.notes).all(|n| n.title_parts.is_empty()));
    }

    #[test]
    fn smart_folders_through_the_bindings() {
        let (core, _dir) = core();
        let home = Place::Folder { id: core.default_folder() };
        let a = core.new_note(home.clone()).id;
        core.lib().set_body(&a, "# Trip #travel\n- [ ] passport\n");
        let b = core.new_note(home).id;
        core.lib().set_body(&b, "# Notes\nplain\n");
        let filter = SmartFilter {
            tags: vec!["#travel ".into(), " ".into()],
            all_tags: false,
            checklists: ChecklistFilter::Off,
            pinned: false,
            locked: false,
            edited_within_days: 0,
        };
        let id = core.add_smart_folder("Travel".into(), filter);
        let ov = core.overview();
        assert_eq!(ov.smart_folders.len(), 1);
        let s = &ov.smart_folders[0];
        assert_eq!((s.name.as_str(), s.count), ("Travel", 1));
        assert_eq!(s.filter.tags, ["travel"], "the # and spaces are taken off");
        let place = Place::Smart { id: id.clone() };
        assert_eq!(core.title(place.clone(), String::new()), "Travel");
        assert_eq!(core.sections(place.clone(), String::new())[0].notes[0].id, a);

        let mut f = s.filter.clone();
        f.tags.clear();
        f.checklists = ChecklistFilter::Unchecked;
        core.update_smart_folder(id.clone(), "To Do".into(), f);
        assert_eq!(core.overview().smart_folders[0].name, "To Do");
        assert_eq!(core.overview().smart_folders[0].count, 1);
        core.delete_smart_folder(id);
        assert!(core.overview().smart_folders.is_empty());
        assert!(!core.place_exists(place));
    }

    #[test]
    fn tables_through_the_bindings() {
        let (core, _dir) = core();
        let e = core.new_note_editor(core.new_note(Place::Folder { id: core.default_folder() }).id);
        type_like_the_text_view(&e, "Plan\n");
        assert!(!e.formatting().in_table);
        e.command(EditorCommand::InsertTable);
        let f = e.formatting();
        assert!(f.in_table && !f.header_row && !f.header_column);
        let cells: Vec<TableCell> = e.state().paragraphs.into_iter().filter_map(|p| p.cell).collect();
        assert_eq!(cells.len(), 9);
        assert_eq!((cells[4].row, cells[4].column, cells[4].rows, cells[4].columns), (1, 1, 3, 3));

        // Typing in a cell, Tab to the next, Return a line break in it.
        type_like_the_text_view(&e, "Day");
        e.command(EditorCommand::NextCell);
        type_like_the_text_view(&e, "What\nWhere");
        let s = e.state();
        let first = s.paragraphs.iter().position(|p| p.cell.is_some()).unwrap();
        assert_eq!(s.paragraphs[first].runs[0].text, "Day");
        assert_eq!(s.paragraphs[first + 1].runs.iter().map(|r| r.text.as_str()).collect::<String>(), "What\nWhere");
        assert_eq!(s.paragraphs.iter().filter(|p| p.cell.is_some()).count(), 9, "Return didn't add a cell");
        e.command(EditorCommand::PreviousCell);
        assert_eq!(e.paragraph_at(e.state().selection_start), first as u32);

        e.command(EditorCommand::ToggleHeaderRow);
        assert!(e.formatting().header_row);
        let headers: Vec<bool> = e.state().paragraphs.into_iter().filter_map(|p| p.cell.map(|c| c.header)).collect();
        assert_eq!(headers, [true, true, true, false, false, false, false, false, false]);

        e.command(EditorCommand::AddColumnAfter);
        assert_eq!(e.state().paragraphs.iter().find_map(|p| p.cell.clone()).unwrap().columns, 4);
        e.command(EditorCommand::DeleteRow);
        assert_eq!(e.state().paragraphs.iter().find_map(|p| p.cell.clone()).unwrap().rows, 2);
        assert_eq!(e.shared_text().lines().nth(1), Some("\t\t\t"), "the header row with Day went: four empty cells");
        e.command(EditorCommand::DeleteTable);
        assert!(e.state().paragraphs.iter().all(|p| p.cell.is_none()));
        assert!(!e.formatting().in_table);
    }

    /// What the Swift tests (ios/CleanNotesTests) expect of the engine.
    #[test]
    fn typing_as_the_text_view_does_makes_lists_as_the_swift_tests_expect() {
        let (core, _dir) = core();
        let home = Place::Folder { id: core.default_folder() };
        let styles = |e: &NoteEditor| e.state().paragraphs.into_iter().map(|p| p.style).collect::<Vec<_>>();

        let e = core.new_note_editor(core.new_note(home.clone()).id);
        type_like_the_text_view(&e, "List\n- eggs\nmilk");
        assert_eq!(styles(&e), [ParagraphStyle::Title, ParagraphStyle::Dashed, ParagraphStyle::Dashed]);
        assert_eq!(e.plain_text(), "List\neggs\nmilk");
        type_like_the_text_view(&e, "\n\n");
        assert_eq!(styles(&e).len(), 4);
        assert_eq!(styles(&e)[3], ParagraphStyle::Body);

        let e = core.new_note_editor(core.new_note(home.clone()).id);
        type_like_the_text_view(&e, "N\n1. one\ntwo\n\n* dot");
        let markers: Vec<String> = e.state().paragraphs.into_iter().map(|p| p.marker).collect();
        assert_eq!(markers, ["", "1.", "2.", "•"]);

        let e = core.new_note_editor(core.new_note(home.clone()).id);
        type_like_the_text_view(&e, "List\n[] eggs");
        assert_eq!(e.shared_text(), "List\n☐ eggs");

        let e = core.new_note_editor(core.new_note(home.clone()).id);
        type_like_the_text_view(&e, "Trip\npassport\ntickets");
        e.select(5, 21);
        e.command(EditorCommand::ToggleChecklist);
        e.command(EditorCommand::CheckAll);
        assert!(e.state().paragraphs[1..].iter().all(|p| p.checked));
        e.command(EditorCommand::DeleteChecked);
        assert_eq!(e.plain_text(), "Trip");

        let e = core.new_note_editor(core.new_note(home).id);
        type_like_the_text_view(&e, "T\n- a");
        e.select(2, 2);
        assert!(!e.replace(1, 2, String::new()), "Backspace at a list item's start isn't plain typing");
        assert_eq!(styles(&e)[1], ParagraphStyle::Body);
        assert_eq!(e.plain_text(), "T\na");
    }

    #[test]
    fn the_core_decides_markers_empty_notes_and_the_default_folder() {
        let e = editor("- [ ] a\n- [x] b\n1. one\n2. two\n* dot\nplain\n");
        let markers: Vec<String> = e.state().paragraphs.into_iter().map(|p| p.marker).collect();
        assert_eq!(markers, ["☐", "☑", "1.", "2.", "•", ""]);
        e.select(0, 0);
        assert!(e.formatting().is_list);
        e.select(30, 30);
        assert!(!e.formatting().is_list, "the plain paragraph");

        let (core, _dir) = core();
        let home = Place::Folder { id: core.lib().default_folder_id(LOCAL) };
        let n = core.new_note(home.clone());
        core.lib().set_body(&n.id, "# Only a title\n");
        let row = core.note(n.id.clone(), home.clone()).unwrap();
        assert!(row.empty && row.snippet == "No additional text");
        core.lib().set_body(&n.id, "# Title\nmore\n");
        assert!(!core.note(n.id.clone(), home).unwrap().empty);

        // A default account that's gone: new notes go to the local Notes folder.
        core.lib().settings.default_account = "gone".into();
        assert_eq!(core.default_folder(), core.lib().default_folder_id(LOCAL));
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
    fn attached_files_are_cards_with_their_view() {
        let (pdf, docx) = (format!("{}.pdf", "a".repeat(64)), format!("{}.docx", "b".repeat(64)));
        let sealed = format!("{}.enc#{}", "c".repeat(64), "d".repeat(64));
        let e = editor(&format!(
            "[trip.pdf](attachments/{pdf})\n[trip.pdf](attachments/{pdf} \"large\")\n[x](attachments/{pdf} \"small\")\n\
             [plan.docx](attachments/{docx} \"large\")\n[Taxes.PDF](attachments/{sealed})\nsee [trip.pdf](attachments/{pdf})\n"
        ));
        let files: Vec<_> = e.state().paragraphs.into_iter().map(|p| p.file.map(|f| (f.reference, f.pdf, f.view))).collect();
        assert_eq!(
            files,
            vec![
                Some((pdf.clone(), true, FileView::Medium)),
                Some((pdf.clone(), true, FileView::Large)),
                Some((pdf.clone(), true, FileView::Small)),
                // Only a PDF shows its pages.
                Some((docx, false, FileView::Small)),
                Some((sealed, true, FileView::Medium)),
                // A link among other text is no card.
                None,
            ]
        );
        assert_eq!(e.paragraph(1).unwrap().file.unwrap().view, FileView::Large);
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
    fn pictures_through_the_bindings() {
        let (core, _dir) = core();
        let home = Place::Folder { id: core.lib().default_folder_id(LOCAL) };
        let n = core.new_note(home);
        let e = core.new_note_editor(n.id.clone());
        e.replace(0, 0, "Trip".into());
        assert!(core.add_image(b"not a picture".to_vec()).is_err());
        let name = core.add_image(b"\x89PNG\r\n\x1a\nphoto".to_vec()).unwrap();
        e.command(EditorCommand::InsertImage { name: name.clone() });
        let s = e.state();
        // One attachment character, a run of its own, one UTF-16 unit.
        let run = &s.paragraphs[1].runs[0];
        assert_eq!((run.text.as_str(), run.image.as_deref()), ("\u{FFFC}", Some(name.as_str())));
        assert_eq!((s.selection_start, s.selection_end), (7, 7));
        assert_eq!(e.paragraph_start(2), 7);
        assert!(core.save_editor(e.clone()));
        assert_eq!(core.markdown(n.id.clone()).unwrap(), format!("# Trip\n![](attachments/{name})\n"));
        let path = core.attachment_path(name.clone()).unwrap();
        assert_eq!(std::fs::read(path).unwrap(), b"\x89PNG\r\n\x1a\nphoto");
        // The list shows it beside the note, and says what's after the title.
        let row = core.note(n.id.clone(), Place::AllNotes).unwrap();
        assert_eq!((row.thumbnail.as_deref(), row.snippet.as_str()), (Some(name.as_str()), "1 photo"));
        core.lib().set_body(&n.id, &format!("# Trip
![](attachments/{name})
Beach
![](attachments/{name})
"));
        let row = core.note(n.id.clone(), Place::AllNotes).unwrap();
        assert_eq!((row.thumbnail.as_deref(), row.snippet.as_str()), (Some(name.as_str()), "Beach"));
        core.lib().set_body(&n.id, &format!("# Trip
![](attachments/{name})
![](attachments/{name})
"));
        assert_eq!(core.note(n.id.clone(), Place::AllNotes).unwrap().snippet, "2 photos");
        // An attached file's link is no picture.
        core.lib().set_body(&n.id, &format!("# Trip
[ticket.pdf](attachments/{name})
"));
        let row = core.note(n.id.clone(), Place::AllNotes).unwrap();
        assert_eq!((row.thumbnail, row.snippet.as_str()), (None, "ticket.pdf"));
        core.lib().set_body(&n.id, &format!("# Trip
![](attachments/{name})
"));
        assert_eq!(core.attachment_path("../x.png".into()), None);
        // Backspace after it takes it out, as typed.
        e.replace(5, 6, String::new());
        assert!(e.state().paragraphs.iter().all(|p| p.runs.iter().all(|r| r.image.is_none())));
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

    #[test]
    fn a_locked_notes_pictures_through_the_bindings() {
        let (core, _dir) = core();
        let home = Place::Folder { id: core.lib().default_folder_id(LOCAL) };
        let n = core.new_note(home);
        core.lib().set_body(&n.id, "# Passport\n");
        core.set_password("pw".into(), String::new()).unwrap();
        assert_eq!(core.lock_note(n.id.clone()), LockOutcome::Locked);
        let photo = b"\x89PNG\r\n\x1a\nscan".to_vec();
        let r = core.add_note_image(n.id.clone(), photo.clone()).unwrap();
        assert!(cleannotes_core::attachments::is_sealed_ref(&r));
        let e = core.open_editor(n.id.clone()).unwrap();
        e.command(EditorCommand::InsertImage { name: r.clone() });
        assert!(core.save_editor(e));
        // Shown from its bytes (decrypted), never handed out as a file.
        assert_eq!(core.attachment_data(r.clone()).unwrap(), photo);
        assert_eq!(core.attachment_path(r), None);
        // A plain note's pictures are plain.
        let plain = core.add_note_image("not-a-locked-note".into(), photo.clone()).unwrap();
        assert!(cleannotes_core::attachments::is_name(&plain));
        assert_eq!(core.attachment_data(plain).unwrap(), photo);
    }
}
