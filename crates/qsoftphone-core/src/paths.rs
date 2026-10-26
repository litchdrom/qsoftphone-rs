//! Per-OS application paths.
//!
//! Mirrors the C++ `AppPaths` module (`src/config/AppPaths.cpp`).
//! Deliberately dependency-free: the platform rules are short enough
//! to own, and the crate budget is tight.
//!
//! Setting `QSOFTPHONE_HOME` puts *everything* under one root —
//! useful for a portable install and for tests that must not touch
//! the user's real profile.

use std::path::{Path, PathBuf};

/// Overrides every other path.
pub const HOME_ENV: &str = "QSOFTPHONE_HOME";

pub const APP_NAME: &str = "qsoftphone";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppPaths {
    config_dir: PathBuf,
    data_dir: PathBuf,
    log_dir: PathBuf,
}

impl AppPaths {
    /// Detect the paths for the current platform.
    pub fn detect() -> Self {
        if let Some(root) = std::env::var_os(HOME_ENV) {
            return Self::under(root);
        }
        match std::env::consts::OS {
            "windows" => Self {
                config_dir: app_data_dir("APPDATA").join(APP_NAME),
                data_dir: app_data_dir("LOCALAPPDATA").join(APP_NAME).join("data"),
                log_dir: app_data_dir("LOCALAPPDATA").join(APP_NAME).join("logs"),
            },
            "macos" => {
                let home = home_dir().unwrap_or_else(|| PathBuf::from("."));
                let support = home
                    .join("Library")
                    .join("Application Support")
                    .join(APP_NAME);
                Self {
                    config_dir: support.clone(),
                    data_dir: support.join("data"),
                    log_dir: home.join("Library").join("Logs").join(APP_NAME),
                }
            }
            // Linux and the other unixes follow the XDG base
            // directory spec.
            _ => {
                let home = home_dir().unwrap_or_else(|| PathBuf::from("."));
                let config_home =
                    env_path("XDG_CONFIG_HOME").unwrap_or_else(|| home.join(".config"));
                let data_home =
                    env_path("XDG_DATA_HOME").unwrap_or_else(|| home.join(".local").join("share"));
                let cache_home = env_path("XDG_CACHE_HOME").unwrap_or_else(|| home.join(".cache"));
                Self {
                    config_dir: config_home.join(APP_NAME),
                    data_dir: data_home.join(APP_NAME).join("data"),
                    log_dir: cache_home.join(APP_NAME).join("logs"),
                }
            }
        }
    }

    /// A portable layout with everything under one root.
    ///
    /// ```text
    /// <root>/config/config.json
    /// <root>/data/accounts.sqlite
    /// <root>/logs/qsoftphone.log.YYYY-MM-DD
    /// ```
    pub fn under(root: impl AsRef<Path>) -> Self {
        let root = root.as_ref();
        Self {
            config_dir: root.join("config"),
            data_dir: root.join("data"),
            log_dir: root.join("logs"),
        }
    }

    pub fn config_dir(&self) -> &Path {
        &self.config_dir
    }

    pub fn data_dir(&self) -> &Path {
        &self.data_dir
    }

    pub fn log_dir(&self) -> &Path {
        &self.log_dir
    }

    /// The JSON settings file.
    pub fn config_file(&self) -> PathBuf {
        self.config_dir.join("config.json")
    }

    /// SIP accounts.
    pub fn accounts_db(&self) -> PathBuf {
        self.data_dir.join("accounts.sqlite")
    }

    /// Call history.
    pub fn history_db(&self) -> PathBuf {
        self.data_dir.join("history.sqlite")
    }

    /// Local contact book.
    pub fn contacts_db(&self) -> PathBuf {
        self.data_dir.join("contacts.sqlite")
    }

    /// Create the config, data and log directories.
    pub fn ensure(&self) -> std::io::Result<()> {
        std::fs::create_dir_all(&self.config_dir)?;
        std::fs::create_dir_all(&self.data_dir)?;
        std::fs::create_dir_all(&self.log_dir)?;
        Ok(())
    }
}

fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
}

fn env_path(key: &str) -> Option<PathBuf> {
    std::env::var_os(key)
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
}

/// A per-user application directory on Windows.
fn app_data_dir(var: &str) -> PathBuf {
    env_path(var).unwrap_or_else(|| home_dir().unwrap_or_else(|| PathBuf::from(".")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn portable_layout_is_under_one_root() {
        let p = AppPaths::under("/tmp/qs");
        assert_eq!(p.config_file(), PathBuf::from("/tmp/qs/config/config.json"));
        assert_eq!(
            p.accounts_db(),
            PathBuf::from("/tmp/qs/data/accounts.sqlite")
        );
        assert_eq!(p.history_db(), PathBuf::from("/tmp/qs/data/history.sqlite"));
        assert_eq!(p.log_dir(), Path::new("/tmp/qs/logs"));
    }

    #[test]
    fn detect_never_returns_empty_directories() {
        let p = AppPaths::detect();
        assert!(!p.config_dir().as_os_str().is_empty());
        assert!(!p.data_dir().as_os_str().is_empty());
        assert!(!p.log_dir().as_os_str().is_empty());
    }
}
