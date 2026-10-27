//! The SDP offer/answer and the DTMF encoding.
//!
//! ## Why the SDP is a placeholder
//!
//! The media plane is M4: `rtc` negotiates the real session and `cpal`
//! plays it. Until then the softphone still has to *answer* an INVITE,
//! and a call without any SDP is not a call most servers expect — the
//! answer has to carry a body.
//!
//! So M2 sends a well-formed but minimal SDP: one audio stream on a
//! port that is not really open. The call connects, the SIP trace is
//! clean, and there is no audio. M4 replaces this wholesale with the
//! session `rtc` negotiated.
//!
//! ## The answer has to match the offer
//!
//! RFC 3264 §6: the answer may only name media formats the offer
//! listed. An answer that names a format the peer did **not** offer is
//! invalid, and the peer is entitled to tear the call down — Asterisk
//! does exactly that, with a BYE right after the `200 OK`.
//!
//! So the answer is built from the offer. The codecs this build speaks
//! are looked for in what the peer offered, and the peer's own payload
//! type is echoed back: a peer that offers only G.711 A-law gets G.711
//! A-law back, not G.711 µ-law.
//!
//! ## DTMF
//!
//! RFC 2833 puts the digits in the RTP stream, which needs the media
//! plane. The signalling-only way to send a digit is an `INFO` request
//! with an `application/dtmf-relay` body (RFC 4733 §3). That is what
//! M2 uses; M4 may switch to RFC 2833.
//!
//! The offer deliberately does **not** list `telephone-event`: there is
//! no RTP yet, so negotiating RFC 2833 would make the peer send the
//! digits into a stream nobody reads. Without it the peer falls back
//! to RFC 4733, which is what this build can receive.

/// The port the placeholder SDP advertises.
///
/// It is not bound. It only has to be a number that says "there is an
/// audio stream here"; M4 replaces it with the real one.
pub const PLACEHOLDER_MEDIA_PORT: u16 = 4000;

/// The `Content-Type` of a DTMF relay body.
pub const DTMF_RELAY_CONTENT_TYPE: &str = "application/dtmf-relay";

/// The codecs this build can speak, in preference order.
///
/// The payload type is the *static* one — what this build offers. In
/// an answer the peer's payload type is used instead, because a peer
/// may have put the same codec on a dynamic number.
const SUPPORTED_CODECS: &[(u8, &str)] = &[(0, "PCMU"), (8, "PCMA")];

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

/// The offer: one audio stream with every codec this build speaks.
///
/// `address` is the `c=` line — the host the peer should send RTP to.
pub fn offer_sdp(address: &str, port: u16, session_id: u64) -> Vec<u8> {
    let payload_types: Vec<String> = SUPPORTED_CODECS
        .iter()
        .map(|(pt, _)| pt.to_string())
        .collect();
    let rtpmaps: String = SUPPORTED_CODECS
        .iter()
        .map(|(pt, name)| format!("a=rtpmap:{pt} {name}/{CLOCK_RATE}\r\n"))
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
pub fn answer_sdp(offer: &[u8], address: &str, port: u16, session_id: u64) -> Vec<u8> {
    let media = match choose_format(offer) {
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

/// The audio formats an offer lists.
///
/// The payload types come from the `m=audio` line, and their names
/// from the `a=rtpmap` lines. A payload type without an rtpmap is
/// looked up in the static table.
pub fn offered_formats(offer: &[u8]) -> Vec<MediaFormat> {
    let text = String::from_utf8_lossy(offer);
    let mut payload_types: Vec<u8> = Vec::new();
    let mut rtpmaps: Vec<(u8, String, u32)> = Vec::new();

    for line in text.lines() {
        let line = line.trim_end_matches('\r');

        if let Some(rest) = line.strip_prefix("m=audio ") {
            // m=<media> <port> <proto> <fmt> ...
            let mut parts = rest.split_whitespace();
            let _port = parts.next();
            let _protocol = parts.next();
            for payload_type in parts {
                if let Ok(number) = payload_type.parse::<u8>() {
                    payload_types.push(number);
                }
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

    payload_types
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
        .collect()
}

/// The format to answer with.
///
/// The first codec this build speaks that the offer listed, with the
/// peer's own payload type. `None` when the offer and this build have
/// nothing in common.
pub fn choose_format(offer: &[u8]) -> Option<MediaFormat> {
    let formats = offered_formats(offer);
    for (_, wanted) in SUPPORTED_CODECS {
        if let Some(format) = formats
            .iter()
            .find(|format| format.encoding.eq_ignore_ascii_case(wanted))
        {
            return Some(format.clone());
        }
    }
    None
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

    #[test]
    fn the_offer_is_well_formed() {
        let sdp = String::from_utf8(offer_sdp("10.0.0.5", 4000, 7)).unwrap();
        assert!(sdp.starts_with("v=0\r\n"), "{sdp}");
        assert!(sdp.contains("o=- 7 7 IN IP4 10.0.0.5\r\n"), "{sdp}");
        assert!(sdp.contains("c=IN IP4 10.0.0.5\r\n"), "{sdp}");
        assert!(sdp.contains("m=audio 4000 RTP/AVP 0 8\r\n"), "{sdp}");
        assert!(sdp.contains("a=rtpmap:0 PCMU/8000\r\n"), "{sdp}");
        assert!(sdp.contains("a=rtpmap:8 PCMA/8000\r\n"), "{sdp}");
        assert!(sdp.ends_with("\r\n"), "{sdp}");
        // Every line has to end with CRLF, or some parsers reject the
        // whole body.
        assert!(!sdp.contains("\n\r"), "{sdp}");
    }

    #[test]
    fn the_answer_to_an_alaw_offer_is_alaw() {
        // The bug this replaces: the answer used to say PCMU whatever
        // the peer offered. Asterisk offers only PCMA, so the answer
        // was invalid and Asterisk hung up with a BYE.
        let answer =
            String::from_utf8(answer_sdp(ASTERISK_OFFER.as_bytes(), "10.0.0.5", 4000, 1)).unwrap();
        assert!(answer.contains("m=audio 4000 RTP/AVP 8\r\n"), "{answer}");
        assert!(answer.contains("a=rtpmap:8 PCMA/8000\r\n"), "{answer}");
        assert!(
            !answer.contains("PCMU"),
            "the answer must not name a codec the peer did not offer: {answer}"
        );
    }

    #[test]
    fn the_answer_to_a_ulaw_offer_is_ulaw() {
        let answer =
            String::from_utf8(answer_sdp(PCMU_OFFER.as_bytes(), "10.0.0.5", 4000, 1)).unwrap();
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
        let format = choose_format(offer.as_bytes()).expect("a format");
        assert_eq!(format.payload_type, 96);
        assert_eq!(format.encoding, "PCMU");

        let answer = String::from_utf8(answer_sdp(offer.as_bytes(), "10.0.0.5", 4000, 1)).unwrap();
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
        assert!(choose_format(offer.as_bytes()).is_none());

        let answer = String::from_utf8(answer_sdp(offer.as_bytes(), "10.0.0.5", 4000, 1)).unwrap();
        assert!(answer.contains("m=audio 0 RTP/AVP 0\r\n"), "{answer}");
    }

    #[test]
    fn an_offer_without_an_rtpmap_uses_the_static_table() {
        let offer = "v=0\r\n\
m=audio 4000 RTP/AVP 0\r\n";
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
