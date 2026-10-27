//! A loopback registrar, to prove the registration path end to end.
//!
//! M1 is done when "the credential in §5.8 works against a real
//! server". This test is the closest thing to that that can run in CI:
//! a real UDP socket, a real SIP REGISTER, a real 401 challenge, a
//! real digest, a real retry — with a registrar written by hand so
//! the challenge can use exactly the algorithm token we want to test.
//!
//! The digest is verified **independently** of rsipstack: the test
//! computes the RFC 7616 SHA-256 itself and compares. If the client
//! picked the wrong hash function, or hashed the wrong strings, this
//! fails.

use std::collections::HashMap;

use qsoftphone_sip::digest::DigestInterop;
use qsoftphone_sip::endpoint::build_endpoint;
use rsipstack::dialog::authenticate::Credential;
use rsipstack::dialog::registration::Registration;
use rsipstack::platform::CancellationToken;
use rsipstack::sip::{StatusCode, Transport};
use tokio::net::UdpSocket;

const NONCE: &str = "0123456789abcdef";
const USERNAME: &str = "104";
const PASSWORD: &str = "s3cret";

/// What the registrar saw.
struct Observations {
    first_request: String,
    second_request: String,
    authorization: String,
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

/// The Request-URI of a raw SIP request.
fn request_uri(message: &str) -> String {
    message
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .unwrap_or_default()
        .to_string()
}

/// Split a digest header's parameters into a map.
fn digest_params(value: &str) -> HashMap<String, String> {
    let mut out = HashMap::new();
    let mut rest = value;
    while let Some(eq) = rest.find('=') {
        let key = rest[..eq]
            .rsplit([' ', ','])
            .next()
            .unwrap_or_default()
            .trim()
            .to_ascii_lowercase();
        let after = &rest[eq + 1..];
        let (value, consumed) = if let Some(stripped) = after.strip_prefix('"') {
            match stripped.find('"') {
                Some(end) => (&stripped[..end], 1 + end + 1),
                None => (stripped, stripped.len()),
            }
        } else {
            let end = after.find(',').unwrap_or(after.len());
            (&after[..end], end)
        };
        if !key.is_empty() {
            out.insert(key, value.to_string());
        }
        rest = &after[consumed..];
        rest = rest.trim_start_matches([' ', ',']);
    }
    out
}

fn sha256_hex(input: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(input.as_bytes());
    hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn md5_hex(input: &str) -> String {
    use md5::Md5;
    use sha2::Digest as _;
    let mut hasher = Md5::new();
    hasher.update(input.as_bytes());
    hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// Build a response that echoes the request's transaction headers.
fn response(request: &str, status: &str, extra: &[(&str, &str)]) -> String {
    let mut out = format!("SIP/2.0 {status}\r\n");
    for name in ["Via", "From", "To", "Call-ID", "CSeq"] {
        if let Some(value) = header_value(request, name) {
            out.push_str(&format!("{name}: {value}\r\n"));
        }
    }
    for (name, value) in extra {
        out.push_str(&format!("{name}: {value}\r\n"));
    }
    out.push_str("Content-Length: 0\r\n\r\n");
    out
}

/// A registrar that challenges once, then accepts.
async fn run_registrar(
    socket: UdpSocket,
    algorithm: &'static str,
    tx: tokio::sync::oneshot::Sender<Observations>,
) {
    let mut buf = vec![0u8; 65535];

    // The first REGISTER.
    let (len, peer) = socket.recv_from(&mut buf).await.expect("first REGISTER");
    let first_request = String::from_utf8_lossy(&buf[..len]).to_string();
    let challenge =
        format!("Digest realm=\"test\", nonce=\"{NONCE}\", algorithm={algorithm}, qop=\"auth\"");
    let reply = response(
        &first_request,
        "401 Unauthorized",
        &[("WWW-Authenticate", &challenge)],
    );
    socket.send_to(reply.as_bytes(), peer).await.expect("401");

    // The authenticated REGISTER.
    let (len, peer) = socket.recv_from(&mut buf).await.expect("retry REGISTER");
    let second_request = String::from_utf8_lossy(&buf[..len]).to_string();
    let authorization = header_value(&second_request, "Authorization").unwrap_or_default();
    let reply = response(&second_request, "200 OK", &[("Expires", "300")]);
    socket.send_to(reply.as_bytes(), peer).await.expect("200");

    let _ = tx.send(Observations {
        first_request,
        second_request,
        authorization,
    });
}

/// Register against the loopback registrar and check what happened.
async fn register_against(algorithm: &'static str) -> (rsipstack::sip::Response, Observations) {
    let socket = UdpSocket::bind("127.0.0.1:0")
        .await
        .expect("registrar socket");
    let registrar_addr = socket.local_addr().expect("registrar addr");
    let (tx, rx) = tokio::sync::oneshot::channel();
    tokio::spawn(run_registrar(socket, algorithm, tx));

    let endpoint = build_endpoint(
        Transport::Udp,
        0,
        CancellationToken::new(),
        Some(Box::new(DigestInterop::new())),
    )
    .await
    .expect("endpoint");

    let inner = endpoint.inner.clone();
    tokio::spawn(async move {
        let _ = inner.serve().await;
    });

    let mut registration = Registration::new(
        endpoint.inner.clone(),
        Some(Credential {
            username: USERNAME.to_string(),
            password: PASSWORD.to_string(),
            realm: None,
            auth_username: None,
        }),
    );

    let uri = format!("sip:{registrar_addr}");
    let response = registration
        .register(uri.parse().expect("registrar uri"), Some(300))
        .await
        .expect("the REGISTER transaction");

    let observations = rx.await.expect("registrar observations");
    (response, observations)
}

#[tokio::test]
async fn registers_against_a_sha256_registrar() {
    let (response, seen) = register_against("SHA-256").await;

    assert_eq!(response.status_code, StatusCode::OK);
    assert!(seen.first_request.starts_with("REGISTER "));
    assert!(seen.second_request.contains("Authorization:"));

    let params = digest_params(&seen.authorization);
    assert_eq!(params.get("username").map(String::as_str), Some(USERNAME));
    assert_eq!(params.get("nonce").map(String::as_str), Some(NONCE));
    assert_eq!(params.get("realm").map(String::as_str), Some("test"));

    // The client echoes the server's own algorithm token.
    assert_eq!(
        params.get("algorithm").map(String::as_str),
        Some("SHA-256"),
        "the RFC 7616 spelling must survive the round trip"
    );

    // The digest is verified independently of rsipstack.
    let uri = request_uri(&seen.second_request);
    assert_eq!(params.get("uri").map(String::as_str), Some(uri.as_str()));

    let nc: u32 = params.get("nc").and_then(|v| v.parse().ok()).expect("nc");
    let cnonce = params.get("cnonce").expect("cnonce");
    let qop = params.get("qop").expect("qop");

    let ha1 = sha256_hex(&format!("{USERNAME}:test:{PASSWORD}"));
    let ha2 = sha256_hex(&format!("REGISTER:{uri}"));
    let expected = sha256_hex(&format!("{ha1}:{NONCE}:{nc:08}:{cnonce}:{qop}:{ha2}"));
    assert_eq!(
        params.get("response").map(String::as_str),
        Some(expected.as_str()),
        "the digest must be the SHA-256 of the RFC 7616 strings"
    );
}

#[tokio::test]
async fn registers_against_an_md5_registrar() {
    let (response, seen) = register_against("MD5").await;

    assert_eq!(response.status_code, StatusCode::OK);

    let params = digest_params(&seen.authorization);
    assert_eq!(params.get("algorithm").map(String::as_str), Some("MD5"));

    let uri = request_uri(&seen.second_request);
    let nc: u32 = params.get("nc").and_then(|v| v.parse().ok()).expect("nc");
    let cnonce = params.get("cnonce").expect("cnonce");
    let qop = params.get("qop").expect("qop");

    let ha1 = md5_hex(&format!("{USERNAME}:test:{PASSWORD}"));
    let ha2 = md5_hex(&format!("REGISTER:{uri}"));
    let expected = md5_hex(&format!("{ha1}:{NONCE}:{nc:08}:{cnonce}:{qop}:{ha2}"));
    assert_eq!(
        params.get("response").map(String::as_str),
        Some(expected.as_str()),
        "the digest must be the MD5 of the RFC 2617 strings"
    );
}

#[tokio::test]
async fn the_challenge_is_answered_with_the_right_credentials() {
    // The first REGISTER goes out without credentials, and the retry
    // carries them.
    let (_, seen) = register_against("SHA-256").await;

    assert!(
        !seen
            .first_request
            .to_ascii_lowercase()
            .contains("authorization:"),
        "the first REGISTER must not be authorised"
    );
    assert!(seen.second_request.contains("Authorization:"));
    assert!(
        header_value(&seen.second_request, "Authorization")
            .unwrap()
            .to_ascii_lowercase()
            .contains(&format!("username=\"{USERNAME}\""))
    );
}

#[tokio::test]
async fn the_retry_bumps_the_cseq() {
    let (_, seen) = register_against("SHA-256").await;

    let first = header_value(&seen.first_request, "CSeq").unwrap_or_default();
    let second = header_value(&seen.second_request, "CSeq").unwrap_or_default();

    let first_seq: u32 = first
        .split_whitespace()
        .next()
        .unwrap_or("0")
        .parse()
        .unwrap();
    let second_seq: u32 = second
        .split_whitespace()
        .next()
        .unwrap_or("0")
        .parse()
        .unwrap();
    assert_eq!(second_seq, first_seq + 1, "the retry must bump the CSeq");
}
