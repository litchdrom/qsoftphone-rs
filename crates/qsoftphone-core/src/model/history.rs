//! Call history records.
//!
//! Port of the C++ `CallRecord` (`src/core/history/CallHistory.h`).

use serde::{Deserialize, Serialize};

use crate::sip::uri::sip_uri_user_part;

/// Which way the call was going.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Direction {
    #[default]
    Incoming,
    Outgoing,
    Missed,
}

impl Direction {
    pub fn as_str(self) -> &'static str {
        match self {
            Direction::Incoming => "incoming",
            Direction::Outgoing => "outgoing",
            Direction::Missed => "missed",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "incoming" => Some(Direction::Incoming),
            "outgoing" => Some(Direction::Outgoing),
            "missed" => Some(Direction::Missed),
            _ => None,
        }
    }
}

/// One row of call history.
///
/// The serialised names mirror the `calls` table columns.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CallRecord {
    /// Row id; zero before the row has been inserted.
    pub id: i64,
    pub account_id: i64,
    /// The SIP call id.
    pub call_id: String,
    pub direction: Direction,
    /// The *remote* number — the user-part of the remote URI. This is
    /// what the history list shows.
    pub remote_number: String,
    /// The full SIP URI as the stack saw it (caller for incoming,
    /// callee for outgoing). Hidden behind an expand affordance in
    /// the UI because most users only care about the number.
    pub remote_uri: String,
    /// The remote party's display name.
    pub remote_name: String,
    /// Epoch milliseconds.
    pub started_at: i64,
    /// Epoch milliseconds.
    pub ended_at: i64,
    pub duration_sec: i64,
    pub sip_code: i32,
    pub hangup_cause: String,
    pub answered: bool,
    pub recorded: bool,
}

impl CallRecord {
    /// Build a record from an explicit remote URI.
    ///
    /// `remote_number` is derived from the URI so the two can never
    /// drift apart.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        account_id: i64,
        call_id: impl Into<String>,
        direction: Direction,
        remote_uri: impl Into<String>,
        remote_name: impl Into<String>,
        started_at: i64,
        ended_at: i64,
        sip_code: i32,
        hangup_cause: impl Into<String>,
        answered: bool,
    ) -> Self {
        let remote_uri = remote_uri.into();
        let remote_number = sip_uri_user_part(&remote_uri);
        let duration_sec = if started_at > 0 && ended_at > started_at {
            (ended_at - started_at) / 1000
        } else {
            0
        };
        Self {
            id: 0,
            account_id,
            call_id: call_id.into(),
            direction,
            remote_number,
            remote_uri,
            remote_name: remote_name.into(),
            started_at,
            ended_at,
            duration_sec,
            sip_code,
            hangup_cause: hangup_cause.into(),
            answered,
            recorded: false,
        }
    }

    /// §5.3 — an incoming call's remote party is the **caller**.
    #[allow(clippy::too_many_arguments)]
    pub fn from_incoming(
        account_id: i64,
        call_id: impl Into<String>,
        caller_uri: impl Into<String>,
        remote_name: impl Into<String>,
        started_at: i64,
        ended_at: i64,
        sip_code: i32,
        hangup_cause: impl Into<String>,
        answered: bool,
    ) -> Self {
        Self::new(
            account_id,
            call_id,
            Direction::Incoming,
            caller_uri,
            remote_name,
            started_at,
            ended_at,
            sip_code,
            hangup_cause,
            answered,
        )
    }

    /// §5.3 — an outgoing call's remote party is the **callee**.
    #[allow(clippy::too_many_arguments)]
    pub fn from_outgoing(
        account_id: i64,
        call_id: impl Into<String>,
        callee_uri: impl Into<String>,
        remote_name: impl Into<String>,
        started_at: i64,
        ended_at: i64,
        sip_code: i32,
        hangup_cause: impl Into<String>,
        answered: bool,
    ) -> Self {
        Self::new(
            account_id,
            call_id,
            Direction::Outgoing,
            callee_uri,
            remote_name,
            started_at,
            ended_at,
            sip_code,
            hangup_cause,
            answered,
        )
    }

    /// A missed call is an incoming call that was never answered, so
    /// the remote party is the caller.
    pub fn missed(
        account_id: i64,
        call_id: impl Into<String>,
        caller_uri: impl Into<String>,
        remote_name: impl Into<String>,
        started_at: i64,
    ) -> Self {
        Self::new(
            account_id,
            call_id,
            Direction::Missed,
            caller_uri,
            remote_name,
            started_at,
            0,
            0,
            "",
            false,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn incoming_history_stores_the_caller_not_the_local_aor() {
        // §5.3. The PBX forwards sip:6666@host to our AOR
        // sip:104@host. The history row has to show 6666.
        let record = CallRecord::from_incoming(
            1,
            "call-1",
            "sip:6666@193.178.169.164",
            "Alice",
            1_000,
            61_000,
            200,
            "Normal",
            true,
        );
        assert_eq!(record.remote_number, "6666");
        assert_eq!(record.remote_uri, "sip:6666@193.178.169.164");
        assert_ne!(record.remote_number, "104");
    }

    #[test]
    fn outgoing_history_stores_the_callee() {
        let record = CallRecord::from_outgoing(
            1,
            "call-2",
            "sip:6666@193.178.169.164",
            "",
            1_000,
            31_000,
            200,
            "Normal",
            true,
        );
        assert_eq!(record.remote_number, "6666");
        assert_eq!(record.remote_uri, "sip:6666@193.178.169.164");
    }

    #[test]
    fn a_missed_call_is_an_incoming_call() {
        let record = CallRecord::missed(1, "call-3", "sip:6666@host", "Alice", 1_000);
        assert_eq!(record.direction, Direction::Missed);
        assert_eq!(record.remote_number, "6666");
        assert!(!record.answered);
        assert_eq!(record.duration_sec, 0);
    }

    #[test]
    fn the_remote_number_is_never_the_angle_bracket_junk() {
        let record =
            CallRecord::from_incoming(1, "call-4", "<sip:6666@host>", "", 0, 0, 0, "", false);
        assert_eq!(record.remote_number, "6666");
    }

    #[test]
    fn duration_is_derived_from_the_timestamps() {
        let record = CallRecord::from_incoming(
            1,
            "call-5",
            "sip:6666@host",
            "",
            1_000,
            61_500,
            200,
            "",
            true,
        );
        assert_eq!(record.duration_sec, 60);
    }

    #[test]
    fn direction_round_trips_through_its_wire_name() {
        for d in [Direction::Incoming, Direction::Outgoing, Direction::Missed] {
            assert_eq!(Direction::parse(d.as_str()), Some(d));
        }
        assert_eq!(Direction::parse("nonsense"), None);
    }
}
