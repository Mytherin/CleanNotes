//! Laying out and painting a note: one egui galley per paragraph, indented
//! for lists and quotes, with the list markers, checkboxes and quote bars
//! drawn in the margin.

use std::ops::Range;
use std::sync::Arc;

use eframe::egui::{self, text::LayoutJob, Align2, Color32, FontId, Galley, Pos2, Rect, Stroke, TextFormat, Vec2};

use crate::doc::{Block, Kind};
use crate::theme::{self, Palette};

/// Width of a list marker's column, and of one indent level.
pub const MARKER_W: f32 = 24.0;
pub const INDENT_W: f32 = 26.0;
/// Space between a quote's bar and its text.
pub const QUOTE_W: f32 = 16.0;

/// A paragraph as laid out last frame.
pub struct Laid {
    /// The whole paragraph, margin included (screen coordinates).
    pub rect: Rect,
    /// Where the galley's top-left goes.
    pub origin: Pos2,
    pub galley: Arc<Galley>,
    /// The checkbox, for clicks.
    pub checkbox: Option<Rect>,
}

/// Font size of a paragraph style, for a body size.
pub fn size(kind: Kind, body: f32) -> f32 {
    match kind {
        Kind::Title => (body * 1.6).round(),
        Kind::Heading => (body * 1.33).round(),
        Kind::Subheading => (body * 1.13).round(),
        Kind::Monospaced => body * 0.93,
        _ => body,
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

/// Plain URLs in text (Apple Notes makes them links), as char ranges.
pub fn auto_links(text: &str) -> Vec<(Range<usize>, String)> {
    let chars: Vec<char> = text.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        let at_word = i == 0 || !chars[i - 1].is_alphanumeric();
        let rest: String = chars[i..chars.len().min(i + 8)].iter().collect::<String>().to_lowercase();
        let scheme = ["https://", "http://", "www."].into_iter().find(|s| rest.starts_with(s));
        if let (true, Some(s)) = (at_word, scheme) {
            let mut j = i + s.chars().count();
            while j < chars.len() && !chars[j].is_whitespace() && !matches!(chars[j], '<' | '>' | '"') {
                j += 1;
            }
            // Trailing punctuation isn't part of the link.
            while j > i && matches!(chars[j - 1], '.' | ',' | ';' | ':' | '!' | '?' | ')' | '\'') {
                j -= 1;
            }
            if j > i + s.chars().count() {
                let url: String = chars[i..j].iter().collect();
                let url = if s == "www." { format!("https://{url}") } else { url };
                out.push((i..j, url));
            }
            i = j.max(i + 1);
        } else {
            i += 1;
        }
    }
    out
}

/// Hashtag ranges in a paragraph (chars), drawn in the accent color.
fn tag_ranges(text: &str) -> Vec<Range<usize>> {
    let chars: Vec<char> = text.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '#' && (i == 0 || chars[i - 1].is_whitespace() || chars[i - 1] == '(') {
            let mut j = i + 1;
            while j < chars.len() && (chars[j].is_alphanumeric() || matches!(chars[j], '-' | '_' | '/')) {
                j += 1;
            }
            let tag: String = chars[i + 1..j].iter().collect();
            if !tag.is_empty() && tag.chars().any(|c| !c.is_ascii_digit()) {
                out.push(i..j);
            }
            i = j;
        } else {
            i += 1;
        }
    }
    out
}

/// The galley text and formats for a paragraph.
pub fn job(b: &Block, body: f32, p: &Palette, wrap: f32, marks: &Marks) -> LayoutJob {
    let size = size(b.kind, body);
    let mono_block = b.kind == Kind::Monospaced;
    let heading = matches!(b.kind, Kind::Title | Kind::Heading | Kind::Subheading);
    let tags = if mono_block { vec![] } else { tag_ranges(&b.text) };
    let autos = if mono_block { vec![] } else { auto_links(&b.text) };
    let fake_italic = !theme::has_italic();
    let mut job = LayoutJob { wrap: egui::text::TextWrapping { max_width: wrap, ..Default::default() }, ..Default::default() };
    job.halign = egui::Align::LEFT;
    let in_any = |rs: &[Range<usize>], i: usize| rs.iter().any(|r| r.contains(&i));
    let format_at = |i: usize| -> TextFormat {
        let s = &b.styles[i];
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
