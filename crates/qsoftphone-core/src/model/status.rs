//! The status an account shows, and what it does when it changes.
//!
//! ## What a status is
//!
//! Three states, and they are the three a PBX knows:
//!
//! * **Ready** — the extension is free and takes calls.
//! * **Online** — the extension is on the line.
//! * **Pause** — the extension is not taking calls at all.
//!
//! The status is *the account's own*: it belongs to the account, it is
//! stored with it, and it is shown on the account's button in the left
//! panel. Two accounts on the same server can be in different states —
//! that is the ordinary case, not an exception.
//!
//! ## What changing it does
//!
//! Setting the status runs the account's **action**. There are four
//! kinds, and they are the four a user asked for:
//!
//! | Kind | What it does |
//! |---|---|
//! | dial a code | DTMF if a call is up, a call to the code otherwise |
//! | run a script | the command, as the user typed it |
//! | run an HTTP query | a `GET` of the URL |
//! | use a plugin | nothing yet — §8 puts the plugin host out of scope |
//!
//! The point of the action is that the *state* lives in the PBX, not in
//! the softphone: a user who is ready tells the PBX so with a feature
//! code, and a user who is online tells a CRM so with an HTTP query.
//! The softphone's own button is the switch, and the action is what the
//! switch is wired to.

use serde::{Deserialize, Serialize};

/// The status an account shows.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum AccountStatus {
    /// The extension is free and takes calls.
    #[default]
    Ready,
    /// The extension is on the line.
    Online,
    /// The extension is not taking calls.
    Pause,
}

impl AccountStatus {
    /// Every status, in the order they are offered.
    pub const ALL: [AccountStatus; 3] = [
        AccountStatus::Ready,
        AccountStatus::Online,
        AccountStatus::Pause,
    ];

    /// The word the button shows.
    pub const fn name(self) -> &'static str {
        match self {
            AccountStatus::Ready => "Ready",
            AccountStatus::Online => "Online",
            AccountStatus::Pause => "Pause",
        }
    }

    /// The status with that name, whatever the case.
    pub fn from_name(name: &str) -> Option<AccountStatus> {
        match name.trim().to_ascii_lowercase().as_str() {
            "ready" => Some(AccountStatus::Ready),
            "online" => Some(AccountStatus::Online),
            "pause" | "paused" => Some(AccountStatus::Pause),
            _ => None,
        }
    }

    /// Whether the account is not taking calls.
    ///
    /// This is the one state that is *not* like the others: it is the
    /// state the user says they are in, and the softphone does not
    /// change anything by it on its own — a paused account is still
    /// dialled, and its lamps still say what they say.
    ///
    /// What the pause *does* is run the account's action, and the
    /// action is what tells the PBX. The state lives outside the
    /// softphone.
    pub const fn is_paused(self) -> bool {
        matches!(self, AccountStatus::Pause)
    }
}

/// What an account does when its status is set.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum StatusAction {
    /// Nothing: the status is only a label.
    #[default]
    None,
    /// Dial a code.
    ///
    /// The code is a PBX feature code (`*51`, `*60`) or a number. If a
    /// call is **up** on the account the code is sent as DTMF on it;
    /// if there is no call, the code is **called**.
    Dial(String),
    /// Run a script.
    ///
    /// The command is run as the user typed it, through the system's
    /// own shell — a screen capture, a program that tells a door
    /// something, a program that writes to a file.
    Script(String),
    /// Run an HTTP query.
    ///
    /// The URL is fetched with `GET`.
    Http(String),
    /// Use a plugin.
    ///
    /// Stored and shown, and **not run**: §8 of the brief puts the
    /// plugin host out of scope, and a plugin that pretends to work is
    /// worse than one that says it is not there yet.
    Plugin(String),
}

impl StatusAction {
    /// The kind of the action, for a list of choices.
    pub fn kind(&self) -> &'static str {
        match self {
            StatusAction::None => "",
            StatusAction::Dial(_) => "dial",
            StatusAction::Script(_) => "script",
            StatusAction::Http(_) => "http",
            StatusAction::Plugin(_) => "plugin",
        }
    }

    /// The value of the action: the code, the command, the URL or the
    /// plugin's name.
    pub fn value(&self) -> &str {
        match self {
            StatusAction::None => "",
            StatusAction::Dial(value)
            | StatusAction::Script(value)
            | StatusAction::Http(value)
            | StatusAction::Plugin(value) => value,
        }
    }

    /// The action of a kind and a value.
    ///
    /// An empty value is **no action**: a kind with nothing in it is a
    /// half-finished setting, and running it would dial nothing.
    pub fn of(kind: &str, value: &str) -> StatusAction {
        let value = value.trim().to_string();
        match kind.trim().to_ascii_lowercase().as_str() {
            "dial" if !value.is_empty() => StatusAction::Dial(value),
            "script" if !value.is_empty() => StatusAction::Script(value),
            "http" if !value.is_empty() => StatusAction::Http(value),
            "plugin" if !value.is_empty() => StatusAction::Plugin(value),
            _ => StatusAction::None,
        }
    }

    /// Whether the action would do anything.
    pub fn is_some(&self) -> bool {
        !matches!(self, StatusAction::None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_three_statuses_have_their_names() {
        assert_eq!(AccountStatus::Ready.name(), "Ready");
        assert_eq!(AccountStatus::Online.name(), "Online");
        assert_eq!(AccountStatus::Pause.name(), "Pause");
        assert_eq!(AccountStatus::ALL.len(), 3);
    }

    #[test]
    fn a_status_is_recognised_by_its_name() {
        assert_eq!(
            AccountStatus::from_name("Ready"),
            Some(AccountStatus::Ready)
        );
        assert_eq!(
            AccountStatus::from_name("online"),
            Some(AccountStatus::Online)
        );
        assert_eq!(
            AccountStatus::from_name(" PAUSE "),
            Some(AccountStatus::Pause)
        );
        assert_eq!(
            AccountStatus::from_name("paused"),
            Some(AccountStatus::Pause)
        );
        assert_eq!(AccountStatus::from_name("busy"), None);
    }

    #[test]
    fn the_default_status_is_ready() {
        // A new account is ready: it is the state a phone is in when
        // nobody has said otherwise.
        assert_eq!(AccountStatus::default(), AccountStatus::Ready);
    }

    #[test]
    fn only_pause_is_paused() {
        assert!(!AccountStatus::Ready.is_paused());
        assert!(!AccountStatus::Online.is_paused());
        assert!(AccountStatus::Pause.is_paused());
    }

    #[test]
    fn an_action_is_a_kind_and_a_value() {
        let action = StatusAction::of("dial", "*51");
        assert_eq!(action, StatusAction::Dial("*51".to_string()));
        assert_eq!(action.kind(), "dial");
        assert_eq!(action.value(), "*51");

        let action = StatusAction::of("script", "capture.exe");
        assert_eq!(action, StatusAction::Script("capture.exe".to_string()));

        let action = StatusAction::of("http", "https://crm/api/ready");
        assert_eq!(
            action,
            StatusAction::Http("https://crm/api/ready".to_string())
        );

        let action = StatusAction::of("plugin", "bitrix");
        assert_eq!(action, StatusAction::Plugin("bitrix".to_string()));
    }

    #[test]
    fn a_kind_with_nothing_in_it_is_no_action() {
        // A half-finished setting is not a setting: running it would
        // dial nothing, or fetch nothing.
        assert_eq!(StatusAction::of("dial", "  "), StatusAction::None);
        assert_eq!(StatusAction::of("script", ""), StatusAction::None);
        assert_eq!(StatusAction::of("", "something"), StatusAction::None);
        assert_eq!(
            StatusAction::of("nonsense", "something"),
            StatusAction::None
        );
        assert!(!StatusAction::None.is_some());
        assert!(StatusAction::of("dial", "5").is_some());
    }

    #[test]
    fn the_action_survives_a_json_round_trip() {
        // The action is stored as JSON in the account's row.
        for action in [
            StatusAction::None,
            StatusAction::Dial("*51".to_string()),
            StatusAction::Script("capture.exe".to_string()),
            StatusAction::Http("https://crm/api/ready".to_string()),
            StatusAction::Plugin("bitrix".to_string()),
        ] {
            let json = serde_json::to_string(&action).expect("the action serialises");
            let back: StatusAction = serde_json::from_str(&json).expect("the action reads back");
            assert_eq!(back, action);
        }
    }

    #[test]
    fn the_status_survives_a_json_round_trip() {
        for status in AccountStatus::ALL {
            let json = serde_json::to_string(&status).expect("the status serialises");
            let back: AccountStatus = serde_json::from_str(&json).expect("the status reads back");
            assert_eq!(back, status);
        }
    }
}
