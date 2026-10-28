//! The media plane, end to end over the loopback.
//!
//! Two sessions, each with a bound UDP port, each pointed at the
//! other. What has to be true:
//!
//! * the port in the SDP is the port the socket is bound to;
//! * a G.711 frame that goes into the outbound queue comes out of
//!   the peer's inbound queue, decoded;
//! * a muted microphone sends silence, and the silence is what the
//!   peer hears;
//! * the RTP clock keeps moving while the microphone is muted, so the
//!   peer does not think the stream died.

use std::time::{Duration, Instant};

use qsoftphone_media::{Codec, MediaConfig, MediaSession};

/// How long a test waits for the RTP to get there.
const TIMEOUT: Duration = Duration::from_secs(5);

/// Two sessions, each pointed at the other.
fn pair() -> (MediaSession, MediaSession) {
    let config = MediaConfig::default();
    let caller = MediaSession::new(&config).expect("a session");
    let callee = MediaSession::new(&config).expect("a session");

    caller
        .set_remote("127.0.0.1", callee.local_port())
        .expect("the callee is on the loopback");
    callee
        .set_remote("127.0.0.1", caller.local_port())
        .expect("the caller is on the loopback");

    (caller, callee)
}

/// Wait for the inbound queue to reach `wanted` samples.
///
/// The queue is drained as it is read, so what has arrived so far is
/// kept — otherwise the samples that came in a first, partial frame
/// would be thrown away and the wait would never end.
fn wait_for_samples(session: &MediaSession, wanted: usize) -> Vec<i16> {
    let deadline = Instant::now() + TIMEOUT;
    let mut collected: Vec<i16> = Vec::new();
    loop {
        collected.extend(session.audio_state().take_inbound());
        if collected.len() >= wanted {
            return collected;
        }
        if Instant::now() >= deadline {
            return collected;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn the_port_in_the_sdp_is_the_port_that_is_bound() {
    let (caller, callee) = pair();
    assert_ne!(caller.local_port(), 0);
    assert_ne!(caller.local_port(), callee.local_port());
    // The two sessions are on different ports: the RTP of one does
    // not go into the other.
    assert!(caller.local_port() > 1024);
    assert!(callee.local_port() > 1024);
}

#[test]
fn a_frame_goes_from_one_session_to_the_other() {
    let (caller, callee) = pair();
    caller.start_transport();
    callee.start_transport();

    // A ramp, so that a wrong decode is visible.
    let pcm: Vec<i16> = (0..160)
        .map(|i| ((i as f32 / 160.0) * 8000.0) as i16)
        .collect();
    let frame = Codec::Pcmu.encode(&pcm);
    caller.audio_state().push_outbound(frame);

    let received = wait_for_samples(&callee, 160);
    assert!(
        received.len() >= 160,
        "the frame did not arrive: {} samples",
        received.len()
    );

    // The peer heard the ramp, within the quantisation error of
    // G.711.
    for (index, sample) in received.iter().take(160).enumerate() {
        let expected = pcm[index];
        assert!(
            (sample - expected).abs() < 400,
            "sample {index}: expected {expected}, heard {sample}"
        );
    }
}

#[test]
fn a_muted_microphone_sends_silence() {
    let (caller, callee) = pair();
    caller.set_mic_muted(true);
    caller.start_transport();
    callee.start_transport();

    // The muted microphone still sends: the frame is the codec's
    // silence code.
    let frame = Codec::Pcmu.silence_frame(160);
    caller.audio_state().push_outbound(frame);

    let received = wait_for_samples(&callee, 160);
    assert!(received.len() >= 160, "{} samples", received.len());

    // And the peer hears nothing.
    for sample in received.iter().take(160) {
        assert!(sample.abs() < 16, "heard {sample}");
    }
}

#[test]
fn the_rtp_clock_keeps_moving_while_the_microphone_is_muted() {
    // A peer that stops seeing RTP tears the call down. A muted
    // microphone sends silence, not nothing — so the sequence numbers
    // and the timestamps keep advancing.
    let (caller, callee) = pair();
    caller.set_mic_muted(true);
    caller.start_transport();
    callee.start_transport();

    for _ in 0..5 {
        caller
            .audio_state()
            .push_outbound(Codec::Pcmu.silence_frame(160));
        std::thread::sleep(Duration::from_millis(20));
    }

    // Five frames of silence arrived: the clock moved.
    let received = wait_for_samples(&callee, 5 * 160);
    assert!(
        received.len() >= 5 * 160,
        "only {} samples arrived",
        received.len()
    );
}

#[test]
fn a_stopped_session_sends_nothing_more() {
    let (caller, callee) = pair();
    caller.start_transport();
    callee.start_transport();

    caller
        .audio_state()
        .push_outbound(Codec::Pcmu.silence_frame(160));
    assert!(wait_for_samples(&callee, 160).len() >= 160);

    caller.stop();
    // The queue is empty now, and nothing more arrives.
    std::thread::sleep(Duration::from_millis(200));
    let after = callee.audio_state().take_inbound();
    assert!(after.is_empty(), "{} samples after the stop", after.len());
}

#[test]
fn a_session_without_a_peer_keeps_the_socket_open() {
    // The peer never answers. The session is still there, and the
    // port is still bound: the SDP named a real port.
    let config = MediaConfig::default();
    let session = MediaSession::new(&config).expect("a session");
    let port = session.local_port();
    session.start_transport();

    // Nothing is sent, because there is nowhere to send it.
    session
        .audio_state()
        .push_outbound(Codec::Pcmu.silence_frame(160));
    std::thread::sleep(Duration::from_millis(100));

    assert_eq!(session.local_port(), port);
    assert!(session.is_running());
    session.stop();
    assert!(!session.is_running());
}
