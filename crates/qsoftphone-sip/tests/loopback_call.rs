//! A call, over real UDP, in both directions.
//!
//! The peer is a hand-written registrar and call agent: it answers the
//! REGISTER, rings the call, answers it, and hangs up. Nothing here
//! is a mock — the messages go over a socket.
//!
//! What is proved:
//!
//! * an outgoing INVITE gets a `180 Ringing` and a `200 OK`, and the
//!   call is reported as answered;
//! * an incoming INVITE is answered with a `180 Ringing` **before** the
//!   user has done anything (§5.4);
//! * the hangup is a plain BYE — no forced status code (§5.5);
//! * a call that is still ringing is declined with 603 (§5.5).

use std::sync::{Arc, Mutex};
use std::time::Duration;

use qsoftphone_core::model::account::Account;
use qsoftphone_core::model::call_event::CallEventType;
use qsoftphone_sip::{CallBus, CallManager, RegistrationManager, SessionRegistry};
use tokio::net::UdpSocket;

/// The address the loopback registrar says it sees the client at.
///
/// RFC 5737's documentation range: it is not routable, and it is not
/// the client's own socket address either — which is the whole point.
/// Behind a NAT the registrar sees the public address, and that is the
/// one the client has to advertise.
const PUBLIC_ADDRESS: &str = "203.0.113.9:5060";

/// A minimal SDP for the peer to answer with.
const PEER_SDP: &str = "v=0\r\n\
o=- 1 1 IN IP4 127.0.0.1\r\n\
s=-\r\n\
c=IN IP4 127.0.0.1\r\n\
t=0 0\r\n\
m=audio 4000 RTP/AVP 0\r\n\
a=rtpmap:0 PCMU/8000\r\n";

/// The peer's answer to a **hold**.
///
/// RFC 3264 §5: an offer of `a=sendonly` is answered with
/// `a=recvonly` — the peer stops sending, and the far end goes quiet.
const PEER_RECVONLY_SDP: &str = "v=0\r\n\
o=- 2 2 IN IP4 127.0.0.1\r\n\
s=-\r\n\
c=IN IP4 127.0.0.1\r\n\
t=0 0\r\n\
m=audio 4000 RTP/AVP 0\r\n\
a=rtpmap:0 PCMU/8000\r\n\
a=recvonly\r\n";

/// The offer the peer sends — the one Asterisk sends.
///
/// G.711 A-law and telephone-event. No µ-law: an answer that names
/// µ-law would be invalid, and Asterisk hangs up with a BYE right
/// after the `200 OK`.
const PEER_ALAW_OFFER: &str = "v=0\r\n\
o=- 124519 124519 IN IP4 127.0.0.1\r\n\
s=asterisk\r\n\
c=IN IP4 127.0.0.1\r\n\
t=0 0\r\n\
m=audio 50344 RTP/AVP 8 101\r\n\
a=rtpmap:8 PCMA/8000\r\n\
a=rtpmap:101 telephone-event/8000\r\n\
a=ptime:20\r\n";

/// The value of a header in a raw SIP message.
fn header_value(message: &str, name: &str) -> Option<String> {
    message.lines().find_map(|line| {
        let (key, value) = line.split_once(':')?;
        if key.trim().eq_ignore_ascii_case(name) {
            Some(value.trim().to_string())
        } else {
            None
        }
    })
}

/// A response to a request, mirroring the headers a transaction needs.
fn response(request: &str, status: &str, extra: &[(&str, &str)], body: &str) -> String {
    let mut out = format!("SIP/2.0 {status}\r\n");
    for name in ["Via", "From", "To", "Call-ID", "CSeq"] {
        if let Some(value) = header_value(request, name) {
            // A dialog-forming response needs a To tag; the peer uses
            // one tag for the whole test.
            let value = if name == "To" && request.starts_with("INVITE") && !value.contains("tag=")
            {
                format!("{value};tag={PEER_TAG}")
            } else {
                value
            };
            out.push_str(&format!("{name}: {value}\r\n"));
        }
    }
    for (name, value) in extra {
        out.push_str(&format!("{name}: {value}\r\n"));
    }
    if body.is_empty() {
        out.push_str("Content-Length: 0\r\n\r\n");
    } else {
        out.push_str("Content-Type: application/sdp\r\n");
        out.push_str(&format!("Content-Length: {}\r\n\r\n{body}", body.len()));
    }
    out
}

/// A response that says where the registrar saw the request come from.
///
/// RFC 3581: `received` is the source address the server saw and
/// `rport` the source port. Both are optional — a server that reports
/// neither is telling us that the address in the `Via` is the real one.
fn response_with_public_address(request: &str, status: &str, public: &str) -> String {
    let out = response(request, status, &[("Expires", "300")], "");
    let (host, port) = public.split_once(':').unwrap_or((public, "5060"));
    let via = header_value(request, "Via").unwrap_or_default();

    // The registrar's `rport` **replaces** the client's, it is not added
    // to it: a Via with two of them is malformed, and the first one is
    // the one that gets read. The client asks for a `rport` of its own
    // (RFC 3581) — that is the bare parameter it puts in its Via.
    let mut parts: Vec<String> = via
        .split(';')
        .map(|part| part.trim().to_string())
        .filter(|part| {
            !part.is_empty()
                && !part.eq_ignore_ascii_case("rport")
                && !part.to_ascii_lowercase().starts_with("rport=")
        })
        .collect();
    parts.push(format!("received={host}"));
    parts.push(format!("rport={port}"));
    let new_via = parts.join(";");

    out.replacen(
        &format!("Via: {via}\r\n"),
        &format!("Via: {new_via}\r\n"),
        1,
    )
}

/// What the peer saw.
///
/// The peer runs forever, so it cannot hand the observations back
/// through a channel that is closed when it returns — it shares them
/// with the test instead.
#[derive(Debug, Default)]
struct Observations {
    /// The first line of every message the peer received, in order.
    ///
    /// A message is either a request (`INVITE …`) or a response
    /// (`SIP/2.0 180 Ringing`).
    messages: Vec<String>,
    /// The full text of every message the peer received, in the same
    /// order.
    raw: Vec<String>,
    /// The full text of every answer the peer sent, in order.
    ///
    /// A test that needs to know what the peer **replied** looks here:
    /// the `observations` above are what the peer saw.
    sent: Vec<String>,
}

impl Observations {
    /// Whether the peer received a request whose method is `method`.
    fn saw_request(&self, method: &str) -> bool {
        self.messages
            .iter()
            .any(|line| !line.starts_with("SIP/") && line.starts_with(method))
    }

    /// Whether the peer received a response whose status line contains
    /// `fragment` — `180 Ringing`, say.
    fn saw_response(&self, fragment: &str) -> bool {
        self.messages
            .iter()
            .any(|line| line.starts_with("SIP/") && line.contains(fragment))
    }

    /// The full text of the first response whose status line contains
    /// `fragment`.
    fn response(&self, fragment: &str) -> Option<String> {
        let mut raw = self.raw.iter();
        self.messages
            .iter()
            .zip(raw.by_ref())
            .find(|(line, _)| line.starts_with("SIP/") && line.contains(fragment))
            .map(|(_, raw)| raw.clone())
    }

    /// The full text of the first request whose method is `method`.
    fn request(&self, method: &str) -> Option<String> {
        let mut raw = self.raw.iter();
        self.messages
            .iter()
            .zip(raw.by_ref())
            .find(|(line, _)| !line.starts_with("SIP/") && line.starts_with(method))
            .map(|(_, raw)| raw.clone())
    }

    /// The full text of every request whose method is `method`, in the
    /// order they arrived.
    ///
    /// A re-INVITE is an INVITE like any other, so a test that wants
    /// the *second* one has to look at all of them.
    fn requests(&self, method: &str) -> Vec<String> {
        let mut raw = self.raw.iter();
        self.messages
            .iter()
            .zip(raw.by_ref())
            .filter(|(line, _)| !line.starts_with("SIP/") && line.starts_with(method))
            .map(|(_, raw)| raw.clone())
            .collect()
    }
}

/// The ACK for a `200 OK` to an INVITE.
///
/// RFC 3261 §13.2.2.4: the ACK goes to the Contact of the answer, and
/// it carries the same To and From as the answer — including the tags
/// that identify the dialog.
fn ack_for(answer: &str, peer_addr: std::net::SocketAddr) -> String {
    let to = header_value(answer, "To").unwrap_or_default();
    let from = header_value(answer, "From").unwrap_or_default();
    let call_id = header_value(answer, "Call-ID").unwrap_or_default();
    let cseq = header_value(answer, "CSeq")
        .unwrap_or_default()
        .split_whitespace()
        .next()
        .unwrap_or("1")
        .to_string();

    // The Request-URI of the ACK is the Contact of the answer.
    let target = header_value(answer, "Contact")
        .map(|c| {
            c.trim()
                .trim_start_matches('<')
                .trim_end_matches('>')
                .to_string()
        })
        .unwrap_or_else(|| to.clone());

    format!(
        concat!(
            "ACK {target} SIP/2.0\r\n",
            "Via: SIP/2.0/UDP {addr};branch=z9hG4bK-peer-ack\r\n",
            "Max-Forwards: 70\r\n",
            "To: {to}\r\n",
            "From: {from}\r\n",
            "Call-ID: {call_id}\r\n",
            "CSeq: {cseq} ACK\r\n",
            "Content-Length: 0\r\n",
            "\r\n",
        ),
        target = target,
        addr = peer_addr,
        to = to,
        from = from,
        call_id = call_id,
        cseq = cseq,
    )
}

/// The identifiers of the dialog the peer is in.
///
/// An in-dialog request has to repeat the Call-ID, the From and the To
/// of the dialog, and the To tag is what makes it in-dialog at all.
#[derive(Clone, Debug, Default)]
struct PeerDialog {
    call_id: String,
    from: String,
    to: String,
    contact: String,
    cseq: u32,
}

/// The `INFO` request that carries a DTMF digit.
///
/// RFC 4733: the body is `application/dtmf-relay`.
fn dtmf_info(dialog: &PeerDialog, peer_addr: std::net::SocketAddr, digit: char) -> String {
    let target = dialog
        .contact
        .trim()
        .trim_start_matches('<')
        .trim_end_matches('>')
        .to_string();
    let body = format!("Signal={digit}\r\nDuration=100\r\n");

    format!(
        concat!(
            "INFO {target} SIP/2.0\r\n",
            "Via: SIP/2.0/UDP {addr};branch=z9hG4bK-peer-info\r\n",
            "Max-Forwards: 70\r\n",
            "To: {to}\r\n",
            "From: {from}\r\n",
            "Call-ID: {call_id}\r\n",
            "CSeq: {cseq} INFO\r\n",
            "Content-Type: application/dtmf-relay\r\n",
            "Content-Length: {len}\r\n",
            "\r\n",
            "{body}"
        ),
        target = target,
        addr = peer_addr,
        to = dialog.to,
        from = dialog.from,
        call_id = dialog.call_id,
        cseq = dialog.cseq + 1,
        len = body.len(),
        body = body,
    )
}

/// The digest challenge the peer answers an INVITE with.
///
/// RFC 7616 §3.3: a realm, a nonce, the algorithm and the quality of
/// protection. A PBX that challenges the INVITE has to be answered
/// with the account's digest — a stack with no credential treats the
/// challenge as the final response.
const PEER_CHALLENGE: &str = concat!(
    r#"Digest realm="asterisk", nonce="4b2c1f0e9a8d7c6b5a4938271605f4e3", "#,
    r#"algorithm=MD5, qop="auth""#,
);

/// The tag the peer puts on the `To` header of its answers.
const PEER_TAG: &str = "peer-tag";

/// The tag in a header value, if it has one.
///
/// `<sip:104@127.0.0.1>;tag=abc123` → `abc123`.
fn tag_of(value: &str) -> Option<String> {
    let after = value.split_once(";tag=")?.1;
    let tag = after.split(';').next().unwrap_or_default().trim();
    if tag.is_empty() {
        None
    } else {
        Some(tag.to_string())
    }
}

/// The URI in a header value, without its parameters.
///
/// `<sip:104@127.0.0.1>;tag=abc123` → `<sip:104@127.0.0.1>`.
fn uri_of(value: &str) -> String {
    match value.split_once(';') {
        Some((uri, _)) => uri.trim().to_string(),
        None => value.trim().to_string(),
    }
}

/// The `NOTIFY` that reports the progress of a transfer.
///
/// RFC 3515 §3.4: the NOTIFY carries `Event: refer` and the
/// `Subscription-State` header; the body is a `message/sipfrag` with
/// the status line of the referred call.
///
/// The branch has to be different for every NOTIFY — a branch that is
/// repeated is a retransmission, and the second progress report would
/// be thrown away as one.
fn refer_notify(
    dialog: &PeerDialog,
    peer_addr: std::net::SocketAddr,
    cseq: u32,
    status: &str,
    subscription_state: &str,
) -> String {
    let target = dialog
        .contact
        .trim()
        .trim_start_matches('<')
        .trim_end_matches('>')
        .to_string();
    let body = format!("{status}\r\n");

    format!(
        concat!(
            "NOTIFY {target} SIP/2.0\r\n",
            "Via: SIP/2.0/UDP {addr};branch=z9hG4bK-peer-notify-{cseq}\r\n",
            "Max-Forwards: 70\r\n",
            "To: {to}\r\n",
            "From: {from}\r\n",
            "Call-ID: {call_id}\r\n",
            "CSeq: {cseq} NOTIFY\r\n",
            "Event: refer\r\n",
            "Subscription-State: {state}\r\n",
            "Content-Type: message/sipfrag\r\n",
            "Content-Length: {len}\r\n",
            "\r\n",
            "{body}"
        ),
        target = target,
        addr = peer_addr,
        to = dialog.to,
        from = dialog.from,
        call_id = dialog.call_id,
        cseq = cseq,
        state = subscription_state,
        len = body.len(),
        body = body,
    )
}

/// The `CANCEL` a peer sends when the party that was called hangs up
/// while the call is still ringing.
///
/// RFC 3261 §9.1: the CANCEL has the **same** Request-URI as the
/// request it cancels, and it carries that request's `Call-ID`, `From`
/// and `To` — the dialog it belongs to. The server then answers the
/// INVITE itself with `487 Request Terminated` (§9.2), and that is
/// what tells the client the call is over.
fn cancel(request: &str, peer_addr: std::net::SocketAddr) -> String {
    let first = request.lines().next().unwrap_or_default();
    let target = first.split_whitespace().nth(1).unwrap_or("sip:peer");
    let mut out = format!("CANCEL {target} SIP/2.0\r\n");
    out.push_str(&format!(
        "Via: SIP/2.0/UDP {peer_addr};branch=z9hG4bK-peer-cancel\r\n"
    ));
    out.push_str("Max-Forwards: 70\r\n");
    for name in ["From", "To", "Call-ID"] {
        if let Some(value) = header_value(request, name) {
            out.push_str(&format!("{name}: {value}\r\n"));
        }
    }
    // The CSeq number is the one of the request being cancelled; only
    // the method changes.
    let cseq = header_value(request, "CSeq").unwrap_or_default();
    let number = cseq.split_whitespace().next().unwrap_or("1");
    out.push_str(&format!("CSeq: {number} CANCEL\r\n"));
    out.push_str("Content-Length: 0\r\n\r\n");
    out
}

/// What the peer reports about a transfer.
///
/// The peer takes the REFER and then reports the progress of the new
/// leg over NOTIFYs (RFC 3515 §3.4).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TransferScript {
    /// The new leg comes up and is answered: the transfer worked.
    Answered,
    /// The peer could not route to the target.
    NoRoute,
}

/// What the peer should do.
#[derive(Debug, Clone, Copy, Default)]
struct PeerOptions {
    /// Call the client back as soon as it registers.
    call_back: bool,
    /// Send a DTMF digit once the call is up.
    dtmf: Option<char>,
    /// Answer the first INVITE with a 401 challenge.
    challenge_invite: bool,
    /// What to report when the client sends a REFER.
    transfer: Option<TransferScript>,
    /// The address the registrar says it sees the client at.
    ///
    /// RFC 3581: the `received` and `rport` parameters of the `Via` in
    /// the answer say where the request came from. Behind a NAT that is
    /// **not** the address of the client's own socket, and it is the
    /// address the client has to advertise in its Contact.
    public_address: Option<&'static str>,
    /// What to answer a BYE with.
    ///
    /// `None` is the usual `200 OK`. A peer that does not know the
    /// dialog answers `481 Call/Transaction Does Not Exist` — which is
    /// what a PBX does after a transfer.
    bye_status: Option<&'static str>,
    /// Whether the `180 Ringing` carries the SDP.
    ///
    /// That is **early media**: the peer is sending audio before the
    /// call is answered. The ordinary `180 Ringing` carries no body at
    /// all, and there is nothing to hear — which is why the softphone
    /// makes its own ringback for it.
    early_media: bool,
    /// Whether the peer cancels the call while it is still ringing.
    ///
    /// This is what the far end hanging up during the consultation
    /// looks like on the wire: the server cancels the INVITE it is
    /// handling and answers it with `487 Request Terminated` (RFC 3261
    /// §9.2). Asterisk does exactly this when the party that was
    /// called gives up while the call is still ringing.
    cancel_the_call: bool,
    /// How long the peer holds a call in the early state, in
    /// milliseconds, before it answers it.
    ///
    /// The peer sends the `180 Ringing` and then waits. That is the
    /// window in which the client can complete a transfer while the
    /// target is still ringing — on a real call the user has the same
    /// window, between the target starting to ring and answering.
    ///
    /// `None` — the ordinary case — is no wait at all.
    ring_for: Option<u64>,
    /// How long the peer waits **before** it rings, in milliseconds.
    ///
    /// The peer's tag arrives with the `180 Ringing` (RFC 3891
    /// §3.1), so a call that has not rung yet cannot be named by a
    /// `Replaces`. A test that wants that state needs the ringing to
    /// be late — otherwise the `180` may land before the test gets to
    /// look at the call.
    ///
    /// `None` — the ordinary case — is no wait at all.
    ring_delay: Option<u64>,
    /// The state the peer reports for a watched extension.
    ///
    /// A Busy Lamp Field subscription: the peer answers the `SUBSCRIBE`
    /// and then sends a `NOTIFY` that says the extension is in this
    /// state — `confirmed`, `terminated`, `trying`.
    ///
    /// `None` — the ordinary case — is a server that does not serve the
    /// event package at all, and answers `489 Bad Event`.
    blf_state: Option<&'static str>,
}

/// A registrar and call agent on one socket.
///
/// It answers the REGISTER, and it can place a call back to whoever
/// registered.
async fn run_peer(socket: UdpSocket, observations: Arc<Mutex<Observations>>, options: PeerOptions) {
    let peer_addr = socket.local_addr().expect("peer addr");
    let mut buf = vec![0u8; 65535];
    let mut client: Option<std::net::SocketAddr> = None;
    // The dialogs the peer is in, keyed by SIP Call-ID.
    //
    // A test can have more than one call up at a time — the attended
    // transfer has two — and the in-dialog requests (the DTMF, the
    // transfer's NOTIFYs) have to go to the right one.
    let mut dialogs: std::collections::HashMap<String, PeerDialog> =
        std::collections::HashMap::new();

    loop {
        let (len, from) = socket.recv_from(&mut buf).await.expect("recv");
        let text = String::from_utf8_lossy(&buf[..len]).to_string();
        let first = text.lines().next().unwrap_or_default().to_string();
        observations
            .lock()
            .expect("observations lock")
            .messages
            .push(first.clone());
        observations
            .lock()
            .expect("observations lock")
            .raw
            .push(text.clone());

        // A response is not a request — answering it would start a
        // message storm.
        if first.starts_with("SIP/") {
            // Our answer to the peer's INVITE: it carries the To tag
            // that makes the dialog identifiable, and the Contact the
            // in-dialog requests are sent to.
            if first.contains("200 OK")
                && header_value(&text, "CSeq").is_some_and(|v| v.contains("INVITE"))
            {
                let call_id = header_value(&text, "Call-ID").unwrap_or_default();
                if let Some(dialog) = dialogs.get_mut(&call_id) {
                    dialog.to = header_value(&text, "To").unwrap_or(dialog.to.clone());
                    dialog.contact = header_value(&text, "Contact").unwrap_or_default();
                }
                let ack = ack_for(&text, peer_addr);
                socket
                    .send_to(ack.as_bytes(), from)
                    .await
                    .expect("peer ACK");

                // The dialog is established on the peer's side now, so
                // the DTMF can follow the ACK.
                if let (Some(digit), Some(dialog)) = (options.dtmf, dialogs.get(&call_id)) {
                    let info = dtmf_info(dialog, peer_addr, digit);
                    socket
                        .send_to(info.as_bytes(), from)
                        .await
                        .expect("peer INFO");
                }
            }
            continue;
        }

        let method = first
            .split_whitespace()
            .next()
            .unwrap_or_default()
            .to_string();

        match method.as_str() {
            "REGISTER" => {
                client = Some(from);
                // The answer says where the registrar sees the client
                // from, when it is not the client's own socket address.
                let message = match &options.public_address {
                    Some(public) => response_with_public_address(&text, "200 OK", public),
                    None => response(&text, "200 OK", &[("Expires", "300")], ""),
                };
                socket
                    .send_to(message.as_bytes(), from)
                    .await
                    .expect("REGISTER reply");
            }
            "SUBSCRIBE" => {
                // The Busy Lamp Field. The client is watching an
                // extension, and the server answers with the state.
                let Some(state) = options.blf_state else {
                    // A server that does not serve the event package.
                    let message = response(&text, "489 Bad Event", &[], "");
                    socket
                        .send_to(message.as_bytes(), from)
                        .await
                        .expect("SUBSCRIBE reply");
                    continue;
                };

                let accepted = subscribe_accepted(&text);
                socket
                    .send_to(accepted.as_bytes(), from)
                    .await
                    .expect("SUBSCRIBE reply");

                // The state comes on its own NOTIFY, right after the
                // answer — which is what a server does.
                let notify = blf_notify(&text, peer_addr, state);
                socket
                    .send_to(notify.as_bytes(), from)
                    .await
                    .expect("peer NOTIFY");
            }
            "INVITE" => {
                // A **re-INVITE** is a hold or an unhold: the dialog is
                // already up, so it is answered straight away — and the
                // direction in the answer is the mirror of the offer's
                // (RFC 3264 §5).
                let reinvite_call_id = header_value(&text, "Call-ID").unwrap_or_default();
                if dialogs.contains_key(&reinvite_call_id) {
                    let held = text.contains("a=sendonly");
                    let answer_sdp = if held { PEER_RECVONLY_SDP } else { PEER_SDP };
                    // The Contact has to be there: the ACK to a
                    // re-INVITE is sent to it.
                    let contact = format!("<sip:peer@{peer_addr}>");
                    let message = response(&text, "200 OK", &[("Contact", &contact)], answer_sdp);
                    observations
                        .lock()
                        .expect("observations lock")
                        .sent
                        .push(message.clone());
                    socket
                        .send_to(message.as_bytes(), from)
                        .await
                        .expect("re-INVITE reply");
                    continue;
                }

                // The challenge comes first, and only once: the client
                // has to answer it with the account's digest.
                if options.challenge_invite && header_value(&text, "Authorization").is_none() {
                    let message = response(
                        &text,
                        "401 Unauthorized",
                        &[("WWW-Authenticate", PEER_CHALLENGE)],
                        "",
                    );
                    socket
                        .send_to(message.as_bytes(), from)
                        .await
                        .expect("401 reply");
                    continue;
                }

                // The client's Contact is where the in-dialog requests
                // go — the transfer's NOTIFYs.
                //
                // The tags are the dialog's, and they are not the
                // obvious ones: RFC 3261 §8.1.1.3 says the `To` header
                // of an in-dialog request carries the **recipient's**
                // tag, so the NOTIFY puts the client's own tag on the
                // `To` and the peer's tag on the `From`. Get this
                // wrong and the in-dialog request arrives with no
                // dialog to go to.
                let client_from = header_value(&text, "From").unwrap_or_default();
                let client_tag = tag_of(&client_from).unwrap_or_default();
                let client_uri = uri_of(&client_from);
                let peer_uri = uri_of(&header_value(&text, "To").unwrap_or_default());
                let call_id = header_value(&text, "Call-ID").unwrap_or_default();
                dialogs.insert(
                    call_id.clone(),
                    PeerDialog {
                        call_id,
                        from: format!("{peer_uri};tag={PEER_TAG}"),
                        to: format!("{client_uri};tag={client_tag}"),
                        contact: header_value(&text, "Contact").unwrap_or_default(),
                        cseq: header_value(&text, "CSeq")
                            .and_then(|v| {
                                v.split_whitespace()
                                    .next()
                                    .and_then(|n| n.parse::<u32>().ok())
                            })
                            .unwrap_or(1),
                    },
                );

                // Ring first, then answer.
                //
                // The `180 Ringing` may carry the SDP: that is early
                // media, and the softphone has something to hear then.
                // The ordinary one carries no body at all.
                //
                // A peer that waits before ringing is a peer whose
                // call has **not been named** yet — the tag comes with
                // the `180`, and without it there is no dialog for a
                // `Replaces` to point at.
                if let Some(milliseconds) = options.ring_delay {
                    tokio::time::sleep(Duration::from_millis(milliseconds)).await;
                }
                let ringing = if options.early_media {
                    response(&text, "180 Ringing", &[], PEER_SDP)
                } else {
                    response(&text, "180 Ringing", &[], "")
                };
                socket.send_to(ringing.as_bytes(), from).await.expect("180");

                // The peer holds the call in the early state for a
                // while. That is the window in which the client can
                // complete a transfer while the target is still
                // ringing — on a real call the user has the same
                // window, between the target starting to ring and
                // answering.
                if let Some(milliseconds) = options.ring_for {
                    tokio::time::sleep(Duration::from_millis(milliseconds)).await;
                }

                // The far end gives up while the call is still
                // ringing: the server cancels the INVITE and answers
                // it with `487` (RFC 3261 §9.2).
                if options.cancel_the_call {
                    socket
                        .send_to(cancel(&text, peer_addr).as_bytes(), from)
                        .await
                        .expect("peer CANCEL");
                    let message = response(&text, "487 Request Terminated", &[], "");
                    socket
                        .send_to(message.as_bytes(), from)
                        .await
                        .expect("487 reply");
                    continue;
                }

                // The Contact has to name the address the peer can
                // actually be reached at — it is where the ACK and the
                // BYE are sent.
                let contact = format!("<sip:peer@{peer_addr}>");
                let message = response(&text, "200 OK", &[("Contact", &contact)], PEER_SDP);
                socket.send_to(message.as_bytes(), from).await.expect("200");
            }
            "REFER" => {
                // The transfer. RFC 3515: the peer answers `202
                // Accepted` and then reports the progress of the new
                // leg over NOTIFYs.
                let message = response(&text, "202 Accepted", &[], "");
                socket
                    .send_to(message.as_bytes(), from)
                    .await
                    .expect("REFER reply");

                let Some(script) = options.transfer else {
                    continue;
                };
                // The NOTIFYs go out on the dialog the REFER came in
                // on — that is the dialog the progress belongs to.
                let refer_call_id = header_value(&text, "Call-ID").unwrap_or_default();
                let Some(dialog) = dialogs.get(&refer_call_id) else {
                    continue;
                };

                // The NOTIFYs go out with the peer's own sequence
                // numbers, so the client can tell them apart.
                let first_cseq = dialog.cseq + 1;
                let reports: &[(&str, &str)] = match script {
                    TransferScript::Answered => &[
                        ("SIP/2.0 100 Trying", "active;expires=60"),
                        ("SIP/2.0 180 Ringing", "active;expires=60"),
                        ("SIP/2.0 200 OK", "terminated;reason=other"),
                    ],
                    TransferScript::NoRoute => {
                        &[("SIP/2.0 100 Trying", "terminated;reason=noresource")]
                    }
                };

                for (step, (status, state)) in reports.iter().enumerate() {
                    let notify =
                        refer_notify(dialog, peer_addr, first_cseq + step as u32, status, state);
                    socket
                        .send_to(notify.as_bytes(), from)
                        .await
                        .expect("peer NOTIFY");
                    // The client answers the NOTIFY before the next
                    // one goes out: a NOTIFY that arrives while the
                    // previous transaction is still open is a
                    // retransmission, not a new report.
                    tokio::time::sleep(Duration::from_millis(80)).await;
                }
            }
            "BYE" => {
                // The peer may not know the dialog. A PBX answers `481
                // Call/Transaction Does Not Exist` after a transfer,
                // because the channel is in a bridge and the dialog is
                // gone.
                let status = options.bye_status.unwrap_or("200 OK");
                let message = response(&text, status, &[], "");
                observations
                    .lock()
                    .expect("observations lock")
                    .sent
                    .push(message.clone());
                socket
                    .send_to(message.as_bytes(), from)
                    .await
                    .expect("BYE reply");
            }
            "OPTIONS" => {
                let message = response(&text, "200 OK", &[], "");
                socket
                    .send_to(message.as_bytes(), from)
                    .await
                    .expect("OPTIONS reply");
            }
            "INFO" => {
                // The DTMF relay. The peer answers it so the dialog
                // does not hang on the transaction.
                let message = response(&text, "200 OK", &[], "");
                socket
                    .send_to(message.as_bytes(), from)
                    .await
                    .expect("INFO reply");
            }
            // An ACK is not answered — it is a transaction on its own.
            "ACK" => {}
            _ => {
                let message = response(&text, "200 OK", &[], "");
                socket
                    .send_to(message.as_bytes(), from)
                    .await
                    .expect("reply");
            }
        }

        // Once the client is registered, call it back.
        if let Some(address) = client {
            let already_called = observations
                .lock()
                .expect("observations lock")
                .saw_request("INVITE");
            if method == "REGISTER" && options.call_back && !already_called {
                // The Via and the Contact both name the peer's real
                // address: our responses to this INVITE go back to it.
                //
                // The offer is the one Asterisk sends: G.711 A-law and
                // telephone-event. No µ-law — so an answer that says
                // µ-law is invalid (RFC 3264 §6), and Asterisk hangs up
                // with a BYE right after the 200 OK.
                let invite = format!(
                    concat!(
                        "INVITE sip:104@{addr} SIP/2.0\r\n",
                        "Via: SIP/2.0/UDP {addr};branch=z9hG4bK-peer-invite\r\n",
                        "Max-Forwards: 70\r\n",
                        "To: <sip:104@{addr}>\r\n",
                        "From: <sip:peer@{addr}>;tag=peer-invite\r\n",
                        "Call-ID: peer-incoming-call@{addr}\r\n",
                        "CSeq: 1 INVITE\r\n",
                        "Contact: <sip:peer@{addr}>\r\n",
                        "Content-Type: application/sdp\r\n",
                        "Content-Length: {length}\r\n",
                        "\r\n",
                        "{body}"
                    ),
                    addr = peer_addr,
                    length = PEER_ALAW_OFFER.len(),
                    body = PEER_ALAW_OFFER,
                );
                socket
                    .send_to(invite.as_bytes(), address)
                    .await
                    .expect("peer INVITE");

                dialogs.insert(
                    format!("peer-incoming-call@{peer_addr}"),
                    PeerDialog {
                        call_id: format!("peer-incoming-call@{peer_addr}"),
                        from: format!("<sip:peer@{peer_addr}>;tag=peer-invite"),
                        to: format!("<sip:104@{peer_addr}>"),
                        contact: String::new(),
                        cseq: 1,
                    },
                );
            }
        }
    }
}

/// The `202 Accepted` that says a subscription is there.
///
/// RFC 6665 §4.1: the answer forms the dialog, so it carries a To tag
/// — and the `NOTIFY`s come back on that dialog.
fn subscribe_accepted(request: &str) -> String {
    let mut out = String::from("SIP/2.0 202 Accepted\r\n");
    for name in ["Via", "From", "To", "Call-ID", "CSeq"] {
        if let Some(value) = header_value(request, name) {
            let value = if name == "To" && !value.contains("tag=") {
                format!("{value};tag={PEER_TAG}")
            } else {
                value
            };
            out.push_str(&format!("{name}: {value}\r\n"));
        }
    }
    out.push_str("Expires: 3600\r\n");
    out.push_str("Content-Length: 0\r\n\r\n");
    out
}

/// The `NOTIFY` that carries the state of a watched extension.
///
/// RFC 3261 §8.1.3.3: the request's `To` carries the **recipient's**
/// tag and its `From` the sender's, so the two ends of the dialog hold
/// the same two tags the other way round. The body is the
/// `dialog-info+xml` document of RFC 3903.
fn blf_notify(request: &str, peer_addr: std::net::SocketAddr, state: &str) -> String {
    let call_id = header_value(request, "Call-ID").unwrap_or_default();
    let client_from = header_value(request, "From").unwrap_or_default();
    let client_tag = tag_of(&client_from).unwrap_or_default();
    let client_uri = uri_of(&client_from);
    let peer_uri = uri_of(&header_value(request, "To").unwrap_or_default());

    let body = format!(
        concat!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\r\n",
            "<dialog-info xmlns=\"urn:ietf:params:xml:ns:dialog-info\" version=\"0\" ",
            "state=\"full\" entity=\"{peer_uri}\">\r\n",
            "  <dialog id=\"peer-dialog-1\" state=\"{state}\">\r\n",
            "    <local><identity display=\"Peer\"><uri>{peer_uri}</uri></identity></local>\r\n",
            "    <remote><identity display=\"Client\"><uri>{client_uri}</uri></identity></remote>\r\n",
            "  </dialog>\r\n",
            "</dialog-info>\r\n"
        ),
        peer_uri = peer_uri,
        state = state,
        client_uri = client_uri,
    );

    format!(
        concat!(
            "NOTIFY {client_uri} SIP/2.0\r\n",
            "Via: SIP/2.0/UDP {addr};branch=z9hG4bK-peer-notify\r\n",
            "Max-Forwards: 70\r\n",
            "From: {peer_uri};tag={PEER_TAG}\r\n",
            "To: {client_uri};tag={client_tag}\r\n",
            "Call-ID: {call_id}\r\n",
            "CSeq: 1 NOTIFY\r\n",
            "Event: dialog-info+simulimit\r\n",
            "Subscription-State: active;expires=3600\r\n",
            "Content-Type: application/dialog-info+xml\r\n",
            "Content-Length: {length}\r\n",
            "\r\n",
            "{body}"
        ),
        client_uri = client_uri,
        addr = peer_addr,
        peer_uri = peer_uri,
        PEER_TAG = PEER_TAG,
        client_tag = client_tag,
        call_id = call_id,
        length = body.len(),
        body = body,
    )
}

/// The Busy Lamp Field says whether an extension is on a call.
///
/// The client subscribes to `dialog-info+simulimit`, the server answers
/// with `202 Accepted` and then sends a `NOTIFY` carrying the RFC 3903
/// document. The lamp says what the document says.
#[tokio::test]
async fn the_busy_lamp_field_says_whether_the_extension_is_on_a_call() {
    init_tracing();
    let socket = UdpSocket::bind("127.0.0.1:0").await.expect("peer socket");
    let peer_addr = socket.local_addr().expect("peer addr");
    let observations = Arc::new(Mutex::new(Observations::default()));
    tokio::spawn(run_peer(
        socket,
        observations.clone(),
        PeerOptions {
            call_back: false,
            dtmf: None,
            challenge_invite: false,
            transfer: None,
            public_address: None,
            bye_status: None,
            early_media: false,
            cancel_the_call: false,
            ring_for: None,
            ring_delay: None,
            // The extension is on a call.
            blf_state: Some("confirmed"),
        },
    ));

    let handle = tokio::runtime::Handle::current();
    let mut stack = stack(handle);

    let mut account = Account::new("Office", peer_addr.to_string(), "104", "secret", "udp");
    account.id = "acct-blf".to_string();
    stack.registrations.start(&account, 0);

    tokio::time::sleep(Duration::from_millis(300)).await;

    stack
        .blf
        .watch(&account.id, "105", &format!("sip:105@{peer_addr}"))
        .expect("the extension was not watched");

    // The lamp says what the NOTIFY said.
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    let mut lamp = None;
    while std::time::Instant::now() < deadline {
        lamp = stack.blf.lamps(&account.id).into_iter().next();
        if let Some(lamp) = &lamp {
            if lamp.state != qsoftphone_core::sip::blf::BlfState::Unavailable {
                break;
            }
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    let lamp = lamp.expect("the lamp never appeared");
    assert_eq!(lamp.extension, "105");
    assert_eq!(
        lamp.state,
        qsoftphone_core::sip::blf::BlfState::Busy,
        "the extension is on a call, so the lamp says busy"
    );

    // And the subscription went out the way it should.
    let subscribe = observations
        .lock()
        .expect("observations lock")
        .request("SUBSCRIBE")
        .expect("the server never saw the SUBSCRIBE");
    assert!(
        header_value(&subscribe, "Event").as_deref() == Some("dialog-info+simulimit"),
        "the subscription asks for the wrong event package:\n{subscribe}"
    );
    assert!(
        header_value(&subscribe, "Accept").as_deref() == Some("application/dialog-info+xml"),
        "the subscription does not say what it will take:\n{subscribe}"
    );
    assert!(
        header_value(&subscribe, "Expires").is_some(),
        "the subscription has no lifetime:\n{subscribe}"
    );
}

/// A lamp that the server will not light stays dark.
///
/// A server that does not serve the event package answers `489 Bad
/// Event`. The lamp must say "unknown" — not "free": a lamp that says
/// "free" when the truth is "we stopped listening" is a lamp that
/// lies.
#[tokio::test]
async fn a_server_that_does_not_serve_the_package_leaves_the_lamp_dark() {
    init_tracing();
    let socket = UdpSocket::bind("127.0.0.1:0").await.expect("peer socket");
    let peer_addr = socket.local_addr().expect("peer addr");
    let observations = Arc::new(Mutex::new(Observations::default()));
    tokio::spawn(run_peer(
        socket,
        observations.clone(),
        PeerOptions {
            call_back: false,
            dtmf: None,
            challenge_invite: false,
            transfer: None,
            public_address: None,
            bye_status: None,
            early_media: false,
            cancel_the_call: false,
            ring_for: None,
            ring_delay: None,
            // The server does not serve the package.
            blf_state: None,
        },
    ));

    let handle = tokio::runtime::Handle::current();
    let mut stack = stack(handle);

    let mut account = Account::new("Office", peer_addr.to_string(), "104", "secret", "udp");
    account.id = "acct-blf-refused".to_string();
    stack.registrations.start(&account, 0);

    tokio::time::sleep(Duration::from_millis(300)).await;

    stack
        .blf
        .watch(&account.id, "105", &format!("sip:105@{peer_addr}"))
        .expect("the extension was not watched");

    // Give the refusal time to arrive.
    tokio::time::sleep(Duration::from_millis(500)).await;

    let lamps = stack.blf.lamps(&account.id);
    assert_eq!(lamps.len(), 1, "the lamp is not there at all");
    assert_eq!(
        lamps[0].state,
        qsoftphone_core::sip::blf::BlfState::Unavailable,
        "a lamp that was never told anything must say unknown, not free"
    );
}

/// The app's SIP stack: a registrar, a call manager and the bus
/// between them.
struct Stack {
    registrations: RegistrationManager,
    calls: CallManager,
    blf: qsoftphone_sip::BlfManager,
}

fn stack(handle: tokio::runtime::Handle) -> Stack {
    let sessions = SessionRegistry::new();
    let (bus, receiver) = CallBus::new();
    Stack {
        registrations: RegistrationManager::new(handle.clone(), sessions.clone(), bus.clone()),
        calls: CallManager::new(handle.clone(), sessions.clone(), bus, receiver),
        blf: qsoftphone_sip::BlfManager::new(sessions, handle),
    }
}

/// Show what the SIP stack is doing.
///
/// The level comes from `RUST_LOG`; without it nothing is printed, so
/// a normal test run stays quiet.
fn init_tracing() {
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn"));
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_test_writer()
        .try_init();
}

/// Wait for an event of a given kind.
///
/// Every event that comes in is kept in `seen`, so a test can look for
/// two kinds of event without the first wait swallowing the second.
async fn wait_for(
    seen: &mut Vec<qsoftphone_core::model::call_event::CallEvent>,
    calls: &mut CallManager,
    kind: CallEventType,
    timeout: Duration,
) -> Option<qsoftphone_core::model::call_event::CallEvent> {
    let deadline = std::time::Instant::now() + timeout;
    loop {
        for update in calls.drain() {
            seen.push(update.event);
        }

        if let Some(position) = seen.iter().position(|e| e.event_type == kind) {
            return Some(seen.remove(position));
        }

        if std::time::Instant::now() >= deadline {
            return None;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

#[tokio::test]
async fn an_outgoing_call_connects_and_hangs_up() {
    init_tracing();
    let socket = UdpSocket::bind("127.0.0.1:0").await.expect("peer socket");
    let peer_addr = socket.local_addr().expect("peer addr");
    let observations = Arc::new(Mutex::new(Observations::default()));
    tokio::spawn(run_peer(
        socket,
        observations.clone(),
        PeerOptions {
            call_back: false,
            dtmf: None,
            challenge_invite: false,
            transfer: None,
            public_address: None,
            bye_status: None,
            early_media: false,
            cancel_the_call: false,
            ring_for: None,
            ring_delay: None,
            blf_state: None,
        },
    ));

    let handle = tokio::runtime::Handle::current();
    let mut stack = stack(handle);

    let mut account = Account::new("Office", peer_addr.to_string(), "104", "secret", "udp");
    account.id = "acct-loopback".to_string();
    stack.registrations.start(&account, 0);

    // The registration has to land before a call can go out.
    tokio::time::sleep(Duration::from_millis(300)).await;

    let mut seen = Vec::new();
    let call_id = stack
        .calls
        .place_call(&account.id, "100")
        .expect("the call was placed");

    // The event carries the callee's **URI**, the way the C++'s
    // `CallEvent` does — the history row keeps the URI and derives
    // the number from it (§5.3).
    let initiated = wait_for(
        &mut seen,
        &mut stack.calls,
        CallEventType::OutgoingCallInitiated,
        Duration::from_secs(5),
    )
    .await
    .expect("the outgoing call was never reported");
    assert!(
        initiated.callee.starts_with("sip:"),
        "the callee is not a URI: {}",
        initiated.callee
    );
    assert!(
        initiated.callee.contains("100"),
        "the callee is not the number that was dialled: {}",
        initiated.callee
    );

    // The peer's 180 Ringing is reported as an event of its own.
    let ringing = wait_for(
        &mut seen,
        &mut stack.calls,
        CallEventType::OutgoingCallRinging,
        Duration::from_secs(5),
    )
    .await;
    assert!(ringing.is_some(), "the call never rang");

    let answered = wait_for(
        &mut seen,
        &mut stack.calls,
        CallEventType::CallAnswered,
        Duration::from_secs(5),
    )
    .await;
    assert!(answered.is_some(), "the call was never answered");

    // The peer saw the INVITE.
    assert!(
        observations
            .lock()
            .expect("observations lock")
            .saw_request("INVITE"),
        "the peer never saw the INVITE"
    );

    stack.calls.hangup(&account.id, &call_id).expect("hangup");

    let ended = wait_for(
        &mut seen,
        &mut stack.calls,
        CallEventType::CallEnded,
        Duration::from_secs(5),
    )
    .await;
    let ended = ended.expect("the call never ended");

    // §5.5: the hangup is a plain BYE. The stack's default for an
    // established call is 200 OK — not a forced 603 Decline.
    assert_eq!(ended.sip_code, 200, "the hangup forced a status code");
    assert_ne!(ended.sip_code, 603, "603 Decline must not be forced");

    // The peer saw the BYE — not a CANCEL.
    assert!(
        observations
            .lock()
            .expect("observations lock")
            .saw_request("BYE"),
        "the peer never saw the BYE"
    );
}

/// The account's own headers go out on the call.
///
/// A PBX often wants something on the INVITE that the SIP headers do
/// not carry — a ticket number, a CRM id, the queue. Every account has
/// its own list of them, and they go on the calls that account makes.
#[tokio::test]
async fn the_call_goes_out_with_the_accounts_own_headers() {
    init_tracing();
    let socket = UdpSocket::bind("127.0.0.1:0").await.expect("peer socket");
    let peer_addr = socket.local_addr().expect("peer addr");
    let observations = Arc::new(Mutex::new(Observations::default()));
    tokio::spawn(run_peer(
        socket,
        observations.clone(),
        PeerOptions {
            call_back: false,
            dtmf: None,
            challenge_invite: false,
            transfer: None,
            public_address: None,
            bye_status: None,
            early_media: false,
            cancel_the_call: false,
            ring_for: None,
            ring_delay: None,
            blf_state: None,
        },
    ));

    let handle = tokio::runtime::Handle::current();
    let mut stack = stack(handle);

    let mut account = Account::new("Office", peer_addr.to_string(), "104", "secret", "udp");
    account.id = "acct-custom-headers".to_string();
    // The account's own headers. The second value is a **template**:
    // it is resolved when the call goes out, not when it is saved.
    account.custom_headers = vec![
        ("X-var1".to_string(), "one".to_string()),
        ("X-var2".to_string(), "$called".to_string()),
        ("X-var3".to_string(), "for $callerid".to_string()),
    ];
    stack.registrations.start(&account, 0);

    tokio::time::sleep(Duration::from_millis(300)).await;

    let mut seen = Vec::new();
    stack
        .calls
        .place_call(&account.id, "100")
        .expect("the call was placed");
    let answered = wait_for(
        &mut seen,
        &mut stack.calls,
        CallEventType::CallAnswered,
        Duration::from_secs(5),
    )
    .await;
    assert!(answered.is_some(), "the call was never answered");

    // The peer saw the INVITE, and the headers are on it.
    let invite = {
        let observations = observations.lock().expect("observations lock");
        observations
            .raw
            .iter()
            .find(|message| message.starts_with("INVITE"))
            .cloned()
            .expect("the peer never saw the INVITE")
    };

    assert_eq!(
        header_value(&invite, "X-var1").as_deref(),
        Some("one"),
        "the account's header is not on the INVITE:\n{invite}"
    );
    assert_eq!(
        header_value(&invite, "X-var2").as_deref(),
        Some("100"),
        "the template was not resolved against the call:\n{invite}"
    );
    assert_eq!(
        header_value(&invite, "X-var3").as_deref(),
        Some("for 104"),
        "the template was not resolved inside the value:\n{invite}"
    );
}

/// A header whose name is not a header name does not go out.
///
/// A name with a space in it would split the message in two, and a peer
/// that parses it strictly will refuse the whole request. One broken
/// header is worth less than the call.
#[tokio::test]
async fn a_header_with_a_broken_name_does_not_go_out() {
    init_tracing();
    let socket = UdpSocket::bind("127.0.0.1:0").await.expect("peer socket");
    let peer_addr = socket.local_addr().expect("peer addr");
    let observations = Arc::new(Mutex::new(Observations::default()));
    tokio::spawn(run_peer(
        socket,
        observations.clone(),
        PeerOptions {
            call_back: false,
            dtmf: None,
            challenge_invite: false,
            transfer: None,
            public_address: None,
            bye_status: None,
            early_media: false,
            cancel_the_call: false,
            ring_for: None,
            ring_delay: None,
            blf_state: None,
        },
    ));

    let handle = tokio::runtime::Handle::current();
    let mut stack = stack(handle);

    let mut account = Account::new("Office", peer_addr.to_string(), "104", "secret", "udp");
    account.id = "acct-broken-header".to_string();
    account.custom_headers = vec![
        ("X good".to_string(), "one".to_string()),
        ("X-fine".to_string(), "two".to_string()),
    ];
    stack.registrations.start(&account, 0);

    tokio::time::sleep(Duration::from_millis(300)).await;

    let mut seen = Vec::new();
    stack
        .calls
        .place_call(&account.id, "100")
        .expect("the call was placed");
    let answered = wait_for(
        &mut seen,
        &mut stack.calls,
        CallEventType::CallAnswered,
        Duration::from_secs(5),
    )
    .await;
    assert!(answered.is_some(), "the call was never answered");

    let invite = {
        let observations = observations.lock().expect("observations lock");
        observations
            .raw
            .iter()
            .find(|message| message.starts_with("INVITE"))
            .cloned()
            .expect("the peer never saw the INVITE")
    };

    assert!(
        header_value(&invite, "X good").is_none(),
        "a header with a space in its name went out:\n{invite}"
    );
    assert_eq!(
        header_value(&invite, "X-fine").as_deref(),
        Some("two"),
        "the good header was dropped with the bad one:\n{invite}"
    );
}

/// An account with no headers sends a request that has none.
///
/// The headers are only there when the account has some: a request that
/// goes out exactly as it did before is a request that cannot break.
#[tokio::test]
async fn an_account_with_no_headers_sends_none() {
    init_tracing();
    let socket = UdpSocket::bind("127.0.0.1:0").await.expect("peer socket");
    let peer_addr = socket.local_addr().expect("peer addr");
    let observations = Arc::new(Mutex::new(Observations::default()));
    tokio::spawn(run_peer(
        socket,
        observations.clone(),
        PeerOptions {
            call_back: false,
            dtmf: None,
            challenge_invite: false,
            transfer: None,
            public_address: None,
            bye_status: None,
            early_media: false,
            cancel_the_call: false,
            ring_for: None,
            ring_delay: None,
            blf_state: None,
        },
    ));

    let handle = tokio::runtime::Handle::current();
    let mut stack = stack(handle);

    let mut account = Account::new("Office", peer_addr.to_string(), "104", "secret", "udp");
    account.id = "acct-no-headers".to_string();
    stack.registrations.start(&account, 0);

    tokio::time::sleep(Duration::from_millis(300)).await;

    let mut seen = Vec::new();
    stack
        .calls
        .place_call(&account.id, "100")
        .expect("the call was placed");
    let answered = wait_for(
        &mut seen,
        &mut stack.calls,
        CallEventType::CallAnswered,
        Duration::from_secs(5),
    )
    .await;
    assert!(answered.is_some(), "the call was never answered");

    let invite = {
        let observations = observations.lock().expect("observations lock");
        observations
            .raw
            .iter()
            .find(|message| message.starts_with("INVITE"))
            .cloned()
            .expect("the peer never saw the INVITE")
    };

    // The stack's own headers are there; none of the account's are.
    assert!(
        !invite.to_lowercase().contains("x-var"),
        "the account has no headers, so none went out:\n{invite}"
    );
    assert!(
        invite.contains("User-Agent:"),
        "the request is a normal SIP request:\n{invite}"
    );
}

#[tokio::test]
async fn an_incoming_call_rings_before_the_user_answers() {
    init_tracing();
    let socket = UdpSocket::bind("127.0.0.1:0").await.expect("peer socket");
    let peer_addr = socket.local_addr().expect("peer addr");
    let observations = Arc::new(Mutex::new(Observations::default()));
    tokio::spawn(run_peer(
        socket,
        observations.clone(),
        PeerOptions {
            call_back: true,
            dtmf: None,
            challenge_invite: false,
            transfer: None,
            public_address: None,
            bye_status: None,
            early_media: false,
            cancel_the_call: false,
            ring_for: None,
            ring_delay: None,
            blf_state: None,
        },
    ));

    let handle = tokio::runtime::Handle::current();
    let mut stack = stack(handle);

    let mut account = Account::new("Office", peer_addr.to_string(), "104", "secret", "udp");
    account.id = "acct-loopback-in".to_string();
    stack.registrations.start(&account, 0);

    // The peer calls back as soon as it sees the REGISTER.
    let mut seen = Vec::new();
    let incoming = wait_for(
        &mut seen,
        &mut stack.calls,
        CallEventType::IncomingCall,
        Duration::from_secs(5),
    )
    .await;
    let incoming = incoming.expect("the incoming call never arrived");

    assert_eq!(
        incoming.call_direction,
        Some(qsoftphone_core::model::call_event::Direction::Incoming)
    );
    assert!(
        incoming.call_id.starts_with("peer-incoming-call@"),
        "unexpected call id {}",
        incoming.call_id
    );
    // The event carries the peer's **URI**, the way the C++'s
    // `CallEvent` does — the history row keeps the URI and derives
    // the number from it (§5.3).
    assert!(
        incoming.caller.starts_with("sip:"),
        "the caller is not a URI: {}",
        incoming.caller
    );
    assert!(
        incoming.caller.contains("peer"),
        "the caller is not the peer's URI: {}",
        incoming.caller
    );
    assert!(
        incoming.callee.starts_with("sip:"),
        "the callee is not a URI: {}",
        incoming.callee
    );

    // §5.4: the 180 Ringing went out before the user answered. The
    // peer recorded it already — the answer below has not happened
    // yet.
    assert!(
        observations
            .lock()
            .expect("observations lock")
            .saw_response("180 Ringing"),
        "the peer never got the 180 Ringing"
    );

    // The user answers.
    stack
        .calls
        .answer(&account.id, &incoming.call_id)
        .expect("answer");

    let answered = wait_for(
        &mut seen,
        &mut stack.calls,
        CallEventType::CallAnswered,
        Duration::from_secs(5),
    )
    .await;
    assert!(answered.is_some(), "the call was never confirmed");

    // The peer got the 200 OK with the SDP body.
    assert!(
        observations
            .lock()
            .expect("observations lock")
            .saw_response("200 OK"),
        "the peer never got the 200 OK"
    );

    // The answer has to name a codec the offer listed (RFC 3264 §6).
    // The peer offered G.711 A-law and telephone-event only; an answer
    // that said µ-law is invalid, and Asterisk hangs up with a BYE
    // right after the 200 OK.
    let answer = observations
        .lock()
        .expect("observations lock")
        .response("200 OK")
        .expect("the peer never got a 200 OK");
    assert!(
        answer.contains("a=rtpmap:8 PCMA/8000"),
        "the answer must use the codec the peer offered: {answer}"
    );
    assert!(
        !answer.contains("PCMU"),
        "the answer must not name a codec the peer did not offer: {answer}"
    );
}

#[tokio::test]
async fn a_ringing_incoming_call_is_declined_when_it_is_hung_up() {
    init_tracing();
    // §5.5: an incoming call that is still ringing is declined with
    // 603 — the stack's default, not a forced code of our own.
    let socket = UdpSocket::bind("127.0.0.1:0").await.expect("peer socket");
    let peer_addr = socket.local_addr().expect("peer addr");
    let observations = Arc::new(Mutex::new(Observations::default()));
    tokio::spawn(run_peer(
        socket,
        observations.clone(),
        PeerOptions {
            call_back: true,
            dtmf: None,
            challenge_invite: false,
            transfer: None,
            public_address: None,
            bye_status: None,
            early_media: false,
            cancel_the_call: false,
            ring_for: None,
            ring_delay: None,
            blf_state: None,
        },
    ));

    let handle = tokio::runtime::Handle::current();
    let mut stack = stack(handle);

    let mut account = Account::new("Office", peer_addr.to_string(), "104", "secret", "udp");
    account.id = "acct-loopback-decline".to_string();
    stack.registrations.start(&account, 0);

    let mut seen = Vec::new();
    let incoming = wait_for(
        &mut seen,
        &mut stack.calls,
        CallEventType::IncomingCall,
        Duration::from_secs(5),
    )
    .await;
    let incoming = incoming.expect("the incoming call never arrived");

    stack
        .calls
        .hangup(&account.id, &incoming.call_id)
        .expect("hangup");

    let ended = wait_for(
        &mut seen,
        &mut stack.calls,
        CallEventType::CallEnded,
        Duration::from_secs(5),
    )
    .await;
    let ended = ended.expect("the call never ended");

    assert_eq!(ended.sip_code, 603, "a ringing call must be declined");

    // The peer was told 603 Decline, not 200 OK.
    assert!(
        observations
            .lock()
            .expect("observations lock")
            .saw_response("603"),
        "the peer never got the 603 Decline"
    );
}

#[tokio::test]
async fn a_dtmf_digit_arrives_as_an_event() {
    init_tracing();
    // RFC 4733 over INFO: the peer sends the digit once the dialog is
    // established, and it has to arrive as an event.
    let socket = UdpSocket::bind("127.0.0.1:0").await.expect("peer socket");
    let peer_addr = socket.local_addr().expect("peer addr");
    let observations = Arc::new(Mutex::new(Observations::default()));
    tokio::spawn(run_peer(
        socket,
        observations.clone(),
        PeerOptions {
            call_back: true,
            dtmf: Some('5'),
            challenge_invite: false,
            transfer: None,
            public_address: None,
            bye_status: None,
            early_media: false,
            cancel_the_call: false,
            ring_for: None,
            ring_delay: None,
            blf_state: None,
        },
    ));

    let handle = tokio::runtime::Handle::current();
    let mut stack = stack(handle);

    let mut account = Account::new("Office", peer_addr.to_string(), "104", "secret", "udp");
    account.id = "acct-loopback-dtmf".to_string();
    stack.registrations.start(&account, 0);

    let mut seen = Vec::new();
    let incoming = wait_for(
        &mut seen,
        &mut stack.calls,
        CallEventType::IncomingCall,
        Duration::from_secs(5),
    )
    .await;
    let incoming = incoming.expect("the incoming call never arrived");

    stack
        .calls
        .answer(&account.id, &incoming.call_id)
        .expect("answer");

    let digit = wait_for(
        &mut seen,
        &mut stack.calls,
        CallEventType::DtmfReceived,
        Duration::from_secs(5),
    )
    .await;
    let digit = digit.expect("the DTMF digit never arrived");
    assert_eq!(digit.digit, "5");
    assert_eq!(digit.call_id, incoming.call_id);
}

#[tokio::test]
async fn a_dtmf_digit_is_sent_as_an_info_request() {
    init_tracing();
    let socket = UdpSocket::bind("127.0.0.1:0").await.expect("peer socket");
    let peer_addr = socket.local_addr().expect("peer addr");
    let observations = Arc::new(Mutex::new(Observations::default()));
    tokio::spawn(run_peer(
        socket,
        observations.clone(),
        PeerOptions {
            call_back: false,
            dtmf: None,
            challenge_invite: false,
            transfer: None,
            public_address: None,
            bye_status: None,
            early_media: false,
            cancel_the_call: false,
            ring_for: None,
            ring_delay: None,
            blf_state: None,
        },
    ));

    let handle = tokio::runtime::Handle::current();
    let mut stack = stack(handle);

    let mut account = Account::new("Office", peer_addr.to_string(), "104", "secret", "udp");
    account.id = "acct-loopback-dtmf-out".to_string();
    stack.registrations.start(&account, 0);

    tokio::time::sleep(Duration::from_millis(300)).await;

    let mut seen = Vec::new();
    let call_id = stack
        .calls
        .place_call(&account.id, "100")
        .expect("the call was placed");

    let answered = wait_for(
        &mut seen,
        &mut stack.calls,
        CallEventType::CallAnswered,
        Duration::from_secs(5),
    )
    .await;
    assert!(answered.is_some(), "the call was never answered");

    stack
        .calls
        .send_dtmf(&account.id, &call_id, "7")
        .expect("the digit was sent");

    // The peer has to see the INFO.
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while std::time::Instant::now() < deadline {
        if observations
            .lock()
            .expect("observations lock")
            .saw_request("INFO")
        {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(
        observations
            .lock()
            .expect("observations lock")
            .saw_request("INFO"),
        "the peer never saw the INFO"
    );
}

/// A call placed from a thread that is not inside the runtime.
///
/// The UI thread is the one that owns the window, and it is **not**
/// inside the tokio runtime: `eframe` runs the app on the main thread
/// while the SIP stack runs on a runtime of its own.
///
/// rsipstack drives the INVITE from a task it spawns with
/// `tokio::spawn`, and `tokio::spawn` panics when the calling thread
/// has no runtime. Placing a call therefore used to take the whole app
/// down, before a single byte went out.
///
/// This test is deliberately **not** a `#[tokio::test]`: a tokio test
/// runs inside the runtime, which is exactly the case that never
/// crashed.
#[test]
fn a_call_placed_off_the_runtime_does_not_take_the_app_down() {
    init_tracing();

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("runtime");
    let handle = runtime.handle().clone();

    let socket =
        runtime.block_on(async { UdpSocket::bind("127.0.0.1:0").await.expect("peer socket") });
    let peer_addr = socket.local_addr().expect("peer addr");
    let observations = Arc::new(Mutex::new(Observations::default()));
    runtime.spawn(run_peer(
        socket,
        observations.clone(),
        PeerOptions {
            call_back: false,
            dtmf: None,
            challenge_invite: false,
            transfer: None,
            public_address: None,
            bye_status: None,
            early_media: false,
            cancel_the_call: false,
            ring_for: None,
            ring_delay: None,
            blf_state: None,
        },
    ));

    let mut stack = stack(handle.clone());
    let mut account = Account::new("Office", peer_addr.to_string(), "104", "secret", "udp");
    account.id = "acct-ui-thread".to_string();
    stack.registrations.start(&account, 0);

    // The registration has to land before a call can go out.
    runtime.block_on(async {
        tokio::time::sleep(Duration::from_millis(300)).await;
    });

    // The UI thread: no runtime context. The old code panicked right
    // here, and the panic took the window down with it.
    assert!(
        tokio::runtime::Handle::try_current().is_err(),
        "the test has to run off the runtime, or it proves nothing"
    );
    stack
        .calls
        .place_call(&account.id, "100")
        .expect("the call was placed");

    // And the call really connects.
    let mut seen = Vec::new();
    let answered = runtime.block_on(async {
        wait_for(
            &mut seen,
            &mut stack.calls,
            CallEventType::CallAnswered,
            Duration::from_secs(5),
        )
        .await
    });
    assert!(answered.is_some(), "the call was never answered");

    assert!(
        observations
            .lock()
            .expect("observations lock")
            .saw_request("INVITE"),
        "the peer never saw the INVITE"
    );
}

/// A challenged INVITE is answered with the account's digest.
///
/// A PBX that challenges the INVITE with 401 or 407 has to be
/// answered with the credential of §5.8. A stack that has no
/// credential treats the challenge as the final response, and the call
/// dies before it ever rings.
#[tokio::test]
async fn a_challenged_invite_is_answered_with_the_account_digest() {
    init_tracing();
    let socket = UdpSocket::bind("127.0.0.1:0").await.expect("peer socket");
    let peer_addr = socket.local_addr().expect("peer addr");
    let observations = Arc::new(Mutex::new(Observations::default()));
    tokio::spawn(run_peer(
        socket,
        observations.clone(),
        PeerOptions {
            call_back: false,
            dtmf: None,
            challenge_invite: true,
            transfer: None,
            public_address: None,
            bye_status: None,
            early_media: false,
            cancel_the_call: false,
            ring_for: None,
            ring_delay: None,
            blf_state: None,
        },
    ));

    let handle = tokio::runtime::Handle::current();
    let mut stack = stack(handle);

    let mut account = Account::new("Office", peer_addr.to_string(), "104", "secret", "udp");
    account.id = "acct-loopback-auth".to_string();
    stack.registrations.start(&account, 0);

    tokio::time::sleep(Duration::from_millis(300)).await;

    let mut seen = Vec::new();
    stack
        .calls
        .place_call(&account.id, "100")
        .expect("the call was placed");

    let answered = wait_for(
        &mut seen,
        &mut stack.calls,
        CallEventType::CallAnswered,
        Duration::from_secs(5),
    )
    .await;
    assert!(answered.is_some(), "the call was never answered");

    // The peer got the INVITE twice: once bare, once with the digest.
    let observations = observations.lock().expect("observations lock");
    let invites: Vec<&String> = observations
        .raw
        .iter()
        .filter(|message| message.starts_with("INVITE "))
        .collect();
    assert!(
        invites
            .iter()
            .any(|message| !message.contains("Authorization")),
        "the peer never saw the bare INVITE it had to challenge"
    );

    let authorised = invites
        .iter()
        .find(|message| message.contains("Authorization:"))
        .expect("the INVITE was never retried with the digest");
    let value = header_value(authorised, "Authorization").expect("the authorization header");

    // The digest is a real digest: the peer can verify it with the
    // account's password, which is the whole point of the challenge.
    let parsed = rsipstack::sip::headers::typed::Authorization::parse(&value)
        .expect("the authorization header parses");
    assert_eq!(parsed.username, "104");
    assert_eq!(parsed.realm, "asterisk");
    assert!(
        rsipstack::dialog::authenticate::verify_digest(
            &parsed,
            "secret",
            &rsipstack::sip::Method::Invite,
            &value,
        ),
        "the digest does not verify against the account's password"
    );
}

/// Wait for an event of a given kind **of a given call**.
///
/// `wait_for` takes the first event of the kind it finds, which is not
/// enough when two calls are in flight: the original call's own `180
/// Ringing` would answer a wait meant for the consultation's. This one
/// looks at which call the event belongs to.
async fn wait_for_call(
    seen: &mut Vec<qsoftphone_core::model::call_event::CallEvent>,
    calls: &mut CallManager,
    kind: CallEventType,
    call_id: &str,
    timeout: Duration,
) -> Option<qsoftphone_core::model::call_event::CallEvent> {
    let deadline = std::time::Instant::now() + timeout;
    loop {
        for update in calls.drain() {
            seen.push(update.event);
        }

        if let Some(position) = seen
            .iter()
            .position(|e| e.event_type == kind && e.call_id == call_id)
        {
            return Some(seen.remove(position));
        }

        if std::time::Instant::now() >= deadline {
            return None;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// Wait for the REFER the peer received.
async fn wait_for_refer(observations: &Arc<Mutex<Observations>>) -> String {
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        {
            let observations = observations.lock().expect("observations lock");
            if let Some(raw) = observations
                .raw
                .iter()
                .find(|message| message.starts_with("REFER "))
            {
                return raw.clone();
            }
        }
        if std::time::Instant::now() >= deadline {
            panic!("the peer never saw the REFER");
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// Wait for the transfer progress that reports a particular code.
///
/// The peer reports the progress in several NOTIFYs — `100 Trying`,
/// then `180 Ringing`, then `200 OK` — so the test asks for the one it
/// cares about rather than for the first one.
async fn wait_for_progress(
    seen: &mut Vec<qsoftphone_core::model::call_event::CallEvent>,
    calls: &mut CallManager,
    code: i32,
    timeout: Duration,
) -> Option<qsoftphone_core::model::call_event::CallEvent> {
    let deadline = std::time::Instant::now() + timeout;
    loop {
        for update in calls.drain() {
            seen.push(update.event);
        }

        if let Some(position) = seen.iter().position(|e| {
            e.event_type == CallEventType::TransferProgress && e.transfer_code == code
        }) {
            return Some(seen.remove(position));
        }

        if std::time::Instant::now() >= deadline {
            return None;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// Every in-dialog request has to carry the **public** address in its
/// Contact.
///
/// Behind a NAT the socket's own address is a private one the peer
/// cannot reach, and the registrar's answer says where it really sees
/// us from (RFC 3581). The Contact of an in-dialog request is what the
/// peer sends **its** in-dialog requests to — and the transfer's
/// NOTIFYs are exactly that.
///
/// The user's own PBX showed what goes wrong: the NOTIFYs went to the
/// VPN interface's address instead of the public one, so the progress
/// never arrived, the app never learned that the transfer had worked,
/// and the call never ended.
#[tokio::test]
async fn an_in_dialog_request_carries_the_public_contact() {
    init_tracing();
    let socket = UdpSocket::bind("127.0.0.1:0").await.expect("peer socket");
    let peer_addr = socket.local_addr().expect("peer addr");
    let observations = Arc::new(Mutex::new(Observations::default()));
    tokio::spawn(run_peer(
        socket,
        observations.clone(),
        PeerOptions {
            call_back: true,
            dtmf: None,
            challenge_invite: false,
            transfer: None,
            public_address: Some(PUBLIC_ADDRESS),
            bye_status: None,
            early_media: false,
            cancel_the_call: false,
            ring_for: None,
            ring_delay: None,
            blf_state: None,
        },
    ));

    let handle = tokio::runtime::Handle::current();
    let mut stack = stack(handle);

    let mut account = Account::new("Office", peer_addr.to_string(), "104", "secret", "udp");
    account.id = "acct-loopback-contact".to_string();
    stack.registrations.start(&account, 0);

    let mut seen = Vec::new();
    let incoming = wait_for(
        &mut seen,
        &mut stack.calls,
        CallEventType::IncomingCall,
        Duration::from_secs(5),
    )
    .await
    .expect("the incoming call never arrived");

    stack
        .calls
        .answer(&account.id, &incoming.call_id)
        .expect("answer");
    wait_for(
        &mut seen,
        &mut stack.calls,
        CallEventType::CallAnswered,
        Duration::from_secs(5),
    )
    .await
    .expect("the call was never answered");

    // The hangup is an in-dialog request, and it carries the dialog's
    // Contact.
    stack
        .calls
        .hangup(&account.id, &incoming.call_id)
        .expect("hangup");
    wait_for(
        &mut seen,
        &mut stack.calls,
        CallEventType::CallEnded,
        Duration::from_secs(5),
    )
    .await
    .expect("the call never ended");

    let bye = observations
        .lock()
        .expect("observations lock")
        .request("BYE")
        .expect("the peer never saw the BYE");
    let contact = header_value(&bye, "Contact").unwrap_or_default();

    assert!(
        contact.contains(PUBLIC_ADDRESS),
        "the BYE's Contact must name the public address, not the local socket: {contact}"
    );
    assert!(
        contact.contains("104"),
        "the BYE's Contact must name the user: {contact}"
    );
}

/// A BYE the peer does not know still ends the call.
///
/// A PBX answers `481 Call/Transaction Does Not Exist` when the dialog
/// is gone — which is what happens after a transfer, when the channel
/// is in a bridge and the dialog is not there any more. The call still
/// has to end: the BYE went out, and RFC 3261 §15.1.1 says the dialog
/// is terminated once the request is handed to its transaction.
#[tokio::test]
async fn a_bye_the_peer_does_not_know_still_ends_the_call() {
    init_tracing();
    let socket = UdpSocket::bind("127.0.0.1:0").await.expect("peer socket");
    let peer_addr = socket.local_addr().expect("peer addr");
    let observations = Arc::new(Mutex::new(Observations::default()));
    tokio::spawn(run_peer(
        socket,
        observations.clone(),
        PeerOptions {
            call_back: true,
            dtmf: None,
            challenge_invite: false,
            transfer: None,
            public_address: None,
            bye_status: Some("481 Call/Transaction Does Not Exist"),
            early_media: false,
            cancel_the_call: false,
            ring_for: None,
            ring_delay: None,
            blf_state: None,
        },
    ));

    let handle = tokio::runtime::Handle::current();
    let mut stack = stack(handle);

    let mut account = Account::new("Office", peer_addr.to_string(), "104", "secret", "udp");
    account.id = "acct-loopback-481".to_string();
    stack.registrations.start(&account, 0);

    let mut seen = Vec::new();
    let incoming = wait_for(
        &mut seen,
        &mut stack.calls,
        CallEventType::IncomingCall,
        Duration::from_secs(5),
    )
    .await
    .expect("the incoming call never arrived");

    stack
        .calls
        .answer(&account.id, &incoming.call_id)
        .expect("answer");
    wait_for(
        &mut seen,
        &mut stack.calls,
        CallEventType::CallAnswered,
        Duration::from_secs(5),
    )
    .await
    .expect("the call was never answered");

    stack
        .calls
        .hangup(&account.id, &incoming.call_id)
        .expect("hangup");
    wait_for(
        &mut seen,
        &mut stack.calls,
        CallEventType::CallEnded,
        Duration::from_secs(5),
    )
    .await
    .expect("the call never ended, even though the peer did not know the dialog");

    // The peer really did answer with 481.
    assert!(
        observations
            .lock()
            .expect("observations lock")
            .sent
            .iter()
            .any(|status| status.contains("481")),
        "the peer never answered the BYE with 481"
    );
}

/// A blind transfer: the peer takes the REFER, reports the progress,
/// and the original leg is released when the new leg is answered
/// (§5.6).
#[tokio::test]
async fn a_blind_transfer_reaches_the_peer_and_releases_the_original_call() {
    init_tracing();
    let socket = UdpSocket::bind("127.0.0.1:0").await.expect("peer socket");
    let peer_addr = socket.local_addr().expect("peer addr");
    let observations = Arc::new(Mutex::new(Observations::default()));
    tokio::spawn(run_peer(
        socket,
        observations.clone(),
        PeerOptions {
            call_back: false,
            dtmf: None,
            challenge_invite: false,
            transfer: Some(TransferScript::Answered),
            public_address: None,
            bye_status: None,
            early_media: false,
            cancel_the_call: false,
            ring_for: None,
            ring_delay: None,
            blf_state: None,
        },
    ));

    let handle = tokio::runtime::Handle::current();
    let mut stack = stack(handle);

    let mut account = Account::new("Office", peer_addr.to_string(), "104", "secret", "udp");
    account.id = "acct-transfer-blind".to_string();
    stack.registrations.start(&account, 0);

    tokio::time::sleep(Duration::from_millis(300)).await;

    let mut seen = Vec::new();
    let call_id = stack
        .calls
        .place_call(&account.id, "100")
        .expect("the call was placed");

    let answered = wait_for(
        &mut seen,
        &mut stack.calls,
        CallEventType::CallAnswered,
        Duration::from_secs(5),
    )
    .await;
    assert!(answered.is_some(), "the call was never answered");

    stack
        .calls
        .transfer_blind(&account.id, &call_id, "200")
        .expect("the transfer did not start");

    // The peer saw the REFER, and the REFER names the new target.
    let refer = wait_for_refer(&observations).await;
    let refer_to = header_value(&refer, "Refer-To").expect("the REFER had no Refer-To");
    assert!(
        refer_to.contains("200"),
        "the Refer-To does not name the new target: {refer_to}"
    );
    assert!(
        !refer_to.contains("Replaces"),
        "a blind transfer must not carry a Replaces: {refer_to}"
    );

    // The progress arrives over the NOTIFY (RFC 3515 §3.4).
    let progress =
        wait_for_progress(&mut seen, &mut stack.calls, 200, Duration::from_secs(5)).await;
    let progress = progress.expect("the transfer progress was never reported");
    assert_eq!(
        progress.transfer_progress,
        Some(qsoftphone_core::sip::refer::TransferProgress::Answered),
        "the transfer did not reach the answered state"
    );

    // The original leg is released: the user is not left connected to
    // the old peer (§5.6).
    let ended = wait_for(
        &mut seen,
        &mut stack.calls,
        CallEventType::CallEnded,
        Duration::from_secs(5),
    )
    .await;
    assert!(ended.is_some(), "the original call was never released");

    assert!(
        observations
            .lock()
            .expect("observations lock")
            .saw_request("BYE"),
        "the original leg was not hung up"
    );
}

/// An attended transfer: the REFER names the consultation that is
/// already up, and carries its dialog identifier (§5.6).
#[tokio::test]
async fn the_attended_transfer_names_the_consultation() {
    init_tracing();
    let socket = UdpSocket::bind("127.0.0.1:0").await.expect("peer socket");
    let peer_addr = socket.local_addr().expect("peer addr");
    let observations = Arc::new(Mutex::new(Observations::default()));
    tokio::spawn(run_peer(
        socket,
        observations.clone(),
        PeerOptions {
            call_back: false,
            dtmf: None,
            challenge_invite: false,
            transfer: Some(TransferScript::Answered),
            public_address: None,
            bye_status: None,
            early_media: false,
            cancel_the_call: false,
            ring_for: None,
            ring_delay: None,
            blf_state: None,
        },
    ));

    let handle = tokio::runtime::Handle::current();
    let mut stack = stack(handle);

    let mut account = Account::new("Office", peer_addr.to_string(), "104", "secret", "udp");
    account.id = "acct-transfer-attended".to_string();
    stack.registrations.start(&account, 0);

    tokio::time::sleep(Duration::from_millis(300)).await;

    let mut seen = Vec::new();
    let source_call_id = stack
        .calls
        .place_call(&account.id, "100")
        .expect("the original call was placed");
    let answered = wait_for(
        &mut seen,
        &mut stack.calls,
        CallEventType::CallAnswered,
        Duration::from_secs(5),
    )
    .await;
    assert!(answered.is_some(), "the original call was never answered");

    // The consultation: a second call, to the party the call is being
    // handed over to.
    let consultation_call_id = stack
        .calls
        .place_call(&account.id, "200")
        .expect("the consultation was placed");
    let answered = wait_for(
        &mut seen,
        &mut stack.calls,
        CallEventType::CallAnswered,
        Duration::from_secs(5),
    )
    .await;
    assert!(answered.is_some(), "the consultation was never answered");

    stack
        .calls
        .transfer_attended(&account.id, &source_call_id, &consultation_call_id)
        .expect("the transfer did not start");

    let refer = wait_for_refer(&observations).await;
    let refer_to = header_value(&refer, "Refer-To").expect("the REFER had no Refer-To");

    // §5.6: the attended form carries the consultation's dialog
    // identifier as a URI parameter.
    assert!(
        refer_to.contains(";method=INVITE?Replaces="),
        "the Refer-To does not carry the Replaces parameter: {refer_to}"
    );

    // The identifier is the consultation's Call-ID with the `@`
    // percent-encoded — the peer matches it against the dialog it has
    // to take over.
    let expected = consultation_call_id.replacen('@', "%40", 1);
    assert!(
        refer_to.contains(&expected),
        "the Replaces is not the consultation's Call-ID: {refer_to}"
    );

    // RFC 3891 §3.1: the dialog is named by the Call-ID **and** its
    // two tags. A server that matches on all three cannot find the
    // dialog without them — and a server that cannot find it treats
    // the transfer as one to another machine.
    assert!(
        refer_to.contains("%3Bto-tag%3Dpeer-tag%3Bfrom-tag%3D"),
        "the Replaces does not carry the dialog's tags: {refer_to}"
    );

    // The `Refer-To` names the **target** the consultation was placed
    // to — the third party, not the server. The dialog's own snapshot
    // carries the peer's Contact, which is the server's address when
    // the call went through a PBX, and a `Refer-To` that names the
    // server is a transfer to the server. It matters for the fallback:
    // a server that cannot match the `Replaces` dials the `Refer-To`
    // instead.
    assert!(
        refer_to.contains("sip:200@"),
        "the Refer-To must name the target that was dialled: {refer_to}"
    );
    assert!(
        !refer_to.contains("sip:peer@"),
        "the Refer-To must not name the server's contact: {refer_to}"
    );

    // The same dialog is named in the `Replaces` **header** (RFC 3891
    // §3.1), which is the one Asterisk reads: a server that cannot
    // find the dialog treats the transfer as one to another machine,
    // and answers "no route to target".
    let replaces = header_value(&refer, "Replaces").expect("the REFER had no Replaces header");
    assert!(
        replaces.contains(&consultation_call_id),
        "the Replaces does not name the consultation's Call-ID: {replaces}"
    );
    assert!(
        replaces.contains("to-tag=peer-tag"),
        "the Replaces does not carry the dialog's to-tag: {replaces}"
    );
    assert!(
        replaces.contains("from-tag="),
        "the Replaces does not carry the dialog's from-tag: {replaces}"
    );
}

/// The transfer can be completed while the target is **still ringing**.
///
/// The user hears the target ring, says "complete the transfer", and
/// the **server** completes it when the target answers — the softphone
/// does not have to wait for the answer.
///
/// The peer holds the consultation in the early state, so the window
/// is there to be used: the `180 Ringing` goes out, the client acts on
/// it, and the `200 OK` comes after the REFER has been sent.
#[tokio::test]
async fn the_refer_goes_out_while_the_consultation_is_still_ringing() {
    init_tracing();
    let socket = UdpSocket::bind("127.0.0.1:0").await.expect("peer socket");
    let peer_addr = socket.local_addr().expect("peer addr");
    let observations = Arc::new(Mutex::new(Observations::default()));
    tokio::spawn(run_peer(
        socket,
        observations.clone(),
        PeerOptions {
            call_back: false,
            dtmf: None,
            challenge_invite: false,
            transfer: Some(TransferScript::Answered),
            public_address: None,
            bye_status: None,
            early_media: false,
            cancel_the_call: false,
            // The peer holds every call in the early state for a
            // second: that is the window the transfer is completed in.
            ring_for: Some(1000),
            ring_delay: None,
            blf_state: None,
        },
    ));

    let handle = tokio::runtime::Handle::current();
    let mut stack = stack(handle);

    let mut account = Account::new("Office", peer_addr.to_string(), "104", "secret", "udp");
    account.id = "acct-transfer-ringing".to_string();
    stack.registrations.start(&account, 0);

    tokio::time::sleep(Duration::from_millis(300)).await;

    let mut seen = Vec::new();
    let source_call_id = stack
        .calls
        .place_call(&account.id, "100")
        .expect("the original call was placed");
    let answered = wait_for(
        &mut seen,
        &mut stack.calls,
        CallEventType::CallAnswered,
        Duration::from_secs(5),
    )
    .await;
    assert!(answered.is_some(), "the original call was never answered");

    // The consultation: the peer rings it and **holds** it there.
    let consultation_call_id = stack
        .calls
        .place_call(&account.id, "200")
        .expect("the consultation was placed");
    let ringing = wait_for_call(
        &mut seen,
        &mut stack.calls,
        CallEventType::OutgoingCallRinging,
        &consultation_call_id,
        Duration::from_secs(5),
    )
    .await;
    assert!(ringing.is_some(), "the consultation never rang");

    // The consultation is **not** answered: the target has not picked
    // up yet. This is the whole point of the feature.
    assert!(
        !seen
            .iter()
            .any(|event| event.call_id == consultation_call_id
                && event.event_type == CallEventType::CallAnswered),
        "the consultation was answered before the transfer was completed"
    );

    // The user completes the transfer now.
    stack
        .calls
        .transfer_attended(&account.id, &source_call_id, &consultation_call_id)
        .expect("the transfer did not start");

    // The peer saw the REFER, and it names the consultation — with the
    // tag the `180 Ringing` carried, which is what lets the server
    // find the dialog it is to take over.
    let refer = wait_for_refer(&observations).await;
    let replaces = header_value(&refer, "Replaces").expect("the REFER had no Replaces header");
    assert!(
        replaces.contains(&consultation_call_id),
        "the Replaces does not name the consultation's Call-ID: {replaces}"
    );
    assert!(
        replaces.contains("to-tag=peer-tag"),
        "the Replaces does not carry the dialog's to-tag: {replaces}"
    );

    // And the transfer goes on: the server reports the progress over
    // the NOTIFY (RFC 3515 §3.4), and the original leg is released
    // when it says the new leg is answered.
    let progress =
        wait_for_progress(&mut seen, &mut stack.calls, 200, Duration::from_secs(5)).await;
    let progress = progress.expect("the transfer progress was never reported");
    assert_eq!(
        progress.transfer_progress,
        Some(qsoftphone_core::sip::refer::TransferProgress::Answered),
        "the transfer did not reach the answered state"
    );
}

/// A REFER that points at a call the server has not named is refused.
///
/// The `Replaces` names the dialog by its Call-ID and its two tags
/// (RFC 3891 §3.1), and the peer's tag arrives with the `180
/// Ringing`. A `Replaces` without it names no dialog, and the server
/// answers "no route to target" — the fault M6.3 was about.
#[tokio::test]
async fn a_refer_to_a_call_that_has_not_rung_is_refused() {
    init_tracing();
    let socket = UdpSocket::bind("127.0.0.1:0").await.expect("peer socket");
    let peer_addr = socket.local_addr().expect("peer addr");
    let observations = Arc::new(Mutex::new(Observations::default()));
    tokio::spawn(run_peer(
        socket,
        observations.clone(),
        PeerOptions {
            call_back: false,
            dtmf: None,
            challenge_invite: false,
            transfer: Some(TransferScript::Answered),
            public_address: None,
            bye_status: None,
            early_media: false,
            cancel_the_call: false,
            ring_for: None,
            // The peer rings late, so the consultation really has not
            // been named when the transfer is attempted.
            ring_delay: Some(800),
            blf_state: None,
        },
    ));

    let handle = tokio::runtime::Handle::current();
    let mut stack = stack(handle);

    let mut account = Account::new("Office", peer_addr.to_string(), "104", "secret", "udp");
    account.id = "acct-transfer-unnamed".to_string();
    stack.registrations.start(&account, 0);

    tokio::time::sleep(Duration::from_millis(300)).await;

    let mut seen = Vec::new();
    let source_call_id = stack
        .calls
        .place_call(&account.id, "100")
        .expect("the original call was placed");
    let answered = wait_for(
        &mut seen,
        &mut stack.calls,
        CallEventType::CallAnswered,
        Duration::from_secs(5),
    )
    .await;
    assert!(answered.is_some(), "the original call was never answered");

    // A consultation that has been placed but has not rung: the server
    // has not named it, so there is no dialog to point at.
    let consultation_call_id = stack
        .calls
        .place_call(&account.id, "200")
        .expect("the consultation was placed");

    let error = stack
        .calls
        .transfer_attended(&account.id, &source_call_id, &consultation_call_id)
        .expect_err("the transfer was started for a call that has not rung");
    assert_eq!(
        error,
        qsoftphone_sip::CallError::ConsultationNotNamed,
        "the wrong reason was given: {error}"
    );

    // And nothing went out on the wire.
    assert!(
        !observations
            .lock()
            .expect("observations lock")
            .saw_request("REFER"),
        "the REFER went out anyway"
    );
}

/// The DTMF digits the peer received, in order.
///
/// The feature code is typed one digit at a time, and the digits come
/// back as the body of an `INFO` request.
async fn wait_for_dtmf(observations: &Arc<Mutex<Observations>>) -> String {
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        {
            let observations = observations.lock().expect("observations lock");
            let digits: String = observations
                .raw
                .iter()
                .filter(|message| message.starts_with("INFO "))
                .filter_map(|message| {
                    let body = message.split("\r\n\r\n").nth(1)?;
                    body.lines().find_map(|line| {
                        line.strip_prefix("Signal=").map(|digit| digit.to_string())
                    })
                })
                .collect();
            // The `#` ends the sequence.
            if digits.ends_with('#') {
                return digits;
            }
        }
        if std::time::Instant::now() >= deadline {
            panic!("the peer never received the feature code");
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// A transfer by the PBX's feature code: the code is typed as DTMF
/// and the server does the work — the way a hardware phone's transfer
/// key works.
#[tokio::test]
async fn a_feature_code_transfer_is_typed_as_dtmf() {
    init_tracing();
    let socket = UdpSocket::bind("127.0.0.1:0").await.expect("peer socket");
    let peer_addr = socket.local_addr().expect("peer addr");
    let observations = Arc::new(Mutex::new(Observations::default()));
    tokio::spawn(run_peer(
        socket,
        observations.clone(),
        PeerOptions {
            call_back: false,
            dtmf: None,
            challenge_invite: false,
            transfer: None,
            public_address: None,
            bye_status: None,
            early_media: false,
            cancel_the_call: false,
            ring_for: None,
            ring_delay: None,
            blf_state: None,
        },
    ));

    let handle = tokio::runtime::Handle::current();
    let mut stack = stack(handle);

    let mut account = Account::new("Office", peer_addr.to_string(), "104", "secret", "udp");
    account.id = "acct-transfer-feature-code".to_string();
    stack.registrations.start(&account, 0);

    tokio::time::sleep(Duration::from_millis(300)).await;

    let mut seen = Vec::new();
    let call_id = stack
        .calls
        .place_call(&account.id, "100")
        .expect("the call was placed");
    let answered = wait_for(
        &mut seen,
        &mut stack.calls,
        CallEventType::CallAnswered,
        Duration::from_secs(5),
    )
    .await;
    assert!(answered.is_some(), "the call was never answered");

    stack
        .calls
        .transfer_by_feature_code(&account.id, &call_id, "*2", "6666")
        .expect("the transfer did not start");

    // The code, the target and the terminator — in that order.
    let digits = wait_for_dtmf(&observations).await;
    assert_eq!(
        digits, "*26666#",
        "the feature code was not typed as it should be"
    );

    // And no REFER went out: the server does the transfer.
    assert!(
        !observations
            .lock()
            .expect("observations lock")
            .saw_request("REFER"),
        "the feature-code transfer also sent a REFER"
    );
}

/// A transfer that cannot reach its target fails, and the original
/// call stays up: the user may want to try again (§5.6).
#[tokio::test]
async fn a_transfer_that_cannot_route_leaves_the_original_call_up() {
    init_tracing();
    let socket = UdpSocket::bind("127.0.0.1:0").await.expect("peer socket");
    let peer_addr = socket.local_addr().expect("peer addr");
    let observations = Arc::new(Mutex::new(Observations::default()));
    tokio::spawn(run_peer(
        socket,
        observations.clone(),
        PeerOptions {
            call_back: false,
            dtmf: None,
            challenge_invite: false,
            transfer: Some(TransferScript::NoRoute),
            public_address: None,
            bye_status: None,
            early_media: false,
            cancel_the_call: false,
            ring_for: None,
            ring_delay: None,
            blf_state: None,
        },
    ));

    let handle = tokio::runtime::Handle::current();
    let mut stack = stack(handle);

    let mut account = Account::new("Office", peer_addr.to_string(), "104", "secret", "udp");
    account.id = "acct-transfer-noroute".to_string();
    stack.registrations.start(&account, 0);

    tokio::time::sleep(Duration::from_millis(300)).await;

    let mut seen = Vec::new();
    let call_id = stack
        .calls
        .place_call(&account.id, "100")
        .expect("the call was placed");
    let answered = wait_for(
        &mut seen,
        &mut stack.calls,
        CallEventType::CallAnswered,
        Duration::from_secs(5),
    )
    .await;
    assert!(answered.is_some(), "the call was never answered");

    stack
        .calls
        .transfer_blind(&account.id, &call_id, "999")
        .expect("the transfer did not start");

    // The peer could not route to the target.
    let progress =
        wait_for_progress(&mut seen, &mut stack.calls, 100, Duration::from_secs(5)).await;
    let progress = progress.expect("the failure was never reported");
    assert_eq!(
        progress.transfer_progress,
        Some(qsoftphone_core::sip::refer::TransferProgress::Failed),
        "a transfer that could not route was not reported as failed"
    );
    assert_eq!(progress.transfer_reason, "noresource");

    // The original call is still up: a transfer that failed must not
    // take the call down with it.
    assert!(
        !seen
            .iter()
            .any(|event| event.event_type == CallEventType::CallEnded),
        "the original call was taken down by a transfer that failed"
    );
    assert!(
        !observations
            .lock()
            .expect("observations lock")
            .saw_request("BYE"),
        "the original leg was hung up by a transfer that failed"
    );
}

/// A call can be put on hold, and taken off hold again.
///
/// The hold is a **re-INVITE** whose SDP says `a=sendonly` (RFC 3264
/// §5): "I will send, I will not receive". The peer answers
/// `a=recvonly` and stops sending, so the far end goes quiet — which
/// is what an attended transfer needs, because the user must not hear
/// the other party while the consultation is ringing.
#[tokio::test]
async fn a_call_can_be_held() {
    init_tracing();
    let socket = UdpSocket::bind("127.0.0.1:0").await.expect("peer socket");
    let peer_addr = socket.local_addr().expect("peer addr");
    let observations = Arc::new(Mutex::new(Observations::default()));
    tokio::spawn(run_peer(
        socket,
        observations.clone(),
        PeerOptions {
            call_back: false,
            dtmf: None,
            challenge_invite: false,
            transfer: None,
            public_address: None,
            bye_status: None,
            early_media: false,
            cancel_the_call: false,
            ring_for: None,
            ring_delay: None,
            blf_state: None,
        },
    ));

    let handle = tokio::runtime::Handle::current();
    let mut stack = stack(handle);

    let mut account = Account::new("Office", peer_addr.to_string(), "104", "secret", "udp");
    account.id = "acct-loopback-hold".to_string();
    stack.registrations.start(&account, 0);

    tokio::time::sleep(Duration::from_millis(300)).await;

    let mut seen = Vec::new();
    let call_id = stack
        .calls
        .place_call(&account.id, "100")
        .expect("the call was placed");

    // The call has to be up before it can be held.
    let answered = wait_for(
        &mut seen,
        &mut stack.calls,
        CallEventType::CallAnswered,
        Duration::from_secs(5),
    )
    .await;
    assert!(answered.is_some(), "the call was never answered");

    // The hold.
    stack
        .calls
        .hold(&account.id, &call_id)
        .expect("the call was not held");

    // The peer sees a re-INVITE that says `a=sendonly`.
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    let hold = loop {
        let invites = observations
            .lock()
            .expect("observations lock")
            .requests("INVITE");
        // The first INVITE is the call itself; the hold is the second.
        if let Some(hold) = invites.get(1) {
            break hold.clone();
        }
        if std::time::Instant::now() >= deadline {
            panic!("the peer never got the re-INVITE that holds the call");
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    };

    assert!(
        hold.contains("a=sendonly"),
        "the hold does not say sendonly:\n{hold}"
    );
    // The codec must not change under the call: the re-offer names the
    // one that was negotiated and nothing else.
    assert!(
        hold.contains("m=audio"),
        "the hold has no media line:\n{hold}"
    );

    // And the peer answers `a=recvonly` — it stops sending.
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        let answers = observations.lock().expect("observations lock").sent.clone();
        if let Some(answer) = answers.iter().find(|answer| answer.contains("200 OK")) {
            assert!(
                answer.contains("a=recvonly"),
                "the peer did not stop sending:\n{answer}"
            );
            break;
        }
        if std::time::Instant::now() >= deadline {
            panic!("the peer never answered the hold");
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    // Off hold: the re-INVITE goes back to `a=sendrecv`.
    stack
        .calls
        .unhold(&account.id, &call_id)
        .expect("the call was not taken off hold");

    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        let invites = observations
            .lock()
            .expect("observations lock")
            .requests("INVITE");
        if invites.len() >= 3 {
            let unhold = invites.last().expect("the unhold");
            assert!(
                unhold.contains("a=sendrecv"),
                "the unhold does not say sendrecv:\n{unhold}"
            );
            break;
        }
        if std::time::Instant::now() >= deadline {
            panic!("the peer never got the re-INVITE that takes the call off hold");
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// A call that is still ringing can be redirected.
///
/// A `302 Moved Temporarily` (RFC 3261 §21.4): the caller is told to
/// try somewhere else, and where is in the `Contact`. The call is
/// never answered — this is forwarding without answering, and it is
/// what the user asked for: a button on the ringing screen that hands
/// the call on.
#[tokio::test]
async fn a_ringing_call_can_be_redirected() {
    init_tracing();
    let socket = UdpSocket::bind("127.0.0.1:0").await.expect("peer socket");
    let peer_addr = socket.local_addr().expect("peer addr");
    let observations = Arc::new(Mutex::new(Observations::default()));
    tokio::spawn(run_peer(
        socket,
        observations.clone(),
        PeerOptions {
            call_back: true,
            dtmf: None,
            challenge_invite: false,
            transfer: None,
            public_address: None,
            bye_status: None,
            early_media: false,
            cancel_the_call: false,
            ring_for: None,
            ring_delay: None,
            blf_state: None,
        },
    ));

    let handle = tokio::runtime::Handle::current();
    let mut stack = stack(handle);

    let mut account = Account::new("Office", peer_addr.to_string(), "104", "secret", "udp");
    account.id = "acct-loopback-redirect".to_string();
    stack.registrations.start(&account, 0);

    let mut seen = Vec::new();
    let incoming = wait_for(
        &mut seen,
        &mut stack.calls,
        CallEventType::IncomingCall,
        Duration::from_secs(5),
    )
    .await;
    let incoming = incoming.expect("the incoming call never arrived");

    // The call is still ringing: it has not been answered.
    stack
        .calls
        .redirect(&account.id, &incoming.call_id, "200")
        .expect("the call was not redirected");

    // The peer got the `302`.
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    let redirect = loop {
        let found = observations
            .lock()
            .expect("observations lock")
            .response("302 Moved Temporarily");
        if let Some(raw) = found {
            break raw;
        }
        if std::time::Instant::now() >= deadline {
            panic!("the peer never got the 302 Moved Temporarily");
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    };

    // The `Contact` says where to try next — and it names the target
    // that was asked for, not the caller's own address.
    let contact = header_value(&redirect, "Contact").expect("the 302 had no Contact");
    assert!(
        contact.contains("sip:200@"),
        "the 302 does not name the target: {contact}"
    );

    // It is a response to the call, and it is a final one: the call is
    // over, and it was never answered.
    assert!(
        !observations
            .lock()
            .expect("observations lock")
            .saw_request("BYE"),
        "the redirected call was hung up instead of redirected"
    );
}

/// A `180 Ringing` that carries no SDP means there is nothing to hear.
///
/// The peer's `180 Ringing` is the ordinary one: no body at all. The
/// softphone has to know that, because it makes its own ringback when
/// there is nothing to hear — and it must not make one over the peer's
/// own audio.
#[tokio::test]
async fn a_ringing_without_media_says_that_there_is_none() {
    init_tracing();
    let socket = UdpSocket::bind("127.0.0.1:0").await.expect("peer socket");
    let peer_addr = socket.local_addr().expect("peer addr");
    let observations = Arc::new(Mutex::new(Observations::default()));
    tokio::spawn(run_peer(
        socket,
        observations.clone(),
        PeerOptions {
            call_back: false,
            dtmf: None,
            challenge_invite: false,
            transfer: None,
            public_address: None,
            bye_status: None,
            early_media: false,
            cancel_the_call: false,
            ring_for: None,
            ring_delay: None,
            blf_state: None,
        },
    ));

    let handle = tokio::runtime::Handle::current();
    let mut stack = stack(handle);

    let mut account = Account::new("Office", peer_addr.to_string(), "104", "secret", "udp");
    account.id = "acct-loopback-no-early-media".to_string();
    stack.registrations.start(&account, 0);

    tokio::time::sleep(Duration::from_millis(300)).await;

    let mut seen = Vec::new();
    stack
        .calls
        .place_call(&account.id, "100")
        .expect("the call was placed");

    let ringing = wait_for(
        &mut seen,
        &mut stack.calls,
        CallEventType::OutgoingCallRinging,
        Duration::from_secs(5),
    )
    .await;
    let ringing = ringing.expect("the call never rang");
    assert!(
        !ringing.has_early_media,
        "a 180 Ringing without an SDP was taken for early media"
    );
}

/// A `180 Ringing` that carries the SDP means the peer is sending
/// audio already.
///
/// That is early media, and it is the one case where the softphone
/// must keep quiet: the peer is already making a sound, and a ringback
/// on top of it is two rings at once.
#[tokio::test]
async fn a_ringing_with_media_says_that_there_is_some() {
    init_tracing();
    let socket = UdpSocket::bind("127.0.0.1:0").await.expect("peer socket");
    let peer_addr = socket.local_addr().expect("peer addr");
    let observations = Arc::new(Mutex::new(Observations::default()));
    tokio::spawn(run_peer(
        socket,
        observations.clone(),
        PeerOptions {
            call_back: false,
            dtmf: None,
            challenge_invite: false,
            transfer: None,
            public_address: None,
            bye_status: None,
            early_media: true,
            cancel_the_call: false,
            ring_for: None,
            ring_delay: None,
            blf_state: None,
        },
    ));

    let handle = tokio::runtime::Handle::current();
    let mut stack = stack(handle);

    let mut account = Account::new("Office", peer_addr.to_string(), "104", "secret", "udp");
    account.id = "acct-loopback-early-media".to_string();
    stack.registrations.start(&account, 0);

    tokio::time::sleep(Duration::from_millis(300)).await;

    let mut seen = Vec::new();
    stack
        .calls
        .place_call(&account.id, "100")
        .expect("the call was placed");

    let ringing = wait_for(
        &mut seen,
        &mut stack.calls,
        CallEventType::OutgoingCallRinging,
        Duration::from_secs(5),
    )
    .await;
    let ringing = ringing.expect("the call never rang");
    assert!(
        ringing.has_early_media,
        "a 180 Ringing with an SDP was not taken for early media"
    );
}

/// A call the far end cancels while it is still ringing.
///
/// This is the consultation of an attended transfer dying: the party
/// that was called gives up, the server cancels the INVITE and answers
/// it with `487 Request Terminated` (RFC 3261 §9.2). The client has to
/// hear about it — a consultation that died in silence leaves the
/// transfer popup offering a transfer to a call that is gone.
#[tokio::test]
async fn a_call_the_peer_cancels_ends() {
    init_tracing();
    let socket = UdpSocket::bind("127.0.0.1:0").await.expect("peer socket");
    let peer_addr = socket.local_addr().expect("peer addr");
    let observations = Arc::new(Mutex::new(Observations::default()));
    tokio::spawn(run_peer(
        socket,
        observations.clone(),
        PeerOptions {
            call_back: false,
            dtmf: None,
            challenge_invite: false,
            transfer: None,
            public_address: None,
            bye_status: None,
            early_media: false,
            cancel_the_call: true,
            ring_for: None,
            ring_delay: None,
            blf_state: None,
        },
    ));

    let handle = tokio::runtime::Handle::current();
    let mut stack = stack(handle);

    let mut account = Account::new("Office", peer_addr.to_string(), "104", "secret", "udp");
    account.id = "acct-loopback-cancelled".to_string();
    stack.registrations.start(&account, 0);

    tokio::time::sleep(Duration::from_millis(300)).await;

    let mut seen = Vec::new();
    stack
        .calls
        .place_call(&account.id, "100")
        .expect("the call was placed");

    let ended = wait_for(
        &mut seen,
        &mut stack.calls,
        CallEventType::CallEnded,
        Duration::from_secs(5),
    )
    .await;
    let ended = ended.expect("the cancelled call never ended");
    assert_eq!(
        ended.sip_code, 487,
        "the call did not end with the code the server gave"
    );

    // And the call was never up: the peer cancelled it while it was
    // still ringing.
    assert!(
        !seen
            .iter()
            .any(|event| event.event_type == CallEventType::CallAnswered),
        "a call that was cancelled was reported as answered"
    );
}
