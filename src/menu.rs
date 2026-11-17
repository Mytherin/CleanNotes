//! Right-click menus drawn as macOS 26's: rows with an icon column, a blue
//! rounded highlight with white text, inset separators, ✓ on chosen items
//! and a chevron on submenus. (The menu's frame — rounding, edge, shadow —
//! is egui's, set in [`crate::theme::apply_style`].)

use eframe::egui::{self, text::LayoutJob, Color32, Pos2, Rect, Sense, Ui, Vec2};

use crate::icons::{self, Icon};
use crate::theme::{self, Palette};

/// A row's height.
pub const ROW_H: f32 = 24.0;
/// Where a row's text starts, and its icon's center, from the row's left.
const TEXT_X: f32 = 30.0;
const ICON_X: f32 = 15.0;
const ICON_SIZE: f32 = 15.0;
/// Room after the text: for the chevron, or the right margin.
const END_PAD: f32 = 14.0;
const ROUNDING: f32 = 6.0;

fn font() -> egui::FontId {
    theme::font(13.5)
}

/// macOS's context menus show no key equivalents; elsewhere they help.
const SHOW_SHORTCUTS: bool = !cfg!(target_os = "macos");

/// The highlight of the row under the pointer, and its text's color: blue
/// and white in menus (see [`begin`]), the popover's own in a popover.
fn highlight(ui: &Ui) -> (Color32, Color32) {
    let w = &ui.visuals().widgets.hovered;
    (w.weak_bg_fill, w.fg_stroke.color)
}

/// Call first in a menu's contents: rows touch and are lit blue, as in
/// macOS.
pub fn begin(ui: &mut Ui, min_width: f32) {
    ui.set_min_width(min_width);
    ui.spacing_mut().item_spacing.y = 0.0;
    let hovered = &mut ui.visuals_mut().widgets.hovered;
    hovered.weak_bg_fill = theme::BLUE;
    hovered.fg_stroke.color = Color32::WHITE;
}

/// A menu row.
pub struct Item<'a> {
    label: &'a str,
    icon: Option<Icon>,
    shortcut: &'a str,
    enabled: bool,
    checked: bool,
    indent: usize,
}

pub fn item(label: &str) -> Item<'_> {
    Item { label, icon: None, shortcut: "", enabled: true, checked: false, indent: 0 }
}

impl<'a> Item<'a> {
    pub fn icon(mut self, icon: Icon) -> Self {
        self.icon = Some(icon);
        self
    }

    pub fn shortcut(mut self, shortcut: &'a str) -> Self {
        self.shortcut = shortcut;
        self
    }

    pub fn enabled(mut self, enabled: bool) -> Self {
        self.enabled = enabled;
        self
    }

    /// A ✓ in the icon column.
    pub fn checked(mut self, checked: bool) -> Self {
        self.checked = checked;
        self
    }

    /// Nested `depth` levels (a folder in a tree).
    pub fn indent(mut self, depth: usize) -> Self {
        self.indent = depth;
        self
    }

    /// Clicked: the menu closes.
    pub fn show(self, ui: &mut Ui) -> egui::Response {
        let p = Palette::get(ui.ctx());
        let indent = self.indent as f32 * 14.0;
        let label = ui.painter().layout_no_wrap(self.label.to_owned(), font(), Color32::PLACEHOLDER);
        let shortcut = (SHOW_SHORTCUTS && !self.shortcut.is_empty())
            .then(|| ui.painter().layout_no_wrap(self.shortcut.to_owned(), font(), Color32::PLACEHOLDER));
        let width = TEXT_X + indent + label.size().x + shortcut.as_ref().map_or(0.0, |s| 28.0 + s.size().x) + END_PAD;
        let sense = if self.enabled { Sense::click() } else { Sense::hover() };
        let (rect, resp) = ui.allocate_at_least(Vec2::new(width, ROW_H), sense);
        let lit = self.enabled && resp.hovered();
        let colors = row_colors(ui, &p, self.enabled, lit);
        if lit {
            ui.painter().rect_filled(rect, ROUNDING, highlight(ui).0);
        }
        paint_row(ui, rect, indent, self.icon, self.checked, colors);
        let y = rect.center().y - label.size().y / 2.0;
        ui.painter().galley(Pos2::new(rect.left() + TEXT_X + indent, y), label, colors.0);
        if let Some(s) = shortcut {
            let at = Pos2::new(rect.right() - END_PAD - s.size().x, rect.center().y - s.size().y / 2.0);
            ui.painter().galley(at, s, if lit { colors.0 } else { p.secondary });
        }
        if resp.clicked() {
            ui.close_menu();
        }
        resp
    }
}

/// Text and icon colors of a row.
fn row_colors(ui: &Ui, p: &Palette, enabled: bool, lit: bool) -> (Color32, Color32) {
    if !enabled {
        (p.tertiary, p.tertiary)
    } else if lit {
        let c = highlight(ui).1;
        (c, c)
    } else {
        (p.text, p.text)
    }
}

/// A row's icon (or ✓).
fn paint_row(ui: &Ui, rect: Rect, indent: f32, icon: Option<Icon>, checked: bool, (_, icon_color): (Color32, Color32)) {
    let icon = if checked { Some(Icon::Check) } else { icon };
    if let Some(icon) = icon {
        let r = Rect::from_center_size(Pos2::new(rect.left() + ICON_X + indent, rect.center().y), Vec2::splat(ICON_SIZE));
        icons::paint(ui.painter(), icon, r, icon_color);
    }
}

/// A line between groups of rows, inset from the menu's edges.
pub fn separator(ui: &mut Ui) {
    let p = Palette::get(ui.ctx());
    let (rect, _) = ui.allocate_at_least(Vec2::new(0.0, 11.0), Sense::hover());
    let y = rect.center().y.round() + 0.5;
    let edge = if p.dark { Color32::from_white_alpha(0x24) } else { Color32::from_black_alpha(0x1C) };
    ui.painter().hline(rect.left() + 10.0..=rect.right() - 10.0, y, egui::Stroke::new(1.0_f32, edge));
}

/// A small gray title over the rows that follow (an account's name).
pub fn header(ui: &mut Ui, text: &str) {
    let p = Palette::get(ui.ctx());
    let galley = ui.painter().layout_no_wrap(text.to_owned(), theme::bold_font(11.5), p.secondary);
    let (rect, _) = ui.allocate_at_least(Vec2::new(TEXT_X + galley.size().x + END_PAD, 22.0), Sense::hover());
    let at = Pos2::new(rect.left() + TEXT_X, rect.bottom() - 4.0 - galley.size().y);
    ui.painter().galley(at, galley, p.secondary);
}

/// A row with a chevron whose rows (`add_contents`) open to its right when
/// it's hovered. Returns what `add_contents` returned, when it was shown.
pub fn submenu<R>(ui: &mut Ui, label: &str, icon: Option<Icon>, enabled: bool, add_contents: impl FnOnce(&mut Ui) -> R) -> Option<R> {
    let p = Palette::get(ui.ctx());
    // egui's submenu button (it alone opens and keeps open a submenu),
    // drawn here: it paints nothing of its own, its label laid out (for
    // its size) in no color and its "⏵" with the widgets' text color, none.
    let bg = ui.painter().add(egui::Shape::Noop);
    let mut job = LayoutJob::single_section(label.to_owned(), egui::TextFormat::simple(font(), Color32::TRANSPARENT));
    job.sections[0].leading_space = TEXT_X - 6.0;
    let text_h = ui.fonts(|f| f.layout_job(job.clone())).size().y;
    let inner = ui.add_enabled_ui(enabled, |ui| {
        let s = ui.style_mut();
        for w in [&mut s.visuals.widgets.inactive, &mut s.visuals.widgets.hovered, &mut s.visuals.widgets.active, &mut s.visuals.widgets.open, &mut s.visuals.widgets.noninteractive] {
            w.fg_stroke.color = Color32::TRANSPARENT;
            w.bg_fill = Color32::TRANSPARENT;
            w.weak_bg_fill = Color32::TRANSPARENT;
            w.bg_stroke = egui::Stroke::NONE;
        }
        s.visuals.selection.bg_fill = Color32::TRANSPARENT;
        s.visuals.selection.stroke.color = Color32::TRANSPARENT;
        s.spacing.button_padding = Vec2::new(6.0, ((ROW_H - text_h) / 2.0).max(0.0));
        // The chevron's room.
        s.spacing.item_spacing.x = END_PAD + 8.0;
        ui.menu_button(job, |ui| {
            begin(ui, 0.0);
            add_contents(ui)
        })
    });
    let resp = inner.inner.response;
    let open = inner.inner.inner.is_some();
    let rect = resp.rect;
    let lit = enabled && (open || resp.hovered());
    if lit {
        ui.painter().set(bg, egui::Shape::rect_filled(rect, ROUNDING, highlight(ui).0));
    }
    let colors = row_colors(ui, &p, enabled, lit);
    paint_row(ui, rect, 0.0, icon, false, colors);
    ui.painter().text(Pos2::new(rect.left() + TEXT_X, rect.center().y), egui::Align2::LEFT_CENTER, label, font(), colors.0);
    let chevron = Rect::from_center_size(Pos2::new(rect.right() - 12.0, rect.center().y), Vec2::splat(12.0));
    icons::paint(ui.painter(), Icon::ChevronRight, chevron, colors.1);
    inner.inner.inner
}
