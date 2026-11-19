//! Reading a vault CleanPassword made (`testdata/cleanpassword-vault.json`:
//! every object of a vault made and synced by CleanPassword's own core,
//! with the master password below) from the fake S3.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use super::*;
use crate::sync::s3::fake::{FakeS3, ACCESS_KEY, BUCKET, REGION, SECRET_KEY};

const PASSWORD: &str = "correct horse battery";
const FIXTURE: &str = include_str!("testdata/cleanpassword-vault.json");

/// The fixture's objects, moved from its `CleanPassword/` folder to `folder`.
fn objects(folder: &str) -> Vec<(String, Vec<u8>)> {
    let m: BTreeMap<String, String> = serde_json::from_str(FIXTURE).unwrap();
    m.into_iter()
        .map(|(k, v)| {
            let rel = k.strip_prefix("CleanPassword/").unwrap();
            let key = if folder.is_empty() { rel.to_owned() } else { format!("{folder}/{rel}") };
            (key, B64.decode(v).unwrap())
        })
        .collect()
}

fn server_with_vault(folder: &str) -> FakeS3 {
    let server = FakeS3::start();
    for (k, v) in objects(folder) {
        server.put_bytes(&k, &v);
    }
    server
}

/// A CleanNotes S3 account's settings, notes in `CleanNotes`.
fn notes_config(server: &FakeS3) -> s3::Config {
    s3::Config { bucket: BUCKET.into(), region: REGION.into(), prefix: "CleanNotes".into(), endpoint: Some(server.url.clone()), encrypted: true }
}

fn secret() -> String {
    // The notes' own encryption key is no use for the vault, and isn't used.
    Keys { access_key_id: ACCESS_KEY.into(), secret_access_key: SECRET_KEY.into(), encryption_key: Some(B64.encode([7u8; 32])) }.to_secret()
}

fn unlocked(server: &FakeS3) -> (Found, Key, Contents) {
    let found = find("acct", &notes_config(server), &secret()).unwrap().expect("found");
    let key = found.header.unwrap_key(PASSWORD).unwrap();
    let c = fetch(&found, &secret(), &key, &Contents::default()).unwrap();
    (found, key, c)
}

fn by_name<'a>(c: &'a Contents, name: &str) -> &'a Item {
    c.all().find(|i| i.name == name).unwrap_or_else(|| panic!("no {name}"))
}

#[test]
fn reads_a_vault_cleanpassword_made_in_its_own_folder() {
    let server = server_with_vault("CleanPassword");
    let (found, _, c) = unlocked(&server);
    assert_eq!(found.account, "acct");
    assert_eq!(found.config.prefix, "CleanPassword");
    assert!(!found.config.encrypted);
    assert_eq!(found.place(), format!("{BUCKET}/CleanPassword"));

    // Four items (one in Recently Deleted); the purged one's tombstone is
    // no item.
    let mut names: Vec<&str> = c.all().map(|i| i.name.as_str()).collect();
    names.sort();
    assert_eq!(names, ["GitHub", "Old Forum", "Wi-Fi", "bank.example"]);
    assert_eq!(c.unreadable, 0);

    let gh = by_name(&c, "GitHub");
    assert_eq!(gh.kind, ItemKind::Login);
    assert_eq!((gh.username.as_str(), gh.password.as_str()), ("octocat", "new-password-42"));
    assert_eq!(gh.uris[0].uri, "https://github.com/login");
    assert_eq!(gh.totp, "JBSWY3DPEHPK3PXP");
    assert_eq!(gh.folder, "Work");
    assert!(gh.favorite);
    assert_eq!(gh.fields, vec![Field { name: "PIN".into(), value: "4321".into(), hidden: true }]);
    assert_eq!(gh.history.len(), 1);
    assert_eq!(gh.history[0].password, "hunter2-hunter2");
    assert!(gh.created.is_some() && gh.modified.is_some() && gh.password_modified.is_some());
    assert!(totp::code_now(&gh.totp).is_some());

    let wifi = by_name(&c, "Wi-Fi");
    assert_eq!(wifi.kind, ItemKind::Note);
    assert!(wifi.notes.contains("swordfish"));
    assert!(by_name(&c, "Old Forum").deleted.is_some());
    assert_eq!(by_name(&c, "bank.example").folder, "Money/Banks");
}

#[test]
fn reads_a_vault_sharing_the_notes_folder() {
    let server = server_with_vault("CleanNotes");
    // CleanNotes' own objects alongside.
    server.put("CleanNotes/head", "x");
    server.put("CleanNotes/notes/n1.md", "---\nid: n1\n---\nhello\n");
    let (found, _, c) = unlocked(&server);
    assert_eq!(found.config.prefix, "CleanNotes");
    assert_eq!(c.all().count(), 4);
}

#[test]
fn the_notes_folder_comes_first() {
    let server = server_with_vault("CleanNotes");
    for (k, v) in objects("CleanPassword") {
        server.put_bytes(&k, &v);
    }
    let found = find("acct", &notes_config(&server), &secret()).unwrap().unwrap();
    assert_eq!(found.config.prefix, "CleanNotes");
}

#[test]
fn a_wrong_master_password_is_said_so() {
    let server = server_with_vault("CleanPassword");
    let found = find("acct", &notes_config(&server), &secret()).unwrap().unwrap();
    let e = found.header.unwrap_key("correct horse battery!").unwrap_err();
    assert!(e.is::<WrongPassword>());
    assert_eq!(message(&e), "That isn't the master password.");
}

#[test]
fn no_vault_is_none_and_unreachable_is_an_error() {
    let server = FakeS3::start();
    server.put("CleanNotes/head", "x");
    assert!(find("acct", &notes_config(&server), &secret()).unwrap().is_none());
    // A key whose policy only allows the notes' folder.
    server.state().forbidden.insert(format!("CleanPassword/{VAULT_OBJECT}"));
    assert!(find("acct", &notes_config(&server), &secret()).unwrap().is_none());
    // Something else's vault.json.
    server.put("CleanNotes/vault.json", "{\"hello\": 1}");
    assert!(find("acct", &notes_config(&server), &secret()).unwrap().is_none());

    let offline = s3::Config { endpoint: Some("http://127.0.0.1:9".into()), ..notes_config(&server) };
    assert!(find("acct", &offline, &secret()).is_err());
}

#[test]
fn reading_again_downloads_only_what_changed() {
    let server = server_with_vault("CleanPassword");
    let (found, key, c) = unlocked(&server);
    server.state().log.clear();
    let again = fetch(&found, &secret(), &key, &c).unwrap();
    assert_eq!(again.all().count(), 4);
    let gets: Vec<String> = server.state().log.iter().filter(|l| l.starts_with("GET ") && l.len() > 4).cloned().collect();
    assert_eq!(gets, [format!("GET CleanPassword/{VAULT_OBJECT}")], "only the header and the listing");

    // An item rewritten (here: another item's object put in its place,
    // which its id doesn't match) is downloaded, and can't be opened.
    let object = |name: &str| format!("CleanPassword/items/{}.bin", by_name(&again, name).id);
    let other = server.state().objects[&object("Wi-Fi")].body.clone();
    server.put_bytes(&object("GitHub"), &other);
    let again = fetch(&found, &secret(), &key, &again).unwrap();
    assert_eq!(again.unreadable, 1);
    assert_eq!(again.all().count(), 3);
}

#[test]
fn another_vault_in_the_folder_is_refused() {
    let server = server_with_vault("CleanPassword");
    let (mut found, key, _) = unlocked(&server);
    found.header.vault_id = "someone-else".into();
    assert!(fetch(&found, &secret(), &key, &Contents::default()).is_err());
}

#[test]
fn hostile_key_settings_are_refused() {
    let server = server_with_vault("CleanPassword");
    let found = find("acct", &notes_config(&server), &secret()).unwrap().unwrap();
    let mut h = found.header.clone();
    h.kdf.memory_kib = 4 * 1024 * 1024;
    assert!(h.unwrap_key(PASSWORD).is_err());
    let mut h = found.header.clone();
    h.kdf.alg = "pbkdf2".into();
    assert!(h.unwrap_key(PASSWORD).is_err());
    assert!(VaultHeader::parse(br#"{"format":"cleanpassword-vault","version":2}"#).is_err());
}

#[test]
fn titles_search_and_folders() {
    let item = Item { uris: vec![ItemUri { uri: "https://www.example.com/x".into() }], folder: "Money/Banks".into(), ..Default::default() };
    assert_eq!(item.title(), "example.com");
    assert_eq!(Item { uris: vec![ItemUri { uri: "example.org".into() }], ..Default::default() }.title(), "example.org");
    assert_eq!(Item { username: "me".into(), ..Default::default() }.title(), "me");
    assert!(item.matches("EXAMPLE banks"));
    assert!(!item.matches("example work"));
    assert!(item.in_folder("Money") && item.in_folder("Money/Banks") && !item.in_folder("Mon"));
}

fn wait(pw: &Passwords, what: impl Fn(&State) -> bool) {
    let start = Instant::now();
    while !what(&pw.state()) {
        assert!(start.elapsed() < Duration::from_secs(60), "timed out");
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn the_service_finds_unlocks_refreshes_and_locks() {
    let server = server_with_vault("CleanPassword");
    let pw = Passwords::new(|| {});
    pw.search(vec![("acct".into(), notes_config(&server), secret())]);
    wait(&pw, |s| !s.searching);
    assert!(pw.state().found.is_some());
    assert!(!pw.state().unlocked());

    pw.unlock("wrong password".into());
    wait(&pw, |s| s.busy.is_none());
    assert_eq!(pw.state().error.as_deref(), Some("That isn't the master password."));

    pw.unlock(PASSWORD.into());
    wait(&pw, |s| s.busy.is_none());
    {
        let s = pw.state();
        assert!(s.unlocked() && s.error.is_none());
        let names: Vec<String> = s.items().iter().map(|i| i.title()).collect();
        assert_eq!(names, ["bank.example", "GitHub", "Wi-Fi"], "by name, Recently Deleted apart");
        assert_eq!(s.deleted().len(), 1);
        assert_eq!(s.folders(), ["Money", "Money/Banks", "Work"]);
    }

    // Deleted elsewhere: gone after a refresh.
    let id = pw.state().items()[0].id.clone();
    server.state().objects.remove(&format!("CleanPassword/items/{id}.bin"));
    pw.refresh();
    wait(&pw, |s| s.busy.is_none());
    assert_eq!(pw.state().contents().unwrap().all().count(), 3);

    pw.lock();
    assert!(!pw.state().unlocked());
    assert!(pw.state().items().is_empty());

    // The vault removed from the bucket: forgotten at the next search.
    server.state().objects.clear();
    pw.search(vec![("acct".into(), notes_config(&server), secret())]);
    wait(&pw, |s| !s.searching);
    assert!(pw.state().found.is_none());
}

#[test]
fn a_lock_during_unlocking_wins() {
    let server = server_with_vault("CleanPassword");
    let pw = Passwords::new(|| {});
    pw.search(vec![("acct".into(), notes_config(&server), secret())]);
    wait(&pw, |s| !s.searching);
    pw.unlock(PASSWORD.into());
    pw.lock();
    std::thread::sleep(Duration::from_secs(3));
    assert!(!pw.state().unlocked());
}
