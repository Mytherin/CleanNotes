//! Syncing notes into a Bitwarden vault (bitwarden.com, bitwarden.eu, or a
//! self-hosted server such as Vaultwarden).
//!
//! STUB: the interface the rest of the app uses. To be implemented.

use anyhow::Result;
use serde::{Deserialize, Serialize};

use super::{Backend, NoteData, Snapshot};

pub const DEFAULT_SERVER: &str = "https://vault.bitwarden.com";
pub const DEFAULT_FOLDER: &str = "Notes";

/// A Bitwarden account's settings (no secrets).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Config {
    /// The server's base URL: `https://vault.bitwarden.com`,
    /// `https://vault.bitwarden.eu`, or a self-hosted server.
    pub server: String,
    pub email: String,
    /// The vault folder the notes go in. App folders become vault folders
    /// under it: `Notes/Work/2026`.
    pub folder: String,
}

/// How to sign in.
#[derive(Clone, Debug, PartialEq)]
pub enum Method {
    /// Email (from the config) and master password.
    Password { password: String },
    /// A personal API key (Account Settings → Security → Keys), which skips
    /// two-step login; the master password is still needed to unlock the vault.
    ApiKey { client_id: String, client_secret: String, password: String },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TwoFactorKind {
    Authenticator,
    Email,
    /// Anything the app can't do (Duo, YubiKey, WebAuthn, …).
    Other(u8),
}

#[derive(Clone, Debug, PartialEq)]
pub struct TwoFactorCode {
    pub kind: TwoFactorKind,
    pub code: String,
    pub remember: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct SignInRequest {
    pub method: Method,
    /// The code for a second step, when the server asked for one.
    pub two_factor: Option<TwoFactorCode>,
    /// The code Bitwarden emailed to confirm a new device, when asked for.
    pub new_device_code: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum SignInStep {
    /// Signed in: what to keep in the credential store (opaque).
    Done { secret: String },
    /// The account uses two-step login with these methods.
    NeedsTwoFactor { kinds: Vec<TwoFactorKind> },
    /// Bitwarden emailed a code to confirm this device.
    NeedsNewDeviceCode,
}

/// Signs in (blocking).
pub fn sign_in(_config: &Config, _req: &SignInRequest) -> Result<SignInStep> {
    anyhow::bail!("Bitwarden sync isn't implemented yet")
}

/// Asks the server to email a two-step login code (for [`TwoFactorKind::Email`]).
pub fn send_email_code(_config: &Config, _req: &SignInRequest) -> Result<()> {
    anyhow::bail!("Bitwarden sync isn't implemented yet")
}

pub struct BitwardenBackend {}

impl BitwardenBackend {
    pub fn open(_config: &Config, _secret: Option<String>) -> Result<BitwardenBackend> {
        anyhow::bail!("Bitwarden sync isn't implemented yet")
    }
}

impl Backend for BitwardenBackend {
    fn cursor(&mut self) -> Result<Option<String>> {
        unimplemented!()
    }
    fn pull(&mut self) -> Result<Snapshot> {
        unimplemented!()
    }
    fn create_folder(&mut self, _path: &[String]) -> Result<String> {
        unimplemented!()
    }
    fn rename_folder(&mut self, _remote_id: &str, _old: &[String], _new: &[String]) -> Result<()> {
        unimplemented!()
    }
    fn delete_folder(&mut self, _remote_id: &str, _path: &[String]) -> Result<()> {
        unimplemented!()
    }
    fn save_note(&mut self, _r: Option<&str>, _b: Option<&str>, _n: &NoteData) -> Result<(String, String)> {
        unimplemented!()
    }
    fn delete_note(&mut self, _remote_id: &str) -> Result<()> {
        unimplemented!()
    }
}
