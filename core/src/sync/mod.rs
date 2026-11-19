//! Syncing accounts with where their notes are kept.
//!
//! A backend ([`Backend`]) only stores folders, notes and attachments (the
//! images notes show, see [`crate::attachments`]) and lists them back;
//! how it writes them down is its own business (the S3 backend writes each
//! note as a Markdown file with YAML front matter, the SQLite backend as
//! rows in tables). Everything else (what to upload, conflicts, merging two
//! devices' edits, edits made while a sync runs) is shared, in
//! [`engine`], and runs on a background thread ([`worker`]).
//!
//! To add a backend: implement [`Backend`], add a [`BackendConfig`] variant,
//! and open it in [`open`].

use serde::{Deserialize, Serialize};

pub mod engine;
pub mod merge;
pub mod s3;
pub mod sqlite;
pub mod worker;
#[cfg(test)]
pub mod test_http;

/// Which backend an account syncs with, and its settings (secrets are kept
/// in the OS credential store, not here).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum BackendConfig {
    /// Not synced ("On My PC").
    Local,
    /// A SQLite database file, e.g. on a network share or in a synced folder.
    Sqlite(sqlite::Config),
    /// Markdown files in a folder of an S3 bucket.
    S3(s3::Config),
}

impl BackendConfig {
    pub fn kind_name(&self) -> &'static str {
        match self {
            BackendConfig::Local => "Local",
            BackendConfig::Sqlite(_) => "SQLite",
            BackendConfig::S3(_) => "S3",
        }
    }
}

/// A note as a backend stores it (shared with CleanTabs, which writes
/// notes into S3 folders too).
pub use cleanbucket::notes::NoteData;

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
    /// The folder's version, for a backend that has them (see
    /// [`Backend::known`]).
    pub revision: Option<String>,
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
    /// Deleting one that's gone already succeeds. With a `base_revision`,
    /// a note that changed since (edited elsewhere) is left alone and
    /// [`Conflict`] returned: an edit beats a delete.
    fn delete_note(&mut self, remote_id: &str, base_revision: Option<&str>) -> anyhow::Result<()>;

    /// Whether the backend keeps the attachment (see [`crate::attachments`]),
    /// as of the last [`Backend::pull`] or uploaded since. Cheap: the
    /// engine asks it for every image of every note it uploads.
    fn has_attachment(&mut self, name: &str) -> anyhow::Result<bool>;

    /// Stores an attachment. Attachments never change (they're named by
    /// their content), so storing one that's there already succeeds.
    fn put_attachment(&mut self, name: &str, data: &[u8]) -> anyhow::Result<()>;

    /// An attachment's bytes, `None` if the backend doesn't have it.
    fn get_attachment(&mut self, name: &str) -> anyhow::Result<Option<Vec<u8>>>;

    /// The notes and folders this device has synced, as (remote id,
    /// revision, the note or folder path at that revision), from the saved
    /// store; given before pulling. A backend may take them as already
    /// downloaded, so a newly started app fetches only what changed.
    fn known(&mut self, _notes: &[(&str, &str, &NoteData)], _folders: &[(&str, &str, &[String])]) {}

    /// Called at the end of every sync that got past the cursor check,
    /// also one that failed, to finish work the backend put off (such as
    /// marking for other devices that something changed).
    fn finish(&mut self) -> anyhow::Result<()> {
        Ok(())
    }

    /// Credentials the backend renewed since last asked, to be saved in
    /// place of the old ones.
    fn renewed_secret(&mut self) -> Option<String> {
        None
    }

    /// Called with the new secret as soon as the backend renews its
    /// credentials, mid-sync. A server that rotates refresh tokens has made
    /// the saved ones useless by then: waiting for the sync to end would
    /// lose them if the app quit first.
    fn on_renewed(&mut self, _f: OnRenewed) {}
}

/// See [`Backend::on_renewed`].
pub type OnRenewed = Box<dyn FnMut(String) + Send>;

/// Opens an account's backend. `secret` is what the account keeps in the
/// credential store (its format is the backend's own).
pub fn open(config: &BackendConfig, secret: Option<String>) -> anyhow::Result<Box<dyn Backend>> {
    match config {
        BackendConfig::Local => anyhow::bail!("the local account isn't synced"),
        BackendConfig::Sqlite(c) => Ok(Box::new(sqlite::SqliteBackend::open(c)?)),
        BackendConfig::S3(c) => Ok(Box::new(s3::S3Backend::open(c, secret)?)),
    }
}
