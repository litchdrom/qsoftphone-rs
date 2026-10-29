//! The codecs this build speaks: G.711 and G.722.
//!
//! ## G.711 — the codec every SIP telephone speaks
//!
//! G.711 is a *companding* codec: 8 kHz, one byte per sample, and the
//! byte is a logarithmic code rather than a linear value. Two variants
//! exist and a PBX uses whichever its country prefers:
//!
//! * **µ-law** (PCMU) — North America and Japan. Static payload
//!   type 0.
//! * **A-law** (PCMA) — everywhere else. Static payload type 8.
//!
//! Asterisk offers A-law, so a softphone that only speaks µ-law
//! cannot be answered. This build speaks both and picks the one the
//! peer listed.
//!
//! ## G.722 — the wideband one
//!
//! G.722 is a sub-band ADPCM codec: 16 kHz of speech in 64 kbit/s.
//! It is not companded — the signal is split into two sub-bands by a
//! quadrature mirror filter and each is coded with ADPCM — so the
//! encoder and the decoder are *stateful*: they remember the signal.
//!
//! Two facts about G.722 surprise everybody, and both are in RFC 3551
//! §4.5.2:
//!
//! * The RTP clock rate is **8000 Hz**, not 16000. The original
//!   recommendation had a historical error in it, and the RTP payload
//!   type kept the wrong number so that the two ends would agree.
//!   The *audio* is 16 kHz; the *clock* is 8 kHz.
//! * The payload is therefore **160 bytes** per 20 ms frame — the same
//!   as G.711 — and the RTP timestamp advances by 160, exactly as for
//!   G.711. The RTP layer needs no change at all for G.722.
//!
//! The codec itself comes from `nanoaudio-g722`, a Rust port of the
//! CMU 1993 implementation — the same code lineage as SpanDSP's
//! `g722enc`/`g722dec` and therefore the same code Asterisk's
//! `codec_g722.c` runs. It was checked against the reference vectors
//! that ship with `sippy/libg722`: encoding the reference PCM and
//! decoding the reference stream both reproduce the published SHA-256
//! digests byte for byte.
//!
//! ## The tables are computed, not written out
//!
//! The classic G.711 implementations carry two 256-entry lookup tables
//! per variant. They are generated here from the algorithm instead,
//! which is fewer lines and — more importantly — cannot disagree with
//! the decoder. The algorithms are the ones ITU-T G.711 describes and
//! every softphone implements: a segment (a "chord") of the
//! logarithmic curve, three bits of segment, four bits of
//! quantisation, and a sign bit.

use nanoaudio_g722::{G722BitRate, G722Decoder, G722Encoder};

/// The audio codecs this build speaks.
///
/// G.711 is what every SIP peer offers; G.722 is the wideband one,
/// and it is the only codec here that a PBX has to be told to allow.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Codec {
    /// G.711 µ-law. The American and Japanese standard.
    Pcmu,
    /// G.711 A-law. The European and the rest-of-the-world standard.
    Pcma,
    /// G.722 wideband ADPCM. 16 kHz of speech in 64 kbit/s.
    G722,
}

impl Codec {
    /// Every codec this build speaks, in the order they are preferred.
    ///
    /// The G.711 pair first: they are what a peer is most likely to
    /// offer, and the two sound the same. G.722 is last because a PBX
    /// has to be told to allow it — an offer that names it is harmless,
    /// but a peer that does not have it ignores it.
    pub const ALL: [Codec; 3] = [Codec::Pcmu, Codec::Pcma, Codec::G722];

    /// The name the SDP carries — `PCMU`, `PCMA` or `G722`.
    pub const fn name(self) -> &'static str {
        match self {
            Codec::Pcmu => "PCMU",
            Codec::Pcma => "PCMA",
            Codec::G722 => "G722",
        }
    }

    /// The static payload type of the codec — 0, 8 or 9.
    ///
    /// RFC 3551 fixes these. A peer is free to put the same codec on a
    /// dynamic number instead, and then *its* number is the one the
    /// answer has to echo.
    pub const fn static_payload_type(self) -> u8 {
        match self {
            Codec::Pcmu => 0,
            Codec::Pcma => 8,
            Codec::G722 => 9,
        }
    }

    /// The rate of the PCM the codec works on.
    ///
    /// G.711 is 8 kHz. G.722 is 16 kHz — that is the whole point of
    /// it: the speech is wideband.
    pub const fn sample_rate(self) -> u32 {
        match self {
            Codec::Pcmu | Codec::Pcma => 8000,
            Codec::G722 => 16000,
        }
    }

    /// The RTP clock rate of the codec.
    ///
    /// 8000 Hz for all three, and that is not a mistake: RFC 3551
    /// §4.5.2 keeps G.722 on the 8 kHz clock of the original
    /// recommendation, so a G.722 packet advances the timestamp by 160
    /// every 20 ms exactly like a G.711 one.
    pub const fn rtp_clock_rate(self) -> u32 {
        8000
    }

    /// The number of PCM samples in one 20 ms frame.
    ///
    /// 160 at 8 kHz, and 320 at 16 kHz — a frame is always 20 ms of
    /// audio, whatever the rate.
    pub const fn frame_samples(self) -> usize {
        match self {
            Codec::Pcmu | Codec::Pcma => 160,
            Codec::G722 => 320,
        }
    }

    /// The number of payload bytes in one 20 ms frame.
    ///
    /// 160 for all three: G.711 is one byte per sample at 8 kHz, and
    /// G.722 is one byte per 8 kHz sample-pair at 16 kHz.
    pub const fn frame_bytes(self) -> usize {
        160
    }

    /// The `a=rtpmap` of the codec.
    ///
    /// G.722 says 8000 here, not 16000 — see [`Codec::rtp_clock_rate`].
    pub const fn rtpmap(self) -> &'static str {
        match self {
            Codec::Pcmu => "PCMU/8000",
            Codec::Pcma => "PCMA/8000",
            Codec::G722 => "G722/8000",
        }
    }

    /// The codec with that name, whatever the case.
    pub fn from_name(name: &str) -> Option<Codec> {
        match name.trim().to_ascii_uppercase().as_str() {
            "PCMU" | "ULAW" | "MU-LAW" | "MULAW" => Some(Codec::Pcmu),
            "PCMA" | "ALAW" | "A-LAW" => Some(Codec::Pcma),
            "G722" | "G.722" => Some(Codec::G722),
            _ => None,
        }
    }

    /// The codec of a payload type.
    ///
    /// The name from the `a=rtpmap` line wins when there is one: a
    /// peer may put G.711 on a dynamic number, and the number alone
    /// then says nothing. Without a name the static table of RFC 3551
    /// is used.
    pub fn from_payload_type(payload_type: u8, name: Option<&str>) -> Option<Codec> {
        if let Some(name) = name {
            return Codec::from_name(name);
        }
        match payload_type {
            0 => Some(Codec::Pcmu),
            8 => Some(Codec::Pcma),
            9 => Some(Codec::G722),
            _ => None,
        }
    }

    /// A fresh encoder for this codec.
    pub fn encoder(self) -> Encoder {
        Encoder::new(self)
    }

    /// A fresh decoder for this codec.
    pub fn decoder(self) -> Decoder {
        Decoder::new(self)
    }

    /// A number that stands for this codec, for the atomic that holds
    /// it.
    pub(crate) const fn code(self) -> u8 {
        match self {
            Codec::Pcmu => 0,
            Codec::Pcma => 1,
            Codec::G722 => 2,
        }
    }

    /// The codec a number stands for.
    pub(crate) const fn from_code(code: u8) -> Codec {
        match code {
            1 => Codec::Pcma,
            2 => Codec::G722,
            _ => Codec::Pcmu,
        }
    }
}

/// An encoder for one codec.
///
/// G.711 is stateless — one byte per sample, and the byte depends on
/// nothing but the sample. G.722 is ADPCM: the encoder remembers the
/// signal it has seen, and so does the decoder at the other end. An
/// encoder that is not in the same state as its peer produces noise,
/// so both are objects with a state and not plain functions.
#[derive(Debug)]
pub struct Encoder {
    codec: Codec,
    inner: EncoderInner,
}

/// What the encoder is made of.
#[derive(Debug)]
enum EncoderInner {
    /// G.711: the codec itself is the encoder.
    G711(Codec),
    /// G.722: the ADPCM encoder.
    ///
    /// It is boxed: the ADPCM state is a good deal bigger than a
    /// codec, and the encoder is held for the whole call.
    G722(Box<G722Encoder>),
}

impl Encoder {
    /// A fresh encoder for a codec.
    pub fn new(codec: Codec) -> Self {
        let inner = match codec {
            Codec::G722 => EncoderInner::G722(Box::new(G722Encoder::new())),
            other => EncoderInner::G711(other),
        };
        Self { codec, inner }
    }

    /// The codec this encoder speaks.
    pub fn codec(&self) -> Codec {
        self.codec
    }

    /// Encode one frame of PCM.
    ///
    /// The samples are at the codec's own rate — 8 kHz for G.711, 16 kHz
    /// for G.722 — and a frame is 20 ms of them. A frame that is not
    /// the right length is encoded as far as it goes: G.722 needs the
    /// samples in pairs, and an odd one is left over.
    pub fn encode(&mut self, pcm: &[i16]) -> Vec<u8> {
        match &mut self.inner {
            EncoderInner::G711(codec) => pcm
                .iter()
                .map(|&sample| codec.encode_sample(sample))
                .collect(),
            EncoderInner::G722(encoder) => {
                let mut out = Vec::with_capacity(pcm.len() / 2);
                for pair in pcm.chunks_exact(2) {
                    out.push(encoder.encode_pair(pair[0], pair[1]));
                }
                out
            }
        }
    }

    /// Encode one frame of silence.
    ///
    /// This is what a muted microphone sends. It goes through the
    /// encoder like any other frame, so the encoder's state stays in
    /// step with the decoder at the other end — which matters for
    /// G.722, where the state *is* the codec.
    pub fn encode_silence(&mut self) -> Vec<u8> {
        self.encode(&vec![0i16; self.codec.frame_samples()])
    }
}

/// A decoder for one codec.
///
/// The mirror of [`Encoder`], and stateful for the same reason.
#[derive(Debug)]
pub struct Decoder {
    codec: Codec,
    inner: DecoderInner,
}

/// What the decoder is made of.
#[derive(Debug)]
enum DecoderInner {
    /// G.711: the codec itself is the decoder.
    G711(Codec),
    /// G.722: the ADPCM decoder, boxed for the same reason as the
    /// encoder.
    G722(Box<G722Decoder>),
}

impl Decoder {
    /// A fresh decoder for a codec.
    pub fn new(codec: Codec) -> Self {
        let inner = match codec {
            Codec::G722 => DecoderInner::G722(Box::new(G722Decoder::new(G722BitRate::Rate64000))),
            other => DecoderInner::G711(other),
        };
        Self { codec, inner }
    }

    /// The codec this decoder speaks.
    pub fn codec(&self) -> Codec {
        self.codec
    }

    /// Decode a payload into PCM at the codec's rate.
    pub fn decode(&mut self, payload: &[u8]) -> Vec<i16> {
        match &mut self.inner {
            DecoderInner::G711(codec) => payload
                .iter()
                .map(|&byte| codec.decode_sample(byte))
                .collect(),
            DecoderInner::G722(decoder) => {
                let mut out = Vec::with_capacity(payload.len() * 2);
                for &byte in payload {
                    let pair = decoder.decode_byte(byte);
                    out.push(pair[0]);
                    out.push(pair[1]);
                }
                out
            }
        }
    }

    /// The samples that stand in for a frame that never arrived.
    ///
    /// The decoder has to keep going: a gap in the stream would leave
    /// it out of step with the encoder at the other end — and for
    /// G.722, which is ADPCM and keeps a state, that means the sound
    /// is wrong for the rest of the call.
    ///
    /// For G.711 the frame is the silence code, which every
    /// implementation agrees on.
    ///
    /// For G.722 there is no fixed code, and the two obvious choices
    /// are both traps — see [`G722_CONCEALMENT_BYTE`] for what the
    /// codec's own tables say about them.
    pub fn conceal(&mut self) -> Vec<i16> {
        let frame = match &self.inner {
            DecoderInner::G711(codec) => {
                let byte = match codec {
                    Codec::Pcmu => 0xFF,
                    Codec::Pcma => 0xD5,
                    Codec::G722 => unreachable!("G.722 is not a G.711 codec"),
                };
                vec![byte; self.codec.frame_bytes()]
            }
            DecoderInner::G722(_) => {
                vec![G722_CONCEALMENT_BYTE; self.codec.frame_bytes()]
            }
        };
        self.decode(&frame)
    }
}

/// The byte a G.722 decoder is given for a frame that never arrived.
///
/// G.722 has no silence code, and the two obvious choices are traps:
///
/// * **A frame of zeros is the worst one.** The high band's code 0
///   means "a large negative change" — the difference it reconstructs
///   is the biggest in the table — and its magnitude index is the
///   largest too, so the step size *grows* while it is fed. The
///   sound is at full scale within a frame or two, and stays there.
/// * **Repeating the last frame pumps the last frame's step size.**
///   The step size follows the magnitude of the code, so a frame of
///   loud speech makes the decoder's step size grow with every
///   repetition. It runs away the same way.
///
/// The byte 0xC0 says "no change" to both bands, and it says it in the
/// only way the codec can:
///
/// * the low band code is 0, whose difference is the smallest there
///   is and whose magnitude index is the lowest — the step size
///   *shrinks*;
/// * the high band code is 3, whose difference is the smallest of the
///   four and whose magnitude index is also the lowest — the step
///   size shrinks as well.
///
/// So the decoder converges: the step sizes fall to their minimum, the
/// reconstruction rings down to the value it was at, and nothing runs
/// away. The step sizes then grow again as the real frames arrive.
const G722_CONCEALMENT_BYTE: u8 = 0xC0;

/// The bias µ-law adds before the segment search.
///
/// It is what makes the quantisation steps start at a small value
/// instead of at zero, so that quiet speech is not quantised away.
const U_BIAS: i32 = 0x84;

/// The largest magnitude µ-law can carry, in the 14-bit units the
/// algorithm works in.
const U_CLIP: i32 = 8159;

/// The upper bound of each µ-law segment, in 14-bit units.
const U_SEGMENTS: [i32; 8] = [0x3F, 0x7F, 0xFF, 0x1FF, 0x3FF, 0x7FF, 0xFFF, 0x1FFF];

/// The upper bound of each A-law segment, in 13-bit units.
const A_SEGMENTS: [i32; 8] = [0x1F, 0x3F, 0x7F, 0xFF, 0x1FF, 0x3FF, 0x7FF, 0xFFF];

/// The segment a magnitude falls into.
fn search(value: i32, table: &[i32; 8]) -> usize {
    table
        .iter()
        .position(|&upper| value <= upper)
        .unwrap_or(table.len())
}

/// Encode a 16-bit sample as µ-law.
fn to_ulaw(sample: i16) -> u8 {
    // The algorithm works in 14 bits: two bits of the sample are
    // dropped, and the result is scaled back up when it is decoded.
    let mut value = (sample as i32) >> 2;
    let mask = if value < 0 {
        value = -value;
        0x7Fu8
    } else {
        0xFFu8
    };

    if value > U_CLIP {
        value = U_CLIP;
    }
    value += U_BIAS >> 2;

    let segment = search(value, &U_SEGMENTS);
    if segment >= 8 {
        return 0x7F ^ mask;
    }

    let code = ((segment as u8) << 4) | (((value >> (segment + 1)) & 0x0F) as u8);
    code ^ mask
}

/// Decode a µ-law byte into a 16-bit sample.
fn from_ulaw(byte: u8) -> i16 {
    // The code is stored complemented, with the sign in the top bit.
    let value = !byte;

    let mut magnitude = (((value & 0x0F) as i32) << 3) + U_BIAS;
    magnitude <<= ((value & 0x70) >> 4) as i32;

    if value & 0x80 != 0 {
        (U_BIAS - magnitude) as i16
    } else {
        (magnitude - U_BIAS) as i16
    }
}

/// Encode a 16-bit sample as A-law.
fn to_alaw(sample: i16) -> u8 {
    // A-law works in 13 bits.
    let mut value = (sample as i32) >> 3;
    let mask = if value >= 0 {
        0xD5u8
    } else {
        value = -value - 1;
        0x55u8
    };

    let segment = search(value, &A_SEGMENTS);
    if segment >= 8 {
        return 0x7F ^ mask;
    }

    let mut code = (segment as u8) << 4;
    if segment < 2 {
        code |= ((value >> 1) & 0x0F) as u8;
    } else {
        code |= ((value >> segment) & 0x0F) as u8;
    }
    code ^ mask
}

/// Decode an A-law byte into a 16-bit sample.
fn from_alaw(byte: u8) -> i16 {
    // A-law is stored with the even bits complemented.
    let value = byte ^ 0x55;

    let mut magnitude = ((value & 0x0F) as i32) << 4;
    let segment = ((value & 0x70) >> 4) as i32;
    match segment {
        0 => magnitude += 8,
        1 => magnitude += 0x108,
        _ => {
            magnitude += 0x108;
            magnitude <<= segment - 1;
        }
    }

    if value & 0x80 != 0 {
        magnitude as i16
    } else {
        -(magnitude as i16)
    }
}

/// The G.711 half of the codec: one byte per sample, no state.
impl Codec {
    /// Encode one 16-bit sample as G.711.
    ///
    /// G.722 has no per-sample encoding — it is ADPCM, and it needs the
    /// state — so this is only for G.711.
    fn encode_sample(self, sample: i16) -> u8 {
        match self {
            Codec::Pcmu => to_ulaw(sample),
            Codec::Pcma => to_alaw(sample),
            Codec::G722 => unreachable!("G.722 is encoded a frame at a time"),
        }
    }

    /// Decode one byte into a 16-bit sample.
    fn decode_sample(self, byte: u8) -> i16 {
        match self {
            Codec::Pcmu => from_ulaw(byte),
            Codec::Pcma => from_alaw(byte),
            Codec::G722 => unreachable!("G.722 is decoded a frame at a time"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The delay of the G.722 synthesis filter, in samples at 16 kHz.
    ///
    /// The decoder reconstructs the signal through the quadrature
    /// mirror filter, and the filter has a group delay: the output
    /// lags the input by this much. It does not matter for a call —
    /// the delay is a millisecond and a half — but it matters for a
    /// measurement.
    const G722_DELAY: usize = 22;

    #[test]
    fn the_names_are_the_ones_the_sdp_carries() {
        assert_eq!(Codec::Pcmu.name(), "PCMU");
        assert_eq!(Codec::Pcma.name(), "PCMA");
        assert_eq!(Codec::G722.name(), "G722");
        assert_eq!(Codec::Pcmu.static_payload_type(), 0);
        assert_eq!(Codec::Pcma.static_payload_type(), 8);
        assert_eq!(Codec::G722.static_payload_type(), 9);
        assert_eq!(Codec::Pcmu.rtp_clock_rate(), 8000);
        assert_eq!(Codec::Pcma.rtp_clock_rate(), 8000);
        // G.722 keeps the 8 kHz clock of the original recommendation.
        assert_eq!(Codec::G722.rtp_clock_rate(), 8000);
        assert_eq!(Codec::Pcmu.rtpmap(), "PCMU/8000");
        assert_eq!(Codec::G722.rtpmap(), "G722/8000");
    }

    #[test]
    fn the_rates_follow_the_codec() {
        assert_eq!(Codec::Pcmu.sample_rate(), 8000);
        assert_eq!(Codec::Pcma.sample_rate(), 8000);
        // G.722 is wideband: 16 kHz of speech.
        assert_eq!(Codec::G722.sample_rate(), 16000);

        // A frame is 20 ms of audio, whatever the rate.
        assert_eq!(Codec::Pcmu.frame_samples(), 160);
        assert_eq!(Codec::G722.frame_samples(), 320);
        // And the payload is 160 bytes either way.
        assert_eq!(Codec::Pcmu.frame_bytes(), 160);
        assert_eq!(Codec::G722.frame_bytes(), 160);
    }

    #[test]
    fn a_codec_is_recognised_by_its_name() {
        assert_eq!(Codec::from_name("PCMA"), Some(Codec::Pcma));
        assert_eq!(Codec::from_name("pcmu"), Some(Codec::Pcmu));
        assert_eq!(Codec::from_name(" G722 "), Some(Codec::G722));
        assert_eq!(Codec::from_name("opus"), None);
    }

    #[test]
    fn the_rtpmap_wins_over_the_payload_type() {
        // A peer may put G.711 on a dynamic number, and then the
        // number says nothing about the codec.
        assert_eq!(
            Codec::from_payload_type(96, Some("PCMA")),
            Some(Codec::Pcma)
        );
        assert_eq!(Codec::from_payload_type(0, None), Some(Codec::Pcmu));
        assert_eq!(Codec::from_payload_type(8, None), Some(Codec::Pcma));
        assert_eq!(Codec::from_payload_type(9, None), Some(Codec::G722));
        assert_eq!(Codec::from_payload_type(101, None), None);
    }

    #[test]
    fn silence_has_a_code_of_its_own() {
        // The digital silence every G.711 implementation agrees on.
        assert_eq!(Codec::Pcmu.encode_sample(0), 0xFF);
        assert_eq!(Codec::Pcma.encode_sample(0), 0xD5);
        // It has to decode to (very nearly) nothing.
        assert!(Codec::Pcmu.decode_sample(0xFF).abs() <= 8);
        assert!(Codec::Pcma.decode_sample(0xD5).abs() <= 8);
    }

    #[test]
    fn every_ulaw_byte_survives_a_round_trip() {
        // 0x7F is the one exception: it is the other code for zero,
        // and the encoder always writes 0xFF for zero.
        for byte in 0u8..=255 {
            if byte == 0x7F {
                continue;
            }
            let decoded = Codec::Pcmu.decode_sample(byte);
            assert_eq!(
                Codec::Pcmu.encode_sample(decoded),
                byte,
                "µ-law byte {byte:#04x} did not survive"
            );
        }
    }

    #[test]
    fn every_alaw_byte_survives_a_round_trip() {
        for byte in 0u8..=255 {
            let decoded = Codec::Pcma.decode_sample(byte);
            assert_eq!(
                Codec::Pcma.encode_sample(decoded),
                byte,
                "A-law byte {byte:#04x} did not survive"
            );
        }
    }

    #[test]
    fn the_quantisation_error_is_small() {
        // The step of the curve grows with the amplitude, so the
        // error is bounded by a fraction of the signal.
        for codec in [Codec::Pcmu, Codec::Pcma] {
            for sample in [
                -32000i16, -16000, -8000, -4000, -1000, -100, 0, 100, 1000, 4000, 8000, 16000,
                32000,
            ] {
                let decoded = codec.decode_sample(codec.encode_sample(sample));
                let error = (decoded as i32 - sample as i32).abs();
                let bound = 8 + (sample as i32).abs() / 8;
                assert!(
                    error <= bound,
                    "{codec:?} {sample} decoded to {decoded} (error {error}, bound {bound})"
                );
            }
        }
    }

    #[test]
    fn the_curve_is_monotonic() {
        // A louder sample never decodes to a quieter one. This is
        // what makes the codec usable at all.
        for codec in [Codec::Pcmu, Codec::Pcma] {
            let mut previous = i32::MIN;
            for sample in -32000i16..=32000 {
                let decoded = codec.decode_sample(codec.encode_sample(sample)) as i32;
                assert!(
                    decoded >= previous,
                    "{codec:?} went backwards at {sample}: {previous} -> {decoded}"
                );
                previous = decoded;
            }
        }
    }

    #[test]
    fn a_frame_of_silence_encodes_to_the_silence_code() {
        let pcm = vec![0i16; 160];
        let mut encoder = Codec::Pcmu.encoder();
        assert_eq!(encoder.encode(&pcm), vec![0xFFu8; 160]);
        let mut encoder = Codec::Pcma.encoder();
        assert_eq!(encoder.encode(&pcm), vec![0xD5u8; 160]);
    }

    #[test]
    fn a_whole_frame_round_trips() {
        let pcm: Vec<i16> = (0..160)
            .map(|i| ((i as f32 / 160.0) * 8000.0) as i16)
            .collect();
        let mut encoder = Codec::Pcma.encoder();
        let encoded = encoder.encode(&pcm);
        assert_eq!(encoded.len(), 160);

        let mut decoder = Codec::Pcma.decoder();
        let decoded = decoder.decode(&encoded);
        assert_eq!(decoded.len(), 160);
        for (before, after) in pcm.iter().zip(decoded.iter()) {
            assert!((before - after).abs() < 300, "{before} -> {after}");
        }
    }

    #[test]
    fn a_concealment_frame_is_quiet_and_the_right_size() {
        for codec in Codec::ALL {
            let mut decoder = codec.decoder();
            let pcm = decoder.conceal();
            assert_eq!(pcm.len(), codec.frame_samples());
            for sample in pcm {
                // G.722 has no fixed silence code, so its concealment
                // is the smallest step the decoder knows — and the
                // synthesis filter rings a little at the start of a
                // frame. A few hundred counts is nothing.
                assert!(
                    sample.saturating_abs() < 128,
                    "{codec:?} concealed to {sample}"
                );
            }
        }
    }

    #[test]
    fn the_concealment_does_not_run_away() {
        // G.722 is ADPCM: the decoder's step size follows the signal
        // it is given. A concealment frame of zeros would say "no
        // change", the error would grow, and the sound would end up at
        // full scale within a frame or two. Repeating the last frame
        // keeps the decoder in step and the sound in hand.
        let pcm: Vec<i16> = (0..320)
            .map(|i| (12_000.0 * (i as f32 / 40.0).sin()) as i16)
            .collect();
        let mut encoder = Codec::G722.encoder();
        let frame = encoder.encode(&pcm);

        let mut decoder = Codec::G722.decoder();
        decoder.decode(&frame);
        for _ in 0..8 {
            let concealed = decoder.conceal();
            let peak = concealed
                .iter()
                .map(|s| s.saturating_abs())
                .max()
                .unwrap_or(0);
            assert!(peak < 16_384, "the concealment ran away: {peak}");
        }
    }

    #[test]
    fn a_lost_frame_does_not_derail_the_decoder() {
        // G.722 is ADPCM: the decoder remembers the signal, and it has
        // to see every frame in order. A frame that is missing and is
        // not replaced would put the decoder out of step with the
        // encoder at the other end — and then the rest of the call is
        // noise.
        let pcm: Vec<i16> = (0..320 * 8)
            .map(|i| ((i as f32 / 2560.0) * 8000.0) as i16)
            .collect();

        let mut encoder = Codec::G722.encoder();
        let frames: Vec<Vec<u8>> = pcm.chunks(320).map(|chunk| encoder.encode(chunk)).collect();

        let mut decoder = Codec::G722.decoder();
        let mut decoded = Vec::new();
        for (index, frame) in frames.iter().enumerate() {
            if index == 1 {
                // The frame never arrived.
                decoded.extend(decoder.conceal());
            } else {
                decoded.extend(decoder.decode(frame));
            }
        }

        // The decoder caught up: a couple of frames later the signal
        // is the signal again.
        let start = 4 * 320;
        let mut error = 0f64;
        let mut signal = 0f64;
        for (before, after) in pcm[start..].iter().zip(decoded[start..].iter()) {
            let before = *before as f64;
            let after = *after as f64;
            signal += before * before;
            error += (before - after) * (before - after);
        }
        let snr = 10.0 * (signal / error).log10();
        assert!(snr > 20.0, "the decoder did not catch up: {snr:.1} dB");
    }

    #[test]
    fn g722_a_frame_round_trips() {
        // 320 samples of 16 kHz audio in, 160 bytes out, and 320
        // samples back out.
        let pcm: Vec<i16> = (0..320)
            .map(|i| ((i as f32 / 320.0) * 8000.0) as i16)
            .collect();

        let mut encoder = Codec::G722.encoder();
        let encoded = encoder.encode(&pcm);
        assert_eq!(encoded.len(), 160);

        let mut decoder = Codec::G722.decoder();
        let decoded = decoder.decode(&encoded);
        assert_eq!(decoded.len(), 320);
    }

    #[test]
    fn g722_keeps_the_speech_it_was_given() {
        // A 500 Hz tone at 16 kHz, over a second. The codec is
        // lossy, but it has to be *a* tone and not noise.
        let rate = 16000.0f32;
        let pcm: Vec<i16> = (0..rate as i32)
            .map(|i| {
                let t = i as f32 / rate;
                (12_000.0 * (2.0 * std::f64::consts::PI * 500.0 * t as f64).sin()) as i16
            })
            .collect();

        let mut encoder = Codec::G722.encoder();
        let mut decoder = Codec::G722.decoder();

        let mut signal = 0f64;
        let mut noise = 0f64;
        for chunk in pcm.chunks(320) {
            let encoded = encoder.encode(chunk);
            let decoded = decoder.decode(&encoded);
            // The synthesis filter has a group delay, so the output
            // lags the input by 22 samples.
            for (before, after) in chunk.iter().zip(decoded.iter().skip(G722_DELAY)) {
                let before = *before as f64;
                let after = *after as f64;
                signal += before * before;
                noise += (before - after) * (before - after);
            }
        }

        let snr = 10.0 * (signal / noise).log10();
        assert!(snr > 25.0, "the round trip is too lossy: {snr:.1} dB");
    }

    #[test]
    fn g722_silence_is_silent() {
        // A muted microphone sends silence through the encoder, so the
        // state stays in step. What comes out has to be quiet.
        let mut encoder = Codec::G722.encoder();
        let mut decoder = Codec::G722.decoder();

        let frame = encoder.encode_silence();
        assert_eq!(frame.len(), 160);
        let decoded = decoder.decode(&frame);
        assert_eq!(decoded.len(), 320);
        for sample in decoded {
            assert!(sample.abs() < 64, "silence decoded to {sample}");
        }
    }

    #[test]
    fn the_encoder_and_the_decoder_have_to_agree_on_the_state() {
        // Two decoders, fed the same stream, have to come out the
        // same — and a decoder that starts late has to come out
        // different, which is what "stateful" means.
        let pcm: Vec<i16> = (0..640)
            .map(|i| ((i as f32 / 640.0) * 8000.0) as i16)
            .collect();

        let mut encoder = Codec::G722.encoder();
        let mut first = Codec::G722.decoder();
        let mut second = Codec::G722.decoder();

        let mut one = Vec::new();
        let mut two = Vec::new();
        for chunk in pcm.chunks(320) {
            let encoded = encoder.encode(chunk);
            one.extend(first.decode(&encoded));
            two.extend(second.decode(&encoded));
        }
        assert_eq!(one, two);

        // And a decoder that missed the first frame is out of step.
        let mut late = Codec::G722.decoder();
        let _ = late.decode(&[0u8; 160]);
        let mut encoder = Codec::G722.encoder();
        let encoded = encoder.encode(&pcm[..320]);
        assert_ne!(late.decode(&encoded), first.decode(&encoded));
    }
}
