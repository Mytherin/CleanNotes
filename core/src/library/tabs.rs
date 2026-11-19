//! CleanTabs' saved tabs. CleanTabs (a browser extension) keeps the tabs
//! it saves as notes in the `tabs/` directory of a CleanNotes S3 account's
//! folder, laid out as a folder of its own (see
//! [`cleanbucket::notes::tabs`]). Here that directory is an account of its
//! own, linked to the S3 account ([`Account::tabs_of`]): synced as any S3
//! account, with that account's access key and encryption key, but never
//! shown as an account. Its notes are only in CleanTabs' section (All
//! Tabs, its folders, its Recently Deleted), not in All Notes, search,
//! tags or counts ([`Library::account_enabled`] is false for it).
//!
//! The desktop and the iPhone app turn the looking on ([`Library::find_tabs`]).

use std::sync::mpsc::{channel, Receiver, TryRecvError};

use super::*;
use crate::sync::{s3, BackendConfig};

/// How often S3 accounts are looked in for saved tabs (and when they
/// change).
const FIND_EVERY: Duration = Duration::from_secs(15 * 60);

/// What each S3 account looked in said: whether it has saved tabs, or why
/// it couldn't be looked in.
type Found = Vec<(String, Result<bool, String>)>;

/// The last look for saved tabs.
pub(super) struct Search {
    /// Its answer, while it runs.
    rx: Option<Receiver<Found>>,
    started: Instant,
    /// The accounts it looked in.
    with: String,
}

impl Library {
    /// Whether an account is CleanTabs' saved tabs of another.
    pub fn is_tabs_account(&self, id: &str) -> bool {
        self.store.account(id).is_some_and(|a| a.is_tabs())
    }

    /// The account whose sign-in an account uses: a tabs account's S3
    /// account, else itself.
    pub(super) fn secret_owner(&self, id: &str) -> String {
        self.store.account(id).and_then(|a| a.tabs_of.clone()).unwrap_or_else(|| id.to_owned())
    }

    /// Whether a tabs account's section is shown: it's on (its directory
    /// was found) and so is its S3 account.
    pub fn tabs_shown(&self, id: &str) -> bool {
        self.store.account(id).is_some_and(|a| a.enabled && a.tabs_of.as_deref().is_some_and(|p| self.account_enabled(p)))
    }

    /// The tabs accounts shown, in their S3 accounts' order: each one's id
    /// and its S3 account's name.
    pub fn tabs_accounts(&self) -> Vec<(String, String)> {
        let mut out = Vec::new();
        for parent in self.store.accounts.iter().filter(|a| !a.is_tabs()) {
            let id = model::tabs_account_id(&parent.id);
            if self.tabs_shown(&id) {
                out.push((id, parent.name.clone()));
            }
        }
        out
    }

    /// An S3 account was looked in for saved tabs: its tabs account is
    /// added (and synced) or switched on if they're there, switched off if
    /// not. Returns whether anything changed.
    pub fn set_tabs_found(&mut self, parent: &str, found: bool) -> bool {
        let id = model::tabs_account_id(parent);
        let config = match self.store.account(parent).filter(|a| !a.is_tabs()).map(|a| &a.backend) {
            Some(BackendConfig::S3(c)) if found => Some(s3::tabs_config(c)),
            _ => None,
        };
        match (self.store.account_mut(&id), config) {
            (Some(a), Some(c)) => {
                let backend = BackendConfig::S3(c);
                if a.enabled && a.backend == backend {
                    return false;
                }
                if a.backend != backend {
                    a.cursor = None;
                }
                a.enabled = true;
                a.backend = backend;
            }
            (Some(a), None) => {
                if !a.enabled {
                    return false;
                }
                a.enabled = false;
            }
            (None, Some(c)) => self.store.accounts.push(Account {
                id: id.clone(),
                name: "CleanTabs".into(),
                backend: BackendConfig::S3(c),
                enabled: true,
                cursor: None,
                last_sync: None,
                tabs_of: Some(parent.to_owned()),
            }),
            (None, None) => return false,
        }
        self.sync_started.remove(&id);
        self.sync_errors.remove(&id);
        self.save(true);
        true
    }

    /// Looks for saved tabs in the S3 accounts now and then (in the
    /// background), and takes in what was found. Returns whether accounts
    /// changed.
    pub(super) fn find_tabs(&mut self) -> bool {
        if !self.find_tabs {
            return false;
        }
        let mut changed = false;
        if let Some(rx) = self.tabs_search.as_ref().and_then(|s| s.rx.as_ref()) {
            let found = match rx.try_recv() {
                Ok(found) => found,
                Err(TryRecvError::Empty) => return false,
                Err(TryRecvError::Disconnected) => Vec::new(),
            };
            if let Some(s) = &mut self.tabs_search {
                s.rx = None;
            }
            for (account, r) in found {
                match r {
                    Ok(f) => changed |= self.set_tabs_found(&account, f),
                    // (Left as it was: offline, say.)
                    Err(e) => eprintln!("looking for CleanTabs' saved tabs in {account}: {e}"),
                }
            }
        }
        let candidates: Vec<(String, s3::Config)> = self
            .store
            .accounts
            .iter()
            .filter(|a| a.enabled && !a.is_tabs() && !self.needs_sign_in.contains(&a.id))
            .filter_map(|a| match &a.backend {
                BackendConfig::S3(c) => Some((a.id.clone(), c.clone())),
                _ => None,
            })
            .collect();
        let with = format!("{candidates:?}");
        let due = match &self.tabs_search {
            None => true,
            Some(s) => s.rx.is_none() && (s.with != with || s.started.elapsed() > FIND_EVERY),
        };
        if !due {
            return changed;
        }
        // Accounts switched off, or no longer S3: their tabs go too.
        let gone: Vec<String> = self
            .store
            .accounts
            .iter()
            .filter_map(|a| a.tabs_of.clone())
            .filter(|p| !candidates.iter().any(|(c, _)| c == p) && !self.needs_sign_in.contains(p))
            .collect();
        for p in gone {
            changed |= self.set_tabs_found(&p, false);
        }
        let jobs: Vec<(String, s3::Config, String)> =
            candidates.into_iter().filter_map(|(id, c)| Some((id.clone(), c, self.db.secret(&id)?))).collect();
        let (tx, rx) = channel();
        let any = !jobs.is_empty();
        if any {
            std::thread::spawn(move || {
                let found: Found = jobs.into_iter().map(|(id, c, secret)| (id, s3::tabs_exist(&c, &secret).map_err(|e| format!("{e:#}")))).collect();
                let _ = tx.send(found);
            });
        }
        self.tabs_search = Some(Search { rx: any.then_some(rx), started: Instant::now(), with });
        changed
    }

    /// CleanTabs' list (see [`Library::list_sections`]): All Tabs or a
    /// folder's tabs (with those in its folders), a section per folder (the
    /// folder itself first, then those in it, named by their path below
    /// it), each in the order its tabs were saved; or its Recently Deleted,
    /// last deleted first. A search (every word must match the page's
    /// title, address or what's written about it) looks through the same.
    pub(super) fn tab_sections(&self, loc: &Location, words: &[String]) -> Vec<(String, Vec<String>)> {
        let (account, folder, deleted) = match loc {
            Location::Tabs(a, f) => (a, f.as_deref(), false),
            Location::TabsDeleted(a) => (a, None, true),
            _ => return Vec::new(),
        };
        if !self.tabs_shown(account) {
            return Vec::new();
        }
        let inside: Option<HashSet<String>> = folder.map(|f| self.store.folder_and_descendants(f).into_iter().collect());
        let depth = folder.map_or(0, |f| self.store.folder_path(f).len());
        let mut notes: Vec<(Vec<String>, &Note)> = self
            .store
            .notes
            .iter()
            .filter(|n| &n.account == account && n.deleted.is_some() == deleted)
            .filter(|n| inside.as_ref().is_none_or(|i| i.contains(&n.folder)))
            .filter(|n| {
                let body = n.body.to_lowercase();
                words.iter().all(|w| body.contains(w.as_str()))
            })
            .map(|n| (self.store.folder_path(&n.folder).get(depth..).unwrap_or_default().to_vec(), n))
            .collect();
        if deleted {
            notes.sort_by(|(_, a), (_, b)| b.deleted.cmp(&a.deleted));
            let ids: Vec<String> = notes.iter().map(|(_, n)| n.id.clone()).collect();
            return if ids.is_empty() { Vec::new() } else { vec![(String::new(), ids)] };
        }
        notes.sort_by(|(fa, a), (fb, b)| fa.cmp(fb).then_with(|| a.created.cmp(&b.created)).then_with(|| a.id.cmp(&b.id)));
        let mut sections: Vec<(String, Vec<String>)> = Vec::new();
        for (f, n) in notes {
            let name = f.join(" / ");
            match sections.last_mut() {
                Some((s, ids)) if *s == name => ids.push(n.id.clone()),
                _ => sections.push((name, vec![n.id.clone()])),
            }
        }
        sections
    }

    /// A tabs account's folders, as the sidebar shows them: (id, name,
    /// depth). A default "Notes" folder (CleanTabs makes none) only if
    /// something's in it.
    pub fn tab_folders(&self, account: &str) -> Vec<(String, String, usize)> {
        self.store
            .folder_tree(account)
            .into_iter()
            .filter(|(f, _)| {
                !f.default
                    || self.store.notes.iter().any(|n| n.folder == f.id && n.deleted.is_none())
                    || self.store.folders.iter().any(|c| c.parent.as_deref() == Some(f.id.as_str()))
            })
            .map(|(f, d)| (f.id.clone(), f.name.clone(), d))
            .collect()
    }

    /// A tabs account's counts, in one pass over the notes: those in each
    /// folder (with its folders'), in all, and in its Recently Deleted.
    pub fn tab_counts(&self, account: &str) -> (HashMap<String, usize>, usize, usize) {
        let mut direct: HashMap<&str, usize> = HashMap::new();
        let (mut all, mut deleted) = (0, 0);
        for n in self.store.notes.iter().filter(|n| n.account == account) {
            if n.deleted.is_some() {
                deleted += 1;
            } else {
                all += 1;
                *direct.entry(n.folder.as_str()).or_default() += 1;
            }
        }
        let counts = self
            .store
            .folders
            .iter()
            .filter(|f| f.account == account)
            .map(|f| {
                let n = self.store.folder_and_descendants(&f.id).iter().map(|d| direct.get(d.as_str()).copied().unwrap_or(0)).sum();
                (f.id.clone(), n)
            })
            .collect();
        (counts, all, deleted)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::tabs::TabNote;

    const S3: &str = "work";

    /// A library with an S3 account (encrypted) with a note in it, and
    /// its saved tabs found.
    fn lib() -> (Library, tempfile::TempDir, String) {
        let dir = tempfile::tempdir().unwrap();
        let mut lib = Library::for_test(dir.path());
        let config = s3::Config { bucket: "b".into(), region: "eu-west-1".into(), prefix: "CleanNotes".into(), endpoint: None, encrypted: true };
        lib.store.accounts.push(Account {
            id: S3.into(),
            name: "Work".into(),
            backend: BackendConfig::S3(config),
            enabled: true,
            cursor: None,
            last_sync: None,
            tabs_of: None,
        });
        lib.store.ensure_local();
        let home = lib.default_folder_id(S3);
        add(&mut lib, S3, &home, "x", "# Plan\n#work\n");
        assert!(lib.set_tabs_found(S3, true));
        (lib, dir, model::tabs_account_id(S3))
    }

    fn add(lib: &mut Library, account: &str, folder: &str, id: &str, body: &str) -> String {
        let mut n = Note::new(account, folder, body);
        if !id.is_empty() {
            n.id = id.into();
        }
        let id = n.id.clone();
        lib.store.notes.push(n);
        id
    }

    fn tab(lib: &mut Library, account: &str, path: &[&str], n: u32, title: &str) -> String {
        let folder = lib.store.ensure_folder_at(account, &path.iter().map(|s| s.to_string()).collect::<Vec<_>>());
        let body = TabNote { title: title.into(), url: format!("https://example.com/{n}"), thumbnail: None, text: "rust".into() }.to_markdown();
        let mut note = Note::new(account, &folder, &body);
        note.id = format!("tab-{n}");
        note.created += chrono::Duration::milliseconds(n as i64);
        let id = note.id.clone();
        lib.store.notes.push(note);
        id
    }

    #[test]
    fn saved_tabs_are_an_account_of_their_own_never_shown_as_one() {
        let (mut lib, _dir, tabs) = lib();
        let a = lib.store.account(&tabs).unwrap().clone();
        assert_eq!(a.tabs_of.as_deref(), Some(S3));
        // The tabs/ directory of the account's folder, encrypted as it is.
        let BackendConfig::S3(c) = &a.backend else { panic!("S3") };
        assert_eq!((c.prefix.as_str(), c.encrypted, c.bucket.as_str()), ("CleanNotes/tabs", true, "b"));
        // Its sign-in is the S3 account's.
        lib.db.set_secret(S3, Some("keys")).unwrap();
        assert_eq!(lib.db.secret(&lib.secret_owner(&tabs)).as_deref(), Some("keys"));
        assert!(lib.store.default_folder(&tabs).is_none(), "CleanTabs makes no Notes folder");

        let r1 = tab(&mut lib, &tabs, &["Research"], 1, "Rust book");
        let r2 = tab(&mut lib, &tabs, &["Research", "Docs"], 2, "Rust docs");
        let r0 = tab(&mut lib, &tabs, &["Research"], 0, "Rust blog");

        // Not in the notes: All Notes, search, tags, counts, accounts.
        assert!(!lib.account_enabled(&tabs));
        let notes = lib.visible_notes(&Location::All, "");
        assert!(!notes.contains(&r1) && notes.contains(&"x".to_owned()));
        assert!(lib.visible_notes(&Location::All, "rust").is_empty());
        assert_eq!(lib.all_count(), 1);
        assert_eq!(lib.all_tags(), ["work"]);
        assert!(lib.indexed_notes().iter().all(|n| n.id == "x"));
        assert!(!lib.can_remove_account(&tabs));
        assert!(!lib.can_lock(&r1));

        // In CleanTabs' section: by folder, in the order they were saved.
        assert_eq!(lib.tabs_accounts(), [(tabs.clone(), "Work".to_owned())]);
        let all = Location::Tabs(tabs.clone(), None);
        assert_eq!(
            lib.list_sections(&all, ""),
            [("Research".to_owned(), vec![r0.clone(), r1.clone()]), ("Research / Docs".to_owned(), vec![r2.clone()])]
        );
        let research = lib.store.folder_at(&tabs, &["Research".into()]).unwrap().id.clone();
        let docs = lib.store.folder_at(&tabs, &["Research".into(), "Docs".into()]).unwrap().id.clone();
        let in_research = Location::Tabs(tabs.clone(), Some(research.clone()));
        assert_eq!(lib.list_sections(&in_research, ""), [(String::new(), vec![r0.clone(), r1.clone()]), ("Docs".to_owned(), vec![r2.clone()])]);
        assert_eq!(lib.visible_notes(&all, "docs"), [r2.clone()], "searched by title");
        assert_eq!(lib.visible_notes(&all, "example.com/1"), [r1.clone()], "and address");
        assert_eq!(lib.location_title(&in_research, ""), "Research");
        assert_eq!(lib.tab_folders(&tabs), [(research.clone(), "Research".to_owned(), 0), (docs.clone(), "Docs".to_owned(), 1)]);
        let (counts, n, deleted) = lib.tab_counts(&tabs);
        assert_eq!((counts[&research], counts[&docs], n, deleted), (3, 1, 3, 0));

        // Deleted: to CleanTabs' Recently Deleted (not the notes').
        lib.trash_notes(&[r2.clone()], &all);
        assert_eq!(lib.visible_notes(&Location::TabsDeleted(tabs.clone()), ""), [r2.clone()]);
        assert!(lib.visible_notes(&Location::Deleted(S3.into()), "").is_empty());
        assert_eq!(lib.tab_counts(&tabs).2, 1);

        // Tabs don't move into notes, nor notes into CleanTabs.
        let home = lib.default_folder_id(S3);
        lib.move_notes(&[r1.clone()], &home);
        lib.move_notes(&["x".to_owned()], &research);
        assert_eq!(lib.store.note(&r1).unwrap().account, tabs);
        assert_eq!(lib.store.note("x").unwrap().folder, home);

        // Gone from the bucket (or the account switched off): hidden.
        assert!(lib.set_tabs_found(S3, false));
        assert!(lib.tabs_accounts().is_empty());
        assert!(!lib.location_exists(&all));
        assert!(lib.set_tabs_found(S3, true));
        assert!(!lib.set_tabs_found(S3, true), "found again: nothing new");
        // Removing the S3 account takes its tabs along.
        lib.remove_account(S3);
        assert!(lib.store.account(&tabs).is_none());
        assert!(lib.store.notes.iter().all(|n| n.account == LOCAL));
    }

    #[test]
    fn only_s3_accounts_have_saved_tabs() {
        let (mut lib, _dir, _) = lib();
        assert!(!lib.set_tabs_found(LOCAL, true));
        assert!(lib.store.account(&model::tabs_account_id(LOCAL)).is_none());
    }

    #[test]
    fn folders_of_the_users_named_like_cleantabs_stay_notes() {
        let (mut lib, _dir, _) = lib();
        for name in ["CleanTabs", "Tabs", "tabs"] {
            let f = lib.store.ensure_folder_at(S3, &[name.to_owned()]);
            let n = add(&mut lib, S3, &f, "", &format!("# In {name}\n"));
            assert_eq!(lib.visible_notes(&Location::Folder(f.clone()), ""), [n.clone()]);
            assert!(lib.visible_notes(&Location::All, "").contains(&n));
            assert!(lib.store.folder_tree(S3).iter().any(|(t, _)| t.id == f));
            assert_eq!(lib.folder_count(&f), 1);
        }
    }
}
