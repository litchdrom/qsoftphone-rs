//! Call history records.
//!
//! Port of the C++ `CallRecord` (`src/core/history/CallHistory.h`).

use serde::{Deserialize, Serialize};
use time::format_description::BorrowedFormatItem;
use time::macros::format_description;
use time::{OffsetDateTime, UtcOffset};

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
    /// The account the call belongs to — the account's UUID.
    ///
    /// The C++ stores pjsip's integer account id here. This app has
    /// no pjsip and its accounts are UUIDs, so the column is text.
    pub account_id: String,
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
    /// The number the remote party actually dialled (§5.1).
    ///
    /// For an incoming call this is resolved from the account's
    /// dialled-number header, then the Request-URI, then the To-URI —
    /// and the To-URI is usually our own extension, which is useless
    /// for a redial. It is not necessarily the same as
    /// `remote_number`: when a PBX does DID routing it forwards
    /// `sip:6666@host` to our AOR `sip:104@host`, so the peer may
    /// identify itself as `asterisk@host` while the number the customer
    /// dialled was 6666.
    pub dialed_number: String,
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
        account_id: impl Into<String>,
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
            account_id: account_id.into(),
            call_id: call_id.into(),
            direction,
            remote_number,
            remote_uri,
            remote_name: remote_name.into(),
            dialed_number: String::new(),
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
    ///
    /// `dialed_number` is the resolved dialled number (§5.1) — the
    /// number the customer actually dialled, which for a DID-forwarded
    /// call is *not* the To-URI.
    #[allow(clippy::too_many_arguments)]
    pub fn from_incoming(
        account_id: impl Into<String>,
        call_id: impl Into<String>,
        caller_uri: impl Into<String>,
        dialed_number: impl Into<String>,
        remote_name: impl Into<String>,
        started_at: i64,
        ended_at: i64,
        sip_code: i32,
        hangup_cause: impl Into<String>,
        answered: bool,
    ) -> Self {
        let mut record = Self::new(
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
        );
        record.dialed_number = dialed_number.into();
        record
    }

    /// §5.3 — an outgoing call's remote party is the **callee**.
    #[allow(clippy::too_many_arguments)]
    pub fn from_outgoing(
        account_id: impl Into<String>,
        call_id: impl Into<String>,
        callee_uri: impl Into<String>,
        remote_name: impl Into<String>,
        started_at: i64,
        ended_at: i64,
        sip_code: i32,
        hangup_cause: impl Into<String>,
        answered: bool,
    ) -> Self {
        let mut record = Self::new(
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
        );
        // We dialled the callee, so the dialled number is its
        // user-part.
        record.dialed_number = sip_uri_user_part(&record.remote_uri);
        record
    }

    /// A missed call is an incoming call that was never answered, so
    /// the remote party is the caller.
    pub fn missed(
        account_id: impl Into<String>,
        call_id: impl Into<String>,
        caller_uri: impl Into<String>,
        dialed_number: impl Into<String>,
        remote_name: impl Into<String>,
        started_at: i64,
    ) -> Self {
        let mut record = Self::new(
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
        );
        record.dialed_number = dialed_number.into();
        record
    }

    /// The number a redial must use — §5.1.
    ///
    /// `our_number` is the extension of the account the call was on.
    ///
    /// For an **outgoing** call it is the **dialled** number: when a PBX
    /// does DID routing, the remote party may be the PBX that forwarded
    /// the call, and dialling the PBX back is not what the user means by
    /// "call them back". The remote number is the fallback, for a row
    /// that has no dialled number at all.
    ///
    /// For an **incoming** call the resolved dialled number is what the
    /// caller dialled to reach us — but that is usually *our own*
    /// extension, and dialling it would call ourselves. So when the
    /// dialled number is our own number the caller is the target
    /// instead: a call from 6661 to 104 has to be called back on 6661.
    ///
    /// The C++ settled the same way: the history shows the peer, and
    /// redial dials the peer.
    pub fn redial_target(&self, our_number: &str) -> &str {
        match self.direction {
            Direction::Outgoing => {
                if self.dialed_number.is_empty() {
                    &self.remote_number
                } else {
                    &self.dialed_number
                }
            }
            Direction::Incoming | Direction::Missed => {
                if self.dialed_number.is_empty() || self.dialed_number == our_number {
                    &self.remote_number
                } else {
                    &self.dialed_number
                }
            }
        }
    }
}

/// The wall-clock time of a call: `03:12`.
const TIME_FORMAT: &[BorrowedFormatItem<'_>] = format_description!("[hour]:[minute]");

/// When a call happened: `28 Oct 2026, 03:12`.
const STAMP_FORMAT: &[BorrowedFormatItem<'_>] =
    format_description!("[day] [month repr:short] [year], [hour]:[minute]");

/// The local time, for the history's timestamps.
///
/// The offset is resolved once and reused: asking the operating system
/// for every row of every frame would be silly. When the system will
/// not say — a locked-down environment, a container with no timezone —
/// UTC is used, which is at least unambiguous.
///
/// The C++ shows local time (`QDateTime::toString`), so this does too:
/// a call history three hours out is a bug report waiting to happen.
#[derive(Debug, Clone, Copy)]
pub struct LocalTime {
    offset: UtcOffset,
}

impl Default for LocalTime {
    fn default() -> Self {
        Self {
            offset: UtcOffset::current_local_offset().unwrap_or(UtcOffset::UTC),
        }
    }
}

impl LocalTime {
    /// The time of day: `03:12`.
    pub fn time(&self, epoch_ms: i64) -> String {
        self.at(epoch_ms).format(TIME_FORMAT).unwrap_or_default()
    }

    /// The date and the time of day: `28 Oct 2026, 03:12`.
    pub fn stamp(&self, epoch_ms: i64) -> String {
        self.at(epoch_ms).format(STAMP_FORMAT).unwrap_or_default()
    }

    fn at(&self, epoch_ms: i64) -> OffsetDateTime {
        OffsetDateTime::from_unix_timestamp_nanos(epoch_ms as i128 * 1_000_000)
            .unwrap_or(OffsetDateTime::UNIX_EPOCH)
            .to_offset(self.offset)
    }
}

/// How long a call lasted: `0:42`, or `1:02:03` for the long ones.
///
/// A call with no duration shows as `—`, the way the C++'s dialer does
/// for a missed call.
pub fn format_duration(seconds: i64) -> String {
    if seconds <= 0 {
        return "—".to_string();
    }
    let (hours, minutes, secs) = (seconds / 3600, (seconds % 3600) / 60, seconds % 60);
    if hours > 0 {
        format!("{hours}:{minutes:02}:{secs:02}")
    } else {
        format!("{minutes}:{secs:02}")
    }
}

/// How a finished call ended.
///
/// The C++ computes this from the record with
/// `HistoryModel::classifyResult`; the names are the same so the
/// badge in the history list reads the same.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CallResult {
    /// The call was answered and lasted.
    Completed,
    /// An incoming call that was never answered.
    Missed,
    /// The local side hung up before the call was answered.
    Cancelled,
    /// The other side was busy.
    Busy,
    /// The other side did not answer.
    NoAnswer,
    /// The other side refused the call.
    Rejected,
    /// Anything else.
    Failed,
}

impl CallResult {
    /// The short label the history list shows.
    pub fn label(self) -> &'static str {
        match self {
            CallResult::Completed => "OK",
            CallResult::Missed => "Missed",
            CallResult::Cancelled => "Cancelled",
            CallResult::Busy => "Busy",
            CallResult::NoAnswer => "No answer",
            CallResult::Rejected => "Rejected",
            CallResult::Failed => "Failed",
        }
    }

    /// The colour the history list shows the result in: red, green and
    /// blue.
    ///
    /// The legend above the list uses this too, so the key and the rows
    /// cannot drift apart. It is plain RGB rather than a window's
    /// colour type, because this crate has no window in it.
    pub fn color(self) -> (u8, u8, u8) {
        match self {
            CallResult::Completed => (61, 220, 151),
            CallResult::Missed => (231, 76, 60),
            CallResult::Busy | CallResult::Rejected | CallResult::Failed => (230, 162, 60),
            CallResult::Cancelled | CallResult::NoAnswer => (140, 140, 140),
        }
    }
}

/// Classify a finished call — the port of the C++'s
/// `HistoryModel::classifyResult`.
///
/// The order matters and is the C++'s: a missed call is missed
/// whatever else is true of it, an answered call that lasted is
/// completed, and everything else is read off the SIP status code.
pub fn classify_result(
    direction: Direction,
    sip_code: i32,
    duration_sec: i64,
    answered: bool,
    hangup_cause: &str,
) -> CallResult {
    if direction == Direction::Missed {
        return CallResult::Missed;
    }
    if answered && duration_sec > 0 {
        return CallResult::Completed;
    }
    // A local hangup before the answer → cancelled.
    let cause = hangup_cause.to_ascii_lowercase();
    if !answered
        && duration_sec == 0
        && (cause.contains("user") || cause.contains("local") || sip_code == 0)
    {
        return CallResult::Cancelled;
    }
    // 4xx / 5xx / 6xx — a failure of some kind.
    if sip_code >= 400 {
        if sip_code == 486 {
            return CallResult::Busy;
        }
        if sip_code == 487 {
            return CallResult::Cancelled;
        }
        if sip_code == 408 {
            return CallResult::NoAnswer;
        }
        if sip_code == 480 || sip_code == 486 {
            return CallResult::Busy;
        }
        if sip_code == 401 || sip_code == 403 || sip_code == 407 {
            return CallResult::Rejected;
        }
        if sip_code == 603 {
            return CallResult::Rejected;
        }
        return CallResult::Failed;
    }
    // An outgoing call that never connected and never got a code.
    if direction == Direction::Outgoing && !answered {
        return CallResult::Failed;
    }
    // Completed-ish but no duration — a local hangup right after the
    // answer.
    CallResult::Cancelled
}

/// The direction a finished call is stored under.
///
/// An incoming call that was never answered is **missed** — that is
/// what the "missed" chip in the history list counts, and what the
/// missed-call badge is for.
///
/// A call **we** declined is not missed: the user was there and said
/// no. The decline leaves a `603` on the call, so that is how the two
/// are told apart.
///
/// The C++ has the capability — `CallHistory::recordMissed` — but
/// never calls it, so its unanswered incoming calls are stored as
/// plain "incoming" and classify as "cancelled".
pub fn history_direction(direction: Direction, answered: bool, sip_code: i32) -> Direction {
    if direction == Direction::Incoming && !answered && sip_code != 603 {
        return Direction::Missed;
    }
    direction
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn incoming_history_stores_the_caller_not_the_local_aor() {
        // §5.3. The PBX forwards sip:6666@host to our AOR
        // sip:104@host. The history row has to show 6666.
        let record = CallRecord::from_incoming(
            "acct-1",
            "call-1",
            "sip:6666@193.178.169.164",
            "6666",
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
    fn the_dialed_number_is_kept_separately_from_the_peer() {
        // §5.1 + §5.3. A PBX forwards a DID call: the peer identifies
        // itself as asterisk, but the customer dialled 6666. History
        // stores both, and redial uses the dialled number.
        let record = CallRecord::from_incoming(
            "acct-1",
            "call-did",
            "sip:asterisk@193.178.169.164",
            "6666",
            "Alice",
            0,
            0,
            0,
            "",
            false,
        );
        assert_eq!(record.remote_number, "asterisk");
        assert_eq!(record.dialed_number, "6666");
        assert_ne!(record.dialed_number, "104");
    }

    #[test]
    fn outgoing_history_stores_the_callee() {
        let record = CallRecord::from_outgoing(
            "acct-1",
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
        // We dialled the callee, so the dialled number is its
        // user-part.
        assert_eq!(record.dialed_number, "6666");
    }

    #[test]
    fn a_missed_call_is_an_incoming_call() {
        let record =
            CallRecord::missed("acct-1", "call-3", "sip:6666@host", "6666", "Alice", 1_000);
        assert_eq!(record.direction, Direction::Missed);
        assert_eq!(record.remote_number, "6666");
        assert_eq!(record.dialed_number, "6666");
        assert!(!record.answered);
        assert_eq!(record.duration_sec, 0);
    }

    #[test]
    fn redial_uses_the_dialled_number_not_the_remote_party() {
        // §5.1. A DID-forwarded call: the remote party is the PBX
        // that forwarded it, and redialing the PBX is not what the
        // user means by "call them back".
        let record = CallRecord::from_incoming(
            "acct-1",
            "call-did",
            "sip:asterisk@193.178.169.164",
            "6666",
            "",
            1_000,
            2_000,
            200,
            "Normal",
            true,
        );
        assert_eq!(record.redial_target("104"), "6666");

        // With no dialled number at all, the remote number is the
        // fallback.
        let mut plain = record.clone();
        plain.dialed_number = String::new();
        assert_eq!(plain.redial_target("104"), "asterisk");
    }

    #[test]
    fn an_incoming_call_redials_the_caller_not_our_own_extension() {
        // The bug the user reported: a call from 6661 to 104 has a
        // dialled number of 104 — our own extension — and the redial
        // called us instead of the caller.
        let record = CallRecord::from_incoming(
            "acct-1",
            "call-in",
            "sip:6661@193.178.169.164",
            "104",
            "Alice",
            1_000,
            2_000,
            200,
            "Normal",
            true,
        );
        assert_eq!(record.remote_number, "6661");
        assert_eq!(record.dialed_number, "104");
        assert_eq!(record.redial_target("104"), "6661");

        // A missed call is an incoming call: the caller is called back.
        let missed =
            CallRecord::missed("acct-1", "call-in", "sip:6661@host", "104", "Alice", 1_000);
        assert_eq!(missed.redial_target("104"), "6661");
    }

    #[test]
    fn an_outgoing_call_redials_the_number_that_was_dialled() {
        // The other half of §5.1: what we dialled is what we dial
        // again, even when the peer that answered is the PBX.
        let mut record = CallRecord::from_outgoing(
            "acct-1",
            "call-out",
            "sip:104@193.178.169.164",
            "Alice",
            1_000,
            2_000,
            200,
            "Normal",
            true,
        );
        record.dialed_number = "6666".to_string();
        assert_eq!(record.redial_target("104"), "6666");
    }

    #[test]
    fn the_result_classification_follows_the_cpp() {
        // The port of `HistoryModel::classifyResult`, case by case.
        assert_eq!(
            classify_result(Direction::Missed, 0, 0, false, ""),
            CallResult::Missed
        );
        assert_eq!(
            classify_result(Direction::Incoming, 200, 30, true, "Normal"),
            CallResult::Completed
        );
        assert_eq!(
            classify_result(Direction::Outgoing, 0, 0, false, "USER"),
            CallResult::Cancelled
        );
        assert_eq!(
            classify_result(Direction::Outgoing, 0, 0, false, "local"),
            CallResult::Cancelled
        );
        assert_eq!(
            classify_result(Direction::Outgoing, 486, 0, false, "Busy"),
            CallResult::Busy
        );
        assert_eq!(
            classify_result(Direction::Outgoing, 480, 0, false, ""),
            CallResult::Busy
        );
        assert_eq!(
            classify_result(Direction::Outgoing, 487, 0, false, ""),
            CallResult::Cancelled
        );
        assert_eq!(
            classify_result(Direction::Outgoing, 408, 0, false, ""),
            CallResult::NoAnswer
        );
        for code in [401, 403, 407, 603] {
            assert_eq!(
                classify_result(Direction::Outgoing, code, 0, false, ""),
                CallResult::Rejected,
                "code {code}"
            );
        }
        assert_eq!(
            classify_result(Direction::Outgoing, 500, 0, false, ""),
            CallResult::Failed
        );
        assert_eq!(
            classify_result(Direction::Outgoing, 404, 0, false, ""),
            CallResult::Failed
        );
        // An outgoing call that never connected and never got a code.
        assert_eq!(
            classify_result(Direction::Outgoing, 200, 0, false, ""),
            CallResult::Failed
        );
    }

    #[test]
    fn an_unanswered_incoming_call_is_missed_but_a_decline_is_not() {
        // The caller rang, nobody picked up → missed.
        assert_eq!(
            history_direction(Direction::Incoming, false, 0),
            Direction::Missed
        );
        // We declined it — the user was there and said no.
        assert_eq!(
            history_direction(Direction::Incoming, false, 603),
            Direction::Incoming
        );
        // An answered call is never missed.
        assert_eq!(
            history_direction(Direction::Incoming, true, 200),
            Direction::Incoming
        );
        // An outgoing call is never missed.
        assert_eq!(
            history_direction(Direction::Outgoing, false, 0),
            Direction::Outgoing
        );
    }

    #[test]
    fn the_remote_number_is_never_the_angle_bracket_junk() {
        let record = CallRecord::from_incoming(
            "acct-1",
            "call-4",
            "<sip:6666@host>",
            "6666",
            "",
            0,
            0,
            0,
            "",
            false,
        );
        assert_eq!(record.remote_number, "6666");
    }

    #[test]
    fn duration_is_derived_from_the_timestamps() {
        let record = CallRecord::from_incoming(
            "acct-1",
            "call-5",
            "sip:6666@host",
            "6666",
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
