//! CleanPassword's vault in the desktop app, when it's in the same S3 bucket
//! as an account's notes: a "CleanPassword" section in the sidebar (All
//! Passwords, Codes, its folders, Recently Deleted), its items in the list,
//! and an item's details (user name, password, verification code,
//! websites, passkeys, fields, notes, earlier passwords) in the note pane,
//! with copy buttons, as in Apple's Passwords app. Read-only: passwords are
//! edited in CleanPassword. Finding, unlocking and reading the vault is the
//! core's ([`cleannotes_core::passwords`]).

use std::collections::HashSet;
use std::time::{Duration, Instant};

use eframe::egui::{self, Align2, Key, Modifiers, Pos2, Rect, RichText, Sense, Ui, Vec2};
use zeroize::Zeroizing;

use super::sidebar::{glass_button, truncate};
use super::{consume_exact, list_time, note_header_time, App, Pane, TOOLBAR_H};
use crate::icons::{self, Icon};
use crate::menu;
use crate::theme::{self, Palette};
use cleannotes_core::passwords::{self, totp, Busy, Candidate, Item, ItemKind, Passwords, State};
use cleannotes_core::sync::BackendConfig;

/// A copied password is taken off the clipboard after this long, as in
/// CleanPassword.
const CLEAR_CLIPBOARD: Duration = Duration::from_secs(90);
/// Looked for again this often (a vault made since, or one that was
/// unreachable).
const SEARCH_EVERY: Duration = Duration::from_secs(10 * 60);
/// The sidebar section's entry in the folded sections.
const SECTION_KEY: &str = "#cleanpassword";
const ROW_H: f32 = 54.0;

/// What the CleanPassword section shows in the list.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum View {
    All,
    /// Items with a verification code.
    Codes,
    /// A folder (`Work`, or `Work/Email` inside it).
    Folder(String),
    /// CleanPassword's Recently Deleted.
    Deleted,
}

pub struct PasswordsUi {
    pub service: Passwords,
    /// Shown in the list and note panes instead of notes.
    pub view: Option<View>,
    pub selected: Option<String>,
    /// The master password being typed.
    typed: Zeroizing<String>,
    focus_password: bool,
    search: String,
    focus_search: bool,
    /// What's shown in the clear in the open item ("password", "field 2",
    /// "history").
    revealed: HashSet<String>,
    /// A copied secret, to take off the clipboard later.
    copied: Option<(Zeroizing<String>, Instant)>,
    /// The S3 accounts last searched with.
    searched_with: Option<String>,
    /// The list as shown last frame (for ↑/↓).
    shown: Vec<String>,
}

impl PasswordsUi {
    pub fn new(service: Passwords) -> PasswordsUi {
        PasswordsUi {
            service,
            view: None,
            selected: None,
            typed: Default::default(),
            focus_password: false,
            search: String::new(),
            focus_search: false,
            revealed: HashSet::new(),
            copied: None,
            searched_with: None,
            shown: Vec::new(),
        }
    }

    /// After the vault locked: nothing typed or shown in the clear stays.
    fn forget(&mut self) {
        self.typed = Default::default();
        self.revealed.clear();
        self.focus_password = true;
    }
}

/// The items `view` shows, matching `search`.
fn items_in<'a>(s: &'a State, view: &View, search: &str) -> Vec<&'a Item> {
    let items = if *view == View::Deleted { s.deleted() } else { s.items() };
    items
        .into_iter()
        .filter(|i| match view {
            View::All | View::Deleted => true,
            View::Codes => totp::code_now(&i.totp).is_some(),
            View::Folder(f) => i.in_folder(f),
        })
        .filter(|i| search.trim().is_empty() || i.matches(search))
        .collect()
}

fn view_title(view: &View) -> String {
    match view {
        View::All => "All Passwords".into(),
        View::Codes => "Codes".into(),
        View::Folder(f) => f.rsplit('/').next().unwrap_or(f).to_owned(),
        View::Deleted => "Recently Deleted".into(),
    }
}

/// The circle with the item's first letter, in a color of its own.
fn item_badge(painter: &egui::Painter, center: Pos2, radius: f32, item: &Item) {
    const COLORS: [egui::Color32; 6] = [
        egui::Color32::from_rgb(0x34, 0x8B, 0xF6),
        egui::Color32::from_rgb(0x30, 0xB0, 0x5A),
        egui::Color32::from_rgb(0xF2, 0x8C, 0x28),
        egui::Color32::from_rgb(0xAF, 0x52, 0xDE),
        egui::Color32::from_rgb(0xE5, 0x48, 0x5C),
        egui::Color32::from_rgb(0x5A, 0x6B, 0x7D),
    ];
    let title = item.title();
    let n = title.bytes().fold(0usize, |a, b| a.wrapping_mul(31).wrapping_add(b as usize));
    painter.circle_filled(center, radius, COLORS[n % COLORS.len()]);
    if item.kind == ItemKind::Note {
        icons::paint(painter, Icon::Notes, Rect::from_center_size(center, Vec2::splat(radius)), egui::Color32::WHITE);
    } else {
        let letter: String = title.chars().next().map(|c| c.to_uppercase().collect()).unwrap_or_default();
        painter.text(center, Align2::CENTER_CENTER, letter, theme::bold_font(radius * 0.95), egui::Color32::WHITE);
    }
}

impl App {
    // ---- In the background --------------------------------------------------

    /// The enabled S3 accounts (whose access keys may read a vault).
    fn s3_accounts(&self) -> Vec<(String, cleannotes_core::sync::s3::Config)> {
        self.lib
            .store
            .accounts
            .iter()
            .filter(|a| a.enabled)
            .filter_map(|a| match &a.backend {
                BackendConfig::S3(c) => Some((a.id.clone(), c.clone())),
                _ => None,
            })
            .collect()
    }

    /// Each frame: looks for the vault when the accounts change (and now
    /// and then), locks it after a while without use, and takes a copied
    /// password off the clipboard.
    pub(super) fn passwords_background(&mut self, ctx: &egui::Context) {
        let accounts = self.s3_accounts();
        let fingerprint = format!("{accounts:?}");
        let stale = self.pw.service.last_search().is_none_or(|t| t.elapsed() > SEARCH_EVERY);
        if self.pw.searched_with.as_deref() != Some(fingerprint.as_str()) || stale {
            // The access keys come from the credential store: only now.
            let candidates: Vec<Candidate> =
                accounts.into_iter().filter_map(|(id, c)| Some((id.clone(), c, self.lib.db.secret(&id)?))).collect();
            self.pw.searched_with = Some(fingerprint);
            if !candidates.is_empty() {
                self.pw.service.search(candidates);
            } else if self.pw.service.state().found.is_some() {
                // The account was removed: so is its vault.
                self.pw.service.search(Vec::new());
            }
        }
        if self.pw.service.tick() {
            self.pw.forget();
        }
        if self.pw.view.is_some() && ctx.input(|i| !i.events.is_empty() || i.pointer.is_moving()) {
            self.pw.service.touch();
        }
        if self.pw.view.is_some() && self.pw.service.state().found.is_none() {
            self.pw.view = None;
        }
        if let Some((text, at)) = &self.pw.copied {
            if at.elapsed() > CLEAR_CLIPBOARD {
                if let Ok(mut cb) = arboard::Clipboard::new() {
                    if cb.get_text().is_ok_and(|t| t == **text) {
                        let _ = cb.clear();
                    }
                }
                self.pw.copied = None;
            } else {
                ctx.request_repaint_after(CLEAR_CLIPBOARD);
            }
        }
    }

    /// Shows a CleanPassword list in place of notes.
    pub fn open_passwords(&mut self, view: View) {
        self.flush_editor();
        self.pw.search.clear();
        if self.pw.view.as_ref() != Some(&view) {
            let svc = self.pw.service.clone();
            let s = svc.state();
            self.pw.selected = items_in(&s, &view, "").first().map(|i| i.id.clone());
            self.pw.revealed.clear();
        }
        self.pw.view = Some(view);
        self.pw.focus_password = true;
    }

    fn select_password(&mut self, id: Option<String>) {
        if self.pw.selected != id {
            self.pw.revealed.clear();
        }
        self.pw.selected = id;
    }

    /// Copies `text`; a secret is taken off the clipboard again later.
    fn copy_from_vault(&mut self, ctx: &egui::Context, text: String, what: &str, secret: bool) {
        ctx.copy_text(text.clone());
        self.pw.service.touch();
        if secret {
            self.pw.copied = Some((Zeroizing::new(text), Instant::now()));
            self.push_notice(format!("Copied {what}. It's cleared from the clipboard in 90 seconds."));
        } else {
            self.push_notice(format!("Copied {what}."));
        }
    }

    pub(super) fn lock_passwords(&mut self) {
        self.pw.service.lock();
        self.pw.forget();
    }

    // ---- The sidebar --------------------------------------------------------

    pub(super) fn passwords_section(&mut self, ui: &mut Ui, p: &Palette) {
        let svc = self.pw.service.clone();
        let s = svc.state();
        let Some(found) = s.found.clone() else { return };
        let unlocked = s.unlocked();
        let busy = s.busy;
        let error = s.error.clone();
        let all = s.items().len();
        let codes = items_in(&s, &View::Codes, "").len();
        let deleted = s.deleted().len();
        let folders = s.folders();
        drop(s);

        let collapsed = self.lib.settings.collapsed.iter().any(|c| c == SECTION_KEY);
        let (rect, resp) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 30.0), Sense::click());
        let painter = ui.painter();
        painter.text(Pos2::new(rect.left() + 16.0, rect.center().y + 2.0), Align2::LEFT_CENTER, "CleanPassword", theme::bold_font(12.0), p.secondary);
        let right = Rect::from_center_size(Pos2::new(rect.right() - 16.0, rect.center().y + 2.0), Vec2::splat(12.0));
        if resp.hovered() {
            icons::paint(painter, if collapsed { Icon::ChevronRight } else { Icon::ChevronDown }, right, p.secondary);
        } else if busy.is_some() {
            let t = ui.input(|i| i.time) as f32;
            painter.add(egui::Shape::circle_stroke(right.center(), 5.0, egui::Stroke::new(1.0_f32, p.tertiary)));
            let a = t * 6.0;
            painter.line_segment([right.center(), right.center() + Vec2::new(a.cos(), a.sin()) * 5.0], egui::Stroke::new(1.5_f32, p.secondary));
            ui.ctx().request_repaint();
        } else if unlocked && error.is_some() {
            icons::paint(painter, Icon::Warning, right, theme::RED);
        } else if !unlocked {
            icons::paint(painter, Icon::Lock, right, p.tertiary);
        }
        let tip = match (&error, unlocked) {
            (Some(e), true) => format!("Couldn't read the vault: {e}"),
            _ => format!("Your CleanPassword vault, in {}. Read-only here: edit passwords in CleanPassword.", found.place()),
        };
        let resp = resp.on_hover_text(tip);
        if resp.clicked() {
            if collapsed {
                self.lib.settings.collapsed.retain(|c| c != SECTION_KEY);
            } else {
                self.lib.settings.collapsed.push(SECTION_KEY.to_owned());
            }
        }
        resp.context_menu(|ui| {
            menu::begin(ui, 200.0);
            if unlocked {
                if menu::item("Refresh").icon(Icon::Sync).show(ui).clicked() {
                    self.pw.service.refresh();
                    ui.close_menu();
                }
                if menu::item("Lock CleanPassword").icon(Icon::LockOutline).show(ui).clicked() {
                    self.lock_passwords();
                    ui.close_menu();
                }
            } else if menu::item("Unlock…").icon(Icon::LockOpen).show(ui).clicked() {
                self.open_passwords(View::All);
                ui.close_menu();
            }
        });
        if collapsed {
            return;
        }

        let current = self.pw.view.clone();
        let mut go: Option<View> = None;
        let count = |n: usize| unlocked.then_some(n);
        if self.paint_row(ui, p, 0, Icon::Key, "All Passwords", count(all), current == Some(View::All), None).clicked() {
            go = Some(View::All);
        }
        if unlocked && codes > 0 && self.paint_row(ui, p, 0, Icon::Timer, "Codes", Some(codes), current == Some(View::Codes), None).clicked() {
            go = Some(View::Codes);
        }
        if unlocked {
            for f in &folders {
                let depth = f.matches('/').count();
                let name = f.rsplit('/').next().unwrap_or(f);
                let n = items_in(&svc.state(), &View::Folder(f.clone()), "").len();
                let view = View::Folder(f.clone());
                if self.paint_row(ui, p, depth, Icon::Folder, name, Some(n), current.as_ref() == Some(&view), None).clicked() {
                    go = Some(view);
                }
            }
            if deleted > 0 && self.paint_row(ui, p, 0, Icon::Trash, "Recently Deleted", Some(deleted), current == Some(View::Deleted), None).clicked() {
                go = Some(View::Deleted);
            }
        }
        if let Some(v) = go {
            self.open_passwords(v);
            self.focus = Pane::Sidebar;
        }
    }

    // ---- The list -----------------------------------------------------------

    pub(super) fn passwords_list(&mut self, ui: &mut Ui, p: &Palette) {
        let Some(view) = self.pw.view.clone() else { return };
        let full = ui.max_rect();
        let bar = Rect::from_min_size(full.min, Vec2::new(full.width(), TOOLBAR_H));
        ui.allocate_rect(bar, Sense::hover());
        let svc = self.pw.service.clone();
        let s = svc.state();
        let unlocked = s.unlocked();
        let rows: Vec<(String, String, String, Option<String>)> = items_in(&s, &view, &self.pw.search)
            .into_iter()
            .map(|i| {
                let sub = match (&view, totp::code_now(&i.totp)) {
                    (View::Codes, Some(c)) => format!("{}  ·  {}s", totp::spaced(&c.code), c.remaining),
                    (View::Deleted, _) => i.deleted.map(list_time).unwrap_or_default(),
                    _ if i.kind == ItemKind::Note => "Secure Note".into(),
                    _ => i.username.clone(),
                };
                (i.id.clone(), i.title(), sub, i.uris.first().map(|u| u.uri.clone()))
            })
            .collect();
        drop(s);
        self.pw.shown = rows.iter().map(|r| r.0.clone()).collect();
        if unlocked && self.pw.selected.as_ref().is_none_or(|s| !self.pw.shown.contains(s)) {
            self.pw.selected = self.pw.shown.first().cloned();
        }

        // The header: the view's name and count, as the notes list's.
        let y = bar.center().y;
        let mut x = bar.left() + 18.0;
        if !self.lib.settings.sidebar_visible {
            if super::sidebar::tool_button(ui, p, Rect::from_center_size(Pos2::new(x + 12.0, y), Vec2::splat(30.0)), Icon::Sidebar, "Show Folders (Ctrl+Alt+S)") {
                self.lib.settings.sidebar_visible = true;
            }
            x += 40.0;
        }
        let title = truncate(&view_title(&view), bar.right() - x - 16.0, ui);
        ui.painter().text(Pos2::new(x, y - 8.0), Align2::LEFT_CENTER, title, theme::bold_font(15.0), p.text);
        let n = rows.len();
        let count = if !unlocked {
            "Locked".to_owned()
        } else if n == 1 {
            "1 item".to_owned()
        } else {
            format!("{n} items")
        };
        ui.painter().text(Pos2::new(x, y + 11.0), Align2::LEFT_CENTER, count, theme::font(12.5), p.secondary);
        if !unlocked {
            return;
        }

        // Search the vault.
        let field = Rect::from_min_size(Pos2::new(full.left() + 14.0, bar.bottom() + 2.0), Vec2::new(full.width() - 28.0, 28.0));
        let te = ui.put(field, egui::TextEdit::singleline(&mut self.pw.search).hint_text("Search").margin(egui::vec2(8.0, 5.0)));
        if std::mem::take(&mut self.pw.focus_search) {
            te.request_focus();
        }
        if te.has_focus() && ui.input(|i| i.key_pressed(Key::Escape)) {
            self.pw.search.clear();
        }

        let area = Rect::from_min_max(Pos2::new(full.left(), field.bottom() + 8.0), full.max);
        let mut child = ui.new_child(egui::UiBuilder::new().max_rect(area).layout(egui::Layout::top_down(egui::Align::LEFT)));
        if rows.is_empty() {
            let msg = if self.pw.search.trim().is_empty() { "No Items" } else { "No Results" };
            child.painter().text(area.center() - Vec2::new(0.0, 40.0), Align2::CENTER_CENTER, msg, theme::font(15.0), p.secondary);
            return;
        }
        if view == View::Codes {
            ui.ctx().request_repaint_after(Duration::from_secs(1));
        }
        egui::ScrollArea::vertical().id_salt("passwords-list").auto_shrink([false, false]).show(&mut child, |ui| {
            ui.spacing_mut().item_spacing.y = 0.0;
            for (i, (id, title, sub, uri)) in rows.iter().enumerate() {
                let selected = self.pw.selected.as_deref() == Some(id);
                let next_selected = rows.get(i + 1).is_some_and(|r| self.pw.selected.as_deref() == Some(&r.0));
                let (rect, resp) = ui.allocate_exact_size(Vec2::new(ui.available_width(), ROW_H), Sense::click());
                let inner = rect.shrink2(Vec2::new(10.0, 0.0));
                let active = selected && self.focus == Pane::List;
                let painter = ui.painter();
                if selected {
                    painter.rect_filled(inner, 10.0, if active { p.selection } else { p.selection_inactive });
                } else if i + 1 < rows.len() && !next_selected {
                    painter.line_segment(
                        [Pos2::new(inner.left() + 52.0, rect.bottom() - 0.5), Pos2::new(inner.right() - 4.0, rect.bottom() - 0.5)],
                        egui::Stroke::new(1.0_f32, p.separator),
                    );
                }
                if let Some(item) = svc.state().item(id) {
                    item_badge(painter, Pos2::new(inner.left() + 26.0, rect.center().y), 15.0, item);
                }
                let text = p.on_selection(active);
                let subc = if active { theme::blend(p.selection, p.selection_text, 0.6) } else { p.secondary };
                let x = inner.left() + 52.0;
                let w = inner.right() - x - 10.0;
                painter.text(Pos2::new(x, rect.center().y - 9.0), Align2::LEFT_CENTER, truncate(title, w, ui), theme::bold_font(13.5), text);
                painter.text(Pos2::new(x, rect.center().y + 10.0), Align2::LEFT_CENTER, truncate(sub, w, ui), theme::font(12.5), subc);
                if selected && self.scroll_list {
                    ui.scroll_to_rect(rect, None);
                }
                if resp.clicked() || resp.secondary_clicked() {
                    self.select_password(Some(id.clone()));
                    self.focus = Pane::List;
                }
                self.password_menu(&resp, id, uri.as_deref());
            }
        });
        self.scroll_list = false;
    }

    /// An item's right-click menu.
    fn password_menu(&mut self, resp: &egui::Response, id: &str, uri: Option<&str>) {
        let Some(item) = self.pw.service.state().item(id).cloned() else { return };
        let code = totp::code_now(&item.totp);
        resp.context_menu(|ui| {
            menu::begin(ui, 210.0);
            let ctx = ui.ctx().clone();
            if !item.username.is_empty() && menu::item("Copy User Name").icon(Icon::Copy).show(ui).clicked() {
                self.copy_from_vault(&ctx, item.username.clone(), "the user name", false);
                ui.close_menu();
            }
            if !item.password.is_empty() && menu::item("Copy Password").icon(Icon::Copy).show(ui).clicked() {
                self.copy_from_vault(&ctx, item.password.clone(), "the password", true);
                ui.close_menu();
            }
            if let Some(c) = &code {
                if menu::item("Copy Verification Code").icon(Icon::Timer).show(ui).clicked() {
                    self.copy_from_vault(&ctx, c.code.clone(), "the verification code", false);
                    ui.close_menu();
                }
            }
            if let Some(u) = uri {
                menu::separator(ui);
                if menu::item("Open Website").icon(Icon::OpenApp).show(ui).clicked() {
                    let _ = open::that(website_url(u));
                    ui.close_menu();
                }
            }
        });
    }

    // ---- The item pane ------------------------------------------------------

    pub(super) fn password_pane(&mut self, ui: &mut Ui, p: &Palette) {
        let full = ui.max_rect();
        let bar = Rect::from_min_size(full.min, Vec2::new(full.width(), TOOLBAR_H));
        ui.allocate_rect(bar, Sense::hover());
        let svc = self.pw.service.clone();
        let (unlocked, busy, error) = {
            let s = svc.state();
            (s.unlocked(), s.busy, s.error.clone())
        };
        let y = bar.center().y;
        if unlocked {
            if glass_button(ui, p, Pos2::new(bar.right() - 32.0, y), Icon::LockOutline, "Lock CleanPassword", true).clicked() {
                self.lock_passwords();
                return;
            }
            let refreshing = busy == Some(Busy::Refreshing);
            if glass_button(ui, p, Pos2::new(bar.right() - 76.0, y), Icon::Sync, "Refresh", !refreshing).clicked() {
                svc.refresh();
            }
        }
        let area = Rect::from_min_max(Pos2::new(full.left(), bar.bottom()), full.max);
        let mut child = ui.new_child(egui::UiBuilder::new().max_rect(area).layout(egui::Layout::top_down(egui::Align::LEFT)));
        if !unlocked {
            self.vault_lock_screen(&mut child, p, area, busy, error);
            return;
        }
        let Some(mut item) = self.pw.selected.as_ref().and_then(|id| svc.state().item(id).cloned()) else {
            let msg = if self.pw.shown.is_empty() { "" } else { "No Item Selected" };
            child.painter().text(area.center(), Align2::CENTER_CENTER, msg, theme::font(15.0), p.secondary);
            return;
        };
        egui::ScrollArea::vertical().id_salt(("password", &item.id)).auto_shrink([false, false]).show(&mut child, |ui| {
            self.item_details(ui, p, &item, error.as_deref());
        });
        item.history.iter_mut().for_each(|h| zeroize::Zeroize::zeroize(&mut h.password));
        zeroize::Zeroize::zeroize(&mut item.password);
    }

    /// "CleanPassword is locked": the master password field.
    fn vault_lock_screen(&mut self, ui: &mut Ui, p: &Palette, area: Rect, busy: Option<Busy>, error: Option<String>) {
        let c = Pos2::new(area.center().x, area.top() + area.height() * 0.32);
        icons::paint(ui.painter(), Icon::Key, Rect::from_center_size(c, Vec2::splat(54.0)), p.secondary);
        ui.painter().text(c + Vec2::new(0.0, 56.0), Align2::CENTER_CENTER, "CleanPassword is locked.", theme::bold_font(17.0), p.text);
        ui.painter().text(c + Vec2::new(0.0, 82.0), Align2::CENTER_CENTER, "Enter the master password to view your passwords.", theme::font(13.0), p.secondary);
        let unlocking = busy == Some(Busy::Unlocking);
        let field = Rect::from_center_size(c + Vec2::new(0.0, 122.0), Vec2::new(260.0, 30.0));
        let te = ui.add_enabled_ui(!unlocking, |ui| {
            ui.put(field, egui::TextEdit::singleline(&mut *self.pw.typed).password(true).hint_text("Master Password").margin(egui::vec2(8.0, 6.0)))
        });
        let te = te.inner;
        if std::mem::take(&mut self.pw.focus_password) && !unlocking {
            te.request_focus();
        }
        if te.gained_focus() {
            self.focus = Pane::Note;
        }
        let enter = te.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));
        let btn = Rect::from_center_size(c + Vec2::new(0.0, 166.0), Vec2::new(120.0, 30.0));
        if unlocking {
            ui.put(btn, egui::Spinner::new().size(18.0));
            ui.painter().text(c + Vec2::new(0.0, 196.0), Align2::CENTER_CENTER, "Unlocking…", theme::font(12.5), p.secondary);
            return;
        }
        let b = egui::Button::new(RichText::new("Unlock").color(egui::Color32::WHITE)).fill(theme::BLUE).rounding(8.0);
        let clicked = ui.put(btn, b).clicked();
        if (enter || clicked) && !self.pw.typed.is_empty() {
            let password = std::mem::take(&mut *self.pw.typed);
            self.pw.service.unlock(password);
            self.pw.focus_password = true;
        }
        if let Some(e) = error {
            let g = ui.painter().layout(e, theme::font(12.5), theme::RED, 360.0);
            ui.painter().galley(c + Vec2::new(-g.size().x / 2.0, 192.0), g, theme::RED);
        }
    }

    fn item_details(&mut self, ui: &mut Ui, p: &Palette, item: &Item, error: Option<&str>) {
        let width = (ui.available_width() - 48.0).clamp(240.0, 600.0);
        let margin = ((ui.available_width() - width) / 2.0).max(24.0);
        let ctx = ui.ctx().clone();
        ui.horizontal(|ui| {
            ui.add_space(margin);
            ui.vertical(|ui| {
                ui.set_width(width);
                ui.spacing_mut().item_spacing.y = 6.0;
                ui.add_space(18.0);
                if let Some(e) = error {
                    ui.colored_label(theme::RED, format!("Couldn't read the vault again: {e}"));
                }
                if item.deleted.is_some() {
                    ui.label(RichText::new("This item is in CleanPassword's Recently Deleted.").color(p.secondary).size(12.5));
                }
                // Badge, name and date.
                ui.horizontal(|ui| {
                    let (r, _) = ui.allocate_exact_size(Vec2::splat(56.0), Sense::hover());
                    item_badge(ui.painter(), r.center(), 28.0, item);
                    ui.add_space(8.0);
                    ui.vertical(|ui| {
                        ui.add_space(6.0);
                        ui.label(RichText::new(item.title()).font(theme::bold_font(22.0)).color(p.text));
                        let when = item.modified.map(|m| format!("Last modified {}", note_header_time(m))).unwrap_or_default();
                        ui.label(RichText::new(when).size(12.5).color(p.secondary));
                    });
                });
                ui.add_space(10.0);

                let mut copy: Option<(String, &'static str, bool)> = None;
                let mut toggle: Option<String> = None;
                let revealed = self.pw.revealed.clone();
                let shown = |k: &str| revealed.contains(k);

                if item.kind != ItemKind::Note || !item.username.is_empty() || !item.password.is_empty() {
                    group(ui, p, |ui| {
                        let mut first = true;
                        let mut sep = |ui: &mut Ui| {
                            if !std::mem::take(&mut first) {
                                divider(ui, p);
                            }
                        };
                        if !item.username.is_empty() {
                            sep(ui);
                            if field_row(ui, p, "User Name", &item.username, false, None).copy {
                                copy = Some((item.username.clone(), "the user name", false));
                            }
                        }
                        if !item.password.is_empty() {
                            sep(ui);
                            let out = field_row(ui, p, "Password", &item.password, true, Some(shown("password")));
                            if out.copy {
                                copy = Some((item.password.clone(), "the password", true));
                            }
                            if out.toggle {
                                toggle = Some("password".into());
                            }
                        }
                        if let Some(c) = totp::code_now(&item.totp) {
                            sep(ui);
                            let shown_code = format!("{}   {}s", totp::spaced(&c.code), c.remaining);
                            if field_row(ui, p, "Verification Code", &shown_code, true, None).copy {
                                copy = Some((c.code.clone(), "the verification code", false));
                            }
                            ui.ctx().request_repaint_after(Duration::from_secs(1));
                        }
                    });
                }

                if !item.uris.is_empty() {
                    group(ui, p, |ui| {
                        caption(ui, p, if item.uris.len() == 1 { "Website" } else { "Websites" });
                        for u in &item.uris {
                            let label = passwords::host(&u.uri).unwrap_or_else(|| u.uri.clone());
                            ui.hyperlink_to(RichText::new(label).size(14.0), website_url(&u.uri)).on_hover_text(&u.uri);
                        }
                        ui.add_space(4.0);
                    });
                }

                if !item.passkeys.is_empty() {
                    group(ui, p, |ui| {
                        for (i, k) in item.passkeys.iter().enumerate() {
                            if i > 0 {
                                divider(ui, p);
                            }
                            let who = if k.user_name.is_empty() { &k.user_display_name } else { &k.user_name };
                            let value = if who.is_empty() { k.rp_id.clone() } else { format!("{who} · {}", k.rp_id) };
                            field_row(ui, p, "Passkey", &value, false, None);
                        }
                    });
                }

                if !item.fields.is_empty() {
                    group(ui, p, |ui| {
                        for (i, f) in item.fields.iter().enumerate() {
                            if i > 0 {
                                divider(ui, p);
                            }
                            let key = format!("field {i}");
                            let out = field_row(ui, p, &f.name, &f.value, f.hidden, f.hidden.then(|| shown(&key)));
                            if out.copy {
                                copy = Some((f.value.clone(), "the field", f.hidden));
                            }
                            if out.toggle {
                                toggle = Some(key);
                            }
                        }
                    });
                }

                if !item.notes.trim().is_empty() {
                    group(ui, p, |ui| {
                        caption(ui, p, "Notes");
                        ui.add(egui::Label::new(RichText::new(item.notes.trim_end()).size(14.0).color(p.text)).selectable(true).wrap());
                        ui.add_space(4.0);
                    });
                }

                if !item.history.is_empty() {
                    group(ui, p, |ui| {
                        let open = shown("history");
                        ui.horizontal(|ui| {
                            caption(ui, p, &format!("Earlier Passwords ({})", item.history.len()));
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                if ui.small_button(if open { "Hide" } else { "Show" }).clicked() {
                                    toggle = Some("history".into());
                                }
                            });
                        });
                        if open {
                            for h in item.history.iter().rev() {
                                divider(ui, p);
                                let when = h.retired.map(|r| format!("Until {}", list_time(r))).unwrap_or_else(|| "Earlier".into());
                                if field_row(ui, p, &when, &h.password, true, Some(true)).copy {
                                    copy = Some((h.password.clone(), "the earlier password", true));
                                }
                            }
                        }
                        ui.add_space(4.0);
                    });
                }

                ui.add_space(6.0);
                ui.label(RichText::new("Shown read-only from your CleanPassword vault. Edit items in CleanPassword.").size(12.0).color(p.tertiary));
                ui.add_space(24.0);

                if let Some((text, what, secret)) = copy {
                    self.copy_from_vault(&ctx, text, what, secret);
                }
                if let Some(k) = toggle {
                    if !self.pw.revealed.remove(&k) {
                        self.pw.revealed.insert(k);
                    }
                    self.pw.service.touch();
                }
            });
        });
    }

    // ---- Keys ---------------------------------------------------------------

    /// The keyboard while CleanPassword is shown. Returns false to leave
    /// it to the notes' shortcuts.
    pub(super) fn passwords_shortcuts(&mut self, ctx: &egui::Context) -> bool {
        if self.pw.view.is_none() {
            return false;
        }
        let cmd = Modifiers::COMMAND;
        let pressed = |m: Modifiers, k: Key| ctx.input_mut(|i| consume_exact(i, m, k));
        if pressed(cmd, Key::N) {
            // Back to the notes, for a new one there.
            self.pw.view = None;
            self.new_note();
            return true;
        }
        if pressed(cmd, Key::F) || pressed(cmd | Modifiers::ALT, Key::F) {
            self.pw.focus_search = true;
        }
        if pressed(cmd, Key::Comma) {
            self.settings_ui.open = true;
        }
        if pressed(cmd | Modifiers::ALT, Key::S) {
            self.lib.settings.sidebar_visible = !self.lib.settings.sidebar_visible;
        }
        if self.focus == Pane::List && !ctx.wants_keyboard_input() {
            let step = if pressed(Modifiers::NONE, Key::ArrowDown) {
                1
            } else if pressed(Modifiers::NONE, Key::ArrowUp) {
                -1
            } else {
                0
            };
            if step != 0 && !self.pw.shown.is_empty() {
                let at = self.pw.selected.as_ref().and_then(|s| self.pw.shown.iter().position(|x| x == s));
                let next = match at {
                    Some(i) => (i as i32 + step).clamp(0, self.pw.shown.len() as i32 - 1) as usize,
                    None => 0,
                };
                self.select_password(Some(self.pw.shown[next].clone()));
                self.scroll_list = true;
            }
        }
        true
    }
}

/// What was clicked in a field's row.
#[derive(Default)]
struct RowOut {
    copy: bool,
    toggle: bool,
}

/// A label above a value, with a copy button (and, for a secret, a button
/// to show it: `reveal` is whether it's shown).
fn field_row(ui: &mut Ui, p: &Palette, label: &str, value: &str, mono: bool, reveal: Option<bool>) -> RowOut {
    let mut out = RowOut::default();
    ui.horizontal(|ui| {
        ui.vertical(|ui| {
            ui.add_space(6.0);
            caption(ui, p, label);
            let font = egui::FontId::new(14.0, theme::family(false, false, mono));
            match reveal {
                Some(false) => {
                    ui.label(RichText::new("••••••••••").font(egui::FontId::new(14.0, egui::FontFamily::Proportional)).color(p.text));
                }
                _ => {
                    ui.add(egui::Label::new(RichText::new(value).font(font).color(p.text)).selectable(true).wrap());
                }
            }
            ui.add_space(6.0);
        });
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if icon_button(ui, p, Icon::Copy, &format!("Copy {label}")) {
                out.copy = true;
            }
            if let Some(shown) = reveal {
                let tip = if shown { format!("Hide {label}") } else { format!("Show {label}") };
                if icon_button(ui, p, Icon::Eye, &tip) {
                    out.toggle = true;
                }
            }
        });
    });
    out
}

fn icon_button(ui: &mut Ui, p: &Palette, icon: Icon, tip: &str) -> bool {
    let (rect, _) = ui.allocate_exact_size(Vec2::splat(28.0), Sense::hover());
    super::sidebar::tool_button(ui, p, rect, icon, tip)
}

fn caption(ui: &mut Ui, p: &Palette, text: &str) {
    ui.label(RichText::new(text).size(12.0).color(p.secondary));
}

fn divider(ui: &mut Ui, p: &Palette) {
    let (r, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 1.0), Sense::hover());
    ui.painter().line_segment([r.left_center(), r.right_center()], egui::Stroke::new(1.0_f32, p.separator));
}

/// A rounded card of rows, as in Apple's Passwords.
fn group(ui: &mut Ui, p: &Palette, add: impl FnOnce(&mut Ui)) {
    egui::Frame::none()
        .fill(p.card)
        .stroke(egui::Stroke::new(1.0_f32, p.separator))
        .rounding(12.0)
        .inner_margin(egui::Margin::symmetric(14.0, 4.0))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            add(ui);
        });
    ui.add_space(8.0);
}

/// A saved website as a URL to open (`github.com` → `https://github.com`).
fn website_url(uri: &str) -> String {
    let u = uri.trim();
    if u.contains("://") {
        u.to_owned()
    } else {
        format!("https://{u}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn notes_take_the_panes_back_from_the_vault() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = App::for_test(dir.path());
        app.open_passwords(View::All);
        assert_eq!(app.pw.view, Some(View::All));
        let loc = app.location.clone();
        app.go_to(loc);
        assert_eq!(app.pw.view, None);
    }

    fn found() -> passwords::Found {
        let header = passwords::VaultHeader {
            format: "cleanpassword-vault".into(),
            version: 1,
            vault_id: "v1".into(),
            kdf: passwords::KdfParams { alg: "argon2id".into(), memory_kib: 8, passes: 1, lanes: 1, salt: String::new() },
            wrapped_key: String::new(),
            created: chrono::Utc::now(),
        };
        let config = cleannotes_core::sync::s3::Config {
            bucket: "b".into(),
            region: "us-east-1".into(),
            prefix: "CleanPassword".into(),
            endpoint: None,
            encrypted: false,
        };
        passwords::Found { account: "acct".into(), config, header }
    }

    fn items() -> Vec<Item> {
        use passwords::{Field, HistoryEntry, ItemUri, Passkey};
        let now = Some(chrono::Utc::now());
        vec![
            Item {
                id: "gh".into(),
                name: "GitHub".into(),
                username: "octocat".into(),
                password: "s3cret-pw".into(),
                uris: vec![ItemUri { uri: "https://github.com/login".into() }],
                totp: "JBSWY3DPEHPK3PXP".into(),
                notes: "Recovery codes in the safe.".into(),
                folder: "Work".into(),
                fields: vec![Field { name: "PIN".into(), value: "4321".into(), hidden: true }],
                history: vec![HistoryEntry { password: "old-pw".into(), retired: now }],
                passkeys: vec![Passkey { rp_id: "github.com".into(), user_name: "octocat".into(), ..Default::default() }],
                modified: now,
                ..Default::default()
            },
            Item { id: "bank".into(), name: "Bank".into(), username: "me".into(), password: "x".into(), folder: "Money/Banks".into(), ..Default::default() },
            Item { id: "wifi".into(), kind: ItemKind::Note, name: "Wi-Fi".into(), notes: "swordfish".into(), ..Default::default() },
            Item { id: "old".into(), name: "Old".into(), deleted: now, ..Default::default() },
        ]
    }

    /// One frame of the window, as [`App::frame`] lays it out.
    fn frame(app: &mut App, ctx: &egui::Context, events: Vec<egui::Event>) {
        let raw = egui::RawInput {
            events,
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(1200.0, 800.0))),
            ..Default::default()
        };
        let _ = ctx.run(raw, |ctx| {
            theme::apply_style(ctx);
            let p = Palette::get(ctx);
            app.shortcuts(ctx);
            egui::SidePanel::left("sidebar").show(ctx, |ui| app.sidebar(ui, &p));
            let vault = app.pw.view.is_some();
            egui::SidePanel::left("list").show(ctx, |ui| if vault { app.passwords_list(ui, &p) } else { app.notes_list(ui, &p) });
            egui::CentralPanel::default().show(ctx, |ui| if vault { app.password_pane(ui, &p) } else { app.note_pane(ui, &p) });
        });
    }

    fn key(key: Key, modifiers: Modifiers) -> egui::Event {
        egui::Event::Key { key, physical_key: None, pressed: true, repeat: false, modifiers }
    }

    #[test]
    fn the_vault_is_shown_locked_then_unlocked_in_every_view() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = App::for_test(dir.path());
        let ctx = egui::Context::default();
        theme::install_fonts(&ctx);

        // Locked: the section, "Locked" in the list, and the master
        // password field with the keyboard.
        app.pw.service.set_for_test(found(), None);
        app.open_passwords(View::All);
        frame(&mut app, &ctx, vec![]);
        frame(&mut app, &ctx, vec![]);
        assert!(ctx.wants_keyboard_input(), "the master password field takes the keyboard");
        assert!(app.pw.shown.is_empty());

        // Unlocked: the items by name, Recently Deleted apart.
        app.pw.service.set_for_test(found(), Some(items()));
        app.open_passwords(View::Codes);
        app.open_passwords(View::All);
        app.focus = Pane::List;
        ctx.memory_mut(|m| m.stop_text_input());
        frame(&mut app, &ctx, vec![]);
        assert_eq!(app.pw.shown, ["bank", "gh", "wifi"]);
        assert_eq!(app.pw.selected.as_deref(), Some("bank"));
        frame(&mut app, &ctx, vec![key(Key::ArrowDown, Modifiers::NONE)]);
        assert_eq!(app.pw.selected.as_deref(), Some("gh"));
        // Its details, with the secrets shown too.
        app.pw.revealed.extend(["password".to_owned(), "field 0".to_owned(), "history".to_owned()]);
        frame(&mut app, &ctx, vec![]);
        app.pw.search = "git".into();
        frame(&mut app, &ctx, vec![]);
        assert_eq!(app.pw.shown, ["gh"]);

        for (view, shown) in [
            (View::Codes, vec!["gh"]),
            (View::Folder("Money".into()), vec!["bank"]),
            (View::Folder("Money/Banks".into()), vec!["bank"]),
            (View::Deleted, vec!["old"]),
        ] {
            app.open_passwords(view.clone());
            frame(&mut app, &ctx, vec![]);
            frame(&mut app, &ctx, vec![]);
            assert_eq!(app.pw.shown, shown, "{view:?}");
            assert_eq!(app.pw.selected.as_deref(), Some(shown[0]));
        }

        // Copying a password remembers it, to clear the clipboard later.
        app.copy_from_vault(&ctx, "s3cret-pw".into(), "the password", true);
        assert!(app.pw.copied.is_some());

        // Lock Now locks the vault too.
        app.lock_now();
        assert!(!app.pw.service.state().unlocked());
        assert!(app.pw.revealed.is_empty());
        frame(&mut app, &ctx, vec![]);

        // ⌘N goes back to the notes for a new one.
        frame(&mut app, &ctx, vec![key(Key::N, Modifiers::COMMAND)]);
        assert_eq!(app.pw.view, None);
        assert!(app.selected.is_some());
        frame(&mut app, &ctx, vec![]);
    }

    #[test]
    fn websites_open_as_https() {
        assert_eq!(website_url("github.com"), "https://github.com");
        assert_eq!(website_url(" https://x.org/a "), "https://x.org/a");
        assert_eq!(view_title(&View::Folder("Money/Banks".into())), "Banks");
    }
}
