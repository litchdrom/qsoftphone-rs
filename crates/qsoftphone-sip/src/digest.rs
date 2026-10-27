//! Digest authentication interop.
//!
//! rsipstack implements RFC 7616 digest authentication with MD5,
//! SHA-256 and SHA-512, and it picks the algorithm from the
//! challenge's `algorithm` parameter. There is one gap: the parser
//! accepts `SHA256` but **not** the hyphenated `SHA-256` that RFC 7616
//! §3.4 specifies — and the hyphenated spelling is what strict
//! servers actually send.
//!
//! `Algorithm::try_from` rejects it outright, which makes the *whole
//! challenge* fail to parse. A registrar that answers with
//! `algorithm=SHA-256` therefore cannot be registered against at all.
//!
//! [`DigestInterop`] closes that gap from the outside, as an
//! endpoint-level [`MessageInspector`]:
//!
//! * on the way **in**, the challenge's algorithm token is rewritten
//!   into the spelling rsipstack's parser accepts;
//! * on the way **out**, the `Authorization` header gets the server's
//!   own token back, because RFC 7616 §3.4.1 has the client echo the
//!   algorithm the server named.
//!
//! The digest itself is still computed by rsipstack — only the label
//! on the packet is rewritten. The rewrite is idempotent, which
//! matters because rsipstack applies `after_received` both when a
//! message arrives and again when it is handed to the transaction.

use std::collections::HashMap;
use std::ops::Range;
use std::sync::Mutex;

use rsipstack::sip::{Header, Request, Response, SipMessage, StatusCode};
use rsipstack::transaction::endpoint::MessageInspector;
use rsipstack::transport::SipAddr;

/// The canonical spelling of each digest algorithm that rsipstack's
/// parser accepts.
///
/// RFC 7616 §3.4 names them `MD5`, `SHA-256`, `SHA-512` (and the
/// `-sess` variants). rsipstack's `Algorithm::try_from` only knows
/// `SHA256` and `SHA512`.
fn canonical_algorithm(token: &str) -> Option<&'static str> {
    match token.trim().to_ascii_lowercase().as_str() {
        "md5" => Some("MD5"),
        "md5-sess" => Some("MD5-sess"),
        "sha-256" | "sha256" => Some("SHA256"),
        "sha-256-sess" | "sha256-sess" => Some("SHA256-sess"),
        "sha-512" | "sha512" => Some("SHA512"),
        "sha-512-sess" | "sha512-sess" => Some("SHA512-sess"),
        _ => None,
    }
}

/// Find a parameter in a digest header value.
///
/// Returns the parameter's value and the byte range it occupies in
/// `raw` — the range of the value itself, without any surrounding
/// quotes, so a substitution keeps the original quoting style.
fn find_param(raw: &str, want: &str) -> Option<(String, Range<usize>)> {
    let bytes = raw.as_bytes();
    let mut i = 0usize;

    while i < bytes.len() {
        // Skip separators.
        while i < bytes.len()
            && (bytes[i].is_ascii_whitespace() || bytes[i] == b',' || bytes[i] == b';')
        {
            i += 1;
        }

        let name_start = i;
        while i < bytes.len()
            && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'-' || bytes[i] == b'_')
        {
            i += 1;
        }
        if i == name_start {
            // Not a parameter name — step over one character.
            i += 1;
            while i < bytes.len() && !raw.is_char_boundary(i) {
                i += 1;
            }
            continue;
        }
        let name = &raw[name_start..i];

        // Skip whitespace before the '='.
        let mut j = i;
        while j < bytes.len() && bytes[j].is_ascii_whitespace() {
            j += 1;
        }
        if j >= bytes.len() || bytes[j] != b'=' {
            continue;
        }
        j += 1;
        while j < bytes.len() && bytes[j].is_ascii_whitespace() {
            j += 1;
        }

        let (value, range, value_end) = if j < bytes.len() && bytes[j] == b'"' {
            let start = j + 1;
            let mut k = start;
            while k < bytes.len() && bytes[k] != b'"' {
                k += 1;
            }
            (raw[start..k].to_string(), start..k, k)
        } else {
            let start = j;
            let mut k = start;
            while k < bytes.len()
                && bytes[k] != b','
                && bytes[k] != b';'
                && !bytes[k].is_ascii_whitespace()
            {
                k += 1;
            }
            (raw[start..k].to_string(), start..k, k)
        };

        if name.eq_ignore_ascii_case(want) {
            return Some((value, range));
        }
        i = value_end.max(i);
    }
    None
}

/// Rewrite a digest header's `algorithm` parameter into the spelling
/// rsipstack's parser accepts.
///
/// Returns the rewritten header value and the token the peer
/// originally used, so it can be echoed back in the `Authorization`
/// header. The rewrite is idempotent.
pub fn rewrite_algorithm(raw: &str) -> (String, Option<String>) {
    let Some((value, range)) = find_param(raw, "algorithm") else {
        return (raw.to_string(), None);
    };
    let Some(canonical) = canonical_algorithm(&value) else {
        return (raw.to_string(), None);
    };

    let mut out = String::with_capacity(raw.len());
    out.push_str(&raw[..range.start]);
    out.push_str(canonical);
    out.push_str(&raw[range.end..]);
    (out, Some(value))
}

/// Put a digest header's `algorithm` parameter back to a specific
/// token.
///
/// Used to echo the server's own algorithm spelling back in the
/// `Authorization` header, as RFC 7616 §3.4.1 requires.
pub fn restore_algorithm(raw: &str, token: &str) -> String {
    let Some((_, range)) = find_param(raw, "algorithm") else {
        return raw.to_string();
    };
    let mut out = String::with_capacity(raw.len());
    out.push_str(&raw[..range.start]);
    out.push_str(token);
    out.push_str(&raw[range.end..]);
    out
}

/// Endpoint-level digest-authentication interop.
///
/// Attach one of these to the endpoint with
/// [`EndpointBuilder::with_inspector`](rsipstack::transaction::EndpointBuilder::with_inspector).
#[derive(Debug, Default)]
pub struct DigestInterop {
    /// The algorithm token each server used, keyed by the nonce it
    /// came with.
    ///
    /// The nonce is the only thing that ties a challenge to its
    /// answer, and it is echoed verbatim in the `Authorization`
    /// header, so it is a reliable key.
    tokens: Mutex<HashMap<String, String>>,
}

impl DigestInterop {
    pub fn new() -> Self {
        Self::default()
    }

    fn remember(&self, nonce: &str, token: &str) {
        if let Ok(mut tokens) = self.tokens.lock() {
            tokens.insert(nonce.to_string(), token.to_string());
        }
    }

    fn token_for(&self, nonce: &str) -> Option<String> {
        self.tokens.lock().ok()?.get(nonce).cloned()
    }

    /// Rewrite a challenge so rsipstack can parse it, and remember the
    /// server's algorithm token.
    fn rewrite_challenge(&self, response: &mut Response) {
        if !matches!(
            response.status_code,
            StatusCode::Unauthorized | StatusCode::ProxyAuthenticationRequired
        ) {
            return;
        }

        for header in response.headers.iter_mut() {
            let raw = match header {
                Header::WwwAuthenticate(h) => h.value().to_string(),
                Header::ProxyAuthenticate(h) => h.value().to_string(),
                _ => continue,
            };

            let (rewritten, token) = rewrite_algorithm(&raw);
            if let Some(token) = token
                && let Some((nonce, _)) = find_param(&raw, "nonce")
            {
                self.remember(&nonce, &token);
            }
            if rewritten == raw {
                continue;
            }
            match header {
                Header::WwwAuthenticate(h) => h.replace(rewritten),
                Header::ProxyAuthenticate(h) => h.replace(rewritten),
                _ => {}
            }
        }
    }

    /// Put the server's algorithm token back into an authorisation.
    fn restore_token(&self, request: &mut Request) {
        for header in request.headers.iter_mut() {
            let raw = match header {
                Header::Authorization(h) => h.value().to_string(),
                Header::ProxyAuthorization(h) => h.value().to_string(),
                _ => continue,
            };

            let Some((nonce, _)) = find_param(&raw, "nonce") else {
                continue;
            };
            let Some(token) = self.token_for(&nonce) else {
                continue;
            };
            let restored = restore_algorithm(&raw, &token);
            if restored == raw {
                continue;
            }
            match header {
                Header::Authorization(h) => h.replace(restored),
                Header::ProxyAuthorization(h) => h.replace(restored),
                _ => {}
            }
        }
    }
}

impl MessageInspector for DigestInterop {
    fn after_received(&self, msg: SipMessage, _from: Option<&SipAddr>) -> SipMessage {
        match msg {
            SipMessage::Response(mut response) => {
                self.rewrite_challenge(&mut response);
                SipMessage::Response(response)
            }
            other => other,
        }
    }

    fn before_send(&self, msg: SipMessage, _dest: Option<&SipAddr>) -> SipMessage {
        match msg {
            SipMessage::Request(mut request) => {
                self.restore_token(&mut request);
                SipMessage::Request(request)
            }
            other => other,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rsipstack::dialog::authenticate::{compute_digest, verify_digest};
    use rsipstack::sip::headers::auth::Algorithm;
    use rsipstack::sip::headers::typed::Authorization as TypedAuthorization;
    use rsipstack::sip::headers::typed::WwwAuthenticate as TypedWwwAuthenticate;
    use rsipstack::sip::{Method, Version};

    fn sha256_hex(input: &str) -> String {
        use sha2::{Digest, Sha256};
        let mut hasher = Sha256::new();
        hasher.update(input.as_bytes());
        hasher
            .finalize()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect()
    }

    fn md5_hex(input: &str) -> String {
        use md5::Md5;
        use sha2::Digest as _;
        let mut hasher = Md5::new();
        hasher.update(input.as_bytes());
        hasher
            .finalize()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect()
    }

    #[test]
    fn the_hyphenated_sha256_token_is_rewritten() {
        let raw = r#"Digest realm="example.com", nonce="abc123", algorithm=SHA-256, qop="auth""#;
        let (rewritten, token) = rewrite_algorithm(raw);
        assert_eq!(token.as_deref(), Some("SHA-256"));
        assert!(rewritten.contains("algorithm=SHA256"));
        assert!(rewritten.contains(r#"realm="example.com""#));
        assert!(rewritten.contains(r#"nonce="abc123""#));
    }

    #[test]
    fn the_rewrite_is_idempotent() {
        let raw = r#"Digest realm="r", nonce="n", algorithm=SHA-256"#;
        let (once, _) = rewrite_algorithm(raw);
        let (twice, token) = rewrite_algorithm(&once);
        assert_eq!(once, twice);
        assert_eq!(token.as_deref(), Some("SHA256"));
    }

    #[test]
    fn quoted_and_unquoted_tokens_are_both_handled() {
        let (quoted, token) = rewrite_algorithm(r#"Digest realm="r", algorithm="SHA-256""#);
        assert_eq!(token.as_deref(), Some("SHA-256"));
        assert!(quoted.contains(r#"algorithm="SHA256""#));

        let (bare, token) = rewrite_algorithm("Digest realm=\"r\", algorithm=MD5");
        assert_eq!(token.as_deref(), Some("MD5"));
        assert!(bare.contains("algorithm=MD5"));
    }

    #[test]
    fn the_parameter_name_is_matched_case_insensitively() {
        let (rewritten, token) = rewrite_algorithm(r#"Digest realm="r", ALGORITHM=SHA-512"#);
        assert_eq!(token.as_deref(), Some("SHA-512"));
        assert!(rewritten.contains("ALGORITHM=SHA512"));
    }

    #[test]
    fn an_unknown_algorithm_is_left_alone() {
        let raw = r#"Digest realm="r", nonce="n", algorithm=SHA-3-256"#;
        let (rewritten, token) = rewrite_algorithm(raw);
        assert_eq!(rewritten, raw);
        assert_eq!(token, None);
    }

    #[test]
    fn a_challenge_without_an_algorithm_is_left_alone() {
        let raw = r#"Digest realm="r", nonce="n""#;
        let (rewritten, token) = rewrite_algorithm(raw);
        assert_eq!(rewritten, raw);
        assert_eq!(token, None);
    }

    #[test]
    fn rsipstack_cannot_parse_the_hyphenated_spelling() {
        // This is the gap being closed. If a future rsipstack fixes
        // it, this test fails and the rewrite can be dropped.
        let raw = r#"Digest realm="example.com", nonce="abc123", algorithm=SHA-256, qop="auth""#;
        assert!(TypedWwwAuthenticate::parse(raw).is_err());
    }

    #[test]
    fn the_rewritten_challenge_parses() {
        let raw = r#"Digest realm="example.com", nonce="abc123", algorithm=SHA-256, qop="auth""#;
        let (rewritten, _) = rewrite_algorithm(raw);
        let parsed = TypedWwwAuthenticate::parse(&rewritten).unwrap();
        assert_eq!(parsed.algorithm, Some(Algorithm::Sha256));
        assert_eq!(parsed.realm, "example.com");
        assert_eq!(parsed.nonce, "abc123");
    }

    #[test]
    fn the_algorithm_restored_is_the_servers_own_token() {
        let raw = r#"Digest username="104", realm="r", nonce="abc123", uri="sip:host", response="deadbeef", algorithm=SHA256"#;
        let restored = restore_algorithm(raw, "SHA-256");
        assert!(restored.contains("algorithm=SHA-256"));
        assert!(!restored.contains("algorithm=SHA256,"));
    }

    #[test]
    fn the_sha256_digest_is_a_real_sha256() {
        // Computed independently of rsipstack, so a mismatch between
        // the algorithm token and the hash function would show up.
        let username = "104";
        let password = "secret";
        let realm = "example.com";
        let nonce = "abc123";
        let uri = "sip:example.com";

        let from_rsipstack = compute_digest(
            username,
            password,
            realm,
            nonce,
            &Method::Register,
            uri,
            Algorithm::Sha256,
            None,
        );

        let ha1 = sha256_hex(&format!("{username}:{realm}:{password}"));
        let ha2 = sha256_hex(&format!("{}:{uri}", Method::Register));
        let expected = sha256_hex(&format!("{ha1}:{nonce}:{ha2}"));

        assert_eq!(from_rsipstack, expected);
    }

    #[test]
    fn the_md5_digest_is_a_real_md5() {
        let username = "104";
        let password = "secret";
        let realm = "example.com";
        let nonce = "abc123";
        let uri = "sip:example.com";

        let from_rsipstack = compute_digest(
            username,
            password,
            realm,
            nonce,
            &Method::Register,
            uri,
            Algorithm::Md5,
            None,
        );

        let ha1 = md5_hex(&format!("{username}:{realm}:{password}"));
        let ha2 = md5_hex(&format!("{}:{uri}", Method::Register));
        let expected = md5_hex(&format!("{ha1}:{nonce}:{ha2}"));

        assert_eq!(from_rsipstack, expected);
    }

    #[test]
    fn the_inspector_rewrites_an_incoming_challenge() {
        let interop = DigestInterop::new();
        let challenge = r#"Digest realm="r", nonce="abc123", algorithm=SHA-256, qop="auth""#;
        let response = Response {
            status_code: StatusCode::Unauthorized,
            wire_reason: None,
            version: Version::V2,
            headers: rsipstack::sip::Headers(vec![Header::WwwAuthenticate(
                rsipstack::sip::headers::WwwAuthenticate::new(challenge),
            )]),
            body: Vec::new(),
        };

        let out = interop.after_received(SipMessage::Response(response), None);
        let SipMessage::Response(response) = out else {
            panic!("expected a response");
        };
        let header = response
            .headers
            .iter()
            .find_map(|h| match h {
                Header::WwwAuthenticate(h) => Some(h.value().to_string()),
                _ => None,
            })
            .unwrap();
        assert!(header.contains("algorithm=SHA256"));
        assert!(!header.contains("SHA-256"));
    }

    #[test]
    fn the_inspector_restores_the_token_on_the_way_out() {
        let interop = DigestInterop::new();
        let challenge = r#"Digest realm="r", nonce="abc123", algorithm=SHA-256, qop="auth""#;
        let response = Response {
            status_code: StatusCode::Unauthorized,
            wire_reason: None,
            version: Version::V2,
            headers: rsipstack::sip::Headers(vec![Header::WwwAuthenticate(
                rsipstack::sip::headers::WwwAuthenticate::new(challenge),
            )]),
            body: Vec::new(),
        };
        interop.after_received(SipMessage::Response(response), None);

        let authorization = r#"Digest username="104", realm="r", nonce="abc123", uri="sip:host", response="deadbeef", algorithm=SHA256, qop=auth, nc=00000001, cnonce="xyz""#;
        let request = Request {
            method: Method::Register,
            uri: rsipstack::sip::Uri::try_from("sip:host").unwrap(),
            version: Version::V2,
            headers: rsipstack::sip::Headers(vec![Header::Authorization(
                rsipstack::sip::headers::Authorization::new(authorization),
            )]),
            body: Vec::new(),
        };

        let out = interop.before_send(SipMessage::Request(request), None);
        let SipMessage::Request(request) = out else {
            panic!("expected a request");
        };
        let header = request
            .headers
            .iter()
            .find_map(|h| match h {
                Header::Authorization(h) => Some(h.value().to_string()),
                _ => None,
            })
            .unwrap();
        assert!(header.contains("algorithm=SHA-256"));
    }

    #[test]
    fn an_authorization_without_a_known_nonce_is_left_alone() {
        let interop = DigestInterop::new();
        let authorization = r#"Digest username="104", realm="r", nonce="unknown", uri="sip:host", response="deadbeef", algorithm=SHA256"#;
        let request = Request {
            method: Method::Register,
            uri: rsipstack::sip::Uri::try_from("sip:host").unwrap(),
            version: Version::V2,
            headers: rsipstack::sip::Headers(vec![Header::Authorization(
                rsipstack::sip::headers::Authorization::new(authorization),
            )]),
            body: Vec::new(),
        };
        let out = interop.before_send(SipMessage::Request(request), None);
        let SipMessage::Request(request) = out else {
            panic!("expected a request");
        };
        let header = request
            .headers
            .iter()
            .find_map(|h| match h {
                Header::Authorization(h) => Some(h.value().to_string()),
                _ => None,
            })
            .unwrap();
        assert!(header.contains("algorithm=SHA256"));
    }

    #[test]
    fn the_authorisation_echoes_the_servers_token_and_carries_a_real_sha256() {
        let challenge = r#"Digest realm="example.com", nonce="abc123", algorithm=SHA-256"#;
        let (rewritten, _) = rewrite_algorithm(challenge);
        let parsed = TypedWwwAuthenticate::parse(&rewritten).unwrap();

        let uri = "sip:example.com";
        let response = compute_digest(
            "104",
            "secret",
            &parsed.realm,
            &parsed.nonce,
            &Method::Register,
            uri,
            parsed.algorithm.unwrap(),
            None,
        );

        // The client echoes the server's own algorithm token — RFC
        // 7616 §3.4.1 — so the wire carries `SHA-256`, not `SHA256`.
        let authorization = format!(
            r#"Digest username="104", realm="{}", nonce="{}", uri="{uri}", response="{response}", algorithm=SHA-256"#,
            parsed.realm, parsed.nonce
        );
        assert!(authorization.contains("algorithm=SHA-256"));

        // And the digest really is the SHA-256 of the RFC 7616
        // strings, computed here independently of rsipstack.
        let ha1 = sha256_hex("104:example.com:secret");
        let ha2 = sha256_hex(&format!("REGISTER:{uri}"));
        let expected = sha256_hex(&format!("{ha1}:abc123:{ha2}"));
        assert_eq!(response, expected);

        // A verifier built on rsipstack's typed parser has to
        // normalise the token first — it cannot read the RFC
        // spelling. That is the same rewrite the client applies to
        // the challenge.
        let (normalised, _) = rewrite_algorithm(&authorization);
        let typed = TypedAuthorization::parse(&normalised).unwrap();
        assert!(verify_digest(
            &typed,
            "secret",
            &Method::Register,
            &normalised
        ));
    }
}
