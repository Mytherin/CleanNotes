//! Attaching pictures to the open note: "Attach Image…" (a file dialog),
//! files dropped on the window, and pasting a picture.
//!
//! Each picture is saved in the attachments folder (see
//! [`cleannotes_core::attachments`]) and put at the caret, on a line of its
//! own. Locked notes can't have pictures (they wouldn't be encrypted with
//! the note), nor can deleted ones be changed.

use std::path::{Path, PathBuf};

use super::App;
use crate::editor::images;
use crate::editor::Command;

/// The file types the dialog offers (HEIC is kept and synced, but shown as
/// a box: it can't be decoded here).
pub const EXTENSIONS: [&str; 8] = ["png", "jpg", "jpeg", "gif", "webp", "bmp", "heic", "heif"];

/// Whether a dropped file looks like a picture.
pub fn is_image_path(p: &Path) -> bool {
    p.extension().and_then(|e| e.to_str()).is_some_and(|e| EXTENSIONS.contains(&e.to_ascii_lowercase().as_str()))
}

impl App {
    /// Whether pictures can be added to the open note now.
    pub fn can_attach(&self) -> bool {
        let Some(e) = &self.editor else { return false };
        self.lib.store.note(&e.note_id).is_some_and(|n| n.deleted.is_none()) && !self.is_locked(&e.note_id)
    }

    /// Why pictures can't be added to the open note, for a notice.
    fn why_not_attach(&self) -> Option<&'static str> {
        let e = self.editor.as_ref()?;
        if self.is_locked(&e.note_id) {
            Some("Images can't be added to locked notes.")
        } else if self.lib.store.note(&e.note_id).is_some_and(|n| n.deleted.is_some()) {
            Some("Deleted notes can't be edited. To edit this note, recover it first.")
        } else {
            None
        }
    }

    /// Puts pictures (their bytes, with a name for messages) at the caret.
    pub fn attach_images(&mut self, pictures: Vec<(String, Vec<u8>)>) {
        if !self.can_attach() {
            if let Some(why) = self.why_not_attach() {
                self.push_notice(why.into());
            }
            return;
        }
        for (label, data) in pictures {
            match self.lib.add_image(&data) {
                Ok(name) => self.editor_command(Command::InsertImage(name)),
                Err(e) => self.push_notice(format!("{label} can't be attached: {e:#}.")),
            }
        }
    }

    /// Attaches image files.
    pub fn attach_files(&mut self, paths: &[PathBuf]) {
        let mut pictures = Vec::new();
        for p in paths {
            let label = p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| p.display().to_string());
            match std::fs::read(p) {
                Ok(data) => pictures.push((label, data)),
                Err(e) => self.push_notice(format!("{label} can't be read: {e}.")),
            }
        }
        self.attach_images(pictures);
    }

    /// "Attach Image…": picks files with the system's dialog.
    pub fn choose_images(&mut self) {
        if !self.can_attach() {
            if let Some(why) = self.why_not_attach() {
                self.push_notice(why.into());
            }
            return;
        }
        let picked = rfd::FileDialog::new().set_title("Attach Image").add_filter("Images", &EXTENSIONS).pick_files();
        if let Some(paths) = picked {
            self.attach_files(&paths);
        }
    }

    /// Paste with a picture (or picture files) on the clipboard and no text
    /// (text is pasted by the editor itself).
    pub fn paste_image(&mut self) {
        let Ok(mut cb) = arboard::Clipboard::new() else { return };
        if cb.get_text().is_ok_and(|t| !t.is_empty()) {
            return;
        }
        if let Ok(files) = cb.get().file_list() {
            let files: Vec<PathBuf> = files.into_iter().filter(|p| is_image_path(p)).collect();
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
        assert!(is_image_path(&file));
        app.attach_files(&[file]);
        let name = cleannotes_core::attachments::name_for(&png, "png");
        app.flush_editor();
        let id = app.selected.clone().unwrap();
        assert_eq!(app.lib.store.note(&id).unwrap().body, format!("![](attachments/{name})\n"));
        assert!(app.lib.attachment_path(&name).is_some());
        // Something that isn't a picture is refused, with a notice.
        let txt = dir.path().join("notes.txt");
        std::fs::write(&txt, "hello").unwrap();
        app.attach_files(&[txt]);
        app.flush_editor();
        assert_eq!(app.lib.store.note(&id).unwrap().body, format!("![](attachments/{name})\n"));
    }
}
