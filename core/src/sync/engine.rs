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
//!    seen, and each note's new images just before it. If one is refused
//!    because it changed in the meantime, pull and reconcile again (at most
//!    a few rounds).
//! 5. Delete folders deleted here.
//! 6. Download the images notes show that this device doesn't have. (A
//!    sync with only that to do still runs: see
//!    [`AccountSnapshot::missing_attachments`].)

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use anyhow::Result;
use chrono::{DateTime, Utc};

use super::merge::{merge_text, merge_value};
use super::{Backend, Conflict, RemoteNote, Snapshot};
#[cfg(test)]
use super::NoteData;
use crate::attachments;
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
    /// The folder this device keeps attachments in (see
    /// [`crate::attachments`]). `None`: attachments aren't synced.
    pub attachments: Option<PathBuf>,
}

impl AccountSnapshot {
    /// With the attachments in `dir` synced too.
    pub fn with_attachments(mut self, dir: &Path) -> AccountSnapshot {
        self.attachments = Some(dir.to_owned());
        self
    }

    /// Images the account's notes show that this device doesn't have yet:
    /// a sync then pulls, even if nothing else changed, to fetch them.
    pub fn missing_attachments(&self) -> bool {
        let Some(dir) = &self.attachments else { return false };
        self.notes.iter().any(|n| attachments::refs_in_body(&n.body).iter().any(|a| !attachments::exists(dir, a)))
    }

    pub fn of(store: &Store, account: &str) -> AccountSnapshot {
        AccountSnapshot {
            account: account.to_owned(),
            cursor: store.account(account).and_then(|a| a.cursor.clone()),
            folders: store.folders.iter().filter(|f| f.account == account).cloned().collect(),
            notes: store.notes.iter().filter(|n| n.account == account).cloned().collect(),
            tombstones: store.tombstones.iter().filter(|t| t.account == account).cloned().collect(),
            attachments: None,
        }
    }

    fn as_store(&self) -> Store {
        Store { accounts: vec![], folders: self.folders.clone(), notes: self.notes.clone(), tombstones: vec![] }
    }

    /// Anything to upload.
    pub fn has_local_changes(&self) -> bool {
        has_local_changes(&self.as_store(), &self.account) || !self.tombstones.is_empty()
    }
}

/// Whether an account has anything to upload, read straight from the store
/// (nothing copied: the app asks this every frame).
pub fn has_local_changes(store: &Store, account: &str) -> bool {
    store.tombstones.iter().any(|t| t.account == account)
        || store.notes.iter().any(|n| n.account == account && (n.sync.dirty || n.sync.remote_id.is_none()))
        || store
            .folders
            .iter()
            .filter(|f| f.account == account)
            .any(|f| f.sync.remote_id.is_none() || f.sync.synced_path.as_ref() != Some(&store.folder_path(&f.id)))
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
    /// Attachments downloaded (pictures that can now be shown).
    pub attachments_fetched: usize,
}

pub fn sync_account(backend: &mut dyn Backend, snap: &AccountSnapshot, now: DateTime<Utc>) -> Result<Outcome> {
    let mut out = Outcome { account: snap.account.clone(), ..Outcome::default() };
    if !snap.has_local_changes() && snap.cursor.is_some() && !snap.missing_attachments() {
        let cursor = backend.cursor()?;
        if cursor.is_some() && cursor == snap.cursor {
            out.unchanged = true;
            out.cursor = cursor;
            return Ok(out);
        }
    }
    let known: Vec<(&str, &str, &super::NoteData)> = snap
        .notes
        .iter()
        .filter_map(|n| Some((n.sync.remote_id.as_deref()?, n.sync.revision.as_deref()?, n.sync.base.as_ref()?)))
        .collect();
    let known_folders: Vec<(&str, &str, &[String])> = snap
        .folders
        .iter()
        .filter_map(|f| Some((f.sync.remote_id.as_deref()?, f.sync.revision.as_deref()?, f.sync.synced_path.as_deref()?)))
        .collect();
    backend.known(&known, &known_folders);
    let result = sync_changes(backend, snap, now, out);
    let finished = backend.finish();
    let mut out = result?;
    if let Err(e) = finished {
        // What was written is there; other devices may just not notice it
        // straight away. Tried again next sync.
        if out.error.is_none() {
            out.error = Some(format!("{e:#}"));
        }
        out.cursor = None;
    }
    Ok(out)
}

fn sync_changes(backend: &mut dyn Backend, snap: &AccountSnapshot, now: DateTime<Utc>, mut out: Outcome) -> Result<Outcome> {
    let mut w = snap.as_store();
    let account = snap.account.as_str();

    // Notes emptied from Recently Deleted go first, so a pull doesn't bring
    // them back.
    for t in snap.tombstones.iter().filter(|t| t.note && t.note_id.is_none()) {
        match backend.delete_note(&t.remote_id, t.revision.as_deref()) {
            Ok(()) => {}
            // Edited on another device since: it stays, and the pull below
            // brings it back here.
            Err(e) if e.is::<Conflict>() => {
                out.notices.push("A note you deleted was changed on another device, so it was kept.".into());
            }
            Err(e) => return Err(e),
        }
        out.tombstones_done.push(t.clone());
    }
    push_folders(backend, &mut w)?;
    let deleting: HashSet<&str> =
        snap.tombstones.iter().filter(|t| !t.note).map(|t| t.remote_id.as_str()).collect();

    let mut finished = false;
    for round in 0..MAX_ROUNDS {
        let cursor_before = backend.cursor()?;
        let mut remote = backend.pull()?;
        let deleted = round == 0 && delete_forgotten(backend, snap, &w, &mut remote, &mut out)?;
        reconcile(&mut w, account, remote, &deleting, now, &mut out.notices);
        match push_notes(backend, &mut w, snap.attachments.as_deref()).map(|(c, wrote)| (c, wrote || deleted)) {
            Ok((false, false)) => {
                // Only the cursor from before the pull is known to be
                // covered: one read now could include another device's
                // change made since, which would then never be pulled.
                out.cursor = cursor_before;
                finished = true;
                break;
            }
            // Everything uploaded, but no round left to confirm it: done,
            // with no cursor, so the next sync pulls everything.
            Ok((false, true)) if round + 1 == MAX_ROUNDS => {
                out.cursor = None;
                finished = true;
                break;
            }
            // Conflicts: pull and merge again. Uploads: one more round, so
            // the saved cursor includes our own writes and the next sync
            // is cheap.
            Ok(_) => continue,
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
        // (The cursor stays the one from before the pull, for the same
        // reason as above.)
    } else {
        out.cursor = None;
    }
    // Pictures the notes show that this device doesn't have yet.
    if let (None, Some(dir)) = (&out.error, &snap.attachments) {
        let (n, err) = fetch_attachments(backend, dir, &w.notes);
        out.attachments_fetched = n;
        if let Some(e) = err {
            out.error = Some(e);
            out.cursor = None;
        }
    }
    out.folders = w.folders;
    out.notes = w.notes;
    Ok(out)
}

/// Notes that went from this account before this device heard back about
/// their upload (see [`Tombstone::note_id`]): deleted on the backend if it
/// has them, and left out of `remote` so they aren't taken as new. Returns
/// whether any were deleted.
fn delete_forgotten(
    backend: &mut dyn Backend,
    snap: &AccountSnapshot,
    w: &Store,
    remote: &mut Snapshot,
    out: &mut Outcome,
) -> Result<bool> {
    let forgotten: Vec<&Tombstone> = snap.tombstones.iter().filter(|t| t.note && t.note_id.is_some()).collect();
    if forgotten.is_empty() {
        return Ok(false);
    }
    let ids: HashSet<&str> = forgotten.iter().filter_map(|t| t.note_id.as_deref()).collect();
    let mut deleted = false;
    let mut kept = Vec::with_capacity(remote.notes.len());
    for r in std::mem::take(&mut remote.notes) {
        // (Still here under that id, e.g. moved away and back: it stays.)
        let ours = w.notes.iter().any(|n| n.id == r.data.id || n.sync.remote_id.as_deref() == Some(&r.remote_id));
        if !ids.contains(r.data.id.as_str()) || ours {
            kept.push(r);
            continue;
        }
        match backend.delete_note(&r.remote_id, Some(&r.revision)) {
            Ok(()) => deleted = true,
            // Changed since the pull: it stays, as a note new here.
            Err(e) if e.is::<Conflict>() => kept.push(r),
            Err(e) => return Err(e),
        }
    }
    remote.notes = kept;
    out.tombstones_done.extend(forgotten.into_iter().cloned());
    Ok(deleted)
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
        // Folders inside it moved along on the backend. (Rewritten there, so
        // their revisions are new; the next pull says which.)
        for d in w.folder_and_descendants(id) {
            let new_path = w.folder_path(&d);
            let f = w.folder_mut(&d).unwrap();
            if let Some(sp) = &mut f.sync.synced_path {
                if sp.starts_with(&old) {
                    let rest = sp[old.len()..].to_vec();
                    *sp = path.iter().cloned().chain(rest).collect();
                    f.sync.revision = None;
                }
            }
            if d == *id {
                f.sync.synced_path = Some(new_path);
                f.sync.revision = None;
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
        f.sync.revision = None;
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
            // One the backend has is taken again from it below, with the
            // folder the backend put it in, even if its revision didn't move.
            if n.sync.remote_id.is_some() && !n.sync.dirty {
                n.sync.revision = None;
            }
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
                let f = w.folder_mut(&id).unwrap();
                f.sync.synced_path = Some(r.path.clone());
                f.sync.revision = r.revision.clone();
            }
            None => {
                let id = w.ensure_folder_at(account, &r.path);
                let f = w.folder_mut(&id).unwrap();
                f.sync.remote_id = Some(r.remote_id.clone());
                f.sync.synced_path = Some(r.path.clone());
                f.sync.revision = r.revision.clone();
            }
        }
    }

    // ---- Notes ----------------------------------------------------------
    // Where a note goes locally: a folder this sync is deleting isn't made
    // again for it; the note goes where the backend will move it, the
    // deleted folder's parent (or the default folder).
    let deleted_paths: Vec<&Vec<String>> =
        remote.folders.iter().filter(|f| deleting.contains(f.remote_id.as_str())).map(|f| &f.path).collect();
    let place = |path: &[String]| -> Vec<String> {
        match deleted_paths.iter().filter(|d| !d.is_empty() && path.starts_with(d)).min_by_key(|d| d.len()) {
            Some(d) if d.len() > 1 => d[..d.len() - 1].to_vec(),
            Some(_) => vec![crate::model::DEFAULT_FOLDER.to_owned()],
            None => path.to_vec(),
        }
    };
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
                // Edited here too, ours can't be saved over what we can't
                // read (or merged with it): it becomes a note of its own.
                if note.sync.dirty {
                    let n = &mut w.notes[i];
                    n.sync.remote_id = None;
                    n.sync.revision = None;
                    n.sync.base = None;
                }
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
        if !w.notes[i].sync.dirty {
            let path = place(&r.data.folder);
            let folder = w.ensure_folder_at(account, &path);
            take_remote(&mut w.notes[i], r, folder);
            // Moved out of a folder being deleted: upload the move (before
            // the delete), as not every backend moves notes up itself.
            w.notes[i].sync.dirty = path != r.data.folder;
            continue;
        }
        // Changed on both sides. (Only the folder used is made, so a folder
        // being deleted isn't brought back.)
        let base = w.notes[i].sync.base.clone().unwrap_or_else(|| r.data.clone());
        let ours_path = w.note_data(&w.notes[i]).folder;
        let path = merge_value(&base.folder, &ours_path, &r.data.folder);
        let merged_folder = w.ensure_folder_at(account, &place(&path));
        let n = &mut w.notes[i];
        n.folder = merged_folder.clone();
        match merge_text(&base.body, &n.body, &r.data.body) {
            Ok(body) => n.body = body,
            Err(()) => {
                // Keep both: theirs becomes a new note next to ours.
                let mut copy = Note::new(account, &merged_folder, &r.data.body);
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
            let path = place(&r.data.folder);
            let folder = w.ensure_folder_at(account, &path);
            let mut n = blank(account, &folder);
            take_remote(&mut n, r, folder);
            n.sync.dirty = path != r.data.folder;
            n.id = new_id();
            w.notes.push(n);
            continue;
        }
        let path = place(&r.data.folder);
        let folder = w.ensure_folder_at(account, &path);
        let mut n = blank(account, &folder);
        take_remote(&mut n, r, folder);
        n.sync.dirty = path != r.data.folder;
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

/// Uploads the images a note shows that the backend doesn't have, so they're
/// there before the note is (another device never sees a note whose
/// pictures it can't get). Images this device doesn't have either are
/// skipped: whoever added them uploads them.
fn push_attachments(backend: &mut dyn Backend, dir: &Path, body: &str) -> Result<()> {
    for name in attachments::refs_in_body(body) {
        if backend.has_attachment(&name)? {
            continue;
        }
        let Ok(data) = std::fs::read(attachments::path(dir, &name)) else { continue };
        if !attachments::verify(&name, &data) {
            continue; // damaged here; not spread
        }
        backend.put_attachment(&name, &data)?;
    }
    Ok(())
}

/// Downloads the images `notes` show that `dir` doesn't have and the backend
/// does, checking each is what its name says. Returns how many arrived, and
/// the error that stopped it, if one did.
fn fetch_attachments(backend: &mut dyn Backend, dir: &Path, notes: &[Note]) -> (usize, Option<String>) {
    let mut wanted: Vec<String> = Vec::new();
    for n in notes {
        for a in attachments::refs_in_body(&n.body) {
            if !wanted.contains(&a) && !attachments::exists(dir, &a) {
                wanted.push(a);
            }
        }
    }
    let mut fetched = 0;
    for name in wanted {
        let r = backend.has_attachment(&name).and_then(|has| if has { backend.get_attachment(&name) } else { Ok(None) });
        match r {
            // A damaged one is left out (and tried again next time).
            Ok(Some(data)) if attachments::verify(&name, &data) => match attachments::write(dir, &name, &data) {
                Ok(()) => fetched += 1,
                Err(e) => return (fetched, Some(format!("{e:#}"))),
            },
            Ok(_) => {}
            Err(e) => return (fetched, Some(format!("downloading a picture: {e:#}"))),
        }
    }
    (fetched, None)
}

/// Uploads changed notes (each note's new images first). Returns (whether
/// some were refused as conflicts, whether anything was written).
fn push_notes(backend: &mut dyn Backend, w: &mut Store, dir: Option<&Path>) -> Result<(bool, bool)> {
    let mut conflicts = false;
    let mut wrote = false;
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
        if let Some(dir) = dir {
            push_attachments(backend, dir, &data.body)?;
        }
        let n = &w.notes[i];
        match backend.save_note(n.sync.remote_id.as_deref(), n.sync.revision.as_deref(), &data) {
            Ok((remote_id, revision)) => {
                let n = &mut w.notes[i];
                n.sync = NoteSync { remote_id: Some(remote_id), revision: Some(revision), base: Some(data), dirty: false };
                wrote = true;
            }
            Err(e) if e.is::<Conflict>() => conflicts = true,
            Err(e) => return Err(e),
        }
    }
    Ok((conflicts, wrote))
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
    // A note deleted for good (or moved to another account) while this
    // sync uploaded it: its tombstone still has the revision from before,
    // which would make the delete look like it lost to an edit elsewhere.
    for o in &out.notes {
        let (Some(rid), Some(old)) = (&o.sync.remote_id, before.get(o.id.as_str())) else { continue };
        for t in store.tombstones.iter_mut().filter(|t| t.note && t.account == account && &t.remote_id == rid) {
            if t.revision == old.sync.revision {
                t.revision = o.sync.revision.clone();
            }
        }
    }
    let mut notes: Vec<Note> = Vec::new();
    for o in &out.notes {
        let cur = store.notes.iter().find(|n| n.id == o.id);
        match (cur, before.get(o.id.as_str())) {
            // Moved to another account during the sync: it stays there, and
            // what the sync uploaded here is deleted again.
            (Some(cur), _) if cur.account != account => {
                if let Some(rid) = &o.sync.remote_id {
                    if !store.tombstones.iter().any(|t| t.account == account && &t.remote_id == rid) {
                        store.tombstones.push(Tombstone {
                            account: account.to_owned(),
                            note: true,
                            remote_id: rid.clone(),
                            path: vec![],
                            revision: o.sync.revision.clone(),
                            note_id: None,
                        });
                    }
                }
            }
            (Some(cur), Some(old)) if cur.version != old.version => {
                let (merged, theirs) = merge_during_sync(old, cur, o, &mut notices);
                notes.push(merged);
                notes.extend(theirs);
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
                        revision: o.sync.revision.clone(),
                        note_id: None,
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
/// it, `cur` how it is now, `synced` what the sync made of it. If the texts
/// can't be merged, the synced text comes back as a second note, so neither
/// version is lost (ours overwrites the backend's on the next sync).
fn merge_during_sync(old: &Note, cur: &Note, synced: &Note, notices: &mut Vec<String>) -> (Note, Option<Note>) {
    let mut n = cur.clone();
    n.sync = synced.sync.clone();
    n.sync.dirty = true;
    let mut theirs = None;
    match merge_text(&old.body, &cur.body, &synced.body) {
        Ok(body) => n.body = body,
        Err(()) => {
            let mut copy = Note::new(&synced.account, &synced.folder, &synced.body);
            copy.created = synced.created;
            copy.modified = synced.modified;
            theirs = Some(copy);
            notices.push(format!(
                "“{}” changed on another device while you were editing it; both versions were kept.",
                crate::markdown::title_of(&cur.body)
            ));
        }
    }
    n.pinned = merge_value(&old.pinned, &cur.pinned, &synced.pinned);
    n.deleted = merge_value(&old.deleted, &cur.deleted, &synced.deleted);
    n.folder = merge_value(&old.folder, &cur.folder, &synced.folder);
    (n, theirs)
}

/// Same `NoteData` for a note as the engine would upload (for tests).
#[cfg(test)]
pub fn data_of(store: &Store, id: &str) -> NoteData {
    store.note_data(store.note(id).unwrap())
}

#[cfg(test)]
mod tests;
