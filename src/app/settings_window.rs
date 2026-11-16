//! The Settings window, laid out like Apple Notes' (General) plus an
//! Accounts tab for sync: add (+) or remove (−) accounts: a folder in an
//! S3 bucket, or point at a SQLite database.

use std::sync::mpsc::{self, Receiver};

use eframe::egui::{self, Align2, Key, Pos2, Rect, RichText, Sense, Ui, Vec2};

use super::{modal, App, Confirm, Location};
use crate::model::{new_id, Account, LOCAL};
use crate::settings::{refresh_label, SortBy, StartWith, REFRESH_CHOICES, TEXT_SIZES};
use crate::sync::{s3, sqlite, BackendConfig};
use crate::theme::{self, Palette};

const LABEL_W: f32 = 190.0;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Tab {
    #[default]
    General,
    Accounts,
}

struct SqliteForm {
    name: String,
    path: String,
}

struct S3Form {
    /// The account whose keys are being replaced.
    again: Option<String>,
    name: String,
    bucket: String,
    region: String,
    prefix: String,
    endpoint: String,
    access_key_id: String,
    secret_access_key: String,
    /// Optional: encrypts the folder's notes with a key made from it.
    passphrase: String,
}

impl S3Form {
    fn config(&self) -> s3::Config {
        let endpoint = self.endpoint.trim().trim_end_matches('/');
        s3::Config {
            bucket: self.bucket.trim().to_owned(),
            region: self.region.trim().to_owned(),
            prefix: self.prefix.trim().trim_matches('/').to_owned(),
            endpoint: (!endpoint.is_empty()).then(|| endpoint.to_owned()),
            encrypted: false,
        }
    }

    fn keys(&self) -> s3::Keys {
        s3::Keys { access_key_id: self.access_key_id.trim().to_owned(), secret_access_key: self.secret_access_key.trim().to_owned(), encryption_key: None }
    }

    fn ready(&self) -> bool {
        let endpoint = self.endpoint.trim();
        !self.bucket.trim().is_empty()
            && !self.region.trim().is_empty()
            && !self.access_key_id.trim().is_empty()
            && !self.secret_access_key.trim().is_empty()
            && (endpoint.is_empty() || endpoint.starts_with("http"))
    }
}

enum Sheet {
    Choose,
    Sqlite(SqliteForm),
    S3(Box<S3Form>),
}

/// A sign-in running in the background.
enum Pending {
    // Each with the config the sign-in used: the account is saved with that,
    // not with the form as it is by the time the reply comes (an edited
    // server URL would get this session's tokens).
    S3(Receiver<(s3::Config, Result<String, String>)>),
}

#[derive(Default)]
pub struct SettingsUi {
    pub open: bool,
    tab: Tab,
    selected: Option<String>,
    sheet: Option<Sheet>,
    pending: Option<Pending>,
    error: Option<String>,
}

impl SettingsUi {
    pub fn open_account(&mut self, id: &str) {
        self.open = true;
        self.tab = Tab::Accounts;
        self.selected = Some(id.to_owned());
    }
}

impl App {
    pub(super) fn settings_window(&mut self, ctx: &egui::Context, p: &Palette) {
        if !self.settings_ui.open {
            return;
        }
        let mut open = true;
        egui::Window::new(match self.settings_ui.tab {
            Tab::General => "General",
            Tab::Accounts => "Accounts",
        })
        .id(egui::Id::new("settings"))
        .open(&mut open)
        .collapsible(false)
        .resizable(false)
        .default_pos(ctx.screen_rect().center() - Vec2::new(280.0, 220.0))
        .fixed_size(Vec2::new(600.0, 560.0))
        .frame(egui::Frame::window(&ctx.style()).fill(p.popover).inner_margin(egui::Margin::same(16.0)))
        .show(ctx, |ui| {
            self.tab_bar(ui, p);
            ui.add_space(10.0);
            match self.settings_ui.tab {
                Tab::General => self.general_tab(ui, p),
                Tab::Accounts => self.accounts_tab(ui, p),
            }
        });
        if !open || (ctx.input(|i| i.key_pressed(Key::Escape)) && self.settings_ui.sheet.is_none()) {
            self.settings_ui.open = false;
        }
        self.poll_sign_in(ctx);
        self.account_sheet(ctx, p);
    }

    fn tab_bar(&mut self, ui: &mut Ui, p: &Palette) {
        ui.horizontal(|ui| {
            ui.add_space(ui.available_width() / 2.0 - 90.0);
            for (tab, icon, label) in [(Tab::General, crate::icons::Icon::Gear, "General"), (Tab::Accounts, crate::icons::Icon::Sync, "Accounts")] {
                let (r, resp) = ui.allocate_exact_size(Vec2::new(84.0, 50.0), Sense::click());
                let on = self.settings_ui.tab == tab;
                if on || resp.hovered() {
                    ui.painter().rect_filled(r, 7.0, p.hover);
                }
                let color = if on { p.accent } else { p.secondary };
                crate::icons::paint(ui.painter(), icon, Rect::from_center_size(Pos2::new(r.center().x, r.top() + 17.0), Vec2::splat(20.0)), color);
                ui.painter().text(Pos2::new(r.center().x, r.bottom() - 10.0), Align2::CENTER_CENTER, label, theme::font(11.5), if on { p.text } else { p.secondary });
                if resp.clicked() {
                    self.settings_ui.tab = tab;
                }
            }
        });
        ui.separator();
    }

    fn general_tab(&mut self, ui: &mut Ui, p: &Palette) {
        let accounts: Vec<(String, String)> = self.lib.store.accounts.iter().filter(|a| a.enabled).map(|a| (a.id.clone(), a.name.clone())).collect();
        let s = &mut self.lib.settings;
        ui.spacing_mut().item_spacing.y = 8.0;
        form_row(ui, p, "Sort notes by:", |ui| {
            let opts: Vec<(SortBy, String)> = SortBy::ALL.iter().map(|o| (*o, o.label().to_owned())).collect();
            combo(ui, "sort-by", &mut s.sort_by, &opts);
        });
        form_row(ui, p, "New notes start with:", |ui| {
            let opts: Vec<(StartWith, String)> = StartWith::ALL.iter().map(|o| (*o, o.label().to_owned())).collect();
            combo(ui, "start-with", &mut s.start_with, &opts);
        });
        form_row(ui, p, "Default account:", |ui| {
            combo(ui, "default-account", &mut s.default_account, &accounts);
        });
        explain(ui, p, "New notes go to this account when no folder of another account is selected.");
        ui.add_space(4.0);
        check_row(ui, p, &mut s.group_by_date, "Group notes by date", "When sorted by Date Edited or Date Created, group notes by date.");
        check_row(ui, p, &mut s.sort_checked, "Automatically sort checked items", "Automatically move checklist items to the bottom of the list as they are checked.");
        check_row(ui, p, &mut s.show_note_count, "Show note count", "Show how many notes each folder has in the sidebar.");
        ui.separator();
        form_row(ui, p, "Default text size:", |ui| {
            let mut i = TEXT_SIZES.iter().position(|&t| t >= s.text_size).unwrap_or(3);
            ui.label(RichText::new("A").size(11.0));
            ui.spacing_mut().slider_width = 270.0;
            ui.visuals_mut().selection.bg_fill = p.folder_selection;
            if ui.add(egui::Slider::new(&mut i, 0..=TEXT_SIZES.len() - 1).show_value(false).trailing_fill(true)).changed() {
                s.text_size = TEXT_SIZES[i];
            }
            ui.label(RichText::new("A").size(19.0));
        });
        ui.separator();
        form_row(ui, p, "Check for changes:", |ui| {
            let opts: Vec<(u32, String)> = REFRESH_CHOICES.iter().map(|&m| (m, refresh_label(m))).collect();
            combo(ui, "refresh", &mut s.refresh_minutes, &opts);
        });
        explain(ui, p, "How often synced accounts look for changes made on other devices.");
        ui.separator();
        let has_password = self.lib.settings.lock_check.is_some();
        let mut change = false;
        let mut lock_now = false;
        let unlocked = self.has_unlocked();
        form_row(ui, p, "Locked notes:", |ui| {
            if ui.button(if has_password { "Change Password…" } else { "Set Password…" }).clicked() {
                change = true;
            }
            if ui.add_enabled(unlocked, egui::Button::new("Lock Now")).clicked() {
                lock_now = true;
            }
        });
        explain(ui, p, "Locked notes are encrypted with this password, also when synced. Unlocked notes lock again after 15 minutes without use and when you quit.");
        if change {
            self.open_change_password();
        }
        if lock_now {
            self.lock_now();
        }
        ui.separator();
        let mut import = false;
        form_row(ui, p, "Import:", |ui| {
            if ui.button("Import from Evernote…").clicked() {
                import = true;
            }
        });
        explain(ui, p, "Adds notes from Evernote export files (.enex), also those made from Apple Notes. You can also drop the files on the window.");
        if import {
            self.settings_ui.open = false;
            self.open_import(&[]);
        }
    }

    fn accounts_tab(&mut self, ui: &mut Ui, p: &Palette) {
        if self.settings_ui.selected.as_ref().map_or(true, |s| self.lib.store.account(s).is_none()) {
            self.settings_ui.selected = Some(LOCAL.to_owned());
        }
        let selected = self.settings_ui.selected.clone().unwrap();
        ui.horizontal_top(|ui| {
            // The account list, with + and − below.
            ui.vertical(|ui| {
                ui.set_width(170.0);
                egui::Frame::none().fill(p.bg).rounding(6.0).stroke(egui::Stroke::new(1.0_f32, p.separator)).show(ui, |ui| {
                    ui.set_min_size(Vec2::new(170.0, 280.0));
                    ui.spacing_mut().item_spacing.y = 0.0;
                    let accounts: Vec<(String, String, String)> =
                        self.lib.store.accounts.iter().map(|a| (a.id.clone(), a.name.clone(), a.backend.kind_name().to_owned())).collect();
                    for (id, name, kind) in accounts {
                        let (r, resp) = ui.allocate_exact_size(Vec2::new(170.0, 40.0), Sense::click());
                        if id == selected {
                            ui.painter().rect_filled(r.shrink(2.0), 5.0, p.selection);
                        }
                        ui.painter().text(Pos2::new(r.left() + 10.0, r.top() + 13.0), Align2::LEFT_CENTER, &name, theme::bold_font(13.0), p.text);
                        let sub = if id == LOCAL { "Only on this computer".to_owned() } else { kind };
                        ui.painter().text(Pos2::new(r.left() + 10.0, r.top() + 29.0), Align2::LEFT_CENTER, sub, theme::font(11.0), p.secondary);
                        if resp.clicked() {
                            self.settings_ui.selected = Some(id.clone());
                        }
                    }
                });
                ui.horizontal(|ui| {
                    if ui.button(" + ").on_hover_text("Add Account").clicked() {
                        self.settings_ui.sheet = Some(Sheet::Choose);
                        self.settings_ui.error = None;
                    }
                    let can_remove = selected != LOCAL;
                    if ui.add_enabled(can_remove, egui::Button::new(" − ")).on_hover_text("Remove Account").clicked() {
                        self.confirm = Some(Confirm::RemoveAccount(selected.clone()));
                    }
                });
            });
            ui.add_space(12.0);
            ui.vertical(|ui| self.account_details(ui, p, &selected));
        });
    }

    fn account_details(&mut self, ui: &mut Ui, p: &Palette, id: &str) {
        let Some(a) = self.lib.store.account(id).cloned() else { return };
        if id == LOCAL {
            ui.label(RichText::new(&a.name).font(theme::bold_font(16.0)));
            ui.add_space(6.0);
            wrapped(ui, "Notes in this account are kept only on this computer, in the app's data folder.", p.secondary);
            ui.add_space(6.0);
            wrapped(ui, &format!("Data folder: {}", crate::db::data_dir().display()), p.secondary);
            return;
        }
        let mut name = a.name.clone();
        let mut enabled = a.enabled;
        if self.lib.db.secret_in_memory_only(id) {
            wrapped(
                ui,
                "The system's credential store isn't available: this sign-in is kept only until CleanNotes quits, so you'll sign in again next time.",
                p.secondary,
            );
            ui.add_space(6.0);
        }
        egui::Grid::new("account-grid").num_columns(2).spacing([10.0, 8.0]).show(ui, |ui| {
            ui.label("Description:");
            ui.add(egui::TextEdit::singleline(&mut name).desired_width(200.0));
            ui.end_row();
            ui.label("");
            ui.checkbox(&mut enabled, "Enable this account");
            ui.end_row();
            match &a.backend {
                BackendConfig::Sqlite(c) => {
                    ui.label("Database:");
                    ui.label(c.path.display().to_string());
                    ui.end_row();
                }
                BackendConfig::S3(c) => {
                    ui.label("Bucket:");
                    ui.label(&c.bucket);
                    ui.end_row();
                    ui.label("Region:");
                    ui.label(&c.region);
                    ui.end_row();
                    ui.label("Folder:");
                    ui.label(if c.prefix.is_empty() { "(top of the bucket)" } else { c.prefix.as_str() });
                    ui.end_row();
                    if let Some(e) = &c.endpoint {
                        ui.label("Endpoint:");
                        ui.label(e);
                        ui.end_row();
                    }
                    ui.label("Encryption:");
                    ui.label(if c.encrypted { "On (with your encryption key)" } else { "Off" });
                    ui.end_row();
                }
                BackendConfig::Local => {}
            }
            ui.label("Status:");
            let status = if self.syncing(id) {
                "Syncing…".to_owned()
            } else if self.lib.needs_sign_in.contains(id) {
                "Needs you to sign in again".to_owned()
            } else if let Some(e) = self.lib.sync_errors.get(id) {
                format!("Couldn't sync: {e}")
            } else if let Some(t) = a.last_sync {
                format!("Synced {}", super::list_time(t))
            } else {
                "Not synced yet".to_owned()
            };
            ui.add(egui::Label::new(RichText::new(status).color(p.secondary)).wrap());
            ui.end_row();
        });
        ui.add_space(10.0);
        ui.horizontal(|ui| {
            if ui.add_enabled(enabled && !self.syncing(id), egui::Button::new("Sync Now")).clicked() {
                self.sync_now(id);
            }
            match &a.backend {
                BackendConfig::S3(c) => {
                    if ui.button("Change Access Key…").clicked() {
                        self.settings_ui.sheet = Some(Sheet::S3(Box::new(S3Form {
                            again: Some(id.to_owned()),
                            name: a.name.clone(),
                            bucket: c.bucket.clone(),
                            region: c.region.clone(),
                            prefix: c.prefix.clone(),
                            endpoint: c.endpoint.clone().unwrap_or_default(),
                            access_key_id: String::new(),
                            secret_access_key: String::new(),
                            passphrase: String::new(),
                        })));
                        self.settings_ui.error = None;
                    }
                }
                _ => {}
            }
        });
        if name != a.name || enabled != a.enabled {
            let disabled = a.enabled && !enabled;
            let acc = self.lib.store.account_mut(id).unwrap();
            acc.name = name;
            acc.enabled = enabled;
            if disabled {
                // Its notes vanish from view: don't stay in one of its
                // folders, or keep one of its notes open.
                let inside = match &self.location {
                    Location::Folder(f) => self.lib.store.folder(f).is_some_and(|f| f.account == id),
                    Location::Deleted(acc) => acc == id,
                    _ => false,
                };
                if inside {
                    let (_, folder) = self.new_note_target();
                    self.go_to(Location::Folder(folder));
                }
                self.retain_visible(None);
            }
        }
    }

    fn poll_sign_in(&mut self, ctx: &egui::Context) {
        let Some(pending) = &self.settings_ui.pending else { return };
        ctx.request_repaint_after(std::time::Duration::from_millis(100));
        match pending {
            Pending::S3(rx) => {
                let Ok((config, r)) = rx.try_recv() else { return };
                self.settings_ui.pending = None;
                let Some(Sheet::S3(form)) = &self.settings_ui.sheet else { return };
                match r {
                    Ok(secret) => {
                        let (id, name) = match &form.again {
                            Some(id) => (id.clone(), form.name.clone()),
                            None => (new_id(), if form.name.trim().is_empty() { "S3".to_owned() } else { form.name.trim().to_owned() }),
                        };
                        let account = self.account_for(&id, name, BackendConfig::S3(config));
                        self.settings_ui.sheet = None;
                        self.settings_ui.selected = Some(id);
                        self.add_account(account, Some(secret));
                    }
                    Err(e) => self.settings_ui.error = Some(e),
                }
            }
        }
    }

    /// The account to save after signing in: the existing one (signing in
    /// again keeps its sync state) or a new one.
    fn account_for(&self, id: &str, name: String, backend: BackendConfig) -> Account {
        self.lib.account_for(id, name, backend)
    }

    fn account_sheet(&mut self, ctx: &egui::Context, p: &Palette) {
        if self.settings_ui.sheet.is_none() {
            return;
        }
        let busy = self.settings_ui.pending.is_some();
        let error = self.settings_ui.error.clone();
        let mut close = false;
        let mut next: Option<Sheet> = None;
        let mut add_sqlite: Option<(String, String)> = None;
        let mut start_s3: Option<(s3::Config, s3::Keys, String)> = None;
        let sheet = self.settings_ui.sheet.as_mut().unwrap();
        modal(ctx, "add-account", p, |ui| {
            ui.set_width(400.0);
            match sheet {
                Sheet::Choose => {
                    ui.label(RichText::new("Choose where to sync notes…").font(theme::bold_font(14.0)));
                    ui.add_space(10.0);
                    let choice = |ui: &mut Ui, title: &str, sub: &str| -> bool {
                        let (r, resp) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 52.0), Sense::click());
                        ui.painter().rect_filled(r, 8.0, if resp.hovered() { p.hover } else { p.field });
                        ui.painter().text(Pos2::new(r.left() + 14.0, r.top() + 17.0), Align2::LEFT_CENTER, title, theme::bold_font(14.0), p.text);
                        ui.painter().text(Pos2::new(r.left() + 14.0, r.top() + 36.0), Align2::LEFT_CENTER, sub, theme::font(11.5), p.secondary);
                        resp.clicked()
                    };
                    if choice(ui, "Amazon S3", "Markdown files in a folder of an S3 (or S3-compatible) bucket") {
                        next = Some(Sheet::S3(Box::new(S3Form {
                            again: None,
                            name: String::new(),
                            bucket: String::new(),
                            region: s3::DEFAULT_REGION.into(),
                            prefix: s3::DEFAULT_PREFIX.into(),
                            endpoint: String::new(),
                            access_key_id: String::new(),
                            secret_access_key: String::new(),
                            passphrase: String::new(),
                        })));
                    }
                    ui.add_space(6.0);
                    if choice(ui, "SQLite Database","A database file, e.g. on a network drive or in a synced folder") {
                        let default = directories::UserDirs::new()
                            .and_then(|d| d.document_dir().map(|p| p.join("CleanNotes.db")))
                            .unwrap_or_else(|| "CleanNotes.db".into());
                        next = Some(Sheet::Sqlite(SqliteForm { name: String::new(), path: default.display().to_string() }));
                    }
                    ui.add_space(12.0);
                    ui.horizontal(|ui| {
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui.button("Cancel").clicked() {
                                close = true;
                            }
                        });
                    });
                }
                Sheet::S3(f) => {
                    ui.label(RichText::new(if f.again.is_some() { "Change the S3 access key" } else { "Add an S3 bucket" }).font(theme::bold_font(14.0)));
                    ui.add_space(8.0);
                    let fixed = f.again.is_some();
                    // Fixed while checking: the account is saved with what
                    // the check used.
                    ui.add_enabled_ui(!busy, |ui| egui::Grid::new("s3").num_columns(2).spacing([10.0, 8.0]).show(ui, |ui| {
                        if !fixed {
                            ui.label("Account name:");
                            ui.add(egui::TextEdit::singleline(&mut f.name).hint_text("S3").desired_width(240.0));
                            ui.end_row();
                            ui.label("Bucket:");
                            ui.add(egui::TextEdit::singleline(&mut f.bucket).hint_text("my-notes-bucket").desired_width(240.0));
                            ui.end_row();
                            ui.label("Region:");
                            ui.add(egui::TextEdit::singleline(&mut f.region).hint_text("us-east-1").desired_width(240.0));
                            ui.end_row();
                            ui.label("Folder:");
                            ui.add(egui::TextEdit::singleline(&mut f.prefix).hint_text("empty: the top of the bucket").desired_width(240.0));
                            ui.end_row();
                            ui.label("Endpoint:");
                            ui.add(egui::TextEdit::singleline(&mut f.endpoint).hint_text("empty for Amazon S3").desired_width(240.0));
                            ui.end_row();
                        }
                        ui.label("Access key ID:");
                        ui.add(egui::TextEdit::singleline(&mut f.access_key_id).hint_text("AKIA…").desired_width(240.0));
                        ui.end_row();
                        ui.label("Secret access key:");
                        ui.add(egui::TextEdit::singleline(&mut f.secret_access_key).password(true).desired_width(240.0));
                        ui.end_row();
                        ui.label("Encryption key:");
                        ui.add(egui::TextEdit::singleline(&mut f.passphrase).password(true).hint_text("optional").desired_width(240.0));
                        ui.end_row();
                    }));
                    ui.add_space(4.0);
                    wrapped(
                        ui,
                        "With an encryption key, notes and folders are encrypted on this computer before they're uploaded; other devices need the same key. For a folder that's encrypted already, enter its key. If the key is lost, the notes can't be recovered.",
                        p.secondary,
                    );
                    ui.add_space(4.0);
                    wrapped(
                        ui,
                        "Each note is a Markdown file in the folder's notes/ subfolder. Use an IAM user that can only list, read, write and delete in this bucket. The access key is kept in the system's credential store. For R2, MinIO and other S3-compatible services, enter their endpoint (R2's region is “auto”).",
                        p.secondary,
                    );
                    if sheet_buttons(ui, p, busy, &error, f.ready(), if fixed { "Save" } else { "Add" }, &mut close) {
                        start_s3 = Some((f.config(), f.keys(), f.passphrase.clone()));
                    }
                }
                Sheet::Sqlite(f) => {
                    ui.label(RichText::new("Add a SQLite database").font(theme::bold_font(14.0)));
                    ui.add_space(8.0);
                    egui::Grid::new("sq").num_columns(2).spacing([10.0, 8.0]).show(ui, |ui| {
                        ui.label("Description:");
                        ui.add(egui::TextEdit::singleline(&mut f.name).hint_text("Shared Notes").desired_width(260.0));
                        ui.end_row();
                        ui.label("Database file:");
                        ui.add(egui::TextEdit::singleline(&mut f.path).desired_width(260.0));
                        ui.end_row();
                    });
                    ui.add_space(4.0);
                    wrapped(ui, "Notes are kept as rows in plain tables (folders, notes, tags). The file is created if it doesn't exist; an existing one is synced with.", p.secondary);
                    if sheet_buttons(ui, p, busy, &error, !f.path.trim().is_empty(), "Add", &mut close) {
                        add_sqlite = Some((f.name.clone(), f.path.trim().to_owned()));
                    }
                }
            }
        });
        if let Some(n) = next {
            self.settings_ui.sheet = Some(n);
        }
        if close {
            self.settings_ui.sheet = None;
            self.settings_ui.pending = None;
            self.settings_ui.error = None;
        }
        if let Some((config, keys, passphrase)) = start_s3 {
            let (tx, rx) = mpsc::channel();
            let ctx = ctx.clone();
            std::thread::spawn(move || {
                let _ = match s3::check(&config, &keys, Some(&passphrase)) {
                    Ok((config, secret)) => tx.send((config, Ok(secret))),
                    Err(e) => tx.send((config, Err(format!("{e:#}")))),
                };
                ctx.request_repaint();
            });
            self.settings_ui.pending = Some(Pending::S3(rx));
            self.settings_ui.error = None;
        }
        if let Some((name, path)) = add_sqlite {
            let config = sqlite::Config { path: path.into() };
            match sqlite::SqliteBackend::open(&config) {
                Ok(_) => {
                    let id = new_id();
                    let name = if name.trim().is_empty() { "SQLite".to_owned() } else { name.trim().to_owned() };
                    let account = self.account_for(&id, name, BackendConfig::Sqlite(config));
                    self.settings_ui.sheet = None;
                    self.settings_ui.selected = Some(id);
                    self.add_account(account, None);
                }
                Err(e) => self.settings_ui.error = Some(format!("{e:#}")),
            }
        }
    }
}

/// Error text, a spinner while busy, and Cancel / the default button.
/// Returns whether the default button was pressed.
pub(super) fn sheet_buttons(ui: &mut Ui, p: &Palette, busy: bool, error: &Option<String>, ready: bool, label: &str, close: &mut bool) -> bool {
    if let Some(e) = error {
        ui.add_space(6.0);
        wrapped(ui, e, theme::RED);
    }
    ui.add_space(12.0);
    let mut go = false;
    ui.horizontal(|ui| {
        if busy {
            ui.spinner();
        }
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let b = egui::Button::new(RichText::new(label).color(egui::Color32::WHITE)).fill(if ready && !busy { theme::BLUE } else { p.tertiary });
            if ui.add_enabled(ready && !busy, b).clicked() || (ready && !busy && ui.input(|i| i.key_pressed(Key::Enter))) {
                go = true;
            }
            if ui.button("Cancel").clicked() {
                *close = true;
            }
        });
    });
    go
}

fn wrapped(ui: &mut Ui, text: &str, color: egui::Color32) {
    ui.add(egui::Label::new(RichText::new(text).color(color).size(11.5)).wrap());
}

fn form_row(ui: &mut Ui, p: &Palette, label: &str, add: impl FnOnce(&mut Ui)) {
    ui.horizontal(|ui| {
        let (r, _) = ui.allocate_exact_size(Vec2::new(LABEL_W, 28.0), Sense::hover());
        ui.painter().text(Pos2::new(r.right(), r.center().y), Align2::RIGHT_CENTER, label, theme::font(14.0), p.text);
        ui.add_space(4.0);
        add(ui);
    });
}

/// A grey note under a setting, lined up with the controls.
fn explain(ui: &mut Ui, p: &Palette, text: &str) {
    ui.horizontal(|ui| {
        ui.add_space(LABEL_W + 16.0);
        ui.add(egui::Label::new(RichText::new(text).color(p.secondary).size(12.5)).wrap());
    });
}

/// Apple's settings checkbox: a yellow box, a label, and a grey
/// explanation under it.
fn check_row(ui: &mut Ui, p: &Palette, value: &mut bool, label: &str, detail: &str) {
    ui.horizontal(|ui| {
        ui.add_space(LABEL_W + 14.0);
        ui.vertical(|ui| {
            ui.spacing_mut().item_spacing.y = 2.0;
            let g = ui.painter().layout_no_wrap(label.to_owned(), theme::font(14.0), p.text);
            let (r, resp) = ui.allocate_exact_size(Vec2::new(26.0 + g.size().x, 24.0), Sense::click());
            let bx = Rect::from_min_size(Pos2::new(r.left(), r.center().y - 9.0), Vec2::splat(18.0));
            if *value {
                ui.painter().rect_filled(bx, 5.0, p.folder_selection);
                let s = egui::Stroke::new(2.0_f32, if p.dark { egui::Color32::from_rgb(0x1E, 0x1E, 0x1E) } else { egui::Color32::WHITE });
                let c = bx.center();
                ui.painter().add(egui::Shape::line(vec![c + Vec2::new(-4.5, 0.0), c + Vec2::new(-1.2, 3.5), c + Vec2::new(4.8, -3.8)], s));
            } else {
                ui.painter().rect(bx, 5.0, p.field, egui::Stroke::new(1.0_f32, p.glass_edge));
            }
            ui.painter().galley(Pos2::new(r.left() + 26.0, r.center().y - g.size().y / 2.0), g, p.text);
            if resp.clicked() {
                *value = !*value;
            }
            ui.horizontal(|ui| {
                ui.add_space(26.0);
                ui.add(egui::Label::new(RichText::new(detail).color(p.secondary).size(12.5)).wrap());
            });
        });
    });
    ui.add_space(2.0);
}

pub(super) fn combo<T: PartialEq + Clone>(ui: &mut Ui, id: &str, value: &mut T, options: &[(T, String)]) {
    let current = options.iter().find(|(v, _)| v == value).map(|(_, l)| l.clone()).unwrap_or_default();
    ui.spacing_mut().interact_size.y = 28.0;
    egui::ComboBox::from_id_salt(id).selected_text(current).width(320.0).height(300.0).show_ui(ui, |ui| {
        for (v, l) in options {
            ui.selectable_value(value, v.clone(), l);
        }
    });
}
