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
//! * [`incoming`] — answering the requests that arrive on that
//!   endpoint, the registrar's OPTIONS keepalive above all.
//! * [`inspector`] — the message inspector: the digest patch, and the
//!   public address the Contact has to name.
//! * [`call`] — placing, answering and ending calls.
//! * [`registrar`] — the REGISTER flow and the refresh loop.
//! * [`sdp`] — the SDP placeholder and the DTMF encoding.
//! * [`session`] — one account's endpoint and the dialogs on it.
//! * [`state`] — what the UI needs to know about an account's
//!   registration.
//! * [`transfer`] — §5.6: the progress of a transfer, and the
//!   consultation an attended transfer points at.
//!
//! The behavioural reference for all of it is the C++ implementation
//! in `../qsoftphone`.

pub mod blf;
pub mod call;
pub mod digest;
pub mod endpoint;
pub mod incoming;
pub mod inspector;
pub mod registrar;
pub mod sdp;
pub mod session;
pub mod state;
pub mod transfer;

pub use blf::{BlfError, BlfEvent, BlfLamp, BlfManager};
pub use call::{CallBus, CallError, CallManager, CallUpdate};
pub use digest::DigestInterop;
pub use endpoint::{USER_AGENT, build_endpoint, split_host_port};
pub use inspector::{PublicAddress, SessionInspector};
pub use registrar::{RegistrationManager, RegistrationTask};
pub use sdp::{answer_sdp, choose_format, dtmf_relay_body, offer_sdp, offered_formats};
pub use session::{AccountSession, SessionRegistry};
pub use state::{RegistrationEvent, RegistrationState};
pub use transfer::{TransferUpdate, consultation_of, refer_progress};

// The two types an application needs to build a session of its own.
// They are re-exported so the app does not have to depend on the SIP
// stack's own dependency to make one.
pub use rsipstack::platform::CancellationToken;
pub use rsipstack::sip::Transport;
