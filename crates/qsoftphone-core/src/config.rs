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
    /// `G722`.
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
            codecs: vec!["PCMU".to_string(), "PCMA".to_string(), "G722".to_string()],
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
        match std::fs::read_to_string(path) {
            Ok(text) => serde_json::from_str(&text).map_err(|e| {
                Error::Message(format!(
                    "config file {} is not valid JSON: {e}",
                    path.display()
                ))
            }),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(Error::Io(e)),
        }
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
}
