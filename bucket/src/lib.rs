//! What CleanNotes and CleanPassword share for keeping things in an S3
//! bucket of the user's own:
//!
//! - [`s3`]: a small S3 client (signed GET, HEAD, conditional PUT, DELETE,
//!   listing) over a [`s3::Transport`] the app supplies. It has no threads
//!   or blocking I/O of its own, so it builds for WebAssembly (CleanPassword's
//!   Chrome extension, on `fetch`) as well as for the desktop and iOS
//!   (a native HTTP client, driven with [`block_on`]).
//! - [`seal`]: XChaCha20-Poly1305 with a random nonce, bound to associated
//!   data naming what's sealed; keys wiped when dropped.
//! - [`notes`]: a CleanNotes account's folder (note format, encryption,
//!   and an async client), for CleanNotes' sync and for CleanTabs, which
//!   saves browser tabs as notes there.
//! - [`kdf`]: Argon2id keys from passwords, with the costs both apps use
//!   and the range a stored setting may ask for.
//! - [`s3::Client::touch`] / [`s3::Client::head`]: a small object rewritten
//!   after every write, so a device with nothing to send can tell with one
//!   `HEAD` whether anything changed, instead of listing the folder.
//!
//! The crate lives in the CleanNotes repository; CleanPassword uses it as a
//! git dependency pinned to a revision.

pub mod kdf;
pub mod notes;
pub mod s3;
pub mod seal;

#[cfg(any(test, feature = "fake"))]
pub mod fake;

use std::future::Future;
use std::pin::pin;
use std::task::{Context, Poll, RawWaker, RawWakerVTable, Waker};

/// Something that couldn't be done, with a message fit to show.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Error {
    /// The request didn't get an answer.
    Network(String),
    /// S3 answered with an error.
    S3(s3::S3Error),
    Invalid(String),
}

impl Error {
    pub fn invalid(message: impl Into<String>) -> Error {
        Error::Invalid(message.into())
    }
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Network(e) => write!(f, "couldn't reach S3: {e}"),
            Error::S3(e) => e.fmt(f),
            Error::Invalid(e) => f.write_str(e),
        }
    }
}

impl std::error::Error for Error {}

impl From<s3::S3Error> for Error {
    fn from(e: s3::S3Error) -> Error {
        Error::S3(e)
    }
}

pub type Result<T, E = Error> = std::result::Result<T, E>;

/// Runs a future to completion on this thread, polling it until it's done:
/// for transports that block (a desktop or iOS HTTP client), whose futures
/// are ready the first time they're polled. Not for futures that wait on
/// something else to wake them (it would spin).
pub fn block_on<F: Future>(f: F) -> F::Output {
    fn clone(p: *const ()) -> RawWaker {
        RawWaker::new(p, &VTABLE)
    }
    fn noop(_: *const ()) {}
    static VTABLE: RawWakerVTable = RawWakerVTable::new(clone, noop, noop, noop);
    // SAFETY: the vtable's functions do nothing with the (null) pointer.
    let waker = unsafe { Waker::from_raw(RawWaker::new(std::ptr::null(), &VTABLE)) };
    let mut cx = Context::from_waker(&waker);
    let mut f = pin!(f);
    loop {
        if let Poll::Ready(v) = f.as_mut().poll(&mut cx) {
            return v;
        }
    }
}
