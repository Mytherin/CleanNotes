//! The note pane: its toolbar (new note, the Aa format menu, checklist,
//! link, search), the find bar, and the editor.

use eframe::egui::{self, Align2, Key, Pos2, Rect, Sense, Ui, Vec2};

use super::sidebar::{capsule_item, glass, glass_button, menu_look, popover_frame, tool_button};
use super::{modal, note_header_time, App, Confirm, Pane, TOOLBAR_H};
use crate::doc::{Attr, Kind};
use crate::editor::{Command, Options};
use crate::icons::Icon;
use crate::theme::{self, Palette};

/// The Aa menu's paragraph styles: label, style, shortcut.
const STYLES: [(&str, Kind, &str); 9] = [
    ("Title", Kind::Title, cmd_key!("Shift+T")),
    ("Heading", Kind::Heading, cmd_key!("Shift+H")),
    ("Subheading", Kind::Subheading, cmd_key!("Shift+J")),
    ("Body", Kind::Body, cmd_key!("Shift+B")),
    ("Monostyled", Kind::Monospaced, cmd_key!("Shift+M")),
    ("•  Bulleted List", Kind::Bulleted, cmd_key!("Shift+7")),
    ("–  Dashed List", Kind::Dashed, cmd_key!("Shift+8")),
    ("1.  Numbered List", Kind::Numbered, cmd_key!("Shift+9")),
    ("Block Quote", Kind::Quote, cmd_key!("'")),
];

impl App {
    pub(super) fn note_pane(&mut self, ui: &mut Ui, p: &Palette) {
        let full = ui.max_rect();
        let bar = Rect::from_min_size(full.min, Vec2::new(full.width(), TOOLBAR_H));
        ui.allocate_rect(bar, Sense::hover());
        let y = bar.center().y;
        let read_only = self.selected.as_ref().and_then(|s| self.lib.store.note(s)).is_some_and(|n| n.deleted.is_some());
        let has_note = self.editor.is_some();
        let mut x = bar.left() + 14.0;
        if self.gallery_open {
            if tool_button(ui, p, Rect::from_center_size(Pos2::new(x + 15.0, y), Vec2::splat(30.0)), Icon::ChevronRight, "Back to Gallery") {
                self.gallery_open = false;
            }
            x += 40.0;
        }
        // New Note, in a round glass button.
        if glass_button(ui, p, Pos2::new(x + 18.0, y), Icon::Compose, "New Note (Ctrl+N)", true).clicked() {
            self.new_note();
        }
        let compose_right = x + 36.0;

        // Lock, "…" and search on the right.
        let search_left = self.search_control(ui, p, bar.right() - 14.0, y);
        let more = glass_button(ui, p, Pos2::new(search_left - 28.0, y), Icon::More, "More", self.selected.is_some() && self.multi.len() < 2);
        self.note_more_menu(ui, p, &more);
        self.lock_button(ui, p, Pos2::new(search_left - 72.0, y));

        // The formatting buttons, in a glass capsule in the middle.
        let editable = has_note && !read_only;
        let item = 40.0;
        let cap_w = item * 4.0 + 8.0;
        let room = (compose_right + 12.0, search_left - 100.0 - 12.0);
        let cx = (bar.center().x).clamp(room.0 + cap_w / 2.0, (room.1 - cap_w / 2.0).max(room.0 + cap_w / 2.0));
        let cap = Rect::from_center_size(Pos2::new(cx, y), Vec2::new(cap_w, 36.0));
        glass(ui, p, cap);
        let slot = |i: usize| Rect::from_min_size(Pos2::new(cap.left() + 4.0 + i as f32 * item, cap.top()), Vec2::new(item, 36.0));
        let aa_rect = slot(0);
        if capsule_item(ui, p, aa_rect, Icon::Format, "Format", editable, self.format_open).clicked() && editable {
            self.format_open = !self.format_open;
        }
        if capsule_item(ui, p, slot(1), Icon::Checklist, &format!("Checklist ({})", cmd_key!("Shift+L")), editable, false).clicked() && editable {
            self.editor_command(Command::SetKind(Kind::Checklist(false)));
        }
        if capsule_item(ui, p, slot(2), Icon::Table, &format!("Table ({})", cmd_key!("Alt+T")), editable, false).clicked() && editable {
            self.editor_command(Command::InsertTable);
        }
        if capsule_item(ui, p, slot(3), Icon::Link, &format!("Add Link ({})", cmd_key!("K")), editable, false).clicked() && editable {
            self.editor_command(Command::EditLink);
        }
        if self.format_open {
            self.format_menu(ui, p, aa_rect);
        }

        let area = Rect::from_min_max(Pos2::new(full.left(), bar.bottom()), full.max);
        let mut child = ui.new_child(egui::UiBuilder::new().max_rect(area).layout(egui::Layout::top_down(egui::Align::LEFT)));
        if self.multi.len() > 1 {
            self.several_selected(&mut child, p, area);
            return;
        }
        let Some(id) = self.selected.clone() else {
            let msg = if self.shown.is_empty() { "" } else { "No Note Selected" };
            child.painter().text(area.center(), Align2::CENTER_CENTER, msg, theme::font(15.0), p.secondary);
            return;
        };
        let Some(note) = self.lib.store.note(&id).cloned() else { return };
        if self.is_closed(&id) {
            self.lock_screen(&mut child, p, area, &id);
            return;
        }

        if read_only {
            self.deleted_banner(&mut child, p, &id);
        }
        self.find_bar(&mut child, p);

        let header = note_header_time(note.modified);
        let highlight = self.search.clone();
        let can_attach = self.can_attach();
        let opts = Options {
            text_size: self.lib.settings.text_size,
            sort_checked: self.lib.settings.sort_checked,
            read_only,
            highlight: &highlight,
            images: &self.images,
            can_attach,
        };
        let Some(editor) = &mut self.editor else { return };
        let mut out = None;
        egui::ScrollArea::vertical()
            .id_salt(("note", &id))
            .auto_shrink([false, false])
            .show(&mut child, |ui| {
                out = Some(editor.show(ui, p, &opts, &header));
            });
        let editor_focused = child.memory(|m| m.has_focus(editor.id()));
        if editor_focused {
            self.focus = Pane::Note;
        }
        if let Some(out) = out {
            if let Some(url) = out.open_url {
                let _ = open::that(url);
            }
            if out.blocked_edit {
                self.push_notice("Deleted notes can't be edited. To edit this note, recover it first.".into());
            }
            if let Some(path) = out.open_image {
                let _ = open::that(path);
            }
            if out.attach_image {
                self.choose_images();
            }
            if out.paste_image {
                self.paste_image();
            }
        }
        self.link_sheet(ui.ctx(), p);
    }

    /// With several notes selected: a little stack of them and a count.
    fn several_selected(&self, ui: &mut Ui, p: &Palette, area: Rect) {
        let c = area.center() - Vec2::new(0.0, 40.0);
        let painter = ui.painter();
        let n = self.multi.len();
        for i in (0..n.min(3)).rev() {
            let off = i as f32 * 8.0;
            let r = Rect::from_center_size(c + Vec2::new(off, -off), Vec2::new(120.0, 150.0));
            painter.rect(r, 10.0, p.card, egui::Stroke::new(1.0_f32, p.separator));
            if i == 0 {
                let title = self.lib.store.note(&self.multi[0]).map(|n| n.title()).unwrap_or_default();
                let t = super::sidebar::truncate(&title, 100.0, ui);
                painter.text(r.left_top() + Vec2::new(10.0, 14.0), Align2::LEFT_CENTER, t, theme::bold_font(10.5), p.text);
                for k in 0..5 {
                    let y = r.top() + 32.0 + k as f32 * 12.0;
                    let w = if k == 4 { 50.0 } else { 96.0 };
                    painter.line_segment([Pos2::new(r.left() + 10.0, y), Pos2::new(r.left() + 10.0 + w, y)], egui::Stroke::new(3.0_f32, p.separator));
                }
            }
        }
        painter.text(c + Vec2::new(0.0, 110.0), Align2::CENTER_CENTER, format!("{n} Notes Selected"), theme::font(15.0), p.secondary);
    }

    /// The lock button, as in Apple Notes: a menu to lock the note, or, once
    /// a locked note is open, to close locked notes or remove its lock.
    fn lock_button(&mut self, ui: &mut Ui, p: &Palette, center: Pos2) {
        if self.multi.len() >= 2 {
            return self.lock_selection_button(ui, p, center);
        }
        let note = self.selected.clone().and_then(|s| self.lib.store.note(&s).cloned());
        let note = note.filter(|n| n.deleted.is_none() && !self.is_closed(&n.id));
        let open = note.as_ref().is_some_and(|n| self.is_locked(&n.id));
        let icon = if open { Icon::LockOpen } else { Icon::LockOutline };
        let resp = glass_button(ui, p, center, icon, if open { "Unlocked" } else { "Lock Note" }, note.is_some());
        let id = ui.id().with("note-lock");
        let Some(note) = note else { return };
        if resp.clicked() {
            ui.memory_mut(|m| m.toggle_popup(id));
        }
        egui::popup_below_widget(ui, id, &resp, egui::PopupCloseBehavior::CloseOnClickOutside, |ui| {
            popover_frame(ui, p).show(ui, |ui| {
                menu_look(ui, p);
                ui.set_min_width(200.0);
                let mut close = false;
                if !open {
                    if ui.button("Lock Note").clicked() {
                        self.lock_note(&note.id);
                        close = true;
                    }
                } else {
                    if ui.button("Close All Locked Notes").clicked() {
                        self.lock_now();
                        close = true;
                    }
                    if ui.button("Remove Lock").clicked() {
                        self.remove_lock(&note.id);
                        close = true;
                    }
                }
                if close {
                    ui.memory_mut(|m| m.close_popup());
                }
            });
        });
    }

    /// The lock button with several notes selected: locks all of them that
    /// aren't locked yet (the password is asked for once).
    fn lock_selection_button(&mut self, ui: &mut Ui, p: &Palette, center: Pos2) {
        let ids = self.selected_ids();
        let to_lock = ids.iter().filter(|i| self.can_lock(i)).count();
        let label = if to_lock == 1 { "Lock Note".to_owned() } else { format!("Lock {to_lock} Notes") };
        let resp = glass_button(ui, p, center, Icon::LockOutline, &label, to_lock > 0);
        let id = ui.id().with("note-lock");
        if to_lock == 0 {
            return;
        }
        if resp.clicked() {
            ui.memory_mut(|m| m.toggle_popup(id));
        }
        egui::popup_below_widget(ui, id, &resp, egui::PopupCloseBehavior::CloseOnClickOutside, |ui| {
            popover_frame(ui, p).show(ui, |ui| {
                menu_look(ui, p);
                ui.set_min_width(200.0);
                if ui.button(&label).clicked() {
                    self.lock_notes(&ids);
                    ui.memory_mut(|m| m.close_popup());
                }
            });
        });
    }

    /// The note's "…" menu: pin, find, move, duplicate, delete.
    fn note_more_menu(&mut self, ui: &mut Ui, p: &Palette, resp: &egui::Response) {
        let id = ui.id().with("note-more");
        if resp.clicked() && self.selected.is_some() {
            ui.memory_mut(|m| m.toggle_popup(id));
        }
        let Some(note) = self.selected.clone().and_then(|s| self.lib.store.note(&s).cloned()) else { return };
        let ids = vec![note.id.clone()];
        egui::popup_below_widget(ui, id, resp, egui::PopupCloseBehavior::CloseOnClickOutside, |ui| {
            popover_frame(ui, p).show(ui, |ui| {
                menu_look(ui, p);
                ui.set_min_width(220.0);
                let mut close = false;
                if note.deleted.is_some() {
                    if ui.button("Recover Note").clicked() {
                        self.recover_note(&note.id);
                        close = true;
                    }
                    if ui.button("Delete Permanently…").clicked() {
                        self.confirm = Some(Confirm::DeleteNotesForever(ids.clone()));
                        close = true;
                    }
                } else {
                    if ui.button(if note.pinned { "Unpin Note" } else { "Pin Note" }).clicked() {
                        self.toggle_pin(&note.id);
                        close = true;
                    }
                    if !self.is_locked(&note.id) {
                        if ui.button("Lock Note").clicked() {
                            self.lock_note(&note.id);
                            close = true;
                        }
                    } else if !self.is_closed(&note.id) {
                        if ui.button("Lock Now").clicked() {
                            self.lock_now();
                            close = true;
                        }
                        if ui.button("Remove Lock").clicked() {
                            self.remove_lock(&note.id);
                            close = true;
                        }
                    }
                    ui.separator();
                    if ui.add_enabled(self.can_attach(), egui::Button::new("Attach Image…")).clicked() {
                        ui.memory_mut(|m| m.close_popup());
                        self.choose_images();
                    }
                    if ui.add(egui::Button::new("Find in Note").shortcut_text(cmd_key!("F"))).clicked() {
                        if let Some(e) = &mut self.editor {
                            e.find.open = true;
                            e.find.focus = true;
                        }
                        close = true;
                    }
                    ui.menu_button("Move to", |ui| self.move_to_menu(ui, &ids));
                    if ui.add(egui::Button::new("Duplicate Note").shortcut_text(cmd_key!("D"))).clicked() {
                        self.duplicate_note(&note.id);
                        close = true;
                    }
                    let plain = self.plain_body(&note.id);
                    if ui.add_enabled(plain.is_some(), egui::Button::new("Copy as Markdown")).clicked() {
                        ui.ctx().copy_text(plain.unwrap_or_default());
                        close = true;
                    }
                    ui.separator();
                    if ui.add(egui::Button::new("Delete Note").shortcut_text(cmd_key!("Backspace"))).clicked() {
                        self.delete_notes(ids.clone());
                        close = true;
                    }
                }
                if close {
                    ui.memory_mut(|m| m.close_popup());
                }
            });
        });
    }

    fn deleted_banner(&mut self, ui: &mut Ui, p: &Palette, id: &str) {
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            ui.add_space(ui.available_width() / 2.0 - 190.0);
            egui::Frame::none().fill(p.field).rounding(8.0).inner_margin(egui::Margin::symmetric(12.0, 6.0)).show(ui, |ui| {
                ui.label(egui::RichText::new("This note is in Recently Deleted.").color(p.secondary));
                if ui.button("Recover").clicked() {
                    self.recover_note(id);
                }
            });
        });
    }

    fn find_bar(&mut self, ui: &mut Ui, p: &Palette) {
        let Some(e) = &mut self.editor else { return };
        if !e.find.open {
            return;
        }
        let count = e.matches(&e.find.query).len();
        let mut changed = false;
        let mut step = 0;
        let mut close = false;
        egui::Frame::none().fill(p.sidebar).inner_margin(egui::Margin::symmetric(14.0, 6.0)).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                let te = ui.add(egui::TextEdit::singleline(&mut e.find.query).hint_text("Find in Note").desired_width(220.0));
                if e.find.focus {
                    te.request_focus();
                    e.find.focus = false;
                }
                changed = te.changed();
                if te.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) {
                    step = if ui.input(|i| i.modifiers.shift) { -1 } else { 1 };
                    te.request_focus();
                }
                if ui.input(|i| i.key_pressed(Key::Escape)) {
                    close = true;
                }
                let label = match (count, e.find.query.is_empty()) {
                    (_, true) => String::new(),
                    (0, _) => "Not found".into(),
                    (n, _) => format!("{} of {n}", e.find.current.min(n - 1) + 1),
                };
                ui.label(egui::RichText::new(label).color(p.secondary).size(12.0));
                if ui.add_enabled(count > 0, egui::Button::new("‹")).on_hover_text(cmd_key!("Shift+G")).clicked() {
                    step = -1;
                }
                if ui.add_enabled(count > 0, egui::Button::new("›")).on_hover_text(cmd_key!("G")).clicked() {
                    step = 1;
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button("Done").clicked() {
                        close = true;
                    }
                });
            });
        });
        if changed {
            e.find_from_caret();
        }
        if step != 0 {
            e.find_step(step);
        }
        if close {
            e.find.open = false;
            e.focus_requested = true;
        }
    }

    /// Search (all notes): a round button ending at `right` that opens
    /// into a field. Returns its left edge.
    pub(super) fn search_control(&mut self, ui: &mut Ui, p: &Palette, right: f32, y: f32) -> f32 {
        if self.search_focus {
            self.search_open = true;
        }
        if !self.search_open && self.search.is_empty() {
            let c = Pos2::new(right - 18.0, y);
            if glass_button(ui, p, c, Icon::Search, "Search (Ctrl+Alt+F)", true).clicked() {
                self.search_open = true;
                self.search_focus = true;
            }
            return c.x - 18.0;
        }
        let w = 240.0;
        let r = Rect::from_min_size(Pos2::new(right - w, y - 18.0), Vec2::new(w, 36.0));
        glass(ui, p, r);
        crate::icons::paint(ui.painter(), Icon::Search, Rect::from_center_size(Pos2::new(r.left() + 20.0, r.center().y), Vec2::splat(15.0)), p.secondary);
        let te_rect = Rect::from_min_max(Pos2::new(r.left() + 34.0, r.top() + 7.0), Pos2::new(r.right() - 28.0, r.bottom() - 7.0));
        let te = ui.put(
            te_rect,
            egui::TextEdit::singleline(&mut self.search).hint_text("Search").frame(false).margin(egui::vec2(2.0, 2.0)),
        );
        if te.lost_focus() && self.search.is_empty() {
            self.search_open = false;
        }
        if self.search_focus {
            te.request_focus();
            self.search_focus = false;
        }
        if te.changed() {
            self.scroll_list = true;
            // Notes the search hides drop out of the selection, so a Delete
            // doesn't reach notes that aren't shown.
            self.search_changed();
        }
        if te.has_focus() && ui.input(|i| i.key_pressed(Key::Escape)) {
            self.search.clear();
            te.surrender_focus();
            self.search_changed();
        }
        if te.has_focus() && ui.input(|i| i.key_pressed(Key::ArrowDown)) {
            self.focus = Pane::List;
            te.surrender_focus();
        }
        if !self.search.is_empty() {
            let x = Rect::from_center_size(Pos2::new(r.right() - 16.0, r.center().y), Vec2::splat(14.0));
            let resp = ui.interact(x, ui.id().with("clear-search"), Sense::click());
            ui.painter().circle_filled(x.center(), 7.0, p.tertiary);
            ui.painter().text(x.center(), Align2::CENTER_CENTER, "×", theme::bold_font(11.0), p.bg);
            if resp.clicked() {
                self.search.clear();
                self.search_open = false;
                self.search_changed();
            }
        }
        r.left()
    }

    /// The Aa popover: character styles on top, paragraph styles below.
    fn format_menu(&mut self, ui: &mut Ui, p: &Palette, anchor: Rect) {
        let current = self.note_kind();
        let attrs: Vec<bool> = [Attr::Bold, Attr::Italic, Attr::Underline, Attr::Strike]
            .iter()
            .map(|a| self.editor.as_ref().is_some_and(|e| e.has_attr(*a)))
            .collect();
        let mut chosen: Option<Command> = None;
        let area = egui::Area::new(egui::Id::new("format-menu"))
            .order(egui::Order::Foreground)
            .fixed_pos(Pos2::new(anchor.left() - 14.0, anchor.bottom() + 8.0))
            .show(ui.ctx(), |ui| {
                popover_frame(ui, p).inner_margin(egui::Margin::same(8.0)).show(ui, |ui| {
                    ui.set_width(216.0);
                    ui.spacing_mut().item_spacing.y = 0.0;
                    // B I U S
                    let (row, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 36.0), Sense::hover());
                    let labels = [("B", Attr::Bold), ("I", Attr::Italic), ("U", Attr::Underline), ("S", Attr::Strike)];
                    for (i, (l, a)) in labels.into_iter().enumerate() {
                        let r = Rect::from_min_size(Pos2::new(row.left() + i as f32 * 42.0, row.top() + 2.0), Vec2::new(36.0, 32.0));
                        let resp = ui.interact(r, ui.id().with(("attr", i)), Sense::click());
                        let tip = match a {
                            Attr::Bold => cmd_key!("B"),
                            Attr::Italic => cmd_key!("I"),
                            Attr::Underline => cmd_key!("U"),
                            Attr::Strike => cmd_key!("Shift+X"),
                        };
                        let resp = resp.on_hover_text(tip);
                        if attrs[i] {
                            ui.painter().rect_filled(r, 8.0, theme::blend(p.popover, p.accent, 0.55));
                        } else if resp.hovered() {
                            ui.painter().rect_filled(r, 8.0, p.hover);
                        }
                        let font = match a {
                            Attr::Bold => theme::bold_font(18.0),
                            Attr::Italic => egui::FontId::new(18.0, theme::italic()),
                            _ => theme::font(18.0),
                        };
                        let color = if attrs[i] { egui::Color32::WHITE } else { p.text };
                        let g = ui.painter().layout_no_wrap(l.to_owned(), font, color);
                        let gr = Rect::from_center_size(r.center(), g.size());
                        ui.painter().galley(gr.min, g, color);
                        let stroke = egui::Stroke::new(1.3_f32, color);
                        match a {
                            Attr::Underline => {
                                ui.painter().line_segment([Pos2::new(gr.left(), gr.bottom() - 1.0), Pos2::new(gr.right(), gr.bottom() - 1.0)], stroke);
                            }
                            Attr::Strike => {
                                ui.painter().line_segment([Pos2::new(gr.left() - 2.0, gr.center().y + 1.0), Pos2::new(gr.right() + 2.0, gr.center().y + 1.0)], stroke);
                            }
                            _ => {}
                        }
                        if resp.clicked() {
                            chosen = Some(Command::ToggleAttr(a));
                        }
                    }
                    ui.add_space(6.0);
                    ui.separator();
                    ui.add_space(4.0);
                    for (label, kind, shortcut) in STYLES {
                        if kind == Kind::Quote {
                            ui.add_space(4.0);
                            ui.separator();
                            ui.add_space(4.0);
                        }
                        let font = match kind {
                            Kind::Title => theme::bold_font(24.0),
                            Kind::Heading => theme::bold_font(19.0),
                            Kind::Subheading => theme::bold_font(15.5),
                            Kind::Monospaced => egui::FontId::monospace(14.0),
                            _ => theme::font(14.5),
                        };
                        let (r, resp) = ui.allocate_exact_size(Vec2::new(ui.available_width(), font.size + 15.0), Sense::click());
                        let resp = resp.on_hover_text(shortcut);
                        let on = current.same(kind);
                        if resp.hovered() {
                            ui.painter().rect_filled(r, 7.0, p.hover);
                        }
                        if on {
                            ui.painter().text(Pos2::new(r.left() + 13.0, r.center().y), Align2::CENTER_CENTER, "✓", theme::font(14.0), p.text);
                        }
                        let tx = r.left() + 30.0;
                        if kind == Kind::Quote {
                            ui.painter().line_segment([Pos2::new(tx, r.center().y - 8.0), Pos2::new(tx, r.center().y + 8.0)], egui::Stroke::new(2.5_f32, p.secondary));
                            ui.painter().text(Pos2::new(tx + 9.0, r.center().y), Align2::LEFT_CENTER, label, font, p.text);
                        } else {
                            ui.painter().text(Pos2::new(tx, r.center().y), Align2::LEFT_CENTER, label, font, p.text);
                        }
                        if resp.clicked() {
                            chosen = Some(Command::SetKind(kind));
                        }
                    }
                });
            });
        if let Some(c) = chosen {
            self.editor_command(c);
            self.format_open = false;
        }
        let clicked_outside = ui.input(|i| i.pointer.any_pressed())
            && ui.input(|i| i.pointer.interact_pos()).is_some_and(|pt| !area.response.rect.contains(pt) && !anchor.contains(pt));
        if clicked_outside || ui.input(|i| i.key_pressed(Key::Escape)) {
            self.format_open = false;
        }
    }

    fn link_sheet(&mut self, ctx: &egui::Context, p: &Palette) {
        let Some(e) = &mut self.editor else { return };
        let Some(sheet) = &mut e.link_sheet else { return };
        let had_link = !sheet.url.is_empty();
        let mut result: Option<Option<Option<String>>> = None;
        modal(ctx, "link", p, |ui| {
            ui.set_width(340.0);
            ui.label(egui::RichText::new("Add Link").font(theme::bold_font(14.0)));
            ui.add_space(6.0);
            let te = ui.add(egui::TextEdit::singleline(&mut sheet.url).hint_text("https://").desired_width(f32::INFINITY));
            if sheet.focus {
                te.request_focus();
                sheet.focus = false;
            }
            ui.add_space(10.0);
            ui.horizontal(|ui| {
                if had_link && ui.button("Remove Link").clicked() {
                    result = Some(Some(None));
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let ok = egui::Button::new(egui::RichText::new("OK").color(egui::Color32::WHITE)).fill(theme::BLUE);
                    if ui.add(ok).clicked() || (te.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter))) {
                        result = Some(Some(Some(sheet.url.clone())));
                    }
                    if ui.button("Cancel").clicked() || ui.input(|i| i.key_pressed(Key::Escape)) {
                        result = Some(None);
                    }
                });
            });
        });
        if let Some(r) = result {
            e.link_sheet = None;
            if let Some(url) = r {
                e.command(Command::SetLink(url));
            }
            e.focus_requested = true;
        }
    }
}
