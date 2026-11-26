//! Configuration loading, default generation, and lazy `(mtime, size)` hot-reloading.
//!
//! Configuration is stored at `%APPDATA%\ortho\config.json`. If the file does not exist
//! on startup, [`ConfigManager`] creates the directory and writes the default configuration.
//! On each hotkey press, [`ConfigManager::reload_if_changed`] checks the file's modification
//! timestamp and byte size `(mtime, size)` and re-parses the JSON only when the metadata changes.
//! If the user saves malformed JSON, the error is logged, the stamp is not advanced, and the
//! last known-good [`Config`] remains active.

use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use serde::{Deserialize, Serialize};

/// Minimum allowed logical gap in pixels (`0` enables edge-to-edge tiling).
const MIN_GAP: i32 = 0;

/// Maximum allowed logical gap in pixels at 96 DPI, preventing hand-edited configs
/// from consuming the entire work area and inverting window dimensions on small displays.
const MAX_GAP: i32 = 128;

/// Default logical gap in pixels at 96 DPI.
const DEFAULT_GAP: i32 = 8;

/// Top-level user configuration persisted in `%APPDATA%\ortho\config.json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// Logical gap in pixels (at 96 DPI) around screen edges and between adjacent tiled windows.
    pub gap: i32,
    /// Global keyboard shortcuts for all window tiling and monitor actions.
    pub hotkeys: HotkeysConfig,
}

impl Default for Config {
    fn default() -> Self {
        Self { gap: DEFAULT_GAP, hotkeys: HotkeysConfig::default() }
    }
}

impl Config {
    /// Clamps numeric fields to safe bounds (`0..=128`) so hand-edited configs cannot
    /// invert window dimensions.
    #[must_use]
    pub fn sanitized(mut self) -> Self {
        self.gap = self.gap.clamp(MIN_GAP, MAX_GAP);
        self
    }
}

/// Keybinding strings for all supported tiling, restore, and multi-monitor commands.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct HotkeysConfig {
    pub left: String,
    pub right: String,
    pub top: String,
    pub bottom: String,
    pub top_left: String,
    pub top_right: String,
    pub bottom_left: String,
    pub bottom_right: String,
    pub almost: String,
    pub maximize: String,
    pub center: String,
    pub restore: String,
    pub prev_monitor: String,
    pub next_monitor: String,
}

impl Default for HotkeysConfig {
    fn default() -> Self {
        Self {
            left: String::from("ctrl+alt+left"),
            right: String::from("ctrl+alt+right"),
            top: String::from("ctrl+alt+up"),
            bottom: String::from("ctrl+alt+down"),
            top_left: String::from("ctrl+alt+u"),
            top_right: String::from("ctrl+alt+i"),
            bottom_left: String::from("ctrl+alt+j"),
            bottom_right: String::from("ctrl+alt+k"),
            almost: String::from("ctrl+alt+enter"),
            maximize: String::from("ctrl+alt+m"),
            center: String::from("ctrl+alt+c"),
            restore: String::from("ctrl+alt+backspace"),
            prev_monitor: String::from("ctrl+alt+win+left"),
            next_monitor: String::from("ctrl+alt+win+right"),
        }
    }
}

/// Manages `%APPDATA%\ortho\config.json` and performs zero-allocation `(mtime, size)` checks.
#[derive(Debug)]
pub struct ConfigManager {
    path: PathBuf,
    last_stamp: Option<(SystemTime, u64)>,
    config: Config,
}

impl ConfigManager {
    /// Loads the configuration from `%APPDATA%\ortho\config.json`, creating the default
    /// file on disk if it does not yet exist.
    #[must_use]
    pub fn load() -> Self {
        Self::load_from(default_config_path())
    }

    /// Loads the configuration from an explicit `path`, creating the default file if missing.
    #[must_use]
    pub fn load_from(path: PathBuf) -> Self {
        if !path.exists() {
            write_default_config(&path);
        }
        let config = read_and_parse(&path).unwrap_or_default();
        Self { path: path.clone(), last_stamp: file_stamp(&path), config }
    }

    /// Checks whether `config.json` has a different `(mtime, size)` stamp than the last
    /// successfully loaded configuration.
    ///
    /// Returns `true` if the file was modified, parsed cleanly, and produced a configuration
    /// different from the currently active one. If the file contains invalid JSON, retains
    /// the existing valid configuration and does not advance `last_stamp` so a subsequent fix
    /// with the same timestamp or size still reloads.
    pub fn reload_if_changed(&mut self) -> bool {
        let Some(stamp) = file_stamp(&self.path) else {
            return false;
        };
        if self.last_stamp == Some(stamp) {
            return false;
        }
        let Some(new_config) = read_and_parse(&self.path) else {
            // Do NOT advance the stamp: a later fix with the same mtime/size must still reload.
            crate::log::line("config.json: parse error, keeping last known-good config");
            return false;
        };
        self.last_stamp = Some(stamp);
        if new_config == self.config {
            return false;
        }
        crate::log::line("config.json: reloaded");
        self.config = new_config;
        true
    }

    /// Returns a reference to the currently active configuration.
    #[must_use]
    pub const fn config(&self) -> &Config {
        &self.config
    }

    /// Returns the filesystem path to `config.json`.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }
}

/// Resolves `%APPDATA%\ortho\config.json`, falling back to `./config.json` if `APPDATA` is unset.
fn default_config_path() -> PathBuf {
    env::var_os("APPDATA").map_or_else(
        || PathBuf::from("config.json"),
        |appdata| PathBuf::from(appdata).join("ortho").join("config.json"),
    )
}

/// Writes the pretty-printed default configuration to `path`, creating parent directories as needed.
fn write_default_config(path: &Path) {
    if let Some(parent) = path.parent()
        && let Err(e) = fs::create_dir_all(parent)
    {
        crate::log::line(&format!("create config dir failed: {e}"));
        return;
    }
    match serde_json::to_string_pretty(&Config::default()) {
        Ok(json) => {
            if let Err(e) = fs::write(path, json) {
                crate::log::line(&format!("write default config failed: {e}"));
            }
        }
        Err(e) => crate::log::line(&format!("serialize default config failed: {e}")),
    }
}

/// Reads and deserializes [`Config`] from `path`, clamping `gap` to valid bounds.
fn read_and_parse(path: &Path) -> Option<Config> {
    let raw = fs::read_to_string(path).ok()?;
    let parsed: Config = serde_json::from_str(&raw).ok()?;
    Some(parsed.sanitized())
}

/// Returns `(mtime, size)` so timestamp-preserving or same-second edits are noticed.
fn file_stamp(path: &Path) -> Option<(SystemTime, u64)> {
    let meta = fs::metadata(path).ok()?;
    Some((meta.modified().ok()?, meta.len()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn partial_json_falls_back_to_ortho_defaults() {
        let raw = r#"{ "gap": 14 }"#;
        let parsed = serde_json::from_str::<Config>(raw).unwrap_or_default().sanitized();
        assert_eq!(parsed.gap, 14);
        assert_eq!(parsed.hotkeys.left, "ctrl+alt+left");
        assert_eq!(parsed.hotkeys.almost, "ctrl+alt+enter");
        assert_eq!(parsed.hotkeys.next_monitor, "ctrl+alt+win+right");
    }

    #[test]
    fn negative_or_excessive_gap_is_clamped() {
        let neg = Config { gap: -50, hotkeys: HotkeysConfig::default() }.sanitized();
        let huge = Config { gap: 9999, hotkeys: HotkeysConfig::default() }.sanitized();
        assert_eq!(neg.gap, MIN_GAP);
        assert_eq!(huge.gap, MAX_GAP);
    }

    #[test]
    fn reload_if_changed_recovers_after_bad_json_even_with_identical_stamp() {
        let path = env::temp_dir().join(format!("ortho_test_cfg_{}.json", std::process::id()));
        let _ = fs::remove_file(&path);
        let mut mgr = ConfigManager::load_from(path.clone());
        assert_eq!(mgr.config().gap, DEFAULT_GAP);
        assert!(!mgr.reload_if_changed());

        // 1. Valid edit reloads cleanly (12 bytes, distinct from the 13-byte payloads below).
        fs::write(&path, r#"{ "gap": 7 }"#).unwrap();
        assert!(mgr.reload_if_changed());
        assert_eq!(mgr.config().gap, 7);

        // 2. Save malformed JSON of exact length 13 (`{ "gap": xx }`) and capture its stamp.
        let bad_payload = r#"{ "gap": xx }"#;
        fs::write(&path, bad_payload).unwrap();
        let bad_mtime = fs::metadata(&path).unwrap().modified().unwrap();
        assert!(!mgr.reload_if_changed());
        assert_eq!(mgr.config().gap, 7);

        // 3. Fix the JSON using the exact same byte length (13 bytes) and force the exact
        // same `mtime` as the broken write: because `last_stamp` was not consumed on error,
        // `reload_if_changed` still detects and applies the fix.
        let fixed_payload = r#"{ "gap": 32 }"#;
        assert_eq!(bad_payload.len(), fixed_payload.len());
        fs::write(&path, fixed_payload).unwrap();
        if let Ok(file) = fs::OpenOptions::new().write(true).open(&path) {
            let _ = file.set_modified(bad_mtime);
        }
        assert!(mgr.reload_if_changed());
        assert_eq!(mgr.config().gap, 32);
        let _ = fs::remove_file(&path);
    }
}
