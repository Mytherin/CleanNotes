//! Optional encryption of an S3 folder's notes and folders with a key of
//! your own.
//!
//! - The key comes from a passphrase with Argon2id (46 MiB, 3 passes, as
//!   CleanPassword; folders set up before kept 64 MiB). The
//!   random salt and costs are in `<folder>/encryption.json`, with a check
//!   value, so every device makes the same key and a wrong passphrase is
//!   noticed. Only the key is kept (in the credential store), not the
//!   passphrase.
//! - Each object is XChaCha20-Poly1305 with a random 24-byte nonce, written
//!   as `CleanNotes-encrypted-1\n` + base64(nonce ‖ ciphertext). The
//!   object's name (relative to the folder) is authenticated with it, so
//!   encrypted files can't be swapped for one another.
//! - Attachments (pictures) are sealed the same way but kept as bytes:
//!   `CleanNotes-encrypted-bin-1\n` + nonce ‖ ciphertext. Their object names
//!   are an HMAC of the attachment's name under the key
//!   ([`attachment_object`]), so the names (hashes of the pictures) don't
//!   give away which known pictures are there.

use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine;
use serde::{Deserialize, Serialize};

use crate::kdf::{self, Cost};
use crate::seal as sealing;
use crate::{Error, Result};

/// The least Argon2 memory a folder's `encryption.json` may ask for. Tests
/// (here and in the apps) pass less to [`derive`] and [`unlock`].
pub const MIN_MEMORY_KIB: u32 = kdf::MIN_MEMORY_KIB;

/// [`unlock`]'s error for a wrong passphrase.
pub const WRONG_KEY: &str = "that's not this folder's encryption key";

pub const MAGIC: &str = "CleanNotes-encrypted-1\n";
pub const SETTINGS: &str = "encryption.json";
const CHECK: &str = "CleanNotes encryption check";

/// What `encryption.json` holds.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Settings {
    pub version: u32,
    pub kdf: String,
    /// Argon2id memory in KiB, passes, lanes.
    pub m: u32,
    pub t: u32,
    pub p: u32,
    pub salt: String,
    /// [`CHECK`] sealed with the key.
    pub check: String,
}

fn aad(name: &str) -> Vec<u8> {
    format!("cleannotes-s3:{name}").into_bytes()
}

/// The key for `passphrase`, if `s` asks for at least `min_memory_kib` of
/// Argon2 memory ([`MIN_MEMORY_KIB`]; tests use less).
pub fn derive(passphrase: &str, s: &Settings, min_memory_kib: u32) -> Result<[u8; 32]> {
    if s.kdf != "argon2id" || s.version != 1 {
        return Err(Error::invalid("this folder is encrypted in a way this version of CleanNotes doesn't know"));
    }
    let salt = B64.decode(&s.salt).map_err(|_| Error::invalid("encryption.json is damaged"))?;
    // A damaged or hostile encryption.json mustn't make this device
    // allocate gigabytes, or make the key cheap to guess.
    let cost = Cost { memory_kib: s.m, passes: s.t, lanes: s.p };
    cost.check(min_memory_kib).map_err(|e| Error::invalid(format!("encryption.json: {e}")))?;
    Ok(*kdf::derive(passphrase.as_bytes(), &salt, cost)?)
}

/// New settings (with a random salt) and the key for `passphrase`, with
/// the costs both apps use.
pub fn create(passphrase: &str) -> Result<(Settings, [u8; 32])> {
    create_with(passphrase, Cost::DEFAULT.memory_kib, Cost::DEFAULT.passes)
}

/// [`create`] with other costs (tests use tiny ones; the costs are written
/// down, so either is read).
pub fn create_with(passphrase: &str, m: u32, t: u32) -> Result<(Settings, [u8; 32])> {
    let salt = sealing::random_bytes::<16>();
    let mut s = Settings { version: 1, kdf: "argon2id".into(), m, t, p: 1, salt: B64.encode(salt), check: String::new() };
    let key = derive(passphrase, &s, m.min(MIN_MEMORY_KIB))?;
    s.check = seal(&key, SETTINGS, CHECK);
    Ok((s, key))
}

/// The key for `passphrase`, if it's the right one ([`WRONG_KEY`] if not).
pub fn unlock(passphrase: &str, s: &Settings, min_memory_kib: u32) -> Result<[u8; 32]> {
    let key = derive(passphrase, s, min_memory_kib)?;
    match open(&key, SETTINGS, &s.check) {
        Some(c) if c == CHECK => Ok(key),
        _ => Err(Error::invalid(WRONG_KEY)),
    }
}

pub fn seal(key: &[u8; 32], name: &str, plain: &str) -> String {
    format!("{MAGIC}{}", B64.encode(sealing::seal(key, &aad(name), plain.as_bytes())))
}

/// The plaintext, or `None` if it can't be decrypted with `key` (another
/// key, damaged, or moved from another name).
pub fn open(key: &[u8; 32], name: &str, text: &str) -> Option<String> {
    let all = B64.decode(text.strip_prefix(MAGIC)?.trim()).ok()?;
    String::from_utf8(sealing::open(key, &aad(name), &all)?.to_vec()).ok()
}

pub fn is_sealed(text: &str) -> bool {
    text.starts_with(MAGIC)
}

/// How a binary object (an attachment) starts when encrypted: then the
/// nonce and the ciphertext, as bytes (no base64: pictures are large).
pub const MAGIC_BYTES: &[u8] = b"CleanNotes-encrypted-bin-1\n";

pub fn seal_bytes(key: &[u8; 32], name: &str, plain: &[u8]) -> Vec<u8> {
    let mut out = MAGIC_BYTES.to_vec();
    out.extend(sealing::seal(key, &aad(name), plain));
    out
}

/// The plaintext of [`seal_bytes`]' output, or `None` if it can't be
/// decrypted with `key` under `name`.
pub fn open_bytes(key: &[u8; 32], name: &str, data: &[u8]) -> Option<Vec<u8>> {
    sealing::open(key, &aad(name), data.strip_prefix(MAGIC_BYTES)?).map(|p| p.to_vec())
}

/// The name an attachment is stored under in an encrypted folder: keyed,
/// so the listing doesn't tell which pictures (of known content) are there.
pub fn attachment_object(key: &[u8; 32], name: &str) -> String {
    use hmac::{Hmac, Mac};
    use sha2::Sha256;
    let mut m = <Hmac<Sha256> as Mac>::new_from_slice(key).expect("any key length");
    m.update(b"cleannotes-attachment:");
    m.update(name.as_bytes());
    let tag: String = m.finalize().into_bytes().iter().map(|b| format!("{b:02x}")).collect();
    format!("{tag}.bin")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_and_wrong_keys() {
        let (s, key) = create_with("correct horse", 64, 1).unwrap();
        assert_eq!(unlock("correct horse", &s, 8).unwrap(), key);
        assert!(unlock("correct horse", &s, MIN_MEMORY_KIB).is_err(), "too cheap for real use");
        assert!(unlock("wrong", &s, 8).is_err());
        let sealed = seal(&key, "notes/a.md", "# Secret\n");
        assert!(is_sealed(&sealed) && !sealed.contains("Secret"));
        assert_eq!(open(&key, "notes/a.md", &sealed).as_deref(), Some("# Secret\n"));
        assert_eq!(open(&key, "notes/b.md", &sealed), None, "bound to its name");
        assert_eq!(open(&[7; 32], "notes/a.md", &sealed), None);
        assert_eq!(open(&key, "notes/a.md", "plain"), None);
        assert_eq!(open(&key, "notes/a.md", &format!("{MAGIC}!!")), None);
    }

    #[test]
    fn binary_objects() {
        let key = [3u8; 32];
        let pic = b"\x89PNG\r\n\x1a\n\0\xff binary";
        let sealed = seal_bytes(&key, "attachments/x.bin", pic);
        assert!(sealed.starts_with(MAGIC_BYTES) && !sealed.windows(6).any(|w| w == b"binary"));
        assert_eq!(open_bytes(&key, "attachments/x.bin", &sealed).as_deref(), Some(&pic[..]));
        assert_eq!(open_bytes(&key, "attachments/y.bin", &sealed), None);
        assert_eq!(open_bytes(&[4; 32], "attachments/x.bin", &sealed), None);
        let a = attachment_object(&key, "abc.png");
        assert_ne!(a, attachment_object(&[4; 32], "abc.png"));
        assert!(a.ends_with(".bin") && !a.contains("abc"));
    }
}
