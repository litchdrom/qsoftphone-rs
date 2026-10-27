//! The SIP layer of qsoftphone-rs.
//!
//! Everything here talks to a registrar; nothing here knows about
//! pixels. The split keeps the window out of the protocol path and
//! keeps the protocol out of the window:
//!
//! * [`digest`] — RFC 7616 digest authentication, and the one
//!   interoperability gap in rsipstack that has to be patched from
//!   the outside.
//! * [`endpoint`] — building an endpoint bound to one transport.
//! * [`registrar`] — the REGISTER flow and the refresh loop.
//! * [`state`] — what the UI needs to know about an account's
//!   registration.
//!
//! The behavioural reference for all of it is the C++ implementation
//! in `../qsoftphone`.

pub mod digest;
pub mod endpoint;
pub mod registrar;
pub mod state;

pub use digest::DigestInterop;
pub use endpoint::{USER_AGENT, build_endpoint, split_host_port};
pub use registrar::{RegistrationManager, RegistrationTask};
pub use state::{RegistrationEvent, RegistrationState};
