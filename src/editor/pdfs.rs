//! Attached PDFs' pages, as textures, for their cards in notes (see
//! [`super::layout::LaidPdf`]). A PDF is read once, on a thread of its own,
//! for its pages' sizes; each page is drawn (by hayro) when it first comes
//! into view, on a thread of its own too. A locked note's PDF is decrypted
//! in memory only, and forgotten when locked notes close.
//!
//! Pages drawn are kept, up to [`MAX_PAGES`] of them: past that, the one
//! looked at longest ago is let go (and drawn again if it's needed again).

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use eframe::egui::{self, ColorImage, TextureHandle, TextureId, TextureOptions, Vec2};

use cleannotes_core::attachments;

/// Pages are drawn at most this many pixels wide.
const MAX_W: f32 = 2048.0;

/// How big a page is drawn: for a small card's picture, a card, or Quick
/// Look (each sharp at about 3x its size on screen).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Res {
    Thumb,
    Card,
    Full,
}

impl Res {
    /// Its height in pixels.
    fn px(self) -> f32 {
        match self {
            Res::Thumb => 200.0,
            Res::Card => 720.0,
            Res::Full => 2000.0,
        }
    }
}

type PageKey = (String, usize, Res);
/// The most pages kept drawn, over all PDFs.
pub const MAX_PAGES: usize = 48;

/// What can be shown of a PDF now.
#[derive(Clone, Debug, PartialEq)]
pub enum Pdf {
    Loading,
    /// Each page's size, in points.
    Ready(Arc<[Vec2]>),
    /// Not on this device (yet), or not a PDF that can be read (an
    /// encrypted one, say): it's shown as a link only.
    Gone,
}

enum DocSlot {
    Loading,
    Ready(Arc<Vec<u8>>, Arc<[Vec2]>),
    Missing,
    Unreadable,
}

enum PageSlot {
    Loading,
    Decoded(ColorImage),
    /// The texture, and when it was last looked at.
    Ready(TextureHandle, u64),
    Failed,
}

type Shared<T> = Arc<Mutex<T>>;

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// The PDFs of the attachments folder.
pub struct PdfCache {
    dir: PathBuf,
    docs: Shared<HashMap<String, DocSlot>>,
    pages: Shared<HashMap<PageKey, PageSlot>>,
    /// Attachments' sizes in bytes (`None`: not on this device).
    file_sizes: Mutex<HashMap<String, Option<u64>>>,
    tick: AtomicU64,
}

impl PdfCache {
    pub fn new(dir: PathBuf) -> PdfCache {
        PdfCache { dir, docs: Default::default(), pages: Default::default(), file_sizes: Default::default(), tick: AtomicU64::new(0) }
    }

    /// The PDF `reference`, starting to read it if it isn't yet.
    pub fn get(&self, ctx: &egui::Context, reference: &str) -> Pdf {
        let mut docs = lock(&self.docs);
        match docs.get(reference) {
            Some(DocSlot::Ready(_, sizes)) => return Pdf::Ready(sizes.clone()),
            Some(DocSlot::Loading) => return Pdf::Loading,
            Some(DocSlot::Missing | DocSlot::Unreadable) => return Pdf::Gone,
            None => {}
        }
        if !attachments::ref_exists(&self.dir, reference) {
            docs.insert(reference.to_owned(), DocSlot::Missing);
            return Pdf::Gone;
        }
        docs.insert(reference.to_owned(), DocSlot::Loading);
        let (dir, key, shared, ctx) = (self.dir.clone(), reference.to_owned(), self.docs.clone(), ctx.clone());
        std::thread::spawn(move || {
            // (A reader that panics on a damaged file makes it unreadable,
            // not loading forever.)
            let read = std::panic::catch_unwind(|| {
                let bytes = Arc::new(attachments::load(&dir, &key)?);
                let sizes = page_sizes(&bytes)?;
                Some((bytes, sizes))
            });
            let slot = match read {
                Ok(Some((bytes, sizes))) => DocSlot::Ready(bytes, sizes.into()),
                _ => DocSlot::Unreadable,
            };
            lock(&shared).insert(key, slot);
            ctx.request_repaint();
        });
        Pdf::Loading
    }

    /// Page `index` of the PDF `reference` (read by [`PdfCache::get`]), if
    /// it's drawn; else starts drawing it.
    pub fn page(&self, ctx: &egui::Context, reference: &str, index: usize, res: Res) -> Option<TextureId> {
        let now = self.tick.fetch_add(1, Ordering::Relaxed);
        let key = (reference.to_owned(), index, res);
        let mut pages = lock(&self.pages);
        match pages.remove(&key) {
            None => {
                let bytes = match lock(&self.docs).get(reference) {
                    Some(DocSlot::Ready(bytes, _)) => bytes.clone(),
                    _ => return None,
                };
                pages.insert(key.clone(), PageSlot::Loading);
                let (shared, ctx) = (self.pages.clone(), ctx.clone());
                std::thread::spawn(move || {
                    let slot = match std::panic::catch_unwind(|| render_page(&bytes, index, res)) {
                        Ok(Some(img)) => PageSlot::Decoded(img),
                        _ => PageSlot::Failed,
                    };
                    let mut pages = lock(&shared);
                    // (Unless it was let go meanwhile: locked notes closed.)
                    if pages.contains_key(&key) {
                        pages.insert(key, slot);
                        ctx.request_repaint();
                    }
                });
                None
            }
            Some(PageSlot::Decoded(img)) => {
                let tex = ctx.load_texture(format!("pdf:{reference}:{index}:{res:?}"), img, TextureOptions::LINEAR);
                let id = tex.id();
                pages.insert(key, PageSlot::Ready(tex, now));
                evict(&mut pages);
                Some(id)
            }
            Some(PageSlot::Ready(tex, _)) => {
                let id = tex.id();
                pages.insert(key, PageSlot::Ready(tex, now));
                Some(id)
            }
            Some(slot) => {
                pages.insert(key, slot);
                None
            }
        }
    }

    /// PDFs that weren't here may have arrived (a sync brought them): they're
    /// looked for again.
    pub fn retry_missing(&self) {
        lock(&self.docs).retain(|_, s| !matches!(s, DocSlot::Missing));
        lock(&self.file_sizes).retain(|_, s| s.is_some());
    }

    /// Locked notes closed: their PDFs, decrypted, aren't kept.
    pub fn forget_sealed(&self) {
        lock(&self.docs).retain(|r, _| !attachments::is_sealed_ref(r));
        lock(&self.pages).retain(|(r, _, _), _| !attachments::is_sealed_ref(r));
    }

    /// An attachment's (any file's) size in bytes, if it's on this device.
    /// (A locked note's is its encrypted file's: a few bytes more.)
    pub fn file_size(&self, reference: &str) -> Option<u64> {
        *lock(&self.file_sizes).entry(reference.to_owned()).or_insert_with(|| {
            let (name, _) = attachments::split_ref(reference);
            std::fs::metadata(attachments::path(&self.dir, name)).ok().map(|m| m.len())
        })
    }

    /// How many pages are drawn and kept.
    #[cfg(test)]
    fn pages_kept(&self) -> usize {
        lock(&self.pages).values().filter(|s| matches!(s, PageSlot::Ready(..))).count()
    }
}

/// Lets the pages looked at longest ago go, past [`MAX_PAGES`].
fn evict(pages: &mut HashMap<PageKey, PageSlot>) {
    loop {
        let ready = pages.iter().filter_map(|(k, s)| match s {
            PageSlot::Ready(_, t) => Some((k, *t)),
            _ => None,
        });
        if ready.clone().count() <= MAX_PAGES {
            return;
        }
        let Some(oldest) = ready.min_by_key(|(_, t)| *t).map(|(k, _)| k.clone()) else { return };
        pages.remove(&oldest);
    }
}

/// Whether `bytes` are a PDF's (its header may come after some junk, as
/// readers allow).
pub fn is_pdf(bytes: &[u8]) -> bool {
    bytes[..bytes.len().min(1024)].windows(5).any(|w| w == b"%PDF-")
}

/// Each page's size in points; `None` if it isn't a PDF that can be read,
/// or has no pages.
fn page_sizes(bytes: &Arc<Vec<u8>>) -> Option<Vec<Vec2>> {
    if !is_pdf(bytes) {
        return None;
    }
    let pdf = hayro::hayro_syntax::Pdf::new(bytes.clone()).ok()?;
    let sizes: Vec<Vec2> = pdf
        .pages()
        .iter()
        .map(|p| {
            let (w, h) = p.render_dimensions();
            Vec2::new(w, h)
        })
        .collect();
    (!sizes.is_empty() && sizes.iter().all(|s| s.x > 0.0 && s.y > 0.0)).then_some(sizes)
}

/// Page `index` of a PDF, drawn on white.
fn render_page(bytes: &Arc<Vec<u8>>, index: usize, res: Res) -> Option<ColorImage> {
    use hayro::hayro_interpret::InterpreterSettings;
    use hayro::vello_cpu::color::palette::css::WHITE;
    let pdf = hayro::hayro_syntax::Pdf::new(bytes.clone()).ok()?;
    let pages = pdf.pages();
    let page = pages.get(index)?;
    let (w, h) = page.render_dimensions();
    if !(w > 0.0 && h > 0.0) {
        return None;
    }
    let scale = (res.px() / h).min(MAX_W / w).min(8.0);
    let settings = hayro::PixmapSettings { x_scale: scale, y_scale: scale, bg_color: WHITE };
    let pixmap = hayro::render(page, &hayro::RenderCache::new(), &InterpreterSettings::default(), &hayro::RenderSettings::default(), &settings);
    let size = [pixmap.width() as usize, pixmap.height() as usize];
    if size[0] == 0 || size[1] == 0 {
        return None;
    }
    // (Over a white page every pixel is opaque: premultiplied or not is the
    // same.)
    Some(ColorImage::from_rgba_unmultiplied(size, pixmap.data_as_u8_slice()))
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;

    /// A PDF with a page for each size (in points), each with a black
    /// square in its middle.
    pub fn test_pdf(sizes: &[(u32, u32)]) -> Vec<u8> {
        let n = sizes.len();
        let kids: Vec<String> = (0..n).map(|i| format!("{} 0 R", 3 + 2 * i)).collect();
        let mut objects = vec![
            "<< /Type /Catalog /Pages 2 0 R >>".to_owned(),
            format!("<< /Type /Pages /Kids [{}] /Count {n} >>", kids.join(" ")),
        ];
        for (i, (w, h)) in sizes.iter().enumerate() {
            let content = format!("0 g {} {} 40 40 re f", w / 2 - 20, h / 2 - 20);
            objects.push(format!("<< /Type /Page /Parent 2 0 R /MediaBox [0 0 {w} {h}] /Contents {} 0 R >>", 4 + 2 * i));
            objects.push(format!("<< /Length {} >>\nstream\n{content}\nendstream", content.len()));
        }
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

    /// Runs `f` until it gives something, as the threads finish.
    pub fn wait<T>(mut f: impl FnMut() -> Option<T>) -> T {
        for _ in 0..500 {
            if let Some(t) = f() {
                return t;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        panic!("never finished");
    }

    #[test]
    fn pages_are_read_and_drawn() {
        let pdf = Arc::new(test_pdf(&[(200, 100), (100, 300)]));
        assert!(is_pdf(&pdf));
        assert_eq!(page_sizes(&pdf), Some(vec![Vec2::new(200.0, 100.0), Vec2::new(100.0, 300.0)]));
        let img = render_page(&pdf, 0, Res::Card).expect("the page is drawn");
        assert_eq!(img.size, [1440, 720]);
        // White, with the black square in its middle.
        let px = |x: usize, y: usize| img.pixels[y * img.size[0] + x];
        assert_eq!(px(5, 5), egui::Color32::WHITE);
        assert_eq!(px(720, 360), egui::Color32::BLACK);
        assert_eq!(render_page(&pdf, 1, Res::Card).unwrap().size, [240, 720]);
        assert!(render_page(&pdf, 2, Res::Card).is_none());
        // Not a PDF, or one cut short.
        assert!(!is_pdf(b"PK\x03\x04 a zip"));
        assert_eq!(page_sizes(&Arc::new(b"%PDF-1.4 cut short".to_vec())), None);
    }

    #[test]
    fn the_cache_reads_draws_and_lets_go() {
        let dir = tempfile::tempdir().unwrap();
        let pdf = test_pdf(&vec![(60, 80); MAX_PAGES + 2]);
        let name = attachments::save_file(dir.path(), &pdf, "a.pdf").unwrap();
        let broken = attachments::save_file(dir.path(), b"%PDF-1.4 cut short", "b.pdf").unwrap();
        let cache = PdfCache::new(dir.path().to_owned());
        let ctx = egui::Context::default();
        let sizes = wait(|| match cache.get(&ctx, &name) {
            Pdf::Ready(s) => Some(s),
            _ => None,
        });
        assert_eq!(sizes.len(), MAX_PAGES + 2);
        assert_eq!(wait(|| (cache.get(&ctx, &broken) != Pdf::Loading).then_some(cache.get(&ctx, &broken))), Pdf::Gone);
        assert_eq!(cache.get(&ctx, &format!("{}.pdf", "a".repeat(64))), Pdf::Gone, "not on this device");
        for i in 0..MAX_PAGES + 2 {
            wait(|| cache.page(&ctx, &name, i, Res::Card));
        }
        // Only so many are kept: the first ones, looked at longest ago, went.
        assert_eq!(cache.pages_kept(), MAX_PAGES);
        assert!(cache.page(&ctx, &name, 0, Res::Card).is_none());
        assert!(cache.page(&ctx, &name, MAX_PAGES + 1, Res::Card).is_some());
    }
}
