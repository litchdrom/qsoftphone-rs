//! Incoming requests — the keepalives, and the calls.
//!
//! rsipstack hands every incoming request that has no matching
//! transaction to the endpoint's `incoming_transactions()` channel.
//! This module owns that channel and routes what arrives:
//!
//! | Request | What happens |
//! |---|---|
//! | `OPTIONS` | `200 OK`. The registrar's keepalive. |
//! | `INVITE` (no To tag) | A new call: `180 Ringing` right away (§5.4), then the call layer takes over. |
//! | anything with a To tag | An in-dialog request — the `BYE`, the DTMF `INFO`, a re-INVITE. It goes to the dialog it belongs to. |
//! | anything else | `501 Not Implemented`. A status code beats a timeout. |
//!
//! The distinction between a new call and an in-dialog request is the
//! To tag: an initial request has none, everything that follows in the
//! dialog has one.

use std::sync::Arc;

use rsipstack::platform::CancellationToken;
use rsipstack::sip::{
    Header, Headers, HeadersExt, Method, Request, Response, StatusCode, ToTypedHeader, Version,
};
use rsipstack::transaction::{TransactionReceiver, make_tag};
use tracing::{info, warn};

use crate::call::{CallBus, handle_in_dialog_request, handle_incoming_invite};
use crate::session::AccountSession;

/// Build the response to an incoming request.
///
/// RFC 3261 §8.2.6: a response copies the request's Via, From, To,
/// Call-ID and CSeq headers. §8.1.3.7 additionally requires a tag on
/// the To header of a `2xx`, so one is added when the request did not
/// carry one.
pub fn make_response(request: &Request, status: StatusCode) -> Response {
    let mut headers = Headers::default();

    for header in request.headers.iter() {
        match header {
            Header::Via(via) => headers.push(Header::Via(via.clone())),
            Header::From(from) => headers.push(Header::From(from.clone())),
            Header::To(to) => {
                let Ok(to) = to.clone().typed() else {
                    // A To header we cannot parse is not worth a
                    // malformed response — skip it.
                    continue;
                };
                let to = if to.tag().is_some() {
                    to
                } else {
                    to.with_tag(make_tag())
                };
                headers.push(Header::To(to.into()));
            }
            Header::CSeq(cseq) => headers.push(Header::CSeq(cseq.clone())),
            Header::CallId(call_id) => headers.push(Header::CallId(call_id.clone())),
            _ => {}
        }
    }

    Response {
        status_code: status,
        version: Version::V2,
        headers,
        body: Vec::new(),
        wire_reason: None,
    }
}

/// Whether a request carries a To tag — and is therefore in-dialog.
fn has_to_tag(request: &Request) -> bool {
    request
        .to_header()
        .ok()
        .and_then(|to| to.tag().ok())
        .flatten()
        .is_some()
}

/// Answer the requests that arrive on the endpoint, until the token is
/// cancelled.
pub async fn serve(
    mut incoming: TransactionReceiver,
    cancelled: CancellationToken,
    session: Arc<AccountSession>,
    calls: CallBus,
) {
    loop {
        let transaction = tokio::select! {
            _ = cancelled.cancelled() => break,
            transaction = incoming.recv() => match transaction {
                Some(transaction) => transaction,
                None => break,
            },
        };

        let mut transaction = transaction;
        let request = transaction.original.clone();

        // An in-dialog request: the BYE, the DTMF INFO, a re-INVITE.
        // It goes to the dialog it belongs to.
        if has_to_tag(&request) {
            if let Err(e) = handle_in_dialog_request(session.clone(), transaction).await {
                warn!(error = %e, "could not route an in-dialog request");
            }
            continue;
        }

        match request.method {
            Method::Invite => {
                if let Err(e) =
                    handle_incoming_invite(session.clone(), transaction, calls.clone()).await
                {
                    warn!(error = %e, "the incoming call could not be set up");
                }
            }
            Method::Options => {
                info!("answered an OPTIONS keepalive");
                let response = make_response(&request, StatusCode::OK);
                if let Err(e) = transaction.respond(response).await {
                    warn!(error = %e, "could not answer the OPTIONS");
                }
            }
            method => {
                // The caller gets an answer rather than a timeout.
                // M2 takes over INVITE.
                warn!(%method, "the method is not implemented yet");
                let response = make_response(&request, StatusCode::NotImplemented);
                if let Err(e) = transaction.respond(response).await {
                    warn!(error = %e, "could not answer the request");
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rsipstack::sip::Uri;
    use rsipstack::sip::headers::typed::To;

    fn request(method: Method) -> Request {
        Request {
            method,
            uri: Uri::try_from("sip:104@example.com").expect("uri"),
            version: Version::V2,
            headers: Headers::default(),
            body: Vec::new(),
        }
    }

    #[test]
    fn the_response_carries_the_status_code() {
        let response = make_response(&request(Method::Options), StatusCode::OK);
        assert_eq!(response.status_code, StatusCode::OK);
    }

    #[test]
    fn a_not_implemented_response_is_built_for_unknown_methods() {
        let response = make_response(&request(Method::Invite), StatusCode::NotImplemented);
        assert_eq!(response.status_code, StatusCode::NotImplemented);
    }

    #[test]
    fn a_request_without_a_to_tag_is_not_in_dialog() {
        assert!(!has_to_tag(&request(Method::Invite)));
    }

    #[test]
    fn a_request_with_a_to_tag_is_in_dialog() {
        let mut request = request(Method::Bye);
        request.headers.push(Header::To(
            To::parse("<sip:104@example.com>;tag=abc123")
                .expect("to")
                .into(),
        ));
        assert!(has_to_tag(&request));
    }
}
