//! Optional encryption of an S3 folder's notes and folders with a key of
//! your own: shared with CleanTabs, in [`cleanbucket::notes::crypto`].
//! Tests here use tiny Argon2 costs.

pub use cleanbucket::notes::crypto::{attachment_object, is_sealed, open, open_bytes, seal, seal_bytes, Settings, MAGIC, MAGIC_BYTES, SETTINGS};

use anyhow::Result;
use cleanbucket::notes::crypto as shared;

/// The least Argon2 memory a folder's `encryption.json` may ask for
/// (tests use tiny costs).
const MIN_MEMORY_KIB: u32 = if cfg!(test) { 8 } else { shared::MIN_MEMORY_KIB };

/// New settings (with a random salt) and the key for `passphrase`.
pub fn create(passphrase: &str) -> Result<(Settings, [u8; 32])> {
    // Cheap in tests; the costs used are written down, so either is read.
    #[cfg(test)]
    return Ok(shared::create_with(passphrase, 64, 1)?);
    #[cfg(not(test))]
    Ok(shared::create(passphrase)?)
}

/// The key for `passphrase`, if it's the right one.
pub fn unlock(passphrase: &str, s: &Settings) -> Result<[u8; 32]> {
    Ok(shared::unlock(passphrase, s, MIN_MEMORY_KIB)?)
}
