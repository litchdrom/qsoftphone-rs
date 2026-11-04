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
    /// The `Subscription-State` reason: `noresource`, `timeout`,
    /// `deactivated`, and so on.
    ///
    /// **`noresource` says nothing about the transfer.** RFC 3515
    /// §2.4.7 asks for it on the final NOTIFY of a *successful*
    /// transfer, and pjsip's own state name for a terminated
    /// subscription is literally `terminated;reason=noresource` — so
    /// a server says it on every transfer that ends. The body decides
    /// whether it worked.
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
    let terminated = subscription_terminated(request);

    let Some(code) = status_code_of(&request.body) else {
        // A NOTIFY with no status line: the subscription ended, and
        // there is nothing to say how far it got. The transfer is over,
        // and it did not work.
        return terminated.then_some(TransferUpdate {
            progress: TransferProgress::Failed,
            code: 0,
            reason,
        });
    };

    // The body says how far the new leg got; the subscription says
    // whether the story is over. A story that is over and did not end
    // in an answer is a failure.
    //
    // The **reason** is not the judge. RFC 3515 §2.4.7 says the final
    // NOTIFY of a *successful* transfer is terminated with
    // `reason=noresource`, because the subscription was to the state
    // of the referred request and that state is over. pjsip's own
    // state name for a terminated subscription is the same string, so
    // a server says "noresource" on every transfer, successful or
    // not. Reading it as "no route to the target" reports every
    // successful transfer as a failure.
    let progress = match code {
        // The success code, and the only one that means the new leg
        // is up.
        200 => TransferProgress::Answered,
        // A failure, whether the subscription is over or not.
        400..=699 => TransferProgress::Failed,
        // The subscription is over and the body never said the new leg
        // was answered: the transfer did not work.
        _ if terminated => TransferProgress::Failed,
        _ => progress_of(code),
    };

    Some(TransferUpdate {
        progress,
        code,
        reason,
    })
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
/// The reasons are `noresource`, `timeout`, `deactivated` and so on.
///
/// **`noresource` is not a failure.** RFC 3515 §2.4.7 asks for it on
/// the final NOTIFY of a *successful* transfer — the subscription was
/// to the state of the referred request, and that state is over — and
/// pjsip's own state name for a terminated subscription is literally
/// `terminated;reason=noresource`. A server says it on every
/// transfer, so it cannot be the judge. The body is.
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
/// with the `@` percent-encoded, plus the dialog's two tags (RFC 3891
/// §3.1). The peer matches the dialog by all three, so the Call-ID
/// alone is not enough to find it.
///
/// The host is the one the Call-ID carries rather than the host of
/// our contact: the peer matches the whole Call-ID it saw in our
/// INVITE.
pub fn consultation_of(dialog: &InviteDialog) -> Consultation {
    let snapshot = dialog.snapshot();
    let (call_id, host) = split_call_id(&snapshot.id.call_id);
    Consultation {
        uri: snapshot.remote_uri.to_string(),
        call_id: call_id.to_string(),
        host: host.to_string(),
        // RFC 3891: the to-tag is the tag the **peer** put on its
        // `To` — our remote tag — and the from-tag is the one we put
        // on our own `From` — our local tag.
        //
        // pjsip's `xferReplaces` puts them in the same places, and
        // that is not a coincidence: the two ends of a dialog hold
        // the same two tags the other way round, so the peer looks
        // the dialog up with exactly this pair.
        to_tag: snapshot.id.remote_tag.clone(),
        from_tag: snapshot.id.local_tag.clone(),
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
    fn a_subscription_that_ended_after_a_trying_is_a_failure() {
        // The subscription is over and the body only ever said `100
        // Trying`: the transfer started and got nowhere, and that is a
        // failure. The **reason** — `noresource` here — is not what
        // decides it; the body is.
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
    fn noresource_does_not_mean_the_transfer_failed() {
        // The correction: RFC 3515 §2.4.7 asks for the final NOTIFY of
        // a **successful** transfer to be terminated with
        // `reason=noresource`, and pjsip's own state name for a
        // terminated subscription is the same string. A server says it
        // on every transfer that ends, so reading it as "no route to
        // the target" reports every successful transfer as a failure.
        let update = refer_progress(&notify(
            Some("refer"),
            Some("terminated;reason=noresource"),
            "SIP/2.0 200 OK\r\n",
        ))
        .expect("the progress was not read");

        assert_eq!(update.progress, TransferProgress::Answered);
        assert_eq!(update.code, 200);
        assert_eq!(update.reason, "noresource");
    }

    #[test]
    fn a_subscription_that_ended_after_a_ringing_is_a_failure() {
        // It rang and was never answered: the transfer did not work,
        // even though nothing in the body says so.
        let update = refer_progress(&notify(
            Some("refer"),
            Some("terminated;reason=noresource"),
            "SIP/2.0 180 Ringing\r\n",
        ))
        .expect("the progress was not read");

        assert_eq!(update.progress, TransferProgress::Failed);
        assert_eq!(update.code, 180);
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
