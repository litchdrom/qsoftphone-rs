//! The call is logged **once**.
//!
//! The media says what the call did when it stops, and the session is
//! dropped right after it stops — so the numbers used to be written
//! twice, and a log that says every call ended twice is a log nobody
//! can count.
//!
//! This reads the log the same way the other logging test in this
//! repository does: a subscriber that keeps what it is given.

use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use qsoftphone_media::{MediaConfig, MediaSession};

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
/// The subscriber has to be the *global* one: the media runs on other
/// threads, where a thread-local default would not be visible.
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

#[test]
fn a_call_that_ended_is_said_once() {
    let buffer = capture();
    let before = buffer.0.lock().expect("buffer lock").len();

    // A call with media: the transport, not the sound card, so the
    // test does not open a microphone on the machine it runs on.
    let session = MediaSession::new(&MediaConfig::default()).expect("a session");
    session.start_transport();
    // Long enough for a frame to go out, so the call has something to
    // say about itself.
    std::thread::sleep(Duration::from_millis(150));
    session.stop();
    // And the session goes away, as it does when the call is
    // forgotten.
    drop(session);

    let log = String::from_utf8_lossy(&buffer.0.lock().expect("buffer lock")[before..]).to_string();
    let said = log.matches("the call's media stopped").count();
    assert_eq!(said, 1, "the call was logged {said} times:\n{log}");
}

#[test]
fn a_call_that_never_had_media_says_nothing() {
    let buffer = capture();
    let before = buffer.0.lock().expect("buffer lock").len();

    // A session that was never started has nothing to report, and a
    // line about it would be a line about a call that never happened.
    let session = MediaSession::new(&MediaConfig::default()).expect("a session");
    drop(session);

    let log = String::from_utf8_lossy(&buffer.0.lock().expect("buffer lock")[before..]).to_string();
    assert!(
        !log.contains("the call's media stopped"),
        "a call with no media was logged:\n{log}"
    );
}
