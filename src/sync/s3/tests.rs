//! The S3 backend against the fake ([`super::fake`]).

use chrono::{DateTime, Utc};

use super::fake::*;
use super::*;
use crate::model::{Account, Note, Store};
use crate::sync::engine::{apply, sync_account, AccountSnapshot};

const PREFIX: &str = "My Notes/work";

fn config(server: &FakeS3) -> Config {
    Config { bucket: BUCKET.into(), region: REGION.into(), prefix: format!("/{PREFIX}/"), endpoint: Some(server.url.clone()) }
}

fn keys() -> Keys {
    Keys { access_key_id: ACCESS_KEY.into(), secret_access_key: SECRET_KEY.into() }
}

fn backend(server: &FakeS3) -> S3Backend {
    S3Backend::open(&config(server), Some(keys().to_secret())).unwrap()
}

fn note(id: &str, folder: &[&str], body: &str) -> NoteData {
    NoteData {
        id: id.into(),
        folder: folder.iter().map(|s| s.to_string()).collect(),
        body: body.into(),
        created: DateTime::from_timestamp_millis(1_763_200_000_000).unwrap(),
        modified: DateTime::from_timestamp_millis(1_763_200_100_000).unwrap(),
        pinned: true,
        deleted: None,
    }
}

#[test]
fn notes_are_markdown_files_in_the_folder() {
    let server = FakeS3::start();
    let mut b = backend(&server);
    let n = note("n1", &["Notes", "Recipes"], "# Pancakes\nflour\n");
    let (rid, rev) = b.save_note(None, None, &n).unwrap();
    assert_eq!(rid, "n1");
    let obj = server.state().objects.get(&format!("{PREFIX}/notes/n1.md")).cloned().unwrap();
    assert_eq!(obj.etag, rev);
    assert!(obj.body.starts_with("---\nid: n1\nfolder: [Notes, Recipes]\n"));
    assert!(obj.body.ends_with("---\n# Pancakes\nflour\n"));
    assert!(server.state().objects.contains_key(&format!("{PREFIX}/head")));

    // A fresh backend (another device) reads it back.
    let snap = backend(&server).pull().unwrap();
    assert_eq!(snap.notes, vec![RemoteNote { remote_id: "n1".into(), revision: rev, data: n }]);
}

#[test]
fn outdated_saves_and_deletes_are_conflicts() {
    let server = FakeS3::start();
    let mut b = backend(&server);
    let (rid, rev1) = b.save_note(None, None, &note("n1", &["Notes"], "v1")).unwrap();
    let (_, rev2) = b.save_note(Some(&rid), Some(&rev1), &note("n1", &["Notes"], "v2")).unwrap();
    let e = b.save_note(Some(&rid), Some(&rev1), &note("n1", &["Notes"], "v3")).unwrap_err();
    assert!(e.is::<Conflict>());
    let e = b.delete_note(&rid, Some(&rev1)).unwrap_err();
    assert!(e.is::<Conflict>());
    b.delete_note(&rid, Some(&rev2)).unwrap();
    // A tombstone stays, and the note is gone from the snapshot.
    assert!(server.state().objects[&format!("{PREFIX}/notes/n1.md")].body.contains("gone:"));
    assert!(backend(&server).pull().unwrap().notes.is_empty());
    // Deleting it again, or one that never existed, is fine.
    b.delete_note(&rid, Some(&rev2)).unwrap();
    b.delete_note("never", Some("\"x\"")).unwrap();
}

#[test]
fn a_deleted_note_can_come_back() {
    let server = FakeS3::start();
    let mut b = backend(&server);
    let (rid, rev) = b.save_note(None, None, &note("n1", &["Notes"], "v1")).unwrap();
    b.delete_note(&rid, Some(&rev)).unwrap();
    // Edited elsewhere meanwhile: the engine creates it again.
    let (rid2, _) = b.save_note(None, None, &note("n1", &["Notes"], "v2")).unwrap();
    assert_eq!(rid2, "n1");
    assert_eq!(backend(&server).pull().unwrap().notes[0].data.body, "v2");
}

#[test]
fn creates_never_overwrite() {
    let server = FakeS3::start();
    let mut b = backend(&server);
    b.save_note(None, None, &note("n1", &["Notes"], "v1")).unwrap();
    // The same create again (a lost reply): a conflict, matched up on pull.
    assert!(b.save_note(None, None, &note("n1", &["Notes"], "v1")).unwrap_err().is::<Conflict>());
    // A different note under the same name goes next to it.
    server.put(&format!("{PREFIX}/notes/n2.md"), "---\nid: other\n---\nhand made");
    let (rid, _) = b.save_note(None, None, &note("n2", &["Notes"], "mine")).unwrap();
    assert!(rid.starts_with("n2-"));
    assert_eq!(server.state().objects[&format!("{PREFIX}/notes/n2.md")].body, "---\nid: other\n---\nhand made");
}

#[test]
fn hand_written_and_broken_objects() {
    let server = FakeS3::start();
    server.put(&format!("{PREFIX}/notes/shopping.md"), "# Shopping\neggs\n");
    server.put(&format!("{PREFIX}/folders/f1.json"), "not json");
    server.put(&format!("{PREFIX}/notes/readme.txt"), "ignored");
    server.put("elsewhere/notes/x.md", "outside the folder");
    let snap = backend(&server).pull().unwrap();
    assert_eq!(snap.notes.len(), 1);
    assert_eq!(snap.notes[0].data.id, "s3-shopping");
    assert_eq!(snap.unreadable, vec!["f1".to_owned()]);
}

#[test]
fn only_changed_objects_are_downloaded() {
    let server = FakeS3::start();
    let mut b = backend(&server);
    for i in 0..40 {
        b.save_note(None, None, &note(&format!("n{i:02}"), &["Notes"], "x")).unwrap();
    }
    server.state().page_size = 7;
    let mut reader = backend(&server);
    assert_eq!(reader.pull().unwrap().notes.len(), 40);
    let gets = |s: &FakeS3| s.state().log.iter().filter(|l| l.starts_with("GET ") && l.len() > 4).count();
    assert_eq!(gets(&server), 40);
    server.state().log.clear();
    b.save_note(Some("n05"), None, &note("n05", &["Notes"], "changed")).unwrap();
    let snap = reader.pull().unwrap();
    assert_eq!(gets(&server), 1);
    assert_eq!(snap.notes.iter().find(|n| n.remote_id == "n05").unwrap().data.body, "changed");
}

#[test]
fn the_cursor_moves_with_every_write() {
    let server = FakeS3::start();
    let mut b = backend(&server);
    let c0 = b.cursor().unwrap();
    assert_eq!(c0, b.cursor().unwrap());
    b.save_note(None, None, &note("n1", &["Notes"], "x")).unwrap();
    let c1 = b.cursor().unwrap();
    assert_ne!(c0, c1);
    b.create_folder(&["Work".into()]).unwrap();
    assert_ne!(c1, b.cursor().unwrap());
}

#[test]
fn folders_rename_with_their_subfolders() {
    let server = FakeS3::start();
    let mut b = backend(&server);
    let work = b.create_folder(&["Work".into()]).unwrap();
    let sub = b.create_folder(&["Work".into(), "2026".into()]).unwrap();
    assert_eq!(b.create_folder(&["Work".into()]).unwrap(), work, "an existing folder is reused");
    b.rename_folder(&work, &["Work".into()], &["Job".into()]).unwrap();
    let mut paths: Vec<Vec<String>> = backend(&server).pull().unwrap().folders.into_iter().map(|f| f.path).collect();
    paths.sort();
    assert_eq!(paths, vec![vec!["Job".to_owned()], vec!["Job".to_owned(), "2026".to_owned()]]);
    b.delete_folder(&sub, &[]).unwrap();
    b.delete_folder(&sub, &[]).unwrap();
    assert_eq!(backend(&server).pull().unwrap().folders.len(), 1);
}

#[test]
fn checking_the_bucket() {
    let server = FakeS3::start();
    let secret = check(&config(&server), &keys()).unwrap();
    assert_eq!(serde_json::from_str::<Keys>(&secret).unwrap(), keys());

    let wrong = Keys { secret_access_key: "nope".into(), ..keys() };
    let e = check(&config(&server), &wrong).unwrap_err();
    assert!(e.chain().any(|c| c.is::<NeedsSignIn>()), "{e:#}");

    let other = Config { bucket: "missing".into(), ..config(&server) };
    let e = check(&other, &keys()).unwrap_err();
    assert!(format!("{e:#}").contains("no bucket"), "{e:#}");

    server.state().ignore_conditions = true;
    let e = check(&config(&server), &keys()).unwrap_err();
    assert!(format!("{e:#}").contains("conditional writes"), "{e:#}");
}

#[test]
fn amazon_urls() {
    let c = Config { bucket: "my-notes".into(), region: "eu-west-1".into(), prefix: "".into(), endpoint: None };
    assert_eq!(c.url_parts("notes/a b.md"), ("https://my-notes.s3.eu-west-1.amazonaws.com".into(), "/notes/a%20b.md".into()));
    let c = Config { bucket: "my.notes".into(), ..c };
    assert_eq!(c.url_parts("head"), ("https://s3.eu-west-1.amazonaws.com".into(), "/my.notes/head".into()));
    assert_eq!(Config { prefix: " /a/b/ ".into(), ..c.clone() }.root(), "a/b/");
    assert_eq!(c.root(), "");
}

#[test]
fn xml_entities() {
    assert_eq!(decode("&quot;abc&quot; &amp; &#65;&#x42; &bogus;"), "\"abc\" & AB &bogus;");
}

struct Device {
    store: Store,
    backend: S3Backend,
}

impl Device {
    fn new(server: &FakeS3) -> Device {
        let mut store = Store::new_install();
        store.accounts.push(Account {
            id: "s3".into(),
            name: "Work notes".into(),
            backend: crate::sync::BackendConfig::S3(config(server)),
            enabled: true,
            cursor: None,
            last_sync: None,
        });
        store.ensure_local();
        Device { store, backend: backend(server) }
    }

    fn sync(&mut self) -> Vec<String> {
        let snap = AccountSnapshot::of(&self.store, "s3");
        let out = sync_account(&mut self.backend, &snap, Utc::now()).unwrap();
        assert_eq!(out.error, None);
        let cursor = out.cursor.clone();
        let notices = apply(&mut self.store, &snap, out);
        assert_eq!(self.store.account("s3").unwrap().cursor, cursor);
        notices
    }
}

#[test]
fn two_devices_sync_through_s3() {
    let server = FakeS3::start();
    let mut laptop = Device::new(&server);
    let mut phone = Device::new(&server);
    let folder = laptop.store.ensure_folder_at("s3", &["Recipes".into()]);
    let n = Note::new("s3", &folder, "# Pancakes\nflour\neggs\n");
    let id = n.id.clone();
    laptop.store.notes.push(n);
    laptop.sync();
    phone.sync();
    let pn = phone.store.note(&id).unwrap();
    assert_eq!(pn.body, "# Pancakes\nflour\neggs\n");
    assert_eq!(phone.store.folder_path(&pn.folder), vec!["Recipes"]);

    // Edits to different paragraphs on both: merged.
    for (dev, body) in [(&mut laptop, "# Pancakes\nflour (200 g)\neggs\n"), (&mut phone, "# Pancakes\nflour\neggs\nmilk\n")] {
        let n = dev.store.note_mut(&id).unwrap();
        n.body = body.into();
        n.touch();
    }
    laptop.sync();
    assert!(phone.sync().is_empty());
    laptop.sync();
    assert_eq!(laptop.store.note(&id).unwrap().body, "# Pancakes\nflour (200 g)\neggs\nmilk\n");
    assert_eq!(phone.store.note(&id).unwrap().body, "# Pancakes\nflour (200 g)\neggs\nmilk\n");

    // Nothing changed: one HEAD, no listing.
    server.state().log.clear();
    laptop.sync();
    let log = server.state().log.clone();
    assert!(log.iter().all(|l| l.starts_with("HEAD ")), "{log:?}");
}
