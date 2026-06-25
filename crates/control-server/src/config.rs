//! Config loading. `config.json` (path via `RMNG_CONFIG`, else `./config.json`)
//! holds every setting incl. secrets; missing → defaults. The Settings UI
//! (`/api/config`, Phase 2) is the intended editor — this is just load/save.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use wire::AppConfig;

pub fn config_path() -> PathBuf {
    std::env::var_os("RMNG_CONFIG")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("config.json"))
}

pub fn load() -> Result<AppConfig> {
    let path = config_path();
    match std::fs::read_to_string(&path) {
        Ok(s) => serde_json::from_str(&s)
            .with_context(|| format!("parsing {}", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            tracing::info!("no {} — using defaults", path.display());
            Ok(AppConfig::default())
        }
        Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wire::CloneAccount;

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
}

/// Resolve the state.json path: `KASM_STATE_FILE` override (matches the Bun
/// server) else `<data_dir>/state.json`.
pub fn state_path(cfg: &AppConfig) -> PathBuf {
    if let Some(p) = std::env::var_os("KASM_STATE_FILE") {
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
/// them); `cloneAccounts` merge by email (a blank token keeps the stored one).
pub fn merge_update(base: &AppConfig, incoming: serde_json::Value) -> Result<AppConfig> {
    let mut cur = serde_json::to_value(base)?;
    // Pull cloneAccounts aside for email-wise merge (generic merge would replace).
    let incoming_accounts = incoming.get("cloneAccounts").cloned();
    deep_merge(&mut cur, &incoming);
    let mut merged: AppConfig = serde_json::from_value(cur)?;
    if let Some(serde_json::Value::Array(rows)) = incoming_accounts {
        merged.clone_accounts = merge_clone_accounts(&base.clone_accounts, &rows);
    }
    Ok(merged)
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
