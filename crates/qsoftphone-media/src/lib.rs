//! The media plane: RTP, G.711 and the sound card.
//!
//! M4 gave the softphone a voice. Before it, a call connected and
//! carried no sound at all: the SDP named a port that nothing was
//! bound to, and the peer sent its RTP into the void.
//!
//! ## The shape of it
//!
//! A [`MediaSession`] owns three things:
//!
//! * an **RTP socket** — a UDP port, bound, and advertised in the
//!   SDP. The port has to be real: a port that is named but not
//!   bound is a port the peer's audio disappears into.
//! * the **negotiation** — which codec was agreed and which payload
//!   type the peer put it on.
//! * the **sound card** — the microphone that captures and the
//!   speaker that plays.
//!
//! ## The threads
//!
//! Four of them, and they are all somebody else's:
//!
//! * `cpal` runs the capture and the playback callbacks on a thread
//!   of its own — two threads.
//! * This crate runs one thread that turns the captured frames into
//!   RTP packets, and one that takes the received packets apart.
//!
//! None of them is the UI thread. The UI never blocks on any of this:
//! it asks the session to do something and carries on.
//!
//! ## What happens without a sound card
//!
//! The RTP still runs. The peer hears our silence — which is what a
//! muted microphone sends anyway — and the status line says that the
//! machine has no microphone. A call is not refused because a laptop
//! has no sound card; that is a fact about the laptop, not about the
//! call.

use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use tracing::{debug, warn};

pub mod audio;
pub mod codec;
pub mod rtp;

pub use audio::{AudioError, AudioState, DeviceInfo, devices};
pub use codec::Codec;
pub use rtp::RtpSocket;

/// How long the receiving thread waits before it looks at the stop
/// flag again.
const READ_TIMEOUT: Duration = Duration::from_millis(20);

/// How long the sending thread waits for a frame before it looks at
/// the stop flag again.
const SEND_TIMEOUT: Duration = Duration::from_millis(50);

/// What the media of a call should be.
#[derive(Debug, Clone)]
pub struct MediaConfig {
    /// The codecs to offer, in preference order.
    ///
    /// The first one this build speaks is what the offer names first.
    /// When the peer answers with another one of these, that one is
    /// used — the peer has the last word.
    pub codecs: Vec<Codec>,
    /// The microphone to use. `None` is the system default.
    pub microphone: Option<String>,
    /// The speaker to use. `None` is the system default.
    pub speaker: Option<String>,
    /// The microphone gain, 0.0 to 1.0.
    pub microphone_gain: f32,
    /// The speaker gain, 0.0 to 1.0.
    pub speaker_gain: f32,
}

impl Default for MediaConfig {
    fn default() -> Self {
        Self {
            codecs: Codec::ALL.to_vec(),
            microphone: None,
            speaker: None,
            microphone_gain: 1.0,
            speaker_gain: 1.0,
        }
    }
}

/// What the media of a call is doing, for the user interface.
#[derive(Debug, Clone, PartialEq)]
pub struct MediaStatus {
    /// The codec that was agreed.
    pub codec: Codec,
    /// The payload type the peer put that codec on.
    pub payload_type: u8,
    /// The local RTP port.
    pub port: u16,
    /// Where the RTP is going, once the peer has said.
    pub remote: Option<String>,
    /// Whether the audio is running.
    pub running: bool,
    /// The microphone, if one could be opened.
    pub microphone: Option<String>,
    /// The speaker, if one could be opened.
    pub speaker: Option<String>,
    /// Why there is no audio, if there is none.
    ///
    /// A machine with no sound card is not a failure of the call —
    /// it is a line in the status bar.
    pub problem: Option<String>,
    /// Whether the microphone is muted.
    pub mic_muted: bool,
    /// Whether the speaker is muted.
    pub speaker_muted: bool,
}

impl MediaStatus {
    /// A one-line description, for the call row.
    pub fn summary(&self) -> String {
        if let Some(problem) = &self.problem {
            return format!("Media: {problem}");
        }
        let mut line = format!("Media: {}", self.codec.name());
        if !self.running {
            line.push_str(" — not started");
        }
        if self.mic_muted || self.speaker_muted {
            let mut parts = Vec::new();
            if self.mic_muted {
                parts.push("mic muted");
            }
            if self.speaker_muted {
                parts.push("speaker muted");
            }
            line.push_str(&format!(" — {}", parts.join(", ")));
        }
        line
    }
}

/// The media of one call.
///
/// It is a handle: cloning it is cheap, and every method takes
/// `&self`. That matters because the user interface reads the status
/// of a call while the call itself is still being set up on another
/// thread — and §6 of the brief forbids the interface waiting for
/// anything.
///
/// The parts the interface touches — the mute flags, the codec, the
/// running flag — are atomics. The parts that are written once, when
/// the session starts, are behind short locks that are never held
/// while a device is being opened.
#[derive(Clone)]
pub struct MediaSession {
    inner: Arc<MediaInner>,
}

/// Everything a session owns.
struct MediaInner {
    /// The RTP socket. The port is what the SDP advertises.
    socket: RtpSocket,
    /// The state the audio threads share.
    state: Arc<AudioState>,
    /// The codec that was agreed.
    codec: AtomicU8,
    /// The payload type the peer put that codec on.
    payload_type: AtomicU8,
    /// Where the RTP goes.
    remote: Mutex<Option<SocketAddr>>,
    /// The synchronisation source of our stream.
    ssrc: u32,
    /// Whether the audio is running.
    running: AtomicBool,
    /// Why there is no audio, if there is none.
    problem: Mutex<Option<String>>,
    /// The names of the devices that were opened.
    microphone_name: Mutex<Option<String>>,
    speaker_name: Mutex<Option<String>>,
    /// The threads and the devices.
    running_parts: Mutex<Option<Running>>,
}

/// The threads and the devices of a running session.
struct Running {
    /// The thread that puts the captured frames on the wire.
    sender: Option<JoinHandle<()>>,
    /// The thread that takes the received packets apart.
    receiver: Option<JoinHandle<()>>,
    /// The microphone. It has to be kept alive: dropping the stream
    /// stops the capture.
    microphone: Option<audio::Microphone>,
    /// The speaker, for the same reason.
    speaker: Option<audio::Speaker>,
    /// The flag the threads watch.
    stop: Arc<AtomicBool>,
}

impl MediaSession {
    /// A session with a bound RTP socket.
    ///
    /// This binds a UDP port. It does not open the sound card and it
    /// does not start any thread — that happens in
    /// [`MediaSession::start`], once the call is answered.
    pub fn new(config: &MediaConfig) -> Result<Self, AudioError> {
        let socket = RtpSocket::bind().map_err(|e| AudioError::Backend(e.to_string()))?;
        let codec = config.codecs.first().copied().unwrap_or(Codec::Pcmu);
        let state = Arc::new(AudioState::new(config.microphone_gain, config.speaker_gain));

        Ok(Self {
            inner: Arc::new(MediaInner {
                socket,
                state,
                codec: AtomicU8::new(codec.code()),
                payload_type: AtomicU8::new(codec.static_payload_type()),
                remote: Mutex::new(None),
                ssrc: rtp::random_ssrc(),
                running: AtomicBool::new(false),
                problem: Mutex::new(None),
                microphone_name: Mutex::new(None),
                speaker_name: Mutex::new(None),
                running_parts: Mutex::new(None),
            }),
        })
    }

    /// The port the SDP has to advertise.
    ///
    /// It is bound — this is not a placeholder. A port that is named
    /// but not bound is a port the peer's audio disappears into.
    pub fn local_port(&self) -> u16 {
        self.inner.socket.port()
    }

    /// The codec this session speaks.
    pub fn codec(&self) -> Codec {
        Codec::from_code(self.inner.codec.load(Ordering::Relaxed))
    }

    /// The payload type the peer put that codec on.
    pub fn payload_type(&self) -> u8 {
        self.inner.payload_type.load(Ordering::Relaxed)
    }

    /// The synchronisation source of our stream.
    pub fn ssrc(&self) -> u32 {
        self.inner.ssrc
    }

    /// Record what the negotiation settled on.
    ///
    /// The codec is one both ends speak, and the payload type is the
    /// peer's own — a peer that put G.711 on a dynamic number has to
    /// have that number echoed back.
    pub fn negotiate(&self, codec: Codec, payload_type: u8) {
        self.inner.codec.store(codec.code(), Ordering::Relaxed);
        self.inner
            .payload_type
            .store(payload_type, Ordering::Relaxed);
    }

    /// Where the RTP is going.
    pub fn set_remote(&self, address: &str, port: u16) -> Result<(), AudioError> {
        let remote = resolve(address, port)?;
        *self.inner.remote.lock().expect("remote lock") = Some(remote);
        Ok(())
    }

    /// The state the audio threads share.
    pub fn audio_state(&self) -> Arc<AudioState> {
        self.inner.state.clone()
    }

    /// Mute or unmute the microphone.
    pub fn set_mic_muted(&self, muted: bool) {
        self.inner.state.set_mic_muted(muted);
    }

    /// Mute or unmute the speaker.
    pub fn set_speaker_muted(&self, muted: bool) {
        self.inner.state.set_speaker_muted(muted);
    }

    /// Whether the microphone is muted.
    pub fn mic_muted(&self) -> bool {
        self.inner.state.mic_muted()
    }

    /// Whether the speaker is muted.
    pub fn speaker_muted(&self) -> bool {
        self.inner.state.speaker_muted()
    }

    /// Set the microphone gain.
    pub fn set_microphone_gain(&self, gain: f32) {
        self.inner.state.set_mic_gain(gain);
    }

    /// Set the speaker gain.
    pub fn set_speaker_gain(&self, gain: f32) {
        self.inner.state.set_speaker_gain(gain);
    }

    /// Start the audio.
    ///
    /// This opens the sound card, which takes a moment, and it starts
    /// the two RTP threads. It must **not** be called on the UI
    /// thread: opening a device is a slow operation and §6 of the
    /// brief forbids blocking the interface.
    ///
    /// A machine with no sound card is not an error: the RTP still
    /// runs, and the status says what is missing.
    pub fn start(&self, config: &MediaConfig) {
        {
            let running = self.inner.running_parts.lock().expect("running lock");
            if running.is_some() {
                return;
            }
        }

        let mut problems: Vec<String> = Vec::new();

        let microphone = match audio::open_microphone(
            config.microphone.as_deref(),
            self.inner.state.clone(),
            self.codec(),
        ) {
            Ok(microphone) => {
                *self.inner.microphone_name.lock().expect("microphone lock") =
                    Some(microphone.name().to_string());
                Some(microphone)
            }
            Err(e) => {
                problems.push(e.to_string());
                None
            }
        };

        let speaker = match audio::open_speaker(config.speaker.as_deref(), self.inner.state.clone())
        {
            Ok(speaker) => {
                *self.inner.speaker_name.lock().expect("speaker lock") =
                    Some(speaker.name().to_string());
                Some(speaker)
            }
            Err(e) => {
                problems.push(e.to_string());
                None
            }
        };

        *self.inner.problem.lock().expect("problem lock") = if problems.is_empty() {
            None
        } else {
            Some(problems.join("; "))
        };

        self.start_transport_with(microphone, speaker);
    }

    /// Start the RTP without the sound card.
    ///
    /// The transport is the part that matters to a peer: it is what
    /// carries the voice. A machine with no microphone still sends
    /// silence, and a test that wants to watch the RTP on its own
    /// does not want the microphone mixed in.
    pub fn start_transport(&self) {
        {
            let running = self.inner.running_parts.lock().expect("running lock");
            if running.is_some() {
                return;
            }
        }
        self.start_transport_with(None, None);
    }

    /// Start the RTP threads, with the devices they belong to.
    fn start_transport_with(
        &self,
        microphone: Option<audio::Microphone>,
        speaker: Option<audio::Speaker>,
    ) {
        let stop = Arc::new(AtomicBool::new(false));
        let socket = self.inner.socket.shared();

        // The sending thread: the captured frames, onto the wire.
        let sender = {
            let socket = socket.clone();
            let state = self.inner.state.clone();
            let stop = stop.clone();
            let mut sender = rtp::RtpSender::new(socket.clone(), self.payload_type(), self.ssrc());
            if let Some(remote) = *self.inner.remote.lock().expect("remote lock") {
                sender.set_remote(remote);
            }
            std::thread::Builder::new()
                .name("qsoftphone-rtp-send".to_string())
                .spawn(move || sender_loop(socket, sender, state, stop))
                .ok()
        };

        // The receiving thread: the packets, into the speaker.
        let receiver = {
            let socket = socket.clone();
            let state = self.inner.state.clone();
            let stop = stop.clone();
            let codec = self.codec();
            let payload_type = self.payload_type();
            std::thread::Builder::new()
                .name("qsoftphone-rtp-receive".to_string())
                .spawn(move || receiver_loop(socket, state, codec, payload_type, stop))
                .ok()
        };

        let mut running = self.inner.running_parts.lock().expect("running lock");
        if running.is_some() {
            // Somebody else won the race; undo ours.
            stop.store(true, Ordering::Relaxed);
            if let Some(sender) = sender {
                let _ = sender.join();
            }
            if let Some(receiver) = receiver {
                let _ = receiver.join();
            }
            return;
        }
        *running = Some(Running {
            sender,
            receiver,
            microphone,
            speaker,
            stop,
        });
        self.inner.running.store(true, Ordering::Relaxed);
    }

    /// Stop the audio.
    ///
    /// The threads are asked to finish and the sound card is closed.
    /// The RTP socket stays bound — the session may be started again.
    pub fn stop(&self) {
        let running = self
            .inner
            .running_parts
            .lock()
            .expect("running lock")
            .take();
        let Some(running) = running else {
            return;
        };

        running.stop.store(true, Ordering::Relaxed);
        self.inner.state.wake_sender();

        if let Some(sender) = running.sender {
            let _ = sender.join();
        }
        if let Some(receiver) = running.receiver {
            let _ = receiver.join();
        }
        // Dropping the streams closes the devices.
        drop(running.microphone);
        drop(running.speaker);
        self.inner.running.store(false, Ordering::Relaxed);
    }

    /// Whether the audio is running.
    pub fn is_running(&self) -> bool {
        self.inner.running.load(Ordering::Relaxed)
    }

    /// What the media is doing, for the user interface.
    pub fn status(&self) -> MediaStatus {
        MediaStatus {
            codec: self.codec(),
            payload_type: self.payload_type(),
            port: self.local_port(),
            remote: self
                .inner
                .remote
                .lock()
                .expect("remote lock")
                .map(|address| address.to_string()),
            running: self.is_running(),
            microphone: self
                .inner
                .microphone_name
                .lock()
                .expect("microphone lock")
                .clone(),
            speaker: self
                .inner
                .speaker_name
                .lock()
                .expect("speaker lock")
                .clone(),
            problem: self.inner.problem.lock().expect("problem lock").clone(),
            mic_muted: self.mic_muted(),
            speaker_muted: self.speaker_muted(),
        }
    }
}

impl Drop for MediaInner {
    fn drop(&mut self) {
        // The threads have to be told to finish before the socket
        // they are using goes away. This runs when the last handle
        // to the session is dropped — the media belongs to the call,
        // not to one of its owners.
        if let Some(running) = self.running_parts.get_mut().expect("running lock").take() {
            running.stop.store(true, Ordering::Relaxed);
            self.state.wake_sender();
            if let Some(sender) = running.sender {
                let _ = sender.join();
            }
            if let Some(receiver) = running.receiver {
                let _ = receiver.join();
            }
        }
    }
}

/// The sending thread: the captured frames, onto the wire.
fn sender_loop(
    _socket: Arc<std::net::UdpSocket>,
    mut sender: rtp::RtpSender,
    state: Arc<AudioState>,
    stop: Arc<AtomicBool>,
) {
    while !stop.load(Ordering::Relaxed) {
        match state.wait_for_outbound(SEND_TIMEOUT) {
            Some(frame) => {
                if let Err(e) = sender.send_frame(&frame) {
                    warn!(error = %e, "an RTP packet could not be sent");
                }
            }
            None => {
                // The wait timed out: look at the stop flag again.
            }
        }
    }
    debug!("the RTP sender has stopped");
}

/// The receiving thread: the packets, into the speaker.
fn receiver_loop(
    socket: Arc<std::net::UdpSocket>,
    state: Arc<AudioState>,
    codec: Codec,
    payload_type: u8,
    stop: Arc<AtomicBool>,
) {
    if socket.set_read_timeout(Some(READ_TIMEOUT)).is_err() {
        warn!("the RTP socket would not take a read timeout");
    }

    let mut buffer = [0u8; 2048];
    while !stop.load(Ordering::Relaxed) {
        match rtp::receive(&socket, &mut buffer) {
            Ok(Some((_from, packet))) => {
                // A peer may put something else on the same port —
                // RTCP, or a second stream. Only our own payload
                // type is audio we understand.
                if packet.payload_type != payload_type {
                    continue;
                }
                let pcm = codec.decode(&packet.payload);
                state.push_inbound(&pcm);
            }
            Ok(None) => {
                // Not an RTP packet. Ignore it.
            }
            Err(e)
                if e.kind() == std::io::ErrorKind::WouldBlock
                    || e.kind() == std::io::ErrorKind::TimedOut =>
            {
                // Nothing arrived in time.
            }
            Err(e) => {
                warn!(error = %e, "the RTP socket failed");
                break;
            }
        }
    }
    debug!("the RTP receiver has stopped");
}

/// Turn an address and a port into a socket address.
///
/// The address may be a name — `example.com` — which is resolved
/// here. The peer may sit behind a NAT, and then the name in its SDP
/// is the one that resolves to the public address.
fn resolve(address: &str, port: u16) -> Result<SocketAddr, AudioError> {
    use std::net::ToSocketAddrs;

    let host = address.trim().trim_start_matches('[').trim_end_matches(']');
    let target = format!("{host}:{port}");
    target
        .to_socket_addrs()
        .map_err(|e| AudioError::Backend(format!("{target} does not resolve: {e}")))?
        .next()
        .ok_or_else(|| AudioError::Backend(format!("{target} does not resolve")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_session_binds_a_real_port() {
        let session = MediaSession::new(&MediaConfig::default()).expect("a session");
        assert_ne!(session.local_port(), 0);
        assert!(session.local_port() > 1024);
    }

    #[test]
    fn the_default_config_offers_both_codecs() {
        let config = MediaConfig::default();
        assert_eq!(config.codecs, vec![Codec::Pcmu, Codec::Pcma]);
    }

    #[test]
    fn the_negotiation_can_change_the_codec_and_the_payload_type() {
        let session = MediaSession::new(&MediaConfig::default()).expect("a session");
        assert_eq!(session.codec(), Codec::Pcmu);
        assert_eq!(session.payload_type(), 0);

        session.negotiate(Codec::Pcma, 8);
        assert_eq!(session.codec(), Codec::Pcma);
        assert_eq!(session.payload_type(), 8);

        // A peer that put G.711 on a dynamic number.
        session.negotiate(Codec::Pcma, 96);
        assert_eq!(session.payload_type(), 96);
    }

    #[test]
    fn the_mute_flags_are_kept() {
        let session = MediaSession::new(&MediaConfig::default()).expect("a session");
        assert!(!session.mic_muted());
        assert!(!session.speaker_muted());

        session.set_mic_muted(true);
        session.set_speaker_muted(true);
        assert!(session.mic_muted());
        assert!(session.speaker_muted());

        let status = session.status();
        assert!(status.mic_muted);
        assert!(status.speaker_muted);
        assert!(
            status.summary().contains("mic muted"),
            "{}",
            status.summary()
        );
    }

    #[test]
    fn the_status_says_when_there_is_no_media() {
        let session = MediaSession::new(&MediaConfig::default()).expect("a session");
        let status = session.status();
        assert!(!status.running);
        assert!(status.problem.is_none());
        assert!(
            status.summary().contains("not started"),
            "{}",
            status.summary()
        );
    }

    #[test]
    fn the_remote_address_is_resolved() {
        let session = MediaSession::new(&MediaConfig::default()).expect("a session");
        session.set_remote("127.0.0.1", 4000).expect("resolve");
        let status = session.status();
        assert_eq!(status.remote.as_deref(), Some("127.0.0.1:4000"));
    }

    #[test]
    fn an_address_that_does_not_resolve_is_an_error() {
        let session = MediaSession::new(&MediaConfig::default()).expect("a session");
        let outcome = session.set_remote("this-host-does-not-exist.invalid", 4000);
        assert!(outcome.is_err());
    }

    #[test]
    fn a_session_without_a_sound_card_still_runs() {
        // The machine may have no microphone at all. The RTP has to
        // run anyway: the call is a call.
        let session = MediaSession::new(&MediaConfig::default()).expect("a session");
        session.set_remote("127.0.0.1", 4000).expect("resolve");
        session.start(&MediaConfig::default());
        assert!(session.is_running());
        session.stop();
        assert!(!session.is_running());
    }
}
