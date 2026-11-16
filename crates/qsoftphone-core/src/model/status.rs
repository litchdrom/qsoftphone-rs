//! The statuses an account can be put into, and what each of them does.
//!
//! ## What a status is
//!
//! Three states come with the app, and they are the three a PBX knows:
//!
//! * **Ready** — the extension is free and takes calls.
//! * **Online** — the extension is on the line.
//! * **Pause** — the extension is not taking calls at all.
//!
//! But the list is the **user's**. The settings page has a STATUSES
//! section, and the user adds a status of their own — "In a meeting",
//! "Lunch", "Do not disturb" — with a colour and a hook of its own.
//!
//! The list is **global**: every account uses the same statuses. A
//! state of the user is a state of the user, not a state of one
//! account — two accounts of the same person are the same person, and
//! giving each of them its own list of states is a list of states that
//! nobody can keep straight.
//!
//! ## What a status does
//!
//! Each status carries its own **hook** — what happens when the status
//! is set. There are four kinds, and they are the four a user asked
//! for:
//!
//! | Kind | What it does |
//! |---|---|
//! | dial a code | DTMF if a call is up, a call to the code otherwise |
//! | run a script | the command, as the user typed it |
//! | run an HTTP query | the URL, with the method and the body the user chose |
//! | use a plugin | nothing yet — §8 puts the plugin host out of scope |
//!
//! The point of the hook is that the *state* lives in the PBX, not in
//! the softphone: a user who is ready tells the PBX so with a feature
//! code, and a user who is online tells a CRM so with an HTTP query.
//! The softphone's own button is the switch, and the hook is what the
//! switch is wired to.
//!
//! So the hook belongs to the **status**, not to the account: "Online"
//! dials 666 whoever is online, and "Pause" dials 555 — and the two
//! numbers are two lines in the settings, not two lines in every
//! account.
//!
//! ## The id
//!
//! A status has an **id** as well as a name, and the id is what the
//! account's row holds. The id never changes: a user who renames
//! "Pause" to "Не беспокоить" has renamed the same status, and the
//! accounts that are in it are still in it.

use serde::{Deserialize, Serialize};

/// The id of the status an account is in when nobody has said
/// otherwise.
///
/// It is also the id of the **Ready** status, which is the one a new
/// account starts in.
pub const DEFAULT_STATUS_ID: &str = "ready";

/// The colour a status gets when the user has not chosen one.
pub const DEFAULT_COLOUR: &str = "#4cc06a";

/// The HTTP method a hook fetches with.
///
/// A GET is what a status that only *reports* needs. A POST or a
/// PATCH is what a CRM wants when it is told to change something —
/// and a GET that changes something is a request that a browser will
/// happily repeat.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum HttpMethod {
    /// Fetch the URL and show what came back.
    #[default]
    Get,
    /// Send the body to the URL and make something of it.
    Post,
    /// Put the body at the URL, making it what it should be.
    Put,
    /// Change part of what is at the URL.
    Patch,
    /// Take what is at the URL away.
    Delete,
}

impl HttpMethod {
    /// Every method, in the order they are offered.
    pub const ALL: [HttpMethod; 5] = [
        HttpMethod::Get,
        HttpMethod::Post,
        HttpMethod::Put,
        HttpMethod::Patch,
        HttpMethod::Delete,
    ];

    /// The word the method is written with.
    pub const fn name(self) -> &'static str {
        match self {
            HttpMethod::Get => "GET",
            HttpMethod::Post => "POST",
            HttpMethod::Put => "PUT",
            HttpMethod::Patch => "PATCH",
            HttpMethod::Delete => "DELETE",
        }
    }

    /// The method with that name, whatever the case.
    pub fn from_name(name: &str) -> Option<HttpMethod> {
        match name.trim().to_ascii_uppercase().as_str() {
            "GET" => Some(HttpMethod::Get),
            "POST" => Some(HttpMethod::Post),
            "PUT" => Some(HttpMethod::Put),
            "PATCH" => Some(HttpMethod::Patch),
            "DELETE" => Some(HttpMethod::Delete),
            _ => None,
        }
    }

    /// Whether the method carries a body.
    ///
    /// A GET and a DELETE carry none: the body of a GET is thrown
    /// away by half the servers in the world, and a DELETE that has a
    /// body is a DELETE with a note attached to it.
    pub const fn carries_body(self) -> bool {
        matches!(self, HttpMethod::Post | HttpMethod::Put | HttpMethod::Patch)
    }
}

/// What a status does when it is set.
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
    /// The URL is fetched with the method the user chose, and the
    /// body is sent with the methods that carry one.
    Http {
        /// The method: GET, POST, PUT, PATCH or DELETE.
        method: HttpMethod,
        /// The URL.
        url: String,
        /// The body, for the methods that carry one.
        body: String,
    },
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
            StatusAction::Http { .. } => "http",
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
            | StatusAction::Plugin(value) => value,
            StatusAction::Http { url, .. } => url,
        }
    }

    /// The HTTP method of the action; GET for the kinds that are not
    /// an HTTP query.
    pub fn method(&self) -> HttpMethod {
        match self {
            StatusAction::Http { method, .. } => *method,
            _ => HttpMethod::default(),
        }
    }

    /// The body of the action; empty for the kinds that have none.
    pub fn body(&self) -> &str {
        match self {
            StatusAction::Http { body, .. } => body,
            _ => "",
        }
    }

    /// The action of a kind and a value.
    ///
    /// An empty value is **no action**: a kind with nothing in it is a
    /// half-finished setting, and running it would dial nothing.
    pub fn of(kind: &str, value: &str) -> StatusAction {
        StatusAction::of_full(kind, value, "GET", "")
    }

    /// The action of a kind, a value, an HTTP method and an HTTP body.
    ///
    /// The method and the body are only used by the HTTP kind; the
    /// others ignore them, so a caller does not have to know which
    /// kind it is building.
    pub fn of_full(kind: &str, value: &str, method: &str, body: &str) -> StatusAction {
        let value = value.trim().to_string();
        match kind.trim().to_ascii_lowercase().as_str() {
            "dial" if !value.is_empty() => StatusAction::Dial(value),
            "script" if !value.is_empty() => StatusAction::Script(value),
            "http" if !value.is_empty() => StatusAction::Http {
                method: HttpMethod::from_name(method).unwrap_or_default(),
                url: value,
                body: body.to_string(),
            },
            "plugin" if !value.is_empty() => StatusAction::Plugin(value),
            _ => StatusAction::None,
        }
    }

    /// Whether the action would do anything.
    pub fn is_some(&self) -> bool {
        !matches!(self, StatusAction::None)
    }

    /// Read an action out of the JSON of an older build.
    ///
    /// The HTTP action was a URL and nothing else before M6.14 —
    /// `{"http":"https://…"}` — and the URL is read as a **GET**: a
    /// user who had one had a GET, and a GET is the method that always
    /// can be fetched.
    ///
    /// This is what the migration off the per-account hook uses: the
    /// hook of an account that was saved by an older build is read out
    /// of the column it was kept in, and put on the status.
    pub fn from_legacy_json(text: &str) -> Option<StatusAction> {
        let text = text.trim();
        if text.is_empty() {
            return None;
        }
        if let Ok(action) = serde_json::from_str::<StatusAction>(text) {
            return Some(action);
        }
        /// The old shape of the HTTP action: a URL and nothing else.
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        enum Legacy {
            Http(String),
        }
        match serde_json::from_str::<Legacy>(text) {
            Ok(Legacy::Http(url)) => Some(StatusAction::Http {
                method: HttpMethod::Get,
                url,
                body: String::new(),
            }),
            Err(_) => None,
        }
    }
}

/// One status the user can put an account into.
///
/// The fields are plain strings on purpose: this is a line in a JSON
/// file that a user may edit by hand, and a file that says
/// `"actionKind": "http"` says what it means. The typed
/// [`StatusAction`] is built from them when the hook runs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct StatusDefinition {
    /// The stable id: `ready`, `online`, `pause`, or one of the
    /// user's own.
    pub id: String,
    /// The word the button shows.
    pub name: String,
    /// The colour of the lamp: `#4cc06a`.
    pub colour: String,
    /// What happens when the status is set: `""`, `dial`, `script`,
    /// `http` or `plugin`.
    pub action_kind: String,
    /// The code, the command or the URL.
    pub action_value: String,
    /// The HTTP method, when the kind is `http`.
    pub action_method: String,
    /// The body the HTTP query sends.
    pub action_body: String,
}

impl Default for StatusDefinition {
    fn default() -> Self {
        Self {
            id: String::new(),
            name: String::new(),
            colour: DEFAULT_COLOUR.to_string(),
            action_kind: String::new(),
            action_value: String::new(),
            action_method: HttpMethod::default().name().to_string(),
            action_body: String::new(),
        }
    }
}

impl StatusDefinition {
    /// The statuses the app comes with.
    ///
    /// They are the three a PBX knows, and they are the start of the
    /// user's list rather than the whole of it: every one of them can
    /// be renamed, recoloured and given a hook of its own.
    pub fn built_ins() -> Vec<StatusDefinition> {
        vec![
            StatusDefinition {
                id: "ready".to_string(),
                name: "Ready".to_string(),
                colour: "#4cc06a".to_string(),
                ..Default::default()
            },
            StatusDefinition {
                id: "online".to_string(),
                name: "Online".to_string(),
                colour: "#d8a630".to_string(),
                ..Default::default()
            },
            StatusDefinition {
                id: "pause".to_string(),
                name: "Pause".to_string(),
                colour: "#808080".to_string(),
                ..Default::default()
            },
        ]
    }

    /// A status of the user's own, with a name and nothing else.
    ///
    /// The id is made from the name, and made unique against the ids
    /// that are already taken: two statuses called "Busy" are two
    /// statuses, and the second one must not take the first one's id.
    pub fn new(name: &str, taken: &[String]) -> StatusDefinition {
        let name = name.trim();
        StatusDefinition {
            id: StatusDefinition::id_for(name, taken),
            name: name.to_string(),
            ..Default::default()
        }
    }

    /// The id a status of this name gets.
    ///
    /// The name is lowercased and trimmed — and **not** reduced to
    /// ASCII, because the user writes in their own language and a name
    /// of "Готов" is a name, not a name with the good parts taken
    /// out.
    pub fn id_for(name: &str, taken: &[String]) -> String {
        let base = name.trim().to_lowercase();
        let base = if base.is_empty() {
            "status".to_string()
        } else {
            base
        };
        if !taken.iter().any(|id| id == &base) {
            return base;
        }
        for n in 2..1000 {
            let candidate = format!("{base}-{n}");
            if !taken.iter().any(|id| id == &candidate) {
                return candidate;
            }
        }
        base
    }

    /// Whether this is one of the statuses the app came with.
    ///
    /// The built-ins cannot be removed: they are what an account falls
    /// back to, and a list without "Ready" is a list whose accounts
    /// are all in a state that is not there.
    pub fn is_built_in(&self) -> bool {
        matches!(self.id.as_str(), "ready" | "online" | "pause")
    }

    /// The hook, as the typed thing it is.
    pub fn action(&self) -> StatusAction {
        StatusAction::of_full(
            &self.action_kind,
            &self.action_value,
            &self.action_method,
            &self.action_body,
        )
    }

    /// The colour as red, green and blue.
    ///
    /// A colour that cannot be read is the default one: a lamp in the
    /// wrong colour is still a lamp, and a lamp that is not there at
    /// all is a status the user cannot see.
    pub fn colour_rgb(&self) -> (u8, u8, u8) {
        parse_colour(&self.colour).unwrap_or((0x4c, 0xc0, 0x6a))
    }

    /// The status an account is in when its own status is not known.
    ///
    /// It is the **Ready** status: the state a phone is in when
    /// nobody has said otherwise.
    pub fn fallback() -> StatusDefinition {
        StatusDefinition {
            id: DEFAULT_STATUS_ID.to_string(),
            name: "Ready".to_string(),
            ..Default::default()
        }
    }

    /// The name a status with no name gets: its id, with the first
    /// letter capitalised.
    ///
    /// An id is a word the user typed, so it is a word that can be a
    /// name — and a name that says what the status is beats a name
    /// that says nothing.
    pub fn name_from_id(id: &str) -> String {
        let mut chars = id.chars();
        match chars.next() {
            Some(first) => {
                let mut name = first.to_uppercase().collect::<String>();
                name.push_str(chars.as_str());
                name
            }
            None => String::new(),
        }
    }
}

/// Read a colour written as `#rrggbb` or `rrggbb`.
///
/// Anything else is nothing: the caller decides what to do about it.
pub fn parse_colour(text: &str) -> Option<(u8, u8, u8)> {
    let text = text.trim();
    let hex = text.strip_prefix('#').unwrap_or(text);
    if hex.len() != 6 || !hex.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    let red = u8::from_str_radix(&hex[0..2], 16).ok()?;
    let green = u8::from_str_radix(&hex[2..4], 16).ok()?;
    let blue = u8::from_str_radix(&hex[4..6], 16).ok()?;
    Some((red, green, blue))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_three_statuses_come_with_the_app() {
        let built_ins = StatusDefinition::built_ins();
        assert_eq!(built_ins.len(), 3);
        assert_eq!(built_ins[0].id, "ready");
        assert_eq!(built_ins[0].name, "Ready");
        assert_eq!(built_ins[1].id, "online");
        assert_eq!(built_ins[2].id, "pause");
        // And they are the ones that cannot be removed.
        assert!(built_ins.iter().all(StatusDefinition::is_built_in));
    }

    #[test]
    fn a_status_of_the_users_own_is_not_a_built_in() {
        let status = StatusDefinition::new("In a meeting", &[]);
        assert_eq!(status.id, "in a meeting");
        assert_eq!(status.name, "In a meeting");
        assert!(!status.is_built_in());
    }

    #[test]
    fn two_statuses_of_the_same_name_get_different_ids() {
        // The id is what the account's row holds, so two statuses
        // called "Busy" must not share one.
        let first = StatusDefinition::new("Busy", &[]);
        let second = StatusDefinition::new("Busy", std::slice::from_ref(&first.id));
        assert_ne!(first.id, second.id);
        assert_eq!(second.id, "busy-2");
    }

    #[test]
    fn an_id_keeps_the_language_the_user_wrote_in() {
        // A name in the user's own language is a name, not a name with
        // the letters that are not Latin taken out.
        let status = StatusDefinition::new("Готов", &[]);
        assert_eq!(status.id, "готов");
        assert_eq!(status.name, "Готов");
    }

    #[test]
    fn a_colour_is_read_from_its_hex() {
        assert_eq!(parse_colour("#4cc06a"), Some((0x4c, 0xc0, 0x6a)));
        assert_eq!(parse_colour("4CC06A"), Some((0x4c, 0xc0, 0x6a)));
        assert_eq!(parse_colour("  #d8a630 "), Some((0xd8, 0xa6, 0x30)));
        // And anything that is not a colour is nothing.
        assert_eq!(parse_colour("green"), None);
        assert_eq!(parse_colour("#4cc"), None);
        assert_eq!(parse_colour("#4cc06"), None);
        assert_eq!(parse_colour(""), None);
    }

    #[test]
    fn a_status_with_a_broken_colour_still_has_one() {
        let mut status = StatusDefinition::built_ins().remove(0);
        status.colour = "not a colour".to_string();
        assert_eq!(status.colour_rgb(), (0x4c, 0xc0, 0x6a));
    }

    #[test]
    fn the_methods_have_their_names() {
        assert_eq!(HttpMethod::Get.name(), "GET");
        assert_eq!(HttpMethod::Post.name(), "POST");
        assert_eq!(HttpMethod::Put.name(), "PUT");
        assert_eq!(HttpMethod::Patch.name(), "PATCH");
        assert_eq!(HttpMethod::Delete.name(), "DELETE");
        assert_eq!(HttpMethod::ALL.len(), 5);
    }

    #[test]
    fn a_method_is_recognised_by_its_name() {
        assert_eq!(HttpMethod::from_name("get"), Some(HttpMethod::Get));
        assert_eq!(HttpMethod::from_name(" POST "), Some(HttpMethod::Post));
        assert_eq!(HttpMethod::from_name("Patch"), Some(HttpMethod::Patch));
        assert_eq!(HttpMethod::from_name("DELETE"), Some(HttpMethod::Delete));
        assert_eq!(HttpMethod::from_name("FETCH"), None);
    }

    #[test]
    fn only_the_methods_that_change_something_carry_a_body() {
        assert!(HttpMethod::Post.carries_body());
        assert!(HttpMethod::Put.carries_body());
        assert!(HttpMethod::Patch.carries_body());
        assert!(!HttpMethod::Get.carries_body());
        assert!(!HttpMethod::Delete.carries_body());
    }

    #[test]
    fn an_action_is_a_kind_and_a_value() {
        let action = StatusAction::of("dial", "*51");
        assert_eq!(action, StatusAction::Dial("*51".to_string()));
        assert_eq!(action.kind(), "dial");
        assert_eq!(action.value(), "*51");

        let action = StatusAction::of("script", "capture.exe");
        assert_eq!(action, StatusAction::Script("capture.exe".to_string()));

        let action = StatusAction::of("plugin", "bitrix");
        assert_eq!(action, StatusAction::Plugin("bitrix".to_string()));
    }

    #[test]
    fn an_http_action_carries_its_method_and_its_body() {
        let action = StatusAction::of_full(
            "http",
            "https://crm.example.com/api/state",
            "patch",
            "{\"state\":\"online\"}",
        );
        assert_eq!(action.kind(), "http");
        assert_eq!(action.value(), "https://crm.example.com/api/state");
        assert_eq!(action.method(), HttpMethod::Patch);
        assert_eq!(action.body(), "{\"state\":\"online\"}");
    }

    #[test]
    fn an_http_action_with_an_unknown_method_is_a_get() {
        // A method that is not one is a method nobody can fetch with,
        // and GET is the one that always can.
        let action = StatusAction::of_full("http", "https://example.com", "TELEPORT", "");
        assert_eq!(action.method(), HttpMethod::Get);
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
    fn a_status_builds_its_own_hook() {
        // The status carries the hook, and the hook is what runs when
        // the status is set.
        let mut status = StatusDefinition::built_ins().remove(1);
        status.action_kind = "dial".to_string();
        status.action_value = "666".to_string();
        assert_eq!(status.action(), StatusAction::Dial("666".to_string()));
    }

    #[test]
    fn the_action_survives_a_json_round_trip() {
        // The action is what the plugin seam will carry.
        for action in [
            StatusAction::None,
            StatusAction::Dial("*51".to_string()),
            StatusAction::Script("capture.exe".to_string()),
            StatusAction::Http {
                method: HttpMethod::Post,
                url: "https://crm.example.com/api/state".to_string(),
                body: "{\"state\":\"online\"}".to_string(),
            },
            StatusAction::Plugin("bitrix".to_string()),
        ] {
            let json = serde_json::to_string(&action).expect("the action serialises");
            let back: StatusAction = serde_json::from_str(&json).expect("the action reads back");
            assert_eq!(back, action);
        }
    }

    #[test]
    fn an_action_of_an_older_build_is_read() {
        // The hook of an account that was saved before the hooks moved
        // to the statuses: it is in the column, and it is not lost.
        let action =
            StatusAction::from_legacy_json(r#"{"dial":"666"}"#).expect("the old action is read");
        assert_eq!(action, StatusAction::Dial("666".to_string()));

        // The old HTTP action was a URL and nothing else, and it is a
        // GET: a user who had one had a GET.
        let action = StatusAction::from_legacy_json(r#"{"http":"https://crm.example.com/api"}"#)
            .expect("the old action is read");
        assert_eq!(
            action,
            StatusAction::Http {
                method: HttpMethod::Get,
                url: "https://crm.example.com/api".to_string(),
                body: String::new(),
            }
        );

        // And nothing that is not an action is one.
        assert!(StatusAction::from_legacy_json("").is_none());
        assert!(StatusAction::from_legacy_json("not json").is_none());
        assert!(StatusAction::from_legacy_json(r#"{"nonsense":"x"}"#).is_none());
    }

    #[test]
    fn a_status_survives_a_json_round_trip() {
        for status in StatusDefinition::built_ins() {
            let json = serde_json::to_string(&status).expect("the status serialises");
            let back: StatusDefinition =
                serde_json::from_str(&json).expect("the status reads back");
            assert_eq!(back, status);
        }
    }
}
