//! Config loading. `./config.json` is the single source of truth: it holds every
//! setting incl. secrets (no `RMNG_*` env overrides); missing → defaults. The Settings
//! UI (`/api/config`) is the intended editor — this is load/save + merge/category logic.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use wire::AppConfig;

pub fn config_path() -> PathBuf {
    PathBuf::from("config.json")
}

pub fn load() -> Result<AppConfig> {
    let path = config_path();
    let cfg = match std::fs::read_to_string(&path) {
        Ok(s) => {
            // Retired keys are dropped at parse, never an error; missing keys
            // take their defaults, so old files always load.
            serde_json::from_str(&s).with_context(|| format!("parsing {}", path.display()))?
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            tracing::info!("no {} — using defaults", path.display());
            AppConfig::default()
        }
        Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
    };
    Ok(cfg)
}

/// The active layout preset of a config file from a build that kept a layout list
/// (`layoutPresets`, `activeLayout`; the active one, else the first), as a desktop's layout.
/// The server keeps no list now: on the first boot after the upgrade it becomes the layout in
/// force, and the first desktop browser takes it as its own (`ensureDesktopLayout`).
pub fn legacy_layout() -> Option<wire::ClientLayout> {
    legacy_layout_of(&std::fs::read_to_string(config_path()).ok()?)
}

fn legacy_layout_of(json: &str) -> Option<wire::ClientLayout> {
    #[derive(serde::Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Old {
        #[serde(default)]
        layout_presets: Vec<OldPreset>,
        #[serde(default)]
        active_layout: String,
    }
    #[derive(serde::Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct OldPreset {
        name: String,
        monitors: Vec<wire::MonitorSpec>,
        #[serde(default)]
        scroll_mode: wire::ScrollMode,
        #[serde(default)]
        scroll_speed: Option<f64>,
    }
    let old: Old = serde_json::from_str(json).ok()?;
    let p = old
        .layout_presets
        .iter()
        .find(|p| p.name == old.active_layout)
        .or(old.layout_presets.first())?;
    if p.monitors.is_empty() {
        return None;
    }
    // Monitor 0 is the primary one now
    let mut monitors = p.monitors.clone();
    if let Some(i) = monitors.iter().position(|m| m.primary) {
        let primary = monitors.remove(i);
        monitors.insert(0, primary);
    }
    for (i, m) in monitors.iter_mut().enumerate() {
        m.primary = i == 0;
    }
    Some(wire::ClientLayout {
        client: "legacy".into(),
        label: "desktop".into(),
        monitors,
        scroll_mode: p.scroll_mode,
        scroll_speed: p.scroll_speed.unwrap_or(100.0),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An old config's active preset becomes a desktop's layout, its primary monitor first.
    #[test]
    fn an_old_configs_active_preset_becomes_a_desktop_layout() {
        let json = r#"{"activeLayout":"B","layoutPresets":[
            {"name":"A","monitors":[{"width":1920,"height":1080,"primary":true}]},
            {"name":"B","scrollSpeed":40,"monitors":[
                {"width":2560,"height":1440,"x":0,"y":0},
                {"width":3840,"height":2160,"x":2560,"y":0,"primary":true}]}]}"#;
        let l = legacy_layout_of(json).unwrap();
        assert_eq!((l.label.as_str(), l.scroll_speed), ("desktop", 40.0));
        assert_eq!((l.monitors[0].width, l.monitors[0].x, l.monitors[0].primary), (3840, 2560, true));
        assert!(!l.monitors[1].primary);
        assert!(legacy_layout_of(r#"{"activeLayout":""}"#).is_none());
    }

    #[test]
    fn merge_preserves_blank_scalars_and_applies_changes() {
        let base = AppConfig::default();
        // The UI sends back a blank unchanged scalar, plus real changes. Unknown keys
        // (e.g. the retired Advanced-pane fields) are dropped, never an error.
        let incoming = serde_json::json!({
            "listen": { "web": 9100 },
            "staticDir": "",
            "dataDir": "data",
            "docker": { "hostnamePrefix": "clone-" },
        });
        let merged = merge_update(&base, incoming).unwrap();
        assert_eq!(merged.docker.hostname_prefix, "clone-");
    }

    #[test]
    fn merge_presets_by_name() {
        use wire::{EnvVar, Preset};
        let mut base = AppConfig::default();
        base.presets = vec![
            Preset {
                name: "med".into(),
                linear_key: "OLD-MED".into(),
                vars: vec![EnvVar {
                    key: "STALE".into(),
                    value: "1".into(),
                }],
                ..Default::default()
            },
            Preset {
                name: "gone".into(),
                linear_key: "OLD-GONE".into(),
                ..Default::default()
            },
        ];
        // UI sends the full list: the key is stored verbatim (blank clears it), new row =
        // added, omitted row ("gone") = deleted (with its key). Labels/Dockerfile replace.
        let incoming = serde_json::json!({
            "presets": [
                { "name": "med", "labels": [" Backend ", ""], "linearKey": "",
                  "dockerfile": "FROM base:x", "startupScript": "echo hi",
                  "vars": [{ "key": " TURBO_TEAM ", "value": "talktomedi" },
                           { "key": "", "value": "dropped" },
                           { "key": "BLANK", "value": "" }] },
                { "name": "new", "labels": [], "linearKey": "NEW-KEY", "vars": [] },
            ],
        });
        let merged = merge_update(&base, incoming).unwrap();
        assert_eq!(merged.presets.len(), 2);
        assert_eq!(merged.presets[0].startup_script, "echo hi");
        assert_eq!(merged.presets[1].startup_script, "");
        assert_eq!(merged.presets[0].linear_key, ""); // blank clears the stored key
        assert_eq!(merged.presets[0].labels, vec!["Backend"]); // trimmed, blanks dropped
        assert_eq!(merged.presets[0].dockerfile, "FROM base:x");
        assert_eq!(merged.presets[1].name, "new");
        assert_eq!(merged.presets[1].linear_key, "NEW-KEY");
        assert!(!merged.presets.iter().any(|p| p.name == "gone")); // omitted → deleted
        // Vars replace wholesale, like labels: the stored STALE row is gone because the patch
        // did not send it. The key is trimmed, a blank-key row is dropped (the editor's "add"
        // button makes one), and a blank VALUE survives — `KEY=` is a real setting that clears
        // an inherited value, not an instruction to leave the variable out.
        assert_eq!(
            merged.presets[0].vars,
            vec![
                EnvVar {
                    key: "TURBO_TEAM".into(),
                    value: "talktomedi".into()
                },
                EnvVar {
                    key: "BLANK".into(),
                    value: String::new()
                },
            ]
        );
        assert_eq!(merged.presets[1].vars, vec![]);
        // No `presets` field at all → unchanged.
        let untouched = merge_update(&base, serde_json::json!({})).unwrap();
        assert_eq!(untouched.presets, base.presets);
    }

    /// A gen-1 `config.json` carried its preset env in `presets[].vars`. That field was retired
    /// when preset env moved into the Dockerfile, so the retired gen-1 → gen-2 migration silently
    /// dropped it and the vars had to be scraped out and written back by hand (the old runbook
    /// §5.4 and §5.8). It is a live field again, so the current code just reads it.
    #[test]
    fn a_gen_1_config_keeps_its_preset_vars() {
        let gen1 = serde_json::json!({
            "presets": [{
                "name": "Medi",
                "labels": ["Dev"],
                "linearKey": "lin_api_x",
                "vars": [
                    { "key": "TURBO_API", "value": "http://10.0.0.101:3000" },
                    { "key": "TURBO_TEAM", "value": "talktomedi" }
                ]
            }]
        });
        let cfg: AppConfig = serde_json::from_value(gen1).unwrap();
        assert_eq!(
            cfg.presets[0]
                .vars
                .iter()
                .map(|v| (v.key.as_str(), v.value.as_str()))
                .collect::<Vec<_>>(),
            vec![
                ("TURBO_API", "http://10.0.0.101:3000"),
                ("TURBO_TEAM", "talktomedi"),
            ]
        );
    }

    #[test]
    fn merge_replaces_account_pools_wholesale() {
        // The editor always sends the full pool list, so a plain array replace is right.
        let mut base = AppConfig::default();
        base.groups = vec![wire::CloneGroup {
            name: "old".into(),
            accounts: vec![],
        }];
        let incoming = serde_json::json!({
            "groups": [
                { "name": "team", "accounts": ["a@x.com"] },
                { "name": "beta", "accounts": [] },
            ],
        });
        let merged = merge_update(&base, incoming).unwrap();
        assert_eq!(merged.groups.len(), 2);
        assert_eq!(merged.groups[0].name, "team");
        assert_eq!(merged.groups[0].accounts, vec![wire::GroupMember::from("a@x.com")]);
        assert_eq!(merged.groups[1].name, "beta");
        // An empty array normalizes to the backstop pool — there is always at least one
        // group. The save sweep then deletes the accounts the emptied list orphaned.
        let cleared = merge_update(&merged, serde_json::json!({ "groups": [] })).unwrap();
        assert_eq!(cleared.groups.len(), 1);
        assert_eq!(cleared.groups[0].name, "Default");
        assert!(cleared.groups[0].accounts.is_empty());
    }

    #[test]
    fn merge_replaces_account_pools_alongside_codex_config() {
        use wire::CodexConfig;
        let mut base = AppConfig::default();
        base.groups = vec![wire::CloneGroup {
            name: "old".into(),
            accounts: vec![],
        }];
        base.codex = CodexConfig {
            ..Default::default()
        };
        // Editor sends the full group list + a codex config patch (retired poll keys
        // are dropped silently).
        let incoming = serde_json::json!({
            "groups": [{ "name": "team", "accounts": [] }],
            "codex": { "pollSecs": 300, "usagePolling": false },
        });
        let merged = merge_update(&base, incoming).unwrap();
        assert_eq!(merged.groups.len(), 1);
        assert_eq!(merged.groups[0].name, "team");
        // A codex-only patch leaves the groups untouched.
        let m2 = merge_update(
            &merged,
            serde_json::json!({ "codex": { "autoReset": true } }),
        )
        .unwrap();
        assert!(m2.codex.auto_reset);
        assert_eq!(m2.groups.len(), 1, "codex patch must not disturb pools");
        assert_eq!(m2.groups[0].name, "team");
    }

    #[test]
    fn empty_pool_list_normalizes_to_one_default_group() {
        // No `groups` key at all (fresh config, old client) → the backstop all the same.
        let merged = merge_update(&AppConfig::default(), serde_json::json!({})).unwrap();
        assert_eq!(merged.groups.len(), 1);
        assert_eq!(merged.groups[0].name, "Default");
        // A patch that names pools keeps them verbatim — no backstop appended.
        let kept = merge_update(
            &AppConfig::default(),
            serde_json::json!({ "groups": [{ "name": "team", "accounts": [] }] }),
        )
        .unwrap();
        assert_eq!(kept.groups.len(), 1);
        assert_eq!(kept.groups[0].name, "team");
    }

    /// A base config that has finished first-run setup (latch locked).
    fn setup_done() -> AppConfig {
        let mut base = AppConfig::default();
        base.setup_complete = true;
        base
    }

    #[test]
    fn setup_latch_is_one_way() {
        let base = setup_done();
        // setupComplete cannot be turned off.
        let e = merge_update(&base, serde_json::json!({ "setupComplete": false })).unwrap_err();
        assert!(e.to_string().contains("setupComplete"), "err: {e}");
        // Retired keys (subnet, dataDir, …) are dropped silently, never an error.
        let ok = merge_update(
            &base,
            serde_json::json!({ "docker": { "subnet": "10.42.0.0/24" }, "dataDir": "" }),
        )
        .unwrap();
        assert!(ok.setup_complete);
    }

    #[test]
    fn retired_subnet_keys_are_dropped_not_validated() {
        // The subnet is hardcoded now; a stale client sending one is ignored, never an error.
        let base = AppConfig::default();
        let ok = merge_update(
            &base,
            serde_json::json!({ "docker": { "subnet": "banana/24" } }),
        )
        .unwrap();
        assert_eq!(ok.docker.hostname_prefix, base.docker.hostname_prefix);
    }

    #[test]
    fn setup_complete_latches_one_way() {
        // false → true is allowed (the wizard finishing).
        let base = AppConfig::default();
        let merged = merge_update(&base, serde_json::json!({ "setupComplete": true })).unwrap();
        assert!(merged.setup_complete);
        // true → false is rejected (the latch can't be undone via the API).
        let base = setup_done();
        let e = merge_update(&base, serde_json::json!({ "setupComplete": false })).unwrap_err();
        assert!(e.to_string().contains("setupComplete"), "err: {e}");
        // true → true (or omitted) is fine.
        let ok = merge_update(&base, serde_json::json!({ "setupComplete": true })).unwrap();
        assert!(ok.setup_complete);
        let ok = merge_update(&base, serde_json::json!({})).unwrap();
        assert!(ok.setup_complete);
    }

    #[test]
    fn merge_replaces_ssh_authorized_keys_wholesale() {
        let mut base = AppConfig::default();
        base.ssh.authorized_keys = vec!["ssh-ed25519 OLD a".into()];
        let incoming = serde_json::json!({
            "ssh": { "authorizedKeys": ["ssh-ed25519 NEW b", "ssh-ed25519 NEW c"] }
        });
        let merged = merge_update(&base, incoming).unwrap();
        assert_eq!(
            merged.ssh.authorized_keys,
            vec![
                "ssh-ed25519 NEW b".to_string(),
                "ssh-ed25519 NEW c".to_string()
            ]
        );
    }

    #[test]
    fn merge_can_clear_ssh_authorized_keys() {
        let mut base = AppConfig::default();
        base.ssh.authorized_keys = vec!["ssh-ed25519 OLD a".into()];
        let merged = merge_update(
            &base,
            serde_json::json!({ "ssh": { "authorizedKeys": [] } }),
        )
        .unwrap();
        assert!(merged.ssh.authorized_keys.is_empty());
    }

    #[test]
    fn ssh_keys_editable_after_setup_complete() {
        // The one-time category guard must not block SSH key edits post-setup.
        let mut base = AppConfig::default();
        base.setup_complete = true;
        let merged = merge_update(
            &base,
            serde_json::json!({ "ssh": { "authorizedKeys": ["ssh-ed25519 AAAA x"] } }),
        )
        .unwrap();
        assert_eq!(
            merged.ssh.authorized_keys,
            vec!["ssh-ed25519 AAAA x".to_string()]
        );
    }
}

/// Resolve the state.json path: always `<DATA_DIR>/state.json`.
pub fn state_path() -> PathBuf {
    Path::new(wire::DATA_DIR).join("state.json")
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
    // Pool backstop: there is always at least one group. A save that empties the list
    // (every pool deleted) normalizes to a single `Default` pool rather than none.
    if merged.groups.is_empty() {
        merged.groups = vec![wire::CloneGroup {
            name: "Default".into(),
            accounts: Vec::new(),
        }];
    }
    if let Some(serde_json::Value::Array(rows)) = incoming_presets {
        merged.presets = merge_presets(&base.presets, &rows);
        // A preset always names a pool default: a blank row falls back to the first pool
        // (`"none"` = any group survives as-is). The pool backstop above guarantees one exists.
        if let Some(first) = merged.groups.first().map(|g| g.name.clone()) {
            for p in merged.presets.iter_mut() {
                if p.group.trim().is_empty() {
                    p.group = first.clone();
                }
            }
        }
    }
    enforce_categories(base, &merged)?;
    Ok(merged)
}

/// Guard the invariants on a merged config: once first-run setup has completed
/// (`base.setup_complete`), the `setupComplete` latch can't be undone. A client re-sending
/// the current value is a no-op, not an error.
fn enforce_categories(base: &AppConfig, merged: &AppConfig) -> Result<()> {
    if base.setup_complete && !merged.setup_complete {
        bail!(
            "setupComplete cannot be turned off — it is a one-way latch set during first-run setup"
        );
    }
    Ok(())
}

/// Merge the UI's preset rows by name: every field is taken verbatim from the row
/// (the Linear key is a regular visible field now — blank clears it); labels and the
/// Dockerfile replace; a preset absent from the list is deleted.
fn merge_presets(_base: &[wire::Preset], rows: &[serde_json::Value]) -> Vec<wire::Preset> {
    let mut out: Vec<wire::Preset> = Vec::new();
    for r in rows {
        let Some(name) = r.get("name").and_then(|v| v.as_str()) else {
            continue;
        };
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
        // The Linear key is a regular visible field now: what the editor sends is what is
        // stored, blank included (blank clears it). No keep-stored logic remains.
        let linear_key = r
            .get("linearKey")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let agent_playbook = r
            .get("agentPlaybook")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let global_prompt = r
            .get("globalPrompt")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        // Startup script: verbatim text, blank clears it (empty means nothing runs).
        let startup_script = r
            .get("startupScript")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        // Default account pool: a pool name or `"none"` (any group). Unlike `linearKey`
        // a blank does NOT keep the stored value — `merge_update` fills blanks with the
        // first pool, so a preset always names a default.
        let group = r
            .get("group")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim()
            .to_string();
        let group = if group.eq_ignore_ascii_case("none") {
            "none".to_string()
        } else {
            group
        };
        // Environment variables, replaced wholesale by what the editor sends. Rows with a
        // blank key are dropped (the editor's "add" button makes an empty pair, and a
        // half-filled one must not reach `/etc/environment`); the key is trimmed because a
        // stray space would write a variable no shell can name. The value is kept verbatim,
        // spaces included, and a blank value is a real setting — `KEY=` clears an inherited
        // one rather than meaning "unset".
        let vars: Vec<wire::EnvVar> = r
            .get("vars")
            .and_then(|v| v.as_array())
            .map(|rows| {
                rows.iter()
                    .filter_map(|v| {
                        let key = v.get("key")?.as_str()?.trim().to_string();
                        if key.is_empty() {
                            return None;
                        }
                        let value = v
                            .get("value")
                            .and_then(|x| x.as_str())
                            .unwrap_or("")
                            .to_string();
                        Some(wire::EnvVar { key, value })
                    })
                    .collect()
            })
            .unwrap_or_default();
        out.push(wire::Preset {
            name,
            labels,
            linear_key,
            group,
            default_fork_clone: r
                .get("defaultForkClone")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .trim()
                .to_string(),
            vars,
            agent_playbook,
            global_prompt,
            startup_script,
            // Empty box resets to the default base Dockerfile (a Dockerfile without
            // FROM cannot build, so there is no meaningful empty state to keep).
            dockerfile: r
                .get("dockerfile")
                .and_then(|v| v.as_str())
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string)
                .unwrap_or_else(|| "FROM pegasis0/rmng-template:latest".into()),
        });
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
