//! Pictures in notes, as textures: each attachment is decoded once, on a
//! thread of its own (a large photo takes a moment), and kept for as long as
//! the app runs.

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
                    let slot = match attachments::load(&dir, &key).and_then(|b| decode(&b)) {
                        Some((img, size)) => Slot::Decoded(img, size),
                        None => Slot::Unreadable,
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
/// if huge) and its own size.
fn decode(bytes: &[u8]) -> Option<(ColorImage, Vec2)> {
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
mod tests {
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
    fn pasted_pixels_become_a_png_that_decodes() {
        let png = png_from_rgba(2, 1, &[255, 0, 0, 255, 0, 255, 0, 255]).unwrap();
        assert_eq!(attachments::image_ext(&png), Some("png"));
        let (img, size) = decode(&png).unwrap();
        assert_eq!((img.size, size), ([2, 1], Vec2::new(2.0, 1.0)));
    }
}
