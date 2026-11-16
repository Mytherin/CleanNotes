//! Bitwarden's vault encryption.
//!
//! - The *master key* is derived from the master password and email with
//!   PBKDF2-SHA256 or Argon2id (the account's KDF settings, from prelogin).
//! - The server checks the *master password hash*, PBKDF2(master key,
//!   password, 1 round), so it never sees the password or the key.
//! - The master key, stretched with HKDF-Expand into an encryption and a MAC
//!   key, decrypts the account's *user key* (64 bytes: AES key + MAC key).
//! - Every vault string is an *EncString* of type 2:
//!   `2.<iv>|<ciphertext>|<mac>` (base64), AES-256-CBC with PKCS#7 padding,
//!   and HMAC-SHA256 over iv‖ciphertext. A cipher may have its own key,
//!   itself encrypted with the user key.

use aes::cipher::{block_padding::Pkcs7, BlockDecryptMut, BlockEncryptMut, KeyIvInit};
use anyhow::{bail, Context, Result};
use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine;
use hmac::{Hmac, Mac};
use sha2::{Digest, Sha256};

type HmacSha256 = Hmac<Sha256>;
type Enc = cbc::Encryptor<aes::Aes256>;
type Dec = cbc::Decryptor<aes::Aes256>;

/// How the account derives its master key (from prelogin).
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Kdf {
    Pbkdf2 { iterations: u32 },
    /// Memory in MiB.
    Argon2id { iterations: u32, memory: u32, parallelism: u32 },
}

/// An encryption key and a MAC key.
#[derive(Clone, PartialEq, Eq)]
pub struct Keys {
    pub enc: [u8; 32],
    pub mac: [u8; 32],
}

impl std::fmt::Debug for Keys {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Keys(…)")
    }
}

impl Keys {
    /// From the 64 bytes of a user or cipher key.
    pub fn from_bytes(b: &[u8]) -> Result<Keys> {
        if b.len() != 64 {
            bail!("a vault key is 64 bytes, not {}", b.len());
        }
        Ok(Keys { enc: b[..32].try_into().unwrap(), mac: b[32..].try_into().unwrap() })
    }

    pub fn to_bytes(&self) -> Vec<u8> {
        [self.enc.as_slice(), self.mac.as_slice()].concat()
    }

    pub fn random() -> Keys {
        use rand::RngCore;
        let mut b = [0u8; 64];
        rand::thread_rng().fill_bytes(&mut b);
        Keys::from_bytes(&b).unwrap()
    }
}

pub fn normalize_email(email: &str) -> String {
    email.trim().to_lowercase()
}

// The bounds Bitwarden's clients enforce on KDF settings.
pub const PBKDF2_MIN_ITERATIONS: u32 = 5_000;
pub const PBKDF2_MAX_ITERATIONS: u32 = 2_000_000;
pub const ARGON2_MIN_MEMORY_MIB: u32 = 16;
pub const ARGON2_MAX_MEMORY_MIB: u32 = 1_024;
pub const ARGON2_MIN_ITERATIONS: u32 = 2;
pub const ARGON2_MAX_ITERATIONS: u32 = 10;
pub const ARGON2_MAX_PARALLELISM: u32 = 16;

pub fn master_key(password: &str, email: &str, kdf: Kdf) -> Result<[u8; 32]> {
    let email = normalize_email(email);
    let mut key = [0u8; 32];
    match kdf {
        Kdf::Pbkdf2 { iterations } => {
            // The server picks these. Weak settings would let whoever runs
            // it (or sits in between) brute-force the password from the
            // hash we send, so refuse them as Bitwarden's own clients do.
            if !(PBKDF2_MIN_ITERATIONS..=PBKDF2_MAX_ITERATIONS).contains(&iterations) {
                bail!("the server asks for unsafe KDF settings ({iterations} PBKDF2 iterations)");
            }
            pbkdf2::pbkdf2_hmac::<Sha256>(password.as_bytes(), email.as_bytes(), iterations, &mut key);
        }
        Kdf::Argon2id { iterations, memory, parallelism } => {
            if !(ARGON2_MIN_MEMORY_MIB..=ARGON2_MAX_MEMORY_MIB).contains(&memory)
                || !(ARGON2_MIN_ITERATIONS..=ARGON2_MAX_ITERATIONS).contains(&iterations)
                || !(1..=ARGON2_MAX_PARALLELISM).contains(&parallelism)
            {
                bail!("the server asks for unsafe KDF settings (Argon2id: {memory} MiB, {iterations} iterations, {parallelism} threads)");
            }
            let salt = Sha256::digest(email.as_bytes());
            let params = argon2::Params::new(memory * 1024, iterations, parallelism, Some(32))
                .map_err(|e| anyhow::anyhow!("bad Argon2 settings: {e}"))?;
            argon2::Argon2::new(argon2::Algorithm::Argon2id, argon2::Version::V0x13, params)
                .hash_password_into(password.as_bytes(), &salt, &mut key)
                .map_err(|e| anyhow::anyhow!("Argon2: {e}"))?;
        }
    }
    Ok(key)
}

/// What the server is sent instead of the password.
pub fn master_password_hash(master_key: &[u8; 32], password: &str) -> String {
    let mut out = [0u8; 32];
    pbkdf2::pbkdf2_hmac::<Sha256>(master_key, password.as_bytes(), 1, &mut out);
    B64.encode(out)
}

/// The master key stretched into the keys that decrypt the user key.
pub fn stretch(master_key: &[u8; 32]) -> Keys {
    let hk = hkdf::Hkdf::<Sha256>::from_prk(master_key).expect("32 bytes is a valid PRK");
    let mut enc = [0u8; 32];
    let mut mac = [0u8; 32];
    hk.expand(b"enc", &mut enc).unwrap();
    hk.expand(b"mac", &mut mac).unwrap();
    Keys { enc, mac }
}

/// Encrypts to an EncString (type 2).
pub fn encrypt(keys: &Keys, plain: &[u8]) -> String {
    use rand::RngCore;
    let mut iv = [0u8; 16];
    rand::thread_rng().fill_bytes(&mut iv);
    encrypt_with_iv(keys, plain, iv)
}

fn encrypt_with_iv(keys: &Keys, plain: &[u8], iv: [u8; 16]) -> String {
    let ct = Enc::new(&keys.enc.into(), &iv.into()).encrypt_padded_vec_mut::<Pkcs7>(plain);
    let mut mac = HmacSha256::new_from_slice(&keys.mac).unwrap();
    mac.update(&iv);
    mac.update(&ct);
    let tag = mac.finalize().into_bytes();
    format!("2.{}|{}|{}", B64.encode(iv), B64.encode(&ct), B64.encode(tag))
}

pub fn encrypt_str(keys: &Keys, s: &str) -> String {
    encrypt(keys, s.as_bytes())
}

/// Decrypts an EncString, checking its MAC first.
pub fn decrypt(keys: &Keys, enc: &str) -> Result<Vec<u8>> {
    let (kind, rest) = enc.split_once('.').context("not an encrypted string")?;
    if kind != "2" {
        bail!("encryption type {kind} isn't supported");
    }
    let mut parts = rest.split('|');
    let (Some(iv), Some(ct), Some(tag)) = (parts.next(), parts.next(), parts.next()) else {
        bail!("malformed encrypted string");
    };
    let iv: [u8; 16] = B64.decode(iv)?.try_into().map_err(|_| anyhow::anyhow!("bad IV"))?;
    let ct = B64.decode(ct)?;
    let tag = B64.decode(tag)?;
    let mut mac = HmacSha256::new_from_slice(&keys.mac).unwrap();
    mac.update(&iv);
    mac.update(&ct);
    mac.verify_slice(&tag).map_err(|_| anyhow::anyhow!("the data doesn't match its key (MAC check failed)"))?;
    Dec::new(&keys.enc.into(), &iv.into())
        .decrypt_padded_vec_mut::<Pkcs7>(&ct)
        .map_err(|_| anyhow::anyhow!("bad padding"))
}

pub fn decrypt_str(keys: &Keys, enc: &str) -> Result<String> {
    String::from_utf8(decrypt(keys, enc)?).context("decrypted text isn't UTF-8")
}

/// The length of the EncString of `n` bytes of plain text.
pub fn encrypted_len(n: usize) -> usize {
    let b64 = |len: usize| len.div_ceil(3) * 4;
    let ct = (n / 16 + 1) * 16;
    2 + b64(16) + 1 + b64(ct) + 1 + b64(32)
}

/// The most plain text bytes whose EncString fits in `limit` characters.
pub fn max_plain_len(limit: usize) -> usize {
    let mut n = limit * 3 / 4;
    while n > 0 && encrypted_len(n) > limit {
        n -= 1;
    }
    n
}

#[cfg(test)]
mod tests {
    use super::*;

    // Known answers computed independently in Node.js (its crypto module, and
    // hash-wasm for Argon2id; HKDF-Expand as HMAC(prk, info‖0x01), since
    // Bitwarden skips HKDF's extract step), for password "correct horse" and
    // email " User@Example.com ". They're in kat.rs.
    const PASSWORD: &str = "correct horse";
    const EMAIL: &str = " User@Example.com ";

    fn hex(b: &[u8]) -> String {
        b.iter().map(|x| format!("{x:02x}")).collect()
    }

    #[test]
    fn pbkdf2_master_key_and_hash_match_node() {
        let mk = master_key(PASSWORD, EMAIL, Kdf::Pbkdf2 { iterations: 5000 }).unwrap();
        assert_eq!(hex(&mk), KAT_MASTER_KEY);
        assert_eq!(master_password_hash(&mk, PASSWORD), KAT_HASH);
        let s = stretch(&mk);
        assert_eq!(hex(&s.enc), KAT_ENC);
        assert_eq!(hex(&s.mac), KAT_MAC);
    }

    #[test]
    fn argon2_master_key_matches_node() {
        let mk = master_key(PASSWORD, EMAIL, Kdf::Argon2id { iterations: 3, memory: 16, parallelism: 2 }).unwrap();
        assert_eq!(hex(&mk), KAT_ARGON2);
    }

    #[test]
    fn unsafe_kdf_settings_from_the_server_are_refused() {
        for kdf in [
            Kdf::Pbkdf2 { iterations: 0 },
            Kdf::Pbkdf2 { iterations: 1 },
            Kdf::Pbkdf2 { iterations: 4999 },
            Kdf::Pbkdf2 { iterations: u32::MAX },
            Kdf::Argon2id { iterations: 1, memory: 64, parallelism: 4 },
            Kdf::Argon2id { iterations: 3, memory: 1, parallelism: 4 },
            Kdf::Argon2id { iterations: 3, memory: 64, parallelism: 0 },
            // Would overflow `memory * 1024`.
            Kdf::Argon2id { iterations: 3, memory: 4_194_304, parallelism: 4 },
            Kdf::Argon2id { iterations: 3, memory: u32::MAX, parallelism: 4 },
        ] {
            assert!(master_key(PASSWORD, EMAIL, kdf).is_err(), "{kdf:?} was accepted");
        }
    }

    #[test]
    fn enc_string_matches_node() {
        let keys = Keys::from_bytes(&(0u8..64).collect::<Vec<_>>()).unwrap();
        let iv: [u8; 16] = (100u8..116).collect::<Vec<_>>().try_into().unwrap();
        assert_eq!(encrypt_with_iv(&keys, b"Hello, vault!", iv), KAT_ENCSTRING);
        assert_eq!(decrypt_str(&keys, KAT_ENCSTRING).unwrap(), "Hello, vault!");
    }

    #[test]
    fn round_trip_and_tamper_detection() {
        let k = Keys::random();
        let e = encrypt_str(&k, "secret note ✓");
        assert_eq!(decrypt_str(&k, &e).unwrap(), "secret note ✓");
        let mut tampered = e.clone();
        let i = tampered.find('|').unwrap() + 2;
        tampered.replace_range(i..i + 1, if &e[i..i + 1] == "A" { "B" } else { "A" });
        assert!(decrypt(&k, &tampered).is_err());
        assert!(decrypt(&Keys::random(), &e).is_err());
    }

    #[test]
    fn encrypted_lengths_are_exact() {
        let k = Keys::random();
        for n in [0, 1, 15, 16, 17, 100, 7000] {
            assert_eq!(encrypt(&k, &vec![b'x'; n]).len(), encrypted_len(n), "n = {n}");
        }
        let m = max_plain_len(10_000);
        assert!(encrypted_len(m) <= 10_000 && encrypted_len(m + 16) > 10_000);
    }

    include!("kat.rs");
}
