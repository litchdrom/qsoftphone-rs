//! Application settings — the JSON file next to the app data.
//!
//! Accounts are *not* here; they live in SQLite
//! ([`crate::storage::AccountStore`]). This file holds the
//! app-level preferences that have no place in an account row.
//!
//! Port of the C++ `Config` (`src/config/Config.h`), minus the
//! account array.

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::model::status::{DEFAULT_COLOUR, DEFAULT_STATUS_ID, StatusDefinition};

/// The settings document.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct AppConfig {
    /// Schema version, for future migrations.
    pub version: u32,
    pub logging: LoggingConfig,
    pub audio: AudioConfig,
    pub media: MediaConfig,
    pub network: NetworkConfig,
    pub ui: UiConfig,
    /// The account the UI should select at startup.
    pub active_account_id: Option<String>,
    /// The statuses an account can be put into.
    ///
    /// The list is **global**: every account uses these, and each of
    /// them carries a colour and a hook of its own. The three that
    /// come with the app — Ready, Online and Pause — are the start of
    /// the list rather than the whole of it.
    pub statuses: Vec<StatusDefinition>,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            version: 1,
            logging: LoggingConfig::default(),
            audio: AudioConfig::default(),
            media: MediaConfig::default(),
            network: NetworkConfig::default(),
            ui: UiConfig::default(),
            active_account_id: None,
            statuses: StatusDefinition::built_ins(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct LoggingConfig {
    /// `tracing` filter directive used when `RUST_LOG` is unset.
    pub level: String,
}

impl Default for LoggingConfig {
    fn default() -> Self {
        Self {
            level: "info".to_string(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct AudioConfig {
    pub speaker_volume: f32,
    pub microphone_volume: f32,
    pub ringtone_volume: f32,
    pub ringback_volume: f32,
    /// Path to a WAV file. Empty = the bundled default.
    pub ringtone_file: String,
}

impl Default for AudioConfig {
    fn default() -> Self {
        Self {
            speaker_volume: 0.8,
            microphone_volume: 0.8,
            ringtone_volume: 0.9,
            ringback_volume: 0.7,
            ringtone_file: String::new(),
        }
    }
}

/// The media settings.
///
/// The C++ kept the codec priorities in the `media` section of the
/// config file, under `audio`. They are global there, and they are
/// global here: the codecs are what the *softphone* speaks, not what
/// one account speaks.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct MediaConfig {
    /// The codecs to offer, in preference order — `PCMU`, `PCMA`,
    /// `G722`, `opus`.
    ///
    /// The first one is the one the peer is asked for; the peer may
    /// answer with another one of these, and then that one is used.
    pub codecs: Vec<String>,
    /// The microphone to use. Empty = the system default.
    pub microphone: String,
    /// The speaker to use. Empty = the system default.
    pub speaker: String,
}

impl Default for MediaConfig {
    fn default() -> Self {
        Self {
            // G.711 first: they are what every peer has. G.722 is
            // there too, but a PBX has to be told to allow it — and
            // the user can move it to the front in the settings.
            //
            // Opus is last of all: it is offered, and a peer that
            // answers with it gets it, but a peer that offers several
            // gets the codec that is known to work against a real
            // PBX. The user can move it up in the settings.
            codecs: vec![
                "PCMU".to_string(),
                "PCMA".to_string(),
                "G722".to_string(),
                "opus".to_string(),
            ],
            microphone: String::new(),
            speaker: String::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct NetworkConfig {
    /// Default transport for new accounts: `udp`, `tcp` or `tls`.
    pub transport: String,
    /// Default local port.
    pub port: u16,
    /// Whether TLS transports are offered.
    pub allow_tls: bool,
}

impl Default for NetworkConfig {
    fn default() -> Self {
        Self {
            transport: "udp".to_string(),
            port: 5060,
            allow_tls: true,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct UiConfig {
    /// `dark` or `light`.
    pub theme: String,
    pub window_width: f32,
    pub window_height: f32,
}

impl Default for UiConfig {
    fn default() -> Self {
        Self {
            theme: "dark".to_string(),
            window_width: 1100.0,
            window_height: 700.0,
        }
    }
}

impl AppConfig {
    /// Read the settings file.
    ///
    /// A missing file is not an error — the defaults are returned and
    /// the file is written on the next save. A *malformed* file is an
    /// error: silently replacing a user's settings with defaults
    /// would lose their configuration.
    pub fn load(path: &Path) -> Result<Self> {
        let mut config = match std::fs::read_to_string(path) {
            Ok(text) => serde_json::from_str(&text).map_err(|e| {
                Error::Message(format!(
                    "config file {} is not valid JSON: {e}",
                    path.display()
                ))
            })?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Self::default(),
            Err(e) => return Err(Error::Io(e)),
        };
        config.repair_statuses();
        Ok(config)
    }

    /// Write the settings file.
    pub fn save(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)?;
            }
        }
        let text = serde_json::to_string_pretty(self)?;
        write_atomic(path, text.as_bytes())
    }

    /// The status with this id.
    pub fn status(&self, id: &str) -> Option<&StatusDefinition> {
        self.statuses.iter().find(|status| status.id == id)
    }

    /// The status an account is in when its own status is not known.
    ///
    /// It is the **Ready** status: the state a phone is in when
    /// nobody has said otherwise.
    pub fn default_status(&self) -> StatusDefinition {
        self.status(DEFAULT_STATUS_ID)
            .cloned()
            .or_else(|| self.statuses.first().cloned())
            .unwrap_or_else(StatusDefinition::fallback)
    }

    /// Make the statuses usable.
    ///
    /// A settings file is a document a user may edit by hand, and a
    /// hand-edited document is missing things:
    ///
    /// * a status with **no id** gets one made from its name, and one
    ///   with **no name** gets one made from its id;
    /// * two statuses with **one id** — the second one gets another,
    ///   because an id that says two things says nothing;
    /// * the three the app comes with are **put back** if they are
    ///   missing: they are what an account falls back to, and a list
    ///   without "Ready" is a list whose accounts are all in a state
    ///   that is not there.
    pub fn repair_statuses(&mut self) {
        let mut taken: Vec<String> = Vec::new();
        for index in 0..self.statuses.len() {
            let name = self.statuses[index].name.trim().to_string();
            let id = self.statuses[index].id.trim().to_string();
            let id = if id.is_empty() {
                StatusDefinition::id_for(&name, &taken)
            } else if taken.iter().any(|other| other == &id) {
                StatusDefinition::id_for(&id, &taken)
            } else {
                id
            };
            taken.push(id.clone());
            self.statuses[index].id = id;
            if self.statuses[index].name.trim().is_empty() {
                let fallback = StatusDefinition::name_from_id(&self.statuses[index].id);
                self.statuses[index].name = fallback;
            }
            if self.statuses[index].colour.trim().is_empty() {
                self.statuses[index].colour = DEFAULT_COLOUR.to_string();
            }
            if self.statuses[index].action.method.trim().is_empty() {
                self.statuses[index].action.method = "GET".to_string();
            }
        }
        // The built-ins are put back, and their names and colours are
        // kept for the ones that are there: the user may have renamed
        // "Ready" and that is their business.
        for built_in in StatusDefinition::built_ins() {
            match self
                .statuses
                .iter_mut()
                .find(|status| status.id == built_in.id)
            {
                Some(status) => {
                    if status.name.trim().is_empty() {
                        status.name = built_in.name.clone();
                    }
                    if status.colour.trim().is_empty() {
                        status.colour = built_in.colour.clone();
                    }
                }
                None => self.statuses.push(built_in),
            }
        }
    }
}

/// Write via a temporary file and a rename, so a crash mid-write
/// cannot leave a half-written config behind.
fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, bytes)?;

    // `rename` replaces the destination atomically on unix but fails
    // on Windows when the destination already exists.
    if cfg!(windows) && path.exists() {
        std::fs::remove_file(path)?;
    }
    std::fs::rename(&tmp, path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::status::{StatusAction, StatusHook};

    fn temp_path(name: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!(
            "qsoftphone-cfg-{}-{name}.json",
            uuid::Uuid::new_v4()
        ))
    }

    #[test]
    fn a_missing_file_yields_the_defaults() {
        let path = temp_path("missing");
        let config = AppConfig::load(&path).unwrap();
        assert_eq!(config.version, 1);
        assert_eq!(config.network.transport, "udp");
        assert_eq!(config.audio.speaker_volume, 0.8);
    }

    #[test]
    fn save_then_load_round_trips() {
        let path = temp_path("roundtrip");
        let config = AppConfig {
            active_account_id: Some("abc".into()),
            audio: AudioConfig {
                ringtone_volume: 0.25,
                ..Default::default()
            },
            ..Default::default()
        };
        config.save(&path).unwrap();

        let loaded = AppConfig::load(&path).unwrap();
        assert_eq!(loaded, config);
        assert_eq!(loaded.active_account_id.as_deref(), Some("abc"));

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn saving_twice_overwrites_cleanly() {
        let path = temp_path("overwrite");
        let mut config = AppConfig::default();
        config.save(&path).unwrap();
        config.network.port = 5080;
        config.save(&path).unwrap();

        let loaded = AppConfig::load(&path).unwrap();
        assert_eq!(loaded.network.port, 5080);

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn a_malformed_file_is_an_error_not_a_silent_reset() {
        let path = temp_path("broken");
        std::fs::write(&path, "{ this is not json").unwrap();
        assert!(AppConfig::load(&path).is_err());
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn unknown_fields_are_tolerated() {
        // A newer build's settings file must still open in an older
        // build rather than refusing to start.
        let path = temp_path("extra");
        std::fs::write(&path, r#"{"version":1,"somethingNew":42}"#).unwrap();
        let config = AppConfig::load(&path).unwrap();
        assert_eq!(config.version, 1);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn the_statuses_come_with_the_app() {
        let config = AppConfig::default();
        assert_eq!(config.statuses.len(), 3);
        assert_eq!(config.statuses[0].id, "ready");
        assert!(config.status("pause").is_some());
        assert!(config.status("busy").is_none());
    }

    #[test]
    fn a_settings_file_without_the_statuses_gets_the_defaults() {
        // The statuses are a section that came later: a file written
        // before it must still open, with the three that come with the
        // app in it.
        let path = temp_path("no-statuses");
        std::fs::write(&path, r#"{"version":1,"network":{"port":5080}}"#).unwrap();
        let config = AppConfig::load(&path).unwrap();
        assert_eq!(config.statuses.len(), 3);
        assert_eq!(config.network.port, 5080);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn the_statuses_survive_a_round_trip() {
        let path = temp_path("statuses");
        let mut config = AppConfig::default();
        config.statuses.push(StatusDefinition {
            id: "in a meeting".to_string(),
            name: "In a meeting".to_string(),
            colour: "#ff0000".to_string(),
            action: StatusHook::of(&StatusAction::Dial("666".to_string())),
            start_script: "start.exe".to_string(),
            stop_script: "stop.exe".to_string(),
            ..Default::default()
        });
        config.save(&path).unwrap();

        let loaded = AppConfig::load(&path).unwrap();
        assert_eq!(loaded.statuses.len(), 4);
        let meeting = loaded.status("in a meeting").expect("the status is there");
        assert_eq!(meeting.name, "In a meeting");
        assert_eq!(meeting.colour, "#ff0000");
        assert_eq!(meeting.action.value, "666");
        assert_eq!(
            meeting.enter_action(),
            StatusAction::Dial("666".to_string()),
            "the action is built from what the status carries"
        );
        assert_eq!(meeting.start_command(), "start.exe");
        assert_eq!(meeting.stop_command(), "stop.exe");

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn a_hand_edited_status_is_put_right() {
        // A status with no name, a status with no id and two statuses
        // with one id: all of them are read, and all of them end up
        // saying one thing each.
        let path = temp_path("hand-edited");
        std::fs::write(
            &path,
            r#"{"version":1,"statuses":[
                 {"id":"","name":"Busy"},
                 {"id":"away","name":""},
                 {"id":"away","name":"Away again"}
               ]}"#,
        )
        .unwrap();
        let config = AppConfig::load(&path).unwrap();

        // The built-ins are back.
        assert!(config.status("ready").is_some());
        assert!(config.status("online").is_some());
        assert!(config.status("pause").is_some());

        // The nameless one got the name of its id.
        let away = config.status("away").expect("the status is there");
        assert_eq!(away.name, "Away");

        // The second one with the same id got an id of its own, and
        // kept the name it was given.
        let other = config
            .statuses
            .iter()
            .find(|status| status.name == "Away again")
            .expect("the second status is there");
        assert_eq!(other.id, "away-2");

        // And every id says one thing.
        let mut ids: Vec<&str> = config.statuses.iter().map(|s| s.id.as_str()).collect();
        let count = ids.len();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), count, "two statuses share one id");

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn an_empty_list_of_statuses_gets_the_defaults_back() {
        let path = temp_path("empty-statuses");
        std::fs::write(&path, r#"{"version":1,"statuses":[]}"#).unwrap();
        let config = AppConfig::load(&path).unwrap();
        assert_eq!(config.statuses.len(), 3);
        let _ = std::fs::remove_file(&path);
    }
}
