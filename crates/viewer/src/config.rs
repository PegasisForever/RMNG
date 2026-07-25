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
pub(crate) fn parse_cmd_is_ctrl_env(v: Option<&str>) -> Option<bool> {
    let s = v?.trim();
    if s.is_empty() {
        return None;
    }
    Some(!matches!(s.to_ascii_lowercase().as_str(), "0" | "false" | "no" | "off"))
}

/// The effective Cmd↔Ctrl setting: `RMNG_CMD_IS_CTRL` wins, else the persisted config, which
/// defaults to enabled.
pub fn cmd_is_ctrl() -> bool {
    parse_cmd_is_ctrl_env(std::env::var("RMNG_CMD_IS_CTRL").ok().as_deref())
        .unwrap_or_else(|| load().cmd_is_ctrl)
}

pub fn config_path() -> PathBuf {
    let base = std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from).unwrap_or_else(|| {
        let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default();
        home.join(".config")
    });
    base.join("rmng-viewer").join("config.json")
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

    #[test]
    fn default_config_has_the_swap_on() {
        assert!(Config { server_addr: DEFAULT_SERVER_ADDR.to_string(), cmd_is_ctrl: true }.cmd_is_ctrl);
    }
}
