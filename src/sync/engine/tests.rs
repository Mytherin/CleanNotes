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
    });
    laptop.sync();
    assert!(laptop.store.folder_at(ACC, &["Work".into()]).is_none(), "not brought back by the pull");
    phone.sync();
    assert!(phone.store.folder_at(ACC, &["Work".into()]).is_none());
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
