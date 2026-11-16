//! Images attached to notes.
//!
//! An attachment is a file named by its content: `<sha256>.<ext>` (the
//! SHA-256 of its bytes in lowercase hex, and the image type's extension).
//! The same image attached twice is kept once, a download can be checked
//! against its name, and two devices attaching the same picture never
//! clash. Files live in the data folder's `Attachments` folder; synced
//! accounts also keep them on their backend (see [`crate::sync`]).
//!
//! In a note's Markdown an image is `![](attachments/<name>)`; in the
//! editor's [`crate::doc::Doc`] it is one character, [`OBJECT`] (as in a
//! system text view's attachments), whose [`crate::doc::Style::image`] is
//! the file's name.

use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use sha2::{Digest, Sha256};

/// The folder (inside the data folder) attachments are kept in.
pub const DIR: &str = "Attachments";
/// What an image's address in the Markdown starts with.
pub const URL_PREFIX: &str = "attachments/";
/// The character an image is in the editor's text.
pub const OBJECT: char = '\u{FFFC}';
/// The largest image accepted (larger files are refused, not resized).
pub const MAX_BYTES: usize = 25 * 1024 * 1024;

/// The SHA-256 of `data` in lowercase hex.
pub fn sha256_hex(data: &[u8]) -> String {
    Sha256::digest(data).iter().map(|b| format!("{b:02x}")).collect()
}

/// Whether `name` is an attachment's file name: 64 lowercase hex digits, a
/// dot, and a short lowercase extension. Nothing else is ever read or
/// written as an attachment (so a note can't name a path elsewhere).
pub fn is_name(name: &str) -> bool {
    let Some((stem, ext)) = name.split_once('.') else { return false };
    stem.len() == 64
        && stem.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        && (1..=5).contains(&ext.len())
        && ext.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
}

/// The image type of `data`, as a file extension, judged by its first
/// bytes (whatever the file was called).
pub fn image_ext(data: &[u8]) -> Option<&'static str> {
    let starts = |m: &[u8]| data.starts_with(m);
    if starts(b"\x89PNG\r\n\x1a\n") {
        Some("png")
    } else if starts(b"\xff\xd8\xff") {
        Some("jpg")
    } else if starts(b"GIF87a") || starts(b"GIF89a") {
        Some("gif")
    } else if data.len() >= 12 && &data[..4] == b"RIFF" && &data[8..12] == b"WEBP" {
        Some("webp")
    } else if starts(b"BM") && data.len() > 26 {
        Some("bmp")
    } else if data.len() >= 12 && &data[4..8] == b"ftyp" && matches!(&data[8..12], b"heic" | b"heix" | b"mif1" | b"msf1" | b"hevc") {
        Some("heic")
    } else {
        None
    }
}

/// The name `data` is kept under.
pub fn name_for(data: &[u8], ext: &str) -> String {
    format!("{}.{ext}", sha256_hex(data))
}

/// Whether `data` is what `name` says it is (its hash matches).
pub fn verify(name: &str, data: &[u8]) -> bool {
    is_name(name) && name.split_once('.').is_some_and(|(stem, _)| stem == sha256_hex(data))
}

/// Where an attachment is kept in `dir`.
pub fn path(dir: &Path, name: &str) -> PathBuf {
    dir.join(name)
}

/// Whether `dir` has the attachment.
pub fn exists(dir: &Path, name: &str) -> bool {
    is_name(name) && path(dir, name).is_file()
}

/// Saves an image in `dir` and returns its name. It must be a picture of a
/// known type (PNG, JPEG, GIF, WebP, BMP or HEIC).
pub fn save(dir: &Path, data: &[u8]) -> Result<String> {
    if data.len() > MAX_BYTES {
        bail!("the image is too large (over {} MB)", MAX_BYTES / (1024 * 1024));
    }
    let Some(ext) = image_ext(data) else { bail!("that isn't an image CleanNotes can show (PNG, JPEG, GIF, WebP, BMP or HEIC)") };
    let name = name_for(data, ext);
    write(dir, &name, data)?;
    Ok(name)
}

/// Writes an attachment's file (once: one already there is kept).
pub fn write(dir: &Path, name: &str, data: &[u8]) -> Result<()> {
    if !is_name(name) {
        bail!("{name} isn't an attachment's name");
    }
    let target = path(dir, name);
    if target.is_file() {
        return Ok(());
    }
    std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    // Written under another name first, so a half-written file never passes
    // for a saved one.
    let part = dir.join(format!("{name}.{}.part", uuid::Uuid::new_v4().simple()));
    std::fs::write(&part, data).with_context(|| format!("writing {}", target.display()))?;
    if let Err(e) = std::fs::rename(&part, &target) {
        let _ = std::fs::remove_file(&part);
        // Another thread may have saved it meanwhile.
        if !target.is_file() {
            return Err(anyhow::Error::new(e).context(format!("writing {}", target.display())));
        }
    }
    Ok(())
}

/// An image's Markdown.
pub fn markdown(name: &str) -> String {
    format!("![]({URL_PREFIX}{name})")
}

/// The attachments a note's Markdown shows, in order, each once.
pub fn refs_in(md: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let needle = format!("]({URL_PREFIX}");
    let mut rest = md;
    while let Some(i) = rest.find(&needle) {
        let after = &rest[i + needle.len()..];
        let end = after.find(')').unwrap_or(after.len());
        let name = &after[..end];
        if is_name(name) && !out.iter().any(|n| n == name) {
            out.push(name.to_owned());
        }
        rest = after;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const PNG: &[u8] = b"\x89PNG\r\n\x1a\nrest of a png";

    #[test]
    fn names_are_checked_strictly() {
        let n = name_for(PNG, "png");
        assert!(is_name(&n));
        assert!(verify(&n, PNG));
        assert!(!verify(&n, b"other"));
        for bad in ["../x.png", "ABC.png", "", "x", &format!("{}.png", "a".repeat(63)), &format!("{}.p/g", "a".repeat(64)), &format!("{}", "a".repeat(64))] {
            assert!(!is_name(bad), "{bad}");
        }
    }

    #[test]
    fn saving_keeps_one_copy_and_sniffs_the_type() {
        let dir = tempfile::tempdir().unwrap();
        let a = save(dir.path(), PNG).unwrap();
        let b = save(dir.path(), PNG).unwrap();
        assert_eq!(a, b);
        assert!(a.ends_with(".png"));
        assert!(exists(dir.path(), &a));
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
        assert!(save(dir.path(), b"plain text").is_err());
        assert_eq!(image_ext(b"\xff\xd8\xff\xe0...."), Some("jpg"));
        assert_eq!(image_ext(b"RIFF\0\0\0\0WEBPVP8 "), Some("webp"));
        assert_eq!(image_ext(b"\0\0\0\x18ftypheic...."), Some("heic"));
    }

    #[test]
    fn references_are_found_once_each() {
        let a = name_for(b"a", "png");
        let b = name_for(b"b", "jpg");
        let md = format!("# x\n![]({URL_PREFIX}{a}) and ![]({URL_PREFIX}{b})\n![]({URL_PREFIX}{a})\n[link](attachments/../../etc)\n");
        assert_eq!(refs_in(&md), vec![a, b]);
    }
}
