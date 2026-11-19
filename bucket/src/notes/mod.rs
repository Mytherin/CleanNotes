//! A CleanNotes account's S3 folder, for apps other than CleanNotes that
//! put notes in it (CleanTabs' Chrome extension saves tabs as notes), and
//! the parts of it CleanNotes' own sync uses:
//!
//! ```text
//! <folder>/head                  rewritten after every device's writes
//! <folder>/encryption.json       if the folder is encrypted (see crypto)
//! <folder>/notes/<stem>.md       a note: front matter + Markdown (see format)
//! <folder>/folders/<id>.json     a folder: its path, as a JSON list
//! <folder>/attachments/<name>    a picture a note shows
//! ```
//!
//! [`Folder`] reads and writes these as CleanNotes does: notes and folders
//! sealed when the folder is encrypted, creates with `If-None-Match: *`,
//! updates with `If-Match`, and `head` touched after writes, so CleanNotes'
//! sync picks them up and merges them like its own.
//!
//! It's async over the app's [`Transport`] and never blocks, so it runs in
//! WebAssembly.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::s3::{Client, Cond, Put, Transport};
use crate::{Error, Result};

pub mod crypto;
pub mod format;
pub mod tabs;

/// Where notes without a folder go.
pub const DEFAULT_FOLDER: &str = "Notes";
/// The change marker (see [`Client::touch`]).
pub const HEAD: &str = "head";
pub const NOTES: &str = "notes/";
pub const FOLDERS: &str = "folders/";
pub const ATTACHMENTS: &str = "attachments/";
const MARKDOWN: &str = "text/markdown; charset=utf-8";

/// A note as a backend stores it. Folders are given by path from the top
/// of the account; the default folder is `["Notes"]`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct NoteData {
    /// The note's id in the app (a UUID). A note a backend didn't get from
    /// the app (say, a Markdown file put into the S3 folder by hand) gets one made up
    /// from its remote id.
    pub id: String,
    pub folder: Vec<String>,
    /// Markdown.
    pub body: String,
    pub created: DateTime<Utc>,
    pub modified: DateTime<Utc>,
    pub pinned: bool,
    /// In Recently Deleted since then.
    pub deleted: Option<DateTime<Utc>>,
}

/// The `#tags` in a line of a note: lower case, without the `#`.
pub fn tags_in(text: &str) -> Vec<String> {
    let chars: Vec<char> = text.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '#' && (i == 0 || chars[i - 1].is_whitespace() || chars[i - 1] == '(') {
            let mut j = i + 1;
            while j < chars.len() && (chars[j].is_alphanumeric() || matches!(chars[j], '-' | '_' | '/')) {
                j += 1;
            }
            let tag: String = chars[i + 1..j].iter().collect();
            let tag = tag.trim_end_matches(['-', '/']).to_lowercase();
            if !tag.is_empty() && tag.chars().any(|c| !c.is_ascii_digit()) {
                out.push(tag);
            }
            i = j;
        } else {
            i += 1;
        }
    }
    out
}

/// An attachment's name: the SHA-256 of its bytes, and its extension.
pub fn attachment_name(data: &[u8], ext: &str) -> String {
    let hex: String = Sha256::digest(data).iter().map(|b| format!("{b:02x}")).collect();
    format!("{hex}.{ext}")
}

/// A note in the folder.
#[derive(Clone, Debug, PartialEq)]
pub struct StoredNote {
    /// Its object's name without `notes/` and `.md`.
    pub stem: String,
    pub etag: String,
    pub data: NoteData,
}

/// A folder object.
#[derive(Clone, Debug, PartialEq)]
pub struct StoredFolder {
    pub id: String,
    pub path: Vec<String>,
}

/// A CleanNotes account's S3 folder.
pub struct Folder<'a, T: Transport> {
    client: Client<'a, T>,
    /// The folder's encryption key, when it's encrypted.
    key: Option<[u8; 32]>,
}

impl<'a, T: Transport> Folder<'a, T> {
    /// `key`: from [`crypto::unlock`] with the folder's [`Folder::settings`].
    pub fn new(client: Client<'a, T>, key: Option<[u8; 32]>) -> Self {
        Folder { client, key }
    }

    pub fn client(&self) -> &Client<'a, T> {
        &self.client
    }

    /// The folder's encryption settings; `None` if it isn't encrypted.
    pub async fn settings(client: &Client<'_, T>) -> Result<Option<crypto::Settings>> {
        match client.get(crypto::SETTINGS).await? {
            Some(o) => serde_json::from_slice(&o.data).map(Some).map_err(|_| Error::invalid("the folder's encryption.json is damaged")),
            None => Ok(None),
        }
    }

    fn sealable(rel: &str) -> bool {
        rel.starts_with(NOTES) || rel.starts_with(FOLDERS)
    }

    /// An object's text, decrypted if it's sealed, with its ETag. The text
    /// is `None` if it can't be decrypted (another key, or damaged).
    pub async fn read(&self, rel: &str) -> Result<Option<(Option<String>, String)>> {
        let Some(o) = self.client.get(rel).await? else { return Ok(None) };
        let raw = String::from_utf8_lossy(&o.data).into_owned();
        let text = if crypto::is_sealed(&raw) { self.key.as_ref().and_then(|k| crypto::open(k, rel, &raw)) } else { Some(raw) };
        Ok(Some((text, o.etag)))
    }

    /// Writes text, sealed if the folder is encrypted. `Ok(None)` when
    /// `cond` failed.
    pub async fn write(&self, rel: &str, text: &str, content_type: &str, cond: Cond<'_>) -> Result<Option<String>> {
        let (body, ct) = match &self.key {
            Some(k) if Self::sealable(rel) => (crypto::seal(k, rel, text).into_bytes(), "application/octet-stream"),
            _ => (text.as_bytes().to_vec(), content_type),
        };
        Ok(match self.client.put_as(rel, body, cond, ct).await? {
            Put::Stored { etag } => Some(etag),
            Put::Conflict => None,
        })
    }

    /// Notes whose stem starts with `stem_prefix` (`""` for all). Ones that
    /// can't be read (sealed with another key) are left out; tombstones too.
    pub async fn notes(&self, stem_prefix: &str) -> Result<Vec<StoredNote>> {
        let mut out = Vec::new();
        for l in self.client.list(&format!("{NOTES}{stem_prefix}")).await? {
            let Some(stem) = l.name.strip_prefix(NOTES).and_then(|n| n.strip_suffix(".md")) else { continue };
            if stem.contains('/') {
                continue;
            }
            let Some((Some(text), etag)) = self.read(&l.name).await? else { continue };
            if let format::Object::Note(data) = format::read(&text, stem, l.modified.unwrap_or_else(Utc::now)) {
                out.push(StoredNote { stem: stem.to_owned(), etag, data });
            }
        }
        Ok(out)
    }

    /// Every folder object.
    pub async fn folders(&self) -> Result<Vec<StoredFolder>> {
        let mut out = Vec::new();
        for l in self.client.list(FOLDERS).await? {
            let Some(id) = l.name.strip_prefix(FOLDERS).and_then(|n| n.strip_suffix(".json")) else { continue };
            let Some((Some(text), _)) = self.read(&l.name).await? else { continue };
            if let Ok(path) = serde_json::from_str::<Vec<String>>(&text) {
                if !path.is_empty() {
                    out.push(StoredFolder { id: id.to_owned(), path });
                }
            }
        }
        Ok(out)
    }

    /// Makes folder objects for `path` and each of its parents that
    /// `known` doesn't have (adding them to it). Returns whether it wrote.
    pub async fn ensure_folder(&self, path: &[String], known: &mut Vec<StoredFolder>) -> Result<bool> {
        let mut wrote = false;
        for n in 1..=path.len() {
            let p = &path[..n];
            if known.iter().any(|f| f.path == p) {
                continue;
            }
            let id = new_id();
            self.write(&format!("{FOLDERS}{id}.json"), &serde_json::to_string(p).expect("a path serializes"), "application/json", Cond::Absent).await?;
            known.push(StoredFolder { id, path: p.to_vec() });
            wrote = true;
        }
        Ok(wrote)
    }

    /// Writes a new note under its id. `Ok(None)` if there's one there.
    pub async fn create_note(&self, n: &NoteData) -> Result<Option<StoredNote>> {
        let stem = n.id.replace(['/', '\\'], "_");
        let etag = self.write(&format!("{NOTES}{stem}.md"), &format::write(n), MARKDOWN, Cond::Absent).await?;
        Ok(etag.map(|etag| StoredNote { stem, etag, data: n.clone() }))
    }

    /// Rewrites a note, if it's still at `etag`. `Ok(None)` if it changed.
    pub async fn update_note(&self, stem: &str, etag: &str, n: &NoteData) -> Result<Option<StoredNote>> {
        let etag = self.write(&format!("{NOTES}{stem}.md"), &format::write(n), MARKDOWN, Cond::Match(etag)).await?;
        Ok(etag.map(|etag| StoredNote { stem: stem.to_owned(), etag, data: n.clone() }))
    }

    /// Stores a picture under `name` (from [`attachment_name`]): what a
    /// note refers to as `attachments/<name>`. One already there is the
    /// same picture.
    pub async fn put_attachment(&self, name: &str, data: &[u8]) -> Result<()> {
        let (key, body, ct) = match &self.key {
            Some(k) => {
                let key = format!("{ATTACHMENTS}{}", crypto::attachment_object(k, name));
                let body = crypto::seal_bytes(k, &key, data);
                (key, body, "application/octet-stream".to_owned())
            }
            None => {
                let ext = name.rsplit('.').next().unwrap_or("png").replace("jpg", "jpeg");
                (format!("{ATTACHMENTS}{name}"), data.to_vec(), format!("image/{ext}"))
            }
        };
        self.client.put_as(&key, body, Cond::Absent, &ct).await?;
        Ok(())
    }

    /// Tells other devices something changed. Call once after writes.
    pub async fn touch(&self) -> Result<String> {
        self.client.touch(HEAD).await
    }
}

/// A random id: 32 hex digits.
pub fn new_id() -> String {
    crate::seal::random_bytes::<16>().iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::block_on;
    use crate::fake::FakeS3;
    use crate::s3::{Credentials, Location};

    fn location() -> Location {
        Location { bucket: "b".into(), region: "us-east-1".into(), prefix: "CleanNotes".into(), endpoint: None }
    }

    fn creds() -> Credentials {
        Credentials { access_key_id: "AK".into(), secret_access_key: "SK".into(), session_token: None }
    }

    fn note(id: &str, folder: &[&str]) -> NoteData {
        let t: DateTime<Utc> = DateTime::parse_from_rfc3339("2026-11-19T21:00:00.123Z").unwrap().into();
        NoteData { id: id.into(), folder: folder.iter().map(|s| s.to_string()).collect(), body: "# Hi\n#tag\n".into(), created: t, modified: t, pinned: false, deleted: None }
    }

    #[test]
    fn notes_folders_and_pictures_plain_and_encrypted() {
        for encrypted in [false, true] {
            let s3 = FakeS3::new();
            let (loc, cr) = (location(), creds());
            let key = encrypted.then(|| [9u8; 32]);
            let f = Folder::new(Client::new(&loc, &cr, &s3), key);
            block_on(async {
                assert!(Folder::settings(f.client()).await.unwrap().is_none());
                let n = note("tab-1", &["Tabs", "Reading"]);
                let stored = f.create_note(&n).await.unwrap().unwrap();
                assert!(f.create_note(&n).await.unwrap().is_none(), "there already");
                f.create_note(&note("other", &["Notes"])).await.unwrap().unwrap();
                let got = f.notes("tab-").await.unwrap();
                assert_eq!(got.len(), 1);
                assert_eq!(got[0].data, n);
                let mut moved = n.clone();
                moved.folder = vec!["Tabs".into()];
                assert!(f.update_note(&stored.stem, "\"stale\"", &moved).await.unwrap().is_none());
                assert!(f.update_note(&stored.stem, &stored.etag, &moved).await.unwrap().is_some());
                assert_eq!(f.notes("").await.unwrap().len(), 2);

                let mut known = f.folders().await.unwrap();
                assert!(f.ensure_folder(&["Tabs".into(), "Reading".into()], &mut known).await.unwrap());
                assert!(!f.ensure_folder(&["Tabs".into()], &mut known).await.unwrap());
                let mut paths: Vec<Vec<String>> = f.folders().await.unwrap().into_iter().map(|f| f.path).collect();
                paths.sort();
                assert_eq!(paths, vec![vec!["Tabs".to_string()], vec!["Tabs".to_string(), "Reading".to_string()]]);

                let pic = b"\xff\xd8\xff jpeg";
                let name = attachment_name(pic, "jpg");
                f.put_attachment(&name, pic).await.unwrap();
                f.put_attachment(&name, pic).await.unwrap();
                f.touch().await.unwrap();
            });
            let objects = s3.objects.borrow();
            let note_obj = &objects.iter().find(|(k, _)| k.ends_with("notes/tab-1.md")).unwrap().1 .0;
            assert_eq!(crypto::is_sealed(&String::from_utf8_lossy(note_obj)), encrypted);
            assert_eq!(objects.keys().any(|k| k.contains("attachments/") && k.ends_with(".jpg")), !encrypted, "encrypted pictures are named by HMAC");
            assert!(objects.contains_key("CleanNotes/head"));
        }
    }

    #[test]
    fn notes_sealed_with_another_key_are_skipped() {
        let s3 = FakeS3::new();
        let (loc, cr) = (location(), creds());
        block_on(async {
            Folder::new(Client::new(&loc, &cr, &s3), Some([1; 32])).create_note(&note("tab-1", &["Tabs"])).await.unwrap();
            assert!(Folder::new(Client::new(&loc, &cr, &s3), Some([2; 32])).notes("").await.unwrap().is_empty());
        });
    }
}
