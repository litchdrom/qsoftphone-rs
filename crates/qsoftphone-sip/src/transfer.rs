//! §5.6 — call transfer over SIP REFER.
//!
//! Two things live here, and both of them are about **reading** the
//! transfer rather than starting it:
//!
//! * **what the peer said** — the progress a NOTIFY carries (RFC 3515
//!   §3.4), turned into something the interface can show;
//! * **what the consultation is** — the mapping from a live dialog to
//!   the `Replaces` identifier an attended transfer has to carry.
//!
//! The REFER itself is sent from [`crate::call`], where the live
//! dialogs live.

use rsipstack::dialog::invite_dialog::InviteDialog;
use rsipstack::sip::{Header, Request};

use qsoftphone_core::sip::refer::{Consultation, TransferProgress, progress_of};

/// What a NOTIFY said about the transfer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransferUpdate {
    /// How far the transfer has got.
    pub progress: TransferProgress,
    /// The status code the peer reported for the referred call.
    ///
    /// This is the code of the **referred** call, out of the NOTIFY's
    /// `message/sipfrag` body: `100` while the new leg is coming up,
    /// `200` when it is answered, a `4xx` or `5xx` when it fails.
    pub code: u16,
    /// The reason the peer gave, when it gave one.
    ///
    /// The `Subscription-State` reason: `noresource` (the peer could
    /// not route to the target), `timeout`, `deactivated`, and so on.
    pub reason: String,
}

/// The progress a NOTIFY carries — `None` when the NOTIFY is not about
/// a refer.
///
/// RFC 3515 §3.4: the progress arrives as a NOTIFY on the refer
/// subscription, with `Event: refer` and a `message/sipfrag` body that
/// carries the referred call's status line.
///
/// The body carries the code; the `Subscription-State` header carries
/// whether the subscription is still alive and why it ended.
///
/// rsipstack has a `ReferStatus::parse` for exactly this, but it only
/// looks for the `Event` and `Subscription-State` headers among the
/// *unknown* ones — and the stack's own parser knows both of them, so
/// it never finds them and the parse always comes back empty. This
/// reads the typed headers the stack actually produces.
pub fn refer_progress(request: &Request) -> Option<TransferUpdate> {
    if !is_refer_event(request) {
        return None;
    }

    let reason = subscription_reason(request);
    let code = status_code_of(&request.body);

    // The subscription ended without a resource being created: the
    // peer could not route to the target, and no call was ever made.
    //
    // This outranks the status line. A `100 Trying` in the body says
    // the transfer *started*; the reason says that it did not get
    // anywhere, and a transfer that did not get anywhere is a
    // transfer that failed.
    if reason == "noresource" {
        return Some(TransferUpdate {
            progress: TransferProgress::Failed,
            code: code.unwrap_or(0),
            reason,
        });
    }

    match code {
        Some(code) => Some(TransferUpdate {
            progress: progress_of(code),
            code,
            reason,
        }),
        // A NOTIFY with no status line still says something: the
        // subscription is over. The transfer is over, and it did not
        // work.
        None if subscription_terminated(request) => Some(TransferUpdate {
            progress: TransferProgress::Failed,
            code: 0,
            reason,
        }),
        None => None,
    }
}

/// Whether the request is a NOTIFY about the refer subscription.
fn is_refer_event(request: &Request) -> bool {
    request.headers.iter().any(|header| match header {
        Header::Event(event) => event.value().to_ascii_lowercase().contains("refer"),
        Header::Other(name, value) => {
            name.eq_ignore_ascii_case("event") && value.to_ascii_lowercase().contains("refer")
        }
        _ => false,
    })
}

/// The status code in a `message/sipfrag` body.
///
/// RFC 3515 §3.4.1: the body is one or more SIP status lines. The
/// **last** one is the one that counts — a body that carries `100
/// Trying` and then `200 OK` is reporting progress, and the progress
/// is the 200.
fn status_code_of(body: &[u8]) -> Option<u16> {
    let text = std::str::from_utf8(body).ok()?;
    text.lines()
        .filter_map(|line| {
            let rest = line.trim().strip_prefix("SIP/2.0")?;
            rest.split_whitespace().next()?.parse::<u16>().ok()
        })
        .next_back()
}

/// The `Subscription-State` header's value.
fn subscription_state_of(request: &Request) -> Option<String> {
    request.headers.iter().find_map(|header| match header {
        Header::SubscriptionState(state) => Some(state.value().to_string()),
        Header::Other(name, value) if name.eq_ignore_ascii_case("subscription-state") => {
            Some(value.clone())
        }
        _ => None,
    })
}

/// Whether the `Subscription-State` header says the subscription is
/// over.
fn subscription_terminated(request: &Request) -> bool {
    subscription_state_of(request)
        .map(|value| {
            value
                .split(';')
                .next()
                .unwrap_or_default()
                .trim()
                .eq_ignore_ascii_case("terminated")
        })
        .unwrap_or(false)
}

/// The `Subscription-State` header's `reason` parameter, if it names
/// one.
///
/// RFC 3515 §7.1.1: `terminated;reason=noresource` means the peer
/// could not route to the target — the transfer failed before it
/// started, and no call was ever made.
fn subscription_reason(request: &Request) -> String {
    let Some(value) = subscription_state_of(request) else {
        return String::new();
    };

    value
        .split(';')
        .filter_map(|part| {
            let (key, val) = part.split_once('=')?;
            key.trim()
                .eq_ignore_ascii_case("reason")
                .then(|| val.trim().to_string())
        })
        .next()
        .unwrap_or_default()
}

/// The consultation an attended transfer points at.
///
/// The `Replaces` identifier is the consultation dialog's **Call-ID**,
/// with the `@` percent-encoded (RFC 3891 §3.1). The peer matches it
/// against the dialog it has to take over, so it has to be the very
/// Call-ID the peer saw in our INVITE — the local part and the host
/// are one string, not two, and the host is the one the Call-ID
/// carries rather than the host of our contact.
pub fn consultation_of(dialog: &InviteDialog) -> Consultation {
    let snapshot = dialog.snapshot();
    let (call_id, host) = split_call_id(&snapshot.id.call_id);
    Consultation {
        uri: snapshot.remote_uri.to_string(),
        call_id: call_id.to_string(),
        host: host.to_string(),
    }
}

/// The two halves of a SIP Call-ID: the local part and the host.
///
/// A Call-ID is `local-part@host`. A Call-ID with no `@` has no host
/// to split off, and the identifier is then the Call-ID itself.
fn split_call_id(call_id: &str) -> (&str, &str) {
    match call_id.split_once('@') {
        Some((local, host)) => (local, host),
        None => (call_id, ""),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rsipstack::sip::message::Request;
    use rsipstack::sip::{Method, Uri, Version};

    /// A NOTIFY request with the headers a refer progress carries.
    fn notify(event: Option<&str>, subscription_state: Option<&str>, body: &str) -> Request {
        let mut headers = Vec::new();
        if let Some(event) = event {
            headers.push(Header::Event(event.into()));
        }
        if let Some(state) = subscription_state {
            headers.push(Header::SubscriptionState(state.into()));
        }
        Request {
            method: Method::Notify,
            uri: "sip:peer@example.com".parse::<Uri>().expect("uri"),
            headers: headers.into(),
            body: body.as_bytes().to_vec(),
            version: Version::V2,
        }
    }

    #[test]
    fn a_notify_about_the_refer_carries_its_progress() {
        let update = refer_progress(&notify(
            Some("refer"),
            Some("active;expires=60"),
            "SIP/2.0 180 Ringing\r\n",
        ))
        .expect("the progress was not read");

        assert_eq!(update.progress, TransferProgress::Ringing);
        assert_eq!(update.code, 180);
        assert!(update.reason.is_empty());
    }

    #[test]
    fn the_last_status_line_is_the_progress() {
        // A body that reports the whole story: the last line is the
        // one that counts.
        let update = refer_progress(&notify(
            Some("refer"),
            Some("terminated"),
            "SIP/2.0 100 Trying\r\n\r\nSIP/2.0 200 OK\r\n",
        ))
        .expect("the progress was not read");

        assert_eq!(update.progress, TransferProgress::Answered);
        assert_eq!(update.code, 200);
    }

    #[test]
    fn a_failure_is_a_failure() {
        let update = refer_progress(&notify(
            Some("refer"),
            Some("terminated"),
            "SIP/2.0 503 Service Unavailable\r\n",
        ))
        .expect("the progress was not read");

        assert_eq!(update.progress, TransferProgress::Failed);
        assert_eq!(update.code, 503);
    }

    #[test]
    fn a_subscription_that_ended_without_a_resource_is_a_failure() {
        // RFC 3515 §7.1.1: `terminated;reason=noresource` means the
        // peer could not route to the target. The body may well say
        // `100 Trying` — the transfer started and got nowhere, and
        // that is a failure whatever the status line says.
        let update = refer_progress(&notify(
            Some("refer"),
            Some("terminated;reason=noresource"),
            "SIP/2.0 100 Trying\r\n",
        ))
        .expect("the progress was not read");

        assert_eq!(update.progress, TransferProgress::Failed);
        assert_eq!(update.code, 100);
        assert_eq!(update.reason, "noresource");
    }

    #[test]
    fn a_subscription_that_ended_without_a_status_line_is_a_failure() {
        // The subscription is over and there is no status line to
        // say what happened: the transfer is over, and it did not
        // work.
        let update = refer_progress(&notify(
            Some("refer"),
            Some("terminated;reason=timeout"),
            "",
        ))
        .expect("the progress was not read");

        assert_eq!(update.progress, TransferProgress::Failed);
        assert_eq!(update.code, 0);
        assert_eq!(update.reason, "timeout");
    }

    #[test]
    fn a_notify_that_is_not_about_a_refer_is_not_a_progress() {
        // A NOTIFY for something else entirely — the message-summary
        // subscription, say — must not be read as a transfer.
        assert!(
            refer_progress(&notify(
                Some("message-summary"),
                Some("active;expires=60"),
                "SIP/2.0 200 OK\r\n"
            ))
            .is_none(),
            "a NOTIFY for another event was read as a transfer"
        );
    }

    #[test]
    fn a_notify_without_a_status_line_is_not_a_progress() {
        assert!(
            refer_progress(&notify(Some("refer"), Some("active;expires=60"), "")).is_none(),
            "an empty body was read as a progress report"
        );
    }

    #[test]
    fn the_subscription_reason_is_read_out_of_the_header() {
        let update = refer_progress(&notify(
            Some("refer"),
            Some("terminated;reason=timeout;expires=0"),
            "SIP/2.0 408 Request Timeout\r\n",
        ))
        .expect("the progress was not read");

        assert_eq!(update.reason, "timeout");
    }

    #[test]
    fn a_call_id_is_split_into_its_two_halves() {
        assert_eq!(
            split_call_id("1a2b3c@example.com"),
            ("1a2b3c", "example.com")
        );
        // A Call-ID with no host has nothing to split off.
        assert_eq!(split_call_id("1a2b3c"), ("1a2b3c", ""));
    }
}
