//! The SDP placeholder and the DTMF encoding.
//!
//! ## Why the SDP is a placeholder
//!
//! The media plane is M4: `rtc` negotiates the real offer/answer and
//! `cpal` plays it. Until then the softphone still has to *answer*
//! an INVITE, and a call without any SDP is not a call most servers
//! expect — the answer has to carry a body.
//!
//! So M2 sends a well-formed but minimal SDP: one audio stream, PCMU,
//! on a port that is not really open. The call connects, the SIP trace
//! is clean, and there is no audio. M4 replaces this wholesale with
//! the session `rtc` negotiated.
//!
//! ## DTMF
//!
//! RFC 2833 puts the digits in the RTP stream, which needs the media
//! plane. The signalling-only way to send a digit is an `INFO` request
//! with an `application/dtmf-relay` body (RFC 4733 §3). That is what
//! M2 uses; M4 may switch to RFC 2833.

/// The port the placeholder SDP advertises.
///
/// It is not bound. It only has to be a number that says "there is an
/// audio stream here"; M4 replaces it with the real one.
pub const PLACEHOLDER_MEDIA_PORT: u16 = 4000;

/// The payload type for PCMU/8000.
const PCMU_PAYLOAD_TYPE: u8 = 0;

/// The `Content-Type` of a DTMF relay body.
pub const DTMF_RELAY_CONTENT_TYPE: &str = "application/dtmf-relay";

/// A minimal SDP: one audio stream, PCMU, no real media behind it.
///
/// `address` is the `c=` line — the host the peer should send RTP
/// to. M2 does not know the public address of the machine, so this is
/// the signalling address; M4 replaces the whole thing.
pub fn placeholder_sdp(address: &str, port: u16, session_id: u64) -> Vec<u8> {
    let sdp = format!(
        "v=0\r\n\
         o=- {session_id} {session_id} IN IP4 {address}\r\n\
         s=-\r\n\
         c=IN IP4 {address}\r\n\
         t=0 0\r\n\
         m=audio {port} RTP/AVP {PCMU_PAYLOAD_TYPE}\r\n\
         a=rtpmap:{PCMU_PAYLOAD_TYPE} PCMU/8000\r\n\
         a=ptime:20\r\n\
         a=sendrecv\r\n"
    );
    sdp.into_bytes()
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

    #[test]
    fn the_placeholder_sdp_is_well_formed() {
        let sdp = String::from_utf8(placeholder_sdp("10.0.0.5", 4000, 7)).unwrap();
        assert!(sdp.starts_with("v=0\r\n"), "{sdp}");
        assert!(sdp.contains("o=- 7 7 IN IP4 10.0.0.5\r\n"), "{sdp}");
        assert!(sdp.contains("c=IN IP4 10.0.0.5\r\n"), "{sdp}");
        assert!(sdp.contains("m=audio 4000 RTP/AVP 0\r\n"), "{sdp}");
        assert!(sdp.contains("a=rtpmap:0 PCMU/8000\r\n"), "{sdp}");
        assert!(sdp.ends_with("\r\n"), "{sdp}");
        // Every line has to end with CRLF, or some parsers reject the
        // whole body.
        assert!(!sdp.contains("\n\r"), "{sdp}");
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
