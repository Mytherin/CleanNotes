//! The app icon: a rounded notepad with a yellow top band and ruled lines,
//! drawn in code so it's sharp at every size. `cargo test
//! app_icon::tests::write_exe_icon -- --ignored` writes
//! `assets/CleanNotes.ico` for the executable (see `build.rs`).

use eframe::egui::IconData;

/// RGBA pixels of the icon at `size`×`size`.
pub fn render(size: u32) -> Vec<u8> {
    draw(size, 0.06, 0.2)
}

/// The icon filling the whole square, for iOS (which rounds the corners
/// itself).
#[cfg(test)]
fn render_full_bleed(size: u32) -> Vec<u8> {
    draw(size, 0.0, 0.0)
}

fn draw(size: u32, margin: f32, radius: f32) -> Vec<u8> {
    let s = size as f32;
    let mut px = vec![0u8; (size * size * 4) as usize];
    let margin = s * margin;
    let radius = s * radius;
    let band = margin + (s - 2.0 * margin) * 0.27;
    const SS: u32 = 4;
    for y in 0..size {
        for x in 0..size {
            let mut acc = [0f32; 4];
            for sy in 0..SS {
                for sx in 0..SS {
                    let fx = x as f32 + (sx as f32 + 0.5) / SS as f32;
                    let fy = y as f32 + (sy as f32 + 0.5) / SS as f32;
                    if !in_rounded(fx, fy, margin, s - margin, radius) {
                        continue;
                    }
                    let c = color_at(fx, fy, s, margin, band);
                    for i in 0..3 {
                        acc[i] += c[i] as f32;
                    }
                    acc[3] += 255.0;
                }
            }
            let n = (SS * SS) as f32;
            let i = ((y * size + x) * 4) as usize;
            let a = acc[3] / n;
            if a > 0.0 {
                for c in 0..3 {
                    px[i + c] = (acc[c] / (acc[3] / 255.0)).round().min(255.0) as u8;
                }
            }
            px[i + 3] = a.round() as u8;
        }
    }
    px
}

fn in_rounded(x: f32, y: f32, lo: f32, hi: f32, r: f32) -> bool {
    if x < lo || x > hi || y < lo || y > hi {
        return false;
    }
    let cx = x.clamp(lo + r, hi - r);
    let cy = y.clamp(lo + r, hi - r);
    (x - cx).powi(2) + (y - cy).powi(2) <= r * r
}

fn color_at(x: f32, y: f32, s: f32, margin: f32, band: f32) -> [u8; 3] {
    if y < band {
        // A yellow band, a little lighter at the top.
        let t = ((y - margin) / (band - margin)).clamp(0.0, 1.0);
        let lerp = |a: f32, b: f32| (a + (b - a) * t) as u8;
        return [lerp(255.0, 252.0), lerp(214.0, 196.0), lerp(66.0, 22.0)];
    }
    // The band's shadow line.
    if y < band + s * 0.012 {
        return [214, 170, 30];
    }
    // Ruled lines on the paper.
    let gap = s * 0.115;
    let rel = (y - band) % gap;
    if s >= 32.0 && rel > gap - s * 0.012 && x > margin + s * 0.08 && x < s - margin - s * 0.08 {
        return [214, 214, 218];
    }
    [252, 252, 250]
}

pub fn icon_data(size: u32) -> IconData {
    IconData { rgba: render(size), width: size, height: size }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn corners_are_transparent_and_the_middle_is_paper() {
        let px = render(64);
        assert_eq!(px[3], 0, "top-left corner");
        let mid = ((40 * 64 + 32) * 4) as usize;
        assert_eq!(px[mid + 3], 255);
        let top = ((10 * 64 + 32) * 4) as usize;
        assert!(px[top] > 240 && px[top + 2] < 90, "the band is yellow");
    }

    /// Writes the iPhone app's icon (1024 px, opaque, square).
    #[test]
    #[ignore]
    fn write_ios_icon() {
        let size = 1024;
        let px = render_full_bleed(size);
        assert!(px.chunks(4).all(|p| p[3] == 255), "iOS icons are opaque");
        let mut out = Vec::new();
        let mut enc = png::Encoder::new(&mut out, size, size);
        enc.set_color(png::ColorType::Rgba);
        enc.set_depth(png::BitDepth::Eight);
        enc.write_header().unwrap().write_image_data(&px).unwrap();
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/ios/CleanNotes/Assets.xcassets/AppIcon.appiconset/AppIcon.png");
        std::fs::write(path, out).unwrap();
    }

    /// Writes assets/CleanNotes.ico (PNG-compressed images).
    #[test]
    #[ignore]
    fn write_exe_icon() {
        let sizes = [16u32, 24, 32, 48, 64, 128, 256];
        let images: Vec<Vec<u8>> = sizes
            .iter()
            .map(|&s| {
                let mut out = Vec::new();
                let mut enc = png::Encoder::new(&mut out, s, s);
                enc.set_color(png::ColorType::Rgba);
                enc.set_depth(png::BitDepth::Eight);
                enc.write_header().unwrap().write_image_data(&render(s)).unwrap();
                out
            })
            .collect();
        let mut ico = Vec::new();
        ico.extend_from_slice(&[0, 0, 1, 0]);
        ico.extend_from_slice(&(sizes.len() as u16).to_le_bytes());
        let mut offset = 6 + 16 * sizes.len() as u32;
        for (s, img) in sizes.iter().zip(&images) {
            let b = if *s >= 256 { 0 } else { *s as u8 };
            ico.extend_from_slice(&[b, b, 0, 0]);
            ico.extend_from_slice(&1u16.to_le_bytes());
            ico.extend_from_slice(&32u16.to_le_bytes());
            ico.extend_from_slice(&(img.len() as u32).to_le_bytes());
            ico.extend_from_slice(&offset.to_le_bytes());
            offset += img.len() as u32;
        }
        for img in &images {
            ico.extend_from_slice(img);
        }
        std::fs::write(concat!(env!("CARGO_MANIFEST_DIR"), "/assets/CleanNotes.ico"), ico).unwrap();
    }
}
