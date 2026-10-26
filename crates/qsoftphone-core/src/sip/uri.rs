//! SIP URI helpers.
//!
//! Port of the C++ `sipUriUserPart()` (`src/core/sip/SipUri.cpp`).
//!
//! The C++ helper exists because pjsip hands URIs back in
//! display-name form: `"<sip:user@host>"` when there is no display
//! name, `"Name <sip:user@host>"` when there is. Without stripping
//! the angle brackets the history stored `<sip:user` — junk that
//! cannot be re-dialled and that the dialer would happily forward
//! as `sip:<sip:user@host`.

/// Extract the user-part of a SIP URI.
///
/// ```text
/// "sip:6666@193.178.169.164:5161"  -> "6666"
/// "sip:104@127.0.0.1"               -> "104"
/// "6666"                            -> "6666"
/// "sip:alice@host;transport=udp"   -> "alice"
/// "<sip:6666@host>"                 -> "6666"
/// ""                                -> ""
/// ```
///
/// The colon-port and `;param` stripping happen only on the host
/// side, so a digit is never accidentally chopped off the user-part.
pub fn sip_uri_user_part(uri: &str) -> String {
    let mut s = uri.trim();

    // 1. Strip a display-name wrapper. pjsip returns
    //    `"<sip:user@host>"` with no display name and
    //    `"Name <sip:user@host>"` with one.
    if let Some(lt) = s.find('<') {
        match s[lt + 1..].find('>') {
            Some(rel) => {
                let gt = lt + 1 + rel;
                s = &s[lt + 1..gt];
            }
            None => {
                // Unbalanced `<` — drop everything up to and
                // including it. Rare, but be defensive.
                s = &s[lt + 1..];
            }
        }
    } else if s.len() >= 2 && s.starts_with('>') && s.ends_with('>') {
        // Bare `>sip:user@host>` with a stray `>` and no `<`.
        s = &s[1..s.len() - 1];
    }

    // 2. Strip the URI scheme (`sip:` / `sips:` / `tel:`).
    if let Some(colon) = s.find(':') {
        if colon > 0 && colon <= 6 {
            let head = s[..colon].to_ascii_lowercase();
            if head == "sip" || head == "sips" || head == "tel" {
                s = &s[colon + 1..];
            }
        }
    }

    // 3. Drop URI parameters (`;transport=udp` and friends).
    if let Some(semi) = s.find(';') {
        s = &s[..semi];
    }

    // 4. The user-part ends at the first `@`. Without an `@` the whole
    //    string is the user-part — a bare number, or a host-less URI.
    if let Some(at) = s.find('@') {
        s = &s[..at];
    }

    // 5. Trim whitespace left over from upstream callers.
    s.trim().to_string()
}

/// Turn whatever the user typed into something routable.
///
/// Port of `CallController::normaliseSipTarget()`
/// (`src/core/call/CallController.cpp`).
///
/// A bare number is not routable — pjsip rejects `sip:555` with
/// `PJSIP_EINVALIDREQURI` — so the active account's registrar is used
/// as the host. Full SIP and `tel:` URIs from contacts and history
/// pass through verbatim.
///
/// `registrar` may be empty, in which case the number is still
/// wrapped in `sip:` so the caller can see what was attempted.
pub fn normalise_sip_target(target: &str, registrar: &str) -> String {
    let mut uri = target.trim().to_string();

    // Strip an accidental `<...>` wrapper.
    if let Some(rest) = uri.strip_prefix('<') {
        if let Some(gt) = rest.find('>') {
            uri = rest[..gt].to_string();
        }
    }

    let lower = uri.to_ascii_lowercase();
    if lower.starts_with("sip:") || lower.starts_with("sips:") || lower.starts_with("tel:") {
        return uri;
    }

    let number: String = uri.chars().filter(|c| !c.is_whitespace()).collect();

    if !registrar.trim().is_empty() {
        format!("sip:{number}@{}", registrar.trim())
    } else {
        format!("sip:{number}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_user_part_from_the_shapes_we_see() {
        assert_eq!(sip_uri_user_part("sip:6666@193.178.169.164:5161"), "6666");
        assert_eq!(sip_uri_user_part("sip:104@127.0.0.1"), "104");
        assert_eq!(sip_uri_user_part("6666"), "6666");
        assert_eq!(sip_uri_user_part("sip:alice@host;transport=udp"), "alice");
        assert_eq!(sip_uri_user_part(""), "");
    }

    #[test]
    fn strips_the_angle_bracket_wrapper_pjsip_adds() {
        // This is the regression: the history used to store `<sip:6666`.
        assert_eq!(sip_uri_user_part("<sip:6666@host>"), "6666");
        assert_eq!(sip_uri_user_part("Alice <sip:6666@host>"), "6666");
        assert_eq!(sip_uri_user_part("<sip:104@host>"), "104");
    }

    #[test]
    fn survives_unbalanced_brackets_and_stray_gt() {
        assert_eq!(sip_uri_user_part("<sip:6666@host"), "6666");
        // A stray `>` on both sides is stripped, and the rest is
        // parsed like any other URI.
        assert_eq!(sip_uri_user_part(">sip:6666@host>"), "6666");
    }

    #[test]
    fn handles_every_scheme_we_accept() {
        assert_eq!(sip_uri_user_part("sips:alice@host"), "alice");
        assert_eq!(sip_uri_user_part("tel:+15551234"), "+15551234");
        assert_eq!(sip_uri_user_part("SIP:Alice@Host"), "Alice");
    }

    #[test]
    fn does_not_chop_digits_off_the_user_part() {
        // A colon inside the user-part must not be treated as a port.
        assert_eq!(sip_uri_user_part("sip:555:123@host"), "555:123");
    }

    #[test]
    fn trims_surrounding_whitespace() {
        assert_eq!(sip_uri_user_part("  sip:6666@host  "), "6666");
    }

    #[test]
    fn normalises_a_bare_number_against_the_registrar() {
        assert_eq!(
            normalise_sip_target("6666", "sip:example.com"),
            "sip:6666@sip:example.com"
        );
        assert_eq!(
            normalise_sip_target(" 666 6 ", "example.com"),
            "sip:6666@example.com"
        );
    }

    #[test]
    fn normalise_passes_real_uris_through() {
        assert_eq!(
            normalise_sip_target("sip:6666@host", "reg"),
            "sip:6666@host"
        );
        assert_eq!(
            normalise_sip_target("sips:6666@host", "reg"),
            "sips:6666@host"
        );
        assert_eq!(
            normalise_sip_target("tel:+15551234", "reg"),
            "tel:+15551234"
        );
        assert_eq!(
            normalise_sip_target("<sip:6666@host>", "reg"),
            "sip:6666@host"
        );
    }

    #[test]
    fn normalise_without_a_registrar_still_produces_a_sip_uri() {
        assert_eq!(normalise_sip_target("6666", ""), "sip:6666");
    }
}
