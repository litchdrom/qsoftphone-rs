//! The SDP offer/answer and the DTMF encoding.
//!
//! ## The offer and the answer
//!
//! RFC 3264 §6: the answer may only name media formats the offer
//! listed. An answer that names a format the peer did **not** offer is
//! invalid, and the peer is entitled to tear the call down — Asterisk
//! does exactly that, with a BYE right after the `200 OK`.
//!
//! So the answer is built from the offer. The codecs this build
//! speaks are looked for in what the peer offered, and the peer's own
//! payload type is echoed back: a peer that offers only G.711 A-law
//! gets G.711 A-law back, not G.711 µ-law.
//!
//! ## The port is real
//!
//! M2 advertised a port that was not bound. M4 does not: the media
//! crate binds a UDP socket before the offer is built, and the port
//! that socket got is the port the SDP names. A port that is named
//! but not bound is a port the peer's audio disappears into.
//!
//! ## DTMF
//!
//! RFC 2833 puts the digits in the RTP stream, which needs the media
//! plane. The signalling-only way to send a digit is an `INFO` request
//! with an `application/dtmf-relay` body (RFC 4733 §3). That is what
//! M2 uses; M4 may switch to RFC 2833.
//!
//! The offer deliberately does **not** list `telephone-event`: the
//! peer would then send the digits into a stream nobody reads.
//! Without it the peer falls back to RFC 4733, which is what this
//! build can receive.

use qsoftphone_media::Codec;

/// The `Content-Type` of a DTMF relay body.
pub const DTMF_RELAY_CONTENT_TYPE: &str = "application/dtmf-relay";

/// The clock rate of every codec this build speaks.
const CLOCK_RATE: u32 = 8000;

/// The static payload types, for an offer that omits the rtpmap.
const STATIC_FORMATS: &[(u8, &str, u32)] = &[
    (0, "PCMU", 8000),
    (3, "GSM", 8000),
    (4, "G723", 8000),
    (8, "PCMA", 8000),
    (9, "G722", 8000),
    (18, "G729", 8000),
];

/// One media format from an SDP.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MediaFormat {
    /// The payload type number.
    pub payload_type: u8,
    /// The encoding name — `PCMU`, `PCMA`, ...
    pub encoding: String,
    /// The clock rate — 8000 for G.711.
    pub clock_rate: u32,
}

/// The media part of an SDP.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MediaDescription {
    /// The port the `m=` line names.
    ///
    /// Zero means the peer does not want any media: RFC 3264 §6, the
    /// polite way of saying "no".
    pub port: u16,
    /// The address the RTP comes from — the `c=` line.
    pub address: Option<String>,
    /// The formats the `m=` line lists, in order.
    pub formats: Vec<MediaFormat>,
}

/// What the peer's answer settled on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Answer {
    /// The codec the peer chose.
    pub codec: Codec,
    /// The payload type the peer put it on.
    ///
    /// A peer may put G.711 on a dynamic number, and then this is the
    /// number that has to be used — not the static one.
    pub payload_type: u8,
    /// The address the peer's RTP comes from.
    pub address: String,
    /// The port the peer's RTP comes from.
    pub port: u16,
}

/// The offer: one audio stream with the codecs this build speaks.
///
/// `address` is the `c=` line — the host the peer should send RTP to.
/// `port` is the port the media socket is bound to.
pub fn offer_sdp(address: &str, port: u16, session_id: u64, codecs: &[Codec]) -> Vec<u8> {
    let payload_types: Vec<String> = codecs
        .iter()
        .map(|codec| codec.static_payload_type().to_string())
        .collect();
    let rtpmaps: String = codecs
        .iter()
        .map(|codec| {
            format!(
                "a=rtpmap:{} {}/{CLOCK_RATE}\r\n",
                codec.static_payload_type(),
                codec.name()
            )
        })
        .collect();

    let sdp = format!(
        "v=0\r\n\
         o=- {session_id} {session_id} IN IP4 {address}\r\n\
         s=-\r\n\
         c=IN IP4 {address}\r\n\
         t=0 0\r\n\
         m=audio {port} RTP/AVP {payload_types}\r\n\
         {rtpmaps}\
         a=ptime:20\r\n\
         a=sendrecv\r\n",
        payload_types = payload_types.join(" "),
    );
    sdp.into_bytes()
}

/// The answer to an offer.
///
/// The codec is one the offer listed — RFC 3264 §6. When this build
/// speaks none of them the stream is rejected with `m=audio 0`, which
/// is the polite way of saying "no media"; the call itself stays up.
pub fn answer_sdp(
    offer: &[u8],
    address: &str,
    port: u16,
    session_id: u64,
    codecs: &[Codec],
) -> Vec<u8> {
    let media = match choose_format(offer, codecs) {
        Some(format) => format!(
            "m=audio {port} RTP/AVP {pt}\r\n\
             a=rtpmap:{pt} {encoding}/{clock}\r\n\
             a=ptime:20\r\n\
             a=sendrecv\r\n",
            pt = format.payload_type,
            encoding = format.encoding,
            clock = format.clock_rate,
        ),
        None => "m=audio 0 RTP/AVP 0\r\n".to_string(),
    };

    let sdp = format!(
        "v=0\r\n\
         o=- {session_id} {session_id} IN IP4 {address}\r\n\
         s=-\r\n\
         c=IN IP4 {address}\r\n\
         t=0 0\r\n\
         {media}"
    );
    sdp.into_bytes()
}

/// The media part of an SDP.
///
/// The `c=` line may be at the session level or at the media level,
/// and the media level wins — RFC 4566 §5.7. Both are looked at.
pub fn parse_media(body: &[u8]) -> Option<MediaDescription> {
    let text = String::from_utf8_lossy(body);

    let mut session_address: Option<String> = None;
    let mut media_address: Option<String> = None;
    let mut port: Option<u16> = None;
    let mut payload_types: Vec<u8> = Vec::new();
    let mut rtpmaps: Vec<(u8, String, u32)> = Vec::new();
    let mut in_audio = false;
    let mut seen_media = false;

    for line in text.lines() {
        let line = line.trim_end_matches('\r');

        if let Some(rest) = line.strip_prefix("m=") {
            seen_media = true;
            in_audio = rest.starts_with("audio ");
            if in_audio {
                let mut parts = rest["audio ".len()..].split_whitespace();
                port = parts.next().and_then(|p| p.parse::<u16>().ok());
                let _protocol = parts.next();
                for payload_type in parts {
                    if let Ok(number) = payload_type.parse::<u8>() {
                        payload_types.push(number);
                    }
                }
            }
            continue;
        }

        if !seen_media {
            // The session level: the `c=` line applies to every
            // media description unless one overrides it.
            if let Some(rest) = line.strip_prefix("c=") {
                if session_address.is_none() {
                    session_address = connection_address(rest);
                }
            }
            continue;
        }

        if !in_audio {
            // A different media type — a video stream, say. Its
            // attributes are not ours.
            continue;
        }

        if let Some(rest) = line.strip_prefix("c=") {
            if let Some(address) = connection_address(rest) {
                media_address = Some(address);
            }
        } else if let Some(rest) = line.strip_prefix("a=rtpmap:") {
            // a=rtpmap:<pt> <encoding>/<clock>[/<channels>]
            let Some((payload_type, parameters)) = rest.split_once(' ') else {
                continue;
            };
            let Ok(payload_type) = payload_type.trim().parse::<u8>() else {
                continue;
            };
            let mut fields = parameters.trim().split('/');
            let encoding = fields.next().unwrap_or_default().to_string();
            let clock_rate = fields
                .next()
                .and_then(|rate| rate.parse::<u32>().ok())
                .unwrap_or(CLOCK_RATE);
            rtpmaps.push((payload_type, encoding, clock_rate));
        }
    }

    if !seen_media {
        return None;
    }

    let formats = payload_types
        .into_iter()
        .map(
            |payload_type| match rtpmaps.iter().find(|(pt, _, _)| *pt == payload_type) {
                Some((_, encoding, clock_rate)) => MediaFormat {
                    payload_type,
                    encoding: encoding.clone(),
                    clock_rate: *clock_rate,
                },
                None => {
                    let (encoding, clock_rate) = STATIC_FORMATS
                        .iter()
                        .find(|(pt, _, _)| *pt == payload_type)
                        .map(|(_, encoding, clock_rate)| (*encoding, *clock_rate))
                        .unwrap_or(("unknown", CLOCK_RATE));
                    MediaFormat {
                        payload_type,
                        encoding: encoding.to_string(),
                        clock_rate,
                    }
                }
            },
        )
        .collect();

    Some(MediaDescription {
        port: port.unwrap_or(0),
        address: media_address.or(session_address),
        formats,
    })
}

/// The address of a `c=` line.
///
/// The line is `c=<nettype> <addrtype> <address>`; a peer may add
/// more fields after the address, and a multicast address comes with
/// a TTL.
fn connection_address(rest: &str) -> Option<String> {
    let address = rest.split_whitespace().nth(2)?;
    if address.is_empty() {
        None
    } else {
        Some(address.to_string())
    }
}

/// The audio formats an offer lists.
///
/// The payload types come from the `m=audio` line, and their names
/// from the `a=rtpmap` lines. A payload type without an rtpmap is
/// looked up in the static table.
pub fn offered_formats(offer: &[u8]) -> Vec<MediaFormat> {
    parse_media(offer)
        .map(|media| media.formats)
        .unwrap_or_default()
}

/// The format to answer with.
///
/// The first codec this build speaks that the offer listed, with the
/// peer's own payload type. `None` when the offer and this build have
/// nothing in common.
pub fn choose_format(offer: &[u8], codecs: &[Codec]) -> Option<MediaFormat> {
    let formats = offered_formats(offer);
    for wanted in codecs {
        if let Some(format) = formats
            .iter()
            .find(|format| format.encoding.eq_ignore_ascii_case(wanted.name()))
        {
            return Some(format.clone());
        }
    }
    None
}

/// What the peer's answer settled on.
///
/// The peer picks one of the formats the offer listed, and says
/// which: the payload type it uses, the address its RTP comes from
/// and the port it listens on. All three are needed — the payload
/// type to decode the RTP, the address and the port to send it to.
pub fn parse_answer(answer: &[u8], codecs: &[Codec]) -> Option<Answer> {
    let media = parse_media(answer)?;

    // A port of zero says the peer does not want any media.
    if media.port == 0 {
        return None;
    }

    // The peer may have chosen a codec this build does not speak.
    // There is nothing to do about that: the call stays up, and the
    // media stays silent.
    for format in &media.formats {
        let Some(codec) = Codec::from_payload_type(format.payload_type, Some(&format.encoding))
        else {
            continue;
        };
        if !codecs.contains(&codec) {
            continue;
        }
        return Some(Answer {
            codec,
            payload_type: format.payload_type,
            address: media.address.clone()?,
            port: media.port,
        });
    }

    None
}

/// Where the peer's RTP comes from, in an offer.
///
/// The peer said it in its offer, and it does not change: the answer
/// only confirms the codec.
pub fn media_endpoint(offer: &[u8]) -> Option<(String, u16)> {
    let media = parse_media(offer)?;
    if media.port == 0 {
        return None;
    }
    Some((media.address?, media.port))
}

/// The `application/dtmf-relay` body for one digit.
///
/// RFC 4733 §3: `Signal` is the key, `Duration` is in milliseconds.
pub fn dtmf_relay_body(digit: &str, duration_ms: u32) -> Vec<u8> {
    format!("Signal={digit}\r\nDuration={duration_ms}\r\n").into_bytes()
}

/// The digit and duration from an `application/dtmf-relay` body.
///
/// Returns `None` when the body is not a DTMF relay — a peer may
/// send anything in an `INFO`.
pub fn parse_dtmf_relay(body: &[u8]) -> Option<(String, u32)> {
    let text = std::str::from_utf8(body).ok()?;
    let mut digit = None;
    let mut duration = None;

    for line in text.lines() {
        let (key, value) = line.split_once('=')?;
        match key.trim().to_ascii_lowercase().as_str() {
            "signal" => digit = Some(value.trim().to_string()),
            "duration" => duration = value.trim().parse::<u32>().ok(),
            _ => {}
        }
    }

    Some((digit?, duration.unwrap_or(100)))
}

/// The digit for an RFC 4733 event code.
///
/// RFC 4733 §2.3: the digits are 0-9, `*` is 10, `#` is 11 and
/// `A`-`D` are 12-15.
pub fn digit_from_event_code(code: u8) -> char {
    match code {
        0..=9 => (b'0' + code) as char,
        10 => '*',
        11 => '#',
        12 => 'A',
        13 => 'B',
        14 => 'C',
        15 => 'D',
        _ => '?',
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The offer Asterisk sends — G.711 A-law and telephone-event.
    const ASTERISK_OFFER: &str = "v=0\r\n\
o=- 124519 124519 IN IP4 193.178.169.164\r\n\
s=asterisk\r\n\
c=IN IP4 193.178.169.164\r\n\
t=0 0\r\n\
m=audio 50344 RTP/AVP 8 101\r\n\
a=rtpmap:8 PCMA/8000\r\n\
a=rtpmap:101 telephone-event/8000\r\n\
a=ptime:20\r\n";

    /// The offer a G.711 µ-law peer sends.
    const PCMU_OFFER: &str = "v=0\r\n\
o=- 1 1 IN IP4 10.0.0.1\r\n\
s=-\r\n\
c=IN IP4 10.0.0.1\r\n\
t=0 0\r\n\
m=audio 4000 RTP/AVP 0\r\n\
a=rtpmap:0 PCMU/8000\r\n";

    /// The codecs this build speaks, in the default order.
    fn codecs() -> Vec<Codec> {
        Codec::ALL.to_vec()
    }

    #[test]
    fn the_offer_is_well_formed() {
        let sdp = String::from_utf8(offer_sdp("10.0.0.5", 41234, 7, &codecs())).unwrap();
        assert!(sdp.starts_with("v=0\r\n"), "{sdp}");
        assert!(sdp.contains("o=- 7 7 IN IP4 10.0.0.5\r\n"), "{sdp}");
        assert!(sdp.contains("c=IN IP4 10.0.0.5\r\n"), "{sdp}");
        assert!(sdp.contains("m=audio 41234 RTP/AVP 0 8\r\n"), "{sdp}");
        assert!(sdp.contains("a=rtpmap:0 PCMU/8000\r\n"), "{sdp}");
        assert!(sdp.contains("a=rtpmap:8 PCMA/8000\r\n"), "{sdp}");
        assert!(sdp.ends_with("\r\n"), "{sdp}");
        // Every line has to end with CRLF, or some parsers reject the
        // whole body.
        assert!(!sdp.contains("\n\r"), "{sdp}");
    }

    #[test]
    fn the_offer_names_the_port_the_socket_got() {
        // The port is not a constant any more: it is the one the
        // media socket is bound to.
        let sdp = String::from_utf8(offer_sdp("10.0.0.5", 54321, 1, &codecs())).unwrap();
        assert!(sdp.contains("m=audio 54321 RTP/AVP"), "{sdp}");
    }

    #[test]
    fn the_offer_can_name_one_codec() {
        // The C++ let the user pick the codec. When the user picks
        // one, only that one is offered.
        let sdp = String::from_utf8(offer_sdp("10.0.0.5", 4000, 1, &[Codec::Pcma])).unwrap();
        assert!(sdp.contains("m=audio 4000 RTP/AVP 8\r\n"), "{sdp}");
        assert!(!sdp.contains("PCMU"), "{sdp}");
    }

    #[test]
    fn the_answer_to_an_alaw_offer_is_alaw() {
        // The bug this replaces: the answer used to say PCMU whatever
        // the peer offered. Asterisk offers only PCMA, so the answer
        // was invalid and Asterisk hung up with a BYE.
        let answer = String::from_utf8(answer_sdp(
            ASTERISK_OFFER.as_bytes(),
            "10.0.0.5",
            41234,
            1,
            &codecs(),
        ))
        .unwrap();
        assert!(answer.contains("m=audio 41234 RTP/AVP 8\r\n"), "{answer}");
        assert!(answer.contains("a=rtpmap:8 PCMA/8000\r\n"), "{answer}");
        assert!(
            !answer.contains("PCMU"),
            "the answer must not name a codec the peer did not offer: {answer}"
        );
    }

    #[test]
    fn the_answer_to_a_ulaw_offer_is_ulaw() {
        let answer = String::from_utf8(answer_sdp(
            PCMU_OFFER.as_bytes(),
            "10.0.0.5",
            4000,
            1,
            &codecs(),
        ))
        .unwrap();
        assert!(answer.contains("m=audio 4000 RTP/AVP 0\r\n"), "{answer}");
        assert!(answer.contains("a=rtpmap:0 PCMU/8000\r\n"), "{answer}");
    }

    #[test]
    fn a_dynamic_payload_type_is_echoed_back() {
        // The peer put PCMU on a dynamic payload type. The answer has
        // to use the same number — the codec name alone is not enough.
        let offer = "v=0\r\n\
o=- 1 1 IN IP4 10.0.0.1\r\n\
s=-\r\n\
c=IN IP4 10.0.0.1\r\n\
t=0 0\r\n\
m=audio 4000 RTP/AVP 96\r\n\
a=rtpmap:96 PCMU/8000\r\n";
        let format = choose_format(offer.as_bytes(), &codecs()).expect("a format");
        assert_eq!(format.payload_type, 96);
        assert_eq!(format.encoding, "PCMU");

        let answer =
            String::from_utf8(answer_sdp(offer.as_bytes(), "10.0.0.5", 4000, 1, &codecs()))
                .unwrap();
        assert!(answer.contains("m=audio 4000 RTP/AVP 96\r\n"), "{answer}");
        assert!(answer.contains("a=rtpmap:96 PCMU/8000\r\n"), "{answer}");
    }

    #[test]
    fn a_codec_nobody_speaks_is_rejected_with_port_zero() {
        // RFC 3264 §6: a port of zero says "no media", and the call
        // itself stays up.
        let offer = "v=0\r\n\
o=- 1 1 IN IP4 10.0.0.1\r\n\
s=-\r\n\
c=IN IP4 10.0.0.1\r\n\
t=0 0\r\n\
m=audio 4000 RTP/AVP 9\r\n\
a=rtpmap:9 G722/8000\r\n";
        assert!(choose_format(offer.as_bytes(), &codecs()).is_none());

        let answer =
            String::from_utf8(answer_sdp(offer.as_bytes(), "10.0.0.5", 4000, 1, &codecs()))
                .unwrap();
        assert!(answer.contains("m=audio 0 RTP/AVP 0\r\n"), "{answer}");
    }

    #[test]
    fn an_offer_without_an_rtpmap_uses_the_static_table() {
        let offer = "v=0\r\nm=audio 4000 RTP/AVP 0\r\n";
        let formats = offered_formats(offer.as_bytes());
        assert_eq!(formats.len(), 1);
        assert_eq!(formats[0].payload_type, 0);
        assert_eq!(formats[0].encoding, "PCMU");
        assert_eq!(formats[0].clock_rate, 8000);
    }

    #[test]
    fn the_offer_formats_are_listed_in_order() {
        let formats = offered_formats(ASTERISK_OFFER.as_bytes());
        assert_eq!(formats.len(), 2);
        assert_eq!(formats[0].payload_type, 8);
        assert_eq!(formats[0].encoding, "PCMA");
        assert_eq!(formats[1].payload_type, 101);
        assert_eq!(formats[1].encoding, "telephone-event");
    }

    #[test]
    fn an_offer_with_no_media_lists_no_formats() {
        assert!(offered_formats(b"").is_empty());
        assert!(offered_formats(b"v=0\r\n").is_empty());
    }

    #[test]
    fn the_media_port_and_address_are_read() {
        let media = parse_media(ASTERISK_OFFER.as_bytes()).expect("the media");
        assert_eq!(media.port, 50344);
        assert_eq!(media.address.as_deref(), Some("193.178.169.164"));
    }

    #[test]
    fn a_media_level_address_wins_over_the_session_level_one() {
        // RFC 4566 §5.7: the media level `c=` overrides the session
        // level. A peer that sends its RTP from a different host than
        // the one it describes in the session part is not rare.
        let offer = "v=0\r\n\
o=- 1 1 IN IP4 10.0.0.1\r\n\
s=-\r\n\
c=IN IP4 10.0.0.1\r\n\
t=0 0\r\n\
m=audio 4000 RTP/AVP 0\r\n\
c=IN IP4 192.168.1.50\r\n\
a=rtpmap:0 PCMU/8000\r\n";
        let media = parse_media(offer.as_bytes()).expect("the media");
        assert_eq!(media.address.as_deref(), Some("192.168.1.50"));
    }

    #[test]
    fn a_video_stream_does_not_confuse_the_audio_one() {
        let offer = "v=0\r\n\
o=- 1 1 IN IP4 10.0.0.1\r\n\
s=-\r\n\
c=IN IP4 10.0.0.1\r\n\
t=0 0\r\n\
m=audio 4000 RTP/AVP 0\r\n\
a=rtpmap:0 PCMU/8000\r\n\
m=video 4002 RTP/AVP 96\r\n\
a=rtpmap:96 H264/90000\r\n";
        let media = parse_media(offer.as_bytes()).expect("the media");
        assert_eq!(media.port, 4000);
        assert_eq!(media.formats.len(), 1);
        assert_eq!(media.formats[0].encoding, "PCMU");
    }

    #[test]
    fn an_answer_says_which_codec_the_peer_chose() {
        // Asterisk answers our offer with A-law, on the static
        // payload type 8.
        let answer = "v=0\r\n\
o=- 124519 124519 IN IP4 193.178.169.164\r\n\
s=asterisk\r\n\
c=IN IP4 193.178.169.164\r\n\
t=0 0\r\n\
m=audio 50344 RTP/AVP 8\r\n\
a=rtpmap:8 PCMA/8000\r\n\
a=ptime:20\r\n";
        let parsed = parse_answer(answer.as_bytes(), &codecs()).expect("an answer");
        assert_eq!(parsed.codec, Codec::Pcma);
        assert_eq!(parsed.payload_type, 8);
        assert_eq!(parsed.address, "193.178.169.164");
        assert_eq!(parsed.port, 50344);
    }

    #[test]
    fn an_answer_on_a_dynamic_payload_type_is_honoured() {
        let answer = "v=0\r\n\
o=- 1 1 IN IP4 10.0.0.1\r\n\
s=-\r\n\
c=IN IP4 10.0.0.1\r\n\
t=0 0\r\n\
m=audio 5000 RTP/AVP 96\r\n\
a=rtpmap:96 PCMU/8000\r\n";
        let parsed = parse_answer(answer.as_bytes(), &codecs()).expect("an answer");
        assert_eq!(parsed.codec, Codec::Pcmu);
        assert_eq!(parsed.payload_type, 96);
        assert_eq!(parsed.port, 5000);
    }

    #[test]
    fn an_answer_that_refuses_the_media_is_no_answer() {
        // `m=audio 0` says the peer does not want any media.
        let answer = "v=0\r\n\
o=- 1 1 IN IP4 10.0.0.1\r\n\
s=-\r\n\
c=IN IP4 10.0.0.1\r\n\
t=0 0\r\n\
m=audio 0 RTP/AVP 0\r\n";
        assert!(parse_answer(answer.as_bytes(), &codecs()).is_none());
    }

    #[test]
    fn an_answer_in_a_codec_we_do_not_speak_is_no_answer() {
        let answer = "v=0\r\n\
o=- 1 1 IN IP4 10.0.0.1\r\n\
s=-\r\n\
c=IN IP4 10.0.0.1\r\n\
t=0 0\r\n\
m=audio 5000 RTP/AVP 9\r\n\
a=rtpmap:9 G722/8000\r\n";
        assert!(parse_answer(answer.as_bytes(), &codecs()).is_none());
    }

    #[test]
    fn the_peer_endpoint_comes_out_of_the_offer() {
        let endpoint = media_endpoint(ASTERISK_OFFER.as_bytes()).expect("an endpoint");
        assert_eq!(endpoint, ("193.178.169.164".to_string(), 50344));
    }

    #[test]
    fn the_dtmf_body_follows_rfc_4733() {
        let body = String::from_utf8(dtmf_relay_body("5", 120)).unwrap();
        assert_eq!(body, "Signal=5\r\nDuration=120\r\n");
    }

    #[test]
    fn a_dtmf_body_round_trips() {
        let (digit, duration) = parse_dtmf_relay(&dtmf_relay_body("7", 250)).unwrap();
        assert_eq!(digit, "7");
        assert_eq!(duration, 250);
    }

    #[test]
    fn a_dtmf_body_without_a_duration_still_parses() {
        let (digit, duration) = parse_dtmf_relay(b"Signal=#\r\n").unwrap();
        assert_eq!(digit, "#");
        assert_eq!(duration, 100);
    }

    #[test]
    fn the_event_codes_map_to_digits() {
        assert_eq!(digit_from_event_code(0), '0');
        assert_eq!(digit_from_event_code(9), '9');
        assert_eq!(digit_from_event_code(10), '*');
        assert_eq!(digit_from_event_code(11), '#');
        assert_eq!(digit_from_event_code(12), 'A');
        assert_eq!(digit_from_event_code(99), '?');
    }

    #[test]
    fn something_that_is_not_a_dtmf_body_is_rejected() {
        assert!(parse_dtmf_relay(b"hello").is_none());
        assert!(parse_dtmf_relay(b"").is_none());
    }
}
