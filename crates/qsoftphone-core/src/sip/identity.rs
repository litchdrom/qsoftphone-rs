//! §5.1 and §5.2 — working out who is on the other end of the call.
//!
//! Both resolutions happen at event-construction time from data the
//! core already has. No plugin round-trip is involved, so the answers
//! are available the instant the event fires — which is what lets a
//! call be answered without waiting on a CRM.

use crate::model::account::{AccountProfile, HeaderMap};
use crate::sip::uri::sip_uri_user_part;
use crate::storage::contacts::Contacts;

/// §5.1 — the number the remote party actually dialled.
///
/// Precedence:
///
/// 1. the account's configured dialled-number header
/// 2. the captured Request-URI — the DID/DND case, where the PBX
///    forwards `sip:6666@host` to our AOR `sip:104@host`
/// 3. the To-URI
///
/// The local AOR must never appear here. Resolving the dialled number
/// to the local AOR was the bug the brief calls out: every call then
/// showed the local extension as the number that had been dialled.
/// The Request-URI capture is what makes the DID case correct, so it
/// has to be passed in — when it is missing, step 3 falls back to the
/// To-URI, which in a forwarded call *is* the AOR.
pub fn resolve_dialed_number(
    profile: &AccountProfile,
    captured: &HeaderMap,
    request_uri: Option<&str>,
    callee_uri: &str,
) -> String {
    // 1. The account's configured header wins over everything.
    let raw = profile.resolve_dialed_number(captured);
    if !raw.trim().is_empty() {
        // A header may carry a bare number or a whole URI; the
        // user-part is what we want either way.
        let number = sip_uri_user_part(&raw);
        if !number.is_empty() {
            return number;
        }
    }

    // 2. The captured Request-URI. This is the DID/DND path.
    if let Some(uri) = request_uri {
        let number = sip_uri_user_part(uri);
        if !number.is_empty() {
            return number;
        }
    }

    // 3. The To-URI.
    sip_uri_user_part(callee_uri)
}

/// §5.2 — the caller's display name.
///
/// Precedence:
///
/// 1. the account's configured caller-id header (`X-Caller-Name`,
///    `From`, `Remote-Party-ID`, ...)
/// 2. the local contact lookup
/// 3. the user-part of the caller URI
///
/// Step 1 is careful about *what kind* of header it is. A name-addr
/// (`"Alice" <sip:alice@host>`) carries the display name outright. A
/// bare URI (`Remote-Party-ID: sip:alice@host`) identifies the
/// number, not the name — so it is fed to the contact lookup instead
/// of being shown as a name.
pub fn resolve_caller_display_name(
    profile: &AccountProfile,
    captured: &HeaderMap,
    caller_uri: &str,
    contacts: Option<&Contacts<'_>>,
) -> String {
    let raw = profile.resolve_caller_id(captured);
    if !raw.trim().is_empty() {
        let raw = raw.trim();

        if let Some(name) = display_name_from_header(raw) {
            return name;
        }

        // A URI-shaped header: use it for the contact lookup.
        let uri_number = sip_uri_user_part(raw);
        if !uri_number.is_empty() {
            if let Some(name) = lookup_contact(contacts, &uri_number) {
                return name;
            }
        }
    }

    // 2. Local contact lookup on the caller's number.
    let number = sip_uri_user_part(caller_uri);
    if let Some(name) = lookup_contact(contacts, &number) {
        return name;
    }

    // 3. The user-part of the caller URI.
    number
}

/// Extract a human-readable display name from a SIP header value.
///
/// ```text
/// "Alice"                -> Some("Alice")
/// "Alice" <sip:a@h>      -> Some("Alice")
/// Alice <sip:a@h>        -> Some("Alice")
/// <sip:a@h>              -> None   (no display name)
/// sip:a@h                -> None   (a URI is not a name)
/// ""                     -> None
/// ```
pub fn display_name_from_header(value: &str) -> Option<String> {
    let v = value.trim();
    if v.is_empty() {
        return None;
    }

    // Quoted-string form: `"Alice" <sip:a@h>`.
    if let Some(rest) = v.strip_prefix('"') {
        if let Some(end) = rest.find('"') {
            let name = &rest[..end];
            if !name.is_empty() {
                return Some(name.to_string());
            }
        }
        return None;
    }

    // name-addr without quotes: `Alice <sip:a@h>`.
    if let Some(lt) = v.find('<') {
        let head = v[..lt].trim();
        if head.is_empty() {
            return None;
        }
        return Some(head.to_string());
    }

    // A bare URI is an identity, not a display name.
    if v.contains(':') && v.contains('@') {
        return None;
    }

    Some(v.to_string())
}

fn lookup_contact(contacts: Option<&Contacts<'_>>, number: &str) -> Option<String> {
    if number.is_empty() {
        return None;
    }
    let contacts = contacts?;
    match contacts.best_match_for_phone(number) {
        Ok(Some(name)) if !name.is_empty() => Some(name),
        Ok(_) => None,
        Err(e) => {
            // A contact-lookup failure must never take the call down.
            tracing::warn!(number, error = %e, "contact lookup failed");
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn headers(pairs: &[(&str, &str)]) -> HeaderMap {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect()
    }

    #[test]
    fn dialled_number_prefers_the_account_header() {
        let profile = AccountProfile {
            dialed_number_header: "X-Dialed-Number".into(),
            ..Default::default()
        };
        let captured = headers(&[("X-Dialed-Number", "7777")]);
        assert_eq!(
            resolve_dialed_number(&profile, &captured, Some("sip:6666@host"), "sip:104@host"),
            "7777"
        );
    }

    #[test]
    fn dialled_number_uses_the_request_uri_not_the_local_aor() {
        // The DID/DND case: the PBX forwards sip:6666@host to our AOR
        // sip:104@host. The dialled number is 6666, never 104.
        let profile = AccountProfile::default();
        let captured = HeaderMap::new();
        let number = resolve_dialed_number(
            &profile,
            &captured,
            Some("sip:6666@193.178.169.164"),
            "sip:104@193.178.169.164",
        );
        assert_eq!(number, "6666");
        assert_ne!(number, "104");
    }

    #[test]
    fn dialled_number_falls_back_to_the_to_uri() {
        let profile = AccountProfile::default();
        let number = resolve_dialed_number(
            &profile,
            &HeaderMap::new(),
            None,
            "sip:6666@193.178.169.164",
        );
        assert_eq!(number, "6666");
    }

    #[test]
    fn dialled_number_strips_a_uri_from_the_header_value() {
        let profile = AccountProfile {
            dialed_number_header: "X-Dialed-Number".into(),
            ..Default::default()
        };
        let captured = headers(&[("X-Dialed-Number", "sip:7777@host")]);
        assert_eq!(
            resolve_dialed_number(&profile, &captured, None, "sip:104@host"),
            "7777"
        );
    }

    #[test]
    fn dialled_number_header_lookup_is_case_insensitive() {
        let profile = AccountProfile {
            dialed_number_header: "x-dialed-number".into(),
            ..Default::default()
        };
        let captured = headers(&[("X-Dialed-Number", "7777")]);
        assert_eq!(
            resolve_dialed_number(&profile, &captured, None, "sip:104@host"),
            "7777"
        );
    }

    #[test]
    fn caller_name_prefers_the_configured_header() {
        let profile = AccountProfile {
            caller_id_header: "X-Caller-Name".into(),
            ..Default::default()
        };
        let captured = headers(&[("X-Caller-Name", "Alice Smith")]);
        assert_eq!(
            resolve_caller_display_name(&profile, &captured, "sip:6666@host", None),
            "Alice Smith"
        );
    }

    #[test]
    fn caller_name_reads_a_name_addr_from_the_from_header() {
        let profile = AccountProfile {
            caller_id_header: "From".into(),
            ..Default::default()
        };
        let captured = headers(&[("From", "\"Alice Smith\" <sip:6666@host>")]);
        assert_eq!(
            resolve_caller_display_name(&profile, &captured, "sip:6666@host", None),
            "Alice Smith"
        );
    }

    #[test]
    fn caller_name_falls_back_to_the_uri_user_part() {
        let profile = AccountProfile::default();
        assert_eq!(
            resolve_caller_display_name(&profile, &HeaderMap::new(), "sip:6666@host", None),
            "6666"
        );
    }

    #[test]
    fn display_name_extraction_handles_every_shape() {
        assert_eq!(display_name_from_header("Alice"), Some("Alice".into()));
        assert_eq!(
            display_name_from_header("\"Alice\" <sip:a@h>"),
            Some("Alice".into())
        );
        assert_eq!(
            display_name_from_header("Alice <sip:a@h>"),
            Some("Alice".into())
        );
        assert_eq!(display_name_from_header("<sip:a@h>"), None);
        assert_eq!(display_name_from_header("sip:a@h"), None);
        assert_eq!(display_name_from_header(""), None);
    }
}
