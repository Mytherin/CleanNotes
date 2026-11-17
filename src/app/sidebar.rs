//! The folders sidebar: each account's folders (nested, with note counts),
//! its Recently Deleted, the tags in use, and "New Folder" at the bottom.

use eframe::egui::{self, Align2, Key, Pos2, Rect, Sense, Ui, Vec2};

use super::{App, Confirm, Location, Pane, Rename, TOOLBAR_H};
use crate::icons::{self, Icon};
use crate::theme::{self, Palette};

/// The notes being dragged onto a folder.
#[derive(Clone, Debug)]
pub struct NoteDrag(pub Vec<String>);

/// The folder being dragged into another folder, or onto an account (to its
/// top level).
#[derive(Clone, Debug)]
pub struct FolderDrag(pub String);

/// The folders around a folder row in the sidebar, for dropping a folder
/// in the gaps above and below it.
#[derive(Clone, Copy)]
struct Neighbours<'a> {
    /// The next folder at the same level (`None`: it's the last).
    next: Option<&'a str>,
    /// The first folder inside it, if it shows any.
    first_kid: Option<&'a str>,
}

/// The line showing where a dragged folder will go: across the sidebar at
/// `y`, indented to `depth`, with a small ring at its start (as in Finder).
fn insertion_line(ui: &Ui, p: &Palette, row: Rect, depth: usize, y: f32) {
    let painter = ui.ctx().layer_painter(egui::LayerId::new(egui::Order::Foreground, ui.id().with("folder drop line")));
    let painter = painter.with_clip_rect(ui.clip_rect());
    let x0 = row.left() + 6.0 + depth as f32 * 16.0 + 16.0;
    let stroke = egui::Stroke::new(2.0_f32, p.folder_selection);
    painter.circle_stroke(Pos2::new(x0, y), 3.5, stroke);
    painter.line_segment([Pos2::new(x0 + 3.5, y), Pos2::new(row.right() - 8.0, y)], stroke);
}

const ROW_H: f32 = 32.0;
/// The Tags section's entry in the folded sections and folders (which are
/// otherwise ids, none of which starts with "#").
const TAGS_KEY: &str = "#tags";
/// The gap around the floating sidebar panel.
const INSET: f32 = 8.0;

impl App {
    pub(super) fn sidebar(&mut self, ui: &mut Ui, p: &Palette) {
        // A rounded panel floating over the window.
        let full = ui.max_rect().shrink(INSET);
        ui.painter().rect(full, 18.0, p.sidebar, egui::Stroke::new(1.0_f32, p.sidebar_edge));
        // Toolbar: settings; New Folder and the sidebar toggle on the right.
        let bar = Rect::from_min_size(full.min, Vec2::new(full.width(), TOOLBAR_H - INSET));
        ui.allocate_rect(bar, Sense::hover());
        let y = bar.center().y;
        if tool_button(ui, p, Rect::from_center_size(Pos2::new(bar.left() + 24.0, y), Vec2::splat(30.0)), Icon::Gear, "Settings (Ctrl+,)") {
            self.settings_ui.open = true;
        }
        if tool_button(ui, p, Rect::from_center_size(Pos2::new(bar.right() - 24.0, y), Vec2::splat(30.0)), Icon::Sidebar, "Hide Folders (Ctrl+Alt+S)") {
            self.lib.settings.sidebar_visible = false;
        }
        if tool_button(ui, p, Rect::from_center_size(Pos2::new(bar.right() - 62.0, y), Vec2::splat(30.0)), Icon::FolderPlus, "New Folder (Ctrl+Shift+N)") {
            self.new_folder(None, None);
        }

        let list_rect = Rect::from_min_max(Pos2::new(full.left() + 4.0, bar.bottom() + 6.0), Pos2::new(full.right() - 4.0, full.bottom() - 10.0));
        let mut child = ui.new_child(egui::UiBuilder::new().max_rect(list_rect).layout(egui::Layout::top_down(egui::Align::LEFT)));
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(&mut child, |ui| {
            ui.spacing_mut().item_spacing.y = 1.0;
            // "All Notes": every note, whatever its folder or account.
            if self.show_all_notes() {
                let count = self.lib.store.notes.iter().filter(|n| n.deleted.is_none() && self.account_enabled(&n.account)).count();
                let selected = self.location == Location::All && self.search.is_empty();
                ui.add_space(4.0);
                let resp = self.row(ui, p, 0, Icon::Notes, "All Notes", Some(count), selected, None);
                if resp.clicked() {
                    self.go_to(Location::All);
                    self.focus = Pane::Sidebar;
                }
                ui.add_space(8.0);
            } else if self.location == Location::All {
                let f = self.new_note_target().1;
                self.go_to(Location::Folder(f));
            }
            let accounts: Vec<(String, String)> =
                self.lib.store.accounts.iter().filter(|a| a.enabled).map(|a| (a.id.clone(), a.name.clone())).collect();
            for (id, name) in accounts {
                self.account_section(ui, p, &id, &name);
                ui.add_space(10.0);
            }
            self.tags_section(ui, p);
        });
        self.folder_drag_preview(ui, p);
    }

    /// The folder being dragged, following the pointer.
    fn folder_drag_preview(&self, ui: &Ui, p: &Palette) {
        let Some(drag) = egui::DragAndDrop::payload::<FolderDrag>(ui.ctx()) else { return };
        let (Some(pos), Some(f)) = (ui.ctx().pointer_latest_pos(), self.lib.store.folder(&drag.0)) else { return };
        let painter = ui.ctx().layer_painter(egui::LayerId::new(egui::Order::Tooltip, ui.id().with("folder drag")));
        let galley = painter.layout_no_wrap(f.name.clone(), theme::font(14.0), p.text);
        let rect = Rect::from_min_size(pos + Vec2::new(12.0, 6.0), Vec2::new(galley.size().x + 44.0, ROW_H - 4.0));
        painter.rect(rect, 10.0, p.popover, egui::Stroke::new(1.0_f32, p.glass_edge));
        icons::paint(&painter, Icon::Folder, Rect::from_center_size(Pos2::new(rect.left() + 18.0, rect.center().y), Vec2::splat(16.0)), p.text);
        painter.galley(Pos2::new(rect.left() + 34.0, rect.center().y - galley.size().y / 2.0), galley, p.text);
    }

    /// The right-click menu's "Move To": an account's top level, or one of
    /// its folders.
    fn folder_move_to_menu(&mut self, ui: &mut Ui, id: &str) {
        let accounts: Vec<(String, String)> =
            self.lib.store.accounts.iter().filter(|a| a.enabled).map(|a| (a.id.clone(), a.name.clone())).collect();
        let mut chosen: Option<(String, Option<String>)> = None;
        for (i, (account, name)) in accounts.iter().enumerate() {
            if i > 0 {
                ui.separator();
            }
            let top = ui
                .add_enabled(self.lib.can_move_folder(id, account, None), egui::Button::new(egui::RichText::new(name).strong()))
                .on_hover_text(format!("The top level of “{name}”"));
            if top.clicked() {
                chosen = Some((account.clone(), None));
            }
            let tree: Vec<(String, String, usize)> =
                self.lib.store.folder_tree(account).into_iter().map(|(f, d)| (f.id.clone(), f.name.clone(), d)).collect();
            for (fid, fname, depth) in tree {
                let label = format!("{}{fname}", "    ".repeat(depth + 1));
                if ui.add_enabled(self.lib.can_move_folder(id, account, Some(&fid)), egui::Button::new(label)).clicked() {
                    chosen = Some((account.clone(), Some(fid)));
                }
            }
        }
        if let Some((account, parent)) = chosen {
            self.move_folder(id, &account, parent.as_deref());
            ui.close_menu();
            ui.memory_mut(|m| m.close_popup());
        }
    }

    fn account_section(&mut self, ui: &mut Ui, p: &Palette, account: &str, name: &str) {
        let collapsed = self.lib.settings.collapsed.iter().any(|c| c == account);
        // Header: the account's name, with a chevron and sync state.
        let (rect, resp) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 30.0), Sense::click());
        let painter = ui.painter();
        painter.text(Pos2::new(rect.left() + 16.0, rect.center().y + 2.0), Align2::LEFT_CENTER, name, theme::bold_font(12.0), p.secondary);
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
        } else if self.lib.needs_sign_in.contains(account) || self.lib.sync_errors.contains_key(account) {
            icons::paint(painter, Icon::Warning, Rect::from_center_size(right, Vec2::splat(13.0)), theme::RED);
        }
        let tip = if self.lib.needs_sign_in.contains(account) {
            Some("Sign in again in Settings → Accounts".to_owned())
        } else {
            self.lib.sync_errors.get(account).map(|e| format!("Couldn't sync: {e}"))
        };
        let resp = match tip {
            Some(t) => resp.on_hover_text(t),
            None => resp,
        };
        if resp.clicked() {
            if collapsed {
                self.lib.settings.collapsed.retain(|c| c != account);
            } else {
                self.lib.settings.collapsed.push(account.to_owned());
            }
        }
        // A folder dropped on the account goes to its top level.
        if resp.dnd_hover_payload::<FolderDrag>().is_some_and(|d| self.lib.can_move_folder(&d.0, account, None)) {
            ui.painter().rect_stroke(rect.shrink2(Vec2::new(6.0, 1.0)), 10.0, egui::Stroke::new(2.0_f32, p.folder_selection));
        }
        if let Some(d) = resp.dnd_release_payload::<FolderDrag>() {
            self.move_folder(&d.0, account, None);
        }
        let synced = self.lib.store.account(account).is_some_and(|a| a.synced());
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
            .lib.store
            .folder_tree(account)
            .into_iter()
            .map(|(f, depth)| {
                let has_kids = self.lib.store.folders.iter().any(|c| c.parent.as_deref() == Some(&f.id));
                (f.id.clone(), depth, f.name.clone(), f.default, has_kids)
            })
            .collect();
        // Notes per folder, counted in one pass (not one per folder).
        let counts: Vec<usize> = {
            let mut m: std::collections::HashMap<&str, usize> = std::collections::HashMap::new();
            for n in self.lib.store.notes.iter().filter(|n| n.deleted.is_none() && n.account == account) {
                *m.entry(n.folder.as_str()).or_default() += 1;
            }
            tree.iter().map(|(id, ..)| m.get(id.as_str()).copied().unwrap_or(0)).collect()
        };
        // Where a folder dragged above or below each row would go: the next
        // folder at its level, and the first folder inside it.
        let neighbours: Vec<(Option<String>, Option<String>)> = (0..tree.len())
            .map(|i| {
                let depth = tree[i].1;
                let next = tree[i + 1..].iter().find(|t| t.1 <= depth).filter(|t| t.1 == depth).map(|t| t.0.clone());
                let first_kid = tree.get(i + 1).filter(|t| t.1 == depth + 1).map(|t| t.0.clone());
                (next, first_kid)
            })
            .collect();
        // Hide folders inside collapsed ones.
        let mut hidden_below: Option<usize> = None;
        for (((id, depth, name, default, has_kids), count), (next, first_kid)) in tree.into_iter().zip(counts).zip(neighbours) {
            if let Some(d) = hidden_below {
                if depth > d {
                    continue;
                }
                hidden_below = None;
            }
            let folded = self.lib.settings.collapsed.contains(&id);
            if folded && has_kids {
                hidden_below = Some(depth);
            }
            let slots = Neighbours { next: next.as_deref(), first_kid: first_kid.as_deref() };
            self.folder_row(ui, p, &id, depth, &name, default, has_kids.then_some(folded), count, slots);
        }
        let deleted = self.lib.store.notes.iter().filter(|n| n.account == account && n.deleted.is_some()).count();
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
    fn folder_row(
        &mut self,
        ui: &mut Ui,
        p: &Palette,
        id: &str,
        depth: usize,
        name: &str,
        default: bool,
        folded: Option<bool>,
        count: usize,
        slots: Neighbours,
    ) {
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
                    self.lib.settings.collapsed.retain(|c| c != id);
                } else {
                    self.lib.settings.collapsed.push(id.to_owned());
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
        // Notes dropped here move here. (Only take a payload of notes: egui
        // drops whatever payload is taken, so this mustn't eat a folder.)
        if resp.dnd_hover_payload::<NoteDrag>().is_some() {
            if let Some(drag) = resp.dnd_release_payload::<NoteDrag>() {
                self.move_notes(&drag.0, id);
            }
        }
        let (account, parent) = match self.lib.store.folder(id) {
            Some(f) => (Some(f.account.clone()), f.parent.clone()),
            None => (None, None),
        };
        // Folders can be dragged into other folders, or between them to put
        // them in order (not the default one, which stays where it is).
        if !default {
            resp.dnd_set_drag_payload(FolderDrag(id.to_owned()));
        }
        if let (Some(acc), Some(drag)) = (&account, resp.dnd_hover_payload::<FolderDrag>()) {
            let pointer = ui.ctx().pointer_latest_pos().unwrap_or(resp.rect.center());
            let t = (pointer.y - resp.rect.top()) / resp.rect.height();
            // The top and bottom of the row are the gaps above and below it
            // (below an open folder is above its first folder); the middle
            // is the folder itself.
            let gap = if t < 0.25 {
                Some((parent.as_deref(), Some(id), depth, resp.rect.top()))
            } else if t > 0.75 {
                match (folded, slots.first_kid) {
                    (Some(false), Some(kid)) => Some((Some(id), Some(kid), depth + 1, resp.rect.bottom())),
                    _ => Some((parent.as_deref(), slots.next, depth, resp.rect.bottom())),
                }
            } else {
                None
            };
            match gap {
                Some((into, before, line_depth, y)) => {
                    if self.lib.can_place_folder(&drag.0, acc, into, before) {
                        insertion_line(ui, p, resp.rect, line_depth, y);
                        if resp.dnd_release_payload::<FolderDrag>().is_some() {
                            self.place_folder(&drag.0, acc, into, before);
                        }
                    }
                }
                None => {
                    if self.lib.can_move_folder(&drag.0, acc, Some(id)) {
                        ui.painter().rect_stroke(resp.rect.shrink2(Vec2::new(6.0, 1.0)), 10.0, egui::Stroke::new(2.0_f32, p.folder_selection));
                        if resp.dnd_release_payload::<FolderDrag>().is_some() {
                            self.move_folder(&drag.0, acc, Some(id));
                        }
                    }
                }
            }
        }
        resp.context_menu(|ui| {
            if ui.button("New Folder").clicked() {
                self.new_folder(account.clone(), Some(id.to_owned()));
                ui.close_menu();
            }
            if default {
                ui.add_enabled(false, egui::Button::new("Move To"));
            } else {
                ui.menu_button("Move To", |ui| self.folder_move_to_menu(ui, id));
            }
            if ui.add_enabled(!default, egui::Button::new("Rename Folder")).clicked() {
                self.rename = Some(Rename { folder: id.to_owned(), text: name.to_owned(), focus: true });
                ui.close_menu();
            }
            if ui.button("Delete Folder").clicked() {
                self.confirm = Some(Confirm::DeleteFolder(id.to_owned()));
                ui.close_menu();
            }
        });
    }

    /// A sidebar row: icon, name, count, selection highlight.
    #[allow(clippy::too_many_arguments)]
    fn row(&self, ui: &mut Ui, p: &Palette, depth: usize, icon: Icon, label: &str, count: Option<usize>, selected: bool, folded: Option<bool>) -> egui::Response {
        // Draggable too: folder rows can be dragged into other folders.
        let (rect, resp) = ui.allocate_exact_size(Vec2::new(ui.available_width(), ROW_H), Sense::click_and_drag());
        let inner = rect.shrink2(Vec2::new(6.0, 1.0));
        let painter = ui.painter();
        let drop_hover = resp.dnd_hover_payload::<NoteDrag>().is_some();
        // The selected folder is a yellow pill, whichever pane has the keyboard.
        if selected {
            painter.rect_filled(inner, 10.0, p.folder_selection);
        } else if drop_hover {
            painter.rect_stroke(inner, 10.0, egui::Stroke::new(2.0_f32, p.folder_selection));
        } else if resp.hovered() {
            painter.rect_filled(inner, 10.0, p.hover);
        }
        let x0 = inner.left() + depth as f32 * 16.0;
        if let Some(f) = folded {
            let ic = if f { Icon::ChevronRight } else { Icon::ChevronDown };
            icons::paint(painter, ic, Rect::from_center_size(Pos2::new(x0 + 4.0, rect.center().y), Vec2::splat(10.0)), p.secondary);
        }
        let text_color = if selected { egui::Color32::WHITE } else { p.text };
        let icon_color = if selected { egui::Color32::WHITE } else { p.text };
        icons::paint(painter, icon, Rect::from_center_size(Pos2::new(x0 + 24.0, rect.center().y), Vec2::splat(18.0)), icon_color);
        let max_w = inner.right() - (x0 + 40.0) - 34.0;
        let font = theme::font(14.0);
        let galley = painter.layout(label.to_owned(), font.clone(), text_color, max_w.max(10.0));
        let galley = if galley.rows.len() > 1 {
            painter.layout_no_wrap(truncate(label, max_w, ui), font, text_color)
        } else {
            galley
        };
        painter.galley(Pos2::new(x0 + 40.0, rect.center().y - galley.size().y / 2.0), galley, text_color);
        if let (Some(n), true) = (count, self.lib.settings.show_note_count) {
            let c = if selected { egui::Color32::WHITE } else { p.secondary };
            painter.text(Pos2::new(inner.right() - 10.0, rect.center().y), Align2::RIGHT_CENTER, n.to_string(), theme::font(13.0), c);
        }
        resp
    }

    fn tags_section(&mut self, ui: &mut Ui, p: &Palette) {
        let tags = self.all_tags();
        if tags.is_empty() {
            return;
        }
        // Folded like an account: click the header; remembered with them.
        let collapsed = self.lib.settings.collapsed.iter().any(|c| c == TAGS_KEY);
        let (rect, resp) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 30.0), Sense::click());
        ui.painter().text(Pos2::new(rect.left() + 16.0, rect.center().y + 2.0), Align2::LEFT_CENTER, "Tags", theme::bold_font(12.0), p.secondary);
        if resp.hovered() {
            let icon = if collapsed { Icon::ChevronRight } else { Icon::ChevronDown };
            let right = Pos2::new(rect.right() - 16.0, rect.center().y + 2.0);
            icons::paint(ui.painter(), icon, Rect::from_center_size(right, Vec2::splat(12.0)), p.secondary);
        }
        if resp.clicked() {
            if collapsed {
                self.lib.settings.collapsed.retain(|c| c != TAGS_KEY);
            } else {
                self.lib.settings.collapsed.push(TAGS_KEY.to_owned());
            }
        }
        if collapsed {
            return;
        }
        ui.add_space(4.0);
        egui::Frame::none().inner_margin(egui::Margin { left: 12.0, right: 8.0, top: 0.0, bottom: 0.0 }).show(ui, |ui| ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing = Vec2::new(6.0, 6.0);
            for t in tags {
                let loc = Location::Tag(t.clone());
                let selected = self.location == loc && self.search.is_empty();
                let text = format!("#{t}");
                let galley = ui.painter().layout_no_wrap(text, theme::font(12.5), if selected { egui::Color32::WHITE } else { p.text });
                let size = galley.size() + Vec2::new(18.0, 8.0);
                let (r, resp) = ui.allocate_exact_size(size, Sense::click());
                let fill = if selected { p.folder_selection } else if resp.hovered() { p.hover } else { p.field };
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
        ui.painter().rect_filled(rect, rect.height() / 2.0, p.hover);
    }
    let color = if !enabled { p.tertiary } else { p.text };
    icons::paint(ui.painter(), icon, rect.shrink(rect.height() * 0.2), color);
    let resp = resp.on_hover_text(tip);
    enabled && resp.clicked()
}

/// Paints a "glass" shape: a raised, softly outlined round rect.
pub fn glass(ui: &Ui, p: &Palette, rect: Rect) {
    let r = rect.height() / 2.0;
    let painter = ui.painter();
    painter.rect_filled(rect.translate(Vec2::new(0.0, 1.5)), r, egui::Color32::from_black_alpha(if p.dark { 70 } else { 18 }));
    painter.rect(rect, r, p.glass, egui::Stroke::new(1.0_f32, p.glass_edge));
}

/// A round glass toolbar button (Apple's New Note, Search, "…").
pub fn glass_button(ui: &mut Ui, p: &Palette, center: Pos2, icon: Icon, tip: &str, enabled: bool) -> egui::Response {
    let rect = Rect::from_center_size(center, Vec2::splat(36.0));
    glass(ui, p, rect);
    capsule_item(ui, p, rect, icon, tip, enabled, false)
}

/// A button inside a glass capsule (or a glass button's face).
pub fn capsule_item(ui: &mut Ui, p: &Palette, rect: Rect, icon: Icon, tip: &str, enabled: bool, on: bool) -> egui::Response {
    let resp = ui.interact(rect, ui.id().with(("glass", tip)), if enabled { Sense::click() } else { Sense::hover() });
    if enabled && (resp.hovered() || on) {
        ui.painter().rect_filled(rect.shrink(3.0), (rect.height() - 6.0) / 2.0, p.hover);
    }
    let color = if enabled { p.text } else { p.tertiary };
    icons::paint(ui.painter(), icon, Rect::from_center_size(rect.center(), Vec2::splat(rect.height() * 0.56)), color);
    resp.on_hover_text(tip)
}

/// The frame of Apple-style menus and popovers.
pub fn popover_frame(ui: &Ui, p: &Palette) -> egui::Frame {
    egui::Frame::popup(ui.style())
        .fill(p.popover)
        .rounding(14.0)
        .stroke(egui::Stroke::new(1.0_f32, p.glass_edge))
        .inner_margin(egui::Margin::same(6.0))
}

/// A popover menu below `anchor`, open while `id` is egui's open popup:
/// as `egui::popup_below_widget`, but in our frame only (egui's would draw
/// a second border around it). Closed by Escape or a click outside it.
pub fn popover_below<R>(ui: &Ui, p: &Palette, id: egui::Id, anchor: &egui::Response, add_contents: impl FnOnce(&mut Ui) -> R) -> Option<R> {
    if !ui.memory(|m| m.is_popup_open(id)) {
        return None;
    }
    let area = egui::Area::new(id)
        .kind(egui::UiKind::Popup)
        .order(egui::Order::Foreground)
        .fixed_pos(anchor.rect.left_bottom() + Vec2::new(0.0, 4.0))
        .pivot(Align2::LEFT_TOP)
        .show(ui.ctx(), |ui| {
            popover_frame(ui, p)
                .show(ui, |ui| {
                    menu_look(ui, p);
                    ui.with_layout(egui::Layout::top_down_justified(egui::Align::LEFT), add_contents).inner
                })
                .inner
        });
    if ui.input(|i| i.key_pressed(Key::Escape)) || (anchor.clicked_elsewhere() && area.response.clicked_elsewhere()) {
        ui.memory_mut(|m| m.close_popup());
    }
    Some(area.inner)
}

/// Menu items without a frame, highlighted when hovered (as in Apple's
/// menus).
pub fn menu_look(ui: &mut Ui, p: &Palette) {
    let v = ui.visuals_mut();
    v.widgets.inactive.weak_bg_fill = egui::Color32::TRANSPARENT;
    v.widgets.inactive.bg_stroke = egui::Stroke::NONE;
    for w in [&mut v.widgets.hovered, &mut v.widgets.active, &mut v.widgets.open] {
        w.weak_bg_fill = p.hover;
        w.bg_stroke = egui::Stroke::NONE;
        w.rounding = egui::Rounding::same(7.0);
    }
    ui.spacing_mut().button_padding = egui::vec2(10.0, 5.0);
    ui.style_mut().text_styles.insert(egui::TextStyle::Button, theme::font(14.0));
}

/// A popover menu item with a ">" at its end that opens `add_contents` to
/// its right when hovered, as Apple's submenus do. (egui's own submenus
/// only work inside its menus; in a popover `menu_button` drops down.)
/// Clicking in the submenu closes the popover, being a click outside it.
pub fn submenu(ui: &mut Ui, p: &Palette, label: &str, add_contents: impl FnOnce(&mut Ui)) {
    let id = ui.id().with(("submenu", label));
    // Which of this menu's submenus is open, and the last frame it was
    // shown (so it isn't open again when the popover next opens).
    let open_key = ui.id().with("open-submenu");
    let frame = ui.ctx().cumulative_pass_nr();
    let open_now = ui.data(|d| d.get_temp::<(egui::Id, u64)>(open_key)).is_some_and(|(o, f)| o == id && f + 1 >= frame);

    let bg = ui.painter().add(egui::Shape::Noop);
    let btn = egui::Button::new(label).min_size(Vec2::new(ui.available_width(), 0.0));
    let resp = ui.add(btn);
    let r = resp.rect;
    icons::paint(ui.painter(), Icon::ChevronRight, Rect::from_center_size(Pos2::new(r.right() - 14.0, r.center().y), Vec2::splat(13.0)), p.secondary);

    // Open while hovered or while the pointer is in it; another item of
    // the menu hovered closes it.
    let sub_rect_key = id.with("rect");
    let sub_rect = ui.data(|d| d.get_temp::<Rect>(sub_rect_key));
    let pointer = ui.ctx().pointer_hover_pos();
    let in_sub = open_now && pointer.zip(sub_rect).is_some_and(|(pos, s)| s.contains(pos));
    let on_menu = pointer.is_some_and(|pos| ui.ctx().layer_id_at(pos) == Some(ui.layer_id()));
    let open = resp.hovered() || resp.clicked() || in_sub || (open_now && !on_menu);
    if !open {
        if open_now {
            ui.data_mut(|d| d.remove::<(egui::Id, u64)>(open_key));
        }
        return;
    }
    ui.data_mut(|d| d.insert_temp(open_key, (id, frame)));
    if !resp.hovered() {
        ui.painter().set(bg, egui::Shape::rect_filled(r, 7.0, p.hover));
    }

    // To its right, its first item level with this one; to its left if
    // there's no room.
    let screen = ui.ctx().screen_rect();
    let width = sub_rect.map_or(180.0, |s| s.width());
    let margin = 7.0; // the popover frame's margin and stroke
    let x = if r.right() + margin + width <= screen.right() { r.right() + margin } else { r.left() - margin - width };
    let top = (r.top() - margin).min(screen.bottom() - sub_rect.map_or(0.0, |s| s.height())).max(screen.top());
    let area = egui::Area::new(id.with("area")).order(egui::Order::Foreground).fixed_pos(Pos2::new(x, top)).show(ui.ctx(), |ui| {
        popover_frame(ui, p).show(ui, |ui| {
            menu_look(ui, p);
            ui.set_min_width(160.0);
            egui::ScrollArea::vertical().max_height(screen.bottom() - top - 20.0).show(ui, add_contents);
        });
    });
    ui.ctx().move_to_top(area.response.layer_id);
    ui.data_mut(|d| d.insert_temp(sub_rect_key, area.response.rect));
}
