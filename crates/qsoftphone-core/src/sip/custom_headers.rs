//! The extra headers an account puts on its outgoing calls.
//!
//! A PBX often wants something on the INVITE that the SIP headers do
//! not carry: a ticket number, a CRM id, the queue the call is for.
//! Every account has its own list of them — `X-var1=…`, `X-var2=…` —
//! and they go out on the call this account makes.
//!
//! Everything here is a pure function over strings, so the rules can be
//! tested without a socket (§6).

/// The values a header value may refer to.
///
/// A header value is a **template**: `$callerid` in it is replaced with
/// the value the call has at the moment it goes out. The template is
/// resolved per call, not per account — the same account dials different
/// numbers, and the value has to follow the call.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HeaderVars {
    /// Who the call is from.
    ///
    /// For an outgoing call that is **us**: the display name we put in
    /// the `From`, and the account's user part when there is no display
    /// name. This is the C++'s `CALLERID(name)` — the name of the party
    /// that is making the call.
    pub caller_id: String,
    /// The number that was dialled.
    ///
    /// The C++'s `CALLED`/`EXTEN`: what the user typed, before it was
    /// turned into a URI.
    pub called: String,
    /// The account's SIP user part.
    pub user: String,
    /// The account's label — "Office", "Home".
    pub account: String,
}

/// Is this a legal SIP header name?
///
/// RFC 3261 §20: a header name is a token — one or more characters from
/// the token set, and nothing else. A name with a space or a colon in it
/// would not be a header at all: it would split the message in two, and
/// a peer that parses it strictly will reject the whole request.
///
/// The token set is small enough to spell out: printable ASCII, minus
/// the separators. `()<>@,;:\"/[]?={} \t` are the separators, and the
/// control characters are not allowed either.
pub fn is_valid_name(name: &str) -> bool {
    if name.is_empty() {
        return false;
    }
    name.chars().all(|c| {
        // Printable ASCII, and not one of the separators.
        c.is_ascii_graphic()
            && !matches!(
                c,
                '(' | ')'
                    | '<'
                    | '>'
                    | '@'
                    | ','
                    | ';'
                    | ':'
                    | '\\'
                    | '"'
                    | '/'
                    | '['
                    | ']'
                    | '?'
                    | '='
                    | '{'
                    | '}'
                    | ' '
            )
    })
}

/// The value of one variable, if it is one we know.
fn lookup<'a>(name: &str, vars: &'a HeaderVars) -> Option<&'a str> {
    match name {
        "callerid" => Some(vars.caller_id.as_str()),
        "called" => Some(vars.called.as_str()),
        "user" => Some(vars.user.as_str()),
        "account" => Some(vars.account.as_str()),
        _ => None,
    }
}

/// Resolve the variables in a header value.
///
/// A `$name` is replaced with the value of that variable. A `$name` that
/// is **not** a variable we know is left exactly as it is — a value that
/// happens to contain a dollar sign must not silently lose it, and a
/// variable that does not exist must not turn into an empty header.
///
/// The name is read to the end of the word: `$callerid` is one name, and
/// a `$` that is followed by nothing usable is a literal `$`.
pub fn resolve(value: &str, vars: &HeaderVars) -> String {
    let mut out = String::with_capacity(value.len());
    let bytes = value.as_bytes();
    let mut i = 0;

    while i < bytes.len() {
        if bytes[i] != b'$' {
            // Copy one whole character — the value may be UTF-8.
            let start = i;
            i += 1;
            while i < bytes.len() && (bytes[i] & 0xC0) == 0x80 {
                i += 1;
            }
            out.push_str(&value[start..i]);
            continue;
        }

        // A `$` starts a name: letters, digits and `_`, and nothing else.
        let start = i + 1;
        let mut end = start;
        while end < bytes.len() && (bytes[end].is_ascii_alphanumeric() || bytes[end] == b'_') {
            end += 1;
        }
        if end == start {
            // No name at all — a literal `$`.
            out.push('$');
            i = start;
            continue;
        }

        let name = &value[start..end];
        match lookup(name, vars) {
            Some(replacement) => out.push_str(replacement),
            None => {
                // Not one of ours: the whole thing stays as it was.
                out.push_str(&value[i..end]);
            }
        }
        i = end;
    }

    out
}

/// The headers that go out on a call.
///
/// The list is the account's own, in the order the user put them in.
/// The values are resolved against the call, and a header whose name is
/// not a legal header name is **dropped** — a message with a broken
/// header in it is a message the peer may refuse, and one broken header
/// is worth less than the call.
///
/// A header with an empty name is dropped for the same reason: the user
/// typed a value and left the name blank, or cleared the name.
pub fn build(headers: &[(String, String)], vars: &HeaderVars) -> Vec<(String, String)> {
    let mut out = Vec::with_capacity(headers.len());
    for (name, value) in headers {
        let name = name.trim();
        if !is_valid_name(name) {
            continue;
        }
        out.push((name.to_string(), resolve(value, vars)));
    }
    out
}

/// The header names the account uses, for the log.
///
/// Only the names: a value can carry a ticket number or a name, and the
/// log is not the place for it.
pub fn names(headers: &[(String, String)]) -> Vec<String> {
    headers
        .iter()
        .map(|(name, _)| name.trim().to_string())
        .filter(|name| is_valid_name(name))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vars() -> HeaderVars {
        HeaderVars {
            caller_id: "Alice".into(),
            called: "105".into(),
            user: "104".into(),
            account: "Office".into(),
        }
    }

    #[test]
    fn a_header_name_is_a_token() {
        // RFC 3261 §20: a token is printable ASCII without the
        // separators.
        assert!(is_valid_name("X-var1"));
        assert!(is_valid_name("X-Var-1"));
        assert!(is_valid_name("X_Ticket"));
        assert!(is_valid_name("X-CALLER-ID"));

        assert!(!is_valid_name(""));
        assert!(!is_valid_name("X var1"), "a space splits the header");
        assert!(!is_valid_name("X-var1:"), "a colon ends the header");
        assert!(!is_valid_name("X:var1"));
        assert!(!is_valid_name("X-var1:value"));
        assert!(!is_valid_name("X (comment)"));
        assert!(!is_valid_name("X-var1\r\nInjected: yes"));
        assert!(!is_valid_name("X-var1\t"));
        assert!(!is_valid_name("Ünicode"), "a name is ASCII");
    }

    #[test]
    fn a_variable_is_replaced_with_its_value() {
        assert_eq!(resolve("$callerid", &vars()), "Alice");
        assert_eq!(resolve("$called", &vars()), "105");
        assert_eq!(resolve("$user", &vars()), "104");
        assert_eq!(resolve("$account", &vars()), "Office");
    }

    #[test]
    fn a_variable_inside_a_value_is_replaced() {
        assert_eq!(
            resolve("call for $called from $callerid", &vars()),
            "call for 105 from Alice"
        );
        assert_eq!(resolve("$callerid/$called", &vars()), "Alice/105");
    }

    #[test]
    fn a_value_without_a_variable_is_left_alone() {
        assert_eq!(resolve("T-4711", &vars()), "T-4711");
        assert_eq!(resolve("", &vars()), "");
    }

    #[test]
    fn a_variable_that_does_not_exist_stays_as_it_was() {
        // A value that happens to contain a dollar sign must not lose
        // it, and a name we do not know must not turn into nothing.
        assert_eq!(resolve("$nosuchthing", &vars()), "$nosuchthing");
        assert_eq!(resolve("cost: $100", &vars()), "cost: $100");
        assert_eq!(resolve("$", &vars()), "$");
        assert_eq!(resolve("100$", &vars()), "100$");
    }

    #[test]
    fn the_whole_value_can_be_a_variable() {
        assert_eq!(
            resolve(
                "$called",
                &HeaderVars {
                    called: "999".into(),
                    ..vars()
                }
            ),
            "999"
        );
    }

    #[test]
    fn a_non_ascii_value_survives_the_resolving() {
        assert_eq!(resolve("Кому: $called", &vars()), "Кому: 105");
    }

    #[test]
    fn the_headers_that_go_out_are_the_accounts_own() {
        let headers = vec![
            ("X-var1".to_string(), "one".to_string()),
            ("X-var2".to_string(), "two".to_string()),
        ];
        let built = build(&headers, &vars());
        assert_eq!(built.len(), 2);
        assert_eq!(built[0], ("X-var1".to_string(), "one".to_string()));
        assert_eq!(built[1], ("X-var2".to_string(), "two".to_string()));
    }

    #[test]
    fn the_order_the_user_put_them_in_is_the_order_they_go_out_in() {
        let headers = vec![
            ("X-Zulu".to_string(), "z".to_string()),
            ("X-Alpha".to_string(), "a".to_string()),
        ];
        let built = build(&headers, &vars());
        assert_eq!(built[0].0, "X-Zulu");
        assert_eq!(built[1].0, "X-Alpha");
    }

    #[test]
    fn a_header_with_a_broken_name_does_not_go_out() {
        // One broken header is worth less than the call: a message with
        // a broken header in it is a message the peer may refuse.
        let headers = vec![
            ("X-good".to_string(), "one".to_string()),
            ("X bad".to_string(), "two".to_string()),
            ("".to_string(), "three".to_string()),
        ];
        let built = build(&headers, &vars());
        assert_eq!(built.len(), 1);
        assert_eq!(built[0].0, "X-good");
    }

    #[test]
    fn the_values_are_resolved_on_the_way_out() {
        let headers = vec![("X-Caller".to_string(), "$callerid".to_string())];
        let built = build(&headers, &vars());
        assert_eq!(built[0].1, "Alice");
    }

    #[test]
    fn the_log_names_the_headers_and_nothing_else() {
        let headers = vec![
            ("X-Ticket".to_string(), "T-4711".to_string()),
            ("X bad".to_string(), "x".to_string()),
        ];
        assert_eq!(names(&headers), vec!["X-Ticket".to_string()]);
    }
}
