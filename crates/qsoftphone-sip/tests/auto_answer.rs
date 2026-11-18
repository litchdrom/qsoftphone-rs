//! The auto answer: an incoming call that answers itself.
//!
//! The test speaks real SIP over a real UDP socket: it sends an INVITE
//! the way a PBX sends one, and it looks at what comes back. The
//! setting has three parts — a switch, a timer and an optional header
//! — and the two that decide are checked here against the wire: the
//! header decides whether the call is answered at all, and a call that
//! is not answered is a call that keeps ringing.

use std::time::Duration;

use qsoftphone_core::config::AutoAnswerConfig;
use qsoftphone_core::model::account::Account;
use qsoftphone_sip::endpoint::build_endpoint;
use qsoftphone_sip::incoming::serve;
use qsoftphone_sip::{AccountSession, CallBus, PublicAddress, SessionRegistry};
use rsipstack::platform::CancellationToken;
use rsipstack::sip::Transport;
use rsipstack::transaction::Endpoint;
use tokio::net::UdpSocket;

/// The offer a PBX sends.
const OFFER: &str = concat!(
    "v=0\r\n",
    "o=asterisk 1 1 IN IP4 127.0.0.1\r\n",
    "s=Asterisk PBX\r\n",
    "c=IN IP4 127.0.0.1\r\n",
    "t=0 0\r\n",
    "m=audio 40000 RTP/AVP 0 8\r\n",
    "a=rtpmap:0 PCMU/8000\r\n",
    "a=rtpmap:8 PCMA/8000\r\n",
    "a=ptime:20\r\n",
);

/// An INVITE, with the headers the test cares about.
///
/// The body is the offer above, and the `Content-Length` is its real
/// length: a length that does not match is a body the peer reads
/// wrongly, and a test that passes on a broken message proves nothing.
fn invite(tag: &str, extra: &str) -> String {
    format!(
        "INVITE sip:104@example.com SIP/2.0\r\n\
         Via: SIP/2.0/UDP 10.0.0.9:5062;branch=z9hG4bK-{tag}\r\n\
         Max-Forwards: 70\r\n\
         To: <sip:104@example.com>\r\n\
         From: <sip:asterisk@example.com>;tag={tag}\r\n\
         Call-ID: {tag}@example.com\r\n\
         CSeq: 1 INVITE\r\n\
         Contact: <sip:asterisk@10.0.0.9:5062>\r\n\
         {extra}\
         Content-Type: application/sdp\r\n\
         Content-Length: {length}\r\n\
         \r\n\
         {body}",
        length = OFFER.len(),
        body = OFFER,
    )
}

/// The responses that come back, until the deadline.
///
/// The order is not fixed: the `100 Trying` and the `180 Ringing` may
/// arrive in either order, and only the `200 OK` is the answer.
async fn responses(socket: &UdpSocket, within: Duration) -> Vec<String> {
    let mut buf = vec![0u8; 65535];
    let mut seen = Vec::new();
    let deadline = tokio::time::Instant::now() + within;
    loop {
        let left = deadline.saturating_duration_since(tokio::time::Instant::now());
        if left.is_zero() {
            break;
        }
        let Ok(Ok((len, _))) = tokio::time::timeout(left, socket.recv_from(&mut buf)).await else {
            break;
        };
        seen.push(String::from_utf8_lossy(&buf[..len]).to_string());
    }
    seen
}

/// Whether one of the responses is this status.
fn has(seen: &[String], status: &str) -> bool {
    seen.iter().any(|message| message.starts_with(status))
}

/// Bring an endpoint up, with the auto answer set as asked.
async fn answering_endpoint(
    auto_answer: AutoAnswerConfig,
) -> (rsipstack::transaction::Endpoint, u16, CancellationToken) {
    let token = CancellationToken::new();
    let endpoint = build_endpoint(Transport::Udp, 0, token.clone(), None)
        .await
        .expect("endpoint");

    let inner = endpoint.inner.clone();
    tokio::spawn(async move {
        let _ = inner.serve().await;
    });

    let incoming = endpoint.incoming_transactions().expect("incoming channel");
    let account = Account::new("Office", "example.com", "104", "pw", "udp");
    let registry = SessionRegistry::new();
    // The setting is global, and the session gets the handle that
    // carries it — the same handle the interface writes to.
    registry.set_auto_answer(auto_answer);
    let session = AccountSession::new(
        account,
        Endpoint {
            inner: endpoint.inner.clone(),
        },
        PublicAddress::default(),
        registry.media_config_handle(),
        registry.auto_answer_handle(),
    );
    let (bus, _receiver) = CallBus::new();

    let serve_token = token.clone();
    tokio::spawn(async move {
        serve(incoming, serve_token, session, bus).await;
    });

    let port = endpoint
        .get_addrs()
        .first()
        .and_then(|addr| addr.addr.port)
        .map(|port| port.value())
        .expect("the endpoint has a local address");

    (endpoint, port, token)
}

#[tokio::test]
async fn a_call_that_carries_the_header_is_answered_by_itself() {
    let (_endpoint, port, token) = answering_endpoint(AutoAnswerConfig {
        enabled: true,
        delay_sec: 0,
        header: "X-Auto-Answer".to_string(),
        header_value: String::new(),
    })
    .await;

    let socket = UdpSocket::bind("127.0.0.1:0").await.expect("client socket");
    socket
        .send_to(
            invite("auto-1", "X-Auto-Answer: agent-3\r\n").as_bytes(),
            ("127.0.0.1", port),
        )
        .await
        .expect("send INVITE");

    let seen = responses(&socket, Duration::from_secs(5)).await;

    assert!(
        has(&seen, "SIP/2.0 180 Ringing"),
        "the call must ring before it is answered:\n{seen:#?}"
    );
    assert!(
        has(&seen, "SIP/2.0 200 OK"),
        "the call must be answered by itself:\n{seen:#?}"
    );

    // The answer carries the media: the call is up, not answered with
    // an empty body.
    let answer = seen
        .iter()
        .find(|message| message.starts_with("SIP/2.0 200 OK"))
        .expect("the answer");
    assert!(
        answer.contains("m=audio"),
        "the answer carries no media:\n{answer}"
    );

    token.cancel();
}

#[tokio::test]
async fn a_call_without_the_header_keeps_ringing() {
    // The setting names a header, and the INVITE does not carry it: the
    // call is **not** answered by itself. The user still gets it.
    let (_endpoint, port, token) = answering_endpoint(AutoAnswerConfig {
        enabled: true,
        delay_sec: 0,
        header: "X-Auto-Answer".to_string(),
        header_value: String::new(),
    })
    .await;

    let socket = UdpSocket::bind("127.0.0.1:0").await.expect("client socket");
    socket
        .send_to(invite("auto-2", "").as_bytes(), ("127.0.0.1", port))
        .await
        .expect("send INVITE");

    let seen = responses(&socket, Duration::from_secs(3)).await;

    assert!(
        has(&seen, "SIP/2.0 180 Ringing"),
        "the call must ring:\n{seen:#?}"
    );
    assert!(
        !has(&seen, "SIP/2.0 200 OK"),
        "a call without the header must not be answered by itself:\n{seen:#?}"
    );

    token.cancel();
}

#[tokio::test]
async fn an_unconditional_auto_answer_answers_a_call_without_any_header() {
    // No header is named: every call is answered.
    let (_endpoint, port, token) = answering_endpoint(AutoAnswerConfig {
        enabled: true,
        delay_sec: 0,
        header: String::new(),
        header_value: String::new(),
    })
    .await;

    let socket = UdpSocket::bind("127.0.0.1:0").await.expect("client socket");
    socket
        .send_to(invite("auto-3", "").as_bytes(), ("127.0.0.1", port))
        .await
        .expect("send INVITE");

    let seen = responses(&socket, Duration::from_secs(5)).await;

    assert!(
        has(&seen, "SIP/2.0 200 OK"),
        "an unconditional auto answer must answer the call:\n{seen:#?}"
    );

    token.cancel();
}

#[tokio::test]
async fn an_auto_answer_that_is_off_keeps_the_call_ringing() {
    // The switch is off: nothing answers the call but the user.
    let (_endpoint, port, token) = answering_endpoint(AutoAnswerConfig::default()).await;

    let socket = UdpSocket::bind("127.0.0.1:0").await.expect("client socket");
    socket
        .send_to(
            invite("auto-4", "X-Auto-Answer: agent-3\r\n").as_bytes(),
            ("127.0.0.1", port),
        )
        .await
        .expect("send INVITE");

    let seen = responses(&socket, Duration::from_secs(3)).await;

    assert!(
        has(&seen, "SIP/2.0 180 Ringing"),
        "the call must ring:\n{seen:#?}"
    );
    assert!(
        !has(&seen, "SIP/2.0 200 OK"),
        "an auto answer that is off must not answer the call:\n{seen:#?}"
    );

    token.cancel();
}
