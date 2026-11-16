//! Optional encryption of an S3 folder's notes and folders with a key of
//! your own.
//!
//! - The key comes from a passphrase with Argon2id (64 MiB, 3 passes). The
//!   random salt and costs are in `<folder>/encryption.json`, with a check
//!   value, so every device makes the same key and a wrong passphrase is
//!   noticed. Only the key is kept (in the credential store), not the
//!   passphrase.
//! - Each object is XChaCha20-Poly1305 with a random 24-byte nonce, written
//!   as `CleanNotes-encrypted-1\n` + base64(nonce ‖ ciphertext). The
//!   object's name (relative to the folder) is authenticated with it, so
//!   encrypted files can't be swapped for one another.

use anyhow::{anyhow, bail, Result};
use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine;
use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{Key, XChaCha20Poly1305, XNonce};
use rand::RngCore;
use serde::{Deserialize, Serialize};

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

pub fn derive(passphrase: &str, s: &Settings) -> Result<[u8; 32]> {
    if s.kdf != "argon2id" || s.version != 1 {
        bail!("this folder is encrypted in a way this version of CleanNotes doesn't know");
    }
    let salt = B64.decode(&s.salt).map_err(|_| anyhow!("encryption.json is damaged"))?;
    let params = argon2::Params::new(s.m, s.t, s.p, Some(32)).map_err(|e| anyhow!("argon2: {e}"))?;
    let mut key = [0u8; 32];
    argon2::Argon2::new(argon2::Algorithm::Argon2id, argon2::Version::V0x13, params)
        .hash_password_into(passphrase.as_bytes(), &salt, &mut key)
        .map_err(|e| anyhow!("argon2: {e}"))?;
    Ok(key)
}

/// New settings (with a random salt) and the key for `passphrase`.
pub fn create(passphrase: &str) -> Result<(Settings, [u8; 32])> {
    // Cheap in tests; the costs used are written down, so either is read.
    #[cfg(test)]
    return create_with(passphrase, 64, 1);
    #[cfg(not(test))]
    create_with(passphrase, 64 * 1024, 3)
}

pub fn create_with(passphrase: &str, m: u32, t: u32) -> Result<(Settings, [u8; 32])> {
    let mut salt = [0u8; 16];
    rand::thread_rng().fill_bytes(&mut salt);
    let mut s = Settings { version: 1, kdf: "argon2id".into(), m, t, p: 1, salt: B64.encode(salt), check: String::new() };
    let key = derive(passphrase, &s)?;
    s.check = seal(&key, SETTINGS, CHECK);
    Ok((s, key))
}

/// The key for `passphrase`, if it's the right one.
pub fn unlock(passphrase: &str, s: &Settings) -> Result<[u8; 32]> {
    let key = derive(passphrase, s)?;
    match open(&key, SETTINGS, &s.check) {
        Some(c) if c == CHECK => Ok(key),
        _ => bail!("that's not this folder's encryption key"),
    }
}

pub fn seal(key: &[u8; 32], name: &str, plain: &str) -> String {
    let mut nonce = [0u8; 24];
    rand::thread_rng().fill_bytes(&mut nonce);
    let ct = XChaCha20Poly1305::new(Key::from_slice(key))
        .encrypt(XNonce::from_slice(&nonce), Payload { msg: plain.as_bytes(), aad: &aad(name) })
        .expect("encryption doesn't fail");
    let mut all = nonce.to_vec();
    all.extend(ct);
    format!("{MAGIC}{}", B64.encode(all))
}

/// The plaintext, or `None` if it can't be decrypted with `key` (another
/// key, damaged, or moved from another name).
pub fn open(key: &[u8; 32], name: &str, text: &str) -> Option<String> {
    let all = B64.decode(text.strip_prefix(MAGIC)?.trim()).ok()?;
    if all.len() < 24 {
        return None;
    }
    let (nonce, ct) = all.split_at(24);
    let plain = XChaCha20Poly1305::new(Key::from_slice(key)).decrypt(XNonce::from_slice(nonce), Payload { msg: ct, aad: &aad(name) }).ok()?;
    String::from_utf8(plain).ok()
}

pub fn is_sealed(text: &str) -> bool {
    text.starts_with(MAGIC)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_and_wrong_keys() {
        let (s, key) = create_with("correct horse", 64, 1).unwrap();
        assert_eq!(unlock("correct horse", &s).unwrap(), key);
        assert!(unlock("wrong", &s).is_err());
        let sealed = seal(&key, "notes/a.md", "# Secret\n");
        assert!(is_sealed(&sealed) && !sealed.contains("Secret"));
        assert_eq!(open(&key, "notes/a.md", &sealed).as_deref(), Some("# Secret\n"));
        assert_eq!(open(&key, "notes/b.md", &sealed), None, "bound to its name");
        assert_eq!(open(&[7; 32], "notes/a.md", &sealed), None);
        assert_eq!(open(&key, "notes/a.md", "plain"), None);
        assert_eq!(open(&key, "notes/a.md", &format!("{MAGIC}!!")), None);
    }
}
