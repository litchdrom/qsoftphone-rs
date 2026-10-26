//! §5.6 — SIP REFER, i.e. call transfer.
//!
//! Both flavours use SIP REFER (RFC 3515). The difference is the
//! `Refer-To` header:
//!
//! ```text
//! Blind:
//!   Refer-To: <sip:bob@target.example.com>
//!
//! Attended (RFC 5589):
//!   Refer-To: <sip:bob@target.example.com;method=INVITE?Replaces=<call-id>%40<host>>
//! ```
//!
//! Note the shape of the attended form: in rsipstack the `Replaces`
//! dialog identifier is a **URI parameter inside the Refer-To
//! value**, not a separate `Replaces` header. That is the form the
//! test suite expects, and it is what Asterisk and Kamailio accept.
//!
//! The C++ reference uses pjsip's `Call::xfer` for the blind case and
//! `Call::xferReplaces` for the attended one, so pjsip builds the
//! header itself. The construction lives here so it can be
//! unit-tested without a live dialog.

/// How a transfer is performed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransferMode {
    /// The remote leg is asked to dial the new target.
    Blind,
    /// RFC 5589 REFER-with-Replaces: the remote leg is asked to
    /// bridge to a consultation call that is already up.
    Attended,
}

impl TransferMode {
    pub fn as_str(self) -> &'static str {
        match self {
            TransferMode::Blind => "blind",
            TransferMode::Attended => "attended",
        }
    }
}

/// The consultation call an attended transfer points at.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Consultation {
    /// The third party's URI, e.g. `sip:bob@target.example.com`.
    pub uri: String,
    /// The consultation dialog's Call-ID **local part** — the bit
    /// before the `@` in the `Call-ID` header.
    pub call_id: String,
    /// The consultation dialog's host.
    pub host: String,
}

/// A REFER request: where it goes and what it carries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReferRequest {
    /// Request-URI of the REFER.
    ///
    /// The REFER is an in-dialog request: it goes to the *transferee*
    /// (the party currently on the other end), because that is the
    /// party that has to act on it and put the new leg up.
    pub request_uri: String,
    /// The `Refer-To` header value.
    pub refer_to: String,
}

impl ReferRequest {
    /// The headers to put on the REFER.
    pub fn headers(&self) -> Vec<(&'static str, &str)> {
        vec![("Refer-To", self.refer_to.as_str())]
    }

    pub fn mode(&self) -> TransferMode {
        if self.refer_to.contains(";method=INVITE") {
            TransferMode::Attended
        } else {
            TransferMode::Blind
        }
    }
}

/// §5.6 — the `Refer-To` header value for a blind transfer.
///
/// ```text
/// Refer-To: <sip:bob@target.example.com>
/// ```
pub fn refer_to_blind(target_uri: &str) -> String {
    format!("<{target_uri}>")
}

/// §5.6 — the `Refer-To` header value for an attended transfer.
///
/// ```text
/// Refer-To: <sip:bob@target.example.com;method=INVITE?Replaces=<call-id>%40<host>>
/// ```
pub fn refer_to_attended(target_uri: &str, dialog_id: &str) -> String {
    format!("<{target_uri};method=INVITE?Replaces={dialog_id}>")
}

/// The `Replaces` dialog identifier: the consultation dialog's
/// Call-ID local part and host, joined by an encoded `@`.
///
/// A SIP Call-ID is `local-part@host`. Inside a URI parameter the
/// `@` has to be percent-encoded, which is what the `%40` in the
/// §5.6 example is.
pub fn replaces_dialog_id(call_id: &str, host: &str) -> String {
    format!("{call_id}%40{host}")
}

/// A blind transfer: the transferee is asked to dial `target_uri`.
pub fn blind_transfer(transferee_uri: &str, target_uri: &str) -> ReferRequest {
    ReferRequest {
        request_uri: transferee_uri.to_string(),
        refer_to: refer_to_blind(target_uri),
    }
}

/// An attended transfer: the transferee is asked to bridge to the
/// consultation call that is already up.
pub fn attended_transfer(transferee_uri: &str, consultation: &Consultation) -> ReferRequest {
    ReferRequest {
        request_uri: transferee_uri.to_string(),
        refer_to: refer_to_attended(
            &consultation.uri,
            &replaces_dialog_id(&consultation.call_id, &consultation.host),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blind_refer_to_is_the_bare_target() {
        assert_eq!(
            refer_to_blind("sip:bob@target.example.com"),
            "<sip:bob@target.example.com>"
        );
    }

    #[test]
    fn attended_refer_to_carries_the_replaces_uri_parameter() {
        // The exact form from §5.6.
        let dialog_id = replaces_dialog_id("<call-id>", "<host>");
        assert_eq!(
            refer_to_attended("sip:bob@target.example.com", &dialog_id),
            "<sip:bob@target.example.com;method=INVITE?Replaces=<call-id>%40<host>>"
        );
    }

    #[test]
    fn the_dialog_id_encodes_the_at_sign() {
        assert_eq!(
            replaces_dialog_id("1a2b3c", "192.0.2.4"),
            "1a2b3c%40192.0.2.4"
        );
    }

    #[test]
    fn a_blind_transfer_points_at_the_new_target() {
        let refer = blind_transfer("sip:alice@peer", "sip:bob@target.example.com");
        assert_eq!(refer.request_uri, "sip:alice@peer");
        assert_eq!(refer.refer_to, "<sip:bob@target.example.com>");
        assert_eq!(refer.mode(), TransferMode::Blind);

        let headers = refer.headers();
        assert_eq!(headers.len(), 1);
        assert_eq!(headers[0].0, "Refer-To");
        assert_eq!(headers[0].1, "<sip:bob@target.example.com>");
    }

    #[test]
    fn an_attended_transfer_points_at_the_consultation() {
        let consultation = Consultation {
            uri: "sip:bob@target.example.com".into(),
            call_id: "1a2b3c".into(),
            host: "192.0.2.4".into(),
        };
        let refer = attended_transfer("sip:alice@peer", &consultation);

        // The REFER still goes to the transferee — that is who has to
        // put the new leg up.
        assert_eq!(refer.request_uri, "sip:alice@peer");

        assert_eq!(
            refer.refer_to,
            "<sip:bob@target.example.com;method=INVITE?Replaces=1a2b3c%40192.0.2.4>"
        );
        assert_eq!(refer.mode(), TransferMode::Attended);
    }
}
