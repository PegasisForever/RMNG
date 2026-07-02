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
        Ok(s) => {
            let mut cfg: AppConfig = serde_json::from_str(&s)
                .with_context(|| format!("parsing {}", path.display()))?;
            // Legacy fields (serde ignores them at parse): fold what's still useful
            // into the current shape and rewrite the file once, so dead secrets
            // (long-lived clone tokens, per-workspace Linear keys) don't linger on disk.
            let raw = serde_json::from_str::<serde_json::Value>(&s).unwrap_or_default();
            if migrate_legacy(&raw, &mut cfg) {
                tracing::info!("migrating legacy config fields in {}", path.display());
                save(&cfg)?;
            }
            cfg
        }
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

/// Fold legacy config fields into the current shape; true = the file must be
/// rewritten. Legacy `envPresets` (env-only presets, pre Linear unification) seed
/// `presets` (no labels/key — the operator adds those in Settings). Legacy `linear`
/// workspace keys (now per-preset) and `cloneAccounts` long-lived tokens (dead since
/// the single-token model) are dropped; the rewrite scrubs them from disk.
fn migrate_legacy(raw: &serde_json::Value, cfg: &mut AppConfig) -> bool {
    let non_empty = |k: &str| match raw.get(k) {
        Some(serde_json::Value::Array(a)) => !a.is_empty(),
        Some(serde_json::Value::Object(o)) => !o.is_empty(),
        _ => false,
    };
    if cfg.presets.is_empty() {
        if let Some(rows) = raw.get("envPresets").and_then(|v| v.as_array()) {
            for r in rows {
                let Some(name) = r.get("name").and_then(|v| v.as_str()) else { continue };
                let vars = r
                    .get("vars")
                    .and_then(|v| serde_json::from_value(v.clone()).ok())
                    .unwrap_or_default();
                cfg.presets.push(wire::Preset {
                    name: name.to_string(),
                    labels: Vec::new(),
                    linear_key: String::new(),
                    vars,
                });
            }
        }
    }
    if non_empty("linear") {
        tracing::info!("dropping legacy per-workspace Linear keys (now per-preset — re-enter in Settings)");
    }
    non_empty("envPresets") || non_empty("linear") || non_empty("cloneAccounts")
}

#[cfg(test)]
mod tests {
    use super::*;
    use wire::CloneGroup;

    #[test]
    fn merge_preserves_blank_secrets_and_applies_changes() {
        let mut base = AppConfig::default();
        base.proxmox.ssh = "root@node".into();
        // The UI sends back blanks for unchanged secrets, plus a real change.
        let incoming = serde_json::json!({
            "listen": { "web": 9100 },
            "proxmox": { "ssh": "", "hostnamePrefix": "clone-" },
        });
        let merged = merge_update(&base, incoming).unwrap();
        assert_eq!(merged.listen.web, 9100); // changed
        assert_eq!(merged.listen.video, 9001); // untouched (merge kept it)
        assert_eq!(merged.proxmox.ssh, "root@node"); // blank secret preserved
        assert_eq!(merged.proxmox.hostname_prefix, "clone-"); // non-secret changed
    }

    #[test]
    fn merge_presets_by_name() {
        use wire::{EnvVar, Preset};
        let mut base = AppConfig::default();
        base.presets = vec![
            Preset { name: "med".into(), linear_key: "OLD-MED".into(), ..Default::default() },
            Preset { name: "gone".into(), linear_key: "OLD-GONE".into(), ..Default::default() },
        ];
        // UI sends the full list: blank linearKey = keep stored, new row = added,
        // omitted row ("gone") = deleted (with its key). Labels/vars replace.
        let incoming = serde_json::json!({
            "presets": [
                { "name": "med", "labels": [" Backend ", ""], "linearKey": "",
                  "vars": [{ "key": "A", "value": "1" }] },
                { "name": "new", "labels": [], "linearKey": "NEW-KEY", "vars": [] },
            ],
        });
        let merged = merge_update(&base, incoming).unwrap();
        assert_eq!(merged.presets.len(), 2);
        assert_eq!(merged.presets[0].linear_key, "OLD-MED"); // blank kept stored
        assert_eq!(merged.presets[0].labels, vec!["Backend"]); // trimmed, blanks dropped
        assert_eq!(merged.presets[0].vars, vec![EnvVar { key: "A".into(), value: "1".into() }]);
        assert_eq!(merged.presets[1].name, "new");
        assert_eq!(merged.presets[1].linear_key, "NEW-KEY");
        assert!(!merged.presets.iter().any(|p| p.name == "gone")); // omitted → deleted
        // No `presets` field at all → unchanged.
        let untouched = merge_update(&base, serde_json::json!({})).unwrap();
        assert_eq!(untouched.presets, base.presets);
    }

    #[test]
    fn migrate_legacy_folds_old_fields() {
        // envPresets seed presets (no labels/key); linear + cloneAccounts just flag a rewrite.
        let raw = serde_json::json!({
            "envPresets": [{ "name": "old", "vars": [{ "key": "A", "value": "1" }] }],
            "linear": [{ "name": "we", "key": "K" }],
        });
        let mut cfg = AppConfig::default();
        assert!(migrate_legacy(&raw, &mut cfg));
        assert_eq!(cfg.presets.len(), 1);
        assert_eq!(cfg.presets[0].name, "old");
        assert!(cfg.presets[0].labels.is_empty() && cfg.presets[0].linear_key.is_empty());
        assert_eq!(cfg.presets[0].vars[0].key, "A");

        // Legacy object-shaped `linear` also counts; existing presets are never clobbered.
        let raw = serde_json::json!({ "linear": { "we": "K1" }, "envPresets": [{ "name": "x" }] });
        let mut cfg = AppConfig::default();
        cfg.presets = vec![wire::Preset { name: "kept".into(), ..Default::default() }];
        assert!(migrate_legacy(&raw, &mut cfg));
        assert_eq!(cfg.presets.len(), 1);
        assert_eq!(cfg.presets[0].name, "kept");

        // Fully-migrated file → no rewrite.
        let raw = serde_json::json!({ "presets": [{ "name": "p" }] });
        let mut cfg = AppConfig::default();
        assert!(!migrate_legacy(&raw, &mut cfg));
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
/// "unchanged"** (so the redacted UI can send back blanks without wiping stored
/// values); `presets` merge by name (a blank `linearKey` keeps the stored one).
pub fn merge_update(base: &AppConfig, incoming: serde_json::Value) -> Result<AppConfig> {
    let mut cur = serde_json::to_value(base)?;
    // Pull the secret-bearing list aside for key-wise merge (generic merge would replace).
    let incoming_presets = incoming.get("presets").cloned();
    deep_merge(&mut cur, &incoming);
    let mut merged: AppConfig = serde_json::from_value(cur)?;
    if let Some(serde_json::Value::Array(rows)) = incoming_presets {
        merged.presets = merge_presets(&base.presets, &rows);
    }
    Ok(merged)
}

/// Merge the UI's preset rows by name: a blank `linearKey` keeps the stored key of
/// the same-named preset (write-only secret); labels/vars are replaced from the row;
/// a preset absent from the list is deleted (along with its key).
fn merge_presets(base: &[wire::Preset], rows: &[serde_json::Value]) -> Vec<wire::Preset> {
    let mut out: Vec<wire::Preset> = Vec::new();
    for r in rows {
        let Some(name) = r.get("name").and_then(|v| v.as_str()) else { continue };
        let name = name.trim().to_string();
        if name.is_empty() || out.iter().any(|p| p.name == name) {
            continue;
        }
        let labels = r
            .get("labels")
            .and_then(|v| v.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str())
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default();
        let vars: Vec<wire::EnvVar> = r
            .get("vars")
            .and_then(|v| serde_json::from_value(v.clone()).ok())
            .unwrap_or_default();
        let sent = r.get("linearKey").and_then(|v| v.as_str()).unwrap_or("");
        let linear_key = if sent.is_empty() {
            base.iter().find(|p| p.name == name).map(|p| p.linear_key.clone()).unwrap_or_default()
        } else {
            sent.to_string()
        };
        out.push(wire::Preset { name, labels, linear_key, vars });
    }
    out
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

