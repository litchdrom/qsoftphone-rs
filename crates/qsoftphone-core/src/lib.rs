//! qsoftphone-core — the parts of qsoftphone-rs that need neither a
//! window, a sound card nor a network.
//!
//! The crate is deliberately split so that the SIP semantics can be
//! unit-tested without standing up a UDP socket or a GPU:
//!
//! * [`sip`] — pure functions over SIP URIs, headers and transfer
//!   parameters. This is where the bugs the brief calls out live
//!   (§5.1 dialled number, §5.2 caller display name, §5.3 history
//!   remote number, §5.6 Refer-To).
//! * [`model`] — the value types that cross the seams: the account
//!   profile, the canonical call event stream, the history record.
//! * [`storage`] — SQLite (bundled) for accounts, contacts and call
//!   history.
//! * [`config`] — the JSON settings file next to the app data.
//! * [`logging`] — tracing wired to a rotating file plus stderr.
//! * [`paths`] — per-OS config/data/log directories.
//!
//! The behavioural reference for everything in here is the existing
//! C++ implementation in `../qsoftphone`. Where this crate and the
//! C++ disagree, the C++ wins unless a comment says otherwise.

pub mod config;
pub mod error;
pub mod logging;
pub mod model;
pub mod paths;
pub mod sip;
pub mod storage;

pub use error::{Error, Result};
