//! Importing notes from Evernote export files (`.enex`, see
//! [`crate::enex`]): from Settings → General, or by dropping the files on
//! the window.
//!
//! Notes go into the account chosen, each into the folder named by the
//! export (made if it doesn't exist yet), keeping their dates. Attachments
//! are saved in the data folder's "Attachments" folder and linked from the
//! notes. A note already imported (same account, text and creation time) is
//! skipped, so importing a file again only adds what's new.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use eframe::egui::{self, Key, RichText};

use super::settings_window::{combo, sheet_buttons};
use super::{modal, App, Location};
use crate::enex::{self, Importer, Resource, Saved};
use cleannotes_core::attachments;
use crate::model::Note;
use crate::theme::{self, Palette};

/// The Import Notes sheet.
#[derive(Default)]
pub struct ImportSheet {
    /// `.enex` files or folders of them, separated by ";".
    pub paths: String,
    pub account: String,
    pub error: Option<String>,
}

#[derive(Debug, Default, PartialEq)]
pub struct ImportReport {
    pub notes: usize,
    /// Already imported before.
    pub skipped: usize,
    pub folders_created: usize,
    pub attachments: usize,
    /// The folder of the first note imported.
    pub first_folder: Option<String>,
}

/// The export files at `paths`: files as they are, folders' `.enex` files.
fn enex_files(paths: &[PathBuf]) -> Result<Vec<PathBuf>> {
    let mut out = Vec::new();
    for p in paths {
        if p.is_dir() {
            let mut found: Vec<PathBuf> = std::fs::read_dir(p)
                .with_context(|| format!("reading {}", p.display()))?
                .filter_map(|e| e.ok().map(|e| e.path()))
                .filter(|f| f.is_file() && f.extension().is_some_and(|e| e.eq_ignore_ascii_case("enex")))
                .collect();
            if found.is_empty() {
                bail!("There are no .enex files in {}.", p.display());
            }
            found.sort();
            out.extend(found);
        } else if p.is_file() {
            out.push(p.clone());
        } else {
            bail!("{} doesn't exist.", p.display());
        }
    }
    if out.is_empty() {
        bail!("Choose a .enex file, or a folder of them.");
    }
    // Each file once, however it was given (a folder and a file in it, the
    // same file twice): else its notes would be added twice, as the check
    // for notes already imported only knows those from before.
    let mut seen = HashSet::new();
    out.retain(|f| seen.insert(std::fs::canonicalize(f).unwrap_or_else(|_| f.clone())));
    Ok(out)
}

/// Saves an attachment in `dir` as the note's own (synced with it): a
/// picture shown in it, another file (a PDF, …) linked by its name. A file
/// too large to attach is kept only here, as `<md5>.<extension>`, linked
/// by its URL.
fn save_attachment(dir: &Path, r: &Resource) -> Result<Saved> {
    if attachments::image_ext(&r.data).is_some() && r.data.len() <= attachments::MAX_BYTES {
        return Ok(Saved::Image(attachments::save(dir, &r.data)?));
    }
    if r.data.len() <= attachments::MAX_FILE_BYTES {
        let name = attachments::save_file(dir, &r.data, &r.name())?;
        return Ok(Saved::File(format!("{}{name}", attachments::URL_PREFIX)));
    }
    std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    let path = dir.join(format!("{}.{}", r.hash, r.extension()));
    if !path.exists() {
        // Written under another name first, so a half-written file never
        // passes for a saved one.
        let part = path.with_extension("part");
        std::fs::write(&part, &r.data).with_context(|| format!("writing {}", path.display()))?;
        std::fs::rename(&part, &path).with_context(|| format!("writing {}", path.display()))?;
    }
    Ok(Saved::File(enex::file_url(&path)))
}

pub fn split_paths(s: &str) -> Vec<PathBuf> {
    s.split(';').map(|p| p.trim().trim_matches('"').trim()).filter(|p| !p.is_empty()).map(PathBuf::from).collect()
}

impl App {
    /// Opens the Import Notes sheet, with these files in it.
    pub fn open_import(&mut self, paths: &[PathBuf]) {
        let joined = paths.iter().map(|p| p.display().to_string()).collect::<Vec<_>>().join("; ");
        let account = self.new_note_target().0;
        self.import = Some(ImportSheet { paths: joined, account, error: None });
    }

    /// Imports the notes of Evernote export files (or folders of them)
    /// into an account.
    pub fn import_enex(&mut self, paths: &[PathBuf], account: &str) -> Result<ImportReport> {
        if self.lib.store.account(account).is_none() {
            bail!("That account no longer exists.");
        }
        let files = enex_files(paths)?;
        let attachments_dir = self.lib.attachments_dir();
        let mut save = |r: &Resource| save_attachment(&attachments_dir, r);
        // Every file is read before any note is added: a file that can't be
        // read adds nothing.
        let mut notes = Vec::new();
        for f in &files {
            let bytes = std::fs::read(f).with_context(|| format!("reading {}", f.display()))?;
            let xml = String::from_utf8_lossy(&bytes);
            let notebook = f.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
            let read = Importer { save: &mut save }.read(&xml, &notebook).with_context(|| format!("{} can't be imported", f.display()))?;
            notes.extend(read);
        }

        self.flush_editor();
        let existing: HashSet<(i64, &str)> =
            self.lib.store.notes.iter().filter(|n| n.account == account).map(|n| (n.created.timestamp(), n.body.as_str())).collect();
        let fresh: Vec<_> = notes.iter().filter(|n| !existing.contains(&(n.created.timestamp(), n.body.as_str()))).cloned().collect();
        let mut report = ImportReport { skipped: notes.len() - fresh.len(), ..ImportReport::default() };
        let folders_before = self.lib.store.folders.len();
        for n in fresh {
            let folder = self.lib.store.ensure_folder_at(account, &[n.folder.clone()]);
            report.first_folder.get_or_insert(folder.clone());
            let mut note = Note::new(account, &folder, &n.body);
            note.created = n.created;
            note.modified = n.modified;
            report.notes += 1;
            report.attachments += n.attachments;
            self.lib.mark_changed(&note.id);
            self.lib.store.notes.push(note);
        }
        report.folders_created = self.lib.store.folders.len() - folders_before;
        self.lib.save(true);
        Ok(report)
    }

    pub(super) fn import_sheet(&mut self, ctx: &egui::Context, p: &Palette) {
        let Some(sheet) = self.import.as_mut() else { return };
        let accounts: Vec<(String, String)> = self.lib.store.accounts.iter().filter(|a| a.enabled).map(|a| (a.id.clone(), a.name.clone())).collect();
        let mut close = false;
        let mut go = false;
        modal(ctx, "import", p, |ui| {
            ui.set_width(420.0);
            ui.label(RichText::new("Import from Evernote").font(theme::bold_font(15.0)).color(p.text));
            ui.add_space(4.0);
            ui.add(
                egui::Label::new(
                    RichText::new(
                        "Notes exported from Evernote (or from Apple Notes as Evernote files) are added to an account, each in its notebook's folder, keeping their dates. Images and other attachments are saved beside your notes and linked from them.",
                    )
                    .size(12.0)
                    .color(p.secondary),
                )
                .wrap(),
            );
            ui.add_space(10.0);
            ui.label(RichText::new("Files (.enex) or a folder of them:").size(12.5).color(p.text));
            ui.add(egui::TextEdit::singleline(&mut sheet.paths).hint_text("C:\\Users\\…\\Notes.enex").desired_width(f32::INFINITY));
            ui.label(RichText::new("Separate several with \";\", or drop the files on the window.").size(11.5).color(p.secondary));
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                ui.label(RichText::new("Into:").size(12.5).color(p.text));
                combo(ui, "import-account", &mut sheet.account, &accounts);
            });
            if sheet_buttons(ui, p, false, &sheet.error, !split_paths(&sheet.paths).is_empty(), "Import", &mut close) {
                go = true;
            }
            if ui.input(|i| i.key_pressed(Key::Escape)) {
                close = true;
            }
        });
        if close {
            self.import = None;
            return;
        }
        if !go {
            return;
        }
        let paths = split_paths(&sheet.paths);
        let account = sheet.account.clone();
        match self.import_enex(&paths, &account) {
            Ok(r) => {
                self.import = None;
                let mut msg = match r.notes {
                    0 => "No new notes to import".to_owned(),
                    1 => "Imported 1 note".to_owned(),
                    n => format!("Imported {n} notes"),
                };
                if r.folders_created > 0 {
                    msg += &format!(" into {} new folder{}", r.folders_created, if r.folders_created == 1 { "" } else { "s" });
                }
                if r.attachments > 0 {
                    msg += &format!(", with {} attachment{}", r.attachments, if r.attachments == 1 { "" } else { "s" });
                }
                if r.skipped > 0 {
                    msg += &format!(" ({} already imported)", r.skipped);
                }
                msg.push('.');
                self.push_notice(msg);
                if let Some(f) = r.first_folder {
                    self.go_to(Location::Folder(f));
                }
            }
            Err(e) => {
                if let Some(s) = self.import.as_mut() {
                    s.error = Some(format!("{e:#}"));
                }
            }
        }
    }

    /// `.enex` files (and folders) dropped on the window open the Import
    /// sheet; other files (pictures, PDFs, …) are attached to the open note.
    pub(super) fn dropped_files(&mut self, ctx: &egui::Context) {
        let dropped: Vec<PathBuf> = ctx.input(|i| i.raw.dropped_files.iter().filter_map(|f| f.path.clone()).collect());
        let is_enex = |p: &PathBuf| p.is_dir() || p.extension().is_some_and(|e| e.eq_ignore_ascii_case("enex"));
        let (enex, rest): (Vec<PathBuf>, Vec<PathBuf>) = dropped.into_iter().partition(is_enex);
        let files: Vec<PathBuf> = rest.into_iter().filter(|p| p.is_file()).collect();
        if !files.is_empty() {
            self.attach_files(&files);
        }
        if !enex.is_empty() {
            self.settings_ui.open = false;
            self.open_import(&enex);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn export(notes: &str) -> String {
        format!("<?xml version=\"1.0\"?><en-export>{notes}</en-export>")
    }

    fn note(title: &str, text: &str, tag: &str, created: &str) -> String {
        format!(
            "<note><title>{title}</title><content><![CDATA[<en-note><div>{text}</div></en-note>]]></content><created>{created}</created><updated>{created}</updated><tag>{tag}</tag></note>"
        )
    }

    #[test]
    fn imports_into_folders_once() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = App::for_test(&dir.path().join("data"));
        let files = dir.path().join("enex");
        std::fs::create_dir_all(&files).unwrap();
        let png = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, b"png bytes");
        let pic = format!(
            "<note><title>Pic</title><content><![CDATA[<en-note><div>x</div></en-note>]]></content><created>20200101T000000Z</created><tag>Cats</tag><resource><data>{png}</data><mime>image/png</mime></resource></note>"
        );
        std::fs::write(
            files.join("a.enex"),
            export(&(note("One", "first", "Cats", "20190101T101010Z") + &note("Two", "second", "Notes", "20190202T101010Z") + &pic)),
        )
        .unwrap();
        std::fs::write(files.join("ignored.txt"), "not an export").unwrap();

        let r = app.import_enex(&[files.clone()], crate::model::LOCAL).unwrap();
        assert_eq!((r.notes, r.skipped, r.folders_created, r.attachments), (3, 0, 1, 1));
        let cats = app.lib.store.folder_at(crate::model::LOCAL, &["Cats".into()]).unwrap().id.clone();
        let one = app.lib.store.notes.iter().find(|n| n.title() == "One").unwrap();
        assert_eq!(one.folder, cats);
        assert_eq!(one.body, "# One\nfirst\n");
        assert_eq!(one.created.to_rfc3339(), "2019-01-01T10:10:10+00:00");
        // "Notes" is the account's own folder.
        let two = app.lib.store.notes.iter().find(|n| n.title() == "Two").unwrap();
        assert!(app.lib.store.folder(&two.folder).unwrap().default);
        // The attachment (not a picture CleanNotes can show) is saved as
        // the note's own, and linked by its name.
        let pic = app.lib.store.notes.iter().find(|n| n.title() == "Pic").unwrap();
        let saved: Vec<_> = std::fs::read_dir(dir.path().join("data").join("Attachments")).unwrap().map(|e| e.unwrap().path()).collect();
        assert_eq!(saved.len(), 1);
        assert_eq!(std::fs::read(&saved[0]).unwrap(), b"png bytes");
        let name = attachments::name_for(b"png bytes", "png");
        assert!(pic.body.contains(&format!("[Attachment.png](attachments/{name})")), "{}", pic.body);
        assert_eq!(attachments::refs_in_body(&pic.body), vec![name]);

        // Again: nothing new.
        let r = app.import_enex(&[files.join("a.enex")], crate::model::LOCAL).unwrap();
        assert_eq!((r.notes, r.skipped), (0, 3));
        assert_eq!(app.lib.store.notes.len(), 3);
        // Saved to the database.
        drop(app);
        let app = App::for_test(&dir.path().join("data"));
        assert_eq!(app.lib.store.notes.len(), 3);
    }

    #[test]
    fn a_file_given_twice_is_read_once() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = App::for_test(&dir.path().join("data"));
        let files = dir.path().join("enex");
        std::fs::create_dir_all(&files).unwrap();
        std::fs::write(files.join("a.enex"), export(&note("One", "first", "Cats", "20190101T101010Z"))).unwrap();
        // The folder, and the file in it (as dropped together, or typed).
        let r = app.import_enex(&[files.clone(), files.join("a.enex"), files.join(".").join("a.enex")], crate::model::LOCAL).unwrap();
        assert_eq!((r.notes, r.skipped), (1, 0));
        assert_eq!(app.lib.store.notes.iter().filter(|n| n.title() == "One").count(), 1);
    }

    #[test]
    fn a_bad_file_imports_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = App::for_test(&dir.path().join("data"));
        let good = dir.path().join("good.enex");
        let bad = dir.path().join("bad.enex");
        std::fs::write(&good, export(&note("One", "first", "Cats", "20190101T101010Z"))).unwrap();
        std::fs::write(&bad, "<html>nope</html>").unwrap();
        let e = app.import_enex(&[good, bad], crate::model::LOCAL).unwrap_err();
        assert!(format!("{e:#}").contains("not an Evernote export"), "{e:#}");
        assert!(app.lib.store.notes.is_empty());
        assert!(app.import_enex(&[dir.path().join("missing.enex")], crate::model::LOCAL).is_err());
    }

    #[test]
    fn paths_split_on_semicolons() {
        assert_eq!(split_paths(" \"C:\\a b.enex\" ; d:\\x ;; "), vec![PathBuf::from("C:\\a b.enex"), PathBuf::from("d:\\x")]);
    }
}
