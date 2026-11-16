//! CleanNotes' core, shared by the desktop app (egui) and the iPhone app
//! (SwiftUI, through `cleannotes-ffi`): everything that isn't drawing.
//!
//! - [`model`], [`db`], [`settings`]: notes, folders, accounts, and storing
//!   them.
//! - [`doc`], [`markdown`]: the rich-text document notes are edited as, and
//!   the Markdown they're stored as.
//! - [`editing`]: the editing engine (typing, lists, styles, undo, find) that
//!   both apps' editors drive.
//! - [`library`]: the notes session: the list and its sections, new notes,
//!   deleting and undoing it, moving, folders, accounts, saving, syncing and
//!   locked notes.
//! - [`lock`], [`sync`]: locked notes' encryption, and the sync backends.
//! - [`attachments`]: images attached to notes.
//! - [`dates`]: how dates are shown.

pub mod attachments;
pub mod dates;
pub mod db;
pub mod doc;
pub mod editing;
pub mod library;
pub mod lock;
pub mod markdown;
pub mod model;
pub mod settings;
pub mod sync;
