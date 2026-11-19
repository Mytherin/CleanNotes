//! Sealing: XChaCha20-Poly1305 with a random 24-byte nonce, as
//! `nonce ‖ ciphertext ‖ tag`. The associated data names what's sealed (a
//! vault and item id, an object's name), so sealed things can't be swapped
//! for one another without failing to open. Each app frames the result its
//! own way (CleanPassword's `CPV1` prefix, CleanNotes' armoured text).

use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};
use rand::rngs::OsRng;
use rand::RngCore;
use zeroize::Zeroizing;

/// A 256-bit key, wiped when dropped.
pub type Key = Zeroizing<[u8; 32]>;

pub const NONCE_LEN: usize = 24;
pub const TAG_LEN: usize = 16;

pub fn random_bytes<const N: usize>() -> [u8; N] {
    let mut b = [0u8; N];
    OsRng.fill_bytes(&mut b);
    b
}

pub fn random_key() -> Key {
    let mut key = Zeroizing::new([0u8; 32]);
    OsRng.fill_bytes(key.as_mut());
    key
}

/// `nonce ‖ ciphertext ‖ tag` of `plain`, bound to `aad`.
pub fn seal(key: &[u8; 32], aad: &[u8], plain: &[u8]) -> Vec<u8> {
    let nonce = random_bytes::<NONCE_LEN>();
    let ct = XChaCha20Poly1305::new(key.into())
        .encrypt(XNonce::from_slice(&nonce), Payload { msg: plain, aad })
        .expect("XChaCha20-Poly1305 encryption can't fail");
    let mut out = Vec::with_capacity(NONCE_LEN + ct.len());
    out.extend_from_slice(&nonce);
    out.extend(ct);
    out
}

/// What [`seal`] sealed, or `None` if it wasn't sealed with this key and
/// `aad`, or was changed since.
pub fn open(key: &[u8; 32], aad: &[u8], sealed: &[u8]) -> Option<Zeroizing<Vec<u8>>> {
    if sealed.len() < NONCE_LEN + TAG_LEN {
        return None;
    }
    let (nonce, ct) = sealed.split_at(NONCE_LEN);
    XChaCha20Poly1305::new(key.into()).decrypt(XNonce::from_slice(nonce), Payload { msg: ct, aad }).ok().map(Zeroizing::new)
}

/// Pads `data` with spaces (which JSON ignores) to a multiple of `block`
/// bytes, so stored sizes don't give away how long what's inside is.
pub fn pad(mut data: Vec<u8>, block: usize) -> Vec<u8> {
    let len = data.len().div_ceil(block).max(1) * block;
    data.resize(len, b' ');
    data
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seal_and_open() {
        let key = random_key();
        let sealed = seal(&key, b"item:1", b"secret");
        assert!(!sealed.windows(6).any(|w| w == b"secret"));
        assert_eq!(open(&key, b"item:1", &sealed).unwrap().as_slice(), b"secret");
        assert!(open(&key, b"item:2", &sealed).is_none(), "bound to its aad");
        assert!(open(&random_key(), b"item:1", &sealed).is_none());
        let mut tampered = sealed.clone();
        *tampered.last_mut().unwrap() ^= 1;
        assert!(open(&key, b"item:1", &tampered).is_none());
        assert!(open(&key, b"item:1", &sealed[..10]).is_none());
        assert_ne!(seal(&key, b"a", b"x"), seal(&key, b"a", b"x"), "random nonces");
    }

    #[test]
    fn padding() {
        assert_eq!(pad(vec![b'x'; 10], 256).len(), 256);
        assert_eq!(pad(vec![b'x'; 256], 256).len(), 256);
        assert_eq!(pad(vec![b'x'; 257], 256).len(), 512);
        assert_eq!(pad(vec![], 256).len(), 256);
    }
}
