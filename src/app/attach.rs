//! Attaching pictures and other files to the open note: "Attach Image…"
//! and "Attach File…" (file dialogs), files dropped on the window, and
//! pasting a picture or files.
//!
//! Each one is saved in the attachments folder (see
//! [`cleannotes_core::attachments`]) and put at the caret, on a line of its
//! own: a picture shown, another file (a PDF, …) as a link with its name,
//! which opens it. A locked note's attachments are encrypted, each with a
//! key of its own kept in the note's sealed text. Deleted notes can't be
//! changed.

use std::path::{Path, PathBuf};

use super::App;
use crate::editor::images;
use crate::editor::Command;

/// The file types the dialog offers (HEIC is kept and synced, but shown as
/// a box: it can't be decoded here).
pub const EXTENSIONS: [&str; 8] = ["png", "jpg", "jpeg", "gif", "webp", "bmp", "heic", "heif"];

/// A file's name, for the note and messages.
fn file_label(p: &Path) -> String {
    p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| p.display().to_string())
}

impl App {
    /// Whether attachments can be added to the open note now.
    pub fn can_attach(&self) -> bool {
        let Some(e) = &self.editor else { return false };
        self.lib.store.note(&e.note_id).is_some_and(|n| n.deleted.is_none())
    }

    /// Why attachments can't be added to the open note, for a notice.
    fn why_not_attach(&self) -> Option<&'static str> {
        let e = self.editor.as_ref()?;
        if self.lib.store.note(&e.note_id).is_some_and(|n| n.deleted.is_some()) {
            Some("Deleted notes can't be edited. To edit this note, recover it first.")
        } else {
            None
        }
    }

    /// Whether attachments can be added; if not, says why.
    fn check_attach(&mut self) -> bool {
        if self.can_attach() {
            return true;
        }
        if let Some(why) = self.why_not_attach() {
            self.push_notice(why.into());
        }
        false
    }

    /// Puts pictures (their bytes, with a name for messages) at the caret.
    pub fn attach_images(&mut self, pictures: Vec<(String, Vec<u8>)>) {
        if !self.check_attach() {
            return;
        }
        let Some(id) = self.editor.as_ref().map(|e| e.note_id.clone()) else { return };
        for (label, data) in pictures {
            match self.lib.add_image_to(&id, &data) {
                Ok(name) => self.editor_command(Command::InsertImage(name)),
                Err(e) => self.push_notice(format!("{label} can't be attached: {e:#}.")),
            }
        }
    }

    /// Puts files of any kind (their names and bytes) at the caret, as
    /// links to them.
    pub fn attach_other_files(&mut self, files: Vec<(String, Vec<u8>)>) {
        if !self.check_attach() {
            return;
        }
        let Some(id) = self.editor.as_ref().map(|e| e.note_id.clone()) else { return };
        for (label, data) in files {
            match self.lib.add_file_to(&id, &data, &label) {
                Ok(reference) => self.editor_command(Command::InsertFile { reference, label }),
                Err(e) => self.push_notice(format!("{label} can't be attached: {e:#}.")),
            }
        }
    }

    /// Attaches files: pictures are shown in the note, other files linked.
    pub fn attach_files(&mut self, paths: &[PathBuf]) {
        if !self.check_attach() {
            return;
        }
        for p in paths {
            let label = file_label(p);
            let data = match std::fs::read(p) {
                Ok(data) => data,
                Err(e) => {
                    self.push_notice(format!("{label} can't be read: {e}."));
                    continue;
                }
            };
            // A picture CleanNotes can show (judged by its bytes) is shown.
            let picture = cleannotes_core::attachments::image_ext(&data).is_some() && data.len() <= cleannotes_core::attachments::MAX_BYTES;
            if picture {
                self.attach_images(vec![(label, data)]);
            } else {
                self.attach_other_files(vec![(label, data)]);
            }
        }
    }

    /// "Attach Image…": picks pictures with the system's dialog.
    pub fn choose_images(&mut self) {
        if !self.check_attach() {
            return;
        }
        let picked = rfd::FileDialog::new().set_title("Attach Image").add_filter("Images", &EXTENSIONS).pick_files();
        if let Some(paths) = picked {
            self.attach_files(&paths);
        }
    }

    /// "Attach File…": picks files of any kind with the system's dialog.
    pub fn choose_files(&mut self) {
        if !self.check_attach() {
            return;
        }
        let picked = rfd::FileDialog::new().set_title("Attach File").pick_files();
        if let Some(paths) = picked {
            self.attach_files(&paths);
        }
    }

    /// Opens an attached file (clicked in the note) in the system's app
    /// for it, from a copy named as it's shown.
    pub fn open_attached_file(&mut self, reference: &str, label: &str) {
        match self.lib.open_attachment(reference, label) {
            Ok(path) => {
                if let Err(e) = open::that(&path) {
                    self.push_notice(format!("{label} can't be opened: {e}."));
                }
            }
            Err(e) => self.push_notice(format!("{label} can't be opened: {e:#}.")),
        }
    }

    /// Copied pictures (their references) go on the system clipboard as
    /// files, to paste into File Explorer (or back into a note, as they
    /// are), and, one picture alone, as its pixels too, for apps that paste
    /// pictures. The files are copies, as for opening one (a locked note's
    /// decrypted, until notes are locked again).
    pub fn copy_images(&mut self, refs: &[String]) {
        let mut paths = Vec::new();
        for (i, r) in refs.iter().enumerate() {
            let label = if refs.len() == 1 { "Picture".to_owned() } else { format!("Picture {}", i + 1) };
            match self.lib.open_attachment(r, &label) {
                Ok(path) => paths.push(path),
                Err(e) => self.push_notice(format!("The picture can't be copied: {e:#}.")),
            }
        }
        if paths.is_empty() {
            return;
        }
        let Ok(mut cb) = arboard::Clipboard::new() else {
            self.push_notice("The clipboard can't be used now.".into());
            return;
        };
        let pixels = match &paths[..] {
            [one] => std::fs::read(one).ok().and_then(|d| images::rgba_of(&d)),
            _ => None,
        };
        // Setting the picture empties the clipboard; adding files doesn't
        // (on Windows), so without a picture it's emptied first: what was
        // copied before mustn't stay with them.
        let pictured = pixels.is_some_and(|(width, height, rgba)| {
            cb.set_image(arboard::ImageData { width, height, bytes: rgba.into() }).is_ok()
        });
        if !pictured {
            let _ = cb.clear();
        }
        if cb.set().file_list(&paths).is_err() && !pictured {
            self.push_notice("The picture can't be copied.".into());
        }
    }

    /// Paste with a picture (or files) on the clipboard and no text (text
    /// is pasted by the editor itself).
    pub fn paste_image(&mut self) {
        let Ok(mut cb) = arboard::Clipboard::new() else { return };
        if cb.get_text().is_ok_and(|t| !t.is_empty()) {
            return;
        }
        if let Ok(files) = cb.get().file_list() {
            let files: Vec<PathBuf> = files.into_iter().filter(|p| p.is_file()).collect();
            if !files.is_empty() {
                self.attach_files(&files);
                return;
            }
        }
        let Ok(img) = cb.get_image() else { return };
        match images::png_from_rgba(img.width, img.height, &img.bytes) {
            Some(png) => self.attach_images(vec![("The picture".into(), png)]),
            None => self.push_notice("The picture on the clipboard can't be pasted.".into()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn attached_pictures_go_into_the_note_and_the_folder() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = App::for_test(&dir.path().join("data"));
        app.new_note();
        let png = images::png_from_rgba(1, 1, &[0, 0, 0, 255]).unwrap();
        let file = dir.path().join("dot.PNG");
        std::fs::write(&file, &png).unwrap();
        app.attach_files(&[file]);
        let name = cleannotes_core::attachments::name_for(&png, "png");
        app.flush_editor();
        let id = app.selected.clone().unwrap();
        assert_eq!(app.lib.store.note(&id).unwrap().body, format!("![](attachments/{name})\n"));
        assert!(app.lib.attachment_path(&name).is_some());
    }

    /// Uses the real system clipboard (replacing what's on it), so it's run
    /// by hand: `cargo test copied_pictures -- --ignored`.
    #[test]
    #[ignore]
    fn copied_pictures_go_on_the_clipboard_as_files_and_pixels() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = App::for_test(&dir.path().join("data"));
        app.new_note();
        let png = images::png_from_rgba(2, 1, &[255, 0, 0, 255, 0, 0, 255, 255]).unwrap();
        let name = app.lib.add_image(&png).unwrap();
        app.copy_images(&[name]);
        let mut cb = arboard::Clipboard::new().unwrap();
        let files = cb.get().file_list().unwrap();
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].file_name().unwrap(), "Picture.png");
        assert_eq!(std::fs::read(&files[0]).unwrap(), png);
        let img = cb.get_image().unwrap();
        assert_eq!((img.width, img.height), (2, 1));
        // Pasted back into a note, it's the same picture (no text in the way).
        assert!(cb.get_text().map_or(true, |t| t.is_empty()));
        app.paste_image();
        app.flush_editor();
        let id = app.selected.clone().unwrap();
        assert_eq!(app.lib.store.note(&id).unwrap().body, format!("![](attachments/{})\n", cleannotes_core::attachments::name_for(&png, "png")));
    }

    #[test]
    fn other_files_are_linked_by_their_names() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = App::for_test(&dir.path().join("data"));
        app.new_note();
        let pdf = b"%PDF-1.4 boarding pass".to_vec();
        let file = dir.path().join("boarding-pass.PDF");
        std::fs::write(&file, &pdf).unwrap();
        app.attach_files(&[file]);
        app.flush_editor();
        let name = cleannotes_core::attachments::name_for(&pdf, "pdf");
        let id = app.selected.clone().unwrap();
        let body = app.lib.store.note(&id).unwrap().body.clone();
        assert_eq!(body, format!("[boarding-pass.PDF](attachments/{name})\n"));
        assert!(app.lib.attachment_path(&name).is_some());
        // It syncs with the note, as pictures do.
        assert_eq!(cleannotes_core::attachments::refs_in_body(&body), vec![name]);
    }
}
