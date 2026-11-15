//! A backend that keeps an account's notes in a SQLite database file, as
//! plain tables any SQLite tool can read:
//!
//! ```sql
//! folders(id, parent_id, name)
//! notes(id, folder_id, title, body, created, modified, pinned, deleted, revision)
//! tags(note_id, tag)
//! meta(key, value)
//! ```
//!
//! `notes.body` is the note's Markdown, `title` its first line and `tags`
//! its hashtags (both kept up to date for other tools; the app reads only
//! the body). Times are RFC 3339 in UTC. Each write bumps the note's
//! `revision` and the database's `meta.change` counter.
//!
//! Several computers can share one file (on a network share, or in a folder
//! synced by Dropbox and the like, as long as only one writes at a time).

use std::path::PathBuf;

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};
use serde::{Deserialize, Serialize};

use super::{Backend, Conflict, NoteData, RemoteFolder, RemoteNote, Snapshot};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Config {
    pub path: PathBuf,
}

pub struct SqliteBackend {
    conn: Connection,
}

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS folders (
    id        TEXT PRIMARY KEY,
    parent_id TEXT REFERENCES folders(id),
    name      TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS notes (
    id        TEXT PRIMARY KEY,
    folder_id TEXT REFERENCES folders(id),
    title     TEXT NOT NULL,
    body      TEXT NOT NULL,
    created   TEXT NOT NULL,
    modified  TEXT NOT NULL,
    pinned    INTEGER NOT NULL DEFAULT 0,
    deleted   TEXT,
    revision  INTEGER NOT NULL DEFAULT 1
);
CREATE TABLE IF NOT EXISTS tags (
    note_id TEXT NOT NULL REFERENCES notes(id) ON DELETE CASCADE,
    tag     TEXT NOT NULL,
    PRIMARY KEY (note_id, tag)
);
CREATE TABLE IF NOT EXISTS meta (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS notes_folder ON notes(folder_id);
INSERT OR IGNORE INTO meta (key, value) VALUES ('schema', '1'), ('change', '0');
";

impl SqliteBackend {
    pub fn open(config: &Config) -> Result<SqliteBackend> {
        if let Some(dir) = config.path.parent().filter(|d| !d.as_os_str().is_empty()) {
            std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
        }
        let conn = Connection::open(&config.path).with_context(|| format!("opening {}", config.path.display()))?;
        // Another computer may be writing to a shared file.
        conn.busy_timeout(std::time::Duration::from_secs(10))?;
        conn.execute_batch("PRAGMA foreign_keys = ON;")?;
        conn.execute_batch(SCHEMA).context("setting up the notes database")?;
        Ok(SqliteBackend { conn })
    }

    fn bump(tx: &rusqlite::Transaction) -> Result<()> {
        tx.execute("UPDATE meta SET value = CAST(value AS INTEGER) + 1 WHERE key = 'change'", [])?;
        Ok(())
    }

    /// The folder id at a path, `None` for the empty path.
    fn folder_id(conn: &Connection, path: &[String]) -> Result<Option<String>> {
        let mut parent: Option<String> = None;
        for name in path {
            let id: Option<String> = conn
                .query_row(
                    "SELECT id FROM folders WHERE name = ?1 AND parent_id IS ?2",
                    params![name, parent],
                    |r| r.get(0),
                )
                .optional()?;
            match id {
                Some(id) => parent = Some(id),
                None => return Ok(None),
            }
        }
        Ok(parent)
    }

    /// The folder id at a path, creating missing folders.
    fn ensure_folder(tx: &rusqlite::Transaction, path: &[String]) -> Result<Option<String>> {
        let mut parent: Option<String> = None;
        for name in path {
            let id: Option<String> = tx
                .query_row(
                    "SELECT id FROM folders WHERE name = ?1 AND parent_id IS ?2",
                    params![name, parent],
                    |r| r.get(0),
                )
                .optional()?;
            let id = match id {
                Some(id) => id,
                None => {
                    let id = uuid::Uuid::new_v4().to_string();
                    tx.execute("INSERT INTO folders (id, parent_id, name) VALUES (?1, ?2, ?3)", params![id, parent, name])?;
                    id
                }
            };
            parent = Some(id);
        }
        Ok(parent)
    }
}

fn ts(t: DateTime<Utc>) -> String {
    t.to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

fn parse_ts(s: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(s).map(|t| t.with_timezone(&Utc)).unwrap_or_default()
}

impl Backend for SqliteBackend {
    fn cursor(&mut self) -> Result<Option<String>> {
        let v: String = self.conn.query_row("SELECT value FROM meta WHERE key = 'change'", [], |r| r.get(0))?;
        Ok(Some(v))
    }

    fn pull(&mut self) -> Result<Snapshot> {
        let tx = self.conn.transaction()?;
        let mut folders: Vec<(String, Option<String>, String)> = Vec::new();
        {
            let mut st = tx.prepare("SELECT id, parent_id, name FROM folders")?;
            let rows = st.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
            for row in rows {
                folders.push(row?);
            }
        }
        let path_of = |id: &str| -> Vec<String> {
            let mut path = Vec::new();
            let mut cur = Some(id.to_owned());
            while let Some(c) = cur {
                let Some(f) = folders.iter().find(|f| f.0 == c) else { break };
                path.push(f.2.clone());
                cur = f.1.clone();
                if path.len() > 64 {
                    break;
                }
            }
            path.reverse();
            path
        };
        let mut snap = Snapshot {
            folders: folders.iter().map(|f| RemoteFolder { remote_id: f.0.clone(), path: path_of(&f.0) }).collect(),
            notes: Vec::new(),
            unreadable: Vec::new(),
        };
        let mut st = tx.prepare("SELECT id, folder_id, body, created, modified, pinned, deleted, revision FROM notes")?;
        let rows = st.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, Option<String>>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, String>(4)?,
                r.get::<_, bool>(5)?,
                r.get::<_, Option<String>>(6)?,
                r.get::<_, i64>(7)?,
            ))
        })?;
        for row in rows {
            let (id, folder, body, created, modified, pinned, deleted, revision) = row?;
            let folder = folder.map(|f| path_of(&f)).filter(|p| !p.is_empty());
            snap.notes.push(RemoteNote {
                remote_id: id.clone(),
                revision: revision.to_string(),
                data: NoteData {
                    id,
                    folder: folder.unwrap_or_else(|| vec![crate::model::DEFAULT_FOLDER.to_owned()]),
                    body,
                    created: parse_ts(&created),
                    modified: parse_ts(&modified),
                    pinned,
                    deleted: deleted.as_deref().map(parse_ts),
                },
            });
        }
        Ok(snap)
    }

    fn create_folder(&mut self, path: &[String]) -> Result<String> {
        let tx = self.conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let existed = Self::folder_id(&tx, path)?.is_some();
        let id = Self::ensure_folder(&tx, path)?.context("a folder needs a name")?;
        if !existed {
            Self::bump(&tx)?;
        }
        tx.commit()?;
        Ok(id)
    }

    fn rename_folder(&mut self, remote_id: &str, _old: &[String], new_path: &[String]) -> Result<()> {
        let tx = self.conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let (parent_path, name) = new_path.split_at(new_path.len().saturating_sub(1));
        let parent = Self::ensure_folder(&tx, parent_path)?;
        tx.execute(
            "UPDATE folders SET name = ?1, parent_id = ?2 WHERE id = ?3",
            params![name.first().context("a folder needs a name")?, parent, remote_id],
        )?;
        Self::bump(&tx)?;
        tx.commit()?;
        Ok(())
    }

    fn delete_folder(&mut self, remote_id: &str, _path: &[String]) -> Result<()> {
        let tx = self.conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        // Anything still in it (put there by another computer) moves up.
        let parent: Option<Option<String>> = tx
            .query_row("SELECT parent_id FROM folders WHERE id = ?1", [remote_id], |r| r.get(0))
            .optional()?;
        let Some(parent) = parent else { return Ok(()) };
        tx.execute("UPDATE folders SET parent_id = ?1 WHERE parent_id = ?2", params![parent, remote_id])?;
        tx.execute("UPDATE notes SET folder_id = ?1 WHERE folder_id = ?2", params![parent, remote_id])?;
        tx.execute("DELETE FROM folders WHERE id = ?1", [remote_id])?;
        Self::bump(&tx)?;
        tx.commit()?;
        Ok(())
    }

    fn save_note(&mut self, remote_id: Option<&str>, base_revision: Option<&str>, note: &NoteData) -> Result<(String, String)> {
        let tx = self.conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let id = remote_id.unwrap_or(&note.id).to_owned();
        let current: Option<i64> =
            tx.query_row("SELECT revision FROM notes WHERE id = ?1", [&id], |r| r.get(0)).optional()?;
        let revision = match (remote_id, current) {
            // A create retried after a lost reply, or an update of a note
            // that changed since: let the engine reconcile.
            (None, Some(_)) => return Err(Conflict.into()),
            (Some(_), Some(cur)) if base_revision != Some(cur.to_string().as_str()) => return Err(Conflict.into()),
            (_, Some(cur)) => cur + 1,
            // Updating a note deleted meanwhile puts it back.
            (_, None) => 1,
        };
        let folder = Self::ensure_folder(&tx, &note.folder)?;
        let title = crate::markdown::title_of(&note.body);
        tx.execute(
            "INSERT INTO notes (id, folder_id, title, body, created, modified, pinned, deleted, revision)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
             ON CONFLICT(id) DO UPDATE SET folder_id = ?2, title = ?3, body = ?4, created = ?5, modified = ?6,
                 pinned = ?7, deleted = ?8, revision = ?9",
            params![id, folder, title, note.body, ts(note.created), ts(note.modified), note.pinned, note.deleted.map(ts), revision],
        )?;
        tx.execute("DELETE FROM tags WHERE note_id = ?1", [&id])?;
        let mut tags = Vec::new();
        for line in note.body.lines() {
            tags.extend(crate::doc::tags_in(line));
        }
        for tag in tags {
            tx.execute("INSERT OR IGNORE INTO tags (note_id, tag) VALUES (?1, ?2)", params![id, tag])?;
        }
        Self::bump(&tx)?;
        tx.commit()?;
        Ok((id, revision.to_string()))
    }

    fn delete_note(&mut self, remote_id: &str) -> Result<()> {
        let tx = self.conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        if tx.execute("DELETE FROM notes WHERE id = ?1", [remote_id])? > 0 {
            Self::bump(&tx)?;
        }
        tx.commit()?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn notes_and_folders_are_tables() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("notes.db");
        let mut b = SqliteBackend::open(&Config { path: path.clone() }).unwrap();
        let c0 = b.cursor().unwrap();
        let (rid, rev) = b.save_note(None, None, &note("n1", &["Work", "2026"], "# Plan #todo\nbody")).unwrap();
        assert_eq!((rid.as_str(), rev.as_str()), ("n1", "1"));
        assert_ne!(b.cursor().unwrap(), c0);

        // Readable from plain SQL.
        let conn = Connection::open(&path).unwrap();
        let (title, folder): (String, String) = conn
            .query_row("SELECT n.title, f.name FROM notes n JOIN folders f ON f.id = n.folder_id", [], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .unwrap();
        assert_eq!((title.as_str(), folder.as_str()), ("Plan #todo", "2026"));
        let tag: String = conn.query_row("SELECT tag FROM tags", [], |r| r.get(0)).unwrap();
        assert_eq!(tag, "todo");

        let snap = b.pull().unwrap();
        assert_eq!(snap.notes[0].data.folder, vec!["Work", "2026"]);
        assert_eq!(snap.folders.len(), 2);
    }

    #[test]
    fn outdated_updates_are_conflicts() {
        let dir = tempfile::tempdir().unwrap();
        let mut b = SqliteBackend::open(&Config { path: dir.path().join("n.db") }).unwrap();
        let (rid, rev) = b.save_note(None, None, &note("n1", &["Notes"], "a")).unwrap();
        let (_, rev2) = b.save_note(Some(&rid), Some(&rev), &note("n1", &["Notes"], "b")).unwrap();
        let err = b.save_note(Some(&rid), Some(&rev), &note("n1", &["Notes"], "c")).unwrap_err();
        assert!(err.is::<Conflict>());
        // A retried create is a conflict too (the engine matches it up).
        assert!(b.save_note(None, None, &note("n1", &["Notes"], "d")).unwrap_err().is::<Conflict>());
        b.save_note(Some(&rid), Some(&rev2), &note("n1", &["Notes"], "c")).unwrap();
    }

    #[test]
    fn folders_rename_and_delete() {
        let dir = tempfile::tempdir().unwrap();
        let mut b = SqliteBackend::open(&Config { path: dir.path().join("n.db") }).unwrap();
        let p = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        let work = b.create_folder(&p(&["Work"])).unwrap();
        assert_eq!(b.create_folder(&p(&["Work"])).unwrap(), work, "creating an existing folder finds it");
        b.create_folder(&p(&["Work", "Old"])).unwrap();
        b.rename_folder(&work, &p(&["Work"]), &p(&["Projects"])).unwrap();
        let mut paths: Vec<_> = b.pull().unwrap().folders.into_iter().map(|f| f.path).collect();
        paths.sort();
        assert_eq!(paths, vec![p(&["Projects"]), p(&["Projects", "Old"])]);
        b.delete_folder(&work, &p(&["Projects"])).unwrap();
        b.delete_folder(&work, &p(&["Projects"])).unwrap();
        let paths: Vec<_> = b.pull().unwrap().folders.into_iter().map(|f| f.path).collect();
        assert_eq!(paths, vec![p(&["Old"])]);
    }
}
