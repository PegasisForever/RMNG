//! The viewer's only persisted config: the server address (`host:port` for the
//! port-1 video/input/clipboard connection), stored in
//! `~/.config/rmng-viewer/config.json`.
//!
//! This is the source of truth, replacing the old `RMNG_VIDEO` env var: the
//! title-bar Settings button edits it at runtime and persists here. `RMNG_VIDEO`
//! only seeds the default the very first run (before any config file exists), so
//! existing setups keep working; once a config file is written it wins.

use std::path::PathBuf;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

/// Fallback address when neither a config file nor `RMNG_VIDEO` provides one.
pub const DEFAULT_SERVER_ADDR: &str = "127.0.0.1:9001";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    pub server_addr: String,
    /// macOS only: swap Cmd and Control on the wire, so Mac muscle memory (Cmd+C, Cmd+T) reaches
    /// the remote GNOME session as Ctrl. `serde(default)` so a config.json written before this
    /// field existed still loads. Ignored on Linux.
    #[serde(default = "default_cmd_is_ctrl")]
    pub cmd_is_ctrl: bool,
}

fn default_cmd_is_ctrl() -> bool {
    true
}

impl Default for Config {
    fn default() -> Self {
        // Seed from RMNG_VIDEO if present (legacy override), else the default.
        let server_addr = std::env::var("RMNG_VIDEO").unwrap_or_else(|_| DEFAULT_SERVER_ADDR.to_string());
        Config { server_addr, cmd_is_ctrl: default_cmd_is_ctrl() }
    }
}

/// Parse an `RMNG_CMD_IS_CTRL` value. `None` means "no opinion" (unset or blank) — the caller
/// falls through to the persisted config rather than treating absence as "off".
///
/// The parser itself is platform-neutral, so its tests run everywhere.
pub fn parse_cmd_is_ctrl_env(v: Option<&str>) -> Option<bool> {
    let s = v?.trim();
    if s.is_empty() {
        return None;
    }
    Some(!matches!(s.to_ascii_lowercase().as_str(), "0" | "false" | "no" | "off"))
}

/// The effective Cmd↔Ctrl setting: `RMNG_CMD_IS_CTRL` wins, else the persisted config, which
/// defaults to enabled. Consulted by the macOS keyboard paths.
pub fn cmd_is_ctrl() -> bool {
    parse_cmd_is_ctrl_env(std::env::var("RMNG_CMD_IS_CTRL").ok().as_deref())
        .unwrap_or_else(|| load().cmd_is_ctrl)
}

pub fn config_path() -> PathBuf {
    config_base().join("rmng-viewer").join("config.json")
}

/// The directory that holds this user's application config.
///
/// An explicit `XDG_CONFIG_HOME` wins on every platform — it is how a portable or test install
/// relocates the file. Otherwise Unix uses `~/.config` and Windows uses `%APPDATA%`.
///
/// Windows needs its own branch rather than the Unix fallback: `HOME` is not set for a process
/// started from Explorer, the Start menu, or a shortcut — only a POSIX-ish shell (Git Bash,
/// MSYS2) defines it. With `HOME` unset the fallback produces the *relative* path
/// `.config\rmng-viewer\config.json`, which resolves against the working directory, so the
/// server address would be written next to wherever the viewer happened to be launched from
/// and silently fail to load the next time it was launched from anywhere else.
fn config_base() -> PathBuf {
    if let Some(xdg) = std::env::var_os("XDG_CONFIG_HOME") {
        return PathBuf::from(xdg);
    }
    #[cfg(target_os = "windows")]
    {
        if let Some(appdata) = std::env::var_os("APPDATA") {
            return PathBuf::from(appdata);
        }
        // %APPDATA% is set for every interactive logon; this only covers an unusual service or
        // stripped environment, where %USERPROFILE% still gives the canonical location.
        if let Some(profile) = std::env::var_os("USERPROFILE") {
            return PathBuf::from(profile).join("AppData").join("Roaming");
        }
    }
    let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default();
    home.join(".config")
}

/// Load the persisted config, falling back to defaults (which seed from
/// `RMNG_VIDEO`) when the file is absent or unreadable.
pub fn load() -> Config {
    let path = config_path();
    match std::fs::read_to_string(&path) {
        Ok(text) => serde_json::from_str(&text).unwrap_or_else(|e| {
            tracing::warn!("invalid config at {path:?}: {e}; using defaults");
            Config::default()
        }),
        Err(_) => Config::default(),
    }
}

pub fn save(config: &Config) -> Result<()> {
    let path = config_path();
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).with_context(|| format!("create {dir:?}"))?;
    }
    let text = serde_json::to_string_pretty(config).context("serialize config")?;
    std::fs::write(&path, text).with_context(|| format!("write {path:?}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn env_override_parses_truthy_and_falsy() {
        assert_eq!(parse_cmd_is_ctrl_env(Some("0")), Some(false));
        assert_eq!(parse_cmd_is_ctrl_env(Some("false")), Some(false));
        assert_eq!(parse_cmd_is_ctrl_env(Some("FALSE")), Some(false));
        assert_eq!(parse_cmd_is_ctrl_env(Some("no")), Some(false));
        assert_eq!(parse_cmd_is_ctrl_env(Some("off")), Some(false));
        assert_eq!(parse_cmd_is_ctrl_env(Some("1")), Some(true));
        assert_eq!(parse_cmd_is_ctrl_env(Some("true")), Some(true));
    }

    /// Unset or blank means "no opinion" — fall through to the persisted config rather than
    /// silently disabling the swap.
    #[test]
    fn env_override_absent_or_blank_has_no_opinion() {
        assert_eq!(parse_cmd_is_ctrl_env(None), None);
        assert_eq!(parse_cmd_is_ctrl_env(Some("")), None);
        assert_eq!(parse_cmd_is_ctrl_env(Some("   ")), None);
    }

    /// A config.json written before this field existed must still load, with the swap on.
    #[test]
    fn legacy_config_without_the_field_still_loads_with_swap_on() {
        let c: Config = serde_json::from_str(r#"{"server_addr":"10.0.0.100:9001"}"#)
            .expect("legacy config must deserialize");
        assert_eq!(c.server_addr, "10.0.0.100:9001");
        assert!(c.cmd_is_ctrl, "the swap defaults on");
    }

    /// The config has to land in the same place no matter how the viewer was started. On
    /// Windows the Unix `HOME` fallback yields a path relative to the working directory, so the
    /// address saved from the Settings dialog would vanish the next time the viewer was
    /// launched from anywhere else. Absolute is the property that rules that out.
    ///
    /// Windows-only on purpose: on Linux this asserts a property of the *environment* (`HOME`
    /// being set), not of this code, and a container that runs the suite without one would fail
    /// it for a reason unrelated to the change.
    #[cfg(target_os = "windows")]
    #[test]
    fn the_config_path_does_not_depend_on_the_working_directory() {
        let path = config_path();
        assert!(path.is_absolute(), "config path must be absolute, got {path:?}");
    }

    #[test]
    fn default_config_has_the_swap_on() {
        assert!(Config { server_addr: DEFAULT_SERVER_ADDR.to_string(), cmd_is_ctrl: true }.cmd_is_ctrl);
    }
}
