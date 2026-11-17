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
//!
//! Pictures in a locked note are encrypted too, each with a key of its own
//! ([`seal_bytes`]) that is written in the note's sealed text, so only
//! whoever opens the note can see them (see [`crate::attachments`]). The
//! envelope lists the encrypted files' names in the clear (`att`), so a
//! sync can carry them without the password.

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
    /// The Argon2id costs the keys were made with, written into what they
    /// seal, so the right password always makes them again.
    cost: (u32, u32, u32),
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
    /// The encrypted pictures the sealed text shows (their file names, not
    /// their keys), for syncing them. Not covered by the MAC, so older
    /// versions still open the note; a changed list can only make a sync
    /// fetch other files, each checked against its name.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub att: Vec<String>,
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
    let mut keys = Keys { enc: [0; 32], mac: [0; 32], cost: (m, t, p) };
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
    // Don't let a damaged or hostile note (it may come from a sync server)
    // freeze the app asking for gigabytes of memory or minutes of work.
    if env.m > 256 * 1024 || env.t > 10 || env.p > 16 {
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
    let (m, t, p) = keys.cost;
    let mut env = Envelope {
        v: 1,
        kdf: "argon2id".into(),
        m,
        t,
        p,
        salt: B64.encode(salt),
        iv: B64.encode(iv),
        ct: B64.encode(&ct),
        mac: String::new(),
        att: crate::attachments::refs_in(text),
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

/// What an encrypted picture's file starts with.
const BLOB_MAGIC: &[u8] = b"CNATT1";
/// What a picture's HMAC covers besides the ciphertext.
const BLOB_CONTEXT: &[u8] = b"cleannotes-attachment-v1";

fn blob_keys(key: &[u8; 32]) -> ([u8; 32], [u8; 32]) {
    let hk = hkdf::Hkdf::<Sha256>::new(None, key);
    let (mut enc, mut mac) = ([0u8; 32], [0u8; 32]);
    hk.expand(b"cleannotes-attachment enc", &mut enc).expect("32 bytes");
    hk.expand(b"cleannotes-attachment mac", &mut mac).expect("32 bytes");
    (enc, mac)
}

fn blob_mac(mac_key: &[u8; 32], iv: &[u8], ct: &[u8]) -> HmacSha256 {
    let mut mac = <HmacSha256 as Mac>::new_from_slice(mac_key).expect("any key size");
    mac.update(BLOB_CONTEXT);
    mac.update(iv);
    mac.update(ct);
    mac
}

/// A new random key for a picture.
pub fn new_blob_key() -> [u8; 32] {
    let mut k = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut k);
    k
}

/// Encrypts a picture's bytes with its key: AES-256-CBC and HMAC-SHA256
/// (encrypt-then-MAC) under keys made from it with HKDF, as
/// `CNATT1 ‖ iv ‖ ciphertext ‖ mac`.
pub fn seal_bytes(key: &[u8; 32], data: &[u8]) -> Vec<u8> {
    let (enc, mac_key) = blob_keys(key);
    let mut iv = [0u8; 16];
    rand::thread_rng().fill_bytes(&mut iv);
    let ct = cbc::Encryptor::<aes::Aes256>::new(&enc.into(), &iv.into()).encrypt_padded_vec_mut::<Pkcs7>(data);
    let tag = blob_mac(&mac_key, &iv, &ct).finalize().into_bytes();
    let mut out = Vec::with_capacity(BLOB_MAGIC.len() + 16 + ct.len() + 32);
    out.extend_from_slice(BLOB_MAGIC);
    out.extend_from_slice(&iv);
    out.extend_from_slice(&ct);
    out.extend_from_slice(&tag);
    out
}

/// Decrypts what [`seal_bytes`] made; fails for a wrong key or a damaged
/// file.
pub fn open_bytes(key: &[u8; 32], blob: &[u8]) -> Result<Vec<u8>> {
    let Some(rest) = blob.strip_prefix(BLOB_MAGIC) else { bail!("not an encrypted picture") };
    if rest.len() < 16 + 16 + 32 {
        bail!("damaged picture");
    }
    let (iv, rest) = rest.split_at(16);
    let (ct, tag) = rest.split_at(rest.len() - 32);
    let (enc, mac_key) = blob_keys(key);
    if blob_mac(&mac_key, iv, ct).verify_slice(tag).is_err() {
        bail!("damaged picture");
    }
    let iv: [u8; 16] = iv.try_into().expect("16 bytes");
    cbc::Decryptor::<aes::Aes256>::new(&enc.into(), &iv.into()).decrypt_padded_vec_mut::<Pkcs7>(ct).map_err(|_| anyhow!("damaged picture"))
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
    // The first line with text, without its pictures: it's kept in the
    // clear, and a picture's reference holds its key.
    let first = plain_md.lines().map(crate::attachments::without_images).find(|l| !l.trim().is_empty()).unwrap_or_default();
    let t = first.trim_start();
    let title = if t.is_empty() || t.starts_with("```") || t.starts_with("~~~") { "Locked Note" } else { first.trim_end() };
    format!("{title}\n\n{FENCE}\n{}\n```\n", encode(env))
}

/// The sealed text of a locked note's body, if it's locked: exactly as
/// [`locked_body`] writes it (a title line, a blank line, the fence),
/// so no text can sit next to the seal unencrypted.
pub fn parse(body: &str) -> Option<Envelope> {
    let lines: Vec<&str> = body.trim_end_matches('\n').split('\n').map(|l| l.strip_suffix('\r').unwrap_or(l)).collect();
    match lines.as_slice() {
        [_title, "", fence, sealed, "```"] if fence.trim_end() == FENCE => decode(sealed).ok(),
        _ => None,
    }
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
    fn resealing_keeps_the_costs_the_keys_were_made_with() {
        // A note sealed with other costs (an older version, another client).
        let salt = new_salt();
        let env = seal(&quick("pw", &salt), &salt, "v1");
        assert_eq!((env.m, env.t, env.p), (64, 1, 1));
        // Opened and sealed again with the keys made for it…
        let k = derive_for("pw", &env).unwrap();
        let again = seal(&k, &env.salt(), "v2");
        // …it still opens with the right password.
        let k2 = derive_for("pw", &again).unwrap();
        assert_eq!(open(&k2, &again).unwrap(), "v2");
    }

    #[test]
    fn only_the_exact_locked_layout_counts_as_locked() {
        let salt = new_salt();
        let k = quick("pw", &salt);
        let body = locked_body("# T\nsecret\n", &seal(&k, &salt, "# T\nsecret\n"));
        assert!(is_locked(&body));
        assert!(is_locked(&body.replace('\n', "\r\n")));
        // Text merged in next to the seal would sit in the clear.
        assert!(!is_locked(&body.replace("# T\n", "# T\nplain text\n")));
        assert!(!is_locked(&format!("{body}after\n")));
    }

    #[test]
    fn a_locked_note_is_never_merged_line_by_line() {
        let salt = new_salt();
        let k = quick("pw", &salt);
        let base = "T\n\npara\n";
        let locked = locked_body(base, &seal(&k, &salt, base));
        // Locked here, the title edited elsewhere: both kept, not merged.
        assert!(crate::sync::merge::merge_text(base, &locked, "T secret\n\npara\n").is_err());
        assert!(crate::sync::merge::merge_text(base, "T secret\n\npara\n", &locked).is_err());
        // Only one side changed: that side wins as usual.
        assert_eq!(crate::sync::merge::merge_text(base, &locked, base), Ok(locked.clone()));
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
    fn pictures_seal_and_open_with_their_own_key() {
        let k = new_blob_key();
        let pic = b"\x89PNG\r\n\x1a\nsome picture";
        let blob = seal_bytes(&k, pic);
        assert!(!blob.windows(4).any(|w| w == b"\x89PNG"));
        assert_eq!(open_bytes(&k, &blob).unwrap(), pic);
        assert_ne!(seal_bytes(&k, pic), blob, "a fresh IV each time");
        assert!(open_bytes(&new_blob_key(), &blob).is_err());
        let mut bad = blob.clone();
        bad[10] ^= 1;
        assert!(open_bytes(&k, &bad).is_err());
        assert!(open_bytes(&k, &blob[..20]).is_err());
        assert!(open_bytes(&k, pic).is_err());
    }

    #[test]
    fn the_envelope_lists_its_pictures_and_the_title_never_shows_them() {
        let salt = new_salt();
        let k = quick("pw", &salt);
        let sealed_ref = format!("{}.enc#{}", "a".repeat(64), "b".repeat(64));
        let text = format!("![](attachments/{sealed_ref}) Trip\nmore\n");
        let env = seal(&k, &salt, &text);
        assert_eq!(env.att, vec![format!("{}.enc", "a".repeat(64))]);
        let body = locked_body(&text, &env);
        assert!(body.starts_with(" Trip\n"), "{body}");
        assert!(!body.contains(&"b".repeat(64)));
        assert_eq!(parse(&body).unwrap().att, env.att);
        // A picture alone on the first line: the next line with text.
        let text = format!("![](attachments/{sealed_ref})\n# Title\n");
        assert!(locked_body(&text, &seal(&k, &salt, &text)).starts_with("# Title\n"));
        // Without pictures there's no list (as older versions wrote it).
        let json = String::from_utf8(B64.decode(encode(&seal(&k, &salt, "x"))).unwrap()).unwrap();
        assert!(!json.contains("att"), "{json}");
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
