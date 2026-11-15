//! Two devices ("laptop" and "phone"), each with its own store, syncing one
//! account through a shared SQLite backend.

use super::*;
use crate::model::{Account, Note, Store};
use crate::sync::sqlite::{Config, SqliteBackend};
use crate::sync::BackendConfig;

const ACC: &str = "acc";

struct Device {
    store: Store,
    backend: SqliteBackend,
}

impl Device {
    fn new(path: &std::path::Path) -> Device {
        let mut store = Store::new_install();
        store.accounts.push(Account {
            id: ACC.into(),
            name: "Shared".into(),
            backend: BackendConfig::Sqlite(Config { path: path.into() }),
            enabled: true,
            cursor: None,
            last_sync: None,
        });
        store.ensure_local();
        Device { store, backend: SqliteBackend::open(&Config { path: path.into() }).unwrap() }
    }

    fn sync(&mut self) -> Vec<String> {
        let snap = AccountSnapshot::of(&self.store, ACC);
        let out = sync_account(&mut self.backend, &snap, Utc::now()).unwrap();
        assert_eq!(out.error, None);
        apply(&mut self.store, &snap, out)
    }

    fn folder(&self) -> String {
        self.store.default_folder(ACC).unwrap().id.clone()
    }

    fn add(&mut self, body: &str) -> String {
        let n = Note::new(ACC, &self.folder(), body);
        let id = n.id.clone();
        self.store.notes.push(n);
        id
    }

    fn edit(&mut self, id: &str, body: &str) {
        let n = self.store.note_mut(id).unwrap();
        n.body = body.into();
        n.touch();
    }

    fn body(&self, id: &str) -> String {
        self.store.note(id).unwrap().body.clone()
    }

    fn bodies(&self) -> Vec<String> {
        let mut v: Vec<String> = self.store.notes.iter().filter(|n| n.account == ACC).map(|n| n.body.clone()).collect();
        v.sort();
        v
    }
}

fn pair() -> (tempfile::TempDir, Device, Device) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("shared.db");
    let laptop = Device::new(&path);
    let phone = Device::new(&path);
    (dir, laptop, phone)
}

#[test]
fn notes_travel_between_devices() {
    let (_d, mut laptop, mut phone) = pair();
    let id = laptop.add("# Groceries\neggs\n");
    laptop.sync();
    phone.sync();
    assert_eq!(phone.body(&id), "# Groceries\neggs\n");
    assert!(!phone.store.note(&id).unwrap().sync.dirty);
    // Both devices share the same default folder on the backend.
    assert_eq!(laptop.store.folders.iter().filter(|f| f.account == ACC).count(), 1);
    assert_eq!(phone.store.folders.iter().filter(|f| f.account == ACC).count(), 1);
}

#[test]
fn nothing_to_do_is_cheap() {
    let (_d, mut laptop, _phone) = pair();
    laptop.add("x");
    laptop.sync();
    let snap = AccountSnapshot::of(&laptop.store, ACC);
    assert!(!snap.has_local_changes());
    let out = sync_account(&mut laptop.backend, &snap, Utc::now()).unwrap();
    assert!(out.unchanged);
}

#[test]
fn edits_to_different_paragraphs_are_merged() {
    let (_d, mut laptop, mut phone) = pair();
    let id = laptop.add("# List\neggs\nmilk\n");
    laptop.sync();
    phone.sync();
    laptop.edit(&id, "# List\neggs (12)\nmilk\n");
    phone.edit(&id, "# List\neggs\nmilk\nbread\n");
    laptop.sync();
    let notices = phone.sync();
    assert!(notices.is_empty());
    assert_eq!(phone.body(&id), "# List\neggs (12)\nmilk\nbread\n");
    laptop.sync();
    assert_eq!(laptop.body(&id), "# List\neggs (12)\nmilk\nbread\n");
}

#[test]
fn clashing_edits_keep_both_versions() {
    let (_d, mut laptop, mut phone) = pair();
    let id = laptop.add("# Plan\nmeet at 5\n");
    laptop.sync();
    phone.sync();
    laptop.edit(&id, "# Plan\nmeet at 6\n");
    phone.edit(&id, "# Plan\nmeet at 7\n");
    laptop.sync();
    let notices = phone.sync();
    assert_eq!(notices.len(), 1, "{notices:?}");
    assert_eq!(phone.body(&id), "# Plan\nmeet at 7\n", "ours stays in the note");
    laptop.sync();
    assert_eq!(laptop.bodies(), vec!["# Plan\nmeet at 6\n", "# Plan\nmeet at 7\n"]);
    assert_eq!(phone.bodies(), laptop.bodies());
}

#[test]
fn an_edit_beats_a_delete() {
    let (_d, mut laptop, mut phone) = pair();
    let id = laptop.add("keep me");
    laptop.sync();
    phone.sync();
    // The phone empties it from Recently Deleted; the laptop edits it.
    phone.store.purge_note(&id);
    phone.sync();
    laptop.edit(&id, "keep me, edited");
    laptop.sync();
    phone.sync();
    assert_eq!(phone.bodies(), vec!["keep me, edited"]);
}

#[test]
fn a_delete_without_edits_goes_through() {
    let (_d, mut laptop, mut phone) = pair();
    let id = laptop.add("bye");
    laptop.sync();
    phone.sync();
    phone.store.purge_note(&id);
    phone.sync();
    assert!(phone.store.tombstones.is_empty());
    laptop.sync();
    assert!(laptop.store.note(&id).is_none());
}

#[test]
fn recently_deleted_and_pins_sync() {
    let (_d, mut laptop, mut phone) = pair();
    let id = laptop.add("x");
    laptop.sync();
    phone.sync();
    let n = laptop.store.note_mut(&id).unwrap();
    n.deleted = Some(Utc::now());
    n.touch();
    let n = phone.store.note_mut(&id).unwrap();
    n.pinned = true;
    n.touch();
    laptop.sync();
    phone.sync();
    let n = phone.store.note(&id).unwrap();
    assert!(n.deleted.is_some() && n.pinned, "both field changes survive");
}

#[test]
fn folders_and_moves_sync() {
    let (_d, mut laptop, mut phone) = pair();
    let id = laptop.add("in work");
    let work = laptop.store.ensure_folder_at(ACC, &["Work".into()]);
    laptop.store.note_mut(&id).unwrap().folder = work.clone();
    laptop.sync();
    phone.sync();
    let pf = phone.store.note(&id).unwrap().folder.clone();
    assert_eq!(phone.store.folder_path(&pf), vec!["Work"]);

    // Rename on the laptop: the phone follows, keeping the note in it.
    laptop.store.folder_mut(&work).unwrap().name = "Projects".into();
    laptop.sync();
    phone.sync();
    let pf = phone.store.note(&id).unwrap().folder.clone();
    assert_eq!(phone.store.folder_path(&pf), vec!["Projects"]);
    assert!(phone.store.folder_at(ACC, &["Work".into()]).is_none());
}

#[test]
fn deleting_a_folder_syncs() {
    let (_d, mut laptop, mut phone) = pair();
    let work = laptop.store.ensure_folder_at(ACC, &["Work".into()]);
    laptop.sync();
    phone.sync();
    assert!(phone.store.folder_at(ACC, &["Work".into()]).is_some());
    let f = laptop.store.folders.iter().position(|f| f.id == work).unwrap();
    let f = laptop.store.folders.remove(f);
    laptop.store.tombstones.push(Tombstone {
        account: ACC.into(),
        note: false,
        remote_id: f.sync.remote_id.unwrap(),
        path: vec!["Work".into()],
        revision: None,
    });
    laptop.sync();
    assert!(laptop.store.folder_at(ACC, &["Work".into()]).is_none(), "not brought back by the pull");
    phone.sync();
    assert!(phone.store.folder_at(ACC, &["Work".into()]).is_none());
}

#[test]
fn notes_left_in_a_deleted_folder_end_up_in_the_same_place_everywhere() {
    let (_d, mut laptop, mut phone) = pair();
    let work = laptop.store.ensure_folder_at(ACC, &["Projects".into(), "Work".into()]);
    laptop.sync();
    phone.sync();
    // The phone puts a note in Work; the laptop deletes Work (as if before
    // it heard of the note), so the backend moves the note up to Projects.
    let pwork = phone.store.folder_at(ACC, &["Projects".into(), "Work".into()]).unwrap().id.clone();
    let n = Note::new(ACC, &pwork, "late note");
    let id = n.id.clone();
    phone.store.notes.push(n);
    phone.sync();
    let f = laptop.store.folders.iter().position(|f| f.id == work).unwrap();
    let f = laptop.store.folders.remove(f);
    laptop.store.tombstones.push(Tombstone {
        account: ACC.into(),
        note: false,
        remote_id: f.sync.remote_id.unwrap(),
        path: vec!["Projects".into(), "Work".into()],
        revision: None,
    });
    laptop.sync();
    phone.sync();
    laptop.sync();
    for d in [&laptop, &phone] {
        let folder = d.store.note(&id).unwrap().folder.clone();
        assert_eq!(d.store.folder_path(&folder), vec!["Projects"]);
    }
}

#[test]
fn a_folder_deleted_here_stays_deleted_when_the_other_device_added_to_it() {
    let (_d, mut laptop, mut phone) = pair();
    let work = laptop.store.ensure_folder_at(ACC, &["Work".into()]);
    laptop.sync();
    phone.sync();
    // The laptop deletes Work while offline…
    let f = laptop.store.folders.iter().position(|f| f.id == work).unwrap();
    let f = laptop.store.folders.remove(f);
    laptop.store.tombstones.push(Tombstone {
        account: ACC.into(),
        note: false,
        remote_id: f.sync.remote_id.unwrap(),
        path: vec!["Work".into()],
        revision: None,
    });
    // …while the phone adds a note to it.
    let pwork = phone.store.folder_at(ACC, &["Work".into()]).unwrap().id.clone();
    let n = Note::new(ACC, &pwork, "added on the phone");
    let id = n.id.clone();
    phone.store.notes.push(n);
    phone.sync();
    laptop.sync();
    laptop.sync();
    phone.sync();
    let work_path = vec!["Work".to_owned()];
    for d in [&laptop, &phone] {
        assert!(d.store.folder_at(ACC, &work_path).is_none(), "Work came back");
        assert!(d.store.note(&id).is_some(), "the phone's note is kept");
    }
}

#[test]
fn a_note_put_back_after_a_delete_gets_a_new_revision() {
    let (_d, mut laptop, _phone) = pair();
    let id = laptop.add("v1");
    laptop.sync();
    let rid = laptop.store.note(&id).unwrap().sync.remote_id.clone().unwrap();
    let rev1 = laptop.store.note(&id).unwrap().sync.revision.clone().unwrap();
    laptop.backend.delete_note(&rid, None).unwrap();
    let (_, rev2) = laptop.backend.save_note(Some(&rid), Some(&rev1), &data_of(&laptop.store, &id)).unwrap();
    assert_ne!(rev1, rev2, "a stale device mustn't take the new text for the one it has");
}

#[test]
fn an_edit_elsewhere_beats_emptying_recently_deleted() {
    let (_d, mut laptop, mut phone) = pair();
    let id = laptop.add("# Keep\nv1\n");
    laptop.sync();
    phone.sync();
    // The laptop deletes it for good while offline; the phone edits it.
    laptop.store.purge_note(&id);
    phone.edit(&id, "# Keep\nv2 (phone)\n");
    phone.sync();
    let notices = laptop.sync();
    assert_eq!(notices.len(), 1, "{notices:?}");
    assert!(laptop.store.tombstones.is_empty());
    laptop.sync();
    phone.sync();
    assert_eq!(laptop.bodies(), vec!["# Keep\nv2 (phone)\n"]);
    assert_eq!(phone.bodies(), vec!["# Keep\nv2 (phone)\n"]);
    // Unchanged elsewhere, it's deleted everywhere.
    let id2 = laptop.store.notes.iter().find(|n| n.account == ACC).unwrap().id.clone();
    laptop.store.purge_note(&id2);
    laptop.sync();
    phone.sync();
    assert!(laptop.bodies().is_empty() && phone.bodies().is_empty());
}

#[test]
fn edits_made_during_a_sync_are_kept() {
    let (_d, mut laptop, mut phone) = pair();
    let id = laptop.add("# A\none\ntwo\n");
    laptop.sync();
    phone.sync();
    phone.edit(&id, "# A\none\ntwo\nthree\n");
    phone.sync();

    // The laptop starts a sync, then the note is edited before it finishes.
    let snap = AccountSnapshot::of(&laptop.store, ACC);
    let out = sync_account(&mut laptop.backend, &snap, Utc::now()).unwrap();
    laptop.edit(&id, "# A (edited)\none\ntwo\n");
    apply(&mut laptop.store, &snap, out);
    assert_eq!(laptop.body(&id), "# A (edited)\none\ntwo\nthree\n");
    assert!(laptop.store.note(&id).unwrap().sync.dirty);
    laptop.sync();
    phone.sync();
    assert_eq!(phone.body(&id), "# A (edited)\none\ntwo\nthree\n");
}

#[test]
fn a_clashing_edit_made_during_a_sync_keeps_both_versions() {
    let (_d, mut laptop, mut phone) = pair();
    let id = laptop.add("# A\none\ntwo\n");
    laptop.sync();
    phone.sync();
    phone.edit(&id, "# A\none\nTWO (phone)\n");
    phone.sync();

    // The laptop's sync takes the phone's line, while the same line is
    // edited on the laptop.
    let snap = AccountSnapshot::of(&laptop.store, ACC);
    let out = sync_account(&mut laptop.backend, &snap, Utc::now()).unwrap();
    laptop.edit(&id, "# A\none\nTWO (laptop)\n");
    let notices = apply(&mut laptop.store, &snap, out);
    assert_eq!(notices.len(), 1, "{notices:?}");
    assert_eq!(laptop.body(&id), "# A\none\nTWO (laptop)\n");
    laptop.sync();
    phone.sync();
    let both = vec!["# A\none\nTWO (laptop)\n".to_owned(), "# A\none\nTWO (phone)\n".to_owned()];
    assert_eq!(laptop.bodies(), both);
    assert_eq!(phone.bodies(), both, "the phone's edit isn't lost");
}

#[test]
fn a_note_moved_to_another_account_during_a_sync_leaves_it() {
    let (_d, mut laptop, _phone) = pair();
    let id = laptop.add("# Moving\n");
    // The sync uploads the new note; meanwhile it's moved to the local
    // account (whose move found no remote id to delete yet).
    let snap = AccountSnapshot::of(&laptop.store, ACC);
    let out = sync_account(&mut laptop.backend, &snap, Utc::now()).unwrap();
    let local = laptop.store.default_folder(crate::model::LOCAL).unwrap().id.clone();
    {
        let n = laptop.store.note_mut(&id).unwrap();
        n.account = crate::model::LOCAL.into();
        n.folder = local;
        n.sync = Default::default();
        n.touch();
    }
    apply(&mut laptop.store, &snap, out);
    assert_eq!(laptop.store.notes.iter().filter(|n| n.id == id).count(), 1, "no second copy");
    assert_eq!(laptop.store.note(&id).unwrap().account, crate::model::LOCAL);
    laptop.sync();
    assert!(laptop.backend.pull().unwrap().notes.is_empty(), "the uploaded copy is deleted again");
    assert!(laptop.bodies().is_empty());
}

/// A backend where another device writes right after each pull, until the
/// hook runs out.
struct Racy<'a> {
    inner: &'a mut SqliteBackend,
    after_pull: Vec<Box<dyn FnOnce() + Send>>,
}

impl Backend for Racy<'_> {
    fn cursor(&mut self) -> anyhow::Result<Option<String>> {
        self.inner.cursor()
    }
    fn pull(&mut self) -> anyhow::Result<Snapshot> {
        let s = self.inner.pull();
        if !self.after_pull.is_empty() {
            (self.after_pull.remove(0))();
        }
        s
    }
    fn create_folder(&mut self, path: &[String]) -> anyhow::Result<String> {
        self.inner.create_folder(path)
    }
    fn rename_folder(&mut self, remote_id: &str, old_path: &[String], new_path: &[String]) -> anyhow::Result<()> {
        self.inner.rename_folder(remote_id, old_path, new_path)
    }
    fn delete_folder(&mut self, remote_id: &str, path: &[String]) -> anyhow::Result<()> {
        self.inner.delete_folder(remote_id, path)
    }
    fn save_note(&mut self, remote_id: Option<&str>, base: Option<&str>, note: &NoteData) -> anyhow::Result<(String, String)> {
        self.inner.save_note(remote_id, base, note)
    }
    fn delete_note(&mut self, remote_id: &str, base: Option<&str>) -> anyhow::Result<()> {
        self.inner.delete_note(remote_id, base)
    }
}

#[test]
fn a_change_made_elsewhere_during_a_sync_is_pulled_later() {
    let (d, mut laptop, mut phone) = pair();
    laptop.add("first");
    laptop.sync();
    phone.add("from the phone");
    phone.sync();
    // The laptop pulls the phone's note; right after its pull, a third
    // device saves another one.
    let path = d.path().join("shared.db");
    let snap = AccountSnapshot::of(&laptop.store, ACC);
    let third = move || {
        let mut b = SqliteBackend::open(&Config { path }).unwrap();
        let n = NoteData {
            id: new_id(),
            folder: vec![crate::model::DEFAULT_FOLDER.to_owned()],
            body: "from a third device".into(),
            created: Utc::now(),
            modified: Utc::now(),
            pinned: false,
            deleted: None,
        };
        b.save_note(None, None, &n).unwrap();
    };
    let mut racy = Racy { inner: &mut laptop.backend, after_pull: vec![Box::new(third)] };
    let out = sync_account(&mut racy, &snap, Utc::now()).unwrap();
    apply(&mut laptop.store, &snap, out);
    laptop.sync();
    assert_eq!(laptop.bodies(), vec!["first", "from a third device", "from the phone"]);
}

#[test]
fn a_create_whose_reply_was_lost_is_not_duplicated() {
    let (_d, mut laptop, _phone) = pair();
    let id = laptop.add("once");
    // The note reached the backend, but the laptop never heard back.
    let data = data_of(&laptop.store, &id);
    laptop.backend.save_note(None, None, &data).unwrap();
    laptop.sync();
    assert_eq!(laptop.bodies(), vec!["once"]);
    assert_eq!(laptop.backend.pull().unwrap().notes.len(), 1);
}

#[test]
fn offline_changes_on_both_sides_converge() {
    let (_d, mut laptop, mut phone) = pair();
    for i in 0..5 {
        laptop.add(&format!("laptop {i}"));
        phone.add(&format!("phone {i}"));
    }
    laptop.sync();
    phone.sync();
    laptop.sync();
    assert_eq!(laptop.bodies().len(), 10);
    assert_eq!(laptop.bodies(), phone.bodies());
}
