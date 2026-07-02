//! Config loading. `config.json` (path via `RMNG_CONFIG`, else `./config.json`)
//! holds every setting incl. secrets; missing → defaults. The Settings UI
//! (`/api/config`, Phase 2) is the intended editor — this is just load/save.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use wire::{AppConfig, ChromaMode};

pub fn config_path() -> PathBuf {
    std::env::var_os("RMNG_CONFIG")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("config.json"))
}

pub fn load() -> Result<AppConfig> {
    let path = config_path();
    let mut cfg = match std::fs::read_to_string(&path) {
        Ok(s) => serde_json::from_str(&s)
            .with_context(|| format!("parsing {}", path.display()))?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            tracing::info!("no {} — using defaults", path.display());
            AppConfig::default()
        }
        Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
    };
    // `RMNG_CHROMA` overrides the file/default chroma mode at load time.
    if let Ok(v) = std::env::var("RMNG_CHROMA") {
        match ChromaMode::from_env_value(&v) {
            Some(m) => cfg.chroma = m,
            None => tracing::warn!("ignoring unrecognized RMNG_CHROMA={v:?}"),
        }
    }
    Ok(cfg)
}

#[cfg(test)]
mod tests {
    use super::*;
    use wire::{CloneAccount, CloneGroup};

    #[test]
    fn merge_preserves_blank_secrets_and_applies_changes() {
        let mut base = AppConfig::default();
        base.proxmox.ssh = "root@node".into();
        base.clone_accounts = vec![CloneAccount {
            email: "a@b".into(),
            long_lived_token: "LONG".into(),
            refresh_token: "REF".into(),
        }];
        // The UI sends back blanks for unchanged secrets, plus a real change.
        let incoming = serde_json::json!({
            "listen": { "web": 9100 },
            "proxmox": { "ssh": "", "hostnamePrefix": "clone-" },
            "cloneAccounts": [{ "email": "a@b", "longLivedToken": "", "refreshToken": "NEWREF" }],
        });
        let merged = merge_update(&base, incoming).unwrap();
        assert_eq!(merged.listen.web, 9100); // changed
        assert_eq!(merged.listen.video, 9001); // untouched (merge kept it)
        assert_eq!(merged.proxmox.ssh, "root@node"); // blank secret preserved
        assert_eq!(merged.proxmox.hostname_prefix, "clone-"); // non-secret changed
        assert_eq!(merged.clone_accounts[0].long_lived_token, "LONG"); // blank kept
        assert_eq!(merged.clone_accounts[0].refresh_token, "NEWREF"); // changed
    }

    #[test]
    fn merge_linear_keys_by_name() {
        use wire::{LinearConfig, LinearKey};
        let mut base = AppConfig::default();
        base.linear = LinearConfig(vec![
            LinearKey { name: "we".into(), key: "OLD-WE".into() },
            LinearKey { name: "dev".into(), key: "OLD-DEV".into() },
        ]);
        // UI sends the full list: blank key = unchanged, new row = added,
        // omitted row ("dev") = deleted. Names are normalized to lowercase.
        let incoming = serde_json::json!({
            "linear": [
                { "name": "we", "key": "" },
                { "name": "OPS", "key": "NEW-OPS" },
            ],
        });
        let merged = merge_update(&base, incoming).unwrap();
        assert_eq!(merged.linear.names(), vec!["we", "ops"]);
        assert_eq!(merged.linear.key_for("we"), Some("OLD-WE")); // blank kept stored
        assert_eq!(merged.linear.key_for("ops"), Some("NEW-OPS"));
        assert_eq!(merged.linear.key_for("dev"), None); // omitted → deleted
        // No `linear` field at all → unchanged.
        let untouched = merge_update(&base, serde_json::json!({})).unwrap();
        assert_eq!(untouched.linear, base.linear);
    }

    #[test]
    fn merge_replaces_clone_groups_wholesale() {
        // The editor always sends the full group list, so a plain array replace is right.
        let mut base = AppConfig::default();
        base.clone_groups = vec![CloneGroup { name: "old".into(), accounts: vec!["a@b".into()] }];
        let incoming = serde_json::json!({
            "cloneGroups": [{ "name": "team", "accounts": ["a@b", "c@d"] }],
        });
        let merged = merge_update(&base, incoming).unwrap();
        assert_eq!(merged.clone_groups.len(), 1);
        assert_eq!(merged.clone_groups[0].name, "team");
        assert_eq!(merged.clone_groups[0].accounts, vec!["a@b".to_string(), "c@d".to_string()]);
        // An empty array clears all groups.
        let cleared = merge_update(&merged, serde_json::json!({ "cloneGroups": [] })).unwrap();
        assert!(cleared.clone_groups.is_empty());
    }
}

/// Resolve the state.json path: `RMNG_STATE_FILE` override else `<data_dir>/state.json`.
pub fn state_path(cfg: &AppConfig) -> PathBuf {
    if let Some(p) = std::env::var_os("RMNG_STATE_FILE") {
        return PathBuf::from(p);
    }
    Path::new(&cfg.data_dir).join("state.json")
}

/// Atomically write `config.json` at 0600 (it holds secrets).
pub fn save(cfg: &AppConfig) -> Result<()> {
    let path = config_path();
    if let Some(d) = path.parent() {
        std::fs::create_dir_all(d).ok();
    }
    let tmp = path.with_extension(format!("tmp.{}", std::process::id()));
    let mut body = serde_json::to_string_pretty(cfg)?;
    body.push('\n');
    std::fs::write(&tmp, body)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600)).ok();
    }
    std::fs::rename(&tmp, &path)?;
    Ok(())
}

/// Merge a partial config update onto `base`, returning the new config. Rules:
/// non-secret fields are replaced; **empty-string scalars are treated as
/// "unchanged"** (so the redacted UI can send back blank secrets without wiping
/// them); `cloneAccounts` merge by email and `linear` by workspace name (a blank
/// token/key keeps the stored one).
pub fn merge_update(base: &AppConfig, incoming: serde_json::Value) -> Result<AppConfig> {
    let mut cur = serde_json::to_value(base)?;
    // Pull the secret-bearing lists aside for key-wise merge (generic merge would replace).
    let incoming_accounts = incoming.get("cloneAccounts").cloned();
    let incoming_linear = incoming.get("linear").cloned();
    deep_merge(&mut cur, &incoming);
    let mut merged: AppConfig = serde_json::from_value(cur)?;
    if let Some(serde_json::Value::Array(rows)) = incoming_accounts {
        merged.clone_accounts = merge_clone_accounts(&base.clone_accounts, &rows);
    }
    if let Some(serde_json::Value::Array(rows)) = incoming_linear {
        merged.linear = merge_linear_keys(&base.linear, &rows);
    }
    Ok(merged)
}

/// Merge the UI's Linear-key rows by workspace name: a blank key keeps the stored
/// one (write-only secret); a row absent from the list deletes that workspace.
fn merge_linear_keys(base: &wire::LinearConfig, rows: &[serde_json::Value]) -> wire::LinearConfig {
    let mut keys: Vec<wire::LinearKey> = Vec::new();
    for r in rows {
        let Some(name) = r.get("name").and_then(|v| v.as_str()) else { continue };
        let name = name.trim().to_ascii_lowercase();
        if name.is_empty() || keys.iter().any(|k| k.name == name) {
            continue;
        }
        let sent = r.get("key").and_then(|v| v.as_str()).unwrap_or("");
        let key = if sent.is_empty() {
            base.key_for(&name).unwrap_or_default().to_string()
        } else {
            sent.to_string()
        };
        keys.push(wire::LinearKey { name, key });
    }
    wire::LinearConfig(keys)
}

/// Overlay `src` onto `dst`. Objects merge recursively; arrays + scalars replace —
/// except an empty-string scalar in `src` is skipped (keeps `dst`).
fn deep_merge(dst: &mut serde_json::Value, src: &serde_json::Value) {
    use serde_json::Value;
    match (dst, src) {
        (Value::Object(d), Value::Object(s)) => {
            for (k, v) in s {
                deep_merge(d.entry(k.clone()).or_insert(Value::Null), v);
            }
        }
        (d, Value::String(s)) if s.is_empty() => {
            // empty string = "unchanged" (preserve the stored value)
            let _ = d;
        }
        (d, s) => *d = s.clone(),
    }
}

fn merge_clone_accounts(
    base: &[wire::CloneAccount],
    rows: &[serde_json::Value],
) -> Vec<wire::CloneAccount> {
    rows.iter()
        .filter_map(|r| {
            let email = r.get("email")?.as_str()?.to_string();
            let prev = base.iter().find(|a| a.email == email);
            let pick = |key: &str| -> String {
                let v = r.get(key).and_then(|x| x.as_str()).unwrap_or("");
                if v.is_empty() {
                    prev.map(|p| field(p, key)).unwrap_or_default()
                } else {
                    v.to_string()
                }
            };
            Some(wire::CloneAccount {
                email,
                long_lived_token: pick("longLivedToken"),
                refresh_token: pick("refreshToken"),
            })
        })
        .collect()
}

fn field(a: &wire::CloneAccount, key: &str) -> String {
    match key {
        "longLivedToken" => a.long_lived_token.clone(),
        "refreshToken" => a.refresh_token.clone(),
        _ => String::new(),
    }
}
