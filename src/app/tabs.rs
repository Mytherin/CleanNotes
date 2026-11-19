//! Tab notes (browser tabs CleanTabs saved as notes; see
//! [`crate::model::tabs`]) shown as previews of their web pages: in the note
//! pane a card with the page's picture, title and address and an "Open in
//! Browser" button above what's written about it; in the notes list the
//! site instead of the first lines, and the picture as the thumbnail; in the
//! gallery the picture as the card.

use eframe::egui::{self, Align2, Color32, Pos2, Rect, Sense, Stroke, Ui, Vec2};

use super::sidebar::truncate;
use crate::editor::images::{ImageCache, Shown};
use crate::icons::{self, Icon};
use crate::model::tabs::{self, TabNote};
use crate::theme::{self, Palette};

/// How round the card's picture is.
const ROUNDING: f32 = 12.0;

/// What a tab note says under its title in the list: its site.
pub fn site(tab: &TabNote) -> String {
    tabs::host(&tab.url).unwrap_or_else(|| tabs::short_url(&tab.url))
}

/// The card's "Open in Browser" button.
pub fn open_button_id(note_id: &str) -> egui::Id {
    egui::Id::new(("tab-open", note_id))
}

/// The page's picture, if it's here to show.
fn picture(ctx: &egui::Context, tab: &TabNote, images: &ImageCache) -> Option<(egui::TextureId, Vec2)> {
    match images.get(ctx, tab.thumbnail.as_deref()?) {
        Shown::Ready(tex, size) if size.x > 0.0 && size.y > 0.0 => Some((tex, size)),
        _ => None,
    }
}

/// A picture of `size` pixels filling `rect` (cut down to its shape, as
/// CSS's `object-fit: cover`), with round corners.
fn paint_cover(ui: &Ui, tex: egui::TextureId, size: Vec2, rect: Rect, rounding: f32) {
    let (img, r) = (size.x / size.y, rect.width() / rect.height().max(1.0));
    let uv = if img > r {
        let w = r / img;
        Rect::from_min_max(Pos2::new((1.0 - w) / 2.0, 0.0), Pos2::new((1.0 + w) / 2.0, 1.0))
    } else {
        let h = img / r;
        Rect::from_min_max(Pos2::new(0.0, (1.0 - h) / 2.0), Pos2::new(1.0, (1.0 + h) / 2.0))
    };
    egui::Image::new((tex, rect.size())).uv(uv).rounding(rounding).paint_at(ui, rect);
}

/// In place of a picture (none taken, or not here yet): the site's name on
/// a soft background.
fn paint_placeholder(ui: &Ui, p: &Palette, tab: &TabNote, rect: Rect, rounding: f32, size: f32) {
    let painter = ui.painter();
    painter.rect(rect, rounding, p.code_bg, Stroke::new(1.0_f32, p.separator));
    let name = site(tab);
    let icon = Rect::from_center_size(Pos2::new(rect.center().x, rect.center().y - size * 0.9), Vec2::splat(size * 1.1));
    if rect.height() > size * 4.0 {
        icons::paint(painter, Icon::Link, icon, p.tertiary);
    }
    let text_y = if rect.height() > size * 4.0 { rect.center().y + size * 0.4 } else { rect.center().y };
    let clip = painter.with_clip_rect(rect.shrink(6.0));
    clip.text(Pos2::new(rect.center().x, text_y), Align2::CENTER_CENTER, name, theme::bold_font(size), p.secondary);
}

/// The page's picture in `rect`, or the placeholder: the gallery's card
/// and the list's thumbnail.
pub fn paint_picture(ui: &Ui, p: &Palette, tab: &TabNote, rect: Rect, rounding: f32, images: &ImageCache, text_size: f32) {
    match picture(ui.ctx(), tab, images) {
        Some((tex, size)) => paint_cover(ui, tex, size, rect, rounding),
        None => paint_placeholder(ui, p, tab, rect, rounding, text_size),
    }
}

/// The card at the top of a tab note: the date, the page's picture (large,
/// or the site's name if there's none), its title and address, and "Open
/// in Browser". Laid out like the editor's text below it, in the same
/// column. Returns whether the page is to be opened.
pub fn card(ui: &mut Ui, p: &Palette, note_id: &str, tab: &TabNote, header: &str, images: &ImageCache) -> bool {
    let full = ui.available_rect_before_wrap();
    // As the editor's column (see `Editor::show`).
    let pad_x = 22.0_f32.max((full.width() - 900.0) / 2.0).min(full.width() / 6.0);
    let left = full.left() + pad_x;
    let width = (full.width() - 2.0 * pad_x).max(80.0);
    let mut open = false;

    // The date, centered, as on every note.
    let painter = ui.painter().clone();
    let date = painter.layout_no_wrap(header.to_owned(), theme::font(12.0), p.secondary);
    let mut y = full.top() + 14.0;
    painter.galley(Pos2::new(full.center().x - date.size().x / 2.0, y), date.clone(), p.secondary);
    y += date.size().y + 14.0;

    // The picture: as wide as the column (up to a point), as tall as it is
    // within reason.
    let pic_w = width.min(720.0);
    let shown = picture(ui.ctx(), tab, images);
    let pic_h = match (shown, &tab.thumbnail) {
        (Some((_, size)), _) => (pic_w * size.y / size.x).clamp(120.0, 420.0),
        // On its way (or can't be shown): about a screenshot's shape.
        (None, Some(_)) => (pic_w * 0.56).clamp(120.0, 360.0),
        (None, None) => 150.0,
    };
    let pic = Rect::from_min_size(Pos2::new(left, y), Vec2::new(pic_w, pic_h));
    painter.rect_filled(pic.translate(Vec2::new(0.0, 1.5)).expand(0.5), ROUNDING, Color32::from_black_alpha(18));
    match shown {
        Some((tex, size)) => {
            paint_cover(ui, tex, size, pic, ROUNDING);
            painter.rect_stroke(pic, ROUNDING, Stroke::new(1.0_f32, p.separator));
        }
        None => paint_placeholder(ui, p, tab, pic, ROUNDING, 22.0),
    }
    let pic_resp = ui.interact(pic, egui::Id::new(("tab-picture", note_id)), Sense::click()).on_hover_text(&tab.url);
    if pic_resp.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    open |= pic_resp.clicked();
    y = pic.bottom() + 16.0;

    // The title, then the address.
    let title = painter.layout(tab.title.clone(), theme::bold_font(22.0), p.text, width);
    let title_h = title.size().y;
    painter.galley(Pos2::new(left, y), title, p.text);
    y += title_h + 6.0;
    let address = truncate(&tabs::short_url(&tab.url), width - 22.0, ui);
    icons::paint(&painter, Icon::Link, Rect::from_center_size(Pos2::new(left + 7.0, y + 9.0), Vec2::splat(13.0)), p.secondary);
    painter.text(Pos2::new(left + 20.0, y + 9.0), Align2::LEFT_CENTER, address, theme::font(13.5), p.secondary);
    y += 18.0 + 14.0;

    // "Open in Browser".
    let label = painter.layout_no_wrap("Open in Browser".to_owned(), theme::bold_font(13.5), Color32::WHITE);
    let button = Rect::from_min_size(Pos2::new(left, y), Vec2::new(label.size().x + 50.0, 32.0));
    let resp = ui.interact(button, open_button_id(note_id), Sense::click()).on_hover_text(&tab.url);
    let fill = if resp.is_pointer_button_down_on() {
        theme::blend(theme::BLUE, Color32::BLACK, 0.2)
    } else if resp.hovered() {
        theme::blend(theme::BLUE, Color32::BLACK, 0.1)
    } else {
        theme::BLUE
    };
    if resp.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    painter.rect_filled(button, 8.0, fill);
    icons::paint(&painter, Icon::OpenApp, Rect::from_center_size(Pos2::new(button.left() + 22.0, button.center().y), Vec2::splat(14.0)), Color32::WHITE);
    painter.galley(Pos2::new(button.left() + 36.0, button.center().y - label.size().y / 2.0), label, Color32::WHITE);
    open |= resp.clicked();
    y = button.bottom() + 18.0;

    // A line between the page and what's written about it.
    painter.line_segment([Pos2::new(left, y), Pos2::new(left + width, y)], Stroke::new(1.0_f32, p.separator));
    y += 2.0;
    ui.allocate_rect(Rect::from_min_max(full.min, Pos2::new(full.right(), y)), Sense::hover());
    open
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::{App, Pane};
    use crate::model::Note;

    const BODY: &str = "# Rust \\*is\\* great\n\n[example.com/rust](https://www.example.com/rust)\n\nRead this later.\n\n- point\n";

    /// A tab note in the Tabs folder, open.
    fn app_with_tab(dir: &std::path::Path, body: &str) -> (App, String) {
        let mut app = App::for_test(dir);
        let folder = app.lib.store.ensure_folder_at(crate::model::LOCAL, &[tabs::FOLDER.to_owned(), "Reading".to_owned()]);
        let mut n = Note::new(crate::model::LOCAL, &folder, body);
        n.id = format!("{}1", tabs::ID_PREFIX);
        let id = n.id.clone();
        app.lib.store.notes.push(n);
        app.go_to(super::super::Location::Folder(folder));
        app.select(Some(id.clone()));
        (app, id)
    }

    fn frame(app: &mut App, ctx: &egui::Context, events: Vec<egui::Event>) {
        let raw = egui::RawInput {
            events,
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(1200.0, 800.0))),
            ..Default::default()
        };
        let _ = ctx.run(raw, |ctx| {
            theme::apply_style(ctx);
            let p = Palette::get(ctx);
            egui::SidePanel::left("list").show(ctx, |ui| app.notes_list(ui, &p));
            egui::CentralPanel::default().show(ctx, |ui| app.note_pane(ui, &p));
        });
        app.flush_editor();
    }

    #[test]
    fn a_tab_note_shows_its_page_above_its_text() {
        let dir = tempfile::tempdir().unwrap();
        let (mut app, id) = app_with_tab(dir.path(), BODY);
        let ctx = egui::Context::default();
        theme::install_fonts(&ctx);
        frame(&mut app, &ctx, vec![]);
        frame(&mut app, &ctx, vec![]);

        // The editor has just the text; the page is the card's.
        let e = app.editor.as_ref().unwrap();
        let tab = e.tab.as_ref().expect("a tab note");
        assert_eq!((tab.title.as_str(), tab.url.as_str()), ("Rust *is* great", "https://www.example.com/rust"));
        assert_eq!(e.doc.blocks[0].text, "Read this later.");
        let button = ctx.read_response(open_button_id(&id)).expect("an Open in Browser button");
        assert!(button.rect.width() > 60.0 && ctx.screen_rect().contains_rect(button.rect));
        // Drawing it changes nothing.
        assert_eq!(app.lib.store.note(&id).unwrap().body, BODY);

        // Typing in the text keeps the page as it was.
        app.editor.as_mut().unwrap().focus_requested = true;
        app.focus = Pane::Note;
        frame(&mut app, &ctx, vec![]);
        frame(&mut app, &ctx, vec![egui::Event::Text("Really. ".into())]);
        let body = app.lib.store.note(&id).unwrap().body.clone();
        let parsed = TabNote::parse(&body).unwrap();
        assert_eq!((parsed.title.as_str(), parsed.url.as_str()), ("Rust *is* great", "https://www.example.com/rust"));
        assert!(parsed.text.starts_with("Really. Read this later."), "{body}");
        assert_eq!(parsed.to_markdown(), body, "stable");

        // "Edit as Markdown" shows (and edits) the whole note.
        app.toggle_markdown_mode();
        frame(&mut app, &ctx, vec![]);
        assert_eq!(app.markdown_source.as_ref().unwrap().1, body);
        app.set_markdown(&body.replace("Rust", "Go"));
        assert_eq!(app.editor.as_ref().unwrap().tab.as_ref().unwrap().title, "Go *is* great");
        assert!(ctx.read_response(open_button_id(&id)).is_some());
    }

    #[test]
    fn the_card_opens_the_page_when_its_button_is_clicked() {
        let ctx = egui::Context::default();
        theme::install_fonts(&ctx);
        let dir = tempfile::tempdir().unwrap();
        let images = ImageCache::new(dir.path().to_owned());
        let tab = TabNote { title: "Example".into(), url: "https://example.com/".into(), thumbnail: None, text: String::new() };
        let run = |events: Vec<egui::Event>| {
            let raw = egui::RawInput { events, screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(800.0, 600.0))), ..Default::default() };
            let mut open = false;
            let _ = ctx.run(raw, |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| open = card(ui, &Palette::get(ctx), "tab-x", &tab, "Today", &images));
            });
            open
        };
        assert!(!run(vec![]));
        let at = ctx.read_response(open_button_id("tab-x")).unwrap().rect.center();
        let button = |pressed| egui::Event::PointerButton { pos: at, button: egui::PointerButton::Primary, pressed, modifiers: Default::default() };
        assert!(!run(vec![egui::Event::PointerMoved(at), button(true)]));
        assert!(run(vec![button(false)]));
    }

    #[test]
    fn a_tab_note_with_a_picture_in_the_list_and_gallery() {
        let dir = tempfile::tempdir().unwrap();
        let name = format!("{}.jpg", "a".repeat(64));
        let body = TabNote { title: "Pic".into(), url: "https://a.b/c".into(), thumbnail: Some(name.clone()), text: String::new() }.to_markdown();
        let (mut app, id) = app_with_tab(dir.path(), &body);
        let ctx = egui::Context::default();
        theme::install_fonts(&ctx);
        // The picture isn't here yet: the site stands in for it.
        frame(&mut app, &ctx, vec![]);
        assert_eq!(app.editor.as_ref().unwrap().tab.as_ref().unwrap().thumbnail.as_deref(), Some(name.as_str()));
        app.lib.settings.view = crate::settings::ListView::Gallery;
        let raw = egui::RawInput { screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(1200.0, 800.0))), ..Default::default() };
        let _ = ctx.run(raw, |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| app.gallery(ui, &Palette::get(ctx)));
        });
        assert_eq!(app.shown, [id.clone()]);
        assert_eq!(app.lib.store.note(&id).unwrap().body, body);
    }

    #[test]
    fn regular_notes_and_notes_merely_shaped_like_tabs_are_not_tab_notes() {
        let n = Note::new(crate::model::LOCAL, "f", BODY);
        assert!(n.tab().is_none(), "not made by CleanTabs");
        let e = crate::editor::Editor::new(&n.id, BODY);
        assert!(e.tab.is_none());
        assert_eq!(e.doc.blocks[0].text, "Rust *is* great");
        let tab = crate::model::tab_of("tab-2", BODY).unwrap();
        assert_eq!(site(&tab), "example.com");
    }
}
