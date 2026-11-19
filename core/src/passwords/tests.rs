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

/// An item object's JSON as stored (for checking what was written).
fn stored_json(server: &FakeS3, found: &Found, key: &[u8; 32], id: &str) -> serde_json::Value {
    let name = format!("{}/{ITEMS}{id}.bin", found.config.prefix);
    let data = server.state().objects[&name].body.clone();
    let aad = format!("cleanpassword/item/v1/{}/{id}", found.header.vault_id);
    let plain = open(key, aad.as_bytes(), data.strip_prefix(MAGIC).unwrap()).unwrap();
    serde_json::from_slice(&plain).unwrap()
}

#[test]
fn deleting_recovering_and_deleting_for_good_write_what_cleanpassword_writes() {
    let server = server_with_vault("CleanPassword");
    let (found, key, c) = unlocked(&server);
    let gh = by_name(&c, "GitHub").id.clone();
    let old = by_name(&c, "Old Forum").id.clone();
    let bank = by_name(&c, "bank.example").id.clone();
    let before = stored_json(&server, &found, &key, &gh);

    assert_eq!(apply(&found, &secret(), &key, &[gh.clone()], Change::Delete).unwrap(), 0);
    let after = stored_json(&server, &found, &key, &gh);
    let item = after["item"].as_object().unwrap();
    assert!(item["deleted"].is_string());
    assert_ne!(item["modified"], before["item"]["modified"]);
    // Everything else as it was.
    for (k, v) in before["item"].as_object().unwrap() {
        if k != "modified" {
            assert_eq!(&item[k], v, "{k}");
        }
    }
    // Padded as CleanPassword pads.
    let len = server.state().objects[&format!("CleanPassword/{ITEMS}{gh}.bin")].body.len();
    assert_eq!((len - MAGIC.len() - NONCE_LEN - TAG_LEN) % PAD, 0);
    // Deleting again changes nothing.
    let etag = server.state().objects[&format!("CleanPassword/{ITEMS}{gh}.bin")].etag.clone();
    assert_eq!(apply(&found, &secret(), &key, &[gh.clone()], Change::Delete).unwrap(), 0);
    assert_eq!(server.state().objects[&format!("CleanPassword/{ITEMS}{gh}.bin")].etag, etag);

    assert_eq!(apply(&found, &secret(), &key, &[old.clone()], Change::Recover).unwrap(), 0);
    assert!(stored_json(&server, &found, &key, &old)["item"].get("deleted").is_none());

    assert_eq!(apply(&found, &secret(), &key, &[bank.clone()], Change::DeleteForever).unwrap(), 0);
    let t = stored_json(&server, &found, &key, &bank);
    assert_eq!(t["tombstone"]["id"], bank.as_str());

    let c = fetch(&found, &secret(), &key, &Contents::default()).unwrap();
    assert!(c.get(&gh).unwrap().deleted.is_some());
    assert!(c.get(&old).unwrap().deleted.is_none());
    assert!(c.get(&bank).is_none());
    assert_eq!(c.unreadable, 0);
}

#[test]
fn fields_this_version_doesnt_read_are_kept() {
    let server = server_with_vault("CleanPassword");
    let (found, key, c) = unlocked(&server);
    let gh = by_name(&c, "GitHub").id.clone();
    // A passkey with its private key, and a field from a newer version.
    let mut v = stored_json(&server, &found, &key, &gh);
    v["item"]["passkeys"] = serde_json::json!([{ "credential_id": "Y3JlZA", "rp_id": "github.com", "user_handle": "dQ", "key": "PRIVATE", "created": "2026-01-01T00:00:00Z" }]);
    v["item"]["future_field"] = serde_json::json!({ "x": 1 });
    let name = format!("CleanPassword/{ITEMS}{gh}.bin");
    server.put_bytes(&name, &seal_item(&key, &found.header.vault_id, &gh, &serde_json::to_vec(&v).unwrap()));

    apply(&found, &secret(), &key, &[gh.clone()], Change::Delete).unwrap();
    let after = stored_json(&server, &found, &key, &gh);
    assert_eq!(after["item"]["passkeys"][0]["key"], "PRIVATE");
    assert_eq!(after["item"]["future_field"]["x"], 1);
    assert!(after["item"]["deleted"].is_string());
}

#[test]
fn an_edit_elsewhere_beats_deleting_for_good_but_not_deleting() {
    let server = server_with_vault("CleanPassword");
    let (found, key, c) = unlocked(&server);
    let gh = by_name(&c, "GitHub").id.clone();
    // A purge refused by If-Match (the item changed after we read it) is
    // not retried: the edit wins.
    server.state().reject_if_match_once = true;
    assert_eq!(apply(&found, &secret(), &key, &[gh.clone()], Change::DeleteForever).unwrap(), 1);
    assert!(stored_json(&server, &found, &key, &gh).get("item").is_some());
    // A delete refused once is tried again on the new version.
    let wifi = by_name(&c, "Wi-Fi").id.clone();
    server.state().reject_if_match_once = true;
    assert_eq!(apply(&found, &secret(), &key, &[wifi.clone()], Change::Delete).unwrap(), 0);
    assert!(stored_json(&server, &found, &key, &wifi)["item"]["deleted"].is_string());
}

#[test]
fn the_service_shows_changes_at_once_and_reads_back_after() {
    let server = server_with_vault("CleanPassword");
    let pw = Passwords::new(|| {});
    pw.search(vec![("acct".into(), notes_config(&server), secret())]);
    wait(&pw, |s| !s.searching);
    pw.unlock(PASSWORD.into());
    wait(&pw, |s| s.busy.is_none());
    let ids: Vec<String> = pw.state().items().iter().map(|i| i.id.clone()).collect();
    assert_eq!(ids.len(), 3);

    pw.change(ids[..2].to_vec(), Change::Delete);
    assert_eq!(pw.state().items().len(), 1, "shown at once");
    assert!(pw.state().saving());
    wait(&pw, |s| !s.saving());
    assert_eq!(pw.state().items().len(), 1);
    assert_eq!(pw.state().deleted().len(), 3);
    assert!(pw.state().error.is_none());

    pw.change(vec![ids[0].clone()], Change::Recover);
    wait(&pw, |s| !s.saving());
    assert_eq!(pw.state().items().len(), 2);

    let trash: Vec<String> = pw.state().deleted().iter().map(|i| i.id.clone()).collect();
    pw.change(trash, Change::DeleteForever);
    wait(&pw, |s| !s.saving());
    assert!(pw.state().deleted().is_empty());
    assert_eq!(pw.state().items().len(), 2);

    // A key that may only read: said so, and the vault shows as it is.
    server.state().refuse_puts = true;
    let id = pw.state().items()[0].id.clone();
    pw.change(vec![id], Change::Delete);
    wait(&pw, |s| !s.saving());
    assert!(pw.state().error.as_deref().unwrap().starts_with("Couldn't change the vault"));
    assert_eq!(pw.state().items().len(), 2);
}

/// With CleanPassword's own core: `CLEANNOTES_CP_DIR` holds a vault's
/// objects (`objects.json`, made by CleanPassword); this deletes GitHub,
/// recovers Old Forum and deletes bank.example for good, and writes the
/// objects after (`after.json`) for CleanPassword to sync with.
#[test]
#[ignore]
fn changes_for_cleanpassword_to_read() {
    let dir = std::env::var("CLEANNOTES_CP_DIR").expect("CLEANNOTES_CP_DIR");
    let m: BTreeMap<String, String> = serde_json::from_str(&std::fs::read_to_string(format!("{dir}/objects.json")).unwrap()).unwrap();
    let server = FakeS3::start();
    for (k, v) in m {
        server.put_bytes(&k, &B64.decode(v).unwrap());
    }
    let (found, key, c) = unlocked(&server);
    for (name, change) in [("GitHub", Change::Delete), ("Old Forum", Change::Recover), ("bank.example", Change::DeleteForever)] {
        assert_eq!(apply(&found, &secret(), &key, &[by_name(&c, name).id.clone()], change).unwrap(), 0);
    }
    let out: BTreeMap<String, String> = server.state().objects.iter().map(|(k, o)| (k.clone(), B64.encode(&o.body))).collect();
    std::fs::write(format!("{dir}/after.json"), serde_json::to_string(&out).unwrap()).unwrap();
}
