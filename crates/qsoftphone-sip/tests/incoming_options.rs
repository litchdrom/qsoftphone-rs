//! The registrar's OPTIONS keepalive has to be answered.
//!
//! Asterisk pings a registered endpoint with SIP OPTIONS. A UA that
//! does not answer is marked *unreachable*, and the calls that follow
//! fail even though the registration itself succeeded — the exact
//! symptom this test pins down.
//!
//! The test speaks real SIP over a real UDP socket: it builds an
//! OPTIONS request the way Asterisk builds one, sends it to the
//! endpoint, and checks the response.

use std::time::Duration;

use qsoftphone_core::model::account::Account;
use qsoftphone_sip::endpoint::build_endpoint;
use qsoftphone_sip::incoming::serve;
use qsoftphone_sip::{AccountSession, CallBus, PublicAddress};
use rsipstack::platform::CancellationToken;
use rsipstack::sip::Transport;
use rsipstack::transaction::Endpoint;
use tokio::net::UdpSocket;

/// The OPTIONS request Asterisk sends to qualify an endpoint.
fn options_request() -> String {
    concat!(
        "OPTIONS sip:104@example.com SIP/2.0\r\n",
        "Via: SIP/2.0/UDP 10.0.0.9:5062;branch=z9hG4bK-qualify-1\r\n",
        "Max-Forwards: 70\r\n",
        "To: <sip:104@example.com>\r\n",
        "From: <sip:asterisk@example.com>;tag=qualify-1\r\n",
        "Call-ID: qualify-call-1@example.com\r\n",
        "CSeq: 1 OPTIONS\r\n",
        "Contact: <sip:asterisk@10.0.0.9:5062>\r\n",
        "Content-Length: 0\r\n",
        "\r\n",
    )
    .to_string()
}

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

/// Serve the requests that arrive on an endpoint.
///
/// The session and the bus are what the call layer needs; the
/// keepalive does not use either, but the same loop serves both.
fn serve_incoming(endpoint: &Endpoint, token: &CancellationToken) {
    let incoming = endpoint.incoming_transactions().expect("incoming channel");

    let account = Account::new("Office", "example.com", "104", "pw", "udp");
    let session = AccountSession::new(
        account,
        Endpoint {
            inner: endpoint.inner.clone(),
        },
        PublicAddress::default(),
    );
    let (bus, _receiver) = CallBus::new();

    let token = token.clone();
    tokio::spawn(async move {
        serve(incoming, token, session, bus).await;
    });
}

/// Bring an endpoint up and answer whatever arrives on it.
async fn answering_endpoint() -> (rsipstack::transaction::Endpoint, u16, CancellationToken) {
    let token = CancellationToken::new();
    let endpoint = build_endpoint(Transport::Udp, 0, token.clone(), None)
        .await
        .expect("endpoint");

    let inner = endpoint.inner.clone();
    tokio::spawn(async move {
        let _ = inner.serve().await;
    });

    serve_incoming(&endpoint, &token);

    let port = endpoint
        .get_addrs()
        .first()
        .and_then(|addr| addr.addr.port)
        .map(|port| port.value())
        .expect("the endpoint has a local address");

    (endpoint, port, token)
}

#[tokio::test]
async fn an_options_keepalive_is_answered_with_200_ok() {
    let (_endpoint, port, token) = answering_endpoint().await;

    let socket = UdpSocket::bind("127.0.0.1:0").await.expect("client socket");
    socket
        .send_to(options_request().as_bytes(), ("127.0.0.1", port))
        .await
        .expect("send OPTIONS");

    let mut buf = vec![0u8; 65535];
    let (len, _) = tokio::time::timeout(Duration::from_secs(5), socket.recv_from(&mut buf))
        .await
        .expect("the OPTIONS was answered in time")
        .expect("recv");
    let response = String::from_utf8_lossy(&buf[..len]).to_string();

    assert!(
        response.starts_with("SIP/2.0 200 OK"),
        "the keepalive must be answered: {response}"
    );

    // The response has to mirror the request, or the transaction on
    // the other side never matches (RFC 3261 §8.2.6).
    assert_eq!(
        header_value(&response, "Call-ID").as_deref(),
        Some("qualify-call-1@example.com"),
        "{response}"
    );
    assert_eq!(
        header_value(&response, "CSeq").as_deref(),
        Some("1 OPTIONS"),
        "{response}"
    );
    assert!(
        header_value(&response, "Via").is_some_and(|via| via.contains("z9hG4bK-qualify-1")),
        "{response}"
    );
    assert!(
        header_value(&response, "From").is_some_and(|from| from.contains("qualify-1")),
        "{response}"
    );
    // §8.1.3.7: a 2xx carries a tag on the To header.
    assert!(
        header_value(&response, "To").is_some_and(|to| to.contains("tag=")),
        "{response}"
    );

    token.cancel();
}

#[tokio::test]
async fn a_method_we_do_not_implement_yet_gets_an_answer() {
    let (_endpoint, port, token) = answering_endpoint().await;

    // A NOTIFY is not implemented in M1. The point is that the peer
    // gets a status code instead of a timeout.
    let notify = concat!(
        "NOTIFY sip:104@example.com SIP/2.0\r\n",
        "Via: SIP/2.0/UDP 10.0.0.9:5062;branch=z9hG4bK-notify-1\r\n",
        "Max-Forwards: 70\r\n",
        "To: <sip:104@example.com>\r\n",
        "From: <sip:asterisk@example.com>;tag=notify-1\r\n",
        "Call-ID: notify-call-1@example.com\r\n",
        "CSeq: 1 NOTIFY\r\n",
        "Content-Length: 0\r\n",
        "\r\n",
    );

    let socket = UdpSocket::bind("127.0.0.1:0").await.expect("client socket");
    socket
        .send_to(notify.as_bytes(), ("127.0.0.1", port))
        .await
        .expect("send NOTIFY");

    let mut buf = vec![0u8; 65535];
    let (len, _) = tokio::time::timeout(Duration::from_secs(5), socket.recv_from(&mut buf))
        .await
        .expect("the NOTIFY was answered in time")
        .expect("recv");
    let response = String::from_utf8_lossy(&buf[..len]).to_string();

    assert!(
        response.starts_with("SIP/2.0 501 Not Implemented"),
        "an unimplemented method must still be answered: {response}"
    );

    token.cancel();
}

#[tokio::test]
async fn a_tcp_endpoint_answers_an_options_on_a_new_connection() {
    // The registrar may open a *new* TCP connection to the Contact
    // instead of reusing the one we dialled out on. A TCP endpoint
    // that does not listen would never see the keepalive.
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let token = CancellationToken::new();
    let endpoint = build_endpoint(Transport::Tcp, 0, token.clone(), None)
        .await
        .expect("endpoint");

    let inner = endpoint.inner.clone();
    tokio::spawn(async move {
        let _ = inner.serve().await;
    });

    serve_incoming(&endpoint, &token);

    // The listener is bound by the transport layer when the endpoint
    // starts serving, so the port is only known once it is up.
    let port = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let Some(port) = endpoint
                .get_addrs()
                .iter()
                .find(|addr| addr.r#type == Some(Transport::Tcp))
                .and_then(|addr| addr.addr.port)
                .map(|port| port.value())
                .filter(|port| *port != 0)
            {
                return port;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("the TCP listener came up");

    let mut stream = tokio::net::TcpStream::connect(("127.0.0.1", port))
        .await
        .expect("connect");
    stream
        .write_all(options_request().as_bytes())
        .await
        .expect("write OPTIONS");

    let mut buf = vec![0u8; 65535];
    let len = tokio::time::timeout(Duration::from_secs(5), stream.read(&mut buf))
        .await
        .expect("the OPTIONS was answered in time")
        .expect("read");
    let response = String::from_utf8_lossy(&buf[..len]).to_string();

    assert!(
        response.starts_with("SIP/2.0 200 OK"),
        "the keepalive must be answered over TCP too: {response}"
    );

    token.cancel();
}
