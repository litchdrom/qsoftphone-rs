//! §6 end to end: the account id has to be on the log line.
//!
//! The unit test in `qsoftphone-core` proves the first half — a span's
//! fields reach the formatter. This proves the second half: the
//! registration task really does run inside the account span, so the
//! lines it writes can be traced back to the account they belong to.
//!
//! It also exercises the other M1 promise — the registration state
//! comes back over a channel that the UI drains, never by blocking the
//! caller.

use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use qsoftphone_core::model::account::Account;
use qsoftphone_sip::{RegistrationManager, RegistrationState};
use tokio::net::UdpSocket;

/// A writer that keeps what it is given.
///
/// `Write` is implemented for `&Buffer` because `Arc<Buffer>` only
/// satisfies `MakeWriter` when `&Buffer: Write`.
#[derive(Clone, Default)]
struct Buffer(Arc<Mutex<Vec<u8>>>);

impl std::io::Write for &Buffer {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.lock().expect("buffer lock").extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// Install the capturing subscriber once and hand back the buffer.
///
/// The subscriber has to be the *global* one: the registration task
/// runs on another thread, where a thread-local default would not be
/// visible.
fn capture() -> &'static Buffer {
    static CAPTURE: OnceLock<Buffer> = OnceLock::new();
    CAPTURE.get_or_init(|| {
        let buffer = Buffer::default();
        let subscriber = tracing_subscriber::fmt()
            .with_writer(Arc::new(buffer.clone()))
            .with_ansi(false)
            .finish();
        let _ = tracing::subscriber::set_global_default(subscriber);
        buffer
    })
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

/// A registrar that accepts the first REGISTER.
///
/// No challenge here — the digest path is covered by
/// `loopback_registrar.rs`. This registrar only has to be good enough
/// to let the client reach the "registered" state.
async fn run_registrar(socket: UdpSocket, tx: tokio::sync::oneshot::Sender<String>) {
    let mut buf = vec![0u8; 65535];
    let (len, peer) = socket.recv_from(&mut buf).await.expect("REGISTER");
    let request = String::from_utf8_lossy(&buf[..len]).to_string();

    let mut reply = String::from("SIP/2.0 200 OK\r\n");
    for name in ["Via", "From", "To", "Call-ID", "CSeq"] {
        if let Some(value) = header_value(&request, name) {
            reply.push_str(&format!("{name}: {value}\r\n"));
        }
    }
    reply.push_str("Expires: 300\r\nContent-Length: 0\r\n\r\n");
    socket
        .send_to(reply.as_bytes(), peer)
        .await
        .expect("200 OK");

    let _ = tx.send(request);
}

#[tokio::test]
async fn the_registration_task_logs_the_account_id() {
    let capture = capture();

    let socket = UdpSocket::bind("127.0.0.1:0")
        .await
        .expect("registrar socket");
    let registrar_addr = socket.local_addr().expect("registrar addr");
    let (tx, rx) = tokio::sync::oneshot::channel();
    tokio::spawn(run_registrar(socket, tx));

    let mut account = Account::new("Office", registrar_addr.to_string(), "104", "s3cret", "udp");
    account.id = "acct-m1-logging".to_string();

    let mut manager = RegistrationManager::new(tokio::runtime::Handle::current());
    manager.start(&account, 0);

    // The UI drains the channel at the top of every frame; this is the
    // same loop, without the frame.
    let mut registered = false;
    for _ in 0..100 {
        for event in manager.drain() {
            if let RegistrationState::Registered { expires } = event.state {
                assert!(expires > 0, "the registrar granted no expiry");
                registered = true;
            }
        }
        if registered {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(registered, "the account never reached the registered state");

    // The request really did go to the loopback registrar.
    let request = rx.await.expect("the registrar saw the REGISTER");
    assert!(request.starts_with("REGISTER "), "{request}");

    // §6: the account id is on the line the task wrote.
    let out = String::from_utf8(capture.0.lock().expect("buffer lock").clone()).unwrap();
    assert!(
        out.contains(&account.id),
        "the account id must be on the log line: {out}"
    );
    assert!(
        out.contains("registered"),
        "the task must have logged the registration: {out}"
    );

    manager.stop_all();
}
