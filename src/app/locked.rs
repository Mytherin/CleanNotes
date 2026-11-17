//! Locked notes on the desktop: the lock screen and the password sheets.
//! Locking, unlocking, the password and re-encrypting a locked note as it's
//! edited are the shared library's ([`cleannotes_core::library`]); the
//! encryption itself is in [`crate::lock`].

use eframe::egui::{self, Align2, Key, Pos2, Rect, RichText, Ui, Vec2};

use super::{modal, App, Pane};
use crate::editor::Editor;
use crate::icons::{self, Icon};
use crate::theme::{self, Palette};
use cleannotes_core::library::LockNote;

/// The lock screen's field and the password sheets.
#[derive(Default)]
pub struct LockUi {
    /// The password typed on the lock screen.
    pub typed: String,
    pub error: Option<String>,
    pub focus: bool,
    pub sheet: Option<Sheet>,
}

pub enum Sheet {
    /// Choose the password (the first time a note is locked).
    Set { password: String, verify: String, hint: String, then_lock: Vec<String>, error: Option<String> },
    /// Enter the password to lock a note.
    Enter { password: String, then_lock: Vec<String>, error: Option<String> },
    /// Settings → Change Password.
    Change { old: String, password: String, verify: String, hint: String, error: Option<String> },
}

impl App {
    pub fn is_locked(&self, id: &str) -> bool {
        self.lib.is_locked(id)
    }

    /// A locked note that hasn't been unlocked.
    pub fn is_closed(&self, id: &str) -> bool {
        self.lib.is_closed(id)
    }

    /// A note's Markdown: its text, or a locked note's once unlocked.
    pub fn plain_body(&mut self, id: &str) -> Option<String> {
        self.lib.plain_body(id)
    }

    /// The editor for a note, unless it's locked.
    pub fn editor_for(&mut self, id: &str) -> Option<Editor> {
        self.lib.plain_body(id).map(|md| Editor::new(id, &md))
    }

    /// The lock screen's "View Note".
    pub fn unlock(&mut self, id: &str) {
        let password = std::mem::take(&mut self.lock.typed);
        match self.lib.unlock(id, &password) {
            Ok(()) => {
                self.lock.error = None;
                if self.selected.as_deref() == Some(id) {
                    self.editor = self.editor_for(id);
                }
            }
            Err(msg) => {
                self.lock.error = Some(msg);
                self.lock.focus = true;
            }
        }
    }

    /// "Lock Note": asks for (or to choose) the password first if needed.
    pub fn lock_note(&mut self, id: &str) {
        self.lock_notes(&[id.to_owned()]);
    }

    /// "Lock Notes" with several notes selected: the password is asked for
    /// once, then every one of them not locked yet (nor deleted) is locked.
    pub fn lock_notes(&mut self, ids: &[String]) {
        let ids: Vec<String> = ids.iter().filter(|id| self.can_lock(id)).cloned().collect();
        if ids.is_empty() {
            return;
        }
        self.flush_editor();
        match self.lib.lock_notes(&ids) {
            LockNote::Locked => {}
            LockNote::ChoosePassword => {
                self.lock.sheet = Some(Sheet::Set { password: String::new(), verify: String::new(), hint: String::new(), then_lock: ids, error: None })
            }
            LockNote::EnterPassword => self.lock.sheet = Some(Sheet::Enter { password: String::new(), then_lock: ids, error: None }),
        }
    }

    /// A note that "Lock Note" applies to: there, not deleted, not locked.
    pub fn can_lock(&self, id: &str) -> bool {
        self.lib.can_lock(id)
    }

    /// "Remove Lock": the note is stored as plain text again.
    pub fn remove_lock(&mut self, id: &str) {
        self.flush_editor();
        self.lib.remove_lock(id);
    }

    /// Closes every unlocked note and forgets the password.
    pub fn lock_now(&mut self) {
        self.flush_editor();
        self.lib.lock_now();
        self.after_lock_now();
    }

    /// After locked notes closed (Lock Now, or a while without use).
    pub(super) fn after_lock_now(&mut self) {
        let any = self.editor.as_ref().is_some_and(|e| self.lib.is_locked(&e.note_id));
        self.images.forget_sealed();
        self.pdfs.forget_sealed();
        self.lock.typed.clear();
        self.lock.error = None;
        // A password sheet left open mustn't stay filled in.
        self.lock.sheet = None;
        if self.selected.as_ref().is_some_and(|s| self.lib.is_locked(s)) {
            self.editor = None;
        }
        if any {
            self.lock.focus = true;
        }
    }

    pub fn has_unlocked(&self) -> bool {
        self.lib.has_unlocked()
    }

    // ---- The lock screen ----------------------------------------------------

    pub(super) fn lock_screen(&mut self, ui: &mut Ui, p: &Palette, area: Rect, id: &str) {
        let c = Pos2::new(area.center().x, area.top() + area.height() * 0.32);
        icons::paint(ui.painter(), Icon::Lock, Rect::from_center_size(c, Vec2::splat(54.0)), p.secondary);
        ui.painter().text(c + Vec2::new(0.0, 56.0), Align2::CENTER_CENTER, "This note is locked.", theme::bold_font(17.0), p.text);
        ui.painter().text(c + Vec2::new(0.0, 82.0), Align2::CENTER_CENTER, "Enter the notes password to view locked notes.", theme::font(13.0), p.secondary);
        let field = Rect::from_center_size(c + Vec2::new(0.0, 122.0), Vec2::new(260.0, 30.0));
        let te = ui.put(
            field,
            egui::TextEdit::singleline(&mut self.lock.typed).password(true).hint_text("Password").margin(egui::vec2(8.0, 6.0)),
        );
        if self.lock.focus {
            // Going through the list with ↑/↓ (or searching) can land on a
            // locked note: as in Apple Notes the keys stay with the list (or
            // the search field) rather than jumping to the password.
            // Return or Tab in the list moves here.
            if self.focus == Pane::Note && !ui.memory(|m| m.has_focus(super::search_field_id())) {
                te.request_focus();
            }
            self.lock.focus = false;
        }
        if te.gained_focus() {
            self.focus = Pane::Note;
        }
        let enter = te.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));
        let btn = Rect::from_center_size(c + Vec2::new(0.0, 166.0), Vec2::new(120.0, 30.0));
        let b = egui::Button::new(RichText::new("View Note").color(egui::Color32::WHITE)).fill(theme::BLUE).rounding(8.0);
        let clicked = ui.put(btn, b).clicked();
        if (enter || clicked) && !self.lock.typed.is_empty() {
            self.unlock(id);
        }
        if let Some(e) = &self.lock.error {
            let g = ui.painter().layout(e.clone(), theme::font(12.5), theme::RED, 360.0);
            ui.painter().galley(c + Vec2::new(-g.size().x / 2.0, 192.0), g, theme::RED);
        }
    }

    // ---- Password sheets ----------------------------------------------------

    pub(super) fn lock_sheets(&mut self, ctx: &egui::Context, p: &Palette) {
        let Some(sheet) = &mut self.lock.sheet else { return };
        let mut done: Option<bool> = None;
        let mut action: Option<SheetAction> = None;
        modal(ctx, "lock-sheet", p, |ui| {
            ui.set_width(360.0);
            let field = |ui: &mut Ui, label: &str, text: &mut String, focus: bool| {
                ui.label(RichText::new(label).color(p.secondary).size(12.5));
                let te = ui.add(egui::TextEdit::singleline(text).password(true).desired_width(f32::INFINITY));
                if focus && ui.memory(|m| m.focused().is_none()) {
                    te.request_focus();
                }
                te
            };
            match sheet {
                Sheet::Set { password, verify, hint, then_lock, error } => {
                    ui.label(RichText::new("Create a password for locked notes").font(theme::bold_font(15.0)));
                    ui.add_space(4.0);
                    ui.add(egui::Label::new(RichText::new("Locked notes are encrypted with this password. If you forget it, they can't be recovered.").color(p.secondary).size(12.5)).wrap());
                    ui.add_space(8.0);
                    field(ui, "Password", password, true);
                    field(ui, "Verify", verify, false);
                    ui.label(RichText::new("Hint (recommended)").color(p.secondary).size(12.5));
                    ui.add(egui::TextEdit::singleline(hint).desired_width(f32::INFINITY));
                    if let Some(e) = error {
                        ui.colored_label(theme::RED, e.as_str());
                    }
                    if let Some(go) = buttons(ui, "Set Password") {
                        done = Some(false);
                        if go {
                            if password.is_empty() {
                                *error = Some("Enter a password.".into());
                                done = None;
                            } else if password != verify {
                                *error = Some("The passwords don't match.".into());
                                done = None;
                            } else {
                                action = Some(SheetAction::Set(password.clone(), hint.clone(), then_lock.clone()));
                            }
                        }
                    }
                }
                Sheet::Enter { password, then_lock, error } => {
                    ui.label(RichText::new("Enter the notes password").font(theme::bold_font(15.0)));
                    ui.add_space(8.0);
                    let te = field(ui, "Password", password, true);
                    if let Some(e) = error {
                        ui.colored_label(theme::RED, e.as_str());
                    }
                    let enter = te.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));
                    match buttons(ui, "OK") {
                        Some(true) => action = Some(SheetAction::Enter(password.clone(), then_lock.clone())),
                        Some(false) => done = Some(false),
                        None if enter => action = Some(SheetAction::Enter(password.clone(), then_lock.clone())),
                        None => {}
                    }
                }
                Sheet::Change { old, password, verify, hint, error } => {
                    ui.label(RichText::new("Change Password").font(theme::bold_font(15.0)));
                    ui.add_space(8.0);
                    field(ui, "Old password", old, true);
                    field(ui, "New password", password, false);
                    field(ui, "Verify", verify, false);
                    ui.label(RichText::new("Hint (recommended)").color(p.secondary).size(12.5));
                    ui.add(egui::TextEdit::singleline(hint).desired_width(f32::INFINITY));
                    if let Some(e) = error {
                        ui.colored_label(theme::RED, e.as_str());
                    }
                    if let Some(go) = buttons(ui, "Change Password") {
                        done = Some(false);
                        if go {
                            if password.is_empty() || password != verify {
                                *error = Some("The new passwords don't match.".into());
                                done = None;
                            } else {
                                action = Some(SheetAction::Change(old.clone(), password.clone(), hint.clone()));
                                done = None;
                            }
                        }
                    }
                }
            }
        });
        if let Some(a) = action {
            match self.sheet_action(a) {
                Ok(()) => self.lock.sheet = None,
                Err(msg) => {
                    if let Some(Sheet::Set { error, .. } | Sheet::Enter { error, .. } | Sheet::Change { error, .. }) = &mut self.lock.sheet {
                        *error = Some(msg);
                    }
                }
            }
        } else if done == Some(false) || ctx.input(|i| i.key_pressed(Key::Escape)) {
            self.lock.sheet = None;
        }
    }


    fn sheet_action(&mut self, a: SheetAction) -> Result<(), String> {
        match a {
            SheetAction::Set(password, hint, then_lock) => {
                self.lib.set_password(&password, &hint)?;
                self.lock_notes(&then_lock);
            }
            SheetAction::Enter(password, then_lock) => {
                self.lib.enter_password(&password)?;
                self.lock_notes(&then_lock);
            }
            SheetAction::Change(old, password, hint) => {
                self.flush_editor();
                self.lib.change_password(&old, &password, &hint)?;
            }
        }
        Ok(())
    }

    pub fn open_change_password(&mut self) {
        self.lock.sheet = Some(if self.lib.has_lock_password() {
            Sheet::Change { old: String::new(), password: String::new(), verify: String::new(), hint: self.lib.settings.lock_hint.clone(), error: None }
        } else {
            Sheet::Set { password: String::new(), verify: String::new(), hint: String::new(), then_lock: Vec::new(), error: None }
        });
    }
}

enum SheetAction {
    Set(String, String, Vec<String>),
    Enter(String, Vec<String>),
    Change(String, String, String),
}

/// Cancel and the default button; Some(true) for the default.
fn buttons(ui: &mut Ui, label: &str) -> Option<bool> {
    ui.add_space(10.0);
    let mut r = None;
    ui.horizontal(|ui| {
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let b = egui::Button::new(RichText::new(label).color(egui::Color32::WHITE)).fill(theme::BLUE);
            if ui.add(b).clicked() {
                r = Some(true);
            }
            if ui.button("Cancel").clicked() {
                r = Some(false);
            }
        });
    });
    r
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lock;
    use crate::model::Note;

    fn app_with_note(dir: &std::path::Path, body: &str) -> (App, String) {
        let mut app = App::for_test(dir);
        let folder = app.lib.default_folder_id(crate::model::LOCAL);
        let n = Note::new(crate::model::LOCAL, &folder, body);
        let id = n.id.clone();
        app.lib.store.notes.push(n);
        app.select(Some(id.clone()));
        (app, id)
    }

    fn set_password(app: &mut App, id: &str, pw: &str) {
        app.lock_note(id);
        assert!(matches!(app.lock.sheet, Some(Sheet::Set { .. })), "the first lock asks for a password");
        app.sheet_action(SheetAction::Set(pw.into(), "the usual".into(), vec![id.to_owned()])).unwrap();
        app.lock.sheet = None;
    }

    #[test]
    fn pictures_attached_to_a_locked_note_are_encrypted() {
        use cleannotes_core::attachments;
        let dir = tempfile::tempdir().unwrap();
        let (mut app, id) = app_with_note(dir.path(), "# Passport\n");
        set_password(&mut app, &id, "hunter2");
        assert!(app.is_locked(&id) && app.can_attach());
        let png = crate::editor::images::png_from_rgba(1, 1, &[1, 2, 3, 255]).unwrap();
        app.attach_images(vec![("scan".into(), png.clone())]);
        app.flush_editor();
        let body = app.lib.store.note(&id).unwrap().body.clone();
        assert!(lock::is_locked(&body));
        let files = attachments::refs_in_body(&body);
        assert!(files.len() == 1 && files[0].ends_with(".enc"), "{body}");
        // Nothing of it in the clear on disk.
        assert!(!attachments::exists(&app.lib.attachments_dir(), &attachments::name_for(&png, "png")));
        let md = app.lib.plain_body_now(&id).unwrap();
        let r = attachments::image_refs_in(&md).pop().unwrap();
        assert_eq!(app.lib.attachment_data(&r).unwrap(), png);
    }

    #[test]
    fn lock_edit_lock_now_unlock_and_remove() {
        let dir = tempfile::tempdir().unwrap();
        let (mut app, id) = app_with_note(dir.path(), "# Bank\nPIN 1234\n");
        set_password(&mut app, &id, "hunter2");
        let body = app.lib.store.note(&id).unwrap().body.clone();
        assert!(lock::is_locked(&body) && !body.contains("1234"), "stored encrypted: {body}");
        assert_eq!(app.lib.store.note(&id).unwrap().title(), "Bank");
        // Still open after locking, as in Apple Notes; edits are encrypted.
        assert!(!app.is_closed(&id));
        app.lib.set_body(&id, "# Bank\nPIN 9876\n");
        let body = app.lib.store.note(&id).unwrap().body.clone();
        assert!(lock::is_locked(&body) && !body.contains("9876"));
        // Lock Now: closed, no editor, nothing in memory.
        app.lock_now();
        assert!(app.is_closed(&id));
        assert!(app.editor.is_none());
        assert!(app.editor_for(&id).is_none());
        assert_eq!(app.plain_body(&id), None);
        // A wrong password says so (with the hint); the right one opens it.
        app.lock.typed = "nope".into();
        app.unlock(&id);
        assert!(app.is_closed(&id));
        assert!(app.lock.error.as_deref().unwrap().contains("the usual"));
        app.lock.typed = "hunter2".into();
        app.unlock(&id);
        assert_eq!(app.plain_body(&id).as_deref(), Some("# Bank\nPIN 9876\n"));
        assert!(app.editor.is_some(), "the open note shows");
        // Remove Lock stores the text in the clear again.
        app.remove_lock(&id);
        assert_eq!(app.lib.store.note(&id).unwrap().body, "# Bank\nPIN 9876\n");
    }

    #[test]
    fn one_password_opens_all_and_change_password_reencrypts() {
        let dir = tempfile::tempdir().unwrap();
        let (mut app, a) = app_with_note(dir.path(), "# A\nsecret a\n");
        set_password(&mut app, &a, "one");
        let folder = app.lib.default_folder_id(crate::model::LOCAL);
        let n = Note::new(crate::model::LOCAL, &folder, "# B\nsecret b\n");
        let b = n.id.clone();
        app.lib.store.notes.push(n);
        // The password is known this session: locking B asks nothing.
        app.lock_note(&b);
        assert!(app.lock.sheet.is_none() && app.is_locked(&b));
        app.lock_now();
        app.lock.typed = "one".into();
        app.unlock(&a);
        // B opens with the same password without asking again.
        assert_eq!(app.editor_for(&b).map(|_| ()), Some(()));
        // Change the password: both notes now open with the new one only.
        app.sheet_action(SheetAction::Change("one".into(), "two".into(), String::new())).unwrap();
        app.lock_now();
        app.lock.typed = "one".into();
        app.unlock(&b);
        assert!(app.is_closed(&b));
        app.lock.typed = "two".into();
        app.unlock(&b);
        assert_eq!(app.plain_body(&b).as_deref(), Some("# B\nsecret b\n"));
        assert_eq!(app.plain_body(&a).as_deref(), Some("# A\nsecret a\n"));
        // A wrong old password changes nothing.
        assert!(app.sheet_action(SheetAction::Change("bad".into(), "three".into(), String::new())).is_err());
    }

    #[test]
    fn locking_several_notes_asks_for_the_password_once() {
        let dir = tempfile::tempdir().unwrap();
        let (mut app, a) = app_with_note(dir.path(), "# A\nsecret a\n");
        let folder = app.lib.default_folder_id(crate::model::LOCAL);
        let add = |app: &mut App, body: &str| {
            let n = Note::new(crate::model::LOCAL, &folder, body);
            let id = n.id.clone();
            app.lib.store.notes.push(n);
            id
        };
        let b = add(&mut app, "# B\nsecret b\n");
        let c = add(&mut app, "# C\nsecret c\n");
        let ids = vec![a.clone(), b.clone(), c.clone()];
        // The first time: one sheet to choose the password, then all lock.
        app.lock_notes(&ids);
        let Some(Sheet::Set { then_lock, .. }) = &app.lock.sheet else { panic!("asks for a password") };
        assert_eq!(*then_lock, ids);
        app.sheet_action(SheetAction::Set("pw".into(), String::new(), ids.clone())).unwrap();
        app.lock.sheet = None;
        for id in &ids {
            let body = &app.lib.store.note(id).unwrap().body;
            assert!(lock::is_locked(body) && !body.contains("secret"), "{body}");
        }
        // After Lock Now, a mixed selection: one Enter sheet, and only the
        // note not locked yet is sealed (the others are left as they are).
        let d = add(&mut app, "# D\nsecret d\n");
        let before = app.lib.store.note(&a).unwrap().body.clone();
        app.lock_now();
        app.lock_notes(&[a.clone(), d.clone()]);
        let Some(Sheet::Enter { then_lock, .. }) = &app.lock.sheet else { panic!("asks for the password") };
        assert_eq!(*then_lock, vec![d.clone()]);
        app.sheet_action(SheetAction::Enter("pw".into(), vec![d.clone()])).unwrap();
        assert!(app.is_locked(&d));
        assert_eq!(app.lib.store.note(&a).unwrap().body, before);
        // Nothing left to lock: no sheet.
        app.lock.sheet = None;
        app.lock_notes(&ids);
        assert!(app.lock.sheet.is_none());
    }

    #[test]
    fn auto_lock_fires_when_coming_back_after_a_long_time() {
        let dir = tempfile::tempdir().unwrap();
        let (mut app, id) = app_with_note(dir.path(), "# A\nsecret\n");
        set_password(&mut app, &id, "pw");
        assert!(app.has_unlocked());
        app.lock.sheet = Some(Sheet::Enter { password: "half-typed".into(), then_lock: Vec::new(), error: None });
        app.lib.test_last_lock_use(cleannotes_core::library::AUTO_LOCK + std::time::Duration::from_secs(60));
        // The first frame back carries input (focus, the mouse moving).
        let ctx = egui::Context::default();
        let mut raw = egui::RawInput::default();
        raw.events.push(egui::Event::PointerMoved(Pos2::new(1.0, 1.0)));
        let _ = ctx.run(raw, |ctx| app.background(ctx));
        assert!(!app.has_unlocked() && app.is_closed(&id));
        assert!(app.editor.is_none());
        assert!(app.lock.sheet.is_none(), "no password left in an open sheet");
    }

    /// One frame of the keys and the note pane (the lock screen).
    fn run_frame(app: &mut App, ctx: &egui::Context, events: Vec<egui::Event>) {
        let raw = egui::RawInput { events, ..Default::default() };
        let _ = ctx.run(raw, |ctx| {
            app.shortcuts(ctx);
            egui::CentralPanel::default().show(ctx, |ui| {
                if let Some(id) = app.selected.clone().filter(|s| app.is_closed(s)) {
                    let p = Palette::get(ctx);
                    let area = ui.max_rect();
                    app.lock_screen(ui, &p, area, &id);
                }
            });
        });
    }

    #[test]
    fn arrows_go_on_through_the_list_past_a_locked_note() {
        let dir = tempfile::tempdir().unwrap();
        let (mut app, a) = app_with_note(dir.path(), "# A\n");
        set_password(&mut app, &a, "pw");
        let folder = app.lib.default_folder_id(crate::model::LOCAL);
        let n = Note::new(crate::model::LOCAL, &folder, "# B\n");
        let b = n.id.clone();
        app.lib.store.notes.push(n);
        app.lock_now();
        app.shown = vec![a.clone(), b.clone()];
        app.select(Some(a.clone()));
        app.focus = Pane::List;
        let down = egui::Event::Key { key: Key::ArrowDown, physical_key: None, pressed: true, repeat: false, modifiers: egui::Modifiers::NONE };
        let ctx = egui::Context::default();
        theme::install_fonts(&ctx);
        // The locked note shows its lock screen without taking the keys…
        run_frame(&mut app, &ctx, vec![]);
        run_frame(&mut app, &ctx, vec![]);
        assert!(!ctx.wants_keyboard_input(), "the password field mustn't take focus");
        // …so ↓ goes on to the next note.
        run_frame(&mut app, &ctx, vec![down]);
        assert_eq!(app.selected.as_deref(), Some(b.as_str()));
        // Return in the list goes to the password field.
        app.select(Some(a.clone()));
        let enter = egui::Event::Key { key: Key::Enter, physical_key: None, pressed: true, repeat: false, modifiers: egui::Modifiers::NONE };
        run_frame(&mut app, &ctx, vec![enter]);
        run_frame(&mut app, &ctx, vec![]);
        assert!(app.focus == Pane::Note && ctx.wants_keyboard_input(), "Return opens the password field");
    }

    #[test]
    fn search_finds_locked_notes_by_title_only() {
        let dir = tempfile::tempdir().unwrap();
        let (mut app, id) = app_with_note(dir.path(), "# Bank\nPIN 1234\n");
        set_password(&mut app, &id, "pw");
        app.search = "bank".into();
        assert!(app.visible_notes().contains(&id));
        app.search = "1234".into();
        assert!(!app.visible_notes().contains(&id));
    }
}
