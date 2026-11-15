//! Syncing accounts with where their notes are kept.
//!
//! A backend ([`Backend`]) only stores folders and notes and lists them back;
//! how it writes them down is its own business (the Bitwarden backend writes
//! each note as a secure note with YAML front matter, the SQLite backend as
//! rows in tables). Everything else (what to upload, conflicts, merging two
//! devices' edits, edits made while a sync runs) is shared, in
//! [`engine`], and runs on a background thread ([`worker`]).
//!
//! To add a backend: implement [`Backend`], add a [`BackendConfig`] variant,
//! and open it in [`open`].

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

pub mod bitwarden;
pub mod cloudkit;
pub mod engine;
pub mod merge;
pub mod sqlite;
pub mod standardnotes;
pub mod worker;
#[cfg(test)]
pub mod test_http;

/// Which backend an account syncs with, and its settings (secrets are kept
/// in the OS credential store, not here).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum BackendConfig {
    /// Not synced ("On My PC").
    Local,
    Bitwarden(bitwarden::Config),
    /// A SQLite database file, e.g. on a network share or in a synced folder.
    Sqlite(sqlite::Config),
    /// Records in a CloudKit container's private database.
    CloudKit(cloudkit::Config),
    /// Encrypted items in a Standard Notes account.
    StandardNotes(standardnotes::Config),
}

impl BackendConfig {
    pub fn kind_name(&self) -> &'static str {
        match self {
            BackendConfig::Local => "Local",
            BackendConfig::Bitwarden(_) => "Bitwarden",
            BackendConfig::Sqlite(_) => "SQLite",
            BackendConfig::CloudKit(_) => "CloudKit",
            BackendConfig::StandardNotes(_) => "Standard Notes",
        }
    }
}

/// A note as a backend stores it. Folder are given by path from the top of
/// the account; the default folder is `["Notes"]`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct NoteData {
    /// The note's id in the app (a UUID). A note a backend didn't get from
    /// the app (say, one typed into Bitwarden directly) gets one made up
    /// from its remote id.
    pub id: String,
    pub folder: Vec<String>,
    /// Markdown.
    pub body: String,
    pub created: DateTime<Utc>,
    pub modified: DateTime<Utc>,
    pub pinned: bool,
    /// In Recently Deleted since then.
    pub deleted: Option<DateTime<Utc>>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct RemoteNote {
    /// The backend's id for the note.
    pub remote_id: String,
    /// The backend's version tag: changes whenever the note does.
    pub revision: String,
    pub data: NoteData,
}

#[derive(Clone, Debug, PartialEq)]
pub struct RemoteFolder {
    pub remote_id: String,
    pub path: Vec<String>,
}

/// Everything an account holds on its backend.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Snapshot {
    pub folders: Vec<RemoteFolder>,
    pub notes: Vec<RemoteNote>,
    /// Remote ids of notes and folders the backend has but couldn't read
    /// (undecryptable, written by a newer client, …). They're left alone
    /// locally instead of being taken as deleted.
    pub unreadable: Vec<String>,
}

/// An update or delete was refused because the note changed on the backend
/// since `base_revision` (someone else saved it first). Returned inside an
/// [`anyhow::Error`]; check with `err.is::<Conflict>()`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Conflict;

impl std::fmt::Display for Conflict {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("the note was changed elsewhere")
    }
}

impl std::error::Error for Conflict {}

/// Signing in again is needed (the saved credentials stopped working).
/// Returned inside an [`anyhow::Error`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NeedsSignIn(pub String);

impl std::fmt::Display for NeedsSignIn {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "sign in again: {}", self.0)
    }
}

impl std::error::Error for NeedsSignIn {}

/// Where an account's notes are kept.
///
/// Calls block (they run on the sync thread). Remote ids and revisions are
/// opaque to the rest of the app.
pub trait Backend: Send {
    /// A cheap marker that changes whenever anything in the account does, so
    /// a sync with nothing to upload can skip [`Backend::pull`]. `None` if
    /// the backend has no such thing.
    fn cursor(&mut self) -> anyhow::Result<Option<String>>;

    /// All folders and notes (including notes in Recently Deleted).
    fn pull(&mut self) -> anyhow::Result<Snapshot>;

    /// Creates a folder (its parents exist already). Returns its remote id.
    /// If the folder exists already (another device made it), returns that
    /// one's id.
    fn create_folder(&mut self, path: &[String]) -> anyhow::Result<String>;

    /// Renames or moves a folder. Folders inside it move along (a backend
    /// that stores full paths renames them too).
    fn rename_folder(&mut self, remote_id: &str, old_path: &[String], new_path: &[String]) -> anyhow::Result<()>;

    /// Deletes an empty folder. Deleting one that's gone already succeeds.
    fn delete_folder(&mut self, remote_id: &str, path: &[String]) -> anyhow::Result<()>;

    /// Creates the note (`remote_id` `None`) or updates it. An update whose
    /// `base_revision` isn't the current one fails with [`Conflict`]. A
    /// create retried after a lost reply may duplicate the note on the
    /// backend; the engine matches it up by [`NoteData::id`] on the next
    /// pull. Returns the remote id and the new revision.
    fn save_note(&mut self, remote_id: Option<&str>, base_revision: Option<&str>, note: &NoteData)
        -> anyhow::Result<(String, String)>;

    /// Deletes a note for good (a backend with a trash may move it there).
    /// Deleting one that's gone already succeeds.
    fn delete_note(&mut self, remote_id: &str) -> anyhow::Result<()>;

    /// Credentials the backend renewed since last asked, to be saved in
    /// place of the old ones.
    fn renewed_secret(&mut self) -> Option<String> {
        None
    }
}

/// Opens an account's backend. `secret` is what the account keeps in the
/// credential store (its format is the backend's own).
pub fn open(config: &BackendConfig, secret: Option<String>) -> anyhow::Result<Box<dyn Backend>> {
    match config {
        BackendConfig::Local => anyhow::bail!("the local account isn't synced"),
        BackendConfig::Bitwarden(c) => Ok(Box::new(bitwarden::BitwardenBackend::open(c, secret)?)),
        BackendConfig::Sqlite(c) => Ok(Box::new(sqlite::SqliteBackend::open(c)?)),
        BackendConfig::CloudKit(c) => Ok(Box::new(cloudkit::CloudKitBackend::open(c, secret)?)),
        BackendConfig::StandardNotes(c) => Ok(Box::new(standardnotes::StandardNotesBackend::open(c, secret)?)),
    }
}
