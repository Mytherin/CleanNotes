//! Syncing notes with CloudKit (STUB: to be implemented).

use anyhow::Result;
use serde::{Deserialize, Serialize};

use super::{Backend, NoteData, Snapshot};

pub const DEFAULT_CALLBACK_PORT: u16 = 52789;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Config {
    /// The CloudKit container, e.g. `iCloud.com.example.CleanNotes`.
    pub container: String,
    /// `development` or `production`.
    pub environment: String,
    /// The container's API token (CloudKit Dashboard → API Access). Not a
    /// secret: it only identifies the app; the user's sign-in is the secret.
    pub api_token: String,
    /// The loopback port the token's sign-in callback redirects to.
    pub callback_port: u16,
}

/// Signs in with an Apple ID in the browser (blocking). Returns the secret.
pub fn sign_in(_config: &Config) -> Result<String> {
    anyhow::bail!("CloudKit sync isn't implemented yet")
}

pub struct CloudKitBackend {}

impl CloudKitBackend {
    pub fn open(_config: &Config, _secret: Option<String>) -> Result<CloudKitBackend> {
        anyhow::bail!("CloudKit sync isn't implemented yet")
    }
}

impl Backend for CloudKitBackend {
    fn cursor(&mut self) -> Result<Option<String>> {
        unimplemented!()
    }
    fn pull(&mut self) -> Result<Snapshot> {
        unimplemented!()
    }
    fn create_folder(&mut self, _path: &[String]) -> Result<String> {
        unimplemented!()
    }
    fn rename_folder(&mut self, _r: &str, _o: &[String], _n: &[String]) -> Result<()> {
        unimplemented!()
    }
    fn delete_folder(&mut self, _r: &str, _p: &[String]) -> Result<()> {
        unimplemented!()
    }
    fn save_note(&mut self, _r: Option<&str>, _b: Option<&str>, _n: &NoteData) -> Result<(String, String)> {
        unimplemented!()
    }
    fn delete_note(&mut self, _r: &str) -> Result<()> {
        unimplemented!()
    }
}
