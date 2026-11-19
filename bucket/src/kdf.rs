//! Keys from passwords: Argon2id.
//!
//! Both apps make new keys with [`Cost::DEFAULT`]: 46 MiB, 3 passes, 1 lane.
//! That's above OWASP's recommendation (19 MiB, 2 passes), and small enough
//! for iOS's AutoFill and share extensions, which get little memory. The
//! costs are always stored with what they protect (CleanPassword's
//! `vault.json`, CleanNotes' `encryption.json` and locked notes), so they
//! can be raised later, and keys made with other costs still open.

use zeroize::Zeroizing;

use crate::seal::Key;
use crate::{Error, Result};

/// Argon2id's costs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Cost {
    pub memory_kib: u32,
    pub passes: u32,
    pub lanes: u32,
}

/// The least memory a stored setting may ask for: below this a stolen
/// vault or folder would be too cheap to attack.
pub const MIN_MEMORY_KIB: u32 = 19 * 1024;
/// The most: a damaged or hostile setting mustn't make a device allocate
/// gigabytes.
pub const MAX_MEMORY_KIB: u32 = 1024 * 1024;

impl Cost {
    pub const DEFAULT: Cost = Cost { memory_kib: 46 * 1024, passes: 3, lanes: 1 };

    /// Whether a stored setting is in the allowed range, with at least
    /// `min_memory_kib` (normally [`MIN_MEMORY_KIB`]; tests use less).
    pub fn check(&self, min_memory_kib: u32) -> Result<()> {
        let ok = (min_memory_kib..=MAX_MEMORY_KIB).contains(&self.memory_kib) && (1..=32).contains(&self.passes) && (1..=16).contains(&self.lanes);
        if ok {
            Ok(())
        } else {
            Err(Error::invalid("the key derivation settings are out of the allowed range"))
        }
    }
}

/// The 32-byte key for `password` with `salt` (at least 16 bytes). Check
/// stored costs with [`Cost::check`] first.
pub fn derive(password: &[u8], salt: &[u8], cost: Cost) -> Result<Key> {
    if salt.len() < 16 {
        return Err(Error::invalid("the salt is too short"));
    }
    let params = argon2::Params::new(cost.memory_kib, cost.passes, cost.lanes, Some(32)).map_err(|e| Error::invalid(format!("argon2: {e}")))?;
    let mut key = Zeroizing::new([0u8; 32]);
    argon2::Argon2::new(argon2::Algorithm::Argon2id, argon2::Version::V0x13, params)
        .hash_password_into(password, salt, key.as_mut())
        .map_err(|e| Error::invalid(format!("argon2: {e}")))?;
    Ok(key)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deterministic_per_salt_and_bounded() {
        let cost = Cost { memory_kib: 64, passes: 1, lanes: 1 };
        let salt = [1u8; 16];
        assert_eq!(*derive(b"pw", &salt, cost).unwrap(), *derive(b"pw", &salt, cost).unwrap());
        assert_ne!(*derive(b"pw", &salt, cost).unwrap(), *derive(b"pw2", &salt, cost).unwrap());
        assert_ne!(*derive(b"pw", &salt, cost).unwrap(), *derive(b"pw", &[2u8; 16], cost).unwrap());
        assert!(derive(b"pw", &[1u8; 8], cost).is_err());
        assert!(Cost::DEFAULT.check(MIN_MEMORY_KIB).is_ok());
        assert!(cost.check(MIN_MEMORY_KIB).is_err());
        assert!(cost.check(8).is_ok());
        assert!(Cost { memory_kib: 4 * 1024 * 1024, ..Cost::DEFAULT }.check(MIN_MEMORY_KIB).is_err());
        assert!(Cost { passes: 0, ..Cost::DEFAULT }.check(MIN_MEMORY_KIB).is_err());
    }
}
