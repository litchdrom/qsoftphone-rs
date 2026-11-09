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
//! Note the shape of the attended form: the `Replaces` dialog
//! identifier is a **URI parameter inside the Refer-To value** (RFC
//! 3515 §2.1). It is also sent as a **`Replaces` header** (RFC 3891
//! §3.1), because the two are not the same thing to the server:
//!
//! * a server that reads the URI parameter finds the dialog itself;
//! * a server that reads the **header** — and Asterisk does
//!   (`ast_sip_get_replaces()`) — does not look at the URI at all.
//!
//! With only the URI parameter, Asterisk could not find the
//! consultation and treated the transfer as one to another machine: the
//! user pressed "Complete transfer" and the server answered "no route
//! to target" (the NOTIFY said `terminated;reason=noresource`) and
//! took the consultation down. The C++ reference sends the header —
//! pjsip's `Call::xferReplaces` builds one — so both are sent here.
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
    /// The consultation dialog's **to-tag**: the tag the peer put on
    /// the `To` header of its answer.
    ///
    /// This is the dialog's remote tag — the peer's own tag.
    pub to_tag: String,
    /// The consultation dialog's **from-tag**: the tag we put on our
    /// own `From` header.
    ///
    /// This is the dialog's local tag — ours.
    pub from_tag: String,
}

/// How a transfer is going.
///
/// The codes are the ones RFC 3515 §7.1.2 puts in the body of the
/// NOTIFY that reports the progress of the referred call.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TransferProgress {
    /// The peer accepted the REFER and is putting the new leg up.
    Trying,
    /// The new target is ringing.
    Ringing,
    /// The new target answered: the transfer worked.
    Answered,
    /// The transfer failed.
    Failed,
}

/// What a transfer's progress code means.
///
/// RFC 3515 §7.1.2: `100 Trying`, `180 Ringing` or `183 Session
/// Progress` while the new leg comes up, `200 OK` when it is answered,
/// and a `4xx` or `5xx` when it fails.
///
/// Anything else is a progress report this build does not know, and it
/// is still in flight — so it is a "trying", not a failure. A transfer
/// that is judged broken by a code it has never seen is a transfer that
/// breaks on a server that is only being helpful.
pub fn progress_of(code: u16) -> TransferProgress {
    match code {
        100 => TransferProgress::Trying,
        180 | 183 => TransferProgress::Ringing,
        200 => TransferProgress::Answered,
        400..=699 => TransferProgress::Failed,
        _ => TransferProgress::Trying,
    }
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
    /// The `Replaces` header value, for an attended transfer.
    ///
    /// The dialog the transfer points at is named twice: as a URI
    /// parameter in the `Refer-To` and as this header. A server reads
    /// one or the other — and a server that reads neither cannot find
    /// the dialog, which is a transfer to another machine.
    pub replaces: Option<String>,
}

impl ReferRequest {
    /// The headers to put on the REFER.
    pub fn headers(&self) -> Vec<(&'static str, &str)> {
        let mut headers = vec![("Refer-To", self.refer_to.as_str())];
        if let Some(replaces) = self.replaces.as_deref() {
            headers.push(("Replaces", replaces));
        }
        headers
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
///
/// A Call-ID with no host at all is passed through unchanged: a
/// trailing `%40` with nothing behind it is not an identifier, it is
/// a fragment.
///
/// This is §5.6's shape, and it is **not enough**: a server that
/// matches a dialog by all three of its identifiers — the Call-ID and
/// the two tags — cannot match a Call-ID on its own. See
/// [`replaces_dialog_id_with_tags`], which is what goes on the wire.
pub fn replaces_dialog_id(call_id: &str, host: &str) -> String {
    if host.is_empty() {
        call_id.to_string()
    } else {
        format!("{call_id}%40{host}")
    }
}

/// The `Replaces` dialog identifier with the dialog's two tags.
///
/// ```text
/// Replaces=<call-id>%40<host>%3Bto-tag%3D<to-tag>%3Bfrom-tag%3D<from-tag>
/// ```
///
/// RFC 3891 §3.1: a dialog is identified by the **three** things
/// that name it — the Call-ID, the tag the peer put on its `To`, and
/// the tag we put on our `From`. Asterisk matches on all three:
///
/// ```c
/// pjsip_ua_find_dialog(&replaces->call_id, &replaces->to_tag, &replaces->from_tag, PJ_TRUE)
/// ```
///
/// A `Replaces` with only the Call-ID is a dialog the server cannot
/// find, and a server that cannot find it treats the transfer as one
/// to another machine — which on Asterisk means an `external_replaces`
/// extension in the dialplan, and a transfer that does not happen.
///
/// This is also what pjsip sends — `pjsua_call_xfer_replaces` builds
/// the same string, with the `;` and `=` percent-encoded because the
/// value is parsed as a `Replaces` header once the escapes are undone.
pub fn replaces_dialog_id_with_tags(
    call_id: &str,
    host: &str,
    to_tag: &str,
    from_tag: &str,
) -> String {
    format!(
        "{}%3Bto-tag%3D{to_tag}%3Bfrom-tag%3D{from_tag}",
        replaces_dialog_id(call_id, host)
    )
}

/// The `Replaces` header value: the dialog's Call-ID and its two tags.
///
/// ```text
/// Replaces: <call-id>@<host>;to-tag=<to-tag>;from-tag=<from-tag>
/// ```
///
/// RFC 3891 §3.1: the header names the dialog by the same three
/// things the URI parameter does — the Call-ID and the two tags — but
/// **unescaped**. A header is not a URI parameter: a server that
/// parses the value as one cannot find the dialog, and a server that
/// cannot find the dialog treats the transfer as one to another
/// machine.
///
/// Asterisk reads this header and nothing else
/// (`ast_sip_get_replaces()`), which is why it goes out as well as in
/// the `Refer-To`.
pub fn replaces_header(call_id: &str, host: &str, to_tag: &str, from_tag: &str) -> String {
    let id = if host.is_empty() {
        call_id.to_string()
    } else {
        format!("{call_id}@{host}")
    };
    format!("{id};to-tag={to_tag};from-tag={from_tag}")
}

/// The DTMF sequence a PBX feature-code transfer is typed as.
///
/// This is how a **hardware SIP phone** transfers: the transfer key
/// sends the PBX's code, the user dials the target, and the phone
/// sends the terminator. The PBX does the transfer; the phone only
/// types.
///
/// Asterisk's `*2` is the blind transfer code and `*3` the attended
/// one — both of them dialplan extensions, so what they do depends on
/// the server's configuration, not on anything in the SIP message.
///
/// The spaces are stripped, because a target pasted as `555 88` is
/// still the number `55588`.
pub fn feature_code_sequence(prefix: &str, target: &str) -> String {
    let cleaned: String = target
        .trim()
        .chars()
        .filter(|c| !c.is_whitespace())
        .collect();
    format!("{prefix}{cleaned}#")
}

/// A blind transfer: the transferee is asked to dial `target_uri`.
pub fn blind_transfer(transferee_uri: &str, target_uri: &str) -> ReferRequest {
    ReferRequest {
        request_uri: transferee_uri.to_string(),
        refer_to: refer_to_blind(target_uri),
        // A blind transfer has no dialog to replace: the peer dials a
        // new one.
        replaces: None,
    }
}

/// An attended transfer: the transferee is asked to bridge to the
/// consultation call that is already up.
pub fn attended_transfer(transferee_uri: &str, consultation: &Consultation) -> ReferRequest {
    ReferRequest {
        request_uri: transferee_uri.to_string(),
        refer_to: refer_to_attended(
            &consultation.uri,
            &replaces_dialog_id_with_tags(
                &consultation.call_id,
                &consultation.host,
                &consultation.to_tag,
                &consultation.from_tag,
            ),
        ),
        // The same dialog, named the way a server that reads the
        // header finds it.
        replaces: Some(replaces_header(
            &consultation.call_id,
            &consultation.host,
            &consultation.to_tag,
            &consultation.from_tag,
        )),
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
    fn the_replaces_header_names_the_dialog_without_escaping_it() {
        // A header is not a URI parameter: the value goes on the wire
        // as it is, and a server that parses it as an escaped URI
        // parameter cannot find the dialog.
        assert_eq!(
            replaces_header("1a2b3c", "192.0.2.4", "peer-tag", "our-tag"),
            "1a2b3c@192.0.2.4;to-tag=peer-tag;from-tag=our-tag"
        );
    }

    #[test]
    fn an_attended_transfer_carries_the_replaces_header() {
        // The server that reads the header (Asterisk does) and the one
        // that reads the URI parameter (RFC 3515 §2.1) have to find the
        // same dialog.
        let consultation = Consultation {
            uri: "sip:bob@target.example.com".into(),
            call_id: "1a2b3c".into(),
            host: "192.0.2.4".into(),
            to_tag: "peer-tag".into(),
            from_tag: "our-tag".into(),
        };
        let refer = attended_transfer("sip:alice@peer", &consultation);

        let headers = refer.headers();
        let replaces = headers
            .iter()
            .find(|(name, _)| *name == "Replaces")
            .map(|(_, value)| *value)
            .expect("the REFER carries no Replaces header");
        assert_eq!(
            replaces, "1a2b3c@192.0.2.4;to-tag=peer-tag;from-tag=our-tag",
            "the Replaces header does not name the dialog"
        );
    }

    #[test]
    fn an_attended_transfer_points_at_the_consultation() {
        let consultation = Consultation {
            uri: "sip:bob@target.example.com".into(),
            call_id: "1a2b3c".into(),
            host: "192.0.2.4".into(),
            to_tag: "peer-tag".into(),
            from_tag: "our-tag".into(),
        };
        let refer = attended_transfer("sip:alice@peer", &consultation);

        // The REFER still goes to the transferee — that is who has to
        // put the new leg up.
        assert_eq!(refer.request_uri, "sip:alice@peer");

        assert_eq!(
            refer.refer_to,
            "<sip:bob@target.example.com;method=INVITE?Replaces=1a2b3c%40192.0.2.4\
             %3Bto-tag%3Dpeer-tag%3Bfrom-tag%3Dour-tag>"
        );
        assert_eq!(refer.mode(), TransferMode::Attended);
    }

    #[test]
    fn the_replaces_names_the_dialog_by_all_three_of_its_identifiers() {
        // RFC 3891 §3.1, and what Asterisk matches on. A Replaces
        // with only the Call-ID is a dialog the server cannot find.
        assert_eq!(
            replaces_dialog_id_with_tags("1a2b3c", "192.0.2.4", "peer-tag", "our-tag"),
            "1a2b3c%40192.0.2.4%3Bto-tag%3Dpeer-tag%3Bfrom-tag%3Dour-tag"
        );
    }

    #[test]
    fn the_tags_are_where_a_server_looks_for_them() {
        // The value is parsed as a `Replaces` header once the escapes
        // are undone, so the separators have to be escaped — and the
        // to-tag is the peer's, the from-tag is ours.
        let id = replaces_dialog_id_with_tags("1a2b3c", "192.0.2.4", "peer-tag", "our-tag");
        let undone = id
            .replace("%40", "@")
            .replace("%3B", ";")
            .replace("%3D", "=");

        assert_eq!(
            undone, "1a2b3c@192.0.2.4;to-tag=peer-tag;from-tag=our-tag",
            "the Replaces does not read as a Replaces header once unescaped"
        );
    }

    #[test]
    fn a_feature_code_transfer_is_typed_as_the_phone_types_it() {
        // The C++ sends `*2<target>#` — the PBX's blind transfer code,
        // the target, and the terminator.
        assert_eq!(feature_code_sequence("*2", "6666"), "*26666#");
        assert_eq!(feature_code_sequence("*3", "6666"), "*36666#");

        // A target pasted with spaces is still the number without them.
        assert_eq!(feature_code_sequence("*2", " 555 88 "), "*255588#");
    }

    #[test]
    fn the_progress_code_says_what_is_happening() {
        // RFC 3515 §7.1.2.
        assert_eq!(progress_of(100), TransferProgress::Trying);
        assert_eq!(progress_of(180), TransferProgress::Ringing);
        assert_eq!(progress_of(183), TransferProgress::Ringing);
        assert_eq!(progress_of(200), TransferProgress::Answered);
        assert_eq!(progress_of(486), TransferProgress::Failed);
        assert_eq!(progress_of(503), TransferProgress::Failed);

        // A code this build does not know is still in flight, not a
        // failure.
        assert_eq!(progress_of(0), TransferProgress::Trying);
        assert_eq!(progress_of(202), TransferProgress::Trying);
    }
}
