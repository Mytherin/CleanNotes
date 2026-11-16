//! The notes list (titles, times, snippets in date sections) and the
//! gallery view (note thumbnails in a grid).

use eframe::egui::{self, Align2, Pos2, Rect, Sense, Ui, Vec2};

use super::sidebar::{glass_button, menu_look, popover_frame, tool_button, truncate, NoteDrag};
use super::{list_time, App, Confirm, Location, Pane, TOOLBAR_H};
use crate::icons::Icon;
use crate::markdown;
use crate::settings::ListView;
use crate::theme::{self, Palette};

const ROW_H: f32 = 64.0;

impl App {
    /// What's shown in the list, for its header.
    fn location_title(&self) -> String {
        if !self.search.trim().is_empty() {
            return "Search".into();
        }
        match &self.location {
            Location::All => "All Notes".into(),
            Location::Folder(f) => self.store.folder(f).map(|f| f.name.clone()).unwrap_or_default(),
            Location::Deleted(_) => "Recently Deleted".into(),
            Location::Tag(t) => format!("#{t}"),
        }
    }

    /// The list's header: the folder's name and note count, and the "…"
    /// view menu at `more_x`. Returns where the header text ends.
    fn list_header(&mut self, ui: &mut Ui, p: &Palette, bar: Rect, more_x: f32) -> f32 {
        let y = bar.center().y;
        let mut x = bar.left() + 18.0;
        if !self.settings.sidebar_visible {
            if tool_button(ui, p, Rect::from_center_size(Pos2::new(x + 12.0, y), Vec2::splat(30.0)), Icon::Sidebar, "Show Folders (Ctrl+Alt+S)") {
                self.settings.sidebar_visible = true;
            }
            x += 40.0;
        }
        let n = self.shown.len();
        let max_w = (more_x - 24.0 - x).max(40.0);
        let title = truncate(&self.location_title(), max_w, ui);
        ui.painter().text(Pos2::new(x, y - 8.0), Align2::LEFT_CENTER, title, theme::bold_font(15.0), p.text);
        let count = if n == 1 { "1 note".to_owned() } else { format!("{n} notes") };
        ui.painter().text(Pos2::new(x, y + 11.0), Align2::LEFT_CENTER, count, theme::font(12.5), p.secondary);

        let resp = glass_button(ui, p, Pos2::new(more_x, y), Icon::More, "View Options", true);
        let id = ui.id().with("view-menu");
        if resp.clicked() {
            ui.memory_mut(|m| m.toggle_popup(id));
        }
        egui::popup_below_widget(ui, id, &resp, egui::PopupCloseBehavior::CloseOnClickOutside, |ui| {
            popover_frame(ui, p).show(ui, |ui| {
                menu_look(ui, p);
                ui.set_min_width(210.0);
                let mut close = false;
                let gallery = self.settings.view == ListView::Gallery;
                let (label, view) = if gallery { ("View as List", ListView::List) } else { ("View as Gallery", ListView::Gallery) };
                if ui.add(egui::Button::new(label).shortcut_text(if gallery { cmd_key!("1") } else { cmd_key!("2") })).clicked() {
                    self.settings.view = view;
                    close = true;
                }
                ui.separator();
                ui.menu_button("Sort By", |ui| {
                    for o in crate::settings::SortBy::ALL {
                        if ui.radio(self.settings.sort_by == o, o.label()).clicked() {
                            self.settings.sort_by = o;
                            ui.close_menu();
                        }
                    }
                });
                ui.menu_button("Group By Date", |ui| {
                    for (on, l) in [(true, "On"), (false, "Off")] {
                        if ui.radio(self.settings.group_by_date == on, l).clicked() {
                            self.settings.group_by_date = on;
                            ui.close_menu();
                        }
                    }
                });
                if ui.checkbox(&mut self.settings.show_note_count, "Show Note Count").clicked() {
                    close = true;
                }
                if close {
                    ui.memory_mut(|m| m.close_popup());
                }
            });
        });
        x
    }

    pub(super) fn notes_list(&mut self, ui: &mut Ui, p: &Palette) {
        let full = ui.max_rect();
        let bar = Rect::from_min_size(full.min, Vec2::new(full.width(), TOOLBAR_H));
        ui.allocate_rect(bar, Sense::hover());
        let sections = self.list_sections();
        self.shown = sections.iter().flat_map(|(_, ids)| ids.clone()).collect();
        self.list_header(ui, p, bar, bar.right() - 30.0);

        let area = Rect::from_min_max(Pos2::new(full.left(), bar.bottom() + 4.0), full.max);
        let mut child = ui.new_child(egui::UiBuilder::new().max_rect(area).layout(egui::Layout::top_down(egui::Align::LEFT)));
        if self.shown.is_empty() {
            let msg = if !self.search.trim().is_empty() { "No Results" } else { "No Notes" };
            child.painter().text(area.center() - Vec2::new(0.0, 40.0), Align2::CENTER_CENTER, msg, theme::font(15.0), p.secondary);
            return;
        }
        let show_folder = !self.search.trim().is_empty() || matches!(self.location, Location::Tag(_) | Location::All);
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(&mut child, |ui| {
            ui.spacing_mut().item_spacing.y = 0.0;
            for (name, ids) in &sections {
                if !name.is_empty() {
                    // Apple's big section titles, with a line under them.
                    let (r, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 44.0), Sense::hover());
                    ui.painter().text(Pos2::new(r.left() + 18.0, r.bottom() - 18.0), Align2::LEFT_CENTER, name, theme::bold_font(16.0), p.text);
                    ui.painter().line_segment(
                        [Pos2::new(r.left() + 16.0, r.bottom() - 0.5), Pos2::new(r.right() - 16.0, r.bottom() - 0.5)],
                        egui::Stroke::new(1.0_f32, p.separator),
                    );
                    ui.add_space(6.0);
                }
                for (i, id) in ids.iter().enumerate() {
                    let next_selected = ids.get(i + 1).is_some_and(|n| self.is_selected(n));
                    self.note_row(ui, p, id, show_folder, i + 1 < ids.len() && !next_selected);
                }
                ui.add_space(14.0);
            }
        });
        self.scroll_list = false;
    }

    fn note_row(&mut self, ui: &mut Ui, p: &Palette, id: &str, show_folder: bool, separator: bool) {
        let Some(n) = self.store.note(id) else { return };
        let title = n.title();
        let title = if title.is_empty() { "New Note".to_owned() } else { title };
        let locked = crate::lock::is_locked(&n.body);
        let snippet = if locked { "Locked".to_owned() } else { markdown::snippet_of(&n.body, 120) };
        let snippet = if snippet.is_empty() { "No additional text".to_owned() } else { snippet };
        let when = if n.deleted.is_some() && matches!(self.location, Location::Deleted(_)) { n.deleted.unwrap() } else if self.settings.sort_by == crate::settings::SortBy::Created { n.created } else { n.modified };
        let time = list_time(when);
        let folder = show_folder.then(|| self.store.folder(&n.folder).map(|f| f.name.clone())).flatten();
        let h = if folder.is_some() { ROW_H + 16.0 } else { ROW_H };
        let selected = self.is_selected(id);
        let active = selected && self.focus == Pane::List;

        let (rect, resp) = ui.allocate_exact_size(Vec2::new(ui.available_width(), h), Sense::click_and_drag());
        if selected && self.scroll_list {
            ui.scroll_to_rect(rect, None);
        }
        let inner = rect.shrink2(Vec2::new(10.0, 0.0));
        let painter = ui.painter();
        if selected {
            painter.rect_filled(inner, 10.0, if active { p.selection } else { p.selection_inactive });
        } else if separator {
            painter.line_segment(
                [Pos2::new(inner.left() + 12.0, rect.bottom() - 0.5), Pos2::new(inner.right() - 4.0, rect.bottom() - 0.5)],
                egui::Stroke::new(1.0_f32, p.separator),
            );
        }
        let text = p.on_selection(active);
        let sub = if active { theme::blend(p.selection, p.selection_text, 0.6) } else { p.secondary };
        let x = inner.left() + 16.0;
        let w = inner.width() - 32.0;
        let title_line = truncate(&title, w, ui);
        painter.text(Pos2::new(x, rect.top() + 18.0), Align2::LEFT_CENTER, title_line, theme::bold_font(13.5), text);
        let tg = painter.layout_no_wrap(time.clone(), theme::bold_font(12.5), text);
        let tw = tg.size().x;
        painter.galley(Pos2::new(x, rect.top() + 38.0 - tg.size().y / 2.0), tg, text);
        let mut sx = x + tw + 7.0;
        if locked {
            crate::icons::paint(painter, Icon::Lock, Rect::from_center_size(Pos2::new(sx + 5.0, rect.top() + 38.0), Vec2::splat(11.0)), sub);
            sx += 14.0;
        }
        let snip = truncate(&snippet, w - tw - 8.0 - (sx - x - tw - 7.0), ui);
        painter.text(Pos2::new(sx, rect.top() + 38.0), Align2::LEFT_CENTER, snip, theme::font(12.5), sub);
        if let Some(f) = folder {
            crate::icons::paint(painter, Icon::Folder, Rect::from_center_size(Pos2::new(x + 6.0, rect.top() + 58.0), Vec2::splat(12.0)), sub);
            painter.text(Pos2::new(x + 16.0, rect.top() + 58.0), Align2::LEFT_CENTER, f, theme::font(11.5), sub);
        }

        self.note_clicks(ui, &resp, id);
        if resp.double_clicked() {
            if let Some(e) = &mut self.editor {
                e.focus_requested = true;
                self.focus = Pane::Note;
            }
        }
        self.note_menu(&resp, id);
    }

    /// Clicks on a note in the list or gallery, and dragging it (or all
    /// the selected notes, if it's one of them).
    fn note_clicks(&mut self, ui: &Ui, resp: &egui::Response, id: &str) {
        if resp.clicked() {
            self.click_note(id, ui.input(|i| i.modifiers));
            self.focus = Pane::List;
        } else if resp.secondary_clicked() && !self.is_selected(id) {
            self.select(Some(id.to_owned()));
            self.focus = Pane::List;
        }
        let drag = if self.is_selected(id) { self.selected_ids() } else { vec![id.to_owned()] };
        resp.dnd_set_drag_payload(NoteDrag(drag));
    }

    /// The right-click menu of a note in the list or gallery; with several
    /// notes selected, it acts on all of them.
    fn note_menu(&mut self, resp: &egui::Response, id: &str) {
        let Some(n) = self.store.note(id).cloned() else { return };
        let ids = if self.is_selected(id) { self.selected_ids() } else { vec![id.to_owned()] };
        let several = ids.len() > 1;
        let noun = if several { "Notes" } else { "Note" };
        resp.context_menu(|ui| {
            ui.set_min_width(190.0);
            if n.deleted.is_some() {
                if ui.button("Recover").clicked() {
                    for id in &ids {
                        self.recover_note(id);
                    }
                    ui.close_menu();
                }
                if ui.button("Delete Permanently…").clicked() {
                    self.confirm = Some(Confirm::DeleteNotesForever(ids.clone()));
                    ui.close_menu();
                }
                return;
            }
            let all_pinned = ids.iter().all(|i| self.store.note(i).is_some_and(|n| n.pinned));
            if ui.button(format!("{} {noun}", if all_pinned { "Unpin" } else { "Pin" })).clicked() {
                for id in &ids {
                    if self.store.note(id).is_some_and(|n| n.pinned == all_pinned) {
                        self.toggle_pin(id);
                    }
                }
                ui.close_menu();
            }
            ui.menu_button("Move to", |ui| self.move_to_menu(ui, &ids));
            if !several && ui.add(egui::Button::new("Duplicate Note").shortcut_text(cmd_key!("D"))).clicked() {
                self.duplicate_note(id);
                ui.close_menu();
            }
            // Locks those not locked yet; hidden once they all are.
            let to_lock = ids.iter().filter(|i| self.can_lock(i)).count();
            if to_lock > 0 {
                let label = if to_lock > 1 { format!("Lock {to_lock} Notes") } else { "Lock Note".to_owned() };
                if ui.button(label).clicked() {
                    self.lock_notes(&ids);
                    ui.close_menu();
                }
            }
            if ui.button("Copy as Markdown").clicked() {
                let text: Vec<String> = ids.iter().filter_map(|i| self.plain_body(i)).collect();
                ui.ctx().copy_text(text.join("

---

"));
                ui.close_menu();
            }
            ui.separator();
            let label = if several { format!("Delete {} Notes", ids.len()) } else { "Delete".to_owned() };
            if ui.add(egui::Button::new(label).shortcut_text(cmd_key!("Backspace"))).clicked() {
                self.delete_notes(ids.clone());
                ui.close_menu();
            }
        });
    }

    /// The folders of every account, to move `ids` into one.
    pub(super) fn move_to_menu(&mut self, ui: &mut Ui, ids: &[String]) {
        let accounts: Vec<(String, String)> = self.store.accounts.iter().filter(|a| a.enabled).map(|a| (a.id.clone(), a.name.clone())).collect();
        let several = accounts.len() > 1;
        for (aid, aname) in accounts {
            if several {
                ui.label(egui::RichText::new(aname).small().strong());
            }
            let folders: Vec<(String, String, usize)> =
                self.store.folder_tree(&aid).into_iter().map(|(f, d)| (f.id.clone(), f.name.clone(), d)).collect();
            for (fid, fname, depth) in folders {
                let label = format!("{}{}", "    ".repeat(depth), fname);
                let all_there = ids.iter().all(|i| self.store.note(i).is_some_and(|n| n.folder == fid));
                if ui.add_enabled(!all_there, egui::Button::new(label)).clicked() {
                    self.move_notes(ids, &fid);
                    ui.close_menu();
                    ui.memory_mut(|m| m.close_popup());
                }
            }
        }
    }

    /// The gallery: thumbnails in a grid; double-click opens a note.
    pub(super) fn gallery(&mut self, ui: &mut Ui, p: &Palette) {
        if self.gallery_open && self.editor.is_some() {
            self.note_pane(ui, p);
            return;
        }
        self.gallery_open = false;
        let full = ui.max_rect();
        let bar = Rect::from_min_size(full.min, Vec2::new(full.width(), TOOLBAR_H));
        ui.allocate_rect(bar, Sense::hover());
        let sections = self.list_sections();
        self.shown = sections.iter().flat_map(|(_, ids)| ids.clone()).collect();
        let left = self.search_control(ui, p, bar.right() - 14.0, bar.center().y);
        if glass_button(ui, p, Pos2::new(left - 28.0, bar.center().y), Icon::Compose, "New Note (Ctrl+N)", true).clicked() {
            self.new_note();
            self.gallery_open = true;
        }
        self.list_header(ui, p, bar, left - 74.0);
        let area = Rect::from_min_max(Pos2::new(full.left(), bar.bottom() + 1.0), full.max);
        let mut child = ui.new_child(egui::UiBuilder::new().max_rect(area).layout(egui::Layout::top_down(egui::Align::LEFT)));
        if self.shown.is_empty() {
            child.painter().text(area.center(), Align2::CENTER_CENTER, "No Notes", theme::font(15.0), p.secondary);
            return;
        }
        let (card_w, card_h) = (172.0, 132.0);
        let gap = 22.0;
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(&mut child, |ui| {
            let cols = (((ui.available_width() - gap) / (card_w + gap)).floor() as usize).max(1);
            let left = ui.max_rect().left() + (ui.available_width() - cols as f32 * (card_w + gap) + gap) / 2.0;
            for (name, ids) in &sections {
                if !name.is_empty() {
                    let (r, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 38.0), Sense::hover());
                    ui.painter().text(Pos2::new(left, r.bottom() - 10.0), Align2::LEFT_CENTER, name, theme::bold_font(14.0), p.text);
                }
                for chunk in ids.chunks(cols) {
                    let (row, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), card_h + 56.0), Sense::hover());
                    for (i, id) in chunk.iter().enumerate() {
                        let r = Rect::from_min_size(Pos2::new(left + i as f32 * (card_w + gap), row.top() + 8.0), Vec2::new(card_w, card_h));
                        self.card(ui, p, id, r);
                    }
                }
            }
        });
    }

    fn card(&mut self, ui: &mut Ui, p: &Palette, id: &str, r: Rect) {
        let Some(n) = self.store.note(id) else { return };
        let selected = self.is_selected(id);
        let painter = ui.painter();
        painter.rect(r, 10.0, p.card, egui::Stroke::new(if selected { 3.0_f32 } else { 1.0 }, if selected { p.accent } else { p.separator }));
        if crate::lock::is_locked(&n.body) {
            crate::icons::paint(painter, Icon::Lock, Rect::from_center_size(r.center(), Vec2::splat(30.0)), p.secondary);
        }
        // A miniature of the note.
        let clip = painter.with_clip_rect(r.shrink(8.0));
        let body = if crate::lock::is_locked(&n.body) { n.title() } else { n.body.clone() };
        let mut y = r.top() + 10.0;
        for (i, line) in body.lines().filter(|l| !l.trim().is_empty()).take(9).enumerate() {
            let b = markdown::from_markdown(line);
            let text = b.blocks.first().map(|b| b.text.clone()).unwrap_or_default();
            let (font, color) = if i == 0 { (theme::bold_font(10.5), p.text) } else { (theme::font(9.0), p.secondary) };
            let g = clip.layout(text, font, color, r.width() - 20.0);
            let h = g.size().y;
            clip.galley(Pos2::new(r.left() + 10.0, y), g, color);
            y += h + 1.0;
            if y > r.bottom() {
                break;
            }
        }
        let title = n.title();
        let title = if title.is_empty() { "New Note".to_owned() } else { title };
        let t = truncate(&title, r.width(), ui);
        painter.text(Pos2::new(r.center().x, r.bottom() + 14.0), Align2::CENTER_CENTER, t, theme::bold_font(12.5), p.text);
        painter.text(Pos2::new(r.center().x, r.bottom() + 31.0), Align2::CENTER_CENTER, list_time(n.modified), theme::font(11.5), p.secondary);
        let resp = ui.interact(r, ui.id().with(("card", id)), Sense::click_and_drag());
        self.note_clicks(ui, &resp, id);
        if resp.double_clicked() {
            self.select(Some(id.to_owned()));
            self.gallery_open = true;
        }
        self.note_menu(&resp, id);
    }
}
