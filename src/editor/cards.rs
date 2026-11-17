//! Attached files in notes, shown as in Apple Notes: a paragraph that's
//! only a link to an attached file ([`crate::doc::Doc::file_at`]) is a
//! card, its link's text (the file's name) the card's title.
//!
//! - **Small** (other files always, and PDFs viewed so): the name, its
//!   kind and size below it, a small picture of it on the right and the
//!   menu's ⌄ beside that.
//! - **Medium**, **Large** (PDFs): the name and ⌄ at the top, "Show
//!   Thumbnails" on the right, and the pages side by side below, scrolled
//!   sideways; large pages are as wide as the card. Thumbnails, if shown,
//!   are a row below the pages, to go to one.
//!
//! The name is the paragraph's galley, drawn by the editor at
//! [`LaidFile::text_origin`] (so it's edited and selected as text); the
//! rest is drawn here.

use eframe::egui::{self, Align2, Color32, FontId, Pos2, Rect, Stroke, TextureId, Vec2};

use crate::doc::FileView;
use crate::theme::{self, Palette};

/// The widest a small card, and a card of pages, get.
pub const SMALL_MAX_W: f32 = 440.0;
pub const PAGES_MAX_W: f32 = 900.0;
/// Space around a card, and inside it.
pub const MARGIN: f32 = 4.0;
const PAD: f32 = 14.0;
/// Space between pages; how high medium pages are, and large ones at most.
pub const PAGE_GAP: f32 = 12.0;
pub const MEDIUM_PAGE_H: f32 = 340.0;
const LARGE_MAX_H: f32 = 1100.0;
/// A small card's picture.
const THUMB: Vec2 = Vec2::new(40.0, 52.0);
/// The ⌄ button's radius; the ‹ › buttons'.
const MENU_R: f32 = 12.0;
const ARROW_R: f32 = 15.0;
/// The thumbnails' row: their height, and the gap between them.
const ROW_H: f32 = 64.0;
const ROW_GAP: f32 = 8.0;
/// Room kept at the top right for "Show Thumbnails".
const TOGGLE_W: f32 = 130.0;
/// The name's size in a card of pages.
pub const TITLE_SIZE: f32 = 17.0;

/// Whether an attachment is a PDF, by its name (a locked note's is
/// encrypted, `….enc`: then by the name it's shown with).
pub fn is_pdf_name(reference: &str, label: &str) -> bool {
    let (name, _) = cleannotes_core::attachments::split_ref(reference);
    let pdf = |s: &str| s.len() > 4 && s.is_char_boundary(s.len() - 4) && s[s.len() - 4..].eq_ignore_ascii_case(".pdf");
    let sealed = name.ends_with(&format!(".{}", cleannotes_core::attachments::SEALED_EXT));
    pdf(name) || (sealed && pdf(label.trim()))
}

/// A file's extension, from the name it's shown with or its attachment's.
pub fn extension(reference: &str, label: &str) -> String {
    let ext = |s: &str| s.rsplit_once('.').map(|(_, e)| e.to_owned()).filter(|e| (1..=5).contains(&e.len()) && e.chars().all(|c| c.is_ascii_alphanumeric()));
    let (name, _) = cleannotes_core::attachments::split_ref(reference);
    ext(label.trim()).or_else(|| ext(name).filter(|e| e != cleannotes_core::attachments::SEALED_EXT)).unwrap_or_default().to_ascii_lowercase()
}

/// What kind of file an extension is, as Apple Notes says it.
pub fn kind_name(ext: &str) -> String {
    match ext {
        "pdf" => "PDF Document".into(),
        "doc" | "docx" | "rtf" => "Word Document".into(),
        "xls" | "xlsx" => "Excel Spreadsheet".into(),
        "ppt" | "pptx" => "PowerPoint Presentation".into(),
        "pages" => "Pages Document".into(),
        "numbers" => "Numbers Spreadsheet".into(),
        "key" => "Keynote Presentation".into(),
        "txt" | "md" => "Plain Text".into(),
        "csv" => "CSV Document".into(),
        "zip" => "ZIP Archive".into(),
        "mp3" | "m4a" | "wav" | "aac" | "flac" => "Audio".into(),
        "mp4" | "mov" | "m4v" | "avi" => "Video".into(),
        "" => "Document".into(),
        e => format!("{} Document", e.to_ascii_uppercase()),
    }
}

/// A size in bytes as Apple Notes shows it: "211 KB", "1.4 MB".
pub fn size_text(bytes: u64) -> String {
    const K: f64 = 1000.0;
    let b = bytes as f64;
    if b < K {
        format!("{bytes} bytes")
    } else if b < K * K {
        format!("{:.0} KB", b / K)
    } else if b < K * K * K {
        format!("{:.1} MB", b / (K * K))
    } else {
        format!("{:.1} GB", b / (K * K * K))
    }
}

/// An attached file as laid out.
#[derive(Clone)]
pub struct LaidFile {
    /// The attachment's reference, and the name it's shown with.
    pub reference: String,
    pub label: String,
    pub pdf: bool,
    /// How it's shown (small for a file that isn't a PDF that can be read).
    pub view: FileView,
    /// "PDF Document · 211 KB", and where it goes on a small card.
    pub details: String,
    pub details_pos: Pos2,
    pub ext: String,
    pub card: Rect,
    /// Where the galley (the name) goes.
    pub text_origin: Pos2,
    /// The ⌄ button.
    pub menu: Rect,
    /// A small card's picture; else where the pages show (cut off at its
    /// edges).
    pub strip: Rect,
    /// The pages in or near view, where they are now (scrolled); a small
    /// card's first page.
    pub pages: Vec<LaidPage>,
    /// Each page's start, from the first's, for going to one.
    pub starts: Vec<f32>,
    pub scroll: f32,
    pub max_scroll: f32,
    /// "Show Thumbnails", and the thumbnails if shown.
    pub toggle: Option<Rect>,
    pub thumbs_shown: bool,
    pub thumbs: Vec<LaidPage>,
    /// The page at the strip's left edge (its thumbnail is marked).
    pub current: usize,
}

/// A page as laid out; `tex` once it's drawn.
#[derive(Clone)]
pub struct LaidPage {
    pub index: usize,
    pub rect: Rect,
    pub tex: Option<TextureId>,
}

impl LaidFile {
    /// How wide the name may be in a small card in a column `width` wide.
    pub fn small_text_w(width: f32) -> f32 {
        width.min(SMALL_MAX_W) - 2.0 * PAD - THUMB.x - 2.0 * MENU_R - 24.0
    }

    /// How wide the name may be in a card of pages.
    pub fn pages_text_w(width: f32) -> f32 {
        width.min(PAGES_MAX_W) - 2.0 * PAD - 2.0 * MENU_R - 16.0 - TOGGLE_W
    }

    /// A small card at `pos` in a column `width` wide, its name `text`
    /// (the galley's size) big; `first_page` the first page's size, for a
    /// PDF that can be read (shown in its picture).
    #[allow(clippy::too_many_arguments)]
    pub fn small(reference: String, label: String, pdf: bool, details: String, ext: String, pos: Pos2, width: f32, text: Vec2, first_page: Option<Vec2>) -> LaidFile {
        let w = width.min(SMALL_MAX_W);
        let h = (text.y + 3.0 + 16.0).max(THUMB.y) + 2.0 * PAD;
        let card = Rect::from_min_size(pos, Vec2::new(w, h));
        let thumb = Rect::from_min_size(Pos2::new(card.right() - PAD - THUMB.x, card.center().y - THUMB.y / 2.0), THUMB);
        let menu = Rect::from_center_size(Pos2::new(thumb.left() - 12.0 - MENU_R, card.center().y), Vec2::splat(2.0 * MENU_R));
        let text_origin = Pos2::new(card.left() + PAD + 2.0, card.center().y - (text.y + 3.0 + 16.0) / 2.0);
        let pages = first_page
            .map(|size| {
                // The page fitted to the picture's box.
                let s = size * (THUMB.x / size.x).min(THUMB.y / size.y);
                vec![LaidPage { index: 0, rect: Rect::from_center_size(thumb.center(), s), tex: None }]
            })
            .unwrap_or_default();
        let details_pos = text_origin + Vec2::new(0.0, text.y + 3.0);
        LaidFile {
            reference,
            label,
            pdf,
            view: FileView::Small,
            details,
            details_pos,
            ext,
            card,
            text_origin,
            menu,
            strip: thumb,
            pages,
            starts: vec![0.0],
            scroll: 0.0,
            max_scroll: 0.0,
            toggle: None,
            thumbs_shown: false,
            thumbs: Vec::new(),
            current: 0,
        }
    }

    /// A card of a PDF's pages (`sizes`: `None` while it's read, then one
    /// A4 page's place) at `pos`, `view` Medium or Large, in a column
    /// `width` wide; its name's galley `first_row` wide (its first row)
    /// and `text_h` high; scrolled by `scroll` (kept within reach).
    #[allow(clippy::too_many_arguments)]
    pub fn pages(
        reference: String,
        label: String,
        details: String,
        view: FileView,
        pos: Pos2,
        width: f32,
        sizes: Option<&[Vec2]>,
        first_row: Vec2,
        text_h: f32,
        scroll: f32,
        thumbs_shown: bool,
    ) -> LaidFile {
        let a4 = [Vec2::new(595.0, 842.0)];
        let sizes = sizes.unwrap_or(&a4);
        let w = width.min(PAGES_MAX_W);
        let left = pos.x + PAD + 2.0;
        let inner = w - 2.0 * (PAD + 2.0);
        let text_origin = Pos2::new(left, pos.y + 12.0);
        let menu = Rect::from_center_size(Pos2::new(left + first_row.x + 10.0 + MENU_R, text_origin.y + first_row.y / 2.0), Vec2::splat(2.0 * MENU_R));
        let toggle = Rect::from_min_size(Pos2::new(pos.x + w - PAD - TOGGLE_W, text_origin.y), Vec2::new(TOGGLE_W, first_row.y));
        let top = text_origin.y + text_h + 12.0;
        let page_h = match view {
            FileView::Large => (inner * sizes[0].y / sizes[0].x).clamp(120.0, LARGE_MAX_H),
            _ => MEDIUM_PAGE_H,
        };
        let page_w = |s: Vec2| (s.x / s.y * page_h).clamp(page_h * 0.25, inner.min(page_h * 2.2).max(page_h * 0.25));
        // The pages run to the card's right edge (cut off there).
        let strip = Rect::from_min_max(Pos2::new(left, top), Pos2::new(pos.x + w, top + page_h));
        let mut starts = Vec::with_capacity(sizes.len());
        let mut x = 0.0;
        for s in sizes {
            starts.push(x);
            x += page_w(*s) + PAGE_GAP;
        }
        let total = x - PAGE_GAP;
        let max_scroll = (total - inner).max(0.0);
        let scroll = scroll.clamp(0.0, max_scroll);
        let current = starts.iter().rposition(|&s| s <= scroll + 1.0).unwrap_or(0);
        let mut pages = Vec::new();
        for (index, s) in sizes.iter().enumerate() {
            let rect = Rect::from_min_size(Pos2::new(left + starts[index] - scroll, top), Vec2::new(page_w(*s), page_h));
            // Those in view, and a strip's width either side (drawn ahead).
            if rect.right() > strip.left() - strip.width() && rect.left() < strip.right() + strip.width() {
                pages.push(LaidPage { index, rect, tex: None });
            }
        }
        let mut bottom = strip.bottom();
        let mut thumbs = Vec::new();
        if thumbs_shown {
            // A row that follows the pages along when it's wider than the card.
            let tw = |s: Vec2| (s.x / s.y * ROW_H).clamp(ROW_H * 0.25, ROW_H * 2.2);
            let row_total: f32 = sizes.iter().map(|s| tw(*s) + ROW_GAP).sum::<f32>() - ROW_GAP;
            let shift = if max_scroll > 0.0 { scroll / max_scroll * (row_total - inner).max(0.0) } else { 0.0 };
            let y = strip.bottom() + 12.0;
            let mut x = left - shift;
            for (index, s) in sizes.iter().enumerate() {
                let rect = Rect::from_min_size(Pos2::new(x, y), Vec2::new(tw(*s), ROW_H));
                if rect.right() > left - 2.0 * ROW_H && rect.left() < pos.x + w + 2.0 * ROW_H {
                    thumbs.push(LaidPage { index, rect, tex: None });
                }
                x += rect.width() + ROW_GAP;
            }
            bottom = y + ROW_H;
        }
        let card = Rect::from_min_max(pos, Pos2::new(pos.x + w, bottom + PAD + 2.0));
        LaidFile {
            reference,
            label,
            pdf: true,
            view,
            details,
            details_pos: text_origin,
            ext: "pdf".into(),
            card,
            text_origin,
            menu,
            strip,
            pages,
            starts,
            scroll,
            max_scroll,
            toggle: Some(toggle),
            thumbs_shown,
            thumbs,
            current,
        }
    }

    /// The ‹ (`-1`) or › (`1`) button, if it can scroll that way.
    pub fn arrow(&self, dir: i32) -> Option<Rect> {
        if self.view == FileView::Small {
            return None;
        }
        let can = if dir < 0 { self.scroll > 0.5 } else { self.scroll < self.max_scroll - 0.5 };
        let x = if dir < 0 { self.strip.left() + 8.0 + ARROW_R } else { self.card.right() - PAD - 8.0 - ARROW_R };
        can.then(|| Rect::from_center_size(Pos2::new(x, self.strip.center().y), Vec2::splat(2.0 * ARROW_R)))
    }

    /// The ‹ › button under a point: its direction.
    pub fn arrow_at(&self, pt: Pos2) -> Option<i32> {
        [-1, 1].into_iter().find(|d| self.arrow(*d).is_some_and(|r| r.expand(2.0).contains(pt)))
    }

    /// The thumbnail under a point: its page.
    pub fn thumb_at(&self, pt: Pos2) -> Option<usize> {
        self.thumbs.iter().find(|t| t.rect.contains(pt) && self.card.contains(pt)).map(|t| t.index)
    }

    /// Whether a point is on "Show Thumbnails".
    pub fn toggle_at(&self, pt: Pos2) -> bool {
        self.toggle.is_some_and(|r| r.contains(pt))
    }

    /// Whether a point is on its pages (or a small card's picture): what
    /// a click opens it with.
    pub fn pages_at(&self, pt: Pos2) -> bool {
        self.strip.intersect(self.card).contains(pt) && self.arrow_at(pt).is_none()
    }

    /// Where page `index` is lined up at the left edge (as far as it can).
    pub fn scroll_to(&self, index: usize) -> f32 {
        self.starts.get(index).copied().unwrap_or(0.0).clamp(0.0, self.max_scroll)
    }

    /// Where a ‹ or › press scrolls to from `target`: the page that starts
    /// a strip's width (less a little) that way, lined up at the left edge.
    pub fn step(&self, target: f32, dir: i32) -> f32 {
        let span = (self.strip.width() - PAD - PAGE_GAP * 4.0).max(PAGE_GAP);
        let want = target + dir as f32 * span;
        // (At least the next page: large ones are a strip wide each.)
        let to = if dir > 0 {
            let ahead = || self.starts.iter().find(|&&s| s > target + 0.5).copied();
            self.starts.iter().rev().find(|&&s| s <= want && s > target + 0.5).copied().or_else(ahead).unwrap_or(want)
        } else {
            let behind = || self.starts.iter().rev().find(|&&s| s < target - 0.5).copied();
            self.starts.iter().find(|&&s| s >= want && s < target - 0.5).copied().or_else(behind).unwrap_or(want)
        };
        to.clamp(0.0, self.max_scroll)
    }
}

/// A small card's colour, and a card of pages'.
fn small_fill(p: &Palette) -> Color32 {
    theme::blend(p.bg, p.secondary, if p.dark { 0.42 } else { 0.16 })
}

fn pages_fill(p: &Palette) -> Color32 {
    theme::blend(p.bg, Color32::BLACK, if p.dark { 0.45 } else { 0.06 })
}

/// Draws a ⌄.
fn chevron_down(painter: &egui::Painter, c: Pos2, s: f32, color: Color32) {
    let pts = vec![c + Vec2::new(-s, -s * 0.45), c + Vec2::new(0.0, s * 0.45), c + Vec2::new(s, -s * 0.45)];
    painter.add(egui::Shape::line(pts, Stroke::new(1.8_f32, color)));
}

/// A page, or white while it's drawn.
fn paint_page(painter: &egui::Painter, pg: &LaidPage, shadow: bool) {
    let r = pg.rect;
    if shadow {
        painter.rect_filled(r.translate(Vec2::new(0.0, 1.0)).expand(1.0), 2.0, Color32::from_black_alpha(40));
    }
    match pg.tex {
        Some(tex) => {
            painter.image(tex, r, Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)), Color32::WHITE);
        }
        None => {
            painter.rect_filled(r, 0.0, Color32::WHITE);
        }
    }
    painter.rect_stroke(r, 0.0, Stroke::new(0.5_f32, Color32::from_black_alpha(50)));
}

/// A document with its corner turned down and its extension on it, for a
/// file without a picture of its own.
fn paint_document(painter: &egui::Painter, r: Rect, ext: &str, p: &Palette) {
    let fold = 10.0;
    let paper = theme::blend(p.bg, Color32::WHITE, if p.dark { 0.85 } else { 1.0 });
    let edge = Stroke::new(1.0_f32, theme::blend(paper, Color32::BLACK, 0.25));
    let pts = vec![r.left_top(), Pos2::new(r.right() - fold, r.top()), Pos2::new(r.right(), r.top() + fold), r.right_bottom(), r.left_bottom()];
    painter.add(egui::Shape::convex_polygon(pts.clone(), paper, edge));
    let corner = vec![Pos2::new(r.right() - fold, r.top()), Pos2::new(r.right() - fold, r.top() + fold), Pos2::new(r.right(), r.top() + fold)];
    painter.add(egui::Shape::line(corner, edge));
    if !ext.is_empty() {
        let label: String = ext.to_ascii_uppercase().chars().take(4).collect();
        painter.text(Pos2::new(r.center().x, r.bottom() - 12.0), Align2::CENTER_CENTER, label, theme::bold_font(9.0), theme::blend(paper, Color32::BLACK, 0.6));
    }
}

/// Draws a card (but not its name: the galley is drawn with the text);
/// `hover` where the pointer is, over the card.
pub fn paint(painter: &egui::Painter, f: &LaidFile, p: &Palette, hover: Option<Pos2>) {
    let over = |r: Rect| hover.is_some_and(|pt| r.contains(pt));
    if f.view == FileView::Small {
        let fill = small_fill(p);
        painter.rect_filled(f.card, 16.0, fill);
        // "PDF Document · 211 KB" below the name.
        painter.text(f.details_pos, Align2::LEFT_TOP, &f.details, FontId::proportional(13.0), p.secondary);
        let button = theme::blend(fill, p.text, if over(f.menu) { 0.22 } else { 0.12 });
        painter.circle_filled(f.menu.center(), MENU_R, button);
        chevron_down(painter, f.menu.center() + Vec2::new(0.0, 1.0), 4.5, p.secondary);
        match f.pages.first() {
            Some(pg) => paint_page(painter, pg, false),
            None => paint_document(painter, f.strip, &f.ext, p),
        }
        return;
    }
    painter.rect_filled(f.card, 12.0, pages_fill(p));
    chevron_down(painter, f.menu.center() + Vec2::new(0.0, 1.0), 5.5, if over(f.menu) { p.text } else { p.secondary });
    if let Some(t) = f.toggle {
        let text = if f.thumbs_shown { "Hide Thumbnails" } else { "Show Thumbnails" };
        painter.text(t.right_center(), Align2::RIGHT_CENTER, text, FontId::proportional(15.0), if over(t) { p.text } else { p.secondary });
    }
    let clip = f.strip.intersect(f.card).intersect(painter.clip_rect());
    let strip = painter.with_clip_rect(clip);
    for pg in &f.pages {
        paint_page(&strip, pg, true);
    }
    if f.thumbs_shown {
        let row = painter.with_clip_rect(Rect::from_x_y_ranges(f.strip.left()..=f.card.right() - PAD, f.card.y_range()).intersect(painter.clip_rect()));
        for t in &f.thumbs {
            paint_page(&row, t, false);
            if t.index == f.current {
                row.rect_stroke(t.rect.expand(2.0), 3.0, Stroke::new(2.0_f32, theme::BLUE));
            }
        }
    }
    if hover.is_some_and(|pt| f.card.contains(pt)) {
        let edge = theme::blend(p.bg, p.tertiary, 0.4);
        for dir in [-1, 1] {
            let Some(r) = f.arrow(dir) else { continue };
            painter.circle(r.center(), ARROW_R, p.bg.gamma_multiply(0.92), Stroke::new(1.0_f32, edge));
            let (c, s) = (r.center(), ARROW_R * 0.32);
            let d = dir as f32;
            let pts = vec![c + Vec2::new(-d * s * 0.5, -s), c + Vec2::new(d * s * 0.5, 0.0), c + Vec2::new(-d * s * 0.5, s)];
            painter.add(egui::Shape::line(pts, Stroke::new(1.8_f32, p.text)));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kinds_sizes_and_names() {
        assert_eq!(size_text(211_000), "211 KB");
        assert_eq!(size_text(1_400_000), "1.4 MB");
        assert_eq!(size_text(900), "900 bytes");
        assert_eq!(kind_name("pdf"), "PDF Document");
        assert_eq!(kind_name("docx"), "Word Document");
        assert_eq!(kind_name("heic"), "HEIC Document");
        let name = format!("{}.pdf", "a".repeat(64));
        let sealed = format!("{}.enc#{}", "b".repeat(64), "c".repeat(64));
        assert!(is_pdf_name(&name, "x"));
        assert!(is_pdf_name(&sealed, "Trip.PDF") && !is_pdf_name(&sealed, "trip.docx"));
        assert_eq!((extension(&sealed, "Notes.DOCX"), extension(&name, "no extension"), extension(&sealed, "x")), ("docx".into(), "pdf".into(), "".into()));
    }

    #[test]
    fn pages_line_up_and_scroll() {
        let sizes = vec![Vec2::new(612.0, 792.0); 6];
        let f = LaidFile::pages("r".into(), "a.pdf".into(), String::new(), FileView::Medium, Pos2::ZERO, 600.0, Some(&sizes), Vec2::new(60.0, 22.0), 22.0, 0.0, false);
        let pw = 612.0 / 792.0 * MEDIUM_PAGE_H;
        assert!((f.pages[0].rect.width() - pw).abs() < 0.01 && f.pages[0].rect.height() == MEDIUM_PAGE_H);
        assert!(f.text_origin.y < f.strip.top() && f.menu.left() > f.text_origin.x + 60.0);
        assert!(f.arrow(-1).is_none() && f.arrow(1).is_some());
        let to = f.step(0.0, 1);
        assert!(to > 0.0 && f.starts.contains(&to), "{to}");
        assert_eq!(f.scroll_to(1), pw + PAGE_GAP);
        // Large: as wide as the card's inside, one at a time.
        let f = LaidFile::pages("r".into(), "a.pdf".into(), String::new(), FileView::Large, Pos2::ZERO, 600.0, Some(&sizes), Vec2::new(60.0, 22.0), 22.0, 0.0, true);
        assert!((f.pages[0].rect.width() - (600.0 - 2.0 * (PAD + 2.0))).abs() < 0.01);
        assert!(f.thumbs.len() >= 2 && f.card.bottom() > f.thumbs[0].rect.bottom());
        assert_eq!(f.step(0.0, 1), f.starts[1]);
    }
}
