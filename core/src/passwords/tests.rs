//! CleanNotes as one of a CleanPassword vault's devices, against the fake
//! S3: vaults made and changed by CleanPassword's own core (as its apps
//! would, on the same bucket), found, unlocked, synced and changed here.
//!
//! CleanPassword's master keys cost what they do in its apps (Argon2id,
//! 46 MiB, 3 passes: its test-sized costs are only for its own tests, and
//! a vault asking for less is refused), so each create, join or unlock
//! takes a moment.

use std::time::{Duration, Instant};

use base64::Engine;

use super::*;
use crate::sync::s3::fake::{FakeS3, ACCESS_KEY, BUCKET, REGION, SECRET_KEY};

const PASSWORD: &str = "correct horse battery";

/// A CleanNotes S3 account's settings, notes in `CleanNotes`.
fn notes_config(server: &FakeS3) -> s3::Config {
    s3::Config { bucket: BUCKET.into(), region: REGION.into(), prefix: "CleanNotes".into(), endpoint: Some(server.url.clone()), encrypted: true }
}

fn secret() -> String {
    // The notes' own encryption key is no use for the vault, and isn't used.
    let key = base64::engine::general_purpose::STANDARD.encode([7u8; 32]);
    Keys { access_key_id: ACCESS_KEY.into(), secret_access_key: SECRET_KEY.into(), encryption_key: Some(key) }.to_secret()
}

fn cp_config(server: &FakeS3, folder: &str) -> S3Config {
    S3Config { bucket: BUCKET.into(), region: REGION.into(), prefix: folder.into(), endpoint: Some(server.url.clone()) }
}

fn cp_creds() -> AwsCredentials {
    AwsCredentials { access_key_id: ACCESS_KEY.into(), secret_access_key: SECRET_KEY.into(), session_token: None }
}

fn sync(v: &mut Vault) -> SyncReport {
    block_on(v.sync(&Blocking::new())).unwrap()
}

fn login(name: &str, url: &str, user: &str, pw: &str) -> ItemData {
    ItemData { name: name.into(), username: user.into(), password: pw.into(), uris: vec![ItemUri::new(url)], ..Default::default() }
}

/// A CleanPassword device that makes a vault in `folder` with four items
/// (one in Recently Deleted) and uploads it.
fn cleanpassword_device(server: &FakeS3, folder: &str) -> Vault {
    let mut v = Vault::create(cp_config(server, folder), cp_creds(), PASSWORD).unwrap();
    let gh = v
        .add(ItemData {
            totp: "JBSWY3DPEHPK3PXP".into(),
            folder: "Work".into(),
            favorite: true,
            fields: vec![Field { name: "PIN".into(), value: "4321".into(), hidden: true }],
            ..login("GitHub", "https://github.com/login", "octocat", "hunter2-hunter2")
        })
        .unwrap();
    let data = ItemData { password: "new-password-42".into(), ..v.get(&gh).unwrap().data() };
    v.update(&gh, data).unwrap();
    v.add(ItemData { kind: ItemKind::Note, name: "Wi-Fi".into(), notes: "The password is swordfish.".into(), ..Default::default() }).unwrap();
    v.add(ItemData { folder: "Money/Banks".into(), ..login("", "https://www.bank.example/", "me", "pw") }).unwrap();
    let old = v.add(login("Old Forum", "https://forum.example", "me", "pw")).unwrap();
    v.delete(&old).unwrap();
    sync(&mut v);
    v
}

/// Another CleanPassword device joining the vault in `folder`.
fn join(server: &FakeS3, folder: &str) -> Vault {
    let (h, etag) = block_on(fetch_header(&Blocking::new(), &cp_config(server, folder), &cp_creds())).unwrap().expect("a vault");
    let mut v = Vault::join(cp_config(server, folder), cp_creds(), h, etag, PASSWORD).unwrap();
    sync(&mut v);
    v
}

fn by_name<'a>(v: &'a Vault, name: &str) -> &'a Item {
    v.items().into_iter().chain(v.trash()).find(|i| i.name == name).unwrap_or_else(|| panic!("no {name}"))
}

fn wait(pw: &Passwords, what: impl Fn(&State) -> bool) {
    let start = Instant::now();
    while !what(&pw.state()) {
        assert!(start.elapsed() < Duration::from_secs(60), "timed out");
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// The service, having found the vault with the notes account's key.
fn service(server: &FakeS3) -> Passwords {
    let pw = Passwords::new(|| {});
    pw.search(vec![("acct".into(), notes_config(server), secret())]);
    wait(&pw, |s| !s.searching);
    assert!(pw.state().found.is_some(), "found");
    pw
}

fn unlocked(server: &FakeS3) -> Passwords {
    let pw = service(server);
    pw.unlock(PASSWORD.into());
    wait(&pw, |s| s.busy.is_none());
    assert!(pw.state().unlocked(), "{:?}", pw.state().error);
    pw
}

fn titles(pw: &Passwords) -> Vec<String> {
    pw.state().items().iter().map(|i| i.title()).collect()
}

fn requests(server: &FakeS3) -> Vec<String> {
    std::mem::take(&mut server.state().log)
}

#[test]
fn finds_and_unlocks_a_vault_in_cleanpasswords_own_folder() {
    let server = FakeS3::start();
    let device = cleanpassword_device(&server, "CleanPassword");
    let found = find("acct", &notes_config(&server), &secret()).unwrap().expect("found");
    assert_eq!(found.account, "acct");
    assert_eq!(found.config.prefix, "CleanPassword");
    assert!(!found.config.encrypted);
    assert_eq!(found.place(), format!("{BUCKET}/CleanPassword"));
    assert_eq!(found.header.vault_id, device.vault_id());

    let pw = unlocked(&server);
    let s = pw.state();
    assert!(s.error.is_none());
    assert_eq!(s.items().iter().map(|i| i.title()).collect::<Vec<_>>(), ["bank.example", "GitHub", "Wi-Fi"], "by name, Recently Deleted apart");
    assert_eq!(s.deleted().iter().map(|i| i.title()).collect::<Vec<_>>(), ["Old Forum"]);
    assert_eq!(s.folders(), ["Money", "Money/Banks", "Work"]);

    let gh = s.items().into_iter().find(|i| i.name == "GitHub").unwrap().clone();
    assert_eq!(gh.kind, ItemKind::Login);
    assert_eq!((gh.username.as_str(), gh.password.as_str()), ("octocat", "new-password-42"));
    assert_eq!(gh.uris[0].uri, "https://github.com/login");
    assert_eq!(gh.folder, "Work");
    assert!(gh.favorite);
    assert_eq!(gh.fields, vec![Field { name: "PIN".into(), value: "4321".into(), hidden: true }]);
    assert_eq!(gh.history.iter().map(|h| h.password.as_str()).collect::<Vec<_>>(), ["hunter2-hunter2"]);
    assert!(totp::code_now(&gh.totp).is_some());
    let wifi = s.items().into_iter().find(|i| i.name == "Wi-Fi").unwrap();
    assert_eq!(wifi.kind, ItemKind::Note);
    assert!(wifi.notes.contains("swordfish"));
}

#[test]
fn finds_a_vault_sharing_the_notes_folder_first() {
    let server = FakeS3::start();
    // CleanNotes' own objects alongside.
    server.put("CleanNotes/head", "x");
    server.put("CleanNotes/notes/n1.md", "---\nid: n1\n---\nhello\n");
    let shared = cleanpassword_device(&server, "CleanNotes");
    let own = cleanpassword_device(&server, "CleanPassword");
    let found = find("acct", &notes_config(&server), &secret()).unwrap().unwrap();
    assert_eq!(found.config.prefix, "CleanNotes");
    assert_eq!(found.header.vault_id, shared.vault_id());
    assert_ne!(found.header.vault_id, own.vault_id());
    let pw = unlocked(&server);
    assert_eq!(pw.state().items().len(), 3);
}

#[test]
fn no_vault_is_none_and_unreachable_is_an_error() {
    let server = FakeS3::start();
    server.put("CleanNotes/head", "x");
    assert!(find("acct", &notes_config(&server), &secret()).unwrap().is_none());
    // A key whose policy only allows the notes' folder.
    server.state().forbidden.insert("CleanPassword/vault.json".into());
    assert!(find("acct", &notes_config(&server), &secret()).unwrap().is_none());
    // Something else's vault.json, and a vault of CleanPassword's first
    // format (an object per item), which it no longer opens either.
    server.put("CleanNotes/vault.json", "{\"hello\": 1}");
    assert!(find("acct", &notes_config(&server), &secret()).unwrap().is_none());
    let saved: serde_json::Value = serde_json::from_str(&cleanpassword_device(&FakeS3::start(), "x").save()).unwrap();
    let mut v1 = saved["header"].clone();
    v1["version"] = 1.into();
    server.put("CleanNotes/vault.json", &v1.to_string());
    assert!(find("acct", &notes_config(&server), &secret()).unwrap().is_none());

    let offline = s3::Config { endpoint: Some("http://127.0.0.1:9".into()), ..notes_config(&server) };
    assert!(find("acct", &offline, &secret()).is_err());
}

#[test]
fn a_wrong_master_password_is_said_so() {
    let server = FakeS3::start();
    cleanpassword_device(&server, "CleanPassword");
    let pw = service(&server);
    pw.unlock("correct horse battery!".into());
    wait(&pw, |s| s.busy.is_none());
    assert!(!pw.state().unlocked());
    assert_eq!(pw.state().error.as_deref(), Some("That isn't the master password."));
}

#[test]
fn deleting_recovering_and_deleting_for_good_reach_cleanpassword() {
    let server = FakeS3::start();
    let mut device = cleanpassword_device(&server, "CleanPassword");
    let gh = by_name(&device, "GitHub").id.clone();
    let wifi = by_name(&device, "Wi-Fi").id.clone();
    let old = by_name(&device, "Old Forum").id.clone();
    let bank = by_name(&device, "bank.example").id.clone();
    let pw = unlocked(&server);

    pw.change(vec![gh.clone(), wifi.clone()], Change::Delete);
    assert_eq!(titles(&pw), ["bank.example"], "shown at once");
    assert!(pw.state().saving());
    pw.change(vec![old.clone()], Change::Recover);
    pw.change(vec![bank.clone()], Change::DeleteForever);
    assert_eq!(titles(&pw), ["Old Forum"]);
    wait(&pw, |s| !s.saving());
    assert!(pw.state().error.is_none(), "{:?}", pw.state().error);
    assert_eq!(titles(&pw), ["Old Forum"]);
    assert_eq!(pw.state().deleted().len(), 2);
    assert!(!pw.state().vault().unwrap().has_unsynced_changes());

    // CleanPassword's devices see it at their next sync.
    let report = sync(&mut device);
    assert_eq!((report.downloaded, report.removed), (3, 1));
    assert!(device.get(&gh).unwrap().deleted.is_some() && device.get(&wifi).unwrap().deleted.is_some());
    assert!(device.get(&old).unwrap().deleted.is_none());
    assert!(device.get(&bank).is_none());
    // Fields CleanNotes doesn't show are as they were.
    assert_eq!(device.get(&gh).unwrap().history.len(), 1);
    assert_eq!(device.get(&gh).unwrap().fields[0].value, "4321");
    let other = join(&server, "CleanPassword");
    assert_eq!(other.items().len(), 1);
    assert_eq!(other.trash().len(), 2);

    // Recovered here: there too.
    pw.change(vec![gh.clone()], Change::Recover);
    wait(&pw, |s| !s.saving());
    sync(&mut device);
    assert!(device.get(&gh).unwrap().deleted.is_none());
}

#[test]
fn changes_merge_with_another_devices() {
    let server = FakeS3::start();
    let mut device = cleanpassword_device(&server, "CleanPassword");
    let gh = by_name(&device, "GitHub").id.clone();
    let bank = by_name(&device, "bank.example").id.clone();
    let pw = unlocked(&server);

    let edit = |device: &mut Vault, id: &str| {
        let data = ItemData { notes: "edited elsewhere".into(), ..device.get(id).unwrap().data() };
        device.update(id, data).unwrap();
        sync(device);
    };

    // CleanPassword edits an item meanwhile, and CleanNotes deletes it for
    // good without having seen the edit: an edit beats deleting for good,
    // and that's said.
    edit(&mut device, &bank);
    pw.change(vec![bank.clone()], Change::DeleteForever);
    assert!(pw.state().item(&bank).is_none());
    wait(&pw, |s| !s.saving());
    assert_eq!(pw.state().item(&bank).unwrap().notes, "edited elsewhere");
    assert_eq!(pw.state().error.as_deref(), Some("1 item was changed elsewhere meanwhile, so it was kept."));

    // Edited there and deleted here: both kept.
    edit(&mut device, &gh);
    pw.change(vec![gh.clone()], Change::Delete);
    wait(&pw, |s| !s.saving());
    {
        let s = pw.state();
        let item = s.item(&gh).unwrap();
        assert!(item.deleted.is_some() && item.notes == "edited elsewhere");
        assert!(s.error.is_none());
    }
    sync(&mut device);
    assert!(device.get(&gh).unwrap().deleted.is_some());
    assert!(device.get(&bank).is_some());
}

#[test]
fn refreshing_picks_up_another_devices_changes() {
    let server = FakeS3::start();
    let mut device = cleanpassword_device(&server, "CleanPassword");
    let pw = unlocked(&server);

    // Nothing changed: vault.json and a HEAD of vault.bin, nothing written.
    requests(&server);
    pw.refresh();
    wait(&pw, |s| s.busy.is_none());
    assert_eq!(requests(&server), ["GET CleanPassword/vault.json", "HEAD CleanPassword/vault.bin"]);

    let id = device.add(login("Example", "https://example.com", "me", "pw")).unwrap();
    let wifi = by_name(&device, "Wi-Fi").id.clone();
    device.purge(&wifi).unwrap();
    sync(&mut device);
    pw.refresh();
    wait(&pw, |s| s.busy.is_none());
    assert!(pw.state().error.is_none());
    assert_eq!(titles(&pw), ["bank.example", "Example", "GitHub"]);
    assert_eq!(pw.state().item(&id).unwrap().username, "me");
}

#[test]
fn locking_wipes_and_wins_over_an_unlock_under_way() {
    let server = FakeS3::start();
    cleanpassword_device(&server, "CleanPassword");
    let pw = unlocked(&server);
    pw.lock();
    assert!(!pw.state().unlocked());
    assert!(pw.state().items().is_empty() && pw.state().vault().is_none());

    pw.unlock(PASSWORD.into());
    pw.lock();
    std::thread::sleep(Duration::from_secs(3));
    assert!(!pw.state().unlocked());

    // The vault removed from the bucket: forgotten at the next search.
    server.state().objects.clear();
    pw.search(vec![("acct".into(), notes_config(&server), secret())]);
    wait(&pw, |s| !s.searching);
    assert!(pw.state().found.is_none());
}

#[test]
fn a_change_that_cant_be_saved_is_kept_and_tried_again() {
    let server = FakeS3::start();
    let mut device = cleanpassword_device(&server, "CleanPassword");
    let gh = by_name(&device, "GitHub").id.clone();
    let pw = unlocked(&server);
    server.state().refuse_puts = true;
    pw.change(vec![gh.clone()], Change::Delete);
    wait(&pw, |s| !s.saving());
    assert!(pw.state().error.as_deref().unwrap().starts_with("Couldn't save the change to the vault"), "{:?}", pw.state().error);
    assert!(pw.state().item(&gh).unwrap().deleted.is_some());
    assert!(pw.state().vault().unwrap().has_unsynced_changes());

    server.state().refuse_puts = false;
    pw.refresh();
    wait(&pw, |s| s.busy.is_none());
    assert!(pw.state().error.is_none());
    sync(&mut device);
    assert!(device.get(&gh).unwrap().deleted.is_some());
}

#[test]
fn unlocking_again_opens_what_this_computer_kept() {
    let server = FakeS3::start();
    let mut device = cleanpassword_device(&server, "CleanPassword");
    let dir = tempfile::tempdir().unwrap();
    let pw = Passwords::new(|| {});
    pw.set_cache_dir(dir.path().to_owned());
    pw.search(vec![("acct".into(), notes_config(&server), secret())]);
    wait(&pw, |s| !s.searching);
    pw.unlock(PASSWORD.into());
    wait(&pw, |s| s.busy.is_none());
    assert_eq!(pw.state().items().len(), 3);
    // Kept sealed: no names, passwords or access key in the file, and
    // neither the master password nor the key.
    let path = cache_path(dir.path(), device.vault_id()).unwrap();
    let raw = std::fs::read_to_string(&path).unwrap();
    for secret in ["GitHub", "hunter2", "new-password", SECRET_KEY, PASSWORD, device.key_b64().as_str()] {
        assert!(!raw.contains(secret), "{secret} is in the kept vault");
    }

    // Unlocked again with nothing changed: nothing downloaded.
    pw.lock();
    requests(&server);
    pw.unlock(PASSWORD.into());
    wait(&pw, |s| s.busy.is_none());
    assert_eq!(pw.state().items().len(), 3);
    assert!(!requests(&server).contains(&"GET CleanPassword/vault.bin".to_owned()));

    // Changed elsewhere meanwhile: downloaded and shown.
    let gh = by_name(&device, "GitHub").id.clone();
    device.delete(&gh).unwrap();
    sync(&mut device);
    pw.lock();
    pw.unlock(PASSWORD.into());
    wait(&pw, |s| s.busy.is_none());
    assert_eq!(pw.state().items().len(), 2);

    // A wrong master password doesn't open what was kept either.
    pw.lock();
    pw.unlock("not the password".into());
    wait(&pw, |s| s.busy.is_none());
    assert!(!pw.state().unlocked());
    assert_eq!(pw.state().error.as_deref(), Some("That isn't the master password."));

    // S3 can't be read: what was kept is shown, and said so.
    server.state().forbidden.insert("CleanPassword/vault.json".into());
    pw.unlock(PASSWORD.into());
    wait(&pw, |s| s.busy.is_none());
    assert!(pw.state().unlocked());
    assert_eq!(pw.state().items().len(), 2);
    assert!(pw.state().error.as_deref().unwrap().contains("showing what this computer kept"), "{:?}", pw.state().error);
}

#[test]
fn titles_search_and_folders() {
    let now = chrono::Utc::now();
    let item = |d: ItemData| Item::new(d, now);
    let bank = item(ItemData { uris: vec![ItemUri::new("https://www.example.com/x")], folder: "Money/Banks".into(), ..Default::default() });
    assert_eq!(bank.title(), "example.com");
    assert_eq!(item(ItemData { uris: vec![ItemUri::new("example.org")], ..Default::default() }).title(), "example.org");
    assert_eq!(item(ItemData { username: "me".into(), ..Default::default() }).title(), "me");
    assert_eq!(item(ItemData::default()).title(), "Untitled");
    assert!(bank.matches("EXAMPLE banks"));
    assert!(!bank.matches("example work"));
    assert!(bank.in_folder("Money") && bank.in_folder("Money/Banks") && !bank.in_folder("Mon"));
    assert_eq!(host("github.com/login").as_deref(), Some("github.com"));
}

#[test]
fn verification_codes() {
    assert!(totp::code_now("JBSWY3DPEHPK3PXP").is_some());
    assert!(totp::code_now("").is_none() && totp::code_now("not base32!").is_none());
    assert_eq!(totp::code("GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQ", 59).unwrap().code, "287082");
    assert_eq!(totp::spaced("287082"), "287 082");
    assert_eq!(totp::spaced("2B4C6"), "2B4C6");
}

#[test]
fn subtitles_by_kind() {
    let now = chrono::Utc::now();
    let item = |data: ItemData| Item::new(data, now);
    assert_eq!(item(login("GitHub", "https://github.com", "octocat", "pw")).subtitle(), "octocat");
    assert_eq!(item(ItemData { kind: ItemKind::Note, name: "Wi-Fi".into(), ..Default::default() }).subtitle(), "Secure Note");
    let public = Field { name: "Public key".into(), value: "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAA me@example.com".into(), hidden: false };
    let key = |fields| item(ItemData { kind: ItemKind::SshKey, name: "Server".into(), fields, ..Default::default() });
    assert_eq!(key(vec![public]).subtitle(), "ssh-ed25519", "the key's type, as in CleanPassword");
    assert_eq!(key(Vec::new()).subtitle(), "SSH key");
}
