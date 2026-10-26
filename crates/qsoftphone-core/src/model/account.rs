//! Accounts: the stored configuration and the per-account policy the
//! call core needs.
//!
//! Port of the C++ `AccountProfile`
//! (`src/core/call/AccountProfile.h`). The SIP layer knows about
//! transports, dialogs and codecs. It does *not* know which headers
//! the user considers meaningful — that is per-account policy, so it
//! lives here and is handed to the core as a plain value.

use serde::{Deserialize, Serialize};

/// A captured SIP header set: header name to value.
///
/// A `BTreeMap` rather than a `HashMap` so that iteration order is
/// deterministic — the set is serialised into events and webhooks,
/// and stable ordering keeps those payloads diffable.
pub type HeaderMap = std::collections::BTreeMap<String, String>;

/// A SIP account as it is stored in SQLite.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Account {
    /// UUID; assigned on creation.
    pub id: String,
    /// Human label, e.g. "Office".
    pub label: String,
    /// The registrar, e.g. `sip:example.com` or `example.com:5060`.
    pub registrar: String,
    /// SIP user part, e.g. "104".
    pub user: String,
    /// SIP password. Never serialised — see [`Account::public_view`].
    #[serde(skip_serializing)]
    pub password: String,
    /// `udp`, `tcp` or `tls`.
    pub transport: String,
    /// Optional SIP domain, when it differs from the registrar.
    pub domain: String,
    /// Optional outbound proxy.
    pub outbound_proxy: String,
    /// Header that carries the caller's display name.
    pub caller_id_header: String,
    /// Header that carries the number the remote party dialled.
    pub dialed_number_header: String,
    /// Allowlist of extra headers that may leave the core.
    pub capture_headers: Vec<String>,
    /// Which CRM plugin handles this account. Empty = local contacts.
    pub crm_preset_id: String,
    /// Epoch milliseconds.
    pub created_at: i64,
    /// Epoch milliseconds.
    pub updated_at: i64,
}

impl Account {
    /// A new account with a generated id.
    pub fn new(
        label: impl Into<String>,
        registrar: impl Into<String>,
        user: impl Into<String>,
        password: impl Into<String>,
        transport: impl Into<String>,
    ) -> Self {
        let now = now_ms();
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            label: label.into(),
            registrar: registrar.into(),
            user: user.into(),
            password: password.into(),
            transport: transport.into(),
            created_at: now,
            updated_at: now,
            ..Default::default()
        }
    }

    /// The per-account policy the call core consumes.
    pub fn profile(&self) -> AccountProfile {
        AccountProfile {
            id: self.id.clone(),
            label: self.label.clone(),
            user: self.user.clone(),
            caller_id_header: self.caller_id_header.clone(),
            dialed_number_header: self.dialed_number_header.clone(),
            capture_headers: self.capture_headers.clone(),
            crm_preset_id: self.crm_preset_id.clone(),
        }
    }

    /// The account without the password, safe to log or serialise.
    pub fn public_view(&self) -> Account {
        let mut view = self.clone();
        view.password.clear();
        view
    }
}

/// Per-account settings the call core needs but that do not belong in
/// the SIP layer.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct AccountProfile {
    /// Config uuid of the account.
    pub id: String,
    /// Human label, e.g. "Office".
    pub label: String,
    /// SIP user part, e.g. "104". Used as the event's `user`.
    pub user: String,
    /// Header that carries the caller's display name, e.g.
    /// "X-Caller-Name". Empty = do not use a header for this.
    pub caller_id_header: String,
    /// Header that carries the number the remote party dialled, e.g.
    /// "X-Dialed-Number". Empty = fall back to the captured
    /// Request-URI and then to the To-URI.
    pub dialed_number_header: String,
    /// Allowlist of extra headers that may leave the core — into
    /// plugins, webhooks and history. An empty list means "no extra
    /// headers", but the two headers above are always included when
    /// they are set, because they are what resolve the caller's
    /// identity in the first place.
    pub capture_headers: Vec<String>,
    /// Which CRM plugin handles this account. Empty = local contacts.
    pub crm_preset_id: String,
}

impl AccountProfile {
    /// The subset of `captured` that is allowed out of the core.
    ///
    /// `captured` is the raw set from the INVITE — it may contain
    /// anything the peer sent. Nothing leaves here without passing
    /// through this function. The allowlist is deliberate: capturing
    /// every header would leak session ids and unrelated PII into
    /// third-party endpoints.
    pub fn filter_headers(&self, captured: &HeaderMap) -> HeaderMap {
        let mut wanted: Vec<String> = self
            .capture_headers
            .iter()
            .map(|h| h.trim().to_ascii_lowercase())
            .filter(|h| !h.is_empty())
            .collect();
        for h in [&self.caller_id_header, &self.dialed_number_header] {
            let h = h.trim();
            if !h.is_empty() {
                wanted.push(h.to_ascii_lowercase());
            }
        }

        let mut out = HeaderMap::new();
        for (name, value) in captured {
            if wanted.contains(&name.to_ascii_lowercase()) {
                out.insert(name.clone(), value.clone());
            }
        }
        out
    }

    /// The caller's display name carried by the configured header.
    ///
    /// Empty when no identity header is configured or the header is
    /// absent — the caller then falls back to the contact lookup and
    /// then to the URI user-part.
    pub fn resolve_caller_id(&self, captured: &HeaderMap) -> String {
        if self.caller_id_header.trim().is_empty() {
            return String::new();
        }
        header_value(captured, &self.caller_id_header)
    }

    /// The number the remote party dialled, carried by the configured
    /// header.
    ///
    /// Empty when no identity header is configured or the header is
    /// absent — the caller then falls back to the captured
    /// Request-URI and then to the To-URI.
    pub fn resolve_dialed_number(&self, captured: &HeaderMap) -> String {
        if self.dialed_number_header.trim().is_empty() {
            return String::new();
        }
        header_value(captured, &self.dialed_number_header)
    }
}

/// Header lookup, case-insensitive on the name.
fn header_value(headers: &HeaderMap, name: &str) -> String {
    let needle = name.trim();
    if needle.is_empty() {
        return String::new();
    }
    for (key, value) in headers {
        if key.eq_ignore_ascii_case(needle) {
            return value.clone();
        }
    }
    String::new()
}

/// Epoch milliseconds.
pub fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or_default()
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
    fn filter_headers_keeps_only_the_allowlist() {
        let profile = AccountProfile {
            capture_headers: vec!["X-Ticket".into()],
            ..Default::default()
        };
        let captured = headers(&[
            ("X-Ticket", "T-1"),
            ("X-Secret", "leak"),
            ("Authorization", "leak"),
        ]);
        let filtered = profile.filter_headers(&captured);
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered.get("X-Ticket").map(String::as_str), Some("T-1"));
    }

    #[test]
    fn filter_headers_always_keeps_the_identity_headers() {
        let profile = AccountProfile {
            caller_id_header: "X-Caller-Name".into(),
            dialed_number_header: "X-Dialed-Number".into(),
            capture_headers: vec![],
            ..Default::default()
        };
        let captured = headers(&[
            ("X-Caller-Name", "Alice"),
            ("X-Dialed-Number", "6666"),
            ("X-Other", "no"),
        ]);
        let filtered = profile.filter_headers(&captured);
        assert_eq!(filtered.len(), 2);
        assert!(filtered.contains_key("X-Caller-Name"));
        assert!(filtered.contains_key("X-Dialed-Number"));
    }

    #[test]
    fn filter_headers_matches_case_insensitively() {
        let profile = AccountProfile {
            capture_headers: vec!["x-ticket".into()],
            ..Default::default()
        };
        let captured = headers(&[("X-Ticket", "T-1")]);
        assert_eq!(profile.filter_headers(&captured).len(), 1);
    }

    #[test]
    fn identity_header_lookup_is_case_insensitive() {
        let profile = AccountProfile {
            caller_id_header: "x-caller-name".into(),
            ..Default::default()
        };
        let captured = headers(&[("X-Caller-Name", "Alice")]);
        assert_eq!(profile.resolve_caller_id(&captured), "Alice");
        assert_eq!(profile.resolve_caller_id(&HeaderMap::new()), "");
    }

    #[test]
    fn password_never_leaves_through_serde() {
        let account = Account::new("Office", "example.com", "104", "s3cret", "udp");
        let json = serde_json::to_string(&account).unwrap();
        assert!(!json.contains("s3cret"));
        assert_eq!(account.public_view().password, "");
    }

    #[test]
    fn new_account_gets_a_uuid_and_timestamps() {
        let account = Account::new("Office", "example.com", "104", "", "udp");
        assert_eq!(account.id.len(), 36);
        assert!(account.created_at > 0);
        assert_eq!(account.updated_at, account.created_at);
    }
}
