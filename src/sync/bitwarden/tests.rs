//! The Bitwarden backend against the fake server ([`super::fake`]).

use chrono::Utc;
use serde_json::json;

use super::crypto::Kdf;
use super::fake::*;
use super::*;
use crate::model::{Account, Note, Store};
use crate::sync::engine::{apply, sync_account, AccountSnapshot};

const EMAIL: &str = "Alice@Example.com";
const PASSWORD: &str = "hunter2 hunter2";
// Few iterations to keep the tests fast; the KDF itself is tested in crypto.rs.
const KDF: Kdf = Kdf::Pbkdf2 { iterations: 5000 };

fn config(server: &FakeServer) -> Config {
    Config { server: server.url.clone(), email: EMAIL.into(), folder: "Notes".into() }
}

fn password() -> SignInRequest {
    SignInRequest { method: Method::Password { password: PASSWORD.into() }, two_factor: None, new_device_code: None }
}

fn signed_in(server: &FakeServer) -> String {
    match sign_in(&config(server), &password()).unwrap() {
        SignInStep::Done { secret } => secret,
        other => panic!("{other:?}"),
    }
}

fn backend(server: &FakeServer) -> BitwardenBackend {
    BitwardenBackend::open(&config(server), Some(signed_in(server))).unwrap()
}

fn note(id: &str, folder: &[&str], body: &str) -> NoteData {
    NoteData {
        id: id.into(),
        folder: folder.iter().map(|s| s.to_string()).collect(),
        body: body.into(),
        created: Utc::now(),
        modified: Utc::now(),
        pinned: false,
        deleted: None,
    }
}

#[test]
fn password_sign_in_unlocks_the_vault() {
    let server = FakeServer::start(EMAIL, PASSWORD, KDF);
    let secret = signed_in(&server);
    let session: Session = serde_json::from_str(&secret).unwrap();
    assert_eq!(session.keys().unwrap(), server.state().user_key);
    assert!(session.refresh_token.is_some());
    assert!(!secret.contains(PASSWORD), "the master password isn't kept");
}

#[test]
fn argon2_accounts_sign_in() {
    let kdf = Kdf::Argon2id { iterations: 2, memory: 16, parallelism: 1 };
    let server = FakeServer::start(EMAIL, PASSWORD, kdf);
    assert!(matches!(sign_in(&config(&server), &password()).unwrap(), SignInStep::Done { .. }));
}

#[test]
fn wrong_password() {
    let server = FakeServer::start(EMAIL, PASSWORD, KDF);
    let req = SignInRequest { method: Method::Password { password: "nope".into() }, ..password() };
    let err = sign_in(&config(&server), &req).unwrap_err();
    assert!(err.to_string().contains("incorrect"), "{err}");
}

#[test]
fn two_step_login() {
    let server = FakeServer::start(EMAIL, PASSWORD, KDF);
    server.state().two_factor = vec![0, 1, 3];
    let step = sign_in(&config(&server), &password()).unwrap();
    assert_eq!(
        step,
        SignInStep::NeedsTwoFactor { kinds: vec![TwoFactorKind::Authenticator, TwoFactorKind::Email, TwoFactorKind::Other(3)] }
    );
    let with = |code: &str, kind| SignInRequest {
        two_factor: Some(TwoFactorCode { kind, code: code.into(), remember: false }),
        ..password()
    };
    assert!(sign_in(&config(&server), &with("000000", TwoFactorKind::Authenticator)).is_err());
    assert!(matches!(sign_in(&config(&server), &with(TOTP_CODE, TwoFactorKind::Authenticator)).unwrap(), SignInStep::Done { .. }));
    // By email: ask for the code, then use it.
    send_email_code(&config(&server), &password()).unwrap();
    assert_eq!(server.state().emails_sent, 1);
    assert!(matches!(sign_in(&config(&server), &with(EMAIL_CODE, TwoFactorKind::Email)).unwrap(), SignInStep::Done { .. }));
}

#[test]
fn new_device_verification() {
    let server = FakeServer::start(EMAIL, PASSWORD, KDF);
    server.state().require_new_device = true;
    assert_eq!(sign_in(&config(&server), &password()).unwrap(), SignInStep::NeedsNewDeviceCode);
    let req = SignInRequest { new_device_code: Some(DEVICE_CODE.into()), ..password() };
    assert!(matches!(sign_in(&config(&server), &req).unwrap(), SignInStep::Done { .. }));
    // The device id is stable, so it's known from now on.
    assert!(matches!(sign_in(&config(&server), &password()).unwrap(), SignInStep::Done { .. }));
}

#[test]
fn api_key_sign_in_renews_with_the_key() {
    let server = FakeServer::start(EMAIL, PASSWORD, KDF);
    let req = SignInRequest {
        method: Method::ApiKey { client_id: API_CLIENT_ID.into(), client_secret: API_CLIENT_SECRET.into(), password: PASSWORD.into() },
        two_factor: None,
        new_device_code: None,
    };
    let SignInStep::Done { secret } = sign_in(&config(&server), &req).unwrap() else { panic!() };
    let mut b = BitwardenBackend::open(&config(&server), Some(secret)).unwrap();
    b.pull().unwrap();
    server.state().expire_tokens();
    b.pull().unwrap();
    assert!(b.renewed_secret().is_some());
}

#[test]
fn notes_round_trip_as_encrypted_secure_notes() {
    let server = FakeServer::start(EMAIL, PASSWORD, KDF);
    let mut b = backend(&server);
    let n = note("n1", &["Work", "2026"], "# Plan #todo\n- [ ] ship it\n");
    let (rid, rev) = b.save_note(None, None, &n).unwrap();

    {
        let s = server.state();
        assert_eq!(s.folder_names(), vec!["Notes/Work/2026"]);
        let c = &s.ciphers[&rid];
        assert_eq!(c["type"], 2);
        assert!(c["notes"].as_str().unwrap().starts_with("2."), "only ciphertext reaches the server");
        assert_eq!(s.decrypt_field(c, "name"), "Plan #todo");
        assert!(s.decrypt_field(c, "notes").starts_with("---\nid: n1\n"));
    }
    let snap = b.pull().unwrap();
    assert_eq!(snap.notes.len(), 1);
    let r = &snap.notes[0];
    assert_eq!((r.remote_id.as_str(), r.revision.as_str()), (rid.as_str(), rev.as_str()));
    assert_eq!(r.data.body, n.body);
    assert_eq!(r.data.folder, vec!["Work", "2026"]);
}

#[test]
fn outdated_updates_are_conflicts() {
    let server = FakeServer::start(EMAIL, PASSWORD, KDF);
    let mut b = backend(&server);
    let (rid, rev) = b.save_note(None, None, &note("n1", &["Notes"], "a")).unwrap();
    let (_, rev2) = b.save_note(Some(&rid), Some(&rev), &note("n1", &["Notes"], "b")).unwrap();
    let err = b.save_note(Some(&rid), Some(&rev), &note("n1", &["Notes"], "c")).unwrap_err();
    assert!(err.is::<Conflict>(), "{err:#}");
    b.save_note(Some(&rid), Some(&rev2), &note("n1", &["Notes"], "c")).unwrap();
    // Gone from the vault (deleted elsewhere): also a conflict, so the engine
    // re-pulls and decides.
    b.delete_note(&rid).unwrap();
    assert!(b.save_note(Some(&rid), None, &note("n1", &["Notes"], "d")).unwrap_err().is::<Conflict>());
}

#[test]
fn long_notes_fit_bitwardens_limits() {
    let server = FakeServer::start(EMAIL, PASSWORD, KDF);
    let mut b = backend(&server);
    let body = "A line of a long note, with ünïcode ✓ in it.\n".repeat(2500);
    assert!(body.len() > 100_000);
    b.save_note(None, None, &note("long", &["Notes"], &body)).unwrap();
    let snap = b.pull().unwrap();
    assert_eq!(snap.notes[0].data.body, body);
}

#[test]
fn notes_typed_into_the_vault_are_read() {
    let server = FakeServer::start(EMAIL, PASSWORD, KDF);
    let (root, other) = {
        let mut s = server.state();
        let root = s.add_folder("Notes");
        let other = s.add_folder("Passwords");
        s.add_note(Some(&root), "Wi-Fi", "password is hunter2", json!({}));
        s.add_note(Some(&other), "Not ours", "x", json!({}));
        s.add_note(None, "No folder", "x", json!({}));
        s.add_note(Some(&root), "Org note", "x", json!({"organizationId": "org1"}));
        s.add_note(Some(&root), "Login", "x", json!({"type": 1}));
        (root, other)
    };
    let snap = backend(&server).pull().unwrap();
    assert_eq!(snap.notes.len(), 1);
    assert_eq!(snap.notes[0].data.body, "# Wi-Fi\npassword is hunter2");
    assert_eq!(snap.notes[0].data.folder, vec!["Notes"]);
    let ids: Vec<&str> = snap.folders.iter().map(|f| f.remote_id.as_str()).collect();
    assert_eq!(ids, vec![root.as_str()]);
    assert!(!ids.contains(&other.as_str()));
}

#[test]
fn updates_keep_what_the_app_does_not_own() {
    let server = FakeServer::start(EMAIL, PASSWORD, KDF);
    let id = {
        let mut s = server.state();
        let root = s.add_folder("Notes");
        // A cipher with its own key, a custom field and a favorite star.
        let ck = crypto::Keys::random();
        let k = crypto::encrypt(&s.user_key, &ck.to_bytes());
        let field = json!({"type": 0, "name": crypto::encrypt_str(&ck, "pin"), "value": crypto::encrypt_str(&ck, "4321"), "linkedId": null});
        let id = s.add_note(Some(&root), "x", "x", json!({"key": k, "favorite": true, "fields": [field]}));
        let c = s.ciphers.get_mut(&id).unwrap();
        c["name"] = json!(crypto::encrypt_str(&ck, "Card PIN"));
        c["notes"] = json!(crypto::encrypt_str(&ck, "the old text"));
        id
    };
    let mut b = backend(&server);
    let snap = b.pull().unwrap();
    let r = snap.notes.iter().find(|n| n.remote_id == id).unwrap();
    assert_eq!(r.data.body, "# Card PIN\nthe old text");
    let mut data = r.data.clone();
    data.body = "# Card PIN\nnew text".into();
    b.save_note(Some(&id), Some(&r.revision), &data).unwrap();
    let s = server.state();
    let c = &s.ciphers[&id];
    assert_eq!(c["favorite"], true);
    assert!(c["key"].is_string());
    assert_eq!(c["fields"].as_array().unwrap().len(), 1, "the custom field stays");
    drop(s);
    assert_eq!(b.pull().unwrap().notes[0].data.body, "# Card PIN\nnew text");
}

#[test]
fn folders_rename_with_their_subfolders_and_delete() {
    let server = FakeServer::start(EMAIL, PASSWORD, KDF);
    let mut b = backend(&server);
    let p = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
    let notes = b.create_folder(&p(&["Notes"])).unwrap();
    let work = b.create_folder(&p(&["Work"])).unwrap();
    assert_eq!(b.create_folder(&p(&["Work"])).unwrap(), work);
    b.create_folder(&p(&["Work", "Old"])).unwrap();
    b.rename_folder(&work, &p(&["Work"]), &p(&["Projects"])).unwrap();
    assert_eq!(server.state().folder_names(), vec!["Notes", "Notes/Projects", "Notes/Projects/Old"]);
    let mut paths: Vec<_> = b.pull().unwrap().folders.into_iter().map(|f| f.path).collect();
    paths.sort();
    assert_eq!(paths, vec![p(&["Notes"]), p(&["Projects"]), p(&["Projects", "Old"])]);
    b.delete_folder(&work, &p(&["Projects"])).unwrap();
    b.delete_folder(&work, &p(&["Projects"])).unwrap();
    assert_eq!(server.state().folder_names(), vec!["Notes", "Notes/Projects/Old"]);
    let _ = notes;
}

#[test]
fn deleting_moves_to_bitwardens_trash() {
    let server = FakeServer::start(EMAIL, PASSWORD, KDF);
    let mut b = backend(&server);
    let (rid, _) = b.save_note(None, None, &note("n1", &["Notes"], "bye")).unwrap();
    b.delete_note(&rid).unwrap();
    b.delete_note(&rid).unwrap();
    assert!(server.state().ciphers[&rid]["deletedDate"].is_string());
    assert!(b.pull().unwrap().notes.is_empty());
}

#[test]
fn expired_tokens_are_refreshed_and_the_new_session_is_handed_back() {
    let server = FakeServer::start(EMAIL, PASSWORD, KDF);
    let mut b = backend(&server);
    b.pull().unwrap();
    assert_eq!(b.renewed_secret(), None);
    server.state().expire_tokens();
    b.pull().unwrap();
    let renewed = b.renewed_secret().expect("a renewed session");
    // The renewed session works on its own.
    let mut b2 = BitwardenBackend::open(&config(&server), Some(renewed)).unwrap();
    b2.pull().unwrap();
}

#[test]
fn a_revoked_session_needs_signing_in_again() {
    let server = FakeServer::start(EMAIL, PASSWORD, KDF);
    let mut session: Session = serde_json::from_str(&signed_in(&server)).unwrap();
    session.refresh_token = Some("revoked".into());
    session.expires_at = 0;
    let mut b = BitwardenBackend::open(&config(&server), Some(serde_json::to_string(&session).unwrap())).unwrap();
    let err = b.pull().unwrap_err();
    assert!(err.is::<NeedsSignIn>(), "{err:#}");
    assert!(BitwardenBackend::open(&config(&server), None).err().unwrap().is::<NeedsSignIn>());
}

// ---- The engine, two devices, one vault ------------------------------------

struct Device {
    store: Store,
    backend: BitwardenBackend,
}

impl Device {
    fn new(server: &FakeServer) -> Device {
        let mut store = Store::new_install();
        store.accounts.push(Account {
            id: "bw".into(),
            name: "Bitwarden".into(),
            backend: crate::sync::BackendConfig::Bitwarden(config(server)),
            enabled: true,
            cursor: None,
            last_sync: None,
        });
        store.ensure_local();
        Device { store, backend: backend(server) }
    }

    fn sync(&mut self) -> Vec<String> {
        let snap = AccountSnapshot::of(&self.store, "bw");
        let out = sync_account(&mut self.backend, &snap, Utc::now()).unwrap();
        assert_eq!(out.error, None);
        apply(&mut self.store, &snap, out)
    }

    fn add(&mut self, folder: &[&str], body: &str) -> String {
        let path: Vec<String> = folder.iter().map(|s| s.to_string()).collect();
        let f = self.store.ensure_folder_at("bw", &path);
        let n = Note::new("bw", &f, body);
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
fn two_devices_sync_through_the_vault() {
    let server = FakeServer::start(EMAIL, PASSWORD, KDF);
    let mut laptop = Device::new(&server);
    let mut phone = Device::new(&server);
    let id = laptop.add(&["Notes"], "# List\neggs\nmilk\n");
    let work = laptop.add(&["Work"], "# Standup\n");
    laptop.sync();
    phone.sync();
    assert_eq!(phone.store.note(&id).unwrap().body, "# List\neggs\nmilk\n");
    let wf = phone.store.note(&work).unwrap().folder.clone();
    assert_eq!(phone.store.folder_path(&wf), vec!["Work"]);
    assert_eq!(server.state().folder_names(), vec!["Notes", "Notes/Work"]);

    // Both edit: merged.
    laptop.edit(&id, "# List\neggs (12)\nmilk\n");
    phone.edit(&id, "# List\neggs\nmilk\nbread\n");
    laptop.sync();
    assert!(phone.sync().is_empty());
    laptop.sync();
    assert_eq!(laptop.store.note(&id).unwrap().body, "# List\neggs (12)\nmilk\nbread\n");
    assert_eq!(phone.store.note(&id).unwrap().body, laptop.store.note(&id).unwrap().body);

    // Nothing changed: only the revision date is asked for.
    server.state().log.clear();
    laptop.sync();
    assert_eq!(server.state().log, vec!["GET /api/accounts/revision-date"]);
}

#[test]
fn a_web_page_instead_of_the_vault_is_an_error_not_an_empty_vault() {
    let server = FakeServer::start(EMAIL, PASSWORD, KDF);
    let mut laptop = Device::new(&server);
    let id = laptop.add(&["Notes"], "# Keep me\n");
    laptop.sync();
    server.state().sync_page = Some("<html><body>Sign in to the hotel Wi-Fi</body></html>".into());
    // Something changed elsewhere, so the sync pulls.
    server.state().add_folder("Notes/Elsewhere");
    let snap = AccountSnapshot::of(&laptop.store, "bw");
    let err = match sync_account(&mut laptop.backend, &snap, Utc::now()) {
        Err(e) => format!("{e:#}"),
        Ok(out) => out.error.expect("the sync must fail"),
    };
    assert!(err.contains("JSON"), "{err}");
    assert!(laptop.store.note(&id).is_some(), "the note survives");
    // A JSON reply without the lists isn't an empty vault either.
    server.state().sync_page = Some("{}".into());
    assert!(laptop.backend.pull().is_err());
}

#[test]
fn items_that_cant_be_decrypted_are_kept_locally() {
    let server = FakeServer::start(EMAIL, PASSWORD, KDF);
    let mut laptop = Device::new(&server);
    let a = laptop.add(&["Notes"], "# A\n");
    let b = laptop.add(&["Work"], "# B\n");
    laptop.sync();
    {
        // Another client re-encrypts one note and the Work folder with
        // something we can't read.
        let rid = laptop.store.note(&a).unwrap().sync.remote_id.clone().unwrap();
        let mut s = server.state();
        s.scramble_note(&rid);
        s.scramble_folder("Notes/Work");
    }
    let snap = laptop.backend.pull().unwrap();
    assert_eq!(snap.unreadable.len(), 3, "the note, the folder and the note in it: {:?}", snap.unreadable);
    laptop.sync();
    assert_eq!(laptop.store.note(&a).unwrap().body, "# A\n");
    assert_eq!(laptop.store.note(&b).unwrap().body, "# B\n");
    let bf = laptop.store.note(&b).unwrap().folder.clone();
    assert_eq!(laptop.store.folder_path(&bf), vec!["Work"]);
}
