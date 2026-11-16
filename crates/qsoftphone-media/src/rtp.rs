//! The RTP session: a UDP socket, and the packets that go on it.
//!
//! RFC 3550 is the whole of it. The `rtc` crate supplies the packet
//! format — the header, the sequence numbers, the timestamp, the
//! fragmentation — and this module supplies the transport, which is
//! what a SIP phone needs: plain UDP, no ICE, no DTLS.
//!
//! ## Why plain RTP
//!
//! The `rtc` crate is a WebRTC stack: it does ICE, DTLS, SRTP and
//! SCTP. A SIP phone needs none of that. It has a signalling channel
//! that already agreed on the address and the codec, and it needs to
//! put a packet on the wire. Using the crate's *packet* layer and
//! bringing our own socket is the honest way to use it: the wire
//! format is identical, and none of the machinery that has nothing to
//! do with SIP is in the way.

use std::io;
use std::net::{SocketAddr, UdpSocket};
use std::sync::Arc;
use std::time::Instant;

use bytes::Bytes;
use rtc::rtp::Packet;
use rtc::rtp::codec::g7xx::G7xxPayloader;
use rtc::rtp::codec::opus::OpusPayloader;
use rtc::rtp::packetizer::{Packetizer, new_packetizer};
use rtc::rtp::sequence::new_random_sequencer;
use rtc::shared::marshal::{Marshal, Unmarshal};

use crate::codec::Codec;

/// The size of an RTP header without any extension.
pub const HEADER_LEN: usize = 12;

/// The number of samples in one G.711 frame: 20 ms at 8 kHz.
pub const FRAME_SAMPLES: usize = 160;

/// The MTU: the header plus one whole frame.
///
/// G.711 never fragments — a frame is at most 160 bytes — so this is
/// also the size of every packet we send. Opus does not have a fixed
/// frame size, and its packets are allowed to be 1275 bytes long, so
/// its MTU is bigger; see [`RtpSender::new`].
pub const MTU: usize = HEADER_LEN + FRAME_SAMPLES;

/// A bound RTP socket.
///
/// The socket is shared: the sender and the receiver each need it, and
/// they run on their own threads.
pub struct RtpSocket {
    socket: Arc<UdpSocket>,
    local: SocketAddr,
}

impl RtpSocket {
    /// Bind an ephemeral UDP port on every interface.
    ///
    /// The port is what goes into the SDP, and it has to be the port
    /// the packets actually leave from — a port that is advertised
    /// but not bound is a port the peer sends into the void.
    pub fn bind() -> io::Result<Self> {
        let socket = UdpSocket::bind(("0.0.0.0", 0))?;
        let local = socket.local_addr()?;
        Ok(Self {
            socket: Arc::new(socket),
            local,
        })
    }

    /// The address the socket is bound to.
    pub fn local(&self) -> SocketAddr {
        self.local
    }

    /// The port the socket is bound to.
    pub fn port(&self) -> u16 {
        self.local.port()
    }

    /// The socket itself, shared with the threads that use it.
    pub fn shared(&self) -> Arc<UdpSocket> {
        self.socket.clone()
    }
}

/// The outbound half of the session: encoded frames into RTP packets.
pub struct RtpSender {
    socket: Arc<UdpSocket>,
    packetizer: Box<dyn Packetizer>,
    remote: Option<SocketAddr>,
    /// The synchronisation source — this end's identifier for the
    /// stream. RFC 3550 wants it to be random.
    ssrc: u32,
    /// Whether anything has been sent yet.
    ///
    /// The first packet of a stream carries the marker bit: it is the
    /// start of the first talkspurt, and the peer may be waiting for
    /// it before it plays anything.
    sent: bool,
    /// Whether the last frame that was sent was silence.
    ///
    /// This is what the next frame is compared against: the first
    /// frame after a silence starts a talkspurt, and that is where the
    /// marker belongs.
    silent: bool,
    /// The number of RTP clock ticks in one frame.
    ///
    /// The clock counts samples of the *clock*, not of the audio, and
    /// the two are not the same for every codec: G.722's audio is
    /// 16 kHz and its clock is 8 kHz, so a frame is 320 samples of
    /// audio but 160 ticks of clock. Opus runs its clock at 48 kHz, so
    /// a frame is 960 ticks.
    timestamp_step: u32,
}

impl RtpSender {
    /// A sender for a stream with the given codec.
    ///
    /// The codec decides three things, and all three used to be
    /// constants:
    ///
    /// * the **payloader** — G.711 and G.722 frames are 160 bytes and
    ///   are never fragmented; an Opus packet is as long as the signal
    ///   needs and is never fragmented either, but it is *not* 160
    ///   bytes;
    /// * the **clock rate** — 8 kHz for the G.711 pair (G.722
    ///   included, RFC 3551 §4.5.2) and 48 kHz for Opus;
    /// * the **MTU** — 172 bytes for the G.711 pair and 1287 for Opus,
    ///   whose packets may be 1275 bytes long (RFC 6716 §4.3).
    pub fn new(socket: Arc<UdpSocket>, codec: Codec, payload_type: u8, ssrc: u32) -> Self {
        let payloader: Box<dyn rtc::rtp::packetizer::Payloader> = match codec {
            // `OpusPayloader` puts one whole Opus frame in one packet
            // and never fragments it: RFC 7587 §4.2 says a packet
            // carries one frame and one frame only.
            Codec::Opus => Box::new(OpusPayloader),
            // `G7xxPayloader` is the G.711/G.722 payloader — one
            // payloader for both, and neither of them is ever
            // fragmented.
            _ => Box::new(G7xxPayloader),
        };

        let packetizer = new_packetizer(
            Instant::now(),
            HEADER_LEN + codec.frame_bytes(),
            payload_type,
            ssrc,
            payloader,
            Box::new(new_random_sequencer()),
            codec.rtp_clock_rate(),
        );
        Self {
            socket,
            packetizer: Box::new(packetizer),
            remote: None,
            ssrc,
            sent: false,
            silent: false,
            timestamp_step: codec.rtp_timestamp_step(),
        }
    }

    /// Where the packets go.
    ///
    /// Until this is set the sender drops what it is given: there is
    /// no point putting a packet on the wire before the peer has
    /// answered and told us where it listens.
    pub fn set_remote(&mut self, remote: SocketAddr) {
        self.remote = Some(remote);
    }

    /// The synchronisation source of this stream.
    pub fn ssrc(&self) -> u32 {
        self.ssrc
    }

    /// Send one encoded frame.
    ///
    /// A G.711 frame is 160 bytes — 20 ms at 8 kHz — and an Opus
    /// packet is however long the codec made it. The packetizer turns
    /// it into exactly one packet, with the sequence number and the
    /// timestamp advanced.
    ///
    /// `silent` says whether the frame is silence, and the **marker
    /// bit** follows from it.
    ///
    /// ## The marker bit
    ///
    /// RFC 3551 §5.1: the marker "is set to 1 to indicate that the
    /// packet contains the first octet of a talkspurt". It is how a
    /// peer sees where one talk begins and the next ends.
    ///
    /// So it goes on the first packet of the stream, and on the first
    /// packet after a silence — the two places where a talkspurt
    /// starts. It does **not** go on every packet: a stream that
    /// claims every frame is the start of a talk says that nothing is
    /// a continuation of anything, and a peer that believes it cannot
    /// tell a talk from a pause.
    ///
    /// The packetizer cannot do this on its own. `rtc`'s packetizer
    /// sets the marker on the *last* payload of a frame, which for a
    /// frame that is one packet long is every packet — so the marker
    /// is set here instead, where the talkspurt is known.
    pub fn send_frame(&mut self, frame: &[u8], silent: bool) -> io::Result<()> {
        let Some(remote) = self.remote else {
            return Ok(());
        };

        // The first packet of the stream, and the first packet after a
        // silence. Nothing else starts a talkspurt.
        let marker = !self.sent || (self.silent && !silent);
        self.sent = true;
        self.silent = silent;

        let packets = self
            .packetizer
            .packetize(
                Instant::now(),
                &Bytes::copy_from_slice(frame),
                self.timestamp_step,
            )
            .map_err(|e| io::Error::other(e.to_string()))?;

        for (index, mut packet) in packets.into_iter().enumerate() {
            // The marker belongs on the **first** packet of a frame:
            // it marks the first octet of the talkspurt, and a frame
            // that is split over several packets starts at the first
            // of them.
            packet.header.marker = marker && index == 0;
            let bytes = packet
                .marshal()
                .map_err(|e| io::Error::other(e.to_string()))?;
            self.socket.send_to(&bytes, remote)?;
        }
        Ok(())
    }
}

/// One RTP packet off the wire.
#[derive(Debug, Clone)]
pub struct Received {
    /// The payload type the peer used.
    pub payload_type: u8,
    /// The sequence number — which frame this is.
    pub sequence_number: u16,
    /// The timestamp — where in the stream it belongs.
    pub timestamp: u32,
    /// The media payload, with the RTP header stripped.
    pub payload: Vec<u8>,
}

/// Read one packet from the socket.
///
/// Returns `None` when the datagram was not an RTP packet this build
/// can use — a stray STUN packet, a RTCP packet, or a truncated
/// header. The caller simply carries on.
pub fn receive(
    socket: &UdpSocket,
    buffer: &mut [u8],
) -> io::Result<Option<(SocketAddr, Received)>> {
    let (length, from) = socket.recv_from(buffer)?;
    if length < HEADER_LEN {
        return Ok(None);
    }

    let mut cursor = Bytes::copy_from_slice(&buffer[..length]);
    let packet = match Packet::unmarshal(&mut cursor) {
        Ok(packet) => packet,
        Err(_) => return Ok(None),
    };

    Ok(Some((
        from,
        Received {
            payload_type: packet.header.payload_type,
            sequence_number: packet.header.sequence_number,
            timestamp: packet.header.timestamp,
            payload: packet.payload.to_vec(),
        },
    )))
}

/// A random synchronisation source.
///
/// RFC 3550 §6.1: the SSRC is chosen at random and must be
/// unlikely to collide with another source.
pub fn random_ssrc() -> u32 {
    use std::collections::hash_map::RandomState;
    use std::hash::{BuildHasher, Hasher};

    // `RandomState` is seeded by the OS, and its hash is not a
    // cryptographic one — which is exactly what RFC 3550 asks for.
    let mut hasher = RandomState::new().build_hasher();
    hasher.write_u32(0);
    (hasher.finish() as u32) & 0x7FFF_FFFF
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The loopback address of a port.
    ///
    /// The socket is bound to `0.0.0.0`, which is not an address
    /// anything can be sent to.
    fn loopback(port: u16) -> SocketAddr {
        SocketAddr::from(([127, 0, 0, 1], port))
    }

    #[test]
    fn a_bound_socket_has_a_port() {
        let socket = RtpSocket::bind().expect("bind");
        assert_ne!(socket.port(), 0);
        assert!(
            socket.port() > 1024,
            "an ephemeral port, not a well-known one"
        );
    }

    #[test]
    fn an_opus_packet_is_one_packet_of_the_size_the_codec_chose() {
        // Opus is the codec whose frames are not all the same length,
        // and the RTP layer has to carry them as they are: one packet,
        // no padding, no fragmentation (RFC 7587 §4.2).
        let socket = RtpSocket::bind().expect("bind");
        let shared = socket.shared();
        let peer = UdpSocket::bind(("127.0.0.1", 0)).expect("bind peer");
        let peer_address = peer.local_addr().expect("peer address");

        let mut sender = RtpSender::new(shared, Codec::Opus, 111, 0xABCDEF);
        sender.set_remote(peer_address);

        // A frame of speech and a frame of silence: the two are not
        // the same size, and the packets are not either.
        let mut encoder = crate::codec::Codec::Opus.encoder();
        let speech = encoder.encode(&vec![1_000i16; 320]);
        let silence = encoder.encode_silence();
        assert_ne!(
            speech.len(),
            silence.len(),
            "the two frames came out the same size — the codec is not sizing them"
        );

        let mut buffer = [0u8; 1500];
        let mut payloads = Vec::new();
        let mut timestamps = Vec::new();
        for frame in [&speech, &silence] {
            sender.send_frame(frame, false).expect("send");
            peer.set_read_timeout(Some(std::time::Duration::from_secs(5)))
                .expect("timeout");
            let (length, _) = peer.recv_from(&mut buffer).expect("receive");
            let mut cursor = Bytes::copy_from_slice(&buffer[..length]);
            let packet = Packet::unmarshal(&mut cursor).expect("unmarshal");
            assert_eq!(packet.header.payload_type, 111);
            payloads.push(packet.payload.len());
            timestamps.push(packet.header.timestamp);
        }

        assert_eq!(payloads[0], speech.len(), "the packet was not the frame");
        assert_eq!(payloads[1], silence.len(), "the packet was not the frame");
        // And the clock moved by 960 — 20 ms of the 48 kHz clock that
        // RFC 7587 §4.1 fixes for Opus, and not by the 320 samples the
        // codec was fed.
        assert_eq!(
            timestamps[1],
            timestamps[0].wrapping_add(960),
            "the Opus timestamp did not advance by 960"
        );
    }

    #[test]
    fn a_frame_becomes_one_packet() {
        let socket = RtpSocket::bind().expect("bind");
        let shared = socket.shared();
        let peer = UdpSocket::bind(("127.0.0.1", 0)).expect("bind peer");
        let peer_address = peer.local_addr().expect("peer address");

        let mut sender = RtpSender::new(shared, Codec::Pcma, 8, 0xDEAD_BEEF);
        sender.set_remote(peer_address);

        let frame = vec![0xD5u8; FRAME_SAMPLES];
        sender.send_frame(&frame, false).expect("send");

        let mut buffer = [0u8; 1500];
        peer.set_read_timeout(Some(std::time::Duration::from_secs(5)))
            .expect("timeout");
        let (length, _) = peer.recv_from(&mut buffer).expect("receive");
        assert_eq!(length, HEADER_LEN + FRAME_SAMPLES);

        let mut cursor = Bytes::copy_from_slice(&buffer[..length]);
        let packet = Packet::unmarshal(&mut cursor).expect("unmarshal");
        assert_eq!(packet.header.version, 2);
        assert_eq!(packet.header.payload_type, 8);
        assert_eq!(packet.header.ssrc, 0xDEAD_BEEF);
        assert_eq!(packet.payload.len(), FRAME_SAMPLES);
        assert!(packet.payload.iter().all(|&b| b == 0xD5));
    }

    #[test]
    fn the_sequence_number_and_the_timestamp_move_on() {
        let socket = RtpSocket::bind().expect("bind");
        let shared = socket.shared();
        let peer = UdpSocket::bind(("127.0.0.1", 0)).expect("bind peer");
        let peer_address = peer.local_addr().expect("peer address");

        let mut sender = RtpSender::new(shared, Codec::Pcmu, 0, 1);
        sender.set_remote(peer_address);

        let mut buffer = [0u8; 1500];
        let mut sequences = Vec::new();
        let mut timestamps = Vec::new();
        for _ in 0..3 {
            sender
                .send_frame(&[0u8; FRAME_SAMPLES], false)
                .expect("send");
            peer.set_read_timeout(Some(std::time::Duration::from_secs(5)))
                .expect("timeout");
            let (length, _) = peer.recv_from(&mut buffer).expect("receive");
            let mut cursor = Bytes::copy_from_slice(&buffer[..length]);
            let packet = Packet::unmarshal(&mut cursor).expect("unmarshal");
            sequences.push(packet.header.sequence_number);
            timestamps.push(packet.header.timestamp);
        }

        assert_eq!(sequences[1], sequences[0].wrapping_add(1));
        assert_eq!(sequences[2], sequences[1].wrapping_add(1));
        // The timestamp counts samples, not frames: 160 per frame.
        assert_eq!(timestamps[1], timestamps[0].wrapping_add(160));
        assert_eq!(timestamps[2], timestamps[1].wrapping_add(160));
    }

    #[test]
    fn nothing_is_sent_before_the_peer_is_known() {
        let socket = RtpSocket::bind().expect("bind");
        let mut sender = RtpSender::new(socket.shared(), Codec::Pcmu, 0, 1);
        // No remote set: the frame goes nowhere, and nothing fails.
        sender
            .send_frame(&[0u8; FRAME_SAMPLES], false)
            .expect("send");
    }

    /// Send some frames and read the marker bit off each packet.
    ///
    /// The frames are `(payload, silent)` pairs, and what comes back is
    /// the marker of every packet that was sent.
    fn markers_of(frames: &[(u8, bool)]) -> Vec<bool> {
        let socket = RtpSocket::bind().expect("bind");
        let shared = socket.shared();
        let peer = UdpSocket::bind(("127.0.0.1", 0)).expect("bind peer");
        let peer_address = peer.local_addr().expect("peer address");

        let mut sender = RtpSender::new(shared, Codec::Pcma, 8, 0x0BADF00D);
        sender.set_remote(peer_address);

        let mut buffer = [0u8; 1500];
        let mut markers = Vec::new();
        for &(byte, silent) in frames {
            sender
                .send_frame(&[byte; FRAME_SAMPLES], silent)
                .expect("send");
            peer.set_read_timeout(Some(std::time::Duration::from_secs(5)))
                .expect("timeout");
            let (length, _) = peer.recv_from(&mut buffer).expect("receive");
            let mut cursor = Bytes::copy_from_slice(&buffer[..length]);
            let packet = Packet::unmarshal(&mut cursor).expect("unmarshal");
            markers.push(packet.header.marker);
        }
        markers
    }

    #[test]
    fn the_marker_is_set_on_the_first_packet_and_on_no_other() {
        // This is the bug the capture showed: the marker was set on
        // **every** packet — 540 of 540 — so every frame of the call
        // claimed to be the start of a talk. MicroSIP's stream, in the
        // same capture, sets it once: on the first packet.
        let markers = markers_of(&[(0x11, false), (0x11, false), (0x11, false)]);
        assert_eq!(
            markers,
            vec![true, false, false],
            "the marker must mark the start of the stream, not every packet"
        );
    }

    #[test]
    fn the_marker_marks_the_start_of_a_talkspurt() {
        // The other half of the rule: when the speech stops and
        // starts again, the peer is told where the new talk begins.
        let markers = markers_of(&[
            (0x11, false), // the first packet of the stream
            (0x11, false), // still talking
            (0x11, true),  // a pause
            (0x11, true),  // still paused
            (0x11, false), // talking again — this one starts a talkspurt
            (0x11, false), // and the next one does not
        ]);
        assert_eq!(
            markers,
            vec![true, false, false, false, true, false],
            "the marker must mark the start of a talkspurt"
        );
    }

    #[test]
    fn a_silent_stream_marks_only_its_first_packet() {
        // A call that nobody speaks into — the machine with no
        // microphone — sends silence for the whole call. That is one
        // talkspurt of nothing, and it is marked once.
        let markers = markers_of(&[(0xD5, true), (0xD5, true), (0xD5, true)]);
        assert_eq!(markers, vec![true, false, false]);
    }

    #[test]
    fn a_packet_comes_back_off_the_wire() {
        let socket = RtpSocket::bind().expect("bind");
        let shared = socket.shared();

        // The peer answers us: it sends a packet to the port we are
        // listening on.
        let peer = UdpSocket::bind(("127.0.0.1", 0)).expect("bind peer");
        let mut sender = RtpSender::new(shared.clone(), Codec::Pcma, 8, 7);
        sender.set_remote(loopback(socket.port()));
        sender
            .send_frame(&[0xD5u8; FRAME_SAMPLES], false)
            .expect("send");

        // A packet of our own, with the peer's payload type.
        let packet = rtc::rtp::Packet {
            header: rtc::rtp::Header {
                version: 2,
                payload_type: 8,
                sequence_number: 1,
                timestamp: 160,
                ssrc: 99,
                ..Default::default()
            },
            payload: bytes::Bytes::from(vec![0xD5u8; FRAME_SAMPLES]),
        };
        let raw = packet.marshal().expect("marshal");
        peer.send_to(&raw, loopback(socket.port())).expect("send");

        shared
            .set_read_timeout(Some(std::time::Duration::from_secs(5)))
            .expect("timeout");
        let mut buffer = [0u8; 1500];
        let (from, received) = receive(&shared, &mut buffer)
            .expect("receive")
            .expect("a packet");
        assert_eq!(from.ip().to_string(), "127.0.0.1");
        assert_eq!(received.payload_type, 8);
        assert_eq!(received.payload.len(), FRAME_SAMPLES);
    }

    #[test]
    fn a_stray_datagram_is_not_a_packet() {
        let socket = RtpSocket::bind().expect("bind");
        let peer = UdpSocket::bind(("127.0.0.1", 0)).expect("bind peer");
        peer.send_to(b"hello", loopback(socket.port()))
            .expect("send");

        socket
            .shared()
            .set_read_timeout(Some(std::time::Duration::from_secs(5)))
            .expect("timeout");
        let mut buffer = [0u8; 1500];
        let outcome = receive(&socket.shared(), &mut buffer).expect("receive");
        assert!(outcome.is_none(), "a non-RTP datagram must be ignored");
    }

    #[test]
    fn the_ssrcs_differ() {
        let a = random_ssrc();
        let b = random_ssrc();
        assert_ne!(a, b);
        assert_eq!(a & 0x8000_0000, 0, "the top bit is reserved");
    }

    #[test]
    fn a_read_timeout_fires() {
        // The receiving thread depends on this: it has to notice that
        // the session is over.
        let socket = UdpSocket::bind(("127.0.0.1", 0)).expect("bind");
        socket
            .set_read_timeout(Some(std::time::Duration::from_millis(20)))
            .expect("the timeout is accepted");

        let start = std::time::Instant::now();
        let outcome = socket.recv_from(&mut [0u8; 64]);
        let elapsed = start.elapsed();

        let error = outcome.expect_err("nothing is waiting, so this must fail");
        assert!(
            elapsed < std::time::Duration::from_secs(2),
            "the timeout did not fire: {elapsed:?}"
        );
        // The receiver loop has to recognise it as "nothing arrived".
        assert!(
            matches!(
                error.kind(),
                std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
            ),
            "unexpected error kind: {:?} — {error}",
            error.kind()
        );
    }
}
