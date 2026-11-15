//! Keeps everything in `data.db`, a SQLite database in the app's data folder
//! (`%APPDATA%\CleanNotes` on Windows, `~/Library/Application Support/
//! CleanNotes` on macOS, `~/.local/share/cleannotes` on Linux; or
//! `CLEANNOTES_DATA`).
//!
//! Notes are rows with their Markdown in `body`; accounts, folders and sync
//! bookkeeping are JSON. Secrets (a sync account's session and keys) go to
//! the OS credential store, never into the database: where there is no
//! store, they're kept in memory only, until the app quits.

use std::collections::{HashMap, HashSet};
use std::sync::Mutex;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use rusqlite::{params, Connection, OptionalExtension};

use crate::model::{Account, Folder, Note, Store, Tombstone};
use crate::settings::Settings;

const KEYRING_SERVICE: &str = "CleanNotes";

/// The desktop app's data folder. (The iPhone app passes its own.)
#[cfg(not(any(target_os = "ios", target_os = "android")))]
pub fn data_dir() -> PathBuf {
    if let Some(d) = std::env::var_os("CLEANNOTES_DATA") {
        return PathBuf::from(d);
    }
    let base = directories::BaseDirs::new().map(|b| b.data_dir().to_path_buf()).unwrap_or_else(|| PathBuf::from("."));
    base.join(if cfg!(target_os = "linux") { "cleannotes" } else { "CleanNotes" })
}

/// Where secrets go.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Secrets {
    /// The OS credential store, falling back to memory.
    Keyring,
    /// Memory only (tests).
    Memory,
}

pub struct Db {
    conn: Connection,
    secrets: Secrets,
    /// Keyring entry names are qualified by the data folder when it isn't
    /// the default one, so test copies don't share secrets.
    keyring_prefix: String,
    /// What was saved last, to only write what changed.
    saved_meta: Option<String>,
    /// Secrets the credential store couldn't take (or every secret, with
    /// [`Secrets::Memory`]): gone when the app quits.
    in_memory: Mutex<HashMap<String, String>>,
}

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS accounts (id TEXT PRIMARY KEY, position INTEGER NOT NULL, data TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS folders (id TEXT PRIMARY KEY, account TEXT NOT NULL, data TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS notes (
    id       TEXT PRIMARY KEY,
    account  TEXT NOT NULL,
    folder   TEXT NOT NULL,
    title    TEXT NOT NULL,
    body     TEXT NOT NULL,
    created  TEXT NOT NULL,
    modified TEXT NOT NULL,
    pinned   INTEGER NOT NULL,
    deleted  TEXT,
    sync     TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS tombstones (data TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS settings (key TEXT PRIMARY KEY, value TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS secrets (key TEXT PRIMARY KEY, value TEXT NOT NULL);
";

/// Whether opening failed because the file isn't a (sound) database.
fn is_corrupt(e: &anyhow::Error) -> bool {
    e.chain().any(|c| {
        matches!(
            c.downcast_ref::<rusqlite::Error>().and_then(rusqlite::Error::sqlite_error_code),
            Some(rusqlite::ErrorCode::DatabaseCorrupt | rusqlite::ErrorCode::NotADatabase)
        )
    })
}

fn ts(t: DateTime<Utc>) -> String {
    t.to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

fn parse_ts(s: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(s).map(|t| t.with_timezone(&Utc)).unwrap_or_default()
}

impl Db {
    pub fn open(dir: &Path, secrets: Secrets) -> Result<(Db, Store, Settings)> {
        std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
        let path = dir.join("data.db");
        let conn = match Self::connect(&path) {
            Ok(c) => c,
            // Only a file that isn't a database (or is damaged) is moved
            // aside to start over. A locked or briefly unreadable one (a
            // backup tool, a sync client) is an error: starting empty would
            // look like every note was gone.
            Err(e) if is_corrupt(&e) => {
                let aside = path.with_extension(format!("db.broken-{}", Utc::now().format("%Y%m%d%H%M%S")));
                eprintln!("data.db can't be read ({e:#}); moving it to {}", aside.display());
                std::fs::rename(&path, &aside).with_context(|| format!("moving the unreadable {} aside", path.display()))?;
                Self::connect(&path)?
            }
            Err(e) => return Err(e.context(format!("opening {}", path.display()))),
        };
        let keyring_prefix = if std::env::var_os("CLEANNOTES_DATA").is_some() || secrets == Secrets::Memory {
            format!("{}:", dir.display())
        } else {
            String::new()
        };
        let mut db = Db { conn, secrets, keyring_prefix, saved_meta: None, in_memory: Mutex::new(HashMap::new()) };
        db.move_secrets_out()?;
        let (store, settings) = db.load()?;
        Ok((db, store, settings))
    }

    fn connect(path: &Path) -> Result<Connection> {
        let conn = Connection::open(path)?;
        // Wait out another process holding a lock rather than failing.
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        // Freed space is overwritten, so a note's text from before it was
        // locked (or deleted for good) doesn't linger in the file.
        conn.execute_batch("PRAGMA journal_mode = WAL; PRAGMA secure_delete = ON;")?;
        conn.execute_batch(SCHEMA)?;
        Ok(conn)
    }

    fn load(&mut self) -> Result<(Store, Settings)> {
        let mut store = Store::default();
        let json = |s: String| -> Option<serde_json::Value> { serde_json::from_str(&s).ok() };
        {
            let mut st = self.conn.prepare("SELECT data FROM accounts ORDER BY position")?;
            for row in st.query_map([], |r| r.get::<_, String>(0))? {
                if let Some(a) = json(row?).and_then(|v| serde_json::from_value::<Account>(v).ok()) {
                    store.accounts.push(a);
                }
            }
            let mut st = self.conn.prepare("SELECT data FROM folders")?;
            for row in st.query_map([], |r| r.get::<_, String>(0))? {
                if let Some(f) = json(row?).and_then(|v| serde_json::from_value::<Folder>(v).ok()) {
                    store.folders.push(f);
                }
            }
            let mut st = self.conn.prepare("SELECT data FROM tombstones")?;
            for row in st.query_map([], |r| r.get::<_, String>(0))? {
                if let Some(t) = json(row?).and_then(|v| serde_json::from_value::<Tombstone>(v).ok()) {
                    store.tombstones.push(t);
                }
            }
            let mut st = self.conn.prepare(
                "SELECT id, account, folder, body, created, modified, pinned, deleted, sync FROM notes",
            )?;
            let rows = st.query_map([], |r| {
                Ok(Note {
                    id: r.get(0)?,
                    account: r.get(1)?,
                    folder: r.get(2)?,
                    body: r.get(3)?,
                    created: parse_ts(&r.get::<_, String>(4)?),
                    modified: parse_ts(&r.get::<_, String>(5)?),
                    pinned: r.get(6)?,
                    deleted: r.get::<_, Option<String>>(7)?.as_deref().map(parse_ts),
                    sync: serde_json::from_str(&r.get::<_, String>(8)?).unwrap_or_default(),
                    version: 0,
                })
            })?;
            for n in rows {
                store.notes.push(n?);
            }
        }
        let settings = self
            .conn
            .query_row("SELECT value FROM settings WHERE key = 'settings'", [], |r| r.get::<_, String>(0))
            .optional()?
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();
        store.ensure_local();
        Ok((store, settings))
    }

    /// Saves the notes in `notes` (deleting those no longer in the store),
    /// and accounts, folders, tombstones and settings if they changed.
    pub fn save(&mut self, store: &Store, settings: &Settings, notes: &HashSet<String>) -> Result<()> {
        let meta = serde_json::to_string(&(&store.accounts, &store.folders, &store.tombstones, settings))?;
        let meta_changed = self.saved_meta.as_deref() != Some(meta.as_str());
        if !meta_changed && notes.is_empty() {
            return Ok(());
        }
        let tx = self.conn.transaction()?;
        for id in notes {
            match store.note(id) {
                Some(n) => {
                    tx.execute(
                        "INSERT OR REPLACE INTO notes (id, account, folder, title, body, created, modified, pinned, deleted, sync)
                         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
                        params![
                            n.id,
                            n.account,
                            n.folder,
                            n.title(),
                            n.body,
                            ts(n.created),
                            ts(n.modified),
                            n.pinned,
                            n.deleted.map(ts),
                            serde_json::to_string(&n.sync)?
                        ],
                    )?;
                }
                None => {
                    tx.execute("DELETE FROM notes WHERE id = ?1", [id])?;
                }
            }
        }
        if meta_changed {
            tx.execute("DELETE FROM accounts", [])?;
            for (i, a) in store.accounts.iter().enumerate() {
                tx.execute(
                    "INSERT INTO accounts (id, position, data) VALUES (?1, ?2, ?3)",
                    params![a.id, i as i64, serde_json::to_string(a)?],
                )?;
            }
            tx.execute("DELETE FROM folders", [])?;
            for f in &store.folders {
                tx.execute(
                    "INSERT INTO folders (id, account, data) VALUES (?1, ?2, ?3)",
                    params![f.id, f.account, serde_json::to_string(f)?],
                )?;
            }
            tx.execute("DELETE FROM tombstones", [])?;
            for t in &store.tombstones {
                tx.execute("INSERT INTO tombstones (data) VALUES (?1)", [serde_json::to_string(t)?])?;
            }
            tx.execute(
                "INSERT OR REPLACE INTO settings (key, value) VALUES ('settings', ?1)",
                [serde_json::to_string(settings)?],
            )?;
        }
        tx.commit()?;
        self.saved_meta = Some(meta);
        Ok(())
    }

    /// Saves every note (after a sync changed many).
    pub fn save_all(&mut self, store: &Store, settings: &Settings) -> Result<()> {
        let mut ids: HashSet<String> = store.notes.iter().map(|n| n.id.clone()).collect();
        let mut st = self.conn.prepare("SELECT id FROM notes")?;
        let saved: Vec<String> = st.query_map([], |r| r.get(0))?.collect::<rusqlite::Result<_>>()?;
        drop(st);
        ids.extend(saved);
        self.save(store, settings, &ids)
    }

    fn keyring_name(&self, account: &str) -> String {
        format!("{}account-{account}", self.keyring_prefix)
    }

    pub fn secret(&self, account: &str) -> Option<String> {
        if let Some(s) = self.in_memory.lock().unwrap().get(account) {
            return Some(s.clone());
        }
        if self.secrets == Secrets::Keyring {
            if let Ok(entry) = keyring::Entry::new(KEYRING_SERVICE, &self.keyring_name(account)) {
                match entry.get_password() {
                    Ok(s) => return Some(s),
                    Err(keyring::Error::NoEntry) => {}
                    Err(e) => eprintln!("credential store: {e}"),
                }
            }
        }
        None
    }

    /// Saves (or with `None`, deletes) an account's secret: in the OS
    /// credential store, or, if that can't take it, in memory only. It's
    /// never written to disk in the clear (it decrypts the account).
    pub fn set_secret(&self, account: &str, secret: Option<&str>) -> Result<()> {
        let mut memory = self.in_memory.lock().unwrap();
        memory.remove(account);
        if self.secrets == Secrets::Keyring {
            let entry = keyring::Entry::new(KEYRING_SERVICE, &self.keyring_name(account));
            match (entry, secret) {
                (Ok(e), Some(s)) if e.set_password(s).is_ok() => return Ok(()),
                (Ok(e), None) => {
                    let _ = e.delete_credential();
                    return Ok(());
                }
                (_, None) => return Ok(()),
                _ => eprintln!("no credential store: the {account} sign-in is kept until CleanNotes quits"),
            }
        }
        if let Some(s) = secret {
            memory.insert(account.to_owned(), s.to_owned());
        }
        Ok(())
    }

    /// Whether an account's sign-in is only kept until the app quits (the
    /// credential store couldn't take it).
    pub fn secret_in_memory_only(&self, account: &str) -> bool {
        self.secrets == Secrets::Keyring && self.in_memory.lock().unwrap().contains_key(account)
    }

    /// Earlier versions kept secrets in the database when the credential
    /// store failed. They move out (to the store, or to memory) and are
    /// wiped from the file.
    fn move_secrets_out(&mut self) -> Result<()> {
        let rows: Vec<(String, String)> = {
            let mut st = self.conn.prepare("SELECT key, value FROM secrets")?;
            let rows = st.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?.collect::<rusqlite::Result<_>>()?;
            rows
        };
        if rows.is_empty() {
            return Ok(());
        }
        for (account, secret) in &rows {
            self.set_secret(account, Some(secret))?;
        }
        // secure_delete overwrites the rows; the checkpoint clears the log.
        self.conn.execute("DELETE FROM secrets", [])?;
        self.conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::LOCAL;

    #[test]
    fn everything_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let (mut db, mut store, mut settings) = Db::open(dir.path(), Secrets::Memory).unwrap();
        assert_eq!(store.accounts.len(), 1);
        let folder = store.default_folder(LOCAL).unwrap().id.clone();
        let mut n = Note::new(LOCAL, &folder, "# Hello\nworld");
        n.pinned = true;
        n.sync.revision = Some("r".into());
        let id = n.id.clone();
        store.notes.push(n);
        store.folders.push(Folder::new(LOCAL, None, "Work"));
        settings.text_size = 19.0;
        db.save(&store, &settings, &[id.clone()].into()).unwrap();
        drop(db);

        let (mut db, store2, settings2) = Db::open(dir.path(), Secrets::Memory).unwrap();
        assert_eq!(settings2.text_size, 19.0);
        assert_eq!(store2.folders.len(), 2);
        let n2 = store2.note(&id).unwrap();
        assert_eq!(n2.body, "# Hello\nworld");
        assert!(n2.pinned);
        assert_eq!(n2.sync.revision.as_deref(), Some("r"));
        assert_eq!(n2.created.timestamp_millis(), store.note(&id).unwrap().created.timestamp_millis());

        // Deleting.
        let mut store3 = store2.clone();
        store3.notes.clear();
        db.save(&store3, &settings2, &[id.clone()].into()).unwrap();
        drop(db);
        let (_, store4, _) = Db::open(dir.path(), Secrets::Memory).unwrap();
        assert!(store4.notes.is_empty());
    }

    #[test]
    fn secrets_never_reach_the_database_file() {
        let dir = tempfile::tempdir().unwrap();
        let (db, _, _) = Db::open(dir.path(), Secrets::Memory).unwrap();
        assert_eq!(db.secret("a"), None);
        db.set_secret("a", Some("s3cret")).unwrap();
        assert_eq!(db.secret("a").as_deref(), Some("s3cret"));
        let rows: i64 = db.conn.query_row("SELECT count(*) FROM secrets", [], |r| r.get(0)).unwrap();
        assert_eq!(rows, 0);
        db.set_secret("a", None).unwrap();
        assert_eq!(db.secret("a"), None);
        // Gone when the app quits.
        db.set_secret("a", Some("s3cret")).unwrap();
        drop(db);
        let (db, _, _) = Db::open(dir.path(), Secrets::Memory).unwrap();
        assert_eq!(db.secret("a"), None);
    }

    #[test]
    fn secrets_left_in_the_file_by_older_versions_are_moved_out_and_wiped() {
        let dir = tempfile::tempdir().unwrap();
        {
            let (db, _, _) = Db::open(dir.path(), Secrets::Memory).unwrap();
            db.conn.execute("INSERT INTO secrets (key, value) VALUES ('acc', 'MASTER-KEY-1234')", []).unwrap();
        }
        let (db, _, _) = Db::open(dir.path(), Secrets::Memory).unwrap();
        assert_eq!(db.secret("acc").as_deref(), Some("MASTER-KEY-1234"), "still signed in this session");
        drop(db);
        // Not in the file any more, not even in freed pages or the log.
        for name in ["data.db", "data.db-wal"] {
            if let Ok(bytes) = std::fs::read(dir.path().join(name)) {
                assert!(!bytes.windows(15).any(|w| w == b"MASTER-KEY-1234"), "found in {name}");
            }
        }
    }

    #[test]
    fn a_file_that_isnt_a_database_is_moved_aside() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("data.db"), "not a database, just some text ".repeat(200)).unwrap();
        let (_, store, _) = Db::open(dir.path(), Secrets::Memory).unwrap();
        assert!(store.notes.is_empty());
        let names: Vec<String> = std::fs::read_dir(dir.path()).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
        assert!(names.iter().any(|n| n.starts_with("data.db.broken-")), "{names:?}");
    }

    #[test]
    fn only_damage_counts_as_corrupt() {
        let busy = anyhow::Error::new(rusqlite::Error::SqliteFailure(rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_BUSY), None));
        assert!(!is_corrupt(&busy), "a locked file isn't moved aside");
        let notadb = anyhow::Error::new(rusqlite::Error::SqliteFailure(rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_NOTADB), None));
        assert!(is_corrupt(&notadb.context("opening")));
    }
}
