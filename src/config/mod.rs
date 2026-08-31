//! The receiver's settings file.
//!
//! Portable by default: `nearscreen-receiver.json` next to the binary, so a
//! copied folder carries its settings with it. When that directory is not
//! writable (Program Files, a read-only volume) the user's own config
//! directory is used instead.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use log::{debug, warn};
use serde::{Deserialize, Serialize};

use crate::net::DEFAULT_PORT;

/// Name of the settings file, in both locations.
pub const FILE_NAME: &str = "nearscreen-receiver.json";

/// A phone the person has already accepted.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AllowedDevice {
    /// `Hello.id` — stable for this app on that phone.
    pub id: String,
    /// Whatever the phone called itself when it was accepted, for the settings UI.
    #[serde(default)]
    pub name: String,
}

/// Everything the receiver remembers between runs. Every field is optional in
/// the file; a missing or unreadable file simply means defaults.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub port: u16,
    pub allowed_devices: Vec<AllowedDevice>,
    pub start_at_login: bool,
    /// Address of the network interface to advertise, when there are several.
    pub preferred_interface: Option<String>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            port: DEFAULT_PORT,
            allowed_devices: Vec::new(),
            start_at_login: false,
            preferred_interface: None,
        }
    }
}

impl Config {
    /// Loads the settings, falling back to defaults on anything unreadable —
    /// a broken file must never keep the receiver from starting.
    pub fn load() -> Self {
        let path = Self::path();
        match Self::load_from(&path) {
            Ok(Some(config)) => {
                debug!("settings loaded from {}", path.display());
                config
            }
            Ok(None) => Self::default(),
            Err(e) => {
                warn!("{} is unusable ({e}); using defaults", path.display());
                Self::default()
            }
        }
    }

    /// `Ok(None)` when the file simply is not there yet.
    pub fn load_from(path: &Path) -> Result<Option<Self>> {
        let text = match fs::read_to_string(path) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e).context("cannot read the settings file"),
        };
        let config = serde_json::from_str(&text).context("cannot parse the settings file")?;
        Ok(Some(config))
    }

    pub fn save(&self) -> Result<()> {
        self.save_to(&Self::path())
    }

    pub fn save_to(&self, path: &Path) -> Result<()> {
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir).with_context(|| format!("cannot create {}", dir.display()))?;
        }
        let text = serde_json::to_string_pretty(self)?;
        fs::write(path, text).with_context(|| format!("cannot write {}", path.display()))
    }

    /// Where the settings live: next to the binary if that is writable, else
    /// in the user's config directory. An existing file wins over both.
    pub fn path() -> PathBuf {
        let portable = portable_path();
        if let Some(path) = &portable {
            if path.exists() {
                return path.clone();
            }
        }
        let user = user_path();
        if let Some(path) = &user {
            if path.exists() {
                return path.clone();
            }
        }
        match portable {
            Some(path) if is_writable_dir(path.parent()) => path,
            _ => user.unwrap_or_else(|| PathBuf::from(FILE_NAME)),
        }
    }

    pub fn is_allowed(&self, id: &str) -> bool {
        !id.is_empty() && self.allowed_devices.iter().any(|d| d.id == id)
    }

    /// Remembers a phone the person chose to always allow.
    pub fn allow(&mut self, id: &str, name: &str) {
        if id.is_empty() {
            return;
        }
        match self.allowed_devices.iter_mut().find(|d| d.id == id) {
            Some(device) => device.name = name.to_string(),
            None => self.allowed_devices.push(AllowedDevice {
                id: id.to_string(),
                name: name.to_string(),
            }),
        }
    }
}

fn portable_path() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    Some(exe.parent()?.join(FILE_NAME))
}

fn user_path() -> Option<PathBuf> {
    let dir = if cfg!(windows) {
        PathBuf::from(std::env::var_os("APPDATA")?).join("Nearscreen")
    } else if cfg!(target_os = "macos") {
        PathBuf::from(std::env::var_os("HOME")?).join("Library/Application Support/Nearscreen")
    } else {
        match std::env::var_os("XDG_CONFIG_HOME") {
            Some(base) => PathBuf::from(base).join("nearscreen"),
            None => PathBuf::from(std::env::var_os("HOME")?).join(".config/nearscreen"),
        }
    };
    Some(dir.join(FILE_NAME))
}

/// Probes by writing, because Windows permissions cannot be read off metadata.
fn is_writable_dir(dir: Option<&Path>) -> bool {
    let Some(dir) = dir else {
        return false;
    };
    let probe = dir.join(".nearscreen-write-test");
    match fs::write(&probe, b"") {
        Ok(()) => {
            let _ = fs::remove_file(&probe);
            true
        }
        Err(_) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_file(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join("nearscreen-receiver-tests");
        fs::create_dir_all(&dir).unwrap();
        dir.join(name)
    }

    #[test]
    fn missing_file_is_not_an_error() {
        let path = temp_file("does-not-exist.json");
        let _ = fs::remove_file(&path);
        assert!(Config::load_from(&path).unwrap().is_none());
    }

    #[test]
    fn saves_and_loads_back() {
        let path = temp_file("roundtrip.json");
        let mut config = Config {
            port: 19913,
            ..Config::default()
        };
        config.allow("VENDOR-ID-1", "Ira iPhone");
        config.save_to(&path).unwrap();

        let loaded = Config::load_from(&path).unwrap().unwrap();
        assert_eq!(loaded, config);
        assert!(loaded.is_allowed("VENDOR-ID-1"));
        assert!(!loaded.is_allowed("SOMEONE-ELSE"));
    }

    #[test]
    fn a_partial_file_keeps_the_defaults_for_the_rest() {
        let path = temp_file("partial.json");
        fs::write(&path, r#"{"port":12345}"#).unwrap();
        let config = Config::load_from(&path).unwrap().unwrap();
        assert_eq!(config.port, 12345);
        assert!(!config.start_at_login);
        assert!(config.allowed_devices.is_empty());
    }

    #[test]
    fn allowing_the_same_phone_twice_updates_its_name() {
        let mut config = Config::default();
        config.allow("ID", "iPhone");
        config.allow("ID", "Ira iPhone");
        assert_eq!(config.allowed_devices.len(), 1);
        assert_eq!(config.allowed_devices[0].name, "Ira iPhone");
    }

    #[test]
    fn an_empty_id_is_never_allowed() {
        let mut config = Config::default();
        config.allow("", "nameless");
        assert!(config.allowed_devices.is_empty());
        assert!(!config.is_allowed(""));
    }
}
