//! Incoming requests — the keepalives the registrar sends us.
//!
//! Asterisk pings a registered endpoint with SIP OPTIONS. A UA that
//! does not answer is marked *unreachable* by the server, and the calls
//! that follow fail even though the registration itself looks perfect.
//! That is the whole symptom: the registration succeeds and nothing
//! else works.
//!
//! rsipstack hands every incoming request that has no matching
//! transaction to the endpoint's `incoming_transactions()` channel.
//! Nothing was reading that channel, so every OPTIONS was dropped and
//! the server timed out waiting for an answer.
//!
//! This module owns that channel and answers what it can:
//!
//! * **OPTIONS** → `200 OK`. The keepalive.
//! * **anything else** → `501 Not Implemented`, so the peer gets an
//!   answer instead of a timeout. INVITE is the one that matters, and
//!   M2 takes it over.

use rsipstack::platform::CancellationToken;
use rsipstack::sip::{
    Header, Headers, Method, Request, Response, StatusCode, ToTypedHeader, Version,
};
use rsipstack::transaction::{TransactionReceiver, make_tag};
use tracing::{info, warn};

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

/// Answer the requests that arrive on the endpoint, until the token is
/// cancelled.
pub async fn serve(mut incoming: TransactionReceiver, cancelled: CancellationToken) {
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

        let response = match request.method {
            Method::Options => {
                info!("answered an OPTIONS keepalive");
                make_response(&request, StatusCode::OK)
            }
            method => {
                // The caller gets an answer rather than a timeout.
                // M2 takes over INVITE.
                warn!(%method, "the method is not implemented yet");
                make_response(&request, StatusCode::NotImplemented)
            }
        };

        if let Err(e) = transaction.respond(response).await {
            warn!(error = %e, "could not answer the request");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rsipstack::sip::Uri;

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
}
