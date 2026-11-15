//! Locked notes: a note's text encrypted with a password the user chooses.
//!
//! The key comes from the password with Argon2id (each note has its own
//! salt), split with HKDF into an AES-256 key and an HMAC-SHA256 key; the
//! text is encrypted with AES-256-CBC and the result authenticated
//! (encrypt-then-MAC), so a wrong password or a damaged note is detected.
//!
//! A locked note's body keeps its first line in the clear (like Apple
//! Notes, the title of a locked note shows in the list) followed by the
//! sealed text in a fenced block:
//!
//! ````text
//! # Weekend trip
//!
//! ```cleannotes-locked
//! eyJ2IjoxLCJrZGYiOiJhcmdvbjJpZCIs…
//! ```
//! ````
//!
//! The sealed text is one line, so the sync engine's line-by-line merge
//! never mixes two versions of it: edits to a locked note on two devices
//! end up as two notes, as other clashing edits do.

use aes::cipher::{block_padding::Pkcs7, BlockDecryptMut, BlockEncryptMut, KeyIvInit};
use anyhow::{anyhow, bail, Result};
use base64::{engine::general_purpose::STANDARD as B64, Engine};
use hmac::{Hmac, Mac};
use rand::RngCore;
use serde::{Deserialize, Serialize};
use sha2::Sha256;

const FENCE: &str = "```cleannotes-locked";
/// What the HMAC covers besides the ciphertext, so nothing can be swapped.
const CONTEXT: &[u8] = b"cleannotes-locked-v1";
/// Argon2id's cost: 64 MiB, 3 passes (Bitwarden's defaults, one lane).
const M_COST: u32 = 64 * 1024;
const T_COST: u32 = 3;
const P_COST: u32 = 1;

type HmacSha256 = Hmac<Sha256>;

/// Keys made from a password and a salt.
#[derive(Clone)]
pub struct Keys {
    enc: [u8; 32],
    mac: [u8; 32],
}

/// The sealed text and how to open it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Envelope {
    pub v: u32,
    pub kdf: String,
    pub m: u32,
    pub t: u32,
    pub p: u32,
    pub salt: String,
    pub iv: String,
    pub ct: String,
    pub mac: String,
}

impl Envelope {
    pub fn salt(&self) -> Vec<u8> {
        B64.decode(&self.salt).unwrap_or_default()
    }
}

pub fn new_salt() -> Vec<u8> {
    let mut s = vec![0u8; 16];
    rand::thread_rng().fill_bytes(&mut s);
    s
}

/// Makes the keys for a password and salt (the slow part, on purpose).
pub fn derive(password: &str, salt: &[u8]) -> Result<Keys> {
    derive_with(password, salt, M_COST, T_COST, P_COST)
}

fn derive_with(password: &str, salt: &[u8], m: u32, t: u32, p: u32) -> Result<Keys> {
    let params = argon2::Params::new(m, t, p, Some(32)).map_err(|e| anyhow!("argon2: {e}"))?;
    let argon = argon2::Argon2::new(argon2::Algorithm::Argon2id, argon2::Version::V0x13, params);
    let mut master = [0u8; 32];
    argon.hash_password_into(password.as_bytes(), salt, &mut master).map_err(|e| anyhow!("argon2: {e}"))?;
    let hk = hkdf::Hkdf::<Sha256>::new(None, &master);
    let mut keys = Keys { enc: [0; 32], mac: [0; 32] };
    hk.expand(b"cleannotes-lock enc", &mut keys.enc).map_err(|_| anyhow!("hkdf"))?;
    hk.expand(b"cleannotes-lock mac", &mut keys.mac).map_err(|_| anyhow!("hkdf"))?;
    Ok(keys)
}

/// Makes the keys for opening an envelope, with the costs it was sealed
/// with.
pub fn derive_for(password: &str, env: &Envelope) -> Result<Keys> {
    if env.kdf != "argon2id" || env.v != 1 {
        bail!("This note was locked by a newer version of CleanNotes.");
    }
    // Don't let a damaged or hostile note ask for gigabytes of memory.
    if env.m > 1024 * 1024 || env.t > 20 || env.p > 16 {
        bail!("This locked note's settings are out of range.");
    }
    derive_with(password, &env.salt(), env.m, env.t, env.p)
}

fn mac_of(keys: &Keys, env: &Envelope, iv: &[u8], ct: &[u8]) -> Vec<u8> {
    let mut mac = <HmacSha256 as Mac>::new_from_slice(&keys.mac).expect("any key size");
    mac.update(CONTEXT);
    mac.update(format!("{}:{}:{}:{}:{}:", env.v, env.kdf, env.m, env.t, env.p).as_bytes());
    mac.update(&env.salt());
    mac.update(iv);
    mac.update(ct);
    mac.finalize().into_bytes().to_vec()
}

/// Encrypts `text` with keys made from `salt`.
pub fn seal(keys: &Keys, salt: &[u8], text: &str) -> Envelope {
    let mut iv = [0u8; 16];
    rand::thread_rng().fill_bytes(&mut iv);
    let ct = cbc::Encryptor::<aes::Aes256>::new(&keys.enc.into(), &iv.into()).encrypt_padded_vec_mut::<Pkcs7>(text.as_bytes());
    let mut env = Envelope {
        v: 1,
        kdf: "argon2id".into(),
        m: M_COST,
        t: T_COST,
        p: P_COST,
        salt: B64.encode(salt),
        iv: B64.encode(iv),
        ct: B64.encode(&ct),
        mac: String::new(),
    };
    env.mac = B64.encode(mac_of(keys, &env, &iv, &ct));
    env
}

/// Decrypts an envelope; fails for a wrong password or a damaged note.
pub fn open(keys: &Keys, env: &Envelope) -> Result<String> {
    let iv = B64.decode(&env.iv)?;
    let ct = B64.decode(&env.ct)?;
    let want = B64.decode(&env.mac)?;
    let mut mac = <HmacSha256 as Mac>::new_from_slice(&keys.mac).expect("any key size");
    mac.update(CONTEXT);
    mac.update(format!("{}:{}:{}:{}:{}:", env.v, env.kdf, env.m, env.t, env.p).as_bytes());
    mac.update(&env.salt());
    mac.update(&iv);
    mac.update(&ct);
    // verify_slice compares in constant time.
    if mac.verify_slice(&want).is_err() || iv.len() != 16 {
        bail!("wrong password");
    }
    let iv: [u8; 16] = iv.try_into().unwrap();
    let plain = cbc::Decryptor::<aes::Aes256>::new(&keys.enc.into(), &iv.into())
        .decrypt_padded_vec_mut::<Pkcs7>(&ct)
        .map_err(|_| anyhow!("damaged note"))?;
    Ok(String::from_utf8(plain)?)
}

/// The envelope as one line of text.
pub fn encode(env: &Envelope) -> String {
    B64.encode(serde_json::to_vec(env).expect("serializable"))
}

pub fn decode(line: &str) -> Result<Envelope> {
    Ok(serde_json::from_slice(&B64.decode(line.trim())?)?)
}

/// A locked note's body: its title line, then the sealed text.
pub fn locked_body(plain_md: &str, env: &Envelope) -> String {
    let first = plain_md.lines().find(|l| !l.trim().is_empty()).unwrap_or("");
    let t = first.trim_start();
    let title = if t.is_empty() || t.starts_with("```") || t.starts_with("~~~") { "Locked Note" } else { first };
    format!("{title}\n\n{FENCE}\n{}\n```\n", encode(env))
}

/// The sealed text of a locked note's body, if it's locked.
pub fn parse(body: &str) -> Option<Envelope> {
    let mut lines = body.lines();
    lines.find(|l| l.trim_end() == FENCE)?;
    decode(lines.next()?).ok()
}

pub fn is_locked(body: &str) -> bool {
    parse(body).is_some()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn quick(password: &str, salt: &[u8]) -> Keys {
        derive_with(password, salt, 64, 1, 1).unwrap()
    }

    #[test]
    fn seals_and_opens() {
        let salt = new_salt();
        let k = quick("correct horse", &salt);
        let text = "# Secret\n\nThe code is 1234 — ünïcode too.\n";
        let env = seal(&k, &salt, text);
        assert_eq!(open(&k, &env).unwrap(), text);
        // A fresh IV each time.
        assert_ne!(seal(&k, &salt, text).ct, env.ct);
    }

    #[test]
    fn wrong_password_and_tampering_fail() {
        let salt = new_salt();
        let env = seal(&quick("right", &salt), &salt, "hello");
        assert!(open(&quick("wrong", &salt), &env).is_err());
        let mut bad = env.clone();
        let mut ct = B64.decode(&bad.ct).unwrap();
        ct[0] ^= 1;
        bad.ct = B64.encode(ct);
        assert!(open(&quick("right", &salt), &bad).is_err());
        // Changing the costs is detected too.
        let mut bad = env.clone();
        bad.t = 2;
        assert!(open(&quick("right", &salt), &bad).is_err());
    }

    #[test]
    fn body_round_trip_keeps_the_title_in_the_clear() {
        let salt = new_salt();
        let k = quick("pw", &salt);
        let text = "# Bank\n\nPIN 0000\n";
        let env = seal(&k, &salt, text);
        let body = locked_body(text, &env);
        assert!(body.starts_with("# Bank\n"));
        assert!(!body.contains("PIN"));
        assert_eq!(crate::markdown::title_of(&body), "Bank");
        let parsed = parse(&body).unwrap();
        assert_eq!(parsed, env);
        assert_eq!(open(&k, &parsed).unwrap(), text);
        assert!(!is_locked(text));
        // The sealed text is a single line.
        assert_eq!(body.lines().count(), 5);
    }

    #[test]
    fn the_real_costs_work() {
        let salt = new_salt();
        let k = derive("pw", &salt).unwrap();
        let env = seal(&k, &salt, "x");
        assert_eq!((env.m, env.t, env.p), (M_COST, T_COST, P_COST));
        assert_eq!(open(&derive_for("pw", &env).unwrap(), &env).unwrap(), "x");
    }
}
