//! Locked notes in the app: the password (set, entered, changed), locking
//! and unlocking notes, the lock screen, and re-encrypting a locked note as
//! it's edited. The encryption itself is in [`crate::lock`].
//!
//! As in Apple Notes, one password unlocks every note locked with it, and
//! they stay unlocked until "Lock Now", a while without use, or quitting.
//! Passwords are only ever kept in memory.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use eframe::egui::{self, Align2, Key, Pos2, Rect, RichText, Ui, Vec2};

use super::{modal, App};
use crate::editor::Editor;
use crate::icons::{self, Icon};
use crate::lock::{self, Keys};
use crate::theme::{self, Palette};

/// What the password check in Settings decrypts to.
const CHECK: &str = "CleanNotes locked notes";
/// Unlocked notes lock again after this long without input.
pub const AUTO_LOCK: Duration = Duration::from_secs(15 * 60);

#[derive(Default)]
pub struct LockState {
    /// The current password, once entered (and checked) this session.
    current: Option<String>,
    /// Keys made this session, by salt (base64).
    keys: HashMap<String, Keys>,
    /// The text of each unlocked note.
    unlocked: HashMap<String, String>,
    /// The password typed on the lock screen.
    pub typed: String,
    pub error: Option<String>,
    pub focus: bool,
    pub sheet: Option<Sheet>,
    pub last_input: Option<Instant>,
}

pub enum Sheet {
    /// Choose the password (the first time a note is locked).
    Set { password: String, verify: String, hint: String, then_lock: Vec<String>, error: Option<String> },
    /// Enter the password to lock notes.
    Enter { password: String, then_lock: Vec<String>, error: Option<String> },
    /// Settings → Change Password.
    Change { old: String, password: String, verify: String, hint: String, error: Option<String> },
}

impl App {
    pub fn is_locked(&self, id: &str) -> bool {
        self.store.note(id).is_some_and(|n| lock::is_locked(&n.body))
    }

    /// A locked note that hasn't been unlocked.
    pub fn is_closed(&self, id: &str) -> bool {
        self.is_locked(id) && !self.lock.unlocked.contains_key(id)
    }

    /// A note's Markdown: its text, or a locked note's once unlocked.
    pub fn plain_body(&self, id: &str) -> Option<String> {
        let n = self.store.note(id)?;
        if lock::is_locked(&n.body) {
            self.lock.unlocked.get(id).cloned()
        } else {
            Some(n.body.clone())
        }
    }

    /// The editor for a note, unless it's locked.
    pub fn editor_for(&mut self, id: &str) -> Option<Editor> {
        if self.is_locked(id) && !self.lock.unlocked.contains_key(id) {
            self.try_open(id);
        }
        self.plain_body(id).map(|md| Editor::new(id, &md))
    }

    fn keys(&mut self, password: &str, env: &lock::Envelope) -> anyhow::Result<Keys> {
        if let Some(k) = self.lock.keys.get(&env.salt) {
            return Ok(k.clone());
        }
        let k = lock::derive_for(password, env)?;
        Ok(k)
    }

    /// Opens a locked note with keys already made this session.
    fn try_open(&mut self, id: &str) -> bool {
        let Some(env) = self.store.note(id).and_then(|n| lock::parse(&n.body)) else { return false };
        let Some(k) = self.lock.keys.get(&env.salt).cloned() else { return false };
        match lock::open(&k, &env) {
            Ok(text) => {
                self.lock.unlocked.insert(id.to_owned(), text);
                true
            }
            Err(_) => false,
        }
    }

    /// Opens every locked note the keys made this session open.
    fn open_all(&mut self) {
        let ids: Vec<String> = self.store.notes.iter().filter(|n| lock::is_locked(&n.body)).map(|n| n.id.clone()).collect();
        for id in ids {
            if !self.lock.unlocked.contains_key(&id) {
                self.try_open(&id);
            }
        }
    }

    /// The lock screen's "View Note".
    pub fn unlock(&mut self, id: &str) {
        let password = std::mem::take(&mut self.lock.typed);
        let Some(env) = self.store.note(id).and_then(|n| lock::parse(&n.body)) else { return };
        let opened = lock::derive_for(&password, &env).and_then(|k| lock::open(&k, &env).map(|t| (k, t)));
        match opened {
            Ok((k, text)) => {
                self.lock.keys.insert(env.salt.clone(), k.clone());
                self.lock.unlocked.insert(id.to_owned(), text);
                self.lock.error = None;
                // The settings' check: this device learns the password's
                // salt from the note if it hasn't got one, and the password
                // is the current one if it opens the check.
                match self.settings.lock_check.as_deref().and_then(|c| lock::decode(c).ok()) {
                    None => {
                        let check = lock::seal(&k, &env.salt(), CHECK);
                        self.settings.lock_check = Some(lock::encode(&check));
                        self.lock.current = Some(password);
                    }
                    Some(check) => {
                        let k2 = if check.salt == env.salt { Ok(k) } else { lock::derive_for(&password, &check) };
                        if let Ok(k2) = k2 {
                            if lock::open(&k2, &check).is_ok() {
                                self.lock.keys.insert(check.salt.clone(), k2);
                                self.lock.current = Some(password);
                            }
                        }
                    }
                }
                // Other notes locked with it open now too.
                self.open_all();
                if self.selected.as_deref() == Some(id) {
                    self.editor = self.editor_for(id);
                }
            }
            Err(e) => {
                let hint = (!self.settings.lock_hint.is_empty()).then(|| format!(" Hint: {}", self.settings.lock_hint)).unwrap_or_default();
                let msg = if e.to_string() == "wrong password" { "Wrong password.".to_owned() } else { e.to_string() };
                self.lock.error = Some(format!("{msg}{hint}"));
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
        if self.settings.lock_check.is_none() {
            self.lock.sheet = Some(Sheet::Set { password: String::new(), verify: String::new(), hint: String::new(), then_lock: ids, error: None });
            return;
        }
        let Some((k, salt)) = self.current_keys() else {
            self.lock.sheet = Some(Sheet::Enter { password: String::new(), then_lock: ids, error: None });
            return;
        };
        self.flush_editor();
        let mut locked = 0;
        for id in &ids {
            let Some(text) = self.plain_body(id) else { continue };
            let env = lock::seal(&k, &salt, &text);
            if let Some(n) = self.store.note_mut(id) {
                n.body = lock::locked_body(&text, &env);
                n.touch();
            }
            self.lock.unlocked.insert(id.clone(), text);
            self.unsaved.insert(id.clone());
            locked += 1;
        }
        let what = if locked == 1 { "Note locked".to_owned() } else { format!("{locked} notes locked") };
        self.push_notice(format!("{what}. Choose Lock Now to close locked notes."));
    }

    /// A note that "Lock Note" applies to: there, not deleted, not locked.
    pub fn can_lock(&self, id: &str) -> bool {
        self.store.note(id).is_some_and(|n| n.deleted.is_none() && !lock::is_locked(&n.body))
    }

    /// The current password's keys and salt (those of the settings' check).
    fn current_keys(&mut self) -> Option<(Keys, Vec<u8>)> {
        let password = self.lock.current.clone()?;
        let check = lock::decode(self.settings.lock_check.as_deref()?).ok()?;
        let k = self.keys(&password, &check).ok()?;
        self.lock.keys.insert(check.salt.clone(), k.clone());
        Some((k, check.salt()))
    }

    /// "Remove Lock": the note is stored as plain text again.
    pub fn remove_lock(&mut self, id: &str) {
        self.flush_editor();
        let Some(text) = self.lock.unlocked.remove(id) else { return };
        if let Some(n) = self.store.note_mut(id) {
            n.body = text;
            n.touch();
        }
        self.unsaved.insert(id.to_owned());
    }

    /// Closes every unlocked note and forgets the password.
    pub fn lock_now(&mut self) {
        self.flush_editor();
        let any = !self.lock.unlocked.is_empty();
        self.lock.unlocked.clear();
        self.lock.keys.clear();
        self.lock.current = None;
        self.lock.typed.clear();
        self.lock.error = None;
        // A password sheet left open mustn't stay filled in.
        self.lock.sheet = None;
        if self.selected.as_ref().is_some_and(|s| self.is_locked(s)) {
            self.editor = None;
        }
        if any {
            self.lock.focus = true;
        }
    }

    pub fn has_unlocked(&self) -> bool {
        !self.lock.unlocked.is_empty()
    }

    /// Re-encrypts an unlocked note's edited text; false if `id` isn't one.
    pub(super) fn store_locked(&mut self, id: &str, md: &str) -> bool {
        let Some(env) = self.store.note(id).and_then(|n| lock::parse(&n.body)) else { return false };
        if !self.lock.unlocked.contains_key(id) {
            // Never write a locked note's text out in the clear.
            return true;
        }
        if self.lock.unlocked.get(id).is_some_and(|t| t == md) {
            return true;
        }
        let Some(k) = self.lock.keys.get(&env.salt).cloned() else {
            // Locked again elsewhere with another password: the edit can't
            // be sealed, so say so and close the note rather than drop it
            // silently (never write it out in the clear).
            self.lock.unlocked.remove(id);
            if self.editor.as_ref().is_some_and(|e| e.note_id == id) {
                self.editor = None;
            }
            self.push_notice("A change to a locked note couldn't be saved: it was locked with another password on another device.".into());
            return true;
        };
        let sealed = lock::seal(&k, &env.salt(), md);
        if let Some(n) = self.store.note_mut(id) {
            n.body = lock::locked_body(md, &sealed);
            n.touch();
        }
        self.lock.unlocked.insert(id.to_owned(), md.to_owned());
        self.unsaved.insert(id.to_owned());
        self.last_edit = Some(Instant::now());
        true
    }

    /// After a sync changed a locked note: its new text, if it opens.
    pub(super) fn reopen_after_sync(&mut self, id: &str) -> Option<String> {
        self.lock.unlocked.remove(id);
        self.try_open(id);
        self.lock.unlocked.get(id).cloned()
    }

    /// After a sync: every unlocked note's text is read again from its
    /// (possibly new) sealed text, so a stale copy is never shown or sealed
    /// over another device's edit. Notes gone, no longer locked, or locked
    /// with keys not at hand are closed.
    pub(super) fn refresh_unlocked(&mut self) {
        let ids: Vec<String> = self.lock.unlocked.keys().cloned().collect();
        for id in ids {
            self.reopen_after_sync(&id);
        }
    }

    /// Locks everything after a while without input.
    pub(super) fn auto_lock(&mut self, ctx: &egui::Context) {
        // Checked before this frame's input counts: coming back to a window
        // left minimized for an hour brings input events with it.
        if self.has_unlocked() && self.lock.last_input.is_some_and(|t| t.elapsed() > AUTO_LOCK) {
            self.lock_now();
        }
        if ctx.input(|i| !i.events.is_empty() || i.pointer.is_moving()) {
            self.lock.last_input = Some(Instant::now());
        }
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
            te.request_focus();
            self.lock.focus = false;
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
                let salt = lock::new_salt();
                let k = lock::derive(&password, &salt).map_err(|e| e.to_string())?;
                let check = lock::seal(&k, &salt, CHECK);
                self.lock.keys.insert(check.salt.clone(), k);
                self.settings.lock_check = Some(lock::encode(&check));
                self.settings.lock_hint = hint;
                self.lock.current = Some(password);
                self.lock_notes(&then_lock);
                Ok(())
            }
            SheetAction::Enter(password, then_lock) => {
                self.check_password(&password)?;
                self.lock.current = Some(password);
                self.lock_notes(&then_lock);
                Ok(())
            }
            SheetAction::Change(old, password, hint) => {
                let old_keys = self.check_password(&old)?;
                let _ = old_keys;
                // Every locked note the old password opens gets the new one.
                let salt = lock::new_salt();
                let k = lock::derive(&password, &salt).map_err(|e| e.to_string())?;
                let mut by_salt: HashMap<String, Option<Keys>> = HashMap::new();
                let mut changed = 0;
                let ids: Vec<String> = self.store.notes.iter().filter(|n| lock::is_locked(&n.body)).map(|n| n.id.clone()).collect();
                self.flush_editor();
                for id in ids {
                    let Some(env) = self.store.note(&id).and_then(|n| lock::parse(&n.body)) else { continue };
                    let ok = by_salt.entry(env.salt.clone()).or_insert_with(|| lock::derive_for(&old, &env).ok()).clone();
                    let Some(ok) = ok else { continue };
                    let Ok(text) = lock::open(&ok, &env) else { continue };
                    let sealed = lock::seal(&k, &salt, &text);
                    if let Some(n) = self.store.note_mut(&id) {
                        n.body = lock::locked_body(&text, &sealed);
                        n.touch();
                    }
                    self.unsaved.insert(id);
                    changed += 1;
                }
                let check = lock::seal(&k, &salt, CHECK);
                self.lock.keys.insert(check.salt.clone(), k);
                self.settings.lock_check = Some(lock::encode(&check));
                self.settings.lock_hint = hint;
                self.lock.current = Some(password);
                self.lock.unlocked.clear();
                self.open_all();
                self.push_notice(format!("Password changed for {changed} locked note{}.", if changed == 1 { "" } else { "s" }));
                Ok(())
            }
        }
    }

    /// Checks a password against the settings' check.
    fn check_password(&mut self, password: &str) -> Result<Keys, String> {
        let check = self.settings.lock_check.as_deref().and_then(|c| lock::decode(c).ok()).ok_or("No password is set.")?;
        let k = lock::derive_for(password, &check).map_err(|e| e.to_string())?;
        lock::open(&k, &check).map_err(|_| {
            let hint = if self.settings.lock_hint.is_empty() { String::new() } else { format!(" Hint: {}", self.settings.lock_hint) };
            format!("Wrong password.{hint}")
        })?;
        self.lock.keys.insert(check.salt.clone(), k.clone());
        Ok(k)
    }

    pub fn open_change_password(&mut self) {
        self.lock.sheet = Some(if self.settings.lock_check.is_some() {
            Sheet::Change { old: String::new(), password: String::new(), verify: String::new(), hint: self.settings.lock_hint.clone(), error: None }
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
    use crate::model::Note;

    fn app_with_note(dir: &std::path::Path, body: &str) -> (App, String) {
        let mut app = App::for_test(dir);
        let folder = app.store.default_folder(crate::model::LOCAL).unwrap().id.clone();
        let n = Note::new(crate::model::LOCAL, &folder, body);
        let id = n.id.clone();
        app.store.notes.push(n);
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
    fn lock_edit_lock_now_unlock_and_remove() {
        let dir = tempfile::tempdir().unwrap();
        let (mut app, id) = app_with_note(dir.path(), "# Bank\nPIN 1234\n");
        set_password(&mut app, &id, "hunter2");
        let body = app.store.note(&id).unwrap().body.clone();
        assert!(lock::is_locked(&body) && !body.contains("1234"), "stored encrypted: {body}");
        assert_eq!(app.store.note(&id).unwrap().title(), "Bank");
        // Still open after locking, as in Apple Notes; edits are encrypted.
        assert!(!app.is_closed(&id));
        assert!(app.store_locked(&id, "# Bank\nPIN 9876\n"));
        let body = app.store.note(&id).unwrap().body.clone();
        assert!(lock::is_locked(&body) && !body.contains("9876"));
        // Lock Now: closed, no editor, nothing in memory.
        app.lock_now();
        assert!(app.is_closed(&id));
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
        // Remove Lock stores the text in the clear again.
        app.remove_lock(&id);
        assert_eq!(app.store.note(&id).unwrap().body, "# Bank\nPIN 9876\n");
    }

    #[test]
    fn one_password_opens_all_and_change_password_reencrypts() {
        let dir = tempfile::tempdir().unwrap();
        let (mut app, a) = app_with_note(dir.path(), "# A\nsecret a\n");
        set_password(&mut app, &a, "one");
        let folder = app.store.default_folder(crate::model::LOCAL).unwrap().id.clone();
        let n = Note::new(crate::model::LOCAL, &folder, "# B\nsecret b\n");
        let b = n.id.clone();
        app.store.notes.push(n);
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
        let folder = app.store.default_folder(crate::model::LOCAL).unwrap().id.clone();
        let add = |app: &mut App, body: &str| {
            let n = Note::new(crate::model::LOCAL, &folder, body);
            let id = n.id.clone();
            app.store.notes.push(n);
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
            let body = &app.store.note(id).unwrap().body;
            assert!(lock::is_locked(body) && !body.contains("secret"), "{body}");
        }
        // After Lock Now, a mixed selection: one Enter sheet, and only the
        // note not locked yet is sealed (the others are left as they are).
        let d = add(&mut app, "# D\nsecret d\n");
        let before = app.store.note(&a).unwrap().body.clone();
        app.lock_now();
        app.lock_notes(&[a.clone(), d.clone()]);
        let Some(Sheet::Enter { then_lock, .. }) = &app.lock.sheet else { panic!("asks for the password") };
        assert_eq!(*then_lock, vec![d.clone()]);
        app.sheet_action(SheetAction::Enter("pw".into(), vec![d.clone()])).unwrap();
        assert!(app.is_locked(&d));
        assert_eq!(app.store.note(&a).unwrap().body, before);
        // Nothing left to lock: no sheet.
        app.lock.sheet = None;
        app.lock_notes(&ids);
        assert!(app.lock.sheet.is_none());
    }

    #[test]
    fn a_sync_never_leaves_stale_text_in_an_unlocked_note() {
        let dir = tempfile::tempdir().unwrap();
        let (mut app, id) = app_with_note(dir.path(), "# A\nv1\n");
        set_password(&mut app, &id, "pw");
        app.select(None);
        // Another device edits it; the sync brings its new sealed text.
        let env = lock::parse(&app.store.note(&id).unwrap().body).unwrap();
        let k = app.lock.keys[&env.salt].clone();
        let theirs = "# A\nv2 from the other device\n";
        app.store.note_mut(&id).unwrap().body = lock::locked_body(theirs, &lock::seal(&k, &env.salt(), theirs));
        app.refresh_unlocked();
        assert_eq!(app.plain_body(&id).as_deref(), Some(theirs));
        // Locked again elsewhere with another password: closed, not shown
        // stale or edited blind.
        let salt = lock::new_salt();
        let other = lock::derive("another", &salt).unwrap();
        app.store.note_mut(&id).unwrap().body = lock::locked_body(theirs, &lock::seal(&other, &salt, theirs));
        app.refresh_unlocked();
        assert!(app.is_closed(&id));
    }

    #[test]
    fn auto_lock_fires_when_coming_back_after_a_long_time() {
        let dir = tempfile::tempdir().unwrap();
        let (mut app, id) = app_with_note(dir.path(), "# A\nsecret\n");
        set_password(&mut app, &id, "pw");
        assert!(app.has_unlocked());
        app.lock.sheet = Some(Sheet::Enter { password: "half-typed".into(), then_lock: Vec::new(), error: None });
        app.lock.last_input = Instant::now().checked_sub(AUTO_LOCK + std::time::Duration::from_secs(60));
        // The first frame back carries input (focus, the mouse moving).
        let ctx = egui::Context::default();
        let mut raw = egui::RawInput::default();
        raw.events.push(egui::Event::PointerMoved(Pos2::new(1.0, 1.0)));
        let _ = ctx.run(raw, |ctx| app.auto_lock(ctx));
        assert!(!app.has_unlocked() && app.is_closed(&id));
        assert!(app.lock.sheet.is_none(), "no password left in an open sheet");
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
