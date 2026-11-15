//! The note pane: its toolbar (new note, the Aa format menu, checklist,
//! link, search), the find bar, and the editor.

use eframe::egui::{self, Align2, Key, Pos2, Rect, Sense, Ui, Vec2};

use super::sidebar::{tool_button, tool_button_state};
use super::{modal, note_header_time, App, Pane, TOOLBAR_H};
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
    ("Monospaced", Kind::Monospaced, cmd_key!("Shift+M")),
    ("•  Bulleted List", Kind::Bulleted, cmd_key!("Shift+7")),
    ("–  Dashed List", Kind::Dashed, cmd_key!("Shift+8")),
    ("1.  Numbered List", Kind::Numbered, cmd_key!("Shift+9")),
    ("|  Block Quote", Kind::Quote, cmd_key!("'")),
];

impl App {
    pub(super) fn note_pane(&mut self, ui: &mut Ui, p: &Palette) {
        let full = ui.max_rect();
        let bar = Rect::from_min_size(full.min, Vec2::new(full.width(), TOOLBAR_H));
        ui.allocate_rect(bar, Sense::hover());
        let y = bar.center().y;
        let read_only = self.selected.as_ref().and_then(|s| self.store.note(s)).is_some_and(|n| n.deleted.is_some());
        let has_note = self.editor.is_some();
        let mut x = bar.left() + 14.0;
        let btn = |x: f32| Rect::from_center_size(Pos2::new(x + 14.0, y), Vec2::new(30.0, 28.0));
        if self.gallery_open {
            if tool_button(ui, p, btn(x), Icon::ChevronRight, "Back to Gallery") {
                self.gallery_open = false;
            }
            x += 38.0;
        } else if !self.settings.sidebar_visible && self.settings.view == crate::settings::ListView::List {
            // The list pane shows the sidebar toggle.
        }
        if tool_button(ui, p, btn(x), Icon::Compose, "New Note (Ctrl+N)") {
            self.new_note();
        }
        x += 52.0;
        let editable = has_note && !read_only;
        let aa_rect = btn(x);
        if tool_button_state(ui, p, aa_rect, Icon::Format, "Format", editable, self.format_open) {
            self.format_open = !self.format_open;
        }
        x += 36.0;
        if tool_button_state(ui, p, btn(x), Icon::Checklist, &format!("Checklist ({})", cmd_key!("Shift+L")), editable, false) {
            self.editor_command(Command::SetKind(Kind::Checklist(false)));
        }
        x += 36.0;
        if tool_button_state(ui, p, btn(x), Icon::Link, &format!("Add Link ({})", cmd_key!("K")), editable, false) {
            self.editor_command(Command::EditLink);
        }
        self.search_field(ui, p, bar);
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
        let Some(note) = self.store.note(&id).cloned() else { return };

        if read_only {
            self.deleted_banner(&mut child, p, &id);
        }
        self.find_bar(&mut child, p);

        let header = note_header_time(note.modified);
        let highlight = self.search.clone();
        let opts = Options { text_size: self.settings.text_size, sort_checked: self.settings.sort_checked, read_only, highlight: &highlight };
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
                let title = self.store.note(&self.multi[0]).map(|n| n.title()).unwrap_or_default();
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

    /// The search field at the right of the toolbar (searches all notes).
    pub(super) fn search_field(&mut self, ui: &mut Ui, p: &Palette, bar: Rect) {
        let w = (bar.width() * 0.32).clamp(120.0, 220.0);
        let r = Rect::from_min_size(Pos2::new(bar.right() - w - 14.0, bar.center().y - 13.0), Vec2::new(w, 26.0));
        ui.painter().rect_filled(r, 7.0, p.field);
        crate::icons::paint(ui.painter(), Icon::Search, Rect::from_center_size(Pos2::new(r.left() + 14.0, r.center().y), Vec2::splat(14.0)), p.secondary);
        let te_rect = Rect::from_min_max(Pos2::new(r.left() + 24.0, r.top() + 2.0), Pos2::new(r.right() - 22.0, r.bottom() - 2.0));
        let te = ui.put(
            te_rect,
            egui::TextEdit::singleline(&mut self.search).hint_text("Search").frame(false).margin(egui::vec2(2.0, 3.0)),
        );
        if self.search_focus {
            te.request_focus();
            self.search_focus = false;
        }
        if te.changed() {
            self.scroll_list = true;
            let first = self.visible_notes().first().cloned();
            if !self.visible_notes().contains(&self.selected.clone().unwrap_or_default()) {
                self.select(first);
            }
        }
        if te.has_focus() && ui.input(|i| i.key_pressed(Key::Escape)) {
            self.search.clear();
            te.surrender_focus();
        }
        if te.has_focus() && ui.input(|i| i.key_pressed(Key::ArrowDown)) {
            self.focus = Pane::List;
            te.surrender_focus();
        }
        if !self.search.is_empty() {
            let x = Rect::from_center_size(Pos2::new(r.right() - 12.0, r.center().y), Vec2::splat(14.0));
            let resp = ui.interact(x, ui.id().with("clear-search"), Sense::click());
            ui.painter().circle_filled(x.center(), 6.5, p.tertiary);
            ui.painter().text(x.center(), Align2::CENTER_CENTER, "×", theme::bold_font(11.0), p.bg);
            if resp.clicked() {
                self.search.clear();
            }
        }
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
            .fixed_pos(Pos2::new(anchor.left() - 10.0, anchor.bottom() + 6.0))
            .show(ui.ctx(), |ui| {
                egui::Frame::popup(ui.style()).fill(p.popover).rounding(10.0).inner_margin(egui::Margin::same(8.0)).show(ui, |ui| {
                    ui.set_width(230.0);
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 4.0;
                        let labels = [("B", Attr::Bold), ("I", Attr::Italic), ("U", Attr::Underline), ("S", Attr::Strike)];
                        for (i, (l, a)) in labels.into_iter().enumerate() {
                            let mut text = egui::RichText::new(l).size(15.0);
                            text = match a {
                                Attr::Bold => text.strong(),
                                Attr::Italic => text.italics(),
                                Attr::Underline => text.underline(),
                                Attr::Strike => text.strikethrough(),
                            };
                            let b = egui::Button::new(text).min_size(Vec2::new(53.0, 30.0)).selected(attrs[i]);
                            if ui.add(b).clicked() {
                                chosen = Some(Command::ToggleAttr(a));
                            }
                        }
                    });
                    ui.add_space(4.0);
                    for (label, kind, shortcut) in STYLES {
                        let font = match kind {
                            Kind::Title => theme::bold_font(21.0),
                            Kind::Heading => theme::bold_font(17.0),
                            Kind::Subheading => theme::bold_font(14.5),
                            Kind::Monospaced => egui::FontId::monospace(13.0),
                            _ => theme::font(13.5),
                        };
                        let (r, resp) = ui.allocate_exact_size(Vec2::new(ui.available_width(), font.size + 12.0), Sense::click());
                        let on = current.same(kind);
                        if resp.hovered() {
                            ui.painter().rect_filled(r, 6.0, p.selection);
                        }
                        if on {
                            ui.painter().text(Pos2::new(r.left() + 10.0, r.center().y), Align2::CENTER_CENTER, "✓", theme::font(12.0), p.text);
                        }
                        ui.painter().text(Pos2::new(r.left() + 24.0, r.center().y), Align2::LEFT_CENTER, label, font, p.text);
                        ui.painter().text(Pos2::new(r.right() - 6.0, r.center().y), Align2::RIGHT_CENTER, shortcut, theme::font(11.0), p.secondary);
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
