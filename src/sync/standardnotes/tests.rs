//! The Standard Notes backend against the fake server ([`super::fake`]).

use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine;
use chrono::{DateTime, Utc};
use serde_json::{json, Value};

use super::fake::*;
use super::items::APP_DOMAIN;
use super::*;
use crate::model::{Account, Note, Store};
use crate::sync::engine::{apply, sync_account, AccountSnapshot};

const EMAIL: &str = "alice@example.com";
const PASSWORD: &str = "correct horse battery staple";

fn config(server: &FakeServer) -> Config {
    Config { server: server.url.clone(), email: EMAIL.into() }
}

fn password() -> SignInRequest {
    SignInRequest { password: PASSWORD.into(), mfa_code: None }
}

fn signed_in(server: &FakeServer) -> String {
    match sign_in(&config(server), &password()).unwrap() {
        SignInStep::Done { secret } => secret,
        other => panic!("{other:?}"),
    }
}

fn backend(server: &FakeServer) -> StandardNotesBackend {
    StandardNotesBackend::open(&config(server), Some(signed_in(server))).unwrap()
}

fn p(v: &[&str]) -> Vec<String> {
    v.iter().map(|s| s.to_string()).collect()
}

/// A time with millisecond precision (what survives a round trip).
fn ms(t: DateTime<Utc>) -> DateTime<Utc> {
    DateTime::from_timestamp_millis(t.timestamp_millis()).unwrap()
}

fn note(id: &str, folder: &[&str], body: &str) -> NoteData {
    NoteData {
        id: id.into(),
        folder: p(folder),
        body: body.into(),
        created: ms(Utc::now() - chrono::Duration::days(3)),
        modified: ms(Utc::now()),
        pinned: false,
        deleted: None,
    }
}

fn new_id() -> String {
    uuid::Uuid::new_v4().to_string()
}

/// A server with an items key, as an account made by the official client has.
fn server_with_key() -> FakeServer {
    let server = FakeServer::start(EMAIL, PASSWORD);
    server.state().add_items_key(true);
    server
}

#[test]
fn sign_in_keeps_the_session_and_master_key_but_not_the_password() {
    let server = FakeServer::start(EMAIL, PASSWORD);
    let secret = signed_in(&server);
    let session: Session = serde_json::from_str(&secret).unwrap();
    let s = server.state();
    assert_eq!(session.master_key, s.master_key);
    assert_eq!(session.user_uuid.as_deref(), Some(s.user_uuid.as_str()));
    assert!(session.key_params.starts_with(r#"{"identifier":"alice@example.com","pw_nonce":"#));
    assert!(!secret.contains(PASSWORD), "the password isn't kept");
    assert_eq!(s.log, vec!["POST /v2/login-params", "POST /v2/login"]);
}

#[test]
fn wrong_password() {
    let server = FakeServer::start(EMAIL, PASSWORD);
    let err = sign_in(&config(&server), &SignInRequest { password: "nope".into(), mfa_code: None }).unwrap_err();
    assert!(err.to_string().contains("incorrect"), "{err}");
}

#[test]
fn an_email_typed_differently_is_tried_cleaned_up() {
    let server = FakeServer::start(EMAIL, PASSWORD);
    let c = Config { email: " Alice@Example.com ".into(), ..config(&server) };
    assert!(matches!(sign_in(&c, &password()).unwrap(), SignInStep::Done { .. }));
    // The key comes from the identifier the server has, so the first try
    // works: login params and login, once each.
    assert_eq!(server.state().log.len(), 2);
}

#[test]
fn a_wrong_two_factor_code_says_so_and_is_tried_once() {
    let server = FakeServer::start(EMAIL, PASSWORD);
    server.state().mfa = true;
    let c = Config { email: "Alice@Example.com".into(), ..config(&server) };
    let req = SignInRequest { mfa_code: Some("000000".into()), ..password() };
    let err = sign_in(&c, &req).unwrap_err().to_string();
    assert!(err.contains("two-factor code"), "{err}");
    assert_eq!(server.state().log.len(), 1, "not tried again in lowercase (the code is spent)");
    // Wrong password: the real message, after one retry in lowercase.
    let req = SignInRequest { password: "nope".into(), mfa_code: Some(MFA_CODE.into()) };
    let err = sign_in(&c, &req).unwrap_err().to_string();
    assert!(err.contains("incorrect"), "{err}");
}

#[test]
fn two_factor_authentication() {
    let server = FakeServer::start(EMAIL, PASSWORD);
    server.state().mfa = true;
    assert_eq!(sign_in(&config(&server), &password()).unwrap(), SignInStep::NeedsMfaCode);
    let with = |code: &str| SignInRequest { mfa_code: Some(code.into()), ..password() };
    let err = sign_in(&config(&server), &with("000000")).unwrap_err();
    assert!(err.to_string().contains("isn't right"), "{err}");
    assert!(matches!(sign_in(&config(&server), &with(MFA_CODE)).unwrap(), SignInStep::Done { .. }));
}

#[test]
fn an_account_without_items_keys_gets_one() {
    let server = FakeServer::start(EMAIL, PASSWORD);
    backend(&server).pull().unwrap();
    let (keys, kp) = {
        let s = server.state();
        (s.live("SN|ItemsKey"), s.key_params.clone())
    };
    assert_eq!(keys.len(), 1);
    let s = server.state();
    let c = s.content(&keys[0]);
    assert_eq!(c["version"], "004");
    assert_eq!(c["isDefault"], true);
    assert_eq!(c["itemsKey"].as_str().unwrap().len(), 64);
    // Encrypted with the root key: the key params are in its authenticated data.
    let ad = s.items[&keys[0]]["content"].as_str().unwrap().split(':').nth(3).unwrap().to_owned();
    let ad = String::from_utf8(B64.decode(ad).unwrap()).unwrap();
    assert!(ad.starts_with(&format!(r#"{{"kp":{kp},"u":"#)), "{ad}");
    drop(s);
    // Another device uses that one.
    backend(&server).pull().unwrap();
    assert_eq!(server.state().live("SN|ItemsKey").len(), 1);
}

#[test]
fn notes_round_trip_encrypted_with_tags_as_folders() {
    let server = server_with_key();
    let mut b = backend(&server);
    b.pull().unwrap();
    let mut n = note(&new_id(), &["Work", "2026"], "# Plan #todo\n- [ ] ship it\n");
    n.pinned = true;
    let (rid, rev) = b.save_note(None, None, &n).unwrap();
    assert_eq!(rid, n.id, "a new note keeps the app's id");
    {
        let s = server.state();
        let item = &s.items[&rid];
        assert_eq!(item["content_type"], "Note");
        assert!(item["content"].as_str().unwrap().starts_with("004:"), "only ciphertext reaches the server");
        let c = s.content(&rid);
        assert_eq!(c["title"], "Plan #todo");
        assert_eq!(c["text"], n.body);
        assert_eq!(c["noteType"], "plain-text");
        assert_eq!(c["appData"][APP_DOMAIN]["pinned"], true);
        let (work, y2026) = (s.tag("Work"), s.tag("2026"));
        assert_eq!(items::parent_of(&s.content(&y2026)), Some(work.as_str()));
        assert!(items::tag_has_note(&s.content(&y2026), &rid));
        assert!(!items::tag_has_note(&s.content(&work), &rid));
    }
    let snap = b.pull().unwrap();
    let r = snap.notes.iter().find(|r| r.remote_id == rid).unwrap();
    assert_eq!(r.revision, rev);
    assert_eq!(r.data, n);
    let mut paths: Vec<Vec<String>> = snap.folders.iter().map(|f| f.path.clone()).collect();
    paths.sort();
    assert_eq!(paths, vec![p(&["Notes"]), p(&["Work"]), p(&["Work", "2026"])]);
    // Saving the same thing again changes nothing.
    let before = server.state().items[&rid]["updated_at_timestamp"].clone();
    assert_eq!(b.save_note(Some(&rid), Some(&rev), &n).unwrap().1, rev);
    assert_eq!(server.state().items[&rid]["updated_at_timestamp"], before);
}

#[test]
fn notes_written_in_standard_notes_keep_what_the_app_does_not_own() {
    let server = server_with_key();
    let id = server.state().add(
        "Note",
        json!({
            "title": "Groceries",
            "text": "eggs\nmilk",
            "references": [{"uuid": "x", "content_type": "Note", "reference_type": "NoteToNote"}],
            "appData": {APP_DOMAIN: {"pinned": true, "client_updated_at": "Sun Nov 15 2026 12:03:00 GMT+0100 (Central European Standard Time)", "archived": false, "custom": 7}},
            "spellcheck": false,
            "editorWidth": "wide",
        }),
    );
    let mut b = backend(&server);
    let snap = b.pull().unwrap();
    let r = &snap.notes[0];
    assert_eq!(r.data.id, id);
    assert_eq!(r.data.body, "# Groceries\neggs\nmilk");
    assert!(r.data.pinned);
    assert_eq!(r.data.folder, p(&["Notes"]));
    assert_eq!(r.data.modified, DateTime::parse_from_rfc3339("2026-11-15T11:03:00Z").unwrap().with_timezone(&Utc));
    let mut data = r.data.clone();
    data.body = "# Groceries\neggs\nmilk\nbread".into();
    data.modified = ms(Utc::now());
    b.save_note(Some(&id), Some(&r.revision), &data).unwrap();
    let c = server.state().content(&id);
    assert_eq!(c["title"], "Groceries");
    assert_eq!(c["text"], "eggs\nmilk\nbread", "the title isn't written into the text");
    assert_eq!(c["spellcheck"], false);
    assert_eq!(c["editorWidth"], "wide");
    assert_eq!(c["references"].as_array().unwrap().len(), 1);
    assert_eq!(c["appData"][APP_DOMAIN]["custom"], 7);
    assert_eq!(c["appData"][APP_DOMAIN]["pinned"], true);
    assert_eq!(b.pull().unwrap().notes[0].data.body, "# Groceries\neggs\nmilk\nbread");
}

#[test]
fn outdated_saves_are_conflicts() {
    let server = server_with_key();
    let mut b = backend(&server);
    b.pull().unwrap();
    let n = note(&new_id(), &["Notes"], "a");
    let (rid, rev) = b.save_note(None, None, &n).unwrap();
    // Changed by another device meanwhile.
    server.state().edit(&rid, |c| {
        c["title"] = json!("b");
        c["text"] = json!("b");
    });
    let err = b.save_note(Some(&rid), Some(&rev), &NoteData { body: "c".into(), ..n.clone() }).unwrap_err();
    assert!(err.is::<Conflict>(), "{err:#}");
    let snap = b.pull().unwrap();
    let r = &snap.notes[0];
    assert_eq!(r.data.body, "b");
    b.save_note(Some(&rid), Some(&r.revision), &NoteData { body: "c".into(), ..n.clone() }).unwrap();
    // A change the backend hadn't heard of yet: the server says conflict.
    let rev = b.pull().unwrap().notes[0].revision.clone();
    server.state().edit(&rid, |c| {
        c["title"] = json!("d");
        c["text"] = json!("d");
    });
    let err = b.save_note(Some(&rid), Some(&rev), &NoteData { body: "e".into(), ..n.clone() }).unwrap_err();
    assert!(err.is::<Conflict>(), "{err:#}");
    assert_eq!(server.state().content(&rid)["text"], "d");
    // Deleted elsewhere: also a conflict, for the engine to decide.
    server.state().delete(&rid);
    b.pull().unwrap();
    assert!(b.save_note(Some(&rid), None, &n).unwrap_err().is::<Conflict>());
}

#[test]
fn tags_nest_and_notes_move_between_them() {
    let server = server_with_key();
    let (a, c, work, old) = {
        let mut s = server.state();
        let a = s.add("Note", json!({"title": "A", "text": "A", "references": []}));
        let _b = s.add("Note", json!({"title": "B", "text": "B", "references": []}));
        let c = s.add("Note", json!({"title": "C", "text": "C", "references": []}));
        let work = s.add_tag("Work", None, &[&c]);
        let old = s.add_tag("Old", Some(&work), &[&a, &c]);
        (a, c, work, old)
    };
    let mut b = backend(&server);
    let snap = b.pull().unwrap();
    let folder = |snap: &Snapshot, title: &str| snap.notes.iter().find(|n| n.data.body == title).unwrap().data.folder.clone();
    assert_eq!(folder(&snap, "A"), p(&["Work", "Old"]));
    assert_eq!(folder(&snap, "B"), p(&["Notes"]), "untagged notes are in the default folder");
    assert_eq!(folder(&snap, "C"), p(&["Work", "Old"]), "in two tags: the deeper one");
    let ids: Vec<(&str, Vec<String>)> = snap.folders.iter().map(|f| (f.remote_id.as_str(), f.path.clone())).collect();
    assert!(ids.contains(&(DEFAULT_FOLDER_ID, p(&["Notes"]))));
    assert!(ids.contains(&(work.as_str(), p(&["Work"]))));
    assert!(ids.contains(&(old.as_str(), p(&["Work", "Old"]))));

    // Moving C to Work takes it out of Old; moving A to Notes untags it.
    let r = snap.notes.iter().find(|n| n.remote_id == c).unwrap();
    let rev = b.save_note(Some(&c), Some(&r.revision), &NoteData { folder: p(&["Work"]), ..r.data.clone() }).unwrap().1;
    assert_ne!(rev, r.revision, "the revision follows the folder");
    let r = snap.notes.iter().find(|n| n.remote_id == a).unwrap();
    b.save_note(Some(&a), Some(&r.revision), &NoteData { folder: p(&["Notes"]), ..r.data.clone() }).unwrap();
    {
        let s = server.state();
        assert!(items::tag_has_note(&s.content(&work), &c));
        assert!(!items::tag_has_note(&s.content(&old), &c));
        assert!(!items::tag_has_note(&s.content(&old), &a));
    }
    let snap = b.pull().unwrap();
    assert_eq!(folder(&snap, "C"), p(&["Work"]));
    assert_eq!(folder(&snap, "A"), p(&["Notes"]));

    // Folders: made once, renamed, moved, deleted.
    assert_eq!(b.create_folder(&p(&["Work"])).unwrap(), work);
    assert_eq!(b.create_folder(&p(&["Notes"])).unwrap(), DEFAULT_FOLDER_ID);
    let deep = b.create_folder(&p(&["Work", "Old", "Deeper"])).unwrap();
    assert_eq!(b.create_folder(&p(&["Work", "Old", "Deeper"])).unwrap(), deep);
    b.rename_folder(&work, &p(&["Work"]), &p(&["Projects"])).unwrap();
    b.rename_folder(&old, &p(&["Projects", "Old"]), &p(&["Archive"])).unwrap();
    let mut paths: Vec<Vec<String>> = b.pull().unwrap().folders.into_iter().map(|f| f.path).collect();
    paths.sort();
    assert_eq!(paths, vec![p(&["Archive"]), p(&["Archive", "Deeper"]), p(&["Notes"]), p(&["Projects"])]);
    assert_eq!(folder(&b.pull().unwrap(), "C"), p(&["Projects"]));
    b.delete_folder(&deep, &p(&["Archive", "Deeper"])).unwrap();
    b.delete_folder(&deep, &p(&["Archive", "Deeper"])).unwrap();
    assert!(server.state().items[&deep]["deleted"] == true);
    assert_eq!(b.pull().unwrap().folders.len(), 3);
}

#[test]
fn recently_deleted_is_the_trash_and_deleting_for_good_deletes() {
    let server = server_with_key();
    let mut b = backend(&server);
    b.pull().unwrap();
    let mut n = note(&new_id(), &["Notes"], "bye");
    let (rid, rev) = b.save_note(None, None, &n).unwrap();
    let gone = ms(Utc::now());
    n.deleted = Some(gone);
    b.save_note(Some(&rid), Some(&rev), &n).unwrap();
    assert_eq!(server.state().content(&rid)["trashed"], true);
    let r = b.pull().unwrap().notes.remove(0);
    assert_eq!(r.data.deleted, Some(gone));
    // Put back.
    n.deleted = None;
    b.save_note(Some(&rid), Some(&r.revision), &n).unwrap();
    assert_eq!(server.state().content(&rid)["trashed"], false);
    // Trashed in Standard Notes: in Recently Deleted since it changed.
    server.state().edit(&rid, |c| c["trashed"] = json!(true));
    let r = b.pull().unwrap().notes.remove(0);
    assert_eq!(r.data.deleted, Some(r.data.modified));
    // A delete based on an older version is refused: the edit wins.
    let stale = r.revision.clone();
    server.state().edit(&rid, |c| c["text"] = json!("edited elsewhere"));
    let r = b.pull().unwrap().notes.remove(0);
    assert!(b.delete_note(&rid, Some(&stale)).unwrap_err().is::<Conflict>());
    b.delete_note(&rid, Some(&r.revision)).unwrap();
    b.delete_note(&rid, None).unwrap();
    assert_eq!(server.state().items[&rid]["deleted"], true);
    assert!(b.pull().unwrap().notes.is_empty());
}

#[test]
fn super_notes_are_read_only_and_never_overwritten() {
    let server = server_with_key();
    let lexical = r#"{"root":{"type":"root","children":[{"type":"paragraph","children":[{"type":"text","text":"Pack light"}]},{"type":"list","listType":"check","children":[{"type":"listitem","checked":false,"children":[{"type":"text","text":"Tickets"}]}]}]}}"#;
    let id = server.state().add(
        "Note",
        json!({"title": "Trip", "text": lexical, "noteType": "super", "editorIdentifier": "com.standardnotes.super-editor", "references": []}),
    );
    let mut b = backend(&server);
    let r = b.pull().unwrap().notes.remove(0);
    assert!(r.data.body.starts_with("# Trip\n> Shown read-only: this is a Super note"), "{}", r.data.body);
    assert!(r.data.body.ends_with("Pack light\n- [ ] Tickets"));
    // Pinning and moving work on the original, text untouched.
    let pinned = NoteData { pinned: true, folder: p(&["Travel"]), ..r.data.clone() };
    let (rid, rev) = b.save_note(Some(&id), Some(&r.revision), &pinned).unwrap();
    assert_eq!(rid, id);
    {
        let s = server.state();
        let c = s.content(&id);
        assert_eq!(c["text"], lexical);
        assert_eq!(c["noteType"], "super");
        assert_eq!(c["appData"][APP_DOMAIN]["pinned"], true);
        assert!(items::tag_has_note(&s.content(&s.tag("Travel")), &id));
    }
    // Editing the text makes a new plain note; the Super note stays as it was.
    let edited = NoteData { body: format!("{}\n- [ ] Passport", pinned.body), ..pinned.clone() };
    let (copy, _) = b.save_note(Some(&id), Some(&rev), &edited).unwrap();
    assert_ne!(copy, id);
    let s = server.state();
    assert_eq!(s.content(&id)["text"], lexical);
    let c = s.content(&copy);
    assert_eq!(c["noteType"], "plain-text");
    assert_eq!(c["title"], "Trip");
    assert_eq!(c["text"], "# Trip\nPack light\n- [ ] Tickets\n- [ ] Passport");
    assert!(items::tag_has_note(&s.content(&s.tag("Travel")), &copy));
}

#[test]
fn downloads_come_in_pages_and_then_only_changes() {
    let server = server_with_key();
    {
        let mut s = server.state();
        s.limit = 3;
        for i in 0..10 {
            s.add("Note", json!({"title": format!("N{i}"), "text": format!("N{i}"), "references": []}));
        }
    }
    let mut b = backend(&server);
    let c0 = b.cursor().unwrap().unwrap();
    assert_eq!(b.pull().unwrap().notes.len(), 10);
    assert!(server.state().log.iter().filter(|l| *l == "POST /v1/items").count() >= 4, "paged");
    // Nothing changed: the same marker, one small request.
    let c1 = b.cursor().unwrap().unwrap();
    assert_ne!(c0, c1, "the first marker of a newly opened account is never trusted");
    server.state().log.clear();
    assert_eq!(b.cursor().unwrap().unwrap(), c1);
    assert_eq!(server.state().log, vec!["POST /v1/items"]);
    // Changed elsewhere: a different marker, until the change is pulled.
    let victim = server.state().live("Note")[0].clone();
    server.state().edit(&victim, |c| c["text"] = json!("changed"));
    let c2 = b.cursor().unwrap().unwrap();
    assert_ne!(c2, c1);
    assert_ne!(b.cursor().unwrap().unwrap(), c2, "unseen changes never match");
    let snap = b.pull().unwrap();
    assert!(snap.notes.iter().any(|n| n.data.body.ends_with("\nchanged")));
    let c3 = b.cursor().unwrap().unwrap();
    assert_eq!(b.cursor().unwrap().unwrap(), c3);
    // Deleted elsewhere: gone after the next pull.
    server.state().delete(&victim);
    assert_ne!(b.cursor().unwrap().unwrap(), c3);
    assert_eq!(b.pull().unwrap().notes.len(), 9);
}

#[test]
fn expired_sessions_are_refreshed_and_handed_back() {
    let server = server_with_key();
    let mut b = backend(&server);
    b.pull().unwrap();
    assert_eq!(b.renewed_secret(), None);
    server.state().expire_tokens();
    b.pull().unwrap();
    let renewed = b.renewed_secret().expect("a renewed session");
    assert!(server.state().log.contains(&"POST /v1/sessions/refresh".to_owned()));
    let mut b2 = StandardNotesBackend::open(&config(&server), Some(renewed)).unwrap();
    b2.pull().unwrap();
}

#[test]
fn an_ended_session_needs_signing_in_again() {
    let server = server_with_key();
    let mut b = backend(&server);
    b.pull().unwrap();
    server.state().revoke_sessions();
    let err = b.pull().unwrap_err();
    assert!(err.is::<NeedsSignIn>(), "{err:#}");
    // Expired and the refresh token no longer valid.
    let mut b = backend(&server);
    server.state().expire_tokens();
    b.client.session.refresh_token = "stale".into();
    let err = b.pull().unwrap_err();
    assert!(err.is::<NeedsSignIn>(), "{err:#}");
    assert!(StandardNotesBackend::open(&config(&server), None).err().unwrap().is::<NeedsSignIn>());
    assert!(StandardNotesBackend::open(&config(&server), Some("{".into())).err().unwrap().is::<NeedsSignIn>());
}

#[test]
fn a_changed_password_needs_signing_in_again() {
    let server = server_with_key();
    let mut b = backend(&server);
    // Another client re-encrypted the items keys with a new root key.
    {
        let mut s = server.state();
        s.master_key = crypto::random_hex(32);
        let keys = s.live("SN|ItemsKey");
        for k in keys {
            s.delete(&k);
        }
        s.add_items_key(true);
    }
    let err = b.pull().unwrap_err();
    assert!(err.is::<NeedsSignIn>(), "{err:#}");
}

// ---- The engine, two devices, one account ----------------------------------

struct Device {
    store: Store,
    backend: StandardNotesBackend,
}

impl Device {
    fn new(server: &FakeServer) -> Device {
        let mut store = Store::new_install();
        store.accounts.push(Account {
            id: "sn".into(),
            name: "Standard Notes".into(),
            // The engine doesn't look at the config.
            backend: crate::sync::BackendConfig::Local,
            enabled: true,
            cursor: None,
            last_sync: None,
        });
        store.ensure_local();
        Device { store, backend: backend(server) }
    }

    fn sync(&mut self) -> Vec<String> {
        let snap = AccountSnapshot::of(&self.store, "sn");
        let out = sync_account(&mut self.backend, &snap, Utc::now()).unwrap();
        assert_eq!(out.error, None);
        apply(&mut self.store, &snap, out)
    }

    fn add(&mut self, folder: &[&str], body: &str) -> String {
        let f = self.store.ensure_folder_at("sn", &p(folder));
        let n = Note::new("sn", &f, body);
        let id = n.id.clone();
        self.store.notes.push(n);
        id
    }

    fn edit(&mut self, id: &str, body: &str) {
        let n = self.store.note_mut(id).unwrap();
        n.body = body.into();
        n.touch();
    }
}

#[test]
fn a_note_deleted_in_standard_notes_but_edited_here_is_uploaded_again() {
    let server = server_with_key();
    let mut laptop = Device::new(&server);
    let id = laptop.add(&["Notes"], "# Keep\nv1\n");
    laptop.sync();
    let rid = laptop.store.note(&id).unwrap().sync.remote_id.clone().unwrap();
    server.state().delete(&rid);
    laptop.edit(&id, "# Keep\nv2 edited here\n");
    // `sync` asserts it ends without an error ("notes kept changing").
    laptop.sync();
    laptop.sync();
    let s = server.state();
    let texts: Vec<String> = s.live(NOTE).iter().map(|u| s.content(u)["text"].as_str().unwrap_or("").to_owned()).collect();
    assert_eq!(texts.len(), 1);
    assert!(texts[0].contains("v2 edited here"), "{texts:?}");
}

#[test]
fn deleting_for_good_respects_edits_made_since_and_works_after_a_restart() {
    let server = server_with_key();
    let mut laptop = Device::new(&server);
    let mut phone = Device::new(&server);
    let a = laptop.add(&["Work"], "# A\n");
    let b = laptop.add(&["Work"], "# B\n");
    laptop.sync();
    phone.sync();
    // The phone edits A after the laptop's last sync; the laptop (whose
    // backend stayed open, with what it saw then) deletes A for good.
    phone.edit(&a, "# A\nedited on the phone\n");
    phone.sync();
    laptop.store.purge_note(&a);
    let notices = laptop.sync();
    assert_eq!(notices.len(), 1, "the edit wins: {notices:?}");
    assert_eq!(laptop.store.note(&a).unwrap().body, "# A\nedited on the phone\n");
    // After a restart (a fresh backend), deleting a note in a folder that
    // nobody changed just deletes it.
    laptop.backend = backend(&server);
    laptop.store.purge_note(&b);
    let notices = laptop.sync();
    assert!(notices.is_empty(), "{notices:?}");
    phone.sync();
    assert!(phone.store.note(&b).is_none());
}

#[test]
fn a_failed_page_doesnt_skip_what_it_held() {
    let server = server_with_key();
    let mut b = backend(&server);
    assert!(b.pull().unwrap().notes.is_empty());
    {
        let mut s = server.state();
        s.limit = 3;
        for i in 0..10 {
            s.add("Note", json!({"title": format!("N{i}"), "text": format!("N{i}"), "references": []}));
        }
        s.fail_next_page = true;
    }
    assert!(b.pull().is_err(), "the second page fails");
    assert_eq!(b.pull().unwrap().notes.len(), 10, "nothing skipped on the next try");
}

#[test]
fn a_folder_made_elsewhere_since_the_last_sync_is_reused() {
    let server = server_with_key();
    let mut b = backend(&server);
    b.pull().unwrap();
    // Another client makes the tag after this backend's last look.
    let theirs = server.state().add_tag("Projects", None, &[]);
    assert_eq!(b.create_folder(&p(&["Projects"])).unwrap(), theirs);
    assert_eq!(server.state().live("Tag").len(), 1, "no second Projects tag");
}

#[test]
fn a_tag_parent_cycle_makes_short_paths_and_no_new_tags() {
    let server = server_with_key();
    let (a, b_tag) = {
        let mut s = server.state();
        let a = s.add_tag("A", None, &[]);
        let b = s.add_tag("B", Some(&a), &[]);
        // Moved under each other on two devices at once.
        s.edit(&a, |c| {
            c["references"] = json!([{"uuid": b, "content_type": "Tag", "reference_type": "TagToParentTag"}]);
        });
        (a, b)
    };
    let mut laptop = Device::new(&server);
    laptop.sync();
    laptop.sync();
    let paths: Vec<Vec<String>> = laptop.store.folders.iter().filter(|f| f.account == "sn").map(|f| laptop.store.folder_path(&f.id)).collect();
    assert!(paths.iter().all(|p| p.len() <= 2), "{paths:?}");
    let live = server.state().live("Tag");
    assert_eq!(live.len(), 2, "no tags made for a cycle");
    assert!(live.contains(&a) && live.contains(&b_tag));
}

#[test]
fn a_tag_that_cant_be_decrypted_keeps_its_folder_here() {
    let server = server_with_key();
    let mut laptop = Device::new(&server);
    let id = laptop.add(&["Work"], "# In work\n");
    laptop.sync();
    {
        // Re-encrypted elsewhere with a key this device doesn't have.
        let mut s = server.state();
        let tag = s.tag("Work");
        let t = s.items[&tag]["updated_at_timestamp"].as_i64().unwrap() + 1;
        let it = s.items.get_mut(&tag).unwrap();
        it["items_key_id"] = json!("a-key-we-dont-have");
        it["updated_at_timestamp"] = json!(t);
    }
    laptop.sync();
    let n = laptop.store.note(&id).unwrap();
    assert_eq!(laptop.store.folder_path(&n.folder), vec!["Work"]);
}

#[test]
fn editing_a_read_only_note_leaves_no_banner_behind() {
    let server = server_with_key();
    let lexical = r#"{"root":{"type":"root","children":[{"type":"paragraph","children":[{"type":"text","text":"Pack light"}]}]}}"#;
    server.state().add(
        "Note",
        json!({"title": "Trip", "text": lexical, "noteType": "super", "editorIdentifier": "com.standardnotes.super-editor", "references": []}),
    );
    let mut laptop = Device::new(&server);
    laptop.sync();
    let id = laptop.store.notes.iter().find(|n| n.account == "sn").unwrap().id.clone();
    let body = laptop.store.note(&id).unwrap().body.clone();
    assert!(body.contains("Shown read-only"));
    laptop.edit(&id, &format!("{body}\nmore"));
    laptop.sync();
    laptop.sync();
    // The edited copy here has the text as saved, without the banner…
    let mine = laptop.store.notes.iter().find(|n| n.account == "sn" && n.body.contains("more")).unwrap();
    assert!(!mine.body.contains("Shown read-only"), "{}", mine.body);
    // …so a later edit doesn't upload it.
    let mine = mine.id.clone();
    let b = laptop.store.note(&mine).unwrap().body.clone();
    laptop.edit(&mine, &format!("{b}\nand more"));
    laptop.sync();
    let s = server.state();
    assert!(s.live(NOTE).iter().all(|u| !s.content(u)["text"].as_str().unwrap_or("").contains("Shown read-only")));
}

#[test]
fn malformed_hex_from_the_server_is_an_error_not_a_panic() {
    assert!(crypto::unhex("aé0").is_err());
    assert!(crypto::unhex("zz").is_err());
    assert_eq!(crypto::unhex("0aff").unwrap(), vec![0x0a, 0xff]);
}

#[test]
fn two_devices_sync_through_standard_notes() {
    let server = server_with_key();
    let mut laptop = Device::new(&server);
    let mut phone = Device::new(&server);
    let id = laptop.add(&["Notes"], "# List\neggs\nmilk\n");
    let work = laptop.add(&["Work"], "# Standup\n");
    laptop.sync();
    phone.sync();
    assert_eq!(phone.store.note(&id).unwrap().body, "# List\neggs\nmilk\n");
    let wf = phone.store.note(&work).unwrap().folder.clone();
    assert_eq!(phone.store.folder_path(&wf), vec!["Work"]);
    assert_eq!(server.state().live("Tag").len(), 1);

    // Both edit: merged.
    laptop.edit(&id, "# List\neggs (12)\nmilk\n");
    phone.edit(&id, "# List\neggs\nmilk\nbread\n");
    laptop.sync();
    assert!(phone.sync().is_empty());
    laptop.sync();
    assert_eq!(laptop.store.note(&id).unwrap().body, "# List\neggs (12)\nmilk\nbread\n");
    assert_eq!(phone.store.note(&id).unwrap().body, laptop.store.note(&id).unwrap().body);

    // Moved on one device: moved on the other.
    let f = phone.store.ensure_folder_at("sn", &p(&["Home"]));
    phone.store.note_mut(&id).unwrap().folder = f;
    phone.store.note_mut(&id).unwrap().touch();
    phone.sync();
    laptop.sync();
    let lf = laptop.store.note(&id).unwrap().folder.clone();
    assert_eq!(laptop.store.folder_path(&lf), vec!["Home"]);

    // Nothing changed: one request to learn that.
    laptop.sync();
    server.state().log.clear();
    laptop.sync();
    assert_eq!(server.state().log, vec!["POST /v1/items"]);
}

#[test]
fn notes_typed_into_standard_notes_reach_the_app() {
    let server = server_with_key();
    let mut laptop = Device::new(&server);
    laptop.sync();
    let id = server.state().add("Note", json!({"title": "From the web", "text": "hello", "references": []}));
    laptop.sync();
    let n = laptop.store.note(&id).expect("the note arrived");
    assert_eq!(n.body, "# From the web\nhello");
    // Edited in the app: written back without a repeated title.
    laptop.edit(&id, "# From the web\nhello there");
    laptop.sync();
    let c: Value = server.state().content(&id);
    assert_eq!(c["text"], "hello there");
}
