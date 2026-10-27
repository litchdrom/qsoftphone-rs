//! Registration state — what the UI needs to know, and nothing more.
//!
//! §3: "everything downstream of the SIP stack talks to the canonical
//! event stream". The UI must never block on the network, so the SIP
//! layer reports state through a channel and the UI drains it at the
//! top of each frame.

use serde::{Deserialize, Serialize};

/// Where an account's registration is.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", tag = "state")]
pub enum RegistrationState {
    /// Nothing has been attempted yet.
    Idle,
    /// A REGISTER is in flight.
    Registering,
    /// The registrar accepted the binding.
    Registered {
        /// The expiry the registrar granted, in seconds.
        expires: u32,
    },
    /// The registrar answered, and the answer was not a success.
    Rejected {
        /// The SIP status code.
        status: u16,
        /// The registrar's reason phrase.
        reason: String,
    },
    /// The attempt failed before the registrar answered — the server
    /// was unreachable, the DNS name did not resolve, and so on.
    Failed {
        /// What went wrong.
        reason: String,
    },
}

impl RegistrationState {
    /// Is the account currently registered?
    pub fn is_registered(&self) -> bool {
        matches!(self, RegistrationState::Registered { .. })
    }

    /// A one-line description for the UI.
    pub fn summary(&self) -> String {
        match self {
            RegistrationState::Idle => "Not registered".to_string(),
            RegistrationState::Registering => "Registering…".to_string(),
            RegistrationState::Registered { expires } => {
                format!("Registered ({expires}s)")
            }
            RegistrationState::Rejected { status, reason } => {
                format!("Rejected: {status} {reason}")
            }
            RegistrationState::Failed { reason } => format!("Failed: {reason}"),
        }
    }

    /// A short badge for the account list, where there is no room for
    /// a sentence.
    pub fn badge(&self) -> &'static str {
        match self {
            RegistrationState::Idle => "",
            RegistrationState::Registering => "…",
            RegistrationState::Registered { .. } => "●",
            RegistrationState::Rejected { .. } => "✗",
            RegistrationState::Failed { .. } => "!",
        }
    }
}

/// A state change for one account.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RegistrationEvent {
    /// The account this is about.
    pub account_id: String,
    /// The SIP user part, for display.
    pub user: String,
    /// The new state.
    pub state: RegistrationState,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_registered_counts_as_registered() {
        assert!(RegistrationState::Registered { expires: 300 }.is_registered());
        assert!(!RegistrationState::Idle.is_registered());
        assert!(!RegistrationState::Registering.is_registered());
        assert!(
            !RegistrationState::Rejected {
                status: 403,
                reason: "Forbidden".into()
            }
            .is_registered()
        );
        assert!(
            !RegistrationState::Failed {
                reason: "unreachable".into()
            }
            .is_registered()
        );
    }

    #[test]
    fn the_summary_is_human_readable() {
        assert_eq!(
            RegistrationState::Registered { expires: 300 }.summary(),
            "Registered (300s)"
        );
        assert_eq!(
            RegistrationState::Rejected {
                status: 401,
                reason: "Unauthorized".into()
            }
            .summary(),
            "Rejected: 401 Unauthorized"
        );
    }

    #[test]
    fn the_state_round_trips_through_json() {
        let event = RegistrationEvent {
            account_id: "a".into(),
            user: "104".into(),
            state: RegistrationState::Registered { expires: 60 },
        };
        let json = serde_json::to_string(&event).unwrap();
        assert_eq!(
            json,
            r#"{"accountId":"a","user":"104","state":{"state":"registered","expires":60}}"#
        );
        let back: RegistrationEvent = serde_json::from_str(&json).unwrap();
        assert_eq!(back, event);
    }
}
