//! §5.6 — SIP REFER, i.e. call transfer.
//!
//! A REFER replaces the dialog; transfer is never implemented as an
//! RTP-level side channel. Both flavours send the REFER to the
//! *remote* leg of the call being transferred, because that is the
//! party that has to act on it. They differ in what `Refer-To`
//! points at:
//!
//! * **blind** — the new target. The remote leg dials it.
//! * **attended** — the consultation call that is already up. The
//!   `Replaces` header (RFC 5589) carries that dialog's identity so
//!   the transferee can bridge to it instead of dialling afresh.
//!
//! The C++ reference uses pjsip's `Call::xfer` for the blind case and
//! `Call::xferReplaces` for the attended one; the header construction
//! lives here so it can be unit-tested without a live dialog.

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
    /// The URI of the third party, e.g. `sip:200@example.com`.
    pub uri: String,
    /// The `Replaces` header value identifying the consultation
    /// dialog (RFC 5589). Filled in by the SIP layer from the live
    /// dialog — it cannot be derived from the URI alone.
    pub replaces: Option<String>,
}

/// A REFER request: where it goes and what it carries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReferRequest {
    /// Request-URI of the REFER — the remote leg that must act on it.
    pub request_uri: String,
    /// The `Refer-To` header value.
    pub refer_to: String,
    /// The `Replaces` header value, present for attended transfers.
    pub replaces: Option<String>,
}

impl ReferRequest {
    /// The headers to put on the REFER.
    pub fn headers(&self) -> Vec<(&'static str, &str)> {
        let mut out = vec![("Refer-To", self.refer_to.as_str())];
        if let Some(replaces) = self.replaces.as_deref() {
            out.push(("Replaces", replaces));
        }
        out
    }

    pub fn mode(&self) -> TransferMode {
        if self.replaces.is_some() {
            TransferMode::Attended
        } else {
            TransferMode::Blind
        }
    }
}

/// §5.6 — build the `Refer-To` header value.
///
/// ```text
/// Refer-To: <sip:target@our-host>;referredBy=<sip:our-user@our-host>
/// ```
pub fn refer_to_header(target_uri: &str, local_user: &str, local_host: &str) -> String {
    format!("<{target_uri}>;referredBy=<sip:{local_user}@{local_host}>")
}

/// Build the `Replaces` header value for a dialog (RFC 5589 §4.1).
pub fn replaces_header(call_id: &str, from_tag: &str, to_tag: &str) -> String {
    format!("<{call_id}>;from-tag={from_tag};to-tag={to_tag}")
}

/// A blind transfer: the remote leg is asked to dial `target_uri`.
pub fn blind_transfer(
    remote_leg_uri: &str,
    target_uri: &str,
    local_user: &str,
    local_host: &str,
) -> ReferRequest {
    ReferRequest {
        request_uri: remote_leg_uri.to_string(),
        refer_to: refer_to_header(target_uri, local_user, local_host),
        replaces: None,
    }
}

/// An attended transfer: the remote leg is asked to bridge to the
/// consultation call.
pub fn attended_transfer(
    remote_leg_uri: &str,
    consultation: &Consultation,
    local_user: &str,
    local_host: &str,
) -> ReferRequest {
    ReferRequest {
        request_uri: remote_leg_uri.to_string(),
        refer_to: refer_to_header(&consultation.uri, local_user, local_host),
        replaces: consultation.replaces.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refer_to_carries_the_referred_by_parameter() {
        assert_eq!(
            refer_to_header("sip:6666@our-host", "104", "our-host"),
            "<sip:6666@our-host>;referredBy=<sip:104@our-host>"
        );
    }

    #[test]
    fn blind_transfer_points_at_the_new_target() {
        let refer = blind_transfer("sip:200@peer", "sip:6666@our-host", "104", "our-host");
        assert_eq!(refer.request_uri, "sip:200@peer");
        assert_eq!(
            refer.refer_to,
            "<sip:6666@our-host>;referredBy=<sip:104@our-host>"
        );
        assert_eq!(refer.replaces, None);
        assert_eq!(refer.mode(), TransferMode::Blind);

        let headers = refer.headers();
        assert_eq!(headers.len(), 1);
        assert_eq!(headers[0].0, "Refer-To");
    }

    #[test]
    fn attended_transfer_points_at_the_consultation_and_carries_replaces() {
        let consultation = Consultation {
            uri: "sip:300@our-host".into(),
            replaces: Some(replaces_header("call-42", "from-tag", "to-tag")),
        };
        let refer = attended_transfer("sip:200@peer", &consultation, "104", "our-host");

        // The REFER still goes to the remote leg of the transferred
        // call — that is who has to bridge.
        assert_eq!(refer.request_uri, "sip:200@peer");

        // Refer-To names the consultation, not the original target.
        assert_eq!(
            refer.refer_to,
            "<sip:300@our-host>;referredBy=<sip:104@our-host>"
        );
        assert_eq!(
            refer.replaces.as_deref(),
            Some("<call-42>;from-tag=from-tag;to-tag=to-tag")
        );
        assert_eq!(refer.mode(), TransferMode::Attended);

        let headers = refer.headers();
        assert_eq!(headers.len(), 2);
        assert_eq!(headers[1].0, "Replaces");
    }

    #[test]
    fn replaces_header_follows_rfc_5589() {
        assert_eq!(
            replaces_header("1a2b3c@our-host", "ftag", "ttag"),
            "<1a2b3c@our-host>;from-tag=ftag;to-tag=ttag"
        );
    }
}
