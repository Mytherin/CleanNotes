//! Apple Notes–like colors and fonts, for both light and dark mode.

use eframe::egui::{self, Color32, FontData, FontDefinitions, FontFamily, FontId};

use crate::fonts::{self, Face};

/// Notes' yellow: folder icons, checked checkboxes, the caret, tags.
pub const ACCENT_LIGHT: Color32 = Color32::from_rgb(0xE3, 0xA4, 0x00);
pub const ACCENT_DARK: Color32 = Color32::from_rgb(0xF5, 0xC2, 0x1B);
/// Links in notes, and buttons in dialogs.
pub const BLUE: Color32 = Color32::from_rgb(0x00, 0x7A, 0xFF);
pub const RED: Color32 = Color32::from_rgb(0xFF, 0x3B, 0x30);

#[derive(Clone, Copy)]
pub struct Palette {
    pub dark: bool,
    /// The note itself.
    pub bg: Color32,
    /// The notes list, and the window behind the floating sidebar.
    pub list_bg: Color32,
    /// The floating sidebar panel, and its edge.
    pub sidebar: Color32,
    pub sidebar_edge: Color32,
    /// The selected folder: a yellow pill with white text.
    pub folder_selection: Color32,
    /// Round toolbar buttons and button groups ("glass"), and their edge.
    pub glass: Color32,
    pub glass_edge: Color32,
    pub text: Color32,
    /// Dates, snippets, counts.
    pub secondary: Color32,
    pub tertiary: Color32,
    /// Lines between notes in the list, under the toolbar.
    pub separator: Color32,
    /// The selected note while the list has the keyboard.
    pub selection: Color32,
    pub selection_text: Color32,
    /// The selected note otherwise, and the selected folder.
    pub selection_inactive: Color32,
    /// Selected text in a note.
    pub text_selection: Color32,
    pub accent: Color32,
    /// Toolbar buttons when hovered.
    pub hover: Color32,
    /// The search field, cards in the gallery.
    pub field: Color32,
    pub card: Color32,
    /// Menus and popovers.
    pub popover: Color32,
    /// Search matches in a note.
    pub highlight: Color32,
    /// Inline code background.
    pub code_bg: Color32,
}

impl Palette {
    pub fn get(ctx: &egui::Context) -> Self {
        if ctx.style().visuals.dark_mode {
            Palette {
                dark: true,
                bg: Color32::from_rgb(0x1E, 0x1E, 0x1E),
                list_bg: Color32::from_rgb(0x26, 0x28, 0x2A),
                sidebar: Color32::from_rgb(0x1F, 0x21, 0x23),
                sidebar_edge: Color32::from_rgb(0x38, 0x3B, 0x3E),
                folder_selection: Color32::from_rgb(0xFF, 0xC9, 0x38),
                glass: Color32::from_rgb(0x26, 0x26, 0x27),
                glass_edge: Color32::from_rgb(0x3A, 0x3A, 0x3C),
                text: Color32::from_rgb(0xE4, 0xE4, 0xE4),
                secondary: Color32::from_rgb(0x98, 0x98, 0x9D),
                tertiary: Color32::from_rgb(0x5E, 0x5E, 0x62),
                separator: Color32::from_rgb(0x3A, 0x3D, 0x40),
                selection: Color32::from_rgb(0x82, 0x6A, 0x24),
                selection_text: Color32::WHITE,
                selection_inactive: Color32::from_rgb(0x46, 0x46, 0x46),
                text_selection: Color32::from_rgb(0x5E, 0x4B, 0x14),
                accent: ACCENT_DARK,
                hover: Color32::from_rgb(0x3A, 0x3A, 0x3C),
                field: Color32::from_rgb(0x32, 0x32, 0x34),
                card: Color32::from_rgb(0x2A, 0x2A, 0x2C),
                popover: Color32::from_rgb(0x26, 0x26, 0x28),
                highlight: Color32::from_rgb(0x8A, 0x6D, 0x00),
                code_bg: Color32::from_rgb(0x30, 0x30, 0x32),
            }
        } else {
            Palette {
                dark: false,
                bg: Color32::WHITE,
                list_bg: Color32::from_rgb(0xF6, 0xF6, 0xF6),
                sidebar: Color32::from_rgb(0xEC, 0xEC, 0xEC),
                sidebar_edge: Color32::from_rgb(0xD9, 0xD9, 0xD9),
                folder_selection: Color32::from_rgb(0xF7, 0xC1, 0x2E),
                glass: Color32::WHITE,
                glass_edge: Color32::from_rgb(0xDD, 0xDD, 0xDF),
                text: Color32::from_rgb(0x1D, 0x1D, 0x1F),
                secondary: Color32::from_rgb(0x86, 0x86, 0x8B),
                tertiary: Color32::from_rgb(0xBF, 0xBF, 0xC4),
                separator: Color32::from_rgb(0xE3, 0xE3, 0xE5),
                selection: Color32::from_rgb(0xFE, 0xDE, 0x6E),
                selection_text: Color32::from_rgb(0x1D, 0x1D, 0x1F),
                selection_inactive: Color32::from_rgb(0xE0, 0xE0, 0xE0),
                text_selection: Color32::from_rgb(0xFC, 0xE8, 0x9E),
                accent: ACCENT_LIGHT,
                hover: Color32::from_rgb(0xE6, 0xE5, 0xE4),
                field: Color32::from_rgb(0xE9, 0xE8, 0xE8),
                card: Color32::WHITE,
                popover: Color32::from_rgb(0xF7, 0xF7, 0xF7),
                highlight: Color32::from_rgb(0xFF, 0xE0, 0x4D),
                code_bg: Color32::from_rgb(0xF0, 0xF0, 0xF2),
            }
        }
    }

    /// The text color on a selected row.
    pub fn on_selection(&self, active: bool) -> Color32 {
        if active {
            self.selection_text
        } else {
            self.text
        }
    }
}

pub fn blend(a: Color32, b: Color32, t: f32) -> Color32 {
    let l = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t).round() as u8;
    Color32::from_rgb(l(a.r(), b.r()), l(a.g(), b.g()), l(a.b(), b.b()))
}

pub fn bold() -> FontFamily {
    FontFamily::Name("bold".into())
}

pub fn italic() -> FontFamily {
    FontFamily::Name("italic".into())
}

pub fn bold_italic() -> FontFamily {
    FontFamily::Name("bold-italic".into())
}

pub fn mono_bold() -> FontFamily {
    FontFamily::Name("mono-bold".into())
}

pub fn font(size: f32) -> FontId {
    FontId::new(size, FontFamily::Proportional)
}

pub fn bold_font(size: f32) -> FontId {
    FontId::new(size, bold())
}

/// The family for a character style.
pub fn family(bold: bool, italic: bool, mono: bool) -> FontFamily {
    match (mono, bold, italic) {
        (true, true, _) => mono_bold(),
        (true, false, _) => FontFamily::Monospace,
        (false, false, false) => FontFamily::Proportional,
        (false, true, false) => self::bold(),
        (false, false, true) => self::italic(),
        (false, true, true) => bold_italic(),
    }
}

/// Whether the real italic face was found (otherwise egui slants the
/// regular one).
pub fn has_italic() -> bool {
    fonts::ui(Face { bold: false, italic: true }).is_some()
}

/// Uses the platform's fonts when available (see [`crate::fonts`]), falling
/// back to egui's built-in ones.
pub fn install_fonts(ctx: &egui::Context) {
    let data = |f: &fonts::FontFile| FontData { index: f.index, ..FontData::from_owned(f.bytes.clone()) };
    let mut defs = FontDefinitions::default();
    let base_prop = defs.families[&FontFamily::Proportional].clone();
    let base_mono = defs.families[&FontFamily::Monospace].clone();
    let add = |defs: &mut FontDefinitions, family: FontFamily, key: &str, file: Option<&fonts::FontFile>, base: &[String]| {
        let mut list = base.to_vec();
        if let Some(f) = file {
            defs.font_data.insert(key.into(), data(f));
            list.insert(0, key.into());
        }
        defs.families.insert(family, list);
    };
    let regular = fonts::ui(Face { bold: false, italic: false });
    add(&mut defs, FontFamily::Proportional, "ui", regular, &base_prop);
    let prop = defs.families[&FontFamily::Proportional].clone();
    add(&mut defs, bold(), "ui-bold", fonts::ui(Face { bold: true, italic: false }), &prop);
    add(&mut defs, italic(), "ui-italic", fonts::ui(Face { bold: false, italic: true }), &prop);
    add(&mut defs, bold_italic(), "ui-bold-italic", fonts::ui(Face { bold: true, italic: true }), &prop);
    add(&mut defs, FontFamily::Monospace, "mono", fonts::mono(false), &base_mono);
    let mono = defs.families[&FontFamily::Monospace].clone();
    add(&mut defs, mono_bold(), "mono-bold", fonts::mono(true), &mono);
    ctx.set_fonts(defs);
}

pub fn apply_style(ctx: &egui::Context) {
    let p = Palette::get(ctx);
    ctx.style_mut(|s| {
        s.spacing.item_spacing = egui::vec2(8.0, 6.0);
        s.spacing.button_padding = egui::vec2(10.0, 4.0);
        s.visuals.panel_fill = p.bg;
        s.visuals.window_fill = p.popover;
        s.visuals.window_rounding = egui::Rounding::same(12.0);
        s.visuals.menu_rounding = egui::Rounding::same(8.0);
        s.visuals.selection.bg_fill = p.text_selection;
        s.visuals.selection.stroke.color = p.text;
        s.visuals.hyperlink_color = BLUE;
        s.visuals.text_cursor.stroke.color = p.accent;
        for w in [
            &mut s.visuals.widgets.inactive,
            &mut s.visuals.widgets.hovered,
            &mut s.visuals.widgets.active,
            &mut s.visuals.widgets.open,
        ] {
            w.rounding = egui::Rounding::same(6.0);
        }
        s.visuals.widgets.noninteractive.bg_stroke.color = p.separator;
        s.text_styles.insert(egui::TextStyle::Body, font(13.5));
        s.text_styles.insert(egui::TextStyle::Button, font(13.5));
        s.text_styles.insert(egui::TextStyle::Small, font(11.0));
        s.text_styles.insert(egui::TextStyle::Heading, bold_font(20.0));
        s.text_styles.insert(egui::TextStyle::Monospace, egui::FontId::monospace(13.0));
    });
}
