//! Tab notes (browser tabs CleanTabs saved as notes; see
//! [`crate::model::tabs`]) shown as previews of their web pages: in the note
//! pane a card with the page's picture, title and address and an "Open in
//! Browser" button above what's written about it; in the notes list the
//! site instead of the first lines, and the picture as the thumbnail; in the
//! gallery the picture as the card.
//!
//! CleanTabs' own section in the sidebar (like CleanPassword's, but not
//! locked): an S3 account's saved tabs, synced as a hidden account of
//! their own (see `Library::tabs_accounts`), with All Tabs, their folders
//! and Recently Deleted, each shown as a gallery of websites.

use eframe::egui::{self, Align2, Color32, Pos2, Rect, Sense, Stroke, Ui, Vec2};

use super::sidebar::truncate;
use super::{App, Confirm, Location, Pane};
use crate::editor::images::{ImageCache, Shown};
use crate::icons::{self, Icon};
use crate::menu;
use crate::model::tabs::{self, TabNote};
use crate::theme::{self, Palette};

/// How round the card's picture is.
const ROUNDING: f32 = 12.0;

/// A CleanTabs section's entry in the folded sections and folders.
fn section_key(account: &str) -> String {
    format!("#cleantabs:{account}")
}

/// The round "Open in Browser" button on a website in the gallery, shown
/// while the pointer is on it. Returns whether it was clicked.
pub fn open_button(ui: &mut Ui, p: &Palette, card: Rect, note_id: &str, card_hovered: bool) -> bool {
    let b = Rect::from_center_size(card.right_top() + Vec2::new(-20.0, 20.0), Vec2::splat(28.0));
    if !card_hovered && !ui.rect_contains_pointer(b) {
        return false;
    }
    let resp = ui.interact(b, egui::Id::new(("tab-card-open", note_id)), Sense::click()).on_hover_text("Open in Browser");
    super::sidebar::glass(ui, p, b);
    if resp.hovered() {
        ui.painter().circle_filled(b.center(), b.height() / 2.0 - 3.0, p.hover);
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    icons::paint(ui.painter(), Icon::OpenApp, Rect::from_center_size(b.center(), Vec2::splat(14.0)), p.text);
    resp.clicked()
}

impl App {
    /// CleanTabs' section in the sidebar for a tabs account: All Tabs, its
    /// folders and its Recently Deleted. `owner` is its S3 account's name,
    /// said when several accounts have saved tabs.
    pub(super) fn tabs_section(&mut self, ui: &mut Ui, p: &Palette, account: &str, owner: &str, several: bool) {
        let key = section_key(account);
        let collapsed = self.lib.settings.collapsed.contains(&key);
        let (rect, resp) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 30.0), Sense::click());
        let painter = ui.painter();
        let title = if several { format!("CleanTabs · {owner}") } else { "CleanTabs".to_owned() };
        let title = truncate(&title, rect.width() - 50.0, theme::bold_font(12.0), ui);
        painter.text(Pos2::new(rect.left() + 16.0, rect.center().y + 2.0), Align2::LEFT_CENTER, title, theme::bold_font(12.0), p.secondary);
        let right = Rect::from_center_size(Pos2::new(rect.right() - 16.0, rect.center().y + 2.0), Vec2::splat(12.0));
        let error = self.lib.sync_errors.get(account).cloned();
        if resp.hovered() {
            icons::paint(painter, if collapsed { Icon::ChevronRight } else { Icon::ChevronDown }, right, p.secondary);
        } else if self.syncing(account) {
            let t = ui.input(|i| i.time) as f32;
            painter.add(egui::Shape::circle_stroke(right.center(), 5.0, egui::Stroke::new(1.0_f32, p.tertiary)));
            let a = t * 6.0;
            painter.line_segment([right.center(), right.center() + Vec2::new(a.cos(), a.sin()) * 5.0], egui::Stroke::new(1.5_f32, p.secondary));
            ui.ctx().request_repaint();
        } else if error.is_some() {
            icons::paint(painter, Icon::Warning, right, theme::RED);
        }
        let tip = match &error {
            Some(e) => format!("Couldn't sync: {e}"),
            None => format!("Tabs saved with CleanTabs, in “{owner}”"),
        };
        let resp = resp.on_hover_text(tip);
        if resp.clicked() {
            if collapsed {
                self.lib.settings.collapsed.retain(|c| *c != key);
            } else {
                self.lib.settings.collapsed.push(key.clone());
            }
        }
        resp.context_menu(|ui| {
            menu::begin(ui, 200.0);
            if menu::item("Sync Now").icon(Icon::Sync).show(ui).clicked() {
                self.sync_now(account);
                ui.close_menu();
            }
        });
        if collapsed {
            return;
        }

        let (counts, all, deleted) = self.lib.tab_counts(account);
        let at = |loc: &Location| self.location == *loc && self.search.is_empty();
        let mut go: Option<Location> = None;
        let loc = Location::Tabs(account.to_owned(), None);
        if self.row(ui, p, 0, Icon::Gallery, "All Tabs", Some(all), at(&loc), None).clicked() {
            go = Some(loc);
        }
        for (id, name, depth) in self.lib.tab_folders(account) {
            let loc = Location::Tabs(account.to_owned(), Some(id.clone()));
            let n = counts.get(&id).copied().unwrap_or(0);
            if self.row(ui, p, depth, Icon::Folder, &name, Some(n), at(&loc), None).clicked() {
                go = Some(loc);
            }
        }
        if deleted > 0 {
            let loc = Location::TabsDeleted(account.to_owned());
            let resp = self.row(ui, p, 0, Icon::Trash, "Recently Deleted", Some(deleted), at(&loc), None);
            if resp.clicked() {
                go = Some(loc);
            }
            resp.context_menu(|ui| {
                menu::begin(ui, 200.0);
                if menu::item("Empty Recently Deleted…").icon(Icon::Trash).show(ui).clicked() {
                    self.confirm = Some(Confirm::EmptyDeleted(account.to_owned()));
                }
            });
        }
        if let Some(loc) = go {
            self.go_to(loc);
            self.focus = Pane::Sidebar;
        }
    }
}

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
    let address = truncate(&tabs::short_url(&tab.url), width - 22.0, theme::font(13.5), ui);
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

    /// A tab note among the notes (as CleanTabs' first versions saved
    /// them), open.
    fn app_with_tab(dir: &std::path::Path, body: &str) -> (App, String) {
        let mut app = App::for_test(dir);
        let folder = app.lib.store.ensure_folder_at(crate::model::LOCAL, &["Tabs".to_owned(), "Reading".to_owned()]);
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

    // ---- CleanTabs' section and gallery ---------------------------------

    /// Every text painted in a frame.
    fn texts(shapes: &[egui::epaint::ClippedShape]) -> Vec<(String, Rect)> {
        fn walk(s: &egui::Shape, out: &mut Vec<(String, Rect)>) {
            match s {
                egui::Shape::Text(t) => out.push((t.galley.text().to_owned(), t.galley.rect.translate(t.pos.to_vec2()))),
                egui::Shape::Vec(v) => v.iter().for_each(|s| walk(s, out)),
                _ => {}
            }
        }
        let mut out = Vec::new();
        shapes.iter().for_each(|c| walk(&c.shape, &mut out));
        out
    }

    /// One frame of the whole window, as `App::frame` lays it out, at
    /// `time`: what was painted.
    fn window(app: &mut App, ctx: &egui::Context, time: f64, events: Vec<egui::Event>) -> egui::FullOutput {
        let raw = egui::RawInput {
            events,
            time: Some(time),
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(1200.0, 800.0))),
            ..Default::default()
        };
        let out = ctx.run(raw, |ctx| {
            theme::apply_style(ctx);
            let p = Palette::get(ctx);
            app.shortcuts(ctx);
            egui::SidePanel::left("sidebar").exact_width(240.0).frame(egui::Frame::none().fill(p.list_bg)).show(ctx, |ui| app.sidebar(ui, &p));
            if !app.shows_gallery() {
                egui::SidePanel::left("notes-list").exact_width(300.0).frame(egui::Frame::none().fill(p.list_bg)).show(ctx, |ui| app.notes_list(ui, &p));
            }
            egui::CentralPanel::default().frame(egui::Frame::none().fill(p.bg)).show(ctx, |ui| {
                if app.shows_gallery() {
                    app.gallery(ui, &p);
                } else {
                    app.note_pane(ui, &p);
                }
            });
        });
        app.flush_editor();
        keep_textures(&out);
        out
    }

    /// A page's picture: a soft gradient with a bar, as a screenshot of a
    /// page might look.
    fn page_picture(hue: [u8; 3]) -> Vec<u8> {
        let (w, h) = (320usize, 200usize);
        let mut px = Vec::with_capacity(w * h * 4);
        for y in 0..h {
            for x in 0..w {
                let bar = y < 28;
                let t = (x + y) as f32 / (w + h) as f32;
                let c = |v: u8| if bar { v / 2 } else { (v as f32 * (0.55 + 0.45 * t)) as u8 };
                px.extend_from_slice(&[c(hue[0]), c(hue[1]), c(hue[2]), 255]);
            }
        }
        crate::editor::images::png_from_rgba(w, h, &px).unwrap()
    }

    #[test]
    fn cleantabs_is_a_section_of_its_own_with_a_gallery_of_websites() {
        use crate::app::Location;
        use crate::sync::{s3, BackendConfig};
        let dir = tempfile::tempdir().unwrap();
        let mut app = App::for_test(dir.path());
        let config = s3::Config { bucket: "b".into(), region: "eu-west-1".into(), prefix: "CleanNotes".into(), endpoint: None, encrypted: false };
        app.lib.store.accounts.push(crate::model::Account {
            id: "work".into(),
            name: "Work".into(),
            backend: BackendConfig::S3(config),
            enabled: true,
            cursor: None,
            last_sync: None,
            tabs_of: None,
        });
        app.lib.store.ensure_local();
        let home = app.lib.default_folder_id("work");
        let mut plan = Note::new("work", &home, "# Plan\nrust everywhere\n");
        plan.id = "plan".into();
        app.lib.store.notes.push(plan);
        // A folder of the user's own called Tabs: just a folder.
        let own = app.lib.store.ensure_folder_at("work", &["Tabs".into()]);
        let mut mine = Note::new("work", &own, "# Tabs to close\n");
        mine.id = "mine".into();
        app.lib.store.notes.push(mine);

        // The bucket has saved tabs: a tabs account, synced as one.
        assert!(app.lib.set_tabs_found("work", true));
        let tabs_acc = crate::model::tabs_account_id("work");
        let pages: [(&[&str], &str, &str, Option<[u8; 3]>); 5] = [
            (&["Research"], "The Rust Programming Language", "https://doc.rust-lang.org/book/", Some([222, 165, 132])),
            (&["Research"], "docs.rs", "https://docs.rs/egui/latest/egui/", Some([90, 140, 220])),
            (&["Research"], "Rust by Example", "https://doc.rust-lang.org/rust-by-example/", None),
            (&["Research", "Crates"], "eframe on crates.io", "https://crates.io/crates/eframe", Some([120, 190, 120])),
            (&["Reading"], "Hacker News", "https://news.ycombinator.com/", None),
        ];
        let mut ids = Vec::new();
        for (i, (path, title, url, hue)) in pages.iter().enumerate() {
            let folder = app.lib.store.ensure_folder_at(&tabs_acc, &path.iter().map(|s| s.to_string()).collect::<Vec<_>>());
            let thumbnail = hue.map(|h| app.lib.add_image(&page_picture(h)).unwrap());
            let body = TabNote { title: (*title).into(), url: (*url).into(), thumbnail, text: String::new() }.to_markdown();
            let mut n = Note::new(&tabs_acc, &folder, &body);
            n.id = format!("tab-{i}");
            n.created += chrono::Duration::milliseconds(i as i64);
            ids.push(n.id.clone());
            app.lib.store.notes.push(n);
        }
        let ctx = egui::Context::default();
        theme::install_fonts(&ctx);

        // The notes: no tabs in All Notes or the account's folders, nor in
        // search; the user's Tabs folder is there. CleanTabs is a section.
        app.go_to(Location::All);
        window(&mut app, &ctx, 0.0, vec![]);
        let out = window(&mut app, &ctx, 0.1, vec![]);
        assert_eq!(app.shown, ["mine", "plan"]);
        let painted: Vec<String> = texts(&out.shapes).into_iter().map(|(t, _)| t).collect();
        for t in ["Work", "Tabs", "CleanTabs", "All Tabs", "Research", "Crates", "Reading"] {
            assert!(painted.iter().any(|p| p == t), "{t} in {painted:?}");
        }
        assert!(!painted.iter().any(|p| p.starts_with("Hacker News")), "no tab in the notes list");
        app.search = "rust".into();
        app.search_changed();
        window(&mut app, &ctx, 0.2, vec![]);
        assert_eq!(app.shown, ["plan"], "search finds notes, not tabs");
        app.search.clear();
        app.search_changed();

        // All Tabs: websites in a gallery (whatever the notes' view), by
        // folder, in the order they were saved.
        app.go_to(Location::Tabs(tabs_acc.clone(), None));
        let mut out = window(&mut app, &ctx, 1.0, vec![]);
        for i in 0..200 {
            // (Pictures decode on threads of their own.)
            out = window(&mut app, &ctx, 1.0 + i as f64 * 0.01, vec![]);
            let ready = ids.iter().filter_map(|id| app.lib.store.note(id)?.tab()?.thumbnail).all(|t| matches!(app.images.get(&ctx, &t), Shown::Ready(..)));
            if ready && i > 2 {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        out = window(&mut app, &ctx, 3.0, vec![]);
        assert!(app.shows_gallery());
        assert_eq!(app.shown, ["tab-4", "tab-0", "tab-1", "tab-2", "tab-3"]);
        let painted = texts(&out.shapes);
        for t in ["All Tabs", "Reading", "Research", "Research / Crates", "docs.rs", "doc.rust-lang.org", "news.ycombinator.com", "crates.io"] {
            assert!(painted.iter().any(|(p, _)| p == t), "{t} in {:?}", painted.iter().map(|(p, _)| p).collect::<Vec<_>>());
        }
        let shot = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("target").join("screenshots").join("cleantabs-gallery.png");
        render(&ctx, &out, &shot);

        // A click opens the tab's note (once it's no double-click): its
        // page's card, with Open in Browser.
        let (_, title) = painted.iter().find(|(p, _)| p.starts_with("Hacker News")).unwrap().clone();
        let card = title.center() - Vec2::new(0.0, 14.0 + 138.0 / 2.0);
        let press = |pressed| egui::Event::PointerButton { pos: card, button: egui::PointerButton::Primary, pressed, modifiers: Default::default() };
        window(&mut app, &ctx, 4.0, vec![egui::Event::PointerMoved(card), press(true)]);
        window(&mut app, &ctx, 4.05, vec![press(false)]);
        assert_eq!(app.selected.as_deref(), Some("tab-4"));
        assert!(!app.gallery_open, "not yet: a double-click may follow");
        window(&mut app, &ctx, 4.6, vec![egui::Event::PointerMoved(Pos2::new(1190.0, 790.0))]);
        assert!(app.gallery_open);
        let out = window(&mut app, &ctx, 4.7, vec![]);
        assert!(ctx.read_response(open_button_id("tab-4")).is_some(), "the tab's note, with its page");
        let shot = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("target").join("screenshots").join("cleantabs-tab.png");
        render(&ctx, &out, &shot);

        // Back, and deleted: to CleanTabs' Recently Deleted.
        app.gallery_open = false;
        app.focus = Pane::List;
        window(&mut app, &ctx, 5.0, vec![egui::Event::Key { key: egui::Key::Delete, physical_key: None, pressed: true, repeat: false, modifiers: Default::default() }]);
        assert!(app.lib.store.note("tab-4").unwrap().deleted.is_some());
        app.go_to(Location::TabsDeleted(tabs_acc.clone()));
        window(&mut app, &ctx, 5.1, vec![]);
        assert_eq!(app.shown, ["tab-4"]);
        app.go_to(Location::Deleted("work".into()));
        assert!(app.visible_notes().is_empty(), "not in the notes' Recently Deleted");

        // A folder: its tabs and those in its folders.
        let research = app.lib.store.folder_at(&tabs_acc, &["Research".into()]).unwrap().id.clone();
        app.go_to(Location::Tabs(tabs_acc.clone(), Some(research)));
        window(&mut app, &ctx, 6.0, vec![]);
        assert_eq!(app.shown, ["tab-0", "tab-1", "tab-2", "tab-3"]);
    }

    /// Paints a frame into a PNG at `path` (egui's triangles, filled in on
    /// the CPU), to look at the gallery without a window.
    fn render(ctx: &egui::Context, out: &egui::FullOutput, path: &std::path::Path) {
        use egui::epaint::Primitive;
        let (w, h) = (1200usize, 800usize);
        let mut buf = vec![[0.0f32; 4]; w * h];
        let prims = ctx.tessellate(out.shapes.clone(), out.pixels_per_point);
        TEXTURES.with(|t| {
            let textures = t.borrow();
            for p in prims {
                let Primitive::Mesh(mesh) = p.primitive else { continue };
                let Some((tw, th, tex)) = textures.get(&mesh.texture_id) else { continue };
                let clip = p.clip_rect.intersect(Rect::from_min_size(Pos2::ZERO, Vec2::new(w as f32, h as f32)));
                for tri in mesh.indices.chunks(3) {
                    let [a, b, c] = [mesh.vertices[tri[0] as usize], mesh.vertices[tri[1] as usize], mesh.vertices[tri[2] as usize]];
                    let area = (b.pos - a.pos).x * (c.pos - a.pos).y - (b.pos - a.pos).y * (c.pos - a.pos).x;
                    if area.abs() < 1e-6 {
                        continue;
                    }
                    let min = a.pos.min(b.pos).min(c.pos).max(clip.min);
                    let max = a.pos.max(b.pos).max(c.pos).min(clip.max);
                    for y in (min.y.floor().max(0.0) as usize)..(max.y.ceil().max(0.0) as usize).min(h) {
                        for x in (min.x.floor().max(0.0) as usize)..(max.x.ceil().max(0.0) as usize).min(w) {
                            let pt = Pos2::new(x as f32 + 0.5, y as f32 + 0.5);
                            if !clip.contains(pt) {
                                continue;
                            }
                            let edge = |p: Pos2, q: Pos2| (q - p).x * (pt - p).y - (q - p).y * (pt - p).x;
                            let (wa, wb, wc) = (edge(b.pos, c.pos) / area, edge(c.pos, a.pos) / area, edge(a.pos, b.pos) / area);
                            if wa < -1e-4 || wb < -1e-4 || wc < -1e-4 {
                                continue;
                            }
                            let uv = a.uv.to_vec2() * wa + b.uv.to_vec2() * wb + c.uv.to_vec2() * wc;
                            let tx = ((uv.x * *tw as f32).max(0.0) as usize).min(tw - 1);
                            let ty = ((uv.y * *th as f32).max(0.0) as usize).min(th - 1);
                            let texel = tex[ty * tw + tx].to_array();
                            let (ca, cb, cc) = (a.color.to_array(), b.color.to_array(), c.color.to_array());
                            let mut src = [0.0f32; 4];
                            for k in 0..4 {
                                let v = (ca[k] as f32 * wa + cb[k] as f32 * wb + cc[k] as f32 * wc) / 255.0;
                                src[k] = v * texel[k] as f32 / 255.0;
                            }
                            let d = &mut buf[y * w + x];
                            for k in 0..4 {
                                d[k] = src[k] + d[k] * (1.0 - src[3]);
                            }
                        }
                    }
                }
            }
        });
        let mut img = image::RgbaImage::new(w as u32, h as u32);
        for (i, px) in buf.iter().enumerate() {
            let c = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
            img.put_pixel((i % w) as u32, (i / w) as u32, image::Rgba([c(px[0]), c(px[1]), c(px[2]), 255]));
        }
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        img.save(path).unwrap();
    }

    thread_local! {
        /// Every texture egui has made in this test: what `render` paints
        /// with (egui only sends what changed each frame).
        static TEXTURES: std::cell::RefCell<std::collections::HashMap<egui::TextureId, (usize, usize, Vec<Color32>)>> = Default::default();
    }

    /// Keeps a frame's new and changed textures.
    fn keep_textures(out: &egui::FullOutput) {
        use egui::epaint::ImageData;
        TEXTURES.with(|t| {
            let mut textures = t.borrow_mut();
            for (id, delta) in &out.textures_delta.set {
                let (w, h, px): (usize, usize, Vec<Color32>) = match &delta.image {
                    ImageData::Color(c) => (c.size[0], c.size[1], c.pixels.clone()),
                    ImageData::Font(f) => (f.size[0], f.size[1], f.srgba_pixels(None).collect()),
                };
                match delta.pos {
                    None => {
                        textures.insert(*id, (w, h, px));
                    }
                    Some([x0, y0]) => {
                        let Some((tw, _, tex)) = textures.get_mut(id) else { continue };
                        for y in 0..h {
                            for x in 0..w {
                                tex[(y0 + y) * *tw + x0 + x] = px[y * w + x];
                            }
                        }
                    }
                }
            }
        });
    }
}
