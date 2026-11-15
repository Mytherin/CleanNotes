//! The Settings window, laid out like Apple Notes' (General) plus an
//! Accounts tab for sync: add (+) or remove (−) accounts, and sign in to
//! Bitwarden or CloudKit, or point at a SQLite database.

use std::sync::mpsc::{self, Receiver};

use eframe::egui::{self, Align2, Key, Pos2, Rect, RichText, Sense, Ui, Vec2};

use super::{modal, App, Confirm, Location};
use crate::model::{new_id, Account, LOCAL};
use crate::settings::{refresh_label, SortBy, StartWith, REFRESH_CHOICES, TEXT_SIZES};
use crate::sync::bitwarden::{self, Method, SignInRequest, SignInStep, TwoFactorCode, TwoFactorKind};
use crate::sync::{cloudkit, sqlite, standardnotes, BackendConfig};
use crate::theme::{self, Palette};

const LABEL_W: f32 = 190.0;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Tab {
    #[default]
    General,
    Accounts,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum BwServer {
    #[default]
    Com,
    Eu,
    SelfHosted,
}

#[derive(Default)]
struct BwForm {
    /// Signing in again to this account (its settings fixed).
    again: Option<String>,
    name: String,
    server: BwServer,
    custom_server: String,
    email: String,
    password: String,
    use_api_key: bool,
    client_id: String,
    client_secret: String,
    folder: String,
    two_factor: Option<Vec<TwoFactorKind>>,
    code: String,
    remember: bool,
    new_device: bool,
    device_code: String,
}

impl BwForm {
    fn config(&self) -> bitwarden::Config {
        let server = match self.server {
            BwServer::Com => bitwarden::DEFAULT_SERVER.to_owned(),
            BwServer::Eu => "https://vault.bitwarden.eu".to_owned(),
            BwServer::SelfHosted => self.custom_server.trim().trim_end_matches('/').to_owned(),
        };
        let folder = if self.folder.trim().is_empty() { bitwarden::DEFAULT_FOLDER.to_owned() } else { self.folder.trim().to_owned() };
        bitwarden::Config { server, email: self.email.trim().to_owned(), folder }
    }

    fn request(&self) -> SignInRequest {
        let method = if self.use_api_key {
            Method::ApiKey {
                client_id: self.client_id.trim().to_owned(),
                client_secret: self.client_secret.trim().to_owned(),
                password: self.password.clone(),
            }
        } else {
            Method::Password { password: self.password.clone() }
        };
        let kind = self.two_factor.as_ref().and_then(|k| {
            k.iter().copied().find(|k| *k == TwoFactorKind::Authenticator).or_else(|| k.iter().copied().find(|k| *k == TwoFactorKind::Email))
        });
        SignInRequest {
            method,
            two_factor: kind.filter(|_| !self.code.trim().is_empty()).map(|kind| TwoFactorCode {
                kind,
                code: self.code.trim().replace(' ', ""),
                remember: self.remember,
            }),
            new_device_code: (self.new_device && !self.device_code.trim().is_empty()).then(|| self.device_code.trim().to_owned()),
        }
    }

    fn ready(&self) -> bool {
        let base = !self.email.trim().is_empty() && !self.password.is_empty();
        let key = !self.use_api_key || (!self.client_id.trim().is_empty() && !self.client_secret.trim().is_empty());
        let server = self.server != BwServer::SelfHosted || self.custom_server.trim().starts_with("http");
        base && key && server
    }
}

#[derive(Default)]
struct SnForm {
    /// Signing in again to this account.
    again: Option<String>,
    name: String,
    self_hosted: bool,
    server: String,
    email: String,
    password: String,
    /// Shown once the server asks for a two-factor code.
    needs_code: bool,
    code: String,
}

impl SnForm {
    fn config(&self) -> standardnotes::Config {
        let server = if self.self_hosted { self.server.trim().trim_end_matches('/').to_owned() } else { standardnotes::DEFAULT_SERVER.to_owned() };
        standardnotes::Config { server, email: self.email.trim().to_owned() }
    }

    fn request(&self) -> standardnotes::SignInRequest {
        standardnotes::SignInRequest { password: self.password.clone(), mfa_code: self.needs_code.then(|| self.code.trim().to_owned()) }
    }

    fn ready(&self) -> bool {
        !self.email.trim().is_empty()
            && !self.password.is_empty()
            && (!self.self_hosted || self.server.trim().starts_with("http"))
            && (!self.needs_code || !self.code.trim().is_empty())
    }
}

struct SqliteForm {
    name: String,
    path: String,
}

struct CkForm {
    again: Option<String>,
    name: String,
    container: String,
    production: bool,
    api_token: String,
    port: String,
}

enum Sheet {
    Choose,
    Bitwarden(Box<BwForm>),
    Sqlite(SqliteForm),
    CloudKit(CkForm),
    StandardNotes(Box<SnForm>),
}

/// A sign-in running in the background.
enum Pending {
    Bitwarden(Receiver<Result<SignInStep, String>>),
    CloudKit(Receiver<Result<String, String>>),
    StandardNotes(Receiver<Result<standardnotes::SignInStep, String>>),
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
        let accounts: Vec<(String, String)> = self.store.accounts.iter().filter(|a| a.enabled).map(|a| (a.id.clone(), a.name.clone())).collect();
        let s = &mut self.settings;
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
        let has_password = self.settings.lock_check.is_some();
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
    }

    fn accounts_tab(&mut self, ui: &mut Ui, p: &Palette) {
        if self.settings_ui.selected.as_ref().map_or(true, |s| self.store.account(s).is_none()) {
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
                        self.store.accounts.iter().map(|a| (a.id.clone(), a.name.clone(), a.backend.kind_name().to_owned())).collect();
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
        let Some(a) = self.store.account(id).cloned() else { return };
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
        egui::Grid::new("account-grid").num_columns(2).spacing([10.0, 8.0]).show(ui, |ui| {
            ui.label("Description:");
            ui.add(egui::TextEdit::singleline(&mut name).desired_width(200.0));
            ui.end_row();
            ui.label("");
            ui.checkbox(&mut enabled, "Enable this account");
            ui.end_row();
            match &a.backend {
                BackendConfig::Bitwarden(c) => {
                    ui.label("Server:");
                    ui.label(&c.server);
                    ui.end_row();
                    ui.label("Email:");
                    ui.label(&c.email);
                    ui.end_row();
                    ui.label("Vault folder:");
                    ui.label(&c.folder);
                    ui.end_row();
                }
                BackendConfig::Sqlite(c) => {
                    ui.label("Database:");
                    ui.label(c.path.display().to_string());
                    ui.end_row();
                }
                BackendConfig::CloudKit(c) => {
                    ui.label("Container:");
                    ui.label(&c.container);
                    ui.end_row();
                    ui.label("Environment:");
                    ui.label(&c.environment);
                    ui.end_row();
                }
                BackendConfig::StandardNotes(c) => {
                    ui.label("Server:");
                    ui.label(&c.server);
                    ui.end_row();
                    ui.label("Email:");
                    ui.label(&c.email);
                    ui.end_row();
                }
                BackendConfig::Local => {}
            }
            ui.label("Status:");
            let status = if self.syncing(id) {
                "Syncing…".to_owned()
            } else if self.needs_sign_in.contains(id) {
                "Needs you to sign in again".to_owned()
            } else if let Some(e) = self.sync_errors.get(id) {
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
                BackendConfig::Bitwarden(c) => {
                    if ui.button("Sign In Again…").clicked() {
                        let server = match c.server.as_str() {
                            bitwarden::DEFAULT_SERVER => BwServer::Com,
                            "https://vault.bitwarden.eu" => BwServer::Eu,
                            _ => BwServer::SelfHosted,
                        };
                        self.settings_ui.sheet = Some(Sheet::Bitwarden(Box::new(BwForm {
                            again: Some(id.to_owned()),
                            name: a.name.clone(),
                            server,
                            custom_server: c.server.clone(),
                            email: c.email.clone(),
                            folder: c.folder.clone(),
                            ..BwForm::default()
                        })));
                        self.settings_ui.error = None;
                    }
                }
                BackendConfig::CloudKit(c) => {
                    if ui.button("Sign In Again…").clicked() {
                        self.settings_ui.sheet = Some(Sheet::CloudKit(CkForm {
                            again: Some(id.to_owned()),
                            name: a.name.clone(),
                            container: c.container.clone(),
                            production: c.environment == "production",
                            api_token: c.api_token.clone(),
                            port: c.callback_port.to_string(),
                        }));
                        self.settings_ui.error = None;
                    }
                }
                BackendConfig::StandardNotes(c) => {
                    if ui.button("Sign In Again…").clicked() {
                        self.settings_ui.sheet = Some(Sheet::StandardNotes(Box::new(SnForm {
                            again: Some(id.to_owned()),
                            name: a.name.clone(),
                            self_hosted: c.server != standardnotes::DEFAULT_SERVER,
                            server: c.server.clone(),
                            email: c.email.clone(),
                            ..SnForm::default()
                        })));
                        self.settings_ui.error = None;
                    }
                }
                _ => {}
            }
        });
        if name != a.name || enabled != a.enabled {
            let disabled = a.enabled && !enabled;
            let acc = self.store.account_mut(id).unwrap();
            acc.name = name;
            acc.enabled = enabled;
            if disabled {
                // Its notes vanish from view: don't stay in one of its
                // folders, or keep one of its notes open.
                let inside = match &self.location {
                    Location::Folder(f) => self.store.folder(f).is_some_and(|f| f.account == id),
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
            Pending::Bitwarden(rx) => {
                let Ok(r) = rx.try_recv() else { return };
                self.settings_ui.pending = None;
                let Some(Sheet::Bitwarden(form)) = &mut self.settings_ui.sheet else { return };
                match r {
                    Ok(SignInStep::Done { secret }) => {
                        let config = form.config();
                        let (id, name) = match &form.again {
                            Some(id) => (id.clone(), form.name.clone()),
                            None => (new_id(), if form.name.trim().is_empty() { "Bitwarden".to_owned() } else { form.name.trim().to_owned() }),
                        };
                        let account = self.account_for(&id, name, BackendConfig::Bitwarden(config));
                        self.settings_ui.sheet = None;
                        self.settings_ui.selected = Some(id);
                        self.add_account(account, Some(secret));
                    }
                    Ok(SignInStep::NeedsTwoFactor { kinds }) => {
                        let usable = kinds.iter().any(|k| matches!(k, TwoFactorKind::Authenticator | TwoFactorKind::Email));
                        if !usable {
                            self.settings_ui.error = Some(
                                "This account's two-step login method isn't supported here. Use an API key (Account Settings → Security → Keys), or add an authenticator app or email as a two-step method.".into(),
                            );
                        } else {
                            let email_only = !kinds.contains(&TwoFactorKind::Authenticator);
                            form.two_factor = Some(kinds);
                            self.settings_ui.error = None;
                            if email_only {
                                let (config, req) = (form.config(), form.request());
                                std::thread::spawn(move || {
                                    let _ = bitwarden::send_email_code(&config, &req);
                                });
                            }
                        }
                    }
                    Ok(SignInStep::NeedsNewDeviceCode) => {
                        form.new_device = true;
                        self.settings_ui.error = None;
                    }
                    Err(e) => self.settings_ui.error = Some(e),
                }
            }
            Pending::StandardNotes(rx) => {
                let Ok(r) = rx.try_recv() else { return };
                self.settings_ui.pending = None;
                let Some(Sheet::StandardNotes(form)) = &mut self.settings_ui.sheet else { return };
                match r {
                    Ok(standardnotes::SignInStep::Done { secret }) => {
                        let config = form.config();
                        let (id, name) = match &form.again {
                            Some(id) => (id.clone(), form.name.clone()),
                            None => (new_id(), if form.name.trim().is_empty() { "Standard Notes".to_owned() } else { form.name.trim().to_owned() }),
                        };
                        let account = self.account_for(&id, name, BackendConfig::StandardNotes(config));
                        self.settings_ui.sheet = None;
                        self.settings_ui.selected = Some(id);
                        self.add_account(account, Some(secret));
                    }
                    Ok(standardnotes::SignInStep::NeedsMfaCode) => {
                        form.needs_code = true;
                        self.settings_ui.error = None;
                    }
                    Err(e) => self.settings_ui.error = Some(e),
                }
            }
            Pending::CloudKit(rx) => {
                let Ok(r) = rx.try_recv() else { return };
                self.settings_ui.pending = None;
                let Some(Sheet::CloudKit(form)) = &self.settings_ui.sheet else { return };
                match r {
                    Ok(secret) => {
                        let config = ck_config(form);
                        let (id, name) = match &form.again {
                            Some(id) => (id.clone(), form.name.clone()),
                            None => (new_id(), if form.name.trim().is_empty() { "iCloud (CloudKit)".to_owned() } else { form.name.trim().to_owned() }),
                        };
                        let account = self.account_for(&id, name, BackendConfig::CloudKit(config));
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
        match self.store.account(id) {
            Some(a) => Account { name, backend, enabled: true, ..a.clone() },
            None => Account { id: id.to_owned(), name, backend, enabled: true, cursor: None, last_sync: None },
        }
    }

    fn account_sheet(&mut self, ctx: &egui::Context, p: &Palette) {
        if self.settings_ui.sheet.is_none() {
            return;
        }
        let busy = self.settings_ui.pending.is_some();
        let error = self.settings_ui.error.clone();
        let mut close = false;
        let mut next: Option<Sheet> = None;
        let mut start_bw: Option<(bitwarden::Config, SignInRequest)> = None;
        let mut start_ck: Option<cloudkit::Config> = None;
        let mut start_sn: Option<(standardnotes::Config, standardnotes::SignInRequest)> = None;
        let mut add_sqlite: Option<(String, String)> = None;
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
                    if choice(ui, "Bitwarden", "Encrypted, as secure notes in your vault") {
                        next = Some(Sheet::Bitwarden(Box::new(BwForm { folder: bitwarden::DEFAULT_FOLDER.into(), ..BwForm::default() })));
                    }
                    ui.add_space(6.0);
                    if choice(ui, "Standard Notes", "End-to-end encrypted notes in your Standard Notes account") {
                        next = Some(Sheet::StandardNotes(Box::default()));
                    }
                    ui.add_space(6.0);
                    if choice(ui, "iCloud (CloudKit)", "Records in your CloudKit container's private database") {
                        next = Some(Sheet::CloudKit(CkForm {
                            again: None,
                            name: String::new(),
                            container: String::new(),
                            production: true,
                            api_token: String::new(),
                            port: cloudkit::DEFAULT_CALLBACK_PORT.to_string(),
                        }));
                    }
                    ui.add_space(6.0);
                    if choice(ui, "SQLite Database", "A database file, e.g. on a network drive or in a synced folder") {
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
                Sheet::Bitwarden(f) => {
                    ui.label(RichText::new(if f.again.is_some() { "Sign in to Bitwarden again" } else { "Add a Bitwarden account" }).font(theme::bold_font(14.0)));
                    ui.add_space(8.0);
                    let fixed = f.again.is_some();
                    egui::Grid::new("bw").num_columns(2).spacing([10.0, 8.0]).show(ui, |ui| {
                        if !fixed {
                            ui.label("Description:");
                            ui.add(egui::TextEdit::singleline(&mut f.name).hint_text("Bitwarden").desired_width(240.0));
                            ui.end_row();
                            ui.label("Server:");
                            ui.horizontal(|ui| {
                                ui.selectable_value(&mut f.server, BwServer::Com, "bitwarden.com");
                                ui.selectable_value(&mut f.server, BwServer::Eu, "bitwarden.eu");
                                ui.selectable_value(&mut f.server, BwServer::SelfHosted, "Self-hosted");
                            });
                            ui.end_row();
                            if f.server == BwServer::SelfHosted {
                                ui.label("Server URL:");
                                ui.add(egui::TextEdit::singleline(&mut f.custom_server).hint_text("https://vault.example.com").desired_width(240.0));
                                ui.end_row();
                            }
                        }
                        ui.label("Email:");
                        ui.add_enabled(!fixed, egui::TextEdit::singleline(&mut f.email).desired_width(240.0));
                        ui.end_row();
                        ui.label("Master password:");
                        ui.add(egui::TextEdit::singleline(&mut f.password).password(true).desired_width(240.0));
                        ui.end_row();
                        ui.label("");
                        ui.checkbox(&mut f.use_api_key, "Sign in with an API key");
                        ui.end_row();
                        if f.use_api_key {
                            ui.label("client_id:");
                            ui.add(egui::TextEdit::singleline(&mut f.client_id).hint_text("user.…").desired_width(240.0));
                            ui.end_row();
                            ui.label("client_secret:");
                            ui.add(egui::TextEdit::singleline(&mut f.client_secret).password(true).desired_width(240.0));
                            ui.end_row();
                        }
                        if !fixed {
                            ui.label("Vault folder:");
                            ui.add(egui::TextEdit::singleline(&mut f.folder).desired_width(240.0));
                            ui.end_row();
                        }
                        if let Some(kinds) = &f.two_factor {
                            let label = if kinds.contains(&TwoFactorKind::Authenticator) { "Authenticator code:" } else { "Code from email:" };
                            ui.label(label);
                            ui.add(egui::TextEdit::singleline(&mut f.code).desired_width(120.0));
                            ui.end_row();
                            ui.label("");
                            ui.checkbox(&mut f.remember, "Remember this computer");
                            ui.end_row();
                        }
                        if f.new_device {
                            ui.label("Device code:");
                            ui.add(egui::TextEdit::singleline(&mut f.device_code).hint_text("from the email Bitwarden sent").desired_width(240.0));
                            ui.end_row();
                        }
                    });
                    ui.add_space(4.0);
                    wrapped(ui, "Notes are stored as secure notes in the vault folder, encrypted on this computer. The master password isn't saved; only the unlocked vault key, in the system's credential store.", p.secondary);
                    if sheet_buttons(ui, p, busy, &error, f.ready(), "Sign In", &mut close) {
                        start_bw = Some((f.config(), f.request()));
                    }
                }
                Sheet::CloudKit(f) => {
                    ui.label(RichText::new(if f.again.is_some() { "Sign in to iCloud again" } else { "Add an iCloud (CloudKit) account" }).font(theme::bold_font(14.0)));
                    ui.add_space(8.0);
                    let fixed = f.again.is_some();
                    egui::Grid::new("ck").num_columns(2).spacing([10.0, 8.0]).show(ui, |ui| {
                        ui.label("Description:");
                        ui.add_enabled(!fixed, egui::TextEdit::singleline(&mut f.name).hint_text("iCloud (CloudKit)").desired_width(240.0));
                        ui.end_row();
                        ui.label("Container:");
                        ui.add_enabled(!fixed, egui::TextEdit::singleline(&mut f.container).hint_text("iCloud.com.example.notes").desired_width(240.0));
                        ui.end_row();
                        ui.label("Environment:");
                        ui.horizontal(|ui| {
                            ui.add_enabled_ui(!fixed, |ui| {
                                ui.selectable_value(&mut f.production, true, "Production");
                                ui.selectable_value(&mut f.production, false, "Development");
                            });
                        });
                        ui.end_row();
                        ui.label("API token:");
                        ui.add(egui::TextEdit::singleline(&mut f.api_token).desired_width(240.0));
                        ui.end_row();
                        ui.label("Callback port:");
                        ui.add(egui::TextEdit::singleline(&mut f.port).desired_width(80.0));
                        ui.end_row();
                    });
                    ui.add_space(4.0);
                    let port = f.port.trim();
                    wrapped(
                        ui,
                        &format!("In CloudKit Dashboard, give the API token the sign-in callback “URL Redirect” to http://127.0.0.1:{port}/. Signing in opens your browser to sign in with your Apple ID."),
                        p.secondary,
                    );
                    let ready = f.container.trim().starts_with("iCloud.") && !f.api_token.trim().is_empty() && f.port.trim().parse::<u16>().is_ok();
                    if sheet_buttons(ui, p, busy, &error, ready, "Sign In", &mut close) {
                        start_ck = Some(ck_config(f));
                    }
                }
                Sheet::StandardNotes(f) => {
                    ui.label(RichText::new(if f.again.is_some() { "Sign in to Standard Notes again" } else { "Add a Standard Notes account" }).font(theme::bold_font(14.0)));
                    ui.add_space(8.0);
                    let fixed = f.again.is_some();
                    egui::Grid::new("sn").num_columns(2).spacing([10.0, 8.0]).show(ui, |ui| {
                        if !fixed {
                            ui.label("Description:");
                            ui.add(egui::TextEdit::singleline(&mut f.name).hint_text("Standard Notes").desired_width(240.0));
                            ui.end_row();
                            ui.label("Server:");
                            ui.horizontal(|ui| {
                                ui.selectable_value(&mut f.self_hosted, false, "standardnotes.com");
                                ui.selectable_value(&mut f.self_hosted, true, "Self-hosted");
                            });
                            ui.end_row();
                            if f.self_hosted {
                                ui.label("Server URL:");
                                ui.add(egui::TextEdit::singleline(&mut f.server).hint_text("https://notes-api.example.com").desired_width(240.0));
                                ui.end_row();
                            }
                        }
                        ui.label("Email:");
                        ui.add_enabled(!fixed, egui::TextEdit::singleline(&mut f.email).desired_width(240.0));
                        ui.end_row();
                        ui.label("Password:");
                        ui.add(egui::TextEdit::singleline(&mut f.password).password(true).desired_width(240.0));
                        ui.end_row();
                        if f.needs_code {
                            ui.label("Two-factor code:");
                            ui.add(egui::TextEdit::singleline(&mut f.code).hint_text("from your authenticator app").desired_width(160.0));
                            ui.end_row();
                        }
                    });
                    ui.add_space(4.0);
                    wrapped(
                        ui,
                        "Notes are encrypted on this computer before they're synced. Tags become folders. Super and other rich notes show read-only; editing one saves a new plain note and keeps the original. The password isn't saved; only the session and keys, in the system's credential store.",
                        p.secondary,
                    );
                    if sheet_buttons(ui, p, busy, &error, f.ready(), "Sign In", &mut close) {
                        start_sn = Some((f.config(), f.request()));
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
        if let Some((config, req)) = start_bw {
            let (tx, rx) = mpsc::channel();
            let ctx = ctx.clone();
            std::thread::spawn(move || {
                let _ = tx.send(bitwarden::sign_in(&config, &req).map_err(|e| format!("{e:#}")));
                ctx.request_repaint();
            });
            self.settings_ui.pending = Some(Pending::Bitwarden(rx));
            self.settings_ui.error = None;
        }
        if let Some((config, req)) = start_sn {
            let (tx, rx) = mpsc::channel();
            let ctx = ctx.clone();
            std::thread::spawn(move || {
                let _ = tx.send(standardnotes::sign_in(&config, &req).map_err(|e| format!("{e:#}")));
                ctx.request_repaint();
            });
            self.settings_ui.pending = Some(Pending::StandardNotes(rx));
            self.settings_ui.error = None;
        }
        if let Some(config) = start_ck {
            let (tx, rx) = mpsc::channel();
            let ctx = ctx.clone();
            std::thread::spawn(move || {
                let _ = tx.send(cloudkit::sign_in(&config).map_err(|e| format!("{e:#}")));
                ctx.request_repaint();
            });
            self.settings_ui.pending = Some(Pending::CloudKit(rx));
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

fn ck_config(f: &CkForm) -> cloudkit::Config {
    cloudkit::Config {
        container: f.container.trim().to_owned(),
        environment: if f.production { "production" } else { "development" }.to_owned(),
        api_token: f.api_token.trim().to_owned(),
        callback_port: f.port.trim().parse().unwrap_or(cloudkit::DEFAULT_CALLBACK_PORT),
        endpoint: None,
    }
}

/// Error text, a spinner while busy, and Cancel / the default button.
/// Returns whether the default button was pressed.
fn sheet_buttons(ui: &mut Ui, p: &Palette, busy: bool, error: &Option<String>, ready: bool, label: &str, close: &mut bool) -> bool {
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

fn combo<T: PartialEq + Clone>(ui: &mut Ui, id: &str, value: &mut T, options: &[(T, String)]) {
    let current = options.iter().find(|(v, _)| v == value).map(|(_, l)| l.clone()).unwrap_or_default();
    ui.spacing_mut().interact_size.y = 28.0;
    egui::ComboBox::from_id_salt(id).selected_text(current).width(320.0).height(300.0).show_ui(ui, |ui| {
        for (v, l) in options {
            ui.selectable_value(value, v.clone(), l);
        }
    });
}
