//! Pictures in notes, as textures: each attachment is decoded once, on a
//! thread of its own (a large photo takes a moment), and kept for as long as
//! the app runs. An attached PDF's first page is drawn the same way, as its
//! preview.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use eframe::egui::{self, ColorImage, TextureHandle, TextureId, TextureOptions, Vec2};

use cleannotes_core::attachments;

/// Pictures are kept at most this many pixels wide or high (a phone photo
/// is shown well under that, and textures that big may not be allowed).
const MAX_SIDE: u32 = 2048;

/// What can be shown of an attachment now.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Shown {
    /// The texture, and the picture's own size in pixels.
    Ready(TextureId, Vec2),
    Loading,
    /// Not on this device (yet: a sync may bring it).
    Missing,
    /// There, but not a picture this app can show (HEIC, damaged).
    Unreadable,
}

enum Slot {
    Loading,
    Decoded(ColorImage, Vec2),
    Ready(TextureHandle, Vec2),
    Missing,
    Unreadable,
}

/// The pictures of the attachments folder.
pub struct ImageCache {
    dir: PathBuf,
    slots: Arc<Mutex<HashMap<String, Slot>>>,
}

impl ImageCache {
    pub fn new(dir: PathBuf) -> ImageCache {
        ImageCache { dir, slots: Arc::new(Mutex::new(HashMap::new())) }
    }

    /// The attachments folder.
    pub fn dir(&self) -> &std::path::Path {
        &self.dir
    }

    /// The attachment `name`, starting to load it if it isn't yet.
    pub fn get(&self, ctx: &egui::Context, name: &str) -> Shown {
        let mut slots = self.slots.lock().unwrap_or_else(|e| e.into_inner());
        match slots.remove(name) {
            None => {
                if !attachments::ref_exists(&self.dir, name) {
                    slots.insert(name.to_owned(), Slot::Missing);
                    return Shown::Missing;
                }
                slots.insert(name.to_owned(), Slot::Loading);
                let (dir, key, shared, ctx) = (self.dir.clone(), name.to_owned(), self.slots.clone(), ctx.clone());
                std::thread::spawn(move || {
                    // A locked note's picture is decrypted here, in memory.
                    // (A decoder that panics on a damaged file, from another
                    // device, makes it unreadable, not loading forever.)
                    let loaded = std::panic::catch_unwind(|| attachments::load(&dir, &key).and_then(|b| decode(&b)));
                    let slot = match loaded {
                        Ok(Some((img, size))) => Slot::Decoded(img, size),
                        _ => Slot::Unreadable,
                    };
                    shared.lock().unwrap_or_else(|e| e.into_inner()).insert(key, slot);
                    ctx.request_repaint();
                });
                Shown::Loading
            }
            Some(Slot::Decoded(img, size)) => {
                let tex = ctx.load_texture(format!("attachment:{name}"), img, TextureOptions::LINEAR);
                let id = tex.id();
                slots.insert(name.to_owned(), Slot::Ready(tex, size));
                Shown::Ready(id, size)
            }
            Some(slot) => {
                let shown = match &slot {
                    Slot::Ready(t, size) => Shown::Ready(t.id(), *size),
                    Slot::Loading | Slot::Decoded(..) => Shown::Loading,
                    Slot::Missing => Shown::Missing,
                    Slot::Unreadable => Shown::Unreadable,
                };
                slots.insert(name.to_owned(), slot);
                shown
            }
        }
    }

    /// Pictures that weren't here may have arrived (a sync brought them):
    /// they're looked for again.
    pub fn retry_missing(&self) {
        self.slots.lock().unwrap_or_else(|e| e.into_inner()).retain(|_, s| !matches!(s, Slot::Missing));
    }

    /// Locked notes closed: their pictures, decrypted, aren't kept.
    pub fn forget_sealed(&self) {
        self.slots.lock().unwrap_or_else(|e| e.into_inner()).retain(|name, _| !attachments::is_sealed_ref(name));
    }
}

/// A picture's pixels (upright, as a camera's EXIF says, and made smaller
/// if huge) and its own size; for a PDF, its first page's, and the page's
/// size in points.
fn decode(bytes: &[u8]) -> Option<(ColorImage, Vec2)> {
    if is_pdf(bytes) {
        return pdf_first_page(bytes);
    }
    use image::ImageDecoder;
    let mut decoder = image::ImageReader::new(std::io::Cursor::new(bytes)).with_guessed_format().ok()?.into_decoder().ok()?;
    let orientation = decoder.orientation().unwrap_or(image::metadata::Orientation::NoTransforms);
    let mut img = image::DynamicImage::from_decoder(decoder).ok()?;
    img.apply_orientation(orientation);
    let size = Vec2::new(img.width() as f32, img.height() as f32);
    let img = if img.width() > MAX_SIDE || img.height() > MAX_SIDE { img.thumbnail(MAX_SIDE, MAX_SIDE) } else { img };
    let rgba = img.to_rgba8();
    Some((ColorImage::from_rgba_unmultiplied([rgba.width() as usize, rgba.height() as usize], rgba.as_raw()), size))
}

/// Whether `bytes` are a PDF's (its header may come after some junk, as
/// readers allow).
pub fn is_pdf(bytes: &[u8]) -> bool {
    bytes[..bytes.len().min(1024)].windows(5).any(|w| w == b"%PDF-")
}

/// A PDF's preview is drawn this many pixels wide (sharp at 3x its usual
/// width on screen).
const PDF_PIXELS: f32 = 960.0;

/// The first page of a PDF, on white, and its size in points. `None` if it
/// has no pages or can't be read (an encrypted one, say).
fn pdf_first_page(bytes: &[u8]) -> Option<(ColorImage, Vec2)> {
    use hayro::hayro_interpret::InterpreterSettings;
    use hayro::hayro_syntax::Pdf;
    use hayro::vello_cpu::color::palette::css::WHITE;
    let pdf = Pdf::new(bytes.to_vec()).ok()?;
    let pages = pdf.pages();
    let page = pages.first()?;
    let (w, h) = page.render_dimensions();
    if !(w > 0.0 && h > 0.0) {
        return None;
    }
    let scale = (PDF_PIXELS / w).min(MAX_SIDE as f32 / h).min(8.0);
    let settings = hayro::PixmapSettings { x_scale: scale, y_scale: scale, bg_color: WHITE };
    let pixmap = hayro::render(page, &hayro::RenderCache::new(), &InterpreterSettings::default(), &hayro::RenderSettings::default(), &settings);
    let size = [pixmap.width() as usize, pixmap.height() as usize];
    if size[0] == 0 || size[1] == 0 {
        return None;
    }
    // (Over a white page every pixel is opaque: premultiplied or not is the
    // same.)
    Some((ColorImage::from_rgba_unmultiplied(size, pixmap.data_as_u8_slice()), Vec2::new(w, h)))
}

/// How wide a PDF's preview is at most, in points.
pub const PREVIEW_W: f32 = 300.0;

/// How big to show a PDF's preview in a column `width` points wide: its
/// first page, at most [`PREVIEW_W`] wide, as high as the page's shape
/// makes it (a tall one kept to a height).
pub fn preview_size(shown: Shown, width: f32) -> Vec2 {
    const MAX_H: f32 = 640.0;
    let page = match shown {
        Shown::Ready(_, size) if size.x > 0.0 && size.y > 0.0 => size,
        // While it's drawn: an A4 page's shape.
        _ => Vec2::new(595.0, 842.0),
    };
    let s = page * (width.min(PREVIEW_W) / page.x);
    if s.y > MAX_H {
        (s * (MAX_H / s.y)).max(Vec2::splat(4.0))
    } else {
        s
    }
}

/// How big to show a picture of `size` pixels in a column `width` points
/// wide: its own size (in points), made smaller to fit the width and a
/// reasonable height.
pub fn display_size(shown: Shown, width: f32, pixels_per_point: f32) -> Vec2 {
    const MAX_H: f32 = 640.0;
    match shown {
        Shown::Ready(_, size) if size.x > 0.0 && size.y > 0.0 => {
            let mut s = size / pixels_per_point.max(0.5);
            if s.x > width {
                s *= width / s.x;
            }
            if s.y > MAX_H {
                s *= MAX_H / s.y;
            }
            s.max(Vec2::splat(4.0))
        }
        // A box to show it's on its way, or can't be shown.
        _ => Vec2::new(width.min(260.0), 150.0),
    }
}

/// An image file's bytes encoded as PNG (for a pasted picture).
pub fn png_from_rgba(width: usize, height: usize, rgba: &[u8]) -> Option<Vec<u8>> {
    let img = image::RgbaImage::from_raw(width as u32, height as u32, rgba.to_vec())?;
    let mut out = std::io::Cursor::new(Vec::new());
    img.write_to(&mut out, image::ImageFormat::Png).ok()?;
    Some(out.into_inner())
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;

    #[test]
    fn pictures_fit_the_column() {
        let tex = TextureId::default();
        let near = |a: Vec2, b: Vec2| assert!((a - b).length() < 0.01, "{a:?} != {b:?}");
        near(display_size(Shown::Ready(tex, Vec2::new(4000.0, 3000.0)), 600.0, 2.0), Vec2::new(600.0, 450.0));
        near(display_size(Shown::Ready(tex, Vec2::new(200.0, 100.0)), 600.0, 2.0), Vec2::new(100.0, 50.0));
        // Tall screenshots are kept to a height.
        near(display_size(Shown::Ready(tex, Vec2::new(1000.0, 8000.0)), 600.0, 1.0), Vec2::new(80.0, 640.0));
        near(display_size(Shown::Missing, 600.0, 1.0), Vec2::new(260.0, 150.0));
    }

    #[test]
    fn previews_fit_the_column() {
        let tex = TextureId::default();
        let near = |a: Vec2, b: Vec2| assert!((a - b).length() < 0.01, "{a:?} != {b:?}");
        // A Letter page, at the preview's width or the column's.
        near(preview_size(Shown::Ready(tex, Vec2::new(612.0, 792.0)), 600.0), Vec2::new(300.0, 300.0 * 792.0 / 612.0));
        near(preview_size(Shown::Ready(tex, Vec2::new(612.0, 792.0)), 153.0), Vec2::new(153.0, 198.0));
        // A landscape slide, and a long receipt kept to a height.
        near(preview_size(Shown::Ready(tex, Vec2::new(960.0, 540.0)), 600.0), Vec2::new(300.0, 168.75));
        near(preview_size(Shown::Ready(tex, Vec2::new(100.0, 1000.0)), 600.0), Vec2::new(64.0, 640.0));
        // On its way: an A4-shaped box.
        near(preview_size(Shown::Loading, 600.0), Vec2::new(300.0, 300.0 * 842.0 / 595.0));
    }

    #[test]
    fn a_pdfs_first_page_is_its_preview() {
        let pdf = test_pdf(200, 100);
        assert!(is_pdf(&pdf));
        let (img, size) = decode(&pdf).expect("the page is drawn");
        assert_eq!(size, Vec2::new(200.0, 100.0));
        assert_eq!(img.size, [960, 480]);
        // White, with the black square drawn in its middle.
        let px = |x: usize, y: usize| img.pixels[y * img.size[0] + x];
        assert_eq!(px(5, 5), egui::Color32::WHITE);
        assert_eq!(px(480, 240), egui::Color32::BLACK);
        // Not a PDF at all, or one cut short: no preview.
        assert!(!is_pdf(b"PK\x03\x04 a zip"));
        assert!(decode(b"%PDF-1.4 cut short").is_none());
    }

    /// A one-page PDF `w` by `h` points, with a black square in the middle.
    pub fn test_pdf(w: u32, h: u32) -> Vec<u8> {
        let content = format!("0 g {} {} 40 40 re f", w / 2 - 20, h / 2 - 20);
        let objects = [
            "<< /Type /Catalog /Pages 2 0 R >>".to_owned(),
            "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_owned(),
            format!("<< /Type /Page /Parent 2 0 R /MediaBox [0 0 {w} {h}] /Contents 4 0 R >>"),
            format!("<< /Length {} >>\nstream\n{content}\nendstream", content.len()),
        ];
        let mut out = b"%PDF-1.4\n".to_vec();
        let mut offsets = Vec::new();
        for (i, o) in objects.iter().enumerate() {
            offsets.push(out.len());
            out.extend(format!("{} 0 obj\n{o}\nendobj\n", i + 1).bytes());
        }
        let xref = out.len();
        out.extend(format!("xref\n0 {}\n0000000000 65535 f \n", objects.len() + 1).bytes());
        for o in offsets {
            out.extend(format!("{o:010} 00000 n \n").bytes());
        }
        out.extend(format!("trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n", objects.len() + 1).bytes());
        out
    }

    #[test]
    fn pasted_pixels_become_a_png_that_decodes() {
        let png = png_from_rgba(2, 1, &[255, 0, 0, 255, 0, 255, 0, 255]).unwrap();
        assert_eq!(attachments::image_ext(&png), Some("png"));
        let (img, size) = decode(&png).unwrap();
        assert_eq!((img.size, size), ([2, 1], Vec2::new(2.0, 1.0)));
    }
}
