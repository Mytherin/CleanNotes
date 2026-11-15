//! Known-answer tests from the official client's own test suites
//! (standardnotes/app): `packages/sncrypto-web/test/crypto.test.js` for
//! Argon2 and XChaCha20-Poly1305, `packages/snjs/mocha/004.test.js` for the
//! root key, and the encrypted backup in
//! `packages/snjs/mocha/model_tests/importing.test.js` (an items key made by
//! a real client, encrypted with a real account's root key).

use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine;

use super::crypto::*;

const SODIUM_KEY: &str = "808182838485868788898a8b8c8d8e8f909192939495969798999a9b9c9d9e9f";
const SODIUM_NONCE: &str = "404142434445464748494a4b4c4d4e4f5051525354555657";
const SODIUM_AD: &str = "50515253c0c1c2c3c4c5c6c7";

#[test]
fn argon2_libsodium_vector() {
    let out = argon2("correct horse battery staple", &unhex("808182838485868788898a8b8c8d8e8f").unwrap(), 2, 67_108_864, 16).unwrap();
    assert_eq!(hex(&out), "720f95400220748a811bca9b8cff5d6e");
}

#[test]
fn argon2_with_an_emoji() {
    let out = argon2("correct horse battery staple ✅", &unhex("808182838485868788898a8b8c8d8e8f").unwrap(), 2, 67_108_864, 16).unwrap();
    assert_eq!(hex(&out), "18dfbc268f251701652c8e38b5273f73");
}

#[test]
fn argon2_with_a_generated_salt() {
    // sha256("foo:bar"), first half: the way protocol 004 salts.
    let s = salt("foo", "bar").unwrap();
    let out = argon2("foobarfoo🔒", &s, 5, 67_108_864, 32).unwrap();
    assert_eq!(hex(&out), "bb6ec440708c271ce34decd7f997e2444d309b1105992779ccdb47f78a5fda6f");
}

#[test]
fn root_key_for_sign_in() {
    let k = root_key("very_secure", "foo@bar.com", "baaec0131d677cf993381367eb082fe377cefe70118c1699cb9b38f0bc850e7b").unwrap();
    assert_eq!(k.master_key, "5d68e78b56d454e32e1f5dbf4c4e7cf25d74dc1efc942e7c9dfce572c1f3b943");
    assert_eq!(k.server_password, "83707dfc837b3fe52b317be367d3ed8e14e903b2902760884fd0246a77c2299d");
}

#[test]
fn xchacha_libsodium_vector() {
    let pt = unhex(
        "4c616469657320616e642047656e746c656d656e206f662074686520636c6173\
         73206f66202739393a204966204920636f756c64206f6666657220796f75206f\
         6e6c79206f6e652074697020666f7220746865206675747572652c2073756e73\
         637265656e20776f756c642062652069742e",
    )
    .unwrap();
    let ct = xchacha_encrypt(&unhex(SODIUM_KEY).unwrap(), &unhex(SODIUM_NONCE).unwrap(), &pt, &unhex(SODIUM_AD).unwrap()).unwrap();
    assert_eq!(
        hex(&ct),
        "bd6d179d3e83d43b9576579493c0e939572a1700252bfaccbed2902c21396cbb\
         731c7f1b0b4aa6440bf3a82f4eda7e39ae64c6708c54c216cb96b72e1213b452\
         2f8c9ba40db5d945b11b69b982c1bb9e3f3fac2bc369488f76b2383565d3fff9\
         21f9664c97637da9768812f615c68b13b52ec0875924c1c7987947deafd8780acf49"
    );
}

#[test]
fn xchacha_with_an_emoji() {
    let pt = "Ladies and Gentlemen of the class of '99: If I could offer you only one tip for the future, sunscreen would be it.🌞";
    let key = unhex(SODIUM_KEY).unwrap();
    let nonce = unhex(SODIUM_NONCE).unwrap();
    let ad = unhex(SODIUM_AD).unwrap();
    let ct = xchacha_encrypt(&key, &nonce, pt.as_bytes(), &ad).unwrap();
    assert_eq!(
        B64.encode(&ct),
        "vW0XnT6D1DuVdleUk8DpOVcqFwAlK/rMvtKQLCE5bLtzHH8bC0qmRAvzqC9O2n45rmTGcIxUwhbLlrcuEhO0Ui+Mm6QNtdlFsRtpuYLBu54/P6wrw2lIj3ayODVl0//5IflmTJdjfal2iBL2FcaLE7UuOqw6kdl7HV6PKzn0pIOeHH3rkwQ="
    );
    assert_eq!(xchacha_decrypt(&key, &nonce, &ct, &ad).unwrap(), pt.as_bytes());
}

/// An items key from a real account's backup (password `password`).
const BACKUP_UUID: &str = "10051be7-4ca2-4af3-aae9-021939df4fab";
const BACKUP_IDENTIFIER: &str = "playground@bitar.io";
const BACKUP_NONCE: &str = "651ae1ec9580339a3567ee7f0f8672492de2c166a556c13d119728a03c60f793";
const BACKUP_AD: &str = "eyJrcCI6eyJpZGVudGlmaWVyIjoicGxheWdyb3VuZEBiaXRhci5pbyIsInB3X25vbmNlIjoiNjUxYWUxZWM5NTgwMzM5YTM1NjdlZTdmMGY4NjcyNDkyZGUyYzE2NmE1NTZjMTNkMTE5NzI4YTAzYzYwZjc5MyIsInZlcnNpb24iOiIwMDQiLCJvcmlnaW5hdGlvbiI6InByb3RvY29sLXVwZ3JhZGUiLCJjcmVhdGVkIjoiMTYxNDc4NDE5MjQ5NyJ9LCJ1IjoiMTAwNTFiZTctNGNhMi00YWYzLWFhZTktMDIxOTM5ZGY0ZmFiIiwidiI6IjAwNCJ9";

#[test]
fn decrypts_an_items_key_made_by_the_official_client() {
    let content = format!("004:77a986823b8ffdd87164b6f541de6ed420b70ac67e055774:+8cjww1QbyXNX+PSKeCwmnysv0rAoEaKh409VWQJpDbEy/pPZCT6c0rKxLzvyMiSq6EwkOiduZMzokRgCKP7RuRqNPJceWsxNnpIUwa40KR1IP2tdreW4J8v9pFEzPMec1oq40u+c+UI/Y6ChOLV/4ozyWmpQCK3y8Ugm7B1/FzaeDs9Ie6Mvf98+XECoi0fWv9SO2TeBvq1G24LXd4zf0j8jd0sKZbLPXH0+gaUXtBH7A56lHvB0ED9NuiHI8xopTBd9ogKlz/b5+JB4zA2zQCQ3WMEE1qz6WeB2S4FMomgeO1e3trabdU0ICu0WMvDVii4qNlQo/inD41oHXKeV5QwnYoGjPrLJIaP0hiLKhDURTHygCdvWdp63OWI+aGxv0/HI+nfcRsqSE+aYECrWB/kp/c5yTrEqBEafuWZkw==:{BACKUP_AD}");
    let enc_item_key = format!("004:d25deb224251b4705a44d8ce125a62f6a2f0e0e856603e8f:FEv1pfU/VfY7XhJrTfpcdhaSBfmNySTQtHohFYDm8V84KlyF5YaXRKV7BfXsa77DKTjOCU/EHHsWwhBEEfsNnzNySHxTHNc26bpoz0V8h50=:{BACKUP_AD}");
    let root = root_key("password", BACKUP_IDENTIFIER, BACKUP_NONCE).unwrap();
    let ids = ItemIds { uuid: BACKUP_UUID, key_system_identifier: None, shared_vault_uuid: None };
    let v = decrypt_item(ids, &Encrypted { content, enc_item_key }, &root.master_key).unwrap();
    // A key first made under protocol 003, kept through the upgrade to 004.
    assert_eq!(v["version"], "003");
    let k = v["itemsKey"].as_str().unwrap();
    assert_eq!(k.len(), 64);
    assert!(unhex(k).is_ok());
    // The wrong password doesn't.
    let wrong = root_key("passw0rd", BACKUP_IDENTIFIER, BACKUP_NONCE).unwrap();
    assert!(decrypt_string(
        &format!("004:d25deb224251b4705a44d8ce125a62f6a2f0e0e856603e8f:FEv1pfU/VfY7XhJrTfpcdhaSBfmNySTQtHohFYDm8V84KlyF5YaXRKV7BfXsa77DKTjOCU/EHHsWwhBEEfsNnzNySHxTHNc26bpoz0V8h50=:{BACKUP_AD}"),
        &wrong.master_key,
        ids
    )
    .is_err());
}
