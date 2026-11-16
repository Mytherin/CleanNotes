//! Accounts, folders and notes as the app keeps them, and the in-memory
//! [`Store`] of all of them.
//!
//! As in Apple Notes, every folder belongs to an account: "On My PC" (only on
//! this computer) or a synced account (an S3 bucket, a shared SQLite database,
//! …; see [`crate::sync`]). Each account has a default "Notes" folder, and
//! deleted notes stay in its Recently Deleted for 30 days.

use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};

use crate::sync::{BackendConfig, NoteData};

/// The id of the account that's only on this computer.
pub const LOCAL: &str = "local";
/// The name of every account's default folder.
pub const DEFAULT_FOLDER: &str = "Notes";
/// How long deleted notes stay in Recently Deleted.
pub const KEEP_DELETED_DAYS: i64 = 30;

pub fn local_account_name() -> &'static str {
    if cfg!(target_os = "macos") {
        "On My Mac"
    } else if cfg!(target_os = "ios") {
        "On My iPhone"
    } else {
        "On My PC"
    }
}

pub fn new_id() -> String {
    uuid::Uuid::new_v4().to_string()
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Account {
    pub id: String,
    /// The name in the sidebar ("Work notes", "Shared notes", …).
    pub name: String,
    pub backend: BackendConfig,
    /// Switched off in Settings: its notes stay but aren't shown or synced.
    pub enabled: bool,
    /// The backend's change marker as of the last sync (see
    /// [`crate::sync::Backend::cursor`]).
    #[serde(default)]
    pub cursor: Option<String>,
    #[serde(default)]
    pub last_sync: Option<DateTime<Utc>>,
}

impl Account {
    pub fn local() -> Account {
        Account {
            id: LOCAL.to_owned(),
            name: local_account_name().to_owned(),
            backend: BackendConfig::Local,
            enabled: true,
            cursor: None,
            last_sync: None,
        }
    }

    pub fn synced(&self) -> bool {
        !matches!(self.backend, BackendConfig::Local)
    }
}

/// A folder's sync bookkeeping (synced accounts only).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct FolderSync {
    /// The backend's id for the folder, once it exists there.
    pub remote_id: Option<String>,
    /// The folder's path as the backend last had it; differs from the
    /// current path after a rename here.
    pub synced_path: Option<Vec<String>>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Folder {
    pub id: String,
    pub account: String,
    pub parent: Option<String>,
    pub name: String,
    /// The account's "Notes" folder: can't be renamed or deleted.
    pub default: bool,
    #[serde(default)]
    pub sync: FolderSync,
}

impl Folder {
    pub fn new(account: &str, parent: Option<&str>, name: &str) -> Folder {
        Folder {
            id: new_id(),
            account: account.to_owned(),
            parent: parent.map(str::to_owned),
            name: name.to_owned(),
            default: false,
            sync: FolderSync::default(),
        }
    }

    pub fn default_for(account: &str) -> Folder {
        Folder { default: true, ..Folder::new(account, None, DEFAULT_FOLDER) }
    }
}

/// A note's sync bookkeeping (synced accounts only).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct NoteSync {
    /// The backend's id for the note, once it exists there.
    pub remote_id: Option<String>,
    /// The backend's version of the note as last seen (an update based on
    /// an older one is refused as a conflict).
    pub revision: Option<String>,
    /// The note as last synced: the common ancestor when both sides change
    /// it.
    pub base: Option<NoteData>,
    /// Changed here since the last sync.
    pub dirty: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Note {
    pub id: String,
    pub account: String,
    pub folder: String,
    /// The note's text as Markdown (see [`crate::markdown`]).
    pub body: String,
    pub created: DateTime<Utc>,
    pub modified: DateTime<Utc>,
    pub pinned: bool,
    /// When it was moved to Recently Deleted.
    pub deleted: Option<DateTime<Utc>>,
    #[serde(default)]
    pub sync: NoteSync,
    /// Bumped on every local change (not saved), so a sync that ran while
    /// the note was being edited doesn't overwrite the edit.
    #[serde(skip)]
    pub version: u64,
}

impl Note {
    pub fn new(account: &str, folder: &str, body: &str) -> Note {
        let now = Utc::now();
        Note {
            id: new_id(),
            account: account.to_owned(),
            folder: folder.to_owned(),
            body: body.to_owned(),
            created: now,
            modified: now,
            pinned: false,
            deleted: None,
            sync: NoteSync { dirty: true, ..NoteSync::default() },
            version: 0,
        }
    }

    pub fn title(&self) -> String {
        crate::markdown::title_of(&self.body)
    }

    /// Records a local change.
    pub fn touch(&mut self) {
        self.modified = Utc::now();
        self.version += 1;
        self.sync.dirty = true;
    }

    /// Records a change to how the note is stored that leaves its text as
    /// it was (locking, removing a lock, a new password): saved and synced,
    /// but its last-modified date stays.
    pub fn touch_keep_date(&mut self) {
        self.version += 1;
        self.sync.dirty = true;
    }

    pub fn tags(&self) -> Vec<String> {
        let mut out = Vec::new();
        for line in self.body.lines() {
            for t in crate::doc::tags_in(line) {
                if !out.contains(&t) {
                    out.push(t);
                }
            }
        }
        out
    }
}

/// A note deleted for good here (emptied from Recently Deleted) that still
/// has to be deleted on its backend.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Tombstone {
    pub account: String,
    /// A note (`true`) or a folder (`false`).
    pub note: bool,
    pub remote_id: String,
    /// A folder's path on the backend.
    #[serde(default)]
    pub path: Vec<String>,
    /// A note's revision when it was last synced: if it changed on the
    /// backend since (edited on another device), it isn't deleted there.
    #[serde(default)]
    pub revision: Option<String>,
}

#[derive(Clone, Debug, Default)]
pub struct Store {
    pub accounts: Vec<Account>,
    pub folders: Vec<Folder>,
    pub notes: Vec<Note>,
    pub tombstones: Vec<Tombstone>,
}

impl Store {
    /// A store with just the local account and its Notes folder.
    pub fn new_install() -> Store {
        let mut s = Store::default();
        s.ensure_local();
        s
    }

    /// Keeps the notes of an account whose kind of sync is no longer
    /// supported: its folders and notes move into the local account, under
    /// a folder with the account's name (its default folder, renamed), with
    /// nothing left to sync. Nothing on the service itself is touched.
    pub fn retire_account(&mut self, id: &str, name: &str) {
        self.ensure_local();
        let top = match self.folders.iter().find(|f| f.account == id && f.default) {
            Some(f) => f.id.clone(),
            None => {
                let f = Folder::new(id, None, name);
                let fid = f.id.clone();
                self.folders.push(f);
                fid
            }
        };
        // A name not already taken at the top of the local account.
        let taken: Vec<String> = self.folders.iter().filter(|f| f.account == LOCAL && f.parent.is_none()).map(|f| f.name.clone()).collect();
        let base = if name.trim().is_empty() { "Imported" } else { name.trim() };
        let mut unique = base.to_owned();
        let mut n = 2;
        while taken.contains(&unique) {
            unique = format!("{base} {n}");
            n += 1;
        }
        for f in self.folders.iter_mut().filter(|f| f.account == id) {
            f.account = LOCAL.to_owned();
            f.sync = Default::default();
            if f.id == top {
                f.default = false;
                f.name = unique.clone();
                f.parent = None;
            } else if f.parent.is_none() {
                f.parent = Some(top.clone());
            }
        }
        for n in self.notes.iter_mut().filter(|n| n.account == id) {
            n.account = LOCAL.to_owned();
            n.sync = Default::default();
            if !self.folders.iter().any(|f| f.id == n.folder) {
                n.folder = top.clone();
            }
        }
        self.tombstones.retain(|t| t.account != id);
        self.accounts.retain(|a| a.id != id);
    }

    /// Makes sure the local account and every account's default folder exist.
    pub fn ensure_local(&mut self) {
        if !self.accounts.iter().any(|a| a.id == LOCAL) {
            self.accounts.insert(0, Account::local());
        }
        for a in self.accounts.clone() {
            if !self.folders.iter().any(|f| f.account == a.id && f.default) {
                self.folders.push(Folder::default_for(&a.id));
            }
        }
    }

    pub fn account(&self, id: &str) -> Option<&Account> {
        self.accounts.iter().find(|a| a.id == id)
    }

    pub fn account_mut(&mut self, id: &str) -> Option<&mut Account> {
        self.accounts.iter_mut().find(|a| a.id == id)
    }

    pub fn folder(&self, id: &str) -> Option<&Folder> {
        self.folders.iter().find(|f| f.id == id)
    }

    pub fn folder_mut(&mut self, id: &str) -> Option<&mut Folder> {
        self.folders.iter_mut().find(|f| f.id == id)
    }

    pub fn note(&self, id: &str) -> Option<&Note> {
        self.notes.iter().find(|n| n.id == id)
    }

    pub fn note_mut(&mut self, id: &str) -> Option<&mut Note> {
        self.notes.iter_mut().find(|n| n.id == id)
    }

    pub fn default_folder(&self, account: &str) -> Option<&Folder> {
        self.folders.iter().find(|f| f.account == account && f.default)
    }

    /// A folder's path from its account's top level, e.g. `["Work", "2026"]`.
    pub fn folder_path(&self, id: &str) -> Vec<String> {
        let mut path = Vec::new();
        let mut cur = self.folder(id);
        let mut guard = 0;
        while let Some(f) = cur {
            path.push(f.name.clone());
            cur = f.parent.as_deref().and_then(|p| self.folder(p));
            guard += 1;
            if guard > 64 {
                break;
            }
        }
        path.reverse();
        path
    }

    /// The folder at a path in an account.
    pub fn folder_at(&self, account: &str, path: &[String]) -> Option<&Folder> {
        let mut parent: Option<&str> = None;
        let mut found = None;
        for name in path {
            let f = self.folders.iter().find(|f| f.account == account && f.parent.as_deref() == parent && &f.name == name)?;
            parent = Some(&f.id);
            found = Some(f);
        }
        found
    }

    /// The folder at a path, created (with any missing parents) if needed.
    pub fn ensure_folder_at(&mut self, account: &str, path: &[String]) -> String {
        let mut parent: Option<String> = None;
        for name in path {
            let existing = self
                .folders
                .iter()
                .find(|f| f.account == account && f.parent == parent && &f.name == name)
                .map(|f| f.id.clone());
            let id = match existing {
                Some(id) => id,
                None => {
                    let f = Folder::new(account, parent.as_deref(), name);
                    let id = f.id.clone();
                    self.folders.push(f);
                    id
                }
            };
            parent = Some(id);
        }
        match parent {
            Some(id) => id,
            None => self.default_folder(account).map(|f| f.id.clone()).unwrap_or_default(),
        }
    }

    /// The folder and every folder inside it.
    pub fn folder_and_descendants(&self, id: &str) -> Vec<String> {
        let mut out = vec![id.to_owned()];
        let mut i = 0;
        while i < out.len() {
            let cur = out[i].clone();
            out.extend(self.folders.iter().filter(|f| f.parent.as_deref() == Some(&cur)).map(|f| f.id.clone()));
            i += 1;
        }
        out
    }

    /// Folders of an account in sidebar order: depth first, by name, the
    /// default folder first. Returns (folder, depth).
    pub fn folder_tree(&self, account: &str) -> Vec<(&Folder, usize)> {
        let mut out = Vec::new();
        self.walk(account, None, 0, &mut out);
        out
    }

    fn walk<'a>(&'a self, account: &str, parent: Option<&str>, depth: usize, out: &mut Vec<(&'a Folder, usize)>) {
        let mut kids: Vec<&Folder> =
            self.folders.iter().filter(|f| f.account == account && f.parent.as_deref() == parent).collect();
        kids.sort_by(|a, b| b.default.cmp(&a.default).then_with(|| natural_cmp(&a.name, &b.name)));
        for f in kids {
            out.push((f, depth));
            if depth < 32 {
                self.walk(account, Some(&f.id), depth + 1, out);
            }
        }
    }

    /// A folder name not yet used among `parent`'s children: "New Folder",
    /// "New Folder 2", …
    pub fn unique_folder_name(&self, account: &str, parent: Option<&str>, base: &str) -> String {
        let taken = |n: &str| {
            self.folders
                .iter()
                .any(|f| f.account == account && f.parent.as_deref() == parent && f.name.eq_ignore_ascii_case(n))
        };
        if !taken(base) {
            return base.to_owned();
        }
        (2..).map(|i| format!("{base} {i}")).find(|n| !taken(n)).unwrap()
    }

    /// Notes deleted more than 30 days ago, which should go for good.
    pub fn expired(&self, now: DateTime<Utc>) -> Vec<String> {
        self.notes
            .iter()
            .filter(|n| n.deleted.is_some_and(|d| now - d > Duration::days(KEEP_DELETED_DAYS)))
            .map(|n| n.id.clone())
            .collect()
    }

    /// Deletes a note for good, leaving a tombstone if its backend has it.
    pub fn purge_note(&mut self, id: &str) -> Option<Note> {
        let i = self.notes.iter().position(|n| n.id == id)?;
        let n = self.notes.remove(i);
        if let Some(remote_id) = &n.sync.remote_id {
            self.tombstones.push(Tombstone {
                account: n.account.clone(),
                note: true,
                remote_id: remote_id.clone(),
                path: vec![],
                revision: n.sync.revision.clone(),
            });
        }
        Some(n)
    }

    /// The note as the backend sees it.
    pub fn note_data(&self, n: &Note) -> NoteData {
        let folder = if self.folder(&n.folder).is_some() {
            self.folder_path(&n.folder)
        } else {
            vec![DEFAULT_FOLDER.to_owned()]
        };
        NoteData {
            id: n.id.clone(),
            folder,
            body: n.body.clone(),
            created: n.created,
            modified: n.modified,
            pinned: n.pinned,
            deleted: n.deleted,
        }
    }
}

/// Compares names the way Finder does: case-insensitively, numbers by value
/// ("Folder 2" before "Folder 10").
pub fn natural_cmp(a: &str, b: &str) -> std::cmp::Ordering {
    let (mut a, mut b) = (a.chars().peekable(), b.chars().peekable());
    loop {
        match (a.peek().copied(), b.peek().copied()) {
            (None, None) => return std::cmp::Ordering::Equal,
            (None, _) => return std::cmp::Ordering::Less,
            (_, None) => return std::cmp::Ordering::Greater,
            (Some(x), Some(y)) if x.is_ascii_digit() && y.is_ascii_digit() => {
                let mut na = String::new();
                while let Some(c) = a.peek().copied().filter(char::is_ascii_digit) {
                    na.push(c);
                    a.next();
                }
                let mut nb = String::new();
                while let Some(c) = b.peek().copied().filter(char::is_ascii_digit) {
                    nb.push(c);
                    b.next();
                }
                let ord = na.trim_start_matches('0').len().cmp(&nb.trim_start_matches('0').len()).then(na.cmp(&nb));
                if ord.is_ne() {
                    return ord;
                }
            }
            (Some(x), Some(y)) => {
                let ord = x.to_lowercase().cmp(y.to_lowercase());
                if ord.is_ne() {
                    return ord;
                }
                a.next();
                b.next();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn folder_paths() {
        let mut s = Store::new_install();
        let id = s.ensure_folder_at(LOCAL, &["Work".into(), "2026".into()]);
        assert_eq!(s.folder_path(&id), vec!["Work", "2026"]);
        assert_eq!(s.ensure_folder_at(LOCAL, &["Work".into(), "2026".into()]), id);
        assert_eq!(s.folder_at(LOCAL, &["Work".into()]).unwrap().name, "Work");
        let work = s.folder_at(LOCAL, &["Work".into()]).unwrap().id.clone();
        assert_eq!(s.folder_and_descendants(&work).len(), 2);
        let names: Vec<_> = s.folder_tree(LOCAL).iter().map(|(f, d)| (f.name.clone(), *d)).collect();
        assert_eq!(names, vec![("Notes".into(), 0), ("Work".into(), 0), ("2026".into(), 1)]);
    }

    #[test]
    fn unique_names() {
        let mut s = Store::new_install();
        s.folders.push(Folder::new(LOCAL, None, "New Folder"));
        assert_eq!(s.unique_folder_name(LOCAL, None, "New Folder"), "New Folder 2");
        assert_eq!(s.unique_folder_name(LOCAL, None, "Other"), "Other");
    }

    #[test]
    fn natural_order() {
        let mut v = vec!["Folder 10", "folder 2", "Apple", "Folder 1"];
        v.sort_by(|a, b| natural_cmp(a, b));
        assert_eq!(v, ["Apple", "Folder 1", "folder 2", "Folder 10"]);
    }

    #[test]
    fn purging_a_synced_note_leaves_a_tombstone() {
        let mut s = Store::new_install();
        let folder = s.default_folder(LOCAL).unwrap().id.clone();
        let mut n = Note::new(LOCAL, &folder, "x");
        n.sync.remote_id = Some("r1".into());
        let id = n.id.clone();
        s.notes.push(n);
        s.purge_note(&id);
        assert!(s.notes.is_empty());
        assert_eq!(s.tombstones.len(), 1);
    }
}
