//! The notes list (titles, times, snippets in date sections) and the
//! gallery view (note thumbnails in a grid).

use eframe::egui::{self, Align2, Pos2, Rect, Sense, Ui, Vec2};

use super::sidebar::{tool_button, tool_button_state, truncate, NoteDrag};
use super::{list_time, App, Confirm, Location, Pane, TOOLBAR_H};
use crate::icons::Icon;
use crate::markdown;
use crate::settings::ListView;
use crate::theme::{self, Palette};

const ROW_H: f32 = 64.0;

impl App {
    /// The list/gallery switch and the delete button, from `x`.
    fn view_controls(&mut self, ui: &mut Ui, p: &Palette, bar: Rect, mut x: f32) -> f32 {
        let y = bar.center().y;
        if !self.settings.sidebar_visible {
            if tool_button(ui, p, Rect::from_center_size(Pos2::new(x + 14.0, y), Vec2::splat(28.0)), Icon::Sidebar, "Show Folders (Ctrl+Alt+S)") {
                self.settings.sidebar_visible = true;
            }
            x += 36.0;
        }
        // Segmented control.
        let track = Rect::from_min_size(Pos2::new(x, y - 13.0), Vec2::new(64.0, 26.0));
        ui.painter().rect_filled(track, 7.0, p.field);
        for (i, (view, icon, tip)) in [(ListView::List, Icon::List, "View as List (Ctrl+1)"), (ListView::Gallery, Icon::Gallery, "View as Gallery (Ctrl+2)")]
            .into_iter()
            .enumerate()
        {
            let r = Rect::from_min_size(Pos2::new(track.left() + 2.0 + i as f32 * 30.0, track.top() + 2.0), Vec2::new(30.0, 22.0));
            if self.settings.view == view {
                ui.painter().rect_filled(r, 5.5, if p.dark { p.hover } else { p.bg });
            }
            let resp = ui.interact(r, ui.id().with(tip), Sense::click()).on_hover_text(tip);
            crate::icons::paint(ui.painter(), icon, r.shrink(4.0), p.secondary);
            if resp.clicked() {
                self.settings.view = view;
            }
        }
        x = track.right() + 10.0;
        let can_delete = self.selected.is_some();
        if tool_button_state(ui, p, Rect::from_center_size(Pos2::new(x + 14.0, y), Vec2::splat(28.0)), Icon::Trash, "Delete", can_delete, false) {
            self.delete_notes(self.selected_ids());
        }
        x + 36.0
    }

    pub(super) fn notes_list(&mut self, ui: &mut Ui, p: &Palette) {
        let full = ui.max_rect();
        let bar = Rect::from_min_size(full.min, Vec2::new(full.width(), TOOLBAR_H));
        ui.allocate_rect(bar, Sense::hover());
        self.view_controls(ui, p, bar, bar.left() + 12.0);

        let sections = self.list_sections();
        self.shown = sections.iter().flat_map(|(_, ids)| ids.clone()).collect();
        let area = Rect::from_min_max(Pos2::new(full.left(), bar.bottom()), full.max);
        let mut child = ui.new_child(egui::UiBuilder::new().max_rect(area).layout(egui::Layout::top_down(egui::Align::LEFT)));
        if self.shown.is_empty() {
            let msg = if !self.search.trim().is_empty() { "No Results" } else { "No Notes" };
            child.painter().text(area.center() - Vec2::new(0.0, 40.0), Align2::CENTER_CENTER, msg, theme::font(15.0), p.secondary);
            return;
        }
        let show_folder = !self.search.trim().is_empty() || matches!(self.location, Location::Tag(_));
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(&mut child, |ui| {
            ui.spacing_mut().item_spacing.y = 0.0;
            for (name, ids) in &sections {
                if !name.is_empty() {
                    let (r, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 30.0), Sense::hover());
                    ui.painter().text(Pos2::new(r.left() + 18.0, r.bottom() - 9.0), Align2::LEFT_CENTER, name, theme::bold_font(13.0), p.text);
                }
                for (i, id) in ids.iter().enumerate() {
                    let next_selected = ids.get(i + 1).is_some_and(|n| self.is_selected(n));
                    self.note_row(ui, p, id, show_folder, i + 1 < ids.len() && !next_selected);
                }
                ui.add_space(6.0);
            }
        });
        self.scroll_list = false;
    }

    fn note_row(&mut self, ui: &mut Ui, p: &Palette, id: &str, show_folder: bool, separator: bool) {
        let Some(n) = self.store.note(id) else { return };
        let title = n.title();
        let title = if title.is_empty() { "New Note".to_owned() } else { title };
        let snippet = markdown::snippet_of(&n.body, 120);
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
        let inner = rect.shrink2(Vec2::new(8.0, 0.0));
        let painter = ui.painter();
        if selected {
            painter.rect_filled(inner, 7.0, if active { p.selection } else { p.selection_inactive });
        } else if separator {
            painter.line_segment(
                [Pos2::new(inner.left() + 12.0, rect.bottom() - 0.5), Pos2::new(inner.right() - 4.0, rect.bottom() - 0.5)],
                egui::Stroke::new(1.0_f32, p.separator),
            );
        }
        let text = p.on_selection(active);
        let sub = if active { theme::blend(p.selection, p.selection_text, 0.6) } else { p.secondary };
        let x = inner.left() + 12.0;
        let w = inner.width() - 24.0;
        let title_line = truncate(&title, w, ui);
        painter.text(Pos2::new(x, rect.top() + 18.0), Align2::LEFT_CENTER, title_line, theme::bold_font(13.5), text);
        let tg = painter.layout_no_wrap(time.clone(), theme::bold_font(12.5), text);
        let tw = tg.size().x;
        painter.galley(Pos2::new(x, rect.top() + 38.0 - tg.size().y / 2.0), tg, text);
        let snip = truncate(&snippet, w - tw - 8.0, ui);
        painter.text(Pos2::new(x + tw + 7.0, rect.top() + 38.0), Align2::LEFT_CENTER, snip, theme::font(12.5), sub);
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
            ui.menu_button("Move to", |ui| {
                let accounts: Vec<(String, String)> =
                    self.store.accounts.iter().filter(|a| a.enabled).map(|a| (a.id.clone(), a.name.clone())).collect();
                let several = accounts.len() > 1;
                for (aid, aname) in accounts {
                    if several {
                        ui.label(egui::RichText::new(aname).small().strong());
                    }
                    let folders: Vec<(String, String, usize)> = self
                        .store
                        .folder_tree(&aid)
                        .into_iter()
                        .map(|(f, d)| (f.id.clone(), f.name.clone(), d))
                        .collect();
                    for (fid, fname, depth) in folders {
                        let label = format!("{}{}", "    ".repeat(depth), fname);
                        let all_there = ids.iter().all(|i| self.store.note(i).is_some_and(|n| n.folder == fid));
                        if ui.add_enabled(!all_there, egui::Button::new(label)).clicked() {
                            self.move_notes(&ids, &fid);
                            ui.close_menu();
                        }
                    }
                }
            });
            if !several && ui.add(egui::Button::new("Duplicate Note").shortcut_text(cmd_key!("D"))).clicked() {
                self.duplicate_note(id);
                ui.close_menu();
            }
            if ui.button("Copy as Markdown").clicked() {
                let text: Vec<String> = ids.iter().filter_map(|i| self.store.note(i).map(|n| n.body.clone())).collect();
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
        let x = self.view_controls(ui, p, bar, bar.left() + 12.0);
        if tool_button(ui, p, Rect::from_center_size(Pos2::new(x + 14.0, bar.center().y), Vec2::splat(28.0)), Icon::Compose, "New Note (Ctrl+N)") {
            self.new_note();
            self.gallery_open = true;
        }
        self.search_field(ui, p, bar);
        ui.painter().line_segment([Pos2::new(full.left(), bar.bottom()), Pos2::new(full.right(), bar.bottom())], egui::Stroke::new(1.0_f32, p.separator));

        let sections = self.list_sections();
        self.shown = sections.iter().flat_map(|(_, ids)| ids.clone()).collect();
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
        // A miniature of the note.
        let clip = painter.with_clip_rect(r.shrink(8.0));
        let mut y = r.top() + 10.0;
        for (i, line) in n.body.lines().filter(|l| !l.trim().is_empty()).take(9).enumerate() {
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
