//! The sync algorithm, shared by all backends.
//!
//! A sync works on a copy of one account ([`AccountSnapshot`]) on the sync
//! thread and returns what the account should look like afterwards
//! ([`Outcome`]); the UI thread then [`apply`]s it, merging in any edits made
//! while the sync ran.
//!
//! One sync:
//! 1. If nothing changed here and the backend's [`Backend::cursor`] is what
//!    it was, stop.
//! 2. Delete notes emptied from Recently Deleted; rename and create folders.
//! 3. Pull everything and reconcile:
//!    - a note changed only there is taken; changed only here, kept;
//!    - changed on both: merged paragraph by paragraph, pins, folders and
//!      deletion field by field (ours wins a clash). If both changed the same
//!      paragraph, ours is kept and theirs becomes a separate note, so
//!      nothing is lost;
//!    - deleted there but edited here: ours is put back (an edit beats a
//!      delete); deleted there and not edited here: deleted here.
//! 4. Upload what changed here, each update based on the revision last
//!    seen. If one is refused because it changed in the meantime, pull and
//!    reconcile again (at most a few rounds).
//! 5. Delete folders deleted here.

use std::collections::{HashMap, HashSet};

use anyhow::Result;
use chrono::{DateTime, Utc};

use super::merge::{merge_text, merge_value};
use super::{Backend, Conflict, RemoteNote, Snapshot};
#[cfg(test)]
use super::NoteData;
use crate::model::{new_id, Folder, Note, NoteSync, Store, Tombstone};

const MAX_ROUNDS: usize = 4;

/// One account's state at the start of a sync.
#[derive(Clone, Debug, Default)]
pub struct AccountSnapshot {
    pub account: String,
    pub cursor: Option<String>,
    pub folders: Vec<Folder>,
    pub notes: Vec<Note>,
    pub tombstones: Vec<Tombstone>,
}

impl AccountSnapshot {
    pub fn of(store: &Store, account: &str) -> AccountSnapshot {
        AccountSnapshot {
            account: account.to_owned(),
            cursor: store.account(account).and_then(|a| a.cursor.clone()),
            folders: store.folders.iter().filter(|f| f.account == account).cloned().collect(),
            notes: store.notes.iter().filter(|n| n.account == account).cloned().collect(),
            tombstones: store.tombstones.iter().filter(|t| t.account == account).cloned().collect(),
        }
    }

    fn as_store(&self) -> Store {
        Store { accounts: vec![], folders: self.folders.clone(), notes: self.notes.clone(), tombstones: vec![] }
    }

    /// Anything to upload.
    pub fn has_local_changes(&self) -> bool {
        let s = self.as_store();
        !self.tombstones.is_empty()
            || self.notes.iter().any(|n| n.sync.dirty || n.sync.remote_id.is_none())
            || self
                .folders
                .iter()
                .any(|f| f.sync.remote_id.is_none() || f.sync.synced_path.as_ref() != Some(&s.folder_path(&f.id)))
    }
}

/// What a sync made of the account.
#[derive(Clone, Debug, Default)]
pub struct Outcome {
    pub account: String,
    /// Nothing changed on either side; the rest is empty.
    pub unchanged: bool,
    pub cursor: Option<String>,
    /// The account's folders and notes after the sync.
    pub folders: Vec<Folder>,
    pub notes: Vec<Note>,
    /// Tombstones that were carried out.
    pub tombstones_done: Vec<Tombstone>,
    /// Things to tell the user (both versions of a note kept, …).
    pub notices: Vec<String>,
    /// Why the sync stopped early, if it did. What was done until then is in
    /// the outcome.
    pub error: Option<String>,
}

pub fn sync_account(backend: &mut dyn Backend, snap: &AccountSnapshot, now: DateTime<Utc>) -> Result<Outcome> {
    let mut out = Outcome { account: snap.account.clone(), ..Outcome::default() };
    if !snap.has_local_changes() && snap.cursor.is_some() {
        let cursor = backend.cursor()?;
        if cursor.is_some() && cursor == snap.cursor {
            out.unchanged = true;
            out.cursor = cursor;
            return Ok(out);
        }
    }
    let mut w = snap.as_store();
    let account = snap.account.as_str();

    // Notes emptied from Recently Deleted go first, so a pull doesn't bring
    // them back.
    for t in snap.tombstones.iter().filter(|t| t.note) {
        backend.delete_note(&t.remote_id)?;
        out.tombstones_done.push(t.clone());
    }
    push_folders(backend, &mut w)?;
    let deleting: HashSet<&str> =
        snap.tombstones.iter().filter(|t| !t.note).map(|t| t.remote_id.as_str()).collect();

    let mut finished = false;
    for _ in 0..MAX_ROUNDS {
        let cursor_before = backend.cursor()?;
        let remote = backend.pull()?;
        reconcile(&mut w, account, remote, &deleting, now, &mut out.notices);
        match push_notes(backend, &mut w) {
            Ok(false) => {
                // Nothing else changed while we uploaded: the cursor now
                // includes our own writes.
                out.cursor = backend.cursor()?.filter(|_| cursor_before.is_some());
                finished = true;
                break;
            }
            Ok(true) => continue,
            Err(e) => {
                out.error = Some(format!("{e:#}"));
                finished = true;
                break;
            }
        }
    }
    if !finished {
        out.error = Some("notes kept changing during the sync; will try again".into());
    }

    if out.error.is_none() {
        // Deepest folders first.
        let mut folders: Vec<&Tombstone> = snap.tombstones.iter().filter(|t| !t.note).collect();
        folders.sort_by_key(|t| std::cmp::Reverse(t.path.len()));
        for t in folders {
            backend.delete_folder(&t.remote_id, &t.path)?;
            out.tombstones_done.push(t.clone());
        }
        if !out.tombstones_done.is_empty() || snap.tombstones.iter().any(|t| !t.note) {
            out.cursor = backend.cursor()?;
        }
    } else {
        out.cursor = None;
    }
    out.folders = w.folders;
    out.notes = w.notes;
    Ok(out)
}

/// Uploads folder renames and new folders, parents first.
fn push_folders(backend: &mut dyn Backend, w: &mut Store) -> Result<()> {
    let mut order: Vec<(usize, String)> = w.folders.iter().map(|f| (w.folder_path(&f.id).len(), f.id.clone())).collect();
    order.sort();
    for (_, id) in &order {
        let path = w.folder_path(id);
        let f = w.folder(id).unwrap();
        let (Some(remote_id), Some(old)) = (f.sync.remote_id.clone(), f.sync.synced_path.clone()) else {
            continue;
        };
        if old == path {
            continue;
        }
        backend.rename_folder(&remote_id, &old, &path)?;
        // Folders inside it moved along on the backend.
        for d in w.folder_and_descendants(id) {
            let new_path = w.folder_path(&d);
            let f = w.folder_mut(&d).unwrap();
            if let Some(sp) = &mut f.sync.synced_path {
                if sp.starts_with(&old) {
                    let rest = sp[old.len()..].to_vec();
                    *sp = path.iter().cloned().chain(rest).collect();
                }
            }
            if d == *id {
                f.sync.synced_path = Some(new_path);
            }
        }
    }
    for (_, id) in &order {
        if w.folder(id).unwrap().sync.remote_id.is_some() {
            continue;
        }
        let path = w.folder_path(id);
        let remote_id = backend.create_folder(&path)?;
        let f = w.folder_mut(id).unwrap();
        f.sync.remote_id = Some(remote_id);
        f.sync.synced_path = Some(path);
    }
    Ok(())
}

fn reconcile(
    w: &mut Store,
    account: &str,
    remote: Snapshot,
    deleting: &HashSet<&str>,
    now: DateTime<Utc>,
    notices: &mut Vec<String>,
) {
    // ---- Folders --------------------------------------------------------
    let remote_folders: HashMap<&str, &Vec<String>> =
        remote.folders.iter().map(|f| (f.remote_id.as_str(), &f.path)).collect();
    let unreadable: HashSet<&str> = remote.unreadable.iter().map(String::as_str).collect();
    // Folders deleted on the backend.
    let gone: Vec<String> = w
        .folders
        .iter()
        .filter(|f| f.sync.remote_id.as_deref().is_some_and(|r| !remote_folders.contains_key(r) && !unreadable.contains(r)))
        .map(|f| f.id.clone())
        .collect();
    let default_id = w.default_folder(account).map(|f| f.id.clone()).unwrap_or_default();
    for id in gone {
        if id == default_id {
            // The default folder always exists; it's made again.
            w.folder_mut(&id).unwrap().sync = Default::default();
            continue;
        }
        if w.folder(&id).is_none() {
            continue;
        }
        let parent = w.folder(&id).and_then(|f| f.parent.clone());
        // Folders inside it that the backend still has move up; notes in it
        // that only exist here go to the default folder. (Notes the backend
        // has carry their own folder.)
        for f in w.folders.iter_mut().filter(|f| f.parent.as_deref() == Some(&id)) {
            f.parent = parent.clone();
        }
        for n in w.notes.iter_mut().filter(|n| n.folder == id) {
            n.folder = default_id.clone();
        }
        w.folders.retain(|f| f.id != id);
    }
    // Folders new or renamed on the backend, parents first.
    let mut rf: Vec<_> = remote.folders.iter().filter(|f| !deleting.contains(f.remote_id.as_str())).collect();
    rf.sort_by_key(|f| f.path.len());
    for r in rf {
        if r.path.is_empty() {
            continue;
        }
        let known = w.folders.iter().find(|f| f.sync.remote_id.as_deref() == Some(&r.remote_id)).map(|f| f.id.clone());
        match known {
            Some(id) => {
                if w.folder_path(&id) != r.path {
                    let parent_path = &r.path[..r.path.len() - 1];
                    let parent = if parent_path.is_empty() { None } else { Some(w.ensure_folder_at(account, parent_path)) };
                    let f = w.folder_mut(&id).unwrap();
                    if !f.default {
                        f.name = r.path.last().unwrap().clone();
                        f.parent = parent;
                    }
                }
                w.folder_mut(&id).unwrap().sync.synced_path = Some(r.path.clone());
            }
            None => {
                let id = w.ensure_folder_at(account, &r.path);
                let f = w.folder_mut(&id).unwrap();
                f.sync.remote_id = Some(r.remote_id.clone());
                f.sync.synced_path = Some(r.path.clone());
            }
        }
    }

    // ---- Notes ----------------------------------------------------------
    let by_remote: HashMap<&str, &RemoteNote> = remote.notes.iter().map(|n| (n.remote_id.as_str(), n)).collect();
    let mut by_id: HashMap<&str, &RemoteNote> = HashMap::new();
    for n in &remote.notes {
        by_id.entry(n.data.id.as_str()).or_insert(n);
    }
    let mut matched: HashSet<String> = HashSet::new();
    let mut extra_notes: Vec<Note> = Vec::new();
    let mut remove: Vec<String> = Vec::new();
    for i in 0..w.notes.len() {
        let note = &w.notes[i];
        let found = match note.sync.remote_id.as_deref() {
            Some(rid) => by_remote.get(rid).copied(),
            // A create whose reply was lost: the backend has it under our id.
            None => by_id.get(note.id.as_str()).copied().filter(|r| !matched.contains(&r.remote_id)),
        };
        let Some(r) = found else {
            if note.sync.remote_id.as_deref().is_some_and(|r| unreadable.contains(r)) {
                // Still there, just not readable right now: keep ours as is.
                continue;
            }
            if note.sync.remote_id.is_some() {
                if note.sync.dirty {
                    // Deleted there, edited here: put it back.
                    let n = &mut w.notes[i];
                    n.sync.remote_id = None;
                    n.sync.revision = None;
                } else {
                    remove.push(note.id.clone());
                }
            }
            continue;
        };
        matched.insert(r.remote_id.clone());
        if note.sync.remote_id.is_none() {
            w.notes[i].sync.remote_id = Some(r.remote_id.clone());
        }
        if w.notes[i].sync.revision.as_deref() == Some(&r.revision) {
            continue;
        }
        let folder = w.ensure_folder_at(account, &r.data.folder);
        if !w.notes[i].sync.dirty {
            take_remote(&mut w.notes[i], r, folder);
            continue;
        }
        // Changed on both sides.
        let base = w.notes[i].sync.base.clone().unwrap_or_else(|| r.data.clone());
        let ours_path = w.note_data(&w.notes[i]).folder;
        let path = merge_value(&base.folder, &ours_path, &r.data.folder);
        let merged_folder = w.ensure_folder_at(account, &path);
        let n = &mut w.notes[i];
        n.folder = merged_folder;
        match merge_text(&base.body, &n.body, &r.data.body) {
            Ok(body) => n.body = body,
            Err(()) => {
                // Keep both: theirs becomes a new note next to ours.
                let mut copy = Note::new(account, &folder, &r.data.body);
                copy.created = r.data.created;
                copy.modified = r.data.modified;
                notices.push(format!(
                    "“{}” was changed on another device too; both versions were kept.",
                    crate::markdown::title_of(&n.body)
                ));
                extra_notes.push(copy);
            }
        }
        n.pinned = merge_value(&base.pinned, &n.pinned, &r.data.pinned);
        n.deleted = merge_value(&base.deleted, &n.deleted, &r.data.deleted);
        n.modified = n.modified.max(r.data.modified);
        n.sync.revision = Some(r.revision.clone());
        n.sync.base = Some(r.data.clone());
    }
    w.notes.retain(|n| !remove.contains(&n.id));
    // Notes new on the backend.
    for r in &remote.notes {
        if matched.contains(&r.remote_id) {
            continue;
        }
        if w.notes.iter().any(|n| n.id == r.data.id) {
            // Two backend notes claim the same id (a create retried after a
            // lost reply): keep the second under a new id.
            let folder = w.ensure_folder_at(account, &r.data.folder);
            let mut n = blank(account, &folder);
            take_remote(&mut n, r, folder);
            n.id = new_id();
            w.notes.push(n);
            continue;
        }
        let folder = w.ensure_folder_at(account, &r.data.folder);
        let mut n = blank(account, &folder);
        take_remote(&mut n, r, folder);
        n.id = r.data.id.clone();
        w.notes.push(n);
    }
    w.notes.extend(extra_notes);
    let _ = now;
}

fn blank(account: &str, folder: &str) -> Note {
    let mut n = Note::new(account, folder, "");
    n.sync = NoteSync::default();
    n
}

fn take_remote(n: &mut Note, r: &RemoteNote, folder: String) {
    n.body = r.data.body.clone();
    n.folder = folder;
    n.pinned = r.data.pinned;
    n.deleted = r.data.deleted;
    n.created = r.data.created;
    n.modified = r.data.modified;
    n.sync = NoteSync {
        remote_id: Some(r.remote_id.clone()),
        revision: Some(r.revision.clone()),
        base: Some(r.data.clone()),
        dirty: false,
    };
}

/// Uploads changed notes. Returns whether any was refused because it had
/// changed on the backend (another round is needed).
fn push_notes(backend: &mut dyn Backend, w: &mut Store) -> Result<bool> {
    let mut conflicts = false;
    for i in 0..w.notes.len() {
        let n = &w.notes[i];
        if !n.sync.dirty && n.sync.remote_id.is_some() {
            continue;
        }
        let data = w.note_data(n);
        if n.sync.remote_id.is_some() && n.sync.base.as_ref() == Some(&data) {
            // Merged back to exactly what the backend has.
            w.notes[i].sync.dirty = false;
            continue;
        }
        match backend.save_note(n.sync.remote_id.as_deref(), n.sync.revision.as_deref(), &data) {
            Ok((remote_id, revision)) => {
                let n = &mut w.notes[i];
                n.sync = NoteSync { remote_id: Some(remote_id), revision: Some(revision), base: Some(data), dirty: false };
            }
            Err(e) if e.is::<Conflict>() => conflicts = true,
            Err(e) => return Err(e),
        }
    }
    Ok(conflicts)
}

/// Applies a sync's outcome to the store. Notes changed while the sync ran
/// keep those changes (merged with what the sync brought in); folders and
/// notes made meanwhile stay.
pub fn apply(store: &mut Store, snap: &AccountSnapshot, out: Outcome) -> Vec<String> {
    let mut notices = out.notices;
    let account = out.account.as_str();
    if let Some(a) = store.account_mut(account) {
        if out.error.is_none() {
            a.last_sync = Some(Utc::now());
        }
        a.cursor = out.cursor.clone();
    }
    store.tombstones.retain(|t| !out.tombstones_done.contains(t));
    if out.unchanged {
        return notices;
    }

    // ---- Folders --------------------------------------------------------
    let before: HashMap<&str, &Folder> = snap.folders.iter().map(|f| (f.id.as_str(), f)).collect();
    let outcome_ids: HashSet<&str> = out.folders.iter().map(|f| f.id.as_str()).collect();
    let mut folders: Vec<Folder> = Vec::new();
    for f in &out.folders {
        match (store.folder(&f.id), before.get(f.id.as_str())) {
            // Changed here during the sync: keep the change, take the sync
            // bookkeeping.
            (Some(cur), Some(old)) if cur != *old => folders.push(Folder { sync: f.sync.clone(), ..cur.clone() }),
            // Deleted here during the sync.
            (None, Some(_)) => {}
            _ => folders.push(f.clone()),
        }
    }
    // Made here during the sync, or removed by the sync but changed here.
    for f in store.folders.iter().filter(|f| f.account == account && !outcome_ids.contains(f.id.as_str())) {
        match before.get(f.id.as_str()) {
            None => folders.push(f.clone()),
            Some(old) if *old != f => folders.push(Folder { sync: Default::default(), ..f.clone() }),
            Some(_) => {}
        }
    }
    store.folders.retain(|f| f.account != account);
    store.folders.extend(folders);

    // ---- Notes ----------------------------------------------------------
    let before: HashMap<&str, &Note> = snap.notes.iter().map(|n| (n.id.as_str(), n)).collect();
    let outcome_ids: HashSet<&str> = out.notes.iter().map(|n| n.id.as_str()).collect();
    let mut notes: Vec<Note> = Vec::new();
    for o in &out.notes {
        let cur = store.notes.iter().find(|n| n.id == o.id);
        match (cur, before.get(o.id.as_str())) {
            (Some(cur), Some(old)) if cur.version != old.version => {
                notes.push(merge_during_sync(old, cur, o, &mut notices));
            }
            (Some(cur), _) => notes.push(Note { version: cur.version, ..o.clone() }),
            (None, Some(_)) => {
                // Emptied from Recently Deleted during the sync.
                if o.sync.remote_id.is_some() && before[o.id.as_str()].sync.remote_id.is_none() {
                    store.tombstones.push(Tombstone {
                        account: account.to_owned(),
                        note: true,
                        remote_id: o.sync.remote_id.clone().unwrap(),
                        path: vec![],
                    });
                }
            }
            (None, None) => {
                let mut n = o.clone();
                if store.notes.iter().any(|x| x.id == n.id) {
                    n.id = new_id();
                }
                notes.push(n);
            }
        }
    }
    for n in store.notes.iter().filter(|n| n.account == account && !outcome_ids.contains(n.id.as_str())) {
        match before.get(n.id.as_str()) {
            // Made here during the sync.
            None => notes.push(n.clone()),
            // Deleted on the backend, but edited here meanwhile: put back.
            Some(old) if old.version != n.version => {
                let mut n = n.clone();
                n.sync = NoteSync { dirty: true, ..NoteSync::default() };
                notes.push(n);
            }
            Some(_) => {}
        }
    }
    store.notes.retain(|n| n.account != account);
    store.notes.extend(notes);
    notices
}

/// A note edited here while a sync changed it: `old` is how the sync found
/// it, `cur` how it is now, `synced` what the sync made of it.
fn merge_during_sync(old: &Note, cur: &Note, synced: &Note, notices: &mut Vec<String>) -> Note {
    let mut n = cur.clone();
    n.sync = synced.sync.clone();
    n.sync.dirty = true;
    match merge_text(&old.body, &cur.body, &synced.body) {
        Ok(body) => n.body = body,
        Err(()) => notices.push(format!(
            "“{}” changed on another device while you were editing it; your version was kept.",
            crate::markdown::title_of(&cur.body)
        )),
    }
    n.pinned = merge_value(&old.pinned, &cur.pinned, &synced.pinned);
    n.deleted = merge_value(&old.deleted, &cur.deleted, &synced.deleted);
    n.folder = merge_value(&old.folder, &cur.folder, &synced.folder);
    n
}

/// Same `NoteData` for a note as the engine would upload (for tests).
#[cfg(test)]
pub fn data_of(store: &Store, id: &str) -> NoteData {
    store.note_data(store.note(id).unwrap())
}

#[cfg(test)]
mod tests;
