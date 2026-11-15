//! The folders sidebar: each account's folders (nested, with note counts),
//! its Recently Deleted, the tags in use, and "New Folder" at the bottom.

use eframe::egui::{self, Align2, Key, Pos2, Rect, Sense, Ui, Vec2};

use super::{App, Confirm, Location, Pane, Rename, TOOLBAR_H};
use crate::icons::{self, Icon};
use crate::theme::{self, Palette};

/// What's being dragged onto a folder.
#[derive(Clone, Debug)]
pub struct NoteDrag(pub String);

const ROW_H: f32 = 28.0;

impl App {
    pub(super) fn sidebar(&mut self, ui: &mut Ui, p: &Palette) {
        let full = ui.max_rect();
        // Toolbar: settings, and the sidebar toggle.
        let bar = Rect::from_min_size(full.min, Vec2::new(full.width(), TOOLBAR_H));
        ui.allocate_rect(bar, Sense::hover());
        if tool_button(ui, p, Rect::from_center_size(Pos2::new(bar.left() + 26.0, bar.center().y), Vec2::splat(28.0)), Icon::Gear, "Settings (Ctrl+,)") {
            self.settings_ui.open = true;
        }
        if tool_button(ui, p, Rect::from_center_size(Pos2::new(bar.right() - 24.0, bar.center().y), Vec2::splat(28.0)), Icon::Sidebar, "Hide Folders (Ctrl+Alt+S)") {
            self.settings.sidebar_visible = false;
        }

        let bottom_h = 40.0;
        let list_rect = Rect::from_min_max(Pos2::new(full.left(), bar.bottom()), Pos2::new(full.right(), full.bottom() - bottom_h));
        let mut child = ui.new_child(egui::UiBuilder::new().max_rect(list_rect).layout(egui::Layout::top_down(egui::Align::LEFT)));
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(&mut child, |ui| {
            ui.spacing_mut().item_spacing.y = 1.0;
            let accounts: Vec<(String, String)> =
                self.store.accounts.iter().filter(|a| a.enabled).map(|a| (a.id.clone(), a.name.clone())).collect();
            for (id, name) in accounts {
                self.account_section(ui, p, &id, &name);
                ui.add_space(10.0);
            }
            self.tags_section(ui, p);
        });

        // "New Folder"
        let nf = Rect::from_min_size(Pos2::new(full.left() + 10.0, full.bottom() - bottom_h + 6.0), Vec2::new(120.0, 28.0));
        let resp = ui.interact(nf, ui.id().with("new-folder"), Sense::click());
        let color = if resp.hovered() { p.text } else { p.secondary };
        icons::paint(ui.painter(), Icon::PlusCircle, Rect::from_center_size(Pos2::new(nf.left() + 11.0, nf.center().y), Vec2::splat(16.0)), color);
        ui.painter().text(Pos2::new(nf.left() + 26.0, nf.center().y), Align2::LEFT_CENTER, "New Folder", theme::font(13.0), color);
        if resp.clicked() {
            self.new_folder(None, None);
        }
    }

    fn account_section(&mut self, ui: &mut Ui, p: &Palette, account: &str, name: &str) {
        let collapsed = self.settings.collapsed.iter().any(|c| c == account);
        // Header: the account's name, with a chevron and sync state.
        let (rect, resp) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 26.0), Sense::click());
        let painter = ui.painter();
        painter.text(Pos2::new(rect.left() + 14.0, rect.center().y + 2.0), Align2::LEFT_CENTER, name, theme::bold_font(11.5), p.secondary);
        let right = Pos2::new(rect.right() - 16.0, rect.center().y + 2.0);
        if resp.hovered() {
            let icon = if collapsed { Icon::ChevronRight } else { Icon::ChevronDown };
            icons::paint(painter, icon, Rect::from_center_size(right, Vec2::splat(12.0)), p.secondary);
        } else if self.syncing(account) {
            let t = ui.input(|i| i.time) as f32;
            let r = Rect::from_center_size(right, Vec2::splat(12.0));
            painter.add(egui::Shape::circle_stroke(r.center(), 5.0, egui::Stroke::new(1.0_f32, p.tertiary)));
            let a = t * 6.0;
            painter.line_segment([r.center(), r.center() + Vec2::new(a.cos(), a.sin()) * 5.0], egui::Stroke::new(1.5_f32, p.secondary));
            ui.ctx().request_repaint();
        } else if self.needs_sign_in.contains(account) || self.sync_errors.contains_key(account) {
            icons::paint(painter, Icon::Warning, Rect::from_center_size(right, Vec2::splat(13.0)), theme::RED);
        }
        let tip = if self.needs_sign_in.contains(account) {
            Some("Sign in again in Settings → Accounts".to_owned())
        } else {
            self.sync_errors.get(account).map(|e| format!("Couldn't sync: {e}"))
        };
        let resp = match tip {
            Some(t) => resp.on_hover_text(t),
            None => resp,
        };
        if resp.clicked() {
            if collapsed {
                self.settings.collapsed.retain(|c| c != account);
            } else {
                self.settings.collapsed.push(account.to_owned());
            }
        }
        let synced = self.store.account(account).is_some_and(|a| a.synced());
        resp.context_menu(|ui| {
            if ui.button("New Folder").clicked() {
                self.new_folder(Some(account.to_owned()), None);
                ui.close_menu();
            }
            if synced && ui.button("Sync Now").clicked() {
                self.sync_now(account);
                ui.close_menu();
            }
            if ui.button("Account Settings…").clicked() {
                self.settings_ui.open_account(account);
                ui.close_menu();
            }
        });
        if collapsed {
            return;
        }

        let tree: Vec<(String, usize, String, bool, bool)> = self
            .store
            .folder_tree(account)
            .into_iter()
            .map(|(f, depth)| {
                let has_kids = self.store.folders.iter().any(|c| c.parent.as_deref() == Some(&f.id));
                (f.id.clone(), depth, f.name.clone(), f.default, has_kids)
            })
            .collect();
        // Hide folders inside collapsed ones.
        let mut hidden_below: Option<usize> = None;
        for (id, depth, name, default, has_kids) in tree {
            if let Some(d) = hidden_below {
                if depth > d {
                    continue;
                }
                hidden_below = None;
            }
            let folded = self.settings.collapsed.contains(&id);
            if folded && has_kids {
                hidden_below = Some(depth);
            }
            let count = self.store.notes.iter().filter(|n| n.folder == id && n.deleted.is_none()).count();
            self.folder_row(ui, p, &id, depth, &name, default, has_kids.then_some(folded), count);
        }
        let deleted = self.store.notes.iter().filter(|n| n.account == account && n.deleted.is_some()).count();
        if deleted > 0 {
            let loc = Location::Deleted(account.to_owned());
            let selected = self.location == loc && self.search.is_empty();
            let resp = self.row(ui, p, 0, Icon::Trash, "Recently Deleted", Some(deleted), selected, None);
            if resp.clicked() {
                self.go_to(loc);
                self.focus = Pane::Sidebar;
            }
            resp.context_menu(|ui| {
                if ui.button("Empty Recently Deleted…").clicked() {
                    self.confirm = Some(Confirm::EmptyDeleted(account.to_owned()));
                    ui.close_menu();
                }
            });
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn folder_row(&mut self, ui: &mut Ui, p: &Palette, id: &str, depth: usize, name: &str, default: bool, folded: Option<bool>, count: usize) {
        let loc = Location::Folder(id.to_owned());
        let selected = self.location == loc && self.search.is_empty();
        let renaming = self.rename.as_ref().is_some_and(|r| r.folder == id);
        let label = if renaming { "" } else { name };
        let resp = self.row(ui, p, depth, Icon::Folder, label, Some(count), selected, folded);

        // The disclosure triangle.
        if folded.is_some() {
            let tri = Rect::from_center_size(Pos2::new(resp.rect.left() + 12.0 + depth as f32 * 16.0, resp.rect.center().y), Vec2::splat(14.0));
            let t = ui.interact(tri, ui.id().with(("fold", id)), Sense::click());
            if t.clicked() {
                if folded == Some(true) {
                    self.settings.collapsed.retain(|c| c != id);
                } else {
                    self.settings.collapsed.push(id.to_owned());
                }
            }
        }
        if renaming {
            let r = self.rename.as_mut().unwrap();
            let text_rect = Rect::from_min_max(
                Pos2::new(resp.rect.left() + 46.0 + depth as f32 * 16.0, resp.rect.top() + 3.0),
                Pos2::new(resp.rect.right() - 40.0, resp.rect.bottom() - 3.0),
            );
            let te = ui.put(text_rect, egui::TextEdit::singleline(&mut r.text).margin(egui::vec2(4.0, 2.0)));
            if r.focus {
                te.request_focus();
                let all = egui::text::CCursorRange::two(egui::text::CCursor::new(0), egui::text::CCursor::new(r.text.chars().count()));
                if let Some(mut state) = egui::TextEdit::load_state(ui.ctx(), te.id) {
                    state.cursor.set_char_range(Some(all));
                    state.store(ui.ctx(), te.id);
                }
                r.focus = false;
            }
            if ui.input(|i| i.key_pressed(Key::Escape)) {
                self.rename = None;
            } else if te.lost_focus() {
                self.finish_rename();
            }
            return;
        }
        if resp.clicked() {
            self.go_to(loc);
            self.focus = Pane::Sidebar;
        }
        if resp.double_clicked() && !default {
            self.rename = Some(Rename { folder: id.to_owned(), text: name.to_owned(), focus: true });
        }
        // Notes dropped here move here.
        if let Some(drag) = resp.dnd_release_payload::<NoteDrag>() {
            self.move_note(&drag.0, id);
        }
        let account = self.store.folder(id).map(|f| f.account.clone());
        resp.context_menu(|ui| {
            if ui.button("New Folder").clicked() {
                self.new_folder(account.clone(), Some(id.to_owned()));
                ui.close_menu();
            }
            if ui.add_enabled(!default, egui::Button::new("Rename Folder")).clicked() {
                self.rename = Some(Rename { folder: id.to_owned(), text: name.to_owned(), focus: true });
                ui.close_menu();
            }
            if ui.add_enabled(!default, egui::Button::new("Delete Folder")).clicked() {
                self.confirm = Some(Confirm::DeleteFolder(id.to_owned()));
                ui.close_menu();
            }
        });
    }

    /// A sidebar row: icon, name, count, selection highlight.
    #[allow(clippy::too_many_arguments)]
    fn row(&self, ui: &mut Ui, p: &Palette, depth: usize, icon: Icon, label: &str, count: Option<usize>, selected: bool, folded: Option<bool>) -> egui::Response {
        let (rect, resp) = ui.allocate_exact_size(Vec2::new(ui.available_width(), ROW_H), Sense::click());
        let inner = rect.shrink2(Vec2::new(8.0, 1.0));
        let painter = ui.painter();
        let drop_hover = resp.dnd_hover_payload::<NoteDrag>().is_some();
        if selected {
            let active = self.focus == Pane::Sidebar;
            painter.rect_filled(inner, 6.0, if active { p.selection } else { p.selection_inactive });
        } else if drop_hover {
            painter.rect_stroke(inner, 6.0, egui::Stroke::new(2.0_f32, p.accent));
        }
        let x0 = inner.left() + depth as f32 * 16.0;
        if let Some(f) = folded {
            let ic = if f { Icon::ChevronRight } else { Icon::ChevronDown };
            icons::paint(painter, ic, Rect::from_center_size(Pos2::new(x0 + 4.0, rect.center().y), Vec2::splat(10.0)), p.secondary);
        }
        let text_color = if selected && self.focus == Pane::Sidebar { p.selection_text } else { p.text };
        let icon_color = if selected && self.focus == Pane::Sidebar && !p.dark { p.selection_text } else { p.accent };
        icons::paint(painter, icon, Rect::from_center_size(Pos2::new(x0 + 22.0, rect.center().y), Vec2::splat(16.0)), icon_color);
        let max_w = inner.right() - (x0 + 36.0) - 30.0;
        let galley = painter.layout(label.to_owned(), theme::font(13.5), text_color, max_w.max(10.0));
        let galley = if galley.rows.len() > 1 {
            painter.layout_no_wrap(truncate(label, max_w, ui), theme::font(13.5), text_color)
        } else {
            galley
        };
        painter.galley(Pos2::new(x0 + 36.0, rect.center().y - galley.size().y / 2.0), galley, text_color);
        if let (Some(n), true) = (count, self.settings.show_note_count) {
            painter.text(Pos2::new(inner.right() - 8.0, rect.center().y), Align2::RIGHT_CENTER, n.to_string(), theme::font(12.5), p.secondary);
        }
        resp
    }

    fn tags_section(&mut self, ui: &mut Ui, p: &Palette) {
        let mut tags: Vec<String> = Vec::new();
        for n in self.store.notes.iter().filter(|n| n.deleted.is_none() && self.account_enabled(&n.account)) {
            for t in n.tags() {
                if !tags.contains(&t) {
                    tags.push(t);
                }
            }
        }
        if tags.is_empty() {
            return;
        }
        tags.sort();
        let (rect, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 26.0), Sense::hover());
        ui.painter().text(Pos2::new(rect.left() + 14.0, rect.center().y + 2.0), Align2::LEFT_CENTER, "Tags", theme::bold_font(11.5), p.secondary);
        ui.add_space(4.0);
        egui::Frame::none().inner_margin(egui::Margin { left: 12.0, right: 8.0, top: 0.0, bottom: 0.0 }).show(ui, |ui| ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing = Vec2::new(6.0, 6.0);
            for t in tags {
                let loc = Location::Tag(t.clone());
                let selected = self.location == loc && self.search.is_empty();
                let text = format!("#{t}");
                let galley = ui.painter().layout_no_wrap(text, theme::font(12.5), if selected { p.selection_text } else { p.text });
                let size = galley.size() + Vec2::new(18.0, 8.0);
                let (r, resp) = ui.allocate_exact_size(size, Sense::click());
                let fill = if selected { p.selection } else if resp.hovered() { p.hover } else { p.field };
                ui.painter().rect_filled(r, r.height() / 2.0, fill);
                ui.painter().galley(r.center() - galley.size() / 2.0, galley, p.text);
                if resp.clicked() {
                    self.go_to(loc);
                    self.focus = Pane::Sidebar;
                }
            }
        }));
    }
}

/// Text cut to fit `max_w` with an ellipsis.
pub fn truncate(text: &str, max_w: f32, ui: &Ui) -> String {
    let font = theme::font(13.5);
    let w = |s: &str| ui.painter().layout_no_wrap(s.to_owned(), font.clone(), egui::Color32::WHITE).size().x;
    if w(text) <= max_w {
        return text.to_owned();
    }
    let chars: Vec<char> = text.chars().collect();
    let (mut lo, mut hi) = (0, chars.len());
    while lo < hi {
        let mid = (lo + hi + 1) / 2;
        let s: String = chars[..mid].iter().collect::<String>() + "…";
        if w(&s) <= max_w {
            lo = mid;
        } else {
            hi = mid - 1;
        }
    }
    chars[..lo].iter().collect::<String>() + "…"
}

/// A round-cornered toolbar button with an icon. Returns whether it was
/// clicked.
pub fn tool_button(ui: &mut Ui, p: &Palette, rect: Rect, icon: Icon, tip: &str) -> bool {
    tool_button_state(ui, p, rect, icon, tip, true, false)
}

pub fn tool_button_state(ui: &mut Ui, p: &Palette, rect: Rect, icon: Icon, tip: &str, enabled: bool, on: bool) -> bool {
    let resp = ui.interact(rect, ui.id().with(("tool", tip)), if enabled { Sense::click() } else { Sense::hover() });
    if enabled && (resp.hovered() || on) {
        ui.painter().rect_filled(rect, 6.0, p.hover);
    }
    let color = if !enabled { p.tertiary } else { p.secondary };
    icons::paint(ui.painter(), icon, rect.shrink(rect.height() * 0.2), color);
    let resp = resp.on_hover_text(tip);
    enabled && resp.clicked()
}
