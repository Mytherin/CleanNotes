//! Standard Notes' encryption, protocol version 004, as the official client
//! does it (`packages/encryption/src/Domain/Operator/004` in
//! standardnotes/app):
//!
//! - **Root key:** Argon2id (libsodium's `crypto_pwhash`: 5 passes, 64 MiB,
//!   one lane) of the password, salted with the first 16 bytes of
//!   `SHA-256("<identifier>:<pw_nonce>")`, 64 bytes out. The first half is
//!   the master key (it encrypts the items keys), the second the "server
//!   password" sent to sign in.
//! - **Protocol strings:** `004:<nonce>:<ciphertext>:<authenticated data>:<additional data>`,
//!   XChaCha20-Poly1305 with a random 24-byte nonce (hex), the ciphertext
//!   in base64 and the authenticated data as base64 of a JSON object with
//!   sorted keys (`{"u": item uuid, "v": "004"}`, plus `kp`, the key
//!   params, for items encrypted with the root key). The AEAD's associated
//!   data is that base64 text. The additional data is `e30=` (`{}`) for
//!   items that aren't signed.
//! - **Items:** each item has its own random content key (64 hex
//!   characters), encrypted with an items key (or, for items keys, with the
//!   master key) into `enc_item_key`; the item's content JSON is encrypted
//!   with the content key into `content`.
//!
//! Keys travel as hex strings, as in the client.

use anyhow::{anyhow, bail, Context, Result};
use base64::engine::general_purpose::{STANDARD as B64, URL_SAFE_NO_PAD};
use base64::Engine;
use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{Key, XChaCha20Poly1305, XNonce};
use rand::RngCore;
use serde_json::value::RawValue;
use serde_json::Value;
use sha2::{Digest, Sha256};

pub const VERSION: &str = "004";
/// `crypto_pwhash` settings of protocol 004.
pub const ARGON_ITERATIONS: u32 = 5;
pub const ARGON_MEMORY_BYTES: u32 = 67_108_864;
pub const ARGON_OUTPUT_BYTES: usize = 64;
/// base64 of `{}`: the additional data of an unsigned item.
pub const EMPTY_ADDITIONAL_DATA: &str = "e30=";

pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

pub fn unhex(s: &str) -> Result<Vec<u8>> {
    if s.len() % 2 != 0 {
        bail!("odd-length hex");
    }
    (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).map_err(|_| anyhow!("bad hex"))).collect()
}

/// `generateRandomKey(bits)`: random bytes as hex.
pub fn random_hex(bytes: usize) -> String {
    let mut b = vec![0u8; bytes];
    rand::thread_rng().fill_bytes(&mut b);
    hex(&b)
}

pub fn sha256_hex(s: &str) -> String {
    hex(&Sha256::digest(s.as_bytes()))
}

/// The PKCE code challenge for a code verifier, as the client makes it:
/// unpadded base64url of the *hex text* of SHA-256(verifier).
pub fn code_challenge(verifier: &str) -> String {
    URL_SAFE_NO_PAD.encode(sha256_hex(verifier).as_bytes())
}

/// libsodium's `crypto_pwhash` with the default algorithm (Argon2id 1.3, one
/// lane), `memory` in bytes.
pub fn argon2(password: &str, salt: &[u8], iterations: u32, memory: u32, len: usize) -> Result<Vec<u8>> {
    use argon2::{Algorithm, Argon2, Params, Version};
    let params = Params::new(memory / 1024, iterations, 1, Some(len)).map_err(|e| anyhow!("argon2 params: {e}"))?;
    let mut out = vec![0u8; len];
    Argon2::new(Algorithm::Argon2id, Version::V0x13, params)
        .hash_password_into(password.as_bytes(), salt, &mut out)
        .map_err(|e| anyhow!("argon2: {e}"))?;
    Ok(out)
}

/// The Argon2 salt: SHA-256 of `identifier:nonce`, its first 128 bits.
pub fn salt(identifier: &str, nonce: &str) -> Result<Vec<u8>> {
    let h = sha256_hex(&format!("{identifier}:{nonce}"));
    unhex(&h[..32])
}

/// A protocol-004 root key, as hex.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RootKey {
    pub master_key: String,
    pub server_password: String,
}

pub fn root_key(password: &str, identifier: &str, nonce: &str) -> Result<RootKey> {
    let derived = argon2(password, &salt(identifier, nonce)?, ARGON_ITERATIONS, ARGON_MEMORY_BYTES, ARGON_OUTPUT_BYTES)?;
    let h = hex(&derived);
    Ok(RootKey { master_key: h[..64].to_owned(), server_password: h[64..].to_owned() })
}

pub fn xchacha_encrypt(key: &[u8], nonce: &[u8], plaintext: &[u8], ad: &[u8]) -> Result<Vec<u8>> {
    if key.len() != 32 || nonce.len() != 24 {
        bail!("bad key or nonce length");
    }
    XChaCha20Poly1305::new(Key::from_slice(key))
        .encrypt(XNonce::from_slice(nonce), Payload { msg: plaintext, aad: ad })
        .map_err(|_| anyhow!("encryption failed"))
}

pub fn xchacha_decrypt(key: &[u8], nonce: &[u8], ciphertext: &[u8], ad: &[u8]) -> Result<Vec<u8>> {
    if key.len() != 32 || nonce.len() != 24 {
        bail!("bad key or nonce length");
    }
    XChaCha20Poly1305::new(Key::from_slice(key))
        .decrypt(XNonce::from_slice(nonce), Payload { msg: ciphertext, aad: ad })
        .map_err(|_| anyhow!("can't decrypt (wrong key, or the data was changed)"))
}

/// A JSON string literal, escaped the way `JSON.stringify` does for the
/// strings that occur here.
fn json_str(s: &str) -> String {
    serde_json::to_string(s).expect("string")
}

/// `base64(JSON.stringify(sortedCopy(omitUndefinedCopy(object))))` for an
/// object given as its members' raw JSON texts. Only the top level is
/// sorted (as `sortedCopy` does); nested objects keep their order.
fn consistent_base64(members: &[(String, String)]) -> String {
    let mut m: Vec<&(String, String)> = members.iter().filter(|(_, v)| v.trim() != "null").collect();
    m.sort_by(|a, b| a.0.cmp(&b.0));
    m.dedup_by(|a, b| a.0 == b.0);
    let body: Vec<String> = m.iter().map(|(k, v)| format!("{}:{}", json_str(k), v)).collect();
    B64.encode(format!("{{{}}}", body.join(",")))
}

/// The authenticated data of an item encrypted with an items key.
pub fn item_authenticated_data(uuid: &str) -> String {
    consistent_base64(&[("u".into(), json_str(uuid)), ("v".into(), json_str(VERSION))])
}

/// The authenticated data of an item encrypted with the root key (an items
/// key): with `kp`, the account's key params (raw JSON, in their order).
pub fn root_authenticated_data(uuid: &str, key_params_json: &str) -> String {
    consistent_base64(&[
        ("kp".into(), key_params_json.to_owned()),
        ("u".into(), json_str(uuid)),
        ("v".into(), json_str(VERSION)),
    ])
}

/// The authenticated data a decrypting client checks: the stored one with
/// `u` and `v` (and `ksi`, `svu`) set from the item, re-encoded.
fn expected_authenticated_data(stored_b64: &str, uuid: &str, version: &str, ksi: Option<&str>, svu: Option<&str>) -> Result<String> {
    let raw = B64.decode(stored_b64).context("authenticated data isn't base64")?;
    let raw = String::from_utf8(raw).context("authenticated data isn't text")?;
    let base: std::collections::BTreeMap<String, Box<RawValue>> =
        serde_json::from_str(&raw).context("authenticated data isn't a JSON object")?;
    // An item whose stored `u` is in capitals (older clients) is checked
    // with its uuid in capitals.
    let upper = base.get("u").and_then(|u| serde_json::from_str::<String>(u.get()).ok()).is_some_and(|u| u.chars().any(|c| c.is_ascii_uppercase()));
    let u = if upper { uuid.to_uppercase() } else { uuid.to_owned() };
    let mut members: Vec<(String, String)> = Vec::new();
    let overrides: [(&str, Option<String>); 4] = [
        ("u", Some(json_str(&u))),
        ("v", Some(json_str(version))),
        ("ksi", ksi.map(json_str)),
        ("svu", svu.map(json_str)),
    ];
    for (k, v) in &base {
        if overrides.iter().any(|(o, _)| o == k) {
            continue;
        }
        members.push((k.clone(), v.get().to_owned()));
    }
    for (k, v) in overrides {
        match v {
            Some(v) => members.push((k.to_owned(), v)),
            // `{...base, ...override}` with the override undefined keeps
            // undefined, which `omitUndefinedCopy` then drops.
            None => {}
        }
    }
    Ok(consistent_base64(&members))
}

/// Encrypts text into a protocol string.
pub fn encrypt_string(plaintext: &str, key_hex: &str, authenticated_data: &str) -> Result<String> {
    let key = unhex(key_hex)?;
    let nonce = random_hex(24);
    let ct = xchacha_encrypt(&key, &unhex(&nonce)?, plaintext.as_bytes(), authenticated_data.as_bytes())?;
    Ok(format!("{VERSION}:{nonce}:{}:{authenticated_data}:{EMPTY_ADDITIONAL_DATA}", B64.encode(ct)))
}

/// What identifies an item for checking its authenticated data.
#[derive(Clone, Copy, Debug)]
pub struct ItemIds<'a> {
    pub uuid: &'a str,
    pub key_system_identifier: Option<&'a str>,
    pub shared_vault_uuid: Option<&'a str>,
}

/// Decrypts a protocol string of an item.
pub fn decrypt_string(s: &str, key_hex: &str, ids: ItemIds) -> Result<String> {
    let parts: Vec<&str> = s.split(':').collect();
    if parts.len() < 4 {
        bail!("not a protocol string");
    }
    if parts[0] != VERSION {
        bail!("encrypted with protocol {}, only {VERSION} is supported", parts[0]);
    }
    let ad = expected_authenticated_data(parts[3], ids.uuid, parts[0], ids.key_system_identifier, ids.shared_vault_uuid)?;
    let pt = xchacha_decrypt(&unhex(key_hex)?, &unhex(parts[1])?, &B64.decode(parts[2]).context("bad ciphertext")?, ad.as_bytes())?;
    String::from_utf8(pt).context("decrypted text isn't UTF-8")
}

/// An item's encrypted fields.
#[derive(Clone, Debug, PartialEq)]
pub struct Encrypted {
    pub content: String,
    pub enc_item_key: String,
}

/// Encrypts an item's content with `key_hex` (an items key, or the master
/// key when `key_params_json` is given, for items keys).
pub fn encrypt_item(uuid: &str, content: &Value, key_hex: &str, key_params_json: Option<&str>) -> Result<Encrypted> {
    let ad = match key_params_json {
        Some(kp) => root_authenticated_data(uuid, kp),
        None => item_authenticated_data(uuid),
    };
    let content_key = random_hex(32);
    Ok(Encrypted {
        enc_item_key: encrypt_string(&content_key, key_hex, &ad)?,
        content: encrypt_string(&serde_json::to_string(content)?, &content_key, &ad)?,
    })
}

/// Decrypts an item's content.
pub fn decrypt_item(ids: ItemIds, enc: &Encrypted, key_hex: &str) -> Result<Value> {
    let content_key = decrypt_string(&enc.enc_item_key, key_hex, ids)?;
    let content = decrypt_string(&enc.content, &content_key, ids)?;
    serde_json::from_str(&content).context("decrypted content isn't JSON")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn ids(uuid: &str) -> ItemIds<'_> {
        ItemIds { uuid, key_system_identifier: None, shared_vault_uuid: None }
    }

    #[test]
    fn items_round_trip_and_are_bound_to_their_uuid() {
        let key = random_hex(32);
        let content = json!({"title": "Hi", "text": "ünïcode ✓", "references": []});
        let enc = encrypt_item("a-uuid", &content, &key, None).unwrap();
        assert!(enc.content.starts_with("004:") && enc.content.ends_with(":e30="));
        assert_eq!(decrypt_item(ids("a-uuid"), &enc, &key).unwrap(), content);
        assert!(decrypt_item(ids("another"), &enc, &key).is_err());
        assert!(decrypt_item(ids("a-uuid"), &enc, &random_hex(32)).is_err());
    }

    #[test]
    fn authenticated_data_is_sorted_at_the_top_only() {
        let kp = r#"{"identifier":"a@b.c","pw_nonce":"n","version":"004","origination":"registration","created":"1"}"#;
        let ad = root_authenticated_data("u1", kp);
        let text = String::from_utf8(B64.decode(&ad).unwrap()).unwrap();
        assert_eq!(text, format!(r#"{{"kp":{kp},"u":"u1","v":"004"}}"#));
        // Re-deriving keeps kp as stored.
        assert_eq!(expected_authenticated_data(&ad, "u1", "004", None, None).unwrap(), ad);
        assert_eq!(B64.decode(item_authenticated_data("x")).unwrap(), br#"{"u":"x","v":"004"}"#);
    }

    #[test]
    fn capitalized_uuids_in_old_items_still_decrypt() {
        // As in the client's test "should decrypt string with case-insensitive
        // uuid check for aad".
        let key = random_hex(32);
        let upper = "959B042A-3892-461E-8C50-477C10C7C40A";
        let enc = encrypt_item(upper, &json!({"text": "world"}), &key, None).unwrap();
        assert_eq!(decrypt_item(ids(&upper.to_lowercase()), &enc, &key).unwrap()["text"], "world");
    }

    #[test]
    fn code_challenge_hashes_the_hex_digest() {
        let c = code_challenge("abc");
        assert_eq!(URL_SAFE_NO_PAD.decode(&c).unwrap(), sha256_hex("abc").as_bytes());
        assert!(!c.contains('='));
    }
}
