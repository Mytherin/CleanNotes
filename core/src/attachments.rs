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
//!
//! **Encrypted pictures.** A locked note's pictures are kept encrypted
//! (see [`crate::lock::seal_bytes`]), each with a random key of its own, in
//! a file named by the SHA-256 of the *encrypted* bytes, with the extension
//! `enc`. The note's text refers to one with its key after a `#`:
//! `![](attachments/<sha256>.enc#<key in hex>)`. That text is sealed with
//! the note, so only whoever opens the note can see the picture, while a
//! sync carries the file like any other (it can check it against its name
//! without the key). A reference, here, is a name or such a name and key.

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

/// An image's Markdown (for a name or a reference with its key).
pub fn markdown(reference: &str) -> String {
    format!("![]({URL_PREFIX}{reference})")
}

/// The extension of an encrypted picture's file.
pub const SEALED_EXT: &str = "enc";

fn is_hex64(s: &str) -> bool {
    s.len() == 64 && s.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// Whether `r` is an image's reference: an attachment's name, or an
/// encrypted picture's name with its key (`<name>.enc#<64 hex digits>`).
pub fn is_ref(r: &str) -> bool {
    match r.split_once('#') {
        None => is_name(r),
        Some((name, key)) => is_name(name) && name.ends_with(&format!(".{SEALED_EXT}")) && is_hex64(key),
    }
}

/// A reference's file name, and the key if it's an encrypted picture's.
pub fn split_ref(r: &str) -> (&str, Option<[u8; 32]>) {
    match r.split_once('#') {
        Some((name, key)) if is_ref(r) => {
            let mut k = [0u8; 32];
            for (i, b) in k.iter_mut().enumerate() {
                *b = u8::from_str_radix(&key[2 * i..2 * i + 2], 16).unwrap_or(0);
            }
            (name, Some(k))
        }
        _ => (r, None),
    }
}

/// Whether `r` is an encrypted picture's reference (with its key).
pub fn is_sealed_ref(r: &str) -> bool {
    is_ref(r) && r.contains('#')
}

/// Each image reference in `md`: where it is (its bytes) and what it is.
fn ref_spans(md: &str) -> Vec<(std::ops::Range<usize>, &str)> {
    let needle = format!("]({URL_PREFIX}");
    let mut out = Vec::new();
    let mut from = 0;
    while let Some(i) = md[from..].find(&needle) {
        let start = from + i + needle.len();
        let len = md[start..].find(')').unwrap_or(md.len() - start);
        let r = &md[start..start + len];
        if is_ref(r) {
            out.push((start..start + len, r));
        }
        from = start;
    }
    out
}

/// The attachments' files a note's Markdown shows, in order, each once
/// (names only: an encrypted picture's key is left out).
pub fn refs_in(md: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for (_, r) in ref_spans(md) {
        let name = split_ref(r).0;
        if !out.iter().any(|n| n == name) {
            out.push(name.to_owned());
        }
    }
    out
}

/// The image references a note's Markdown shows, in order, each once
/// (encrypted pictures' with their keys).
pub fn image_refs_in(md: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for (_, r) in ref_spans(md) {
        if !out.iter().any(|n| n == r) {
            out.push(r.to_owned());
        }
    }
    out
}

/// The attachments' files a note's stored body needs: those its Markdown
/// shows, or a locked note's, listed beside its sealed text (see
/// [`crate::lock::Envelope::att`]). What a sync uploads and downloads.
pub fn refs_in_body(body: &str) -> Vec<String> {
    match crate::lock::parse(body) {
        Some(env) => {
            let mut out: Vec<String> = Vec::new();
            for a in env.att {
                if is_name(&a) && !out.contains(&a) {
                    out.push(a);
                }
            }
            out
        }
        None => refs_in(body),
    }
}

/// `md` with each image reference `f` gives another for replaced.
pub fn replace_refs(md: &str, mut f: impl FnMut(&str) -> Option<String>) -> String {
    let mut out = String::with_capacity(md.len());
    let mut last = 0;
    for (span, r) in ref_spans(md) {
        if let Some(new) = f(r) {
            out.push_str(&md[last..span.start]);
            out.push_str(&new);
            last = span.end;
        }
    }
    out.push_str(&md[last..]);
    out
}

/// A line of Markdown without its images.
pub fn without_images(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut last = 0;
    for (span, _) in ref_spans(line) {
        // An image is `![…](attachments/<ref>)`; a plain link is kept.
        let bracket = span.start - URL_PREFIX.len() - 2;
        if let Some(open) = line[last..bracket].rfind("![").map(|i| last + i) {
            out.push_str(&line[last..open]);
            last = (span.end + 1).min(line.len());
        }
    }
    out.push_str(&line[last..]);
    out
}

/// Whether `dir` has the file of the image reference `r`.
pub fn ref_exists(dir: &Path, r: &str) -> bool {
    is_ref(r) && exists(dir, split_ref(r).0)
}

/// An image's bytes, by reference: read from `dir`, and decrypted (in
/// memory) if it's an encrypted picture. `None` if the file isn't here,
/// or doesn't decrypt.
pub fn load(dir: &Path, r: &str) -> Option<Vec<u8>> {
    if !is_ref(r) {
        return None;
    }
    let (name, key) = split_ref(r);
    let data = std::fs::read(path(dir, name)).ok()?;
    match key {
        None => Some(data),
        Some(k) => crate::lock::open_bytes(&k, &data).ok(),
    }
}

/// Saves an image encrypted with a new key of its own and returns its
/// reference (with the key), for a locked note. As [`save`], it must be a
/// picture of a known type.
pub fn save_sealed(dir: &Path, data: &[u8]) -> Result<String> {
    if data.len() > MAX_BYTES {
        bail!("the image is too large (over {} MB)", MAX_BYTES / (1024 * 1024));
    }
    if image_ext(data).is_none() {
        bail!("that isn't an image CleanNotes can show (PNG, JPEG, GIF, WebP, BMP or HEIC)");
    }
    let key = crate::lock::new_blob_key();
    let blob = crate::lock::seal_bytes(&key, data);
    let name = name_for(&blob, SEALED_EXT);
    write(dir, &name, &blob)?;
    let hex: String = key.iter().map(|b| format!("{b:02x}")).collect();
    Ok(format!("{name}#{hex}"))
}

/// The encrypted picture's bytes saved as a plain attachment: its name.
pub fn unseal(dir: &Path, r: &str) -> Result<String> {
    let Some(data) = load(dir, r) else { bail!("the picture isn't on this device yet") };
    save(dir, &data)
}

/// A plain attachment saved encrypted: its reference (with the key).
pub fn seal(dir: &Path, name: &str) -> Result<String> {
    let Some(data) = load(dir, name) else { bail!("the picture isn't on this device yet") };
    save_sealed(dir, &data)
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

    #[test]
    fn sealed_pictures_are_encrypted_on_disk_and_load_with_their_key() {
        let dir = tempfile::tempdir().unwrap();
        let r = save_sealed(dir.path(), PNG).unwrap();
        assert!(is_ref(&r) && is_sealed_ref(&r));
        let (name, key) = split_ref(&r);
        assert!(name.ends_with(".enc") && key.is_some());
        let on_disk = std::fs::read(path(dir.path(), name)).unwrap();
        assert!(verify(name, &on_disk), "named by the encrypted bytes");
        assert!(!on_disk.windows(4).any(|w| w == b"\x89PNG"));
        assert_eq!(load(dir.path(), &r).unwrap(), PNG);
        assert!(ref_exists(dir.path(), &r));
        // Its name alone (as a sync knows it) doesn't open it.
        assert_eq!(load(dir.path(), name).unwrap(), on_disk);
        let wrong = format!("{name}#{}", "0".repeat(64));
        assert_eq!(load(dir.path(), &wrong), None);
        // Back to a plain attachment, the same as saving it plainly.
        assert_eq!(unseal(dir.path(), &r).unwrap(), name_for(PNG, "png"));
        assert!(save_sealed(dir.path(), b"text").is_err());
        // Keys only go with encrypted pictures, and must be whole.
        let plain = name_for(PNG, "png");
        assert!(!is_ref(&format!("{plain}#{}", "0".repeat(64))));
        assert!(!is_ref(&format!("{name}#{}", "0".repeat(63))));
        assert!(!is_ref(&format!("{name}#{}", "A".repeat(64))));
    }

    #[test]
    fn references_with_keys_are_found_and_replaced() {
        let a = name_for(b"a", "png");
        let s = format!("{}#{}", name_for(b"s", "enc"), "c".repeat(64));
        let md = format!("x ![](attachments/{a}) y\n![](attachments/{s})\n");
        assert_eq!(refs_in(&md), vec![a.clone(), name_for(b"s", "enc")]);
        assert_eq!(image_refs_in(&md), vec![a.clone(), s.clone()]);
        let out = replace_refs(&md, |r| (r == s).then(|| "Z".repeat(1)));
        assert_eq!(out, format!("x ![](attachments/{a}) y\n![](attachments/Z)\n"));
        assert_eq!(without_images(&format!("x ![](attachments/{a}) y ![](attachments/{s})")), "x  y ");
        assert_eq!(without_images("![](attachments/nope) z"), "![](attachments/nope) z");
    }
}
