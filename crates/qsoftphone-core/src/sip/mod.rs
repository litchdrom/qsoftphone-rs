//! SIP semantics that do not need a socket.
//!
//! Everything here is a pure function over strings, which is exactly
//! why the brief's §6 unit tests can cover it.

pub mod identity;
pub mod refer;
pub mod uri;

pub use identity::{display_name_from_header, resolve_caller_display_name, resolve_dialed_number};
pub use refer::{
    Consultation, ReferRequest, TransferMode, attended_transfer, blind_transfer, refer_to_attended,
    refer_to_blind, replaces_dialog_id,
};
pub use uri::{normalise_sip_target, sip_uri_user_part};
