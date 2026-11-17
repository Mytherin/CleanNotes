//! Laying out and painting a note: one egui galley per paragraph, indented
//! for lists and quotes, with the list markers, checkboxes and quote bars
//! drawn in the margin.

use std::ops::Range;
use std::sync::Arc;

use eframe::egui::{self, text::LayoutJob, Align2, Color32, FontId, Galley, Pos2, Rect, Stroke, TextFormat, Vec2};

use crate::doc::{Block, Kind};
pub use crate::doc::{auto_links, font_size as size, tag_ranges};
use crate::theme::{self, Palette};

/// Width of a list marker's column, and of one indent level.
pub const MARKER_W: f32 = 24.0;
pub const INDENT_W: f32 = 26.0;
/// Space between a quote's bar and its text.
pub const QUOTE_W: f32 = 16.0;
/// Space between a table cell's border and its text.
pub const CELL_PAD: Vec2 = Vec2::new(7.0, 5.0);

/// A table as laid out: for drawing its grid and header cells.
pub struct LaidTable {
    pub rect: Rect,
    /// Where each column and row starts, and the right and bottom edges.
    pub xs: Vec<f32>,
    pub ys: Vec<f32>,
    pub header_row: bool,
    pub header_col: bool,
}

/// Draws a table's header cells and its grid (thin lines, as in Apple
/// Notes).
pub fn paint_table(painter: &egui::Painter, t: &LaidTable, p: &Palette) {
    let fill = theme::blend(p.bg, p.tertiary, 0.12);
    if t.header_row && t.ys.len() > 1 {
        painter.rect_filled(Rect::from_x_y_ranges(t.rect.x_range(), t.ys[0]..=t.ys[1]), 0.0, fill);
    }
    if t.header_col && t.xs.len() > 1 {
        let top = if t.header_row && t.ys.len() > 1 { t.ys[1] } else { t.ys[0] };
        painter.rect_filled(Rect::from_x_y_ranges(t.xs[0]..=t.xs[1], top..=t.rect.bottom()), 0.0, fill);
    }
    let line = Stroke::new(1.0_f32, theme::blend(p.bg, p.tertiary, 0.55));
    for &x in &t.xs {
        painter.line_segment([Pos2::new(x, t.rect.top()), Pos2::new(x, t.rect.bottom())], line);
    }
    for &y in &t.ys {
        painter.line_segment([Pos2::new(t.rect.left(), y), Pos2::new(t.rect.right(), y)], line);
    }
}

/// A paragraph as laid out last frame.
pub struct Laid {
    /// The whole paragraph, margin included (screen coordinates).
    pub rect: Rect,
    /// Where the galley's top-left goes.
    pub origin: Pos2,
    pub galley: Arc<Galley>,
    /// The checkbox, for clicks.
    pub checkbox: Option<Rect>,
    /// A paragraph of pictures: each one's place, one below the other
    /// (character `i` is picture `i`). Its galley is then empty: the caret,
    /// clicks and selection go by these.
    pub images: Vec<LaidImage>,
    /// A paragraph that's a link to an attached PDF: shown as a card, its
    /// pages side by side above the link's text (the galley, as usual).
    pub pdf: Option<LaidPdf>,
}

/// The widest a PDF's card gets.
pub const CARD_MAX_W: f32 = 620.0;
/// Space around the card, inside it, and between its pages.
pub const CARD_MARGIN: f32 = 4.0;
pub const CARD_PAD: f32 = 10.0;
pub const PAGE_GAP: f32 = 10.0;
/// How high pages are shown.
pub const PAGE_H: f32 = 220.0;
/// Room kept right of the file's name for the number of pages.
pub const COUNT_W: f32 = 84.0;
/// The ‹ › buttons' size, and how far they're kept from the strip's edge.
const ARROW_R: f32 = 14.0;
const ARROW_INSET: f32 = 8.0;

/// An attached PDF as laid out: a card with its pages in a strip that
/// scrolls sideways, and the file's name below them.
pub struct LaidPdf {
    /// The attachment's reference, and the link's text (the file's name).
    pub reference: String,
    pub label: String,
    pub card: Rect,
    /// Where the pages show (they're cut off at its edges).
    pub strip: Rect,
    /// The pages in or near view, where they are now (scrolled).
    pub pages: Vec<LaidPage>,
    /// Its number of pages (0 while it's read).
    pub count: usize,
    /// How far the strip is scrolled, and can be.
    pub scroll: f32,
    pub max_scroll: f32,
    /// Where the galley (the file's name) goes.
    pub text_origin: Pos2,
}

/// A page as laid out; `tex` once it's drawn.
pub struct LaidPage {
    pub index: usize,
    pub rect: Rect,
    pub tex: Option<egui::TextureId>,
}

/// How wide a page of `size` is shown, [`PAGE_H`] high (very wide or
/// narrow pages kept to a reasonable shape).
fn page_w(size: Vec2) -> f32 {
    (size.x / size.y * PAGE_H).clamp(PAGE_H * 0.25, PAGE_H * 2.2)
}

impl LaidPdf {
    /// The card at `pos`, `width` wide, for a PDF with pages of `sizes`
    /// (`None` while it's read: one A4 page's place), its name's galley
    /// `text_h` high, scrolled by `scroll` (kept within reach).
    pub fn new(reference: String, label: String, pos: Pos2, width: f32, sizes: Option<&[Vec2]>, text_h: f32, scroll: f32) -> LaidPdf {
        let a4 = [Vec2::new(595.0, 842.0)];
        let count = sizes.map_or(0, <[Vec2]>::len);
        let sizes = sizes.unwrap_or(&a4);
        let strip = Rect::from_min_size(pos + Vec2::splat(CARD_PAD), Vec2::new(width - 2.0 * CARD_PAD, PAGE_H));
        let total: f32 = sizes.iter().map(|s| page_w(*s)).sum::<f32>() + PAGE_GAP * (sizes.len() - 1) as f32;
        let max_scroll = (total - strip.width()).max(0.0);
        let scroll = scroll.clamp(0.0, max_scroll);
        let mut pages = Vec::new();
        let mut x = strip.left() - scroll;
        for (index, s) in sizes.iter().enumerate() {
            let rect = Rect::from_min_size(Pos2::new(x, strip.top()), Vec2::new(page_w(*s), PAGE_H));
            // Those in view, and a strip's width either side (drawn ahead).
            if rect.right() > strip.left() - strip.width() && rect.left() < strip.right() + strip.width() {
                pages.push(LaidPage { index, rect, tex: None });
            }
            x += rect.width() + PAGE_GAP;
        }
        let text_origin = Pos2::new(strip.left(), strip.bottom() + CARD_PAD);
        let card = Rect::from_min_size(pos, Vec2::new(width, PAGE_H + 3.0 * CARD_PAD + text_h));
        LaidPdf { reference, label, card, strip, pages, count, scroll, max_scroll, text_origin }
    }

    /// The ‹ (`-1`) or › (`1`) button, if it can scroll that way.
    pub fn arrow(&self, dir: i32) -> Option<Rect> {
        let can = if dir < 0 { self.scroll > 0.5 } else { self.scroll < self.max_scroll - 0.5 };
        let x = if dir < 0 { self.strip.left() + ARROW_INSET + ARROW_R } else { self.strip.right() - ARROW_INSET - ARROW_R };
        can.then(|| Rect::from_center_size(Pos2::new(x, self.strip.center().y), Vec2::splat(2.0 * ARROW_R)))
    }

    /// The button under a point: its direction.
    pub fn arrow_at(&self, pt: Pos2) -> Option<i32> {
        [-1, 1].into_iter().find(|d| self.arrow(*d).is_some_and(|r| r.expand(2.0).contains(pt)))
    }

    /// Where a ‹ or › press scrolls to from `target`: the page that starts
    /// a strip's width (less a little) that way, lined up at the left edge.
    pub fn step(&self, target: f32, dir: i32, sizes: &[Vec2]) -> f32 {
        let mut starts = Vec::with_capacity(sizes.len());
        let mut x = 0.0;
        for s in sizes {
            starts.push(x);
            x += page_w(*s) + PAGE_GAP;
        }
        let want = target + dir as f32 * (self.strip.width() - PAGE_GAP * 4.0).max(PAGE_GAP);
        let to = if dir > 0 {
            starts.iter().rev().find(|&&s| s <= want && s > target + 0.5).copied().unwrap_or(want)
        } else {
            starts.iter().find(|&&s| s >= want && s < target - 0.5).copied().unwrap_or(want)
        };
        to.clamp(0.0, self.max_scroll)
    }
}

/// Draws a PDF's card (but not its name: the galley is drawn with the
/// text): its pages, the number of them, and ‹ › while `hovered`.
pub fn paint_pdf(painter: &egui::Painter, pdf: &LaidPdf, p: &Palette, hovered: bool) {
    let edge = theme::blend(p.bg, p.tertiary, 0.4);
    painter.rect(pdf.card, 10.0, p.code_bg, Stroke::new(1.0_f32, edge));
    let strip = painter.with_clip_rect(pdf.strip.intersect(painter.clip_rect()));
    for pg in &pdf.pages {
        let r = pg.rect;
        strip.rect_filled(r.translate(Vec2::new(0.0, 1.0)).expand(1.0), 2.0, Color32::from_black_alpha(30));
        match pg.tex {
            Some(tex) => {
                strip.image(tex, r, Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)), Color32::WHITE);
            }
            None => {
                strip.rect_filled(r, 0.0, Color32::WHITE);
            }
        }
        strip.rect_stroke(r, 0.0, Stroke::new(0.5_f32, Color32::from_black_alpha(40)));
    }
    let line_y = pdf.strip.bottom() + CARD_PAD / 2.0;
    painter.hline(pdf.card.x_range().shrink(1.0), line_y, Stroke::new(1.0_f32, edge));
    let count = match pdf.count {
        0 => "PDF".to_owned(),
        1 => "PDF · 1 page".to_owned(),
        n => format!("PDF · {n} pages"),
    };
    let at = Pos2::new(pdf.strip.right(), pdf.text_origin.y + 1.0);
    painter.text(at, Align2::RIGHT_TOP, count, FontId::proportional(12.0), p.secondary);
    if hovered {
        for dir in [-1, 1] {
            let Some(r) = pdf.arrow(dir) else { continue };
            painter.circle(r.center(), ARROW_R, p.bg.gamma_multiply(0.92), Stroke::new(1.0_f32, edge));
            let (c, s) = (r.center(), ARROW_R * 0.32);
            let d = dir as f32;
            let pts = vec![c + Vec2::new(-d * s * 0.5, -s), c + Vec2::new(d * s * 0.5, 0.0), c + Vec2::new(-d * s * 0.5, s)];
            painter.add(egui::Shape::line(pts, Stroke::new(1.8_f32, p.text)));
        }
    }
}

/// If a paragraph is only a link to an attached PDF (as attaching one
/// makes it), the attachment's reference and the link's text. (A PDF of a
/// locked note is encrypted, its reference ending in `.enc`: its name
/// tells.)
pub fn pdf_link(b: &Block) -> Option<(String, String)> {
    if matches!(b.kind, Kind::Monospaced) || b.kind.is_cell() {
        return None;
    }
    let url = b.styles.first()?.link.clone()?;
    if b.styles.iter().any(|s| s.link.as_deref() != Some(&*url) || s.image.is_some()) {
        return None;
    }
    let reference = cleannotes_core::attachments::file_link_ref(&url)?;
    let (name, _) = cleannotes_core::attachments::split_ref(reference);
    let label = b.text.trim();
    let pdf = |s: &str| s.len() > 4 && s[s.len() - 4..].eq_ignore_ascii_case(".pdf");
    let sealed = name.ends_with(&format!(".{}", cleannotes_core::attachments::SEALED_EXT));
    (pdf(name) || (sealed && pdf(label))).then(|| (reference.to_owned(), label.to_owned()))
}

/// A picture as laid out.
pub struct LaidImage {
    pub name: String,
    pub shown: super::images::Shown,
    pub rect: Rect,
}

/// Space between pictures in one paragraph.
pub const IMAGE_GAP: f32 = 6.0;

/// Whether a paragraph is only pictures (as both apps make them): it's laid
/// out as pictures. (A picture inside text shows as a placeholder.)
pub fn is_image_block(b: &Block) -> bool {
    !b.styles.is_empty() && b.styles.iter().all(|s| s.image.is_some())
}

/// Draws a picture, or a box saying why it isn't there.
pub fn paint_image(painter: &egui::Painter, img: &LaidImage, p: &Palette) {
    use super::images::Shown;
    let r = img.rect;
    match img.shown {
        Shown::Ready(tex, _) => {
            painter.image(tex, r, Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)), Color32::WHITE);
        }
        other => {
            painter.rect(r, 8.0, p.code_bg, Stroke::new(1.0_f32, theme::blend(p.bg, p.tertiary, 0.4)));
            let label = match other {
                Shown::Loading => "Loading…",
                Shown::Missing => "Waiting for this picture to sync",
                _ => "This picture can't be shown here",
            };
            painter.text(r.center(), Align2::CENTER_CENTER, label, FontId::proportional(12.5), p.secondary);
        }
    }
}


/// Space above a paragraph.
pub fn space_before(kind: Kind, prev: Option<Kind>, body: f32) -> f32 {
    let s = match kind {
        Kind::Title => 0.3,
        Kind::Heading => 0.45,
        Kind::Subheading => 0.3,
        _ => 0.0,
    } * body;
    match prev {
        None => 0.0,
        // Code blocks and quotes sit apart a little.
        Some(p) if (p == Kind::Monospaced) != (kind == Kind::Monospaced) => s + 0.25 * body,
        _ => s,
    }
}

/// How far a paragraph's text starts from the left edge.
pub fn text_indent(b: &Block) -> f32 {
    let level = b.indent as f32 * INDENT_W;
    match b.kind {
        k if k.is_list() => level + MARKER_W,
        Kind::Quote => level + QUOTE_W,
        _ => level,
    }
}

/// What to show on the characters of a paragraph besides their style.
#[derive(Default)]
pub struct Marks {
    /// Search or find matches (char ranges); `current` is drawn stronger.
    pub highlights: Vec<Range<usize>>,
    pub current: Option<Range<usize>>,
}


/// The galley text and formats for a paragraph; `strong` makes it all bold
/// (a table's header cells).
pub fn job(b: &Block, body: f32, p: &Palette, wrap: f32, marks: &Marks, strong: bool) -> LayoutJob {
    let size = size(b.kind, body);
    let mono_block = b.kind == Kind::Monospaced;
    let heading = strong || matches!(b.kind, Kind::Title | Kind::Heading | Kind::Subheading);
    let tags = if mono_block { vec![] } else { tag_ranges(&b.text) };
    let autos = if mono_block { vec![] } else { auto_links(&b.text) };
    let fake_italic = !theme::has_italic();
    let mut job = LayoutJob { wrap: egui::text::TextWrapping { max_width: wrap, ..Default::default() }, ..Default::default() };
    job.halign = egui::Align::LEFT;
    let in_any = |rs: &[Range<usize>], i: usize| rs.iter().any(|r| r.contains(&i));
    let format_at = |i: usize| -> TextFormat {
        let s = &b.styles[i];
        if s.image.is_some() {
            // A picture inside text (from Markdown written elsewhere): a
            // placeholder mark.
            return TextFormat { font_id: FontId::new(size, theme::family(false, false, false)), color: p.secondary, ..Default::default() };
        }
        let mono = mono_block || s.code;
        let bold = s.bold || heading;
        let link = s.link.is_some() || autos.iter().any(|(r, _)| r.contains(&i));
        let color = if link {
            theme::BLUE
        } else if in_any(&tags, i) {
            p.accent
        } else {
            p.text
        };
        let background = if marks.current.as_ref().is_some_and(|r| r.contains(&i)) {
            p.highlight
        } else if in_any(&marks.highlights, i) {
            theme::blend(p.bg, p.highlight, 0.45)
        } else if s.code && !mono_block {
            p.code_bg
        } else {
            Color32::TRANSPARENT
        };
        let line = |on: bool| if on { Stroke::new((size / 14.0).max(1.0), color) } else { Stroke::NONE };
        TextFormat {
            font_id: FontId::new(if s.code && !mono_block { size * 0.92 } else { size }, theme::family(bold, s.italic, mono)),
            color,
            italics: s.italic && (fake_italic || mono),
            underline: line(s.underline || (link && s.link.is_some())),
            strikethrough: line(s.strike),
            background,
            ..Default::default()
        }
    };
    if b.text.is_empty() {
        job.append("", 0.0, TextFormat { font_id: FontId::new(size, theme::family(heading, false, mono_block)), ..Default::default() });
        return job;
    }
    let mut start_byte = 0;
    let mut cur = format_at(0);
    for (ci, (bi, _)) in b.text.char_indices().enumerate().skip(1) {
        let f = format_at(ci);
        if f != cur {
            job.append(&b.text[start_byte..bi], 0.0, cur);
            cur = f;
            start_byte = bi;
        }
    }
    job.append(&b.text[start_byte..], 0.0, cur);
    job
}

/// Draws the list marker, checkbox or quote bar of a paragraph. Returns the
/// checkbox's rect.
pub fn paint_margin(
    painter: &egui::Painter,
    b: &Block,
    number: u32,
    laid_rect: Rect,
    origin: Pos2,
    galley: &Galley,
    body: f32,
    p: &Palette,
) -> Option<Rect> {
    let first_row_h = galley.rows.first().map_or(galley.rect.height(), |r| r.rect.height());
    let mid_y = origin.y + first_row_h / 2.0;
    let marker_x = origin.x - MARKER_W;
    let size = size(b.kind, body);
    match b.kind {
        Kind::Bulleted => {
            painter.circle_filled(Pos2::new(marker_x + MARKER_W * 0.42, mid_y), (size * 0.16).max(2.5), p.text);
        }
        Kind::Dashed => {
            let c = Pos2::new(marker_x + MARKER_W * 0.42, mid_y);
            painter.line_segment([c - Vec2::new(size * 0.28, 0.0), c + Vec2::new(size * 0.28, 0.0)], Stroke::new(1.3_f32, p.text));
        }
        Kind::Numbered => {
            painter.text(
                Pos2::new(origin.x - 6.0, mid_y),
                Align2::RIGHT_CENTER,
                format!("{number}."),
                FontId::new(size, egui::FontFamily::Proportional),
                p.text,
            );
        }
        Kind::Checklist(checked) => {
            let r = (size * 0.62).max(7.5);
            let c = Pos2::new(marker_x + MARKER_W * 0.45, mid_y);
            if checked {
                painter.circle_filled(c, r, p.accent);
                let s = r * 0.5;
                let pts = [c + Vec2::new(-s, 0.0), c + Vec2::new(-s * 0.25, s * 0.7), c + Vec2::new(s, -s * 0.65)];
                painter.add(egui::Shape::line(pts.to_vec(), Stroke::new(1.8_f32, Color32::WHITE)));
            } else {
                painter.circle_stroke(c, r - 0.5, Stroke::new(1.2_f32, p.tertiary));
            }
            return Some(Rect::from_center_size(c, Vec2::splat(r * 2.0 + 6.0)));
        }
        Kind::Quote => {
            let x = origin.x - QUOTE_W + 3.0;
            painter.line_segment(
                [Pos2::new(x, laid_rect.top() + 1.0), Pos2::new(x, laid_rect.bottom() - 1.0)],
                Stroke::new(2.5_f32, p.tertiary),
            );
        }
        _ => {}
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_urls_are_found() {
        let links = auto_links("see https://example.com/a?b=1, or www.rust-lang.org. (http://x.y)");
        let urls: Vec<&str> = links.iter().map(|(_, u)| u.as_str()).collect();
        assert_eq!(urls, ["https://example.com/a?b=1", "https://www.rust-lang.org", "http://x.y"]);
        assert_eq!(links[0].0, 4..29);
        assert!(auto_links("nothttps://x").is_empty());
    }
}
