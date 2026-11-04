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

/// A minimal SDP for the peer to answer with.
const PEER_SDP: &str = "v=0\r\n\
o=- 1 1 IN IP4 127.0.0.1\r\n\
s=-\r\n\
c=IN IP4 127.0.0.1\r\n\
t=0 0\r\n\
m=audio 4000 RTP/AVP 0\r\n\
a=rtpmap:0 PCMU/8000\r\n";

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
                let message = response(&text, "200 OK", &[("Expires", "300")], "");
                socket
                    .send_to(message.as_bytes(), from)
                    .await
                    .expect("REGISTER reply");
            }
            "INVITE" => {
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
                let ringing = response(&text, "180 Ringing", &[], "");
                socket.send_to(ringing.as_bytes(), from).await.expect("180");
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
                let message = response(&text, "200 OK", &[], "");
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

/// The app's SIP stack: a registrar, a call manager and the bus
/// between them.
struct Stack {
    registrations: RegistrationManager,
    calls: CallManager,
}

fn stack(handle: tokio::runtime::Handle) -> Stack {
    let sessions = SessionRegistry::new();
    let (bus, receiver) = CallBus::new();
    Stack {
        registrations: RegistrationManager::new(handle.clone(), sessions.clone(), bus.clone()),
        calls: CallManager::new(handle, sessions, bus, receiver),
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
