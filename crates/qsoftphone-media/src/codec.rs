//! G.711 — the codec every SIP telephone speaks.
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
//! ## The tables are computed, not written out
//!
//! The classic implementations carry two 256-entry lookup tables per
//! variant. They are generated here from the algorithm instead, which
//! is fewer lines and — more importantly — cannot disagree with the
//! decoder. The algorithms are the ones ITU-T G.711 describes and
//! every softphone implements: a segment (a "chord") of the
//! logarithmic curve, three bits of segment, four bits of
//! quantisation, and a sign bit.

/// The audio codecs this build speaks.
///
/// G.711 is the only one. It is what every SIP peer offers, it needs
/// no licence, and it costs almost nothing to run — which is why it is
/// still the default of every PBX on earth.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Codec {
    /// G.711 µ-law. The American and Japanese standard.
    Pcmu,
    /// G.711 A-law. The European and the rest-of-the-world standard.
    Pcma,
}

impl Codec {
    /// Every codec this build speaks, in the order they are preferred.
    ///
    /// µ-law first: it is the one a peer is most likely to offer, and
    /// the two sound the same.
    pub const ALL: [Codec; 2] = [Codec::Pcmu, Codec::Pcma];

    /// The name the SDP carries — `PCMU` or `PCMA`.
    pub const fn name(self) -> &'static str {
        match self {
            Codec::Pcmu => "PCMU",
            Codec::Pcma => "PCMA",
        }
    }

    /// The static payload type of the codec — 0 for µ-law, 8 for A-law.
    ///
    /// RFC 3551 fixes these two. A peer is free to put the same codec
    /// on a dynamic number instead, and then *its* number is the one
    /// the answer has to echo.
    pub const fn static_payload_type(self) -> u8 {
        match self {
            Codec::Pcmu => 0,
            Codec::Pcma => 8,
        }
    }

    /// The clock rate of the codec. G.711 is always 8 kHz.
    pub const fn clock_rate(self) -> u32 {
        8000
    }

    /// The codec with that name, whatever the case.
    pub fn from_name(name: &str) -> Option<Codec> {
        match name.trim().to_ascii_uppercase().as_str() {
            "PCMU" | "ULAW" | "MU-LAW" | "MULAW" => Some(Codec::Pcmu),
            "PCMA" | "ALAW" | "A-LAW" => Some(Codec::Pcma),
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
            _ => None,
        }
    }

    /// The byte that stands for silence in this codec.
    ///
    /// G.711 has a code for zero: 0xFF in µ-law, 0xD5 in A-law. It is
    /// what a muted microphone sends, and what a decoder turns into
    /// (very nearly) nothing.
    pub const fn silence_byte(self) -> u8 {
        match self {
            Codec::Pcmu => 0xFF,
            Codec::Pcma => 0xD5,
        }
    }

    /// A number that stands for this codec, for the atomic that holds
    /// it.
    pub(crate) const fn code(self) -> u8 {
        match self {
            Codec::Pcmu => 0,
            Codec::Pcma => 1,
        }
    }

    /// The codec a number stands for.
    pub(crate) const fn from_code(code: u8) -> Codec {
        match code {
            1 => Codec::Pcma,
            _ => Codec::Pcmu,
        }
    }

    /// A frame of silence, `samples` long.
    pub fn silence_frame(self, samples: usize) -> Vec<u8> {
        vec![self.silence_byte(); samples]
    }

    /// Encode 16-bit linear PCM into this codec.
    pub fn encode(self, pcm: &[i16]) -> Vec<u8> {
        pcm.iter()
            .map(|&sample| self.encode_sample(sample))
            .collect()
    }

    /// Decode this codec's bytes into 16-bit linear PCM.
    pub fn decode(self, payload: &[u8]) -> Vec<i16> {
        payload
            .iter()
            .map(|&byte| self.decode_sample(byte))
            .collect()
    }

    /// Encode one 16-bit sample.
    pub fn encode_sample(self, sample: i16) -> u8 {
        match self {
            Codec::Pcmu => to_ulaw(sample),
            Codec::Pcma => to_alaw(sample),
        }
    }

    /// Decode one byte into a 16-bit sample.
    pub fn decode_sample(self, byte: u8) -> i16 {
        match self {
            Codec::Pcmu => from_ulaw(byte),
            Codec::Pcma => from_alaw(byte),
        }
    }
}

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_names_are_the_ones_the_sdp_carries() {
        assert_eq!(Codec::Pcmu.name(), "PCMU");
        assert_eq!(Codec::Pcma.name(), "PCMA");
        assert_eq!(Codec::Pcmu.static_payload_type(), 0);
        assert_eq!(Codec::Pcma.static_payload_type(), 8);
        assert_eq!(Codec::Pcmu.clock_rate(), 8000);
    }

    #[test]
    fn a_codec_is_recognised_by_its_name() {
        assert_eq!(Codec::from_name("PCMA"), Some(Codec::Pcma));
        assert_eq!(Codec::from_name("pcmu"), Some(Codec::Pcmu));
        assert_eq!(Codec::from_name(" G722 "), None);
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
        assert_eq!(Codec::from_payload_type(101, None), None);
    }

    #[test]
    fn silence_has_a_code_of_its_own() {
        // The digital silence every G.711 implementation agrees on.
        assert_eq!(Codec::Pcmu.silence_byte(), 0xFF);
        assert_eq!(Codec::Pcma.silence_byte(), 0xD5);
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
        assert_eq!(Codec::Pcmu.encode(&pcm), vec![0xFFu8; 160]);
        assert_eq!(Codec::Pcma.encode(&pcm), vec![0xD5u8; 160]);
    }

    #[test]
    fn a_whole_frame_round_trips() {
        let pcm: Vec<i16> = (0..160)
            .map(|i| ((i as f32 / 160.0) * 8000.0) as i16)
            .collect();
        let encoded = Codec::Pcma.encode(&pcm);
        assert_eq!(encoded.len(), 160);
        let decoded = Codec::Pcma.decode(&encoded);
        assert_eq!(decoded.len(), 160);
        for (before, after) in pcm.iter().zip(decoded.iter()) {
            assert!((before - after).abs() < 300, "{before} -> {after}");
        }
    }
}
