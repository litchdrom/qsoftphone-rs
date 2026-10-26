//! The canonical call event stream.
//!
//! Port of the C++ `CallEvent` (`src/core/call/CallEvent.h`).
//!
//! The SIP layer is the only producer. The REST API, the CRM
//! plugins, the webhook dispatcher and the UI all subscribe here and
//! never to the SIP layer directly. That is the contract that lets a
//! new trigger be added without touching the SIP code.

use serde::{Deserialize, Serialize};
use time::format_description::well_known::Rfc3339;

use crate::model::account::HeaderMap;

/// The kind of event.
///
/// The serialised names match the C++ `CallEvent::typeName()` strings
/// exactly — they are part of the webhook / REST contract and must
/// not be renamed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CallEventType {
    #[serde(rename = "registration_state_changed")]
    RegistrationStateChanged,
    #[serde(rename = "call_incoming")]
    IncomingCall,
    #[serde(rename = "call_outgoing_initiated")]
    OutgoingCallInitiated,
    #[serde(rename = "call_ringing")]
    OutgoingCallRinging,
    #[serde(rename = "call_answered")]
    CallAnswered,
    #[serde(rename = "call_ended")]
    CallEnded,
    #[serde(rename = "dtmf")]
    DtmfReceived,
    #[serde(rename = "call_muted")]
    CallMuted,
    #[serde(rename = "call_unmuted")]
    CallUnmuted,
    #[serde(rename = "call_on_hold")]
    CallOnHold,
    #[serde(rename = "call_resumed")]
    CallResumed,
    #[serde(rename = "contact_enriched")]
    ContactEnriched,
}

impl CallEventType {
    /// The wire name, identical to the serialised form.
    pub fn as_str(self) -> &'static str {
        match self {
            CallEventType::RegistrationStateChanged => "registration_state_changed",
            CallEventType::IncomingCall => "call_incoming",
            CallEventType::OutgoingCallInitiated => "call_outgoing_initiated",
            CallEventType::OutgoingCallRinging => "call_ringing",
            CallEventType::CallAnswered => "call_answered",
            CallEventType::CallEnded => "call_ended",
            CallEventType::DtmfReceived => "dtmf",
            CallEventType::CallMuted => "call_muted",
            CallEventType::CallUnmuted => "call_unmuted",
            CallEventType::CallOnHold => "call_on_hold",
            CallEventType::CallResumed => "call_resumed",
            CallEventType::ContactEnriched => "contact_enriched",
        }
    }
}

/// Which way the call was going.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Direction {
    Incoming,
    Outgoing,
}

impl Direction {
    pub fn as_str(self) -> &'static str {
        match self {
            Direction::Incoming => "incoming",
            Direction::Outgoing => "outgoing",
        }
    }
}

/// The event itself.
///
/// Field names and the skip rules match the C++ `toJson()` exactly:
/// the serialised names are snake_case (`account_id`, `call_id`,
/// `dialed_number`, ...) because they are part of the REST / webhook
/// contract, and empty fields are omitted rather than serialised as
/// blanks, so a webhook payload only carries what is actually known.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CallEvent {
    /// The event kind.
    #[serde(rename = "type")]
    pub event_type: CallEventType,
    /// ISO-8601 timestamp, UTC.
    pub timestamp_iso: String,
    /// Epoch seconds.
    pub timestamp_unix: i64,
    /// Who the event is for — the local user / account.
    pub user: String,

    // -- Call context (any of these may be empty) --
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub call_id: String,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub account_id: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub call_direction: Option<Direction>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub caller: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub caller_name: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub callee: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub call_state: String,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub sip_code: i32,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub hangup_cause: String,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub duration_sec: i64,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub digit: String,

    // -- Registration context (only on RegistrationStateChanged) --
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub reg_state: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub reg_reason: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub reg_server: String,

    // -- Configurable SIP headers --
    ///
    /// Only headers on the account's capture allowlist end up here.
    /// The SIP layer captures a bounded set at INVITE time and the
    /// account config decides which of them may travel any further.
    #[serde(default, skip_serializing_if = "HeaderMap::is_empty")]
    pub headers: HeaderMap,

    // -- Resolved identity --
    ///
    /// `caller_id` is the display name to show for the remote party.
    /// `dialed_number` is the number the remote party actually
    /// dialled — never the local AOR.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub caller_id: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub dialed_number: String,

    // -- Plugin-provided enrichment (ContactEnriched only) --
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub contact: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub contact_source: String,
    #[serde(default, skip_serializing_if = "is_false")]
    pub contact_found: bool,
}

impl CallEvent {
    /// A new event, stamped with the current UTC time.
    pub fn new(event_type: CallEventType) -> Self {
        let now = time::OffsetDateTime::now_utc();
        Self {
            event_type,
            timestamp_iso: now.format(&Rfc3339).unwrap_or_default(),
            timestamp_unix: now.unix_timestamp(),
            user: String::new(),
            call_id: String::new(),
            account_id: 0,
            call_direction: None,
            caller: String::new(),
            caller_name: String::new(),
            callee: String::new(),
            call_state: String::new(),
            sip_code: 0,
            hangup_cause: String::new(),
            duration_sec: 0,
            digit: String::new(),
            reg_state: String::new(),
            reg_reason: String::new(),
            reg_server: String::new(),
            headers: HeaderMap::new(),
            caller_id: String::new(),
            dialed_number: String::new(),
            contact: None,
            contact_source: String::new(),
            contact_found: false,
        }
    }
}

fn is_zero<T: Default + PartialEq>(value: &T) -> bool {
    *value == T::default()
}

fn is_false(value: &bool) -> bool {
    !*value
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn event_type_names_match_the_cpp_contract() {
        assert_eq!(CallEventType::IncomingCall.as_str(), "call_incoming");
        assert_eq!(
            CallEventType::OutgoingCallInitiated.as_str(),
            "call_outgoing_initiated"
        );
        assert_eq!(CallEventType::OutgoingCallRinging.as_str(), "call_ringing");
        assert_eq!(CallEventType::DtmfReceived.as_str(), "dtmf");
        assert_eq!(
            CallEventType::RegistrationStateChanged.as_str(),
            "registration_state_changed"
        );
        assert_eq!(CallEventType::ContactEnriched.as_str(), "contact_enriched");
    }

    #[test]
    fn serialised_type_name_is_the_wire_name() {
        let json = serde_json::to_string(&CallEventType::IncomingCall).unwrap();
        assert_eq!(json, "\"call_incoming\"");
    }

    #[test]
    fn empty_fields_are_omitted() {
        let ev = CallEvent::new(CallEventType::CallAnswered);
        let json = serde_json::to_value(&ev).unwrap();
        assert!(json.get("caller_id").is_none());
        assert!(json.get("sip_code").is_none());
        assert!(json.get("headers").is_none());
        assert!(json.get("contact").is_none());
        assert_eq!(json["type"], "call_answered");
    }

    #[test]
    fn populated_fields_survive_a_round_trip() {
        let mut ev = CallEvent::new(CallEventType::IncomingCall);
        ev.user = "104".into();
        ev.call_id = "42".into();
        ev.account_id = 7;
        ev.call_direction = Some(Direction::Incoming);
        ev.caller_id = "Alice".into();
        ev.dialed_number = "6666".into();
        ev.headers.insert("X-Ticket".into(), "T-1".into());

        let json = serde_json::to_value(&ev).unwrap();
        assert_eq!(json["user"], "104");
        assert_eq!(json["account_id"], 7);
        assert_eq!(json["call_direction"], "incoming");
        assert_eq!(json["headers"]["X-Ticket"], "T-1");

        let back: CallEvent = serde_json::from_value(json).unwrap();
        assert_eq!(back, ev);
    }

    #[test]
    fn a_new_event_is_timestamped() {
        let ev = CallEvent::new(CallEventType::CallEnded);
        assert!(ev.timestamp_unix > 1_600_000_000);
        assert!(ev.timestamp_iso.contains('T'));
    }
}
