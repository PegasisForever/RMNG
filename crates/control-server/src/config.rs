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
            let mut cfg: AppConfig =
                serde_json::from_str(&s).with_context(|| format!("parsing {}", path.display()))?;
            // Legacy fields (serde ignores them at parse): fold what's still useful
            // into the current shape and rewrite the file once, so dead secrets
            // (long-lived clone tokens, per-workspace Linear keys) don't linger on disk.
            // Also scrubs the retired `proxmox` block, carrying its `hostnamePrefix`
            // into `docker.hostnamePrefix` when no `docker` key is present.
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
    Ok(cfg)
}

/// Fold legacy config fields into the current shape; true = the file must be
/// rewritten. Legacy `envPresets` (env-only presets, pre Linear unification) seed
/// `presets` (no labels/key — the operator adds those in Settings). Legacy `linear`
/// workspace keys (now per-preset) and `cloneAccounts` long-lived tokens (dead since
/// the single-token model) are dropped; the rewrite scrubs them from disk. The retired
/// Proxmox backend is gone: any `proxmox` block is scrubbed (rewrite), and its
/// `hostnamePrefix` is carried into `docker.hostnamePrefix` when the new config has no
/// `docker` key. Legacy top-level `monitors` array is migrated to a `"Default"` layout
/// preset (one-shot only, when `layout_presets` is still empty). There is no
/// `setupComplete` grandfather — an old `config.json` re-runs the wizard (new machine,
/// no `rmng` network / base image), so `setupComplete` stays whatever the file said
/// (default `false` when absent).
fn migrate_legacy(raw: &serde_json::Value, cfg: &mut AppConfig) -> bool {
    let non_empty = |k: &str| match raw.get(k) {
        Some(serde_json::Value::Array(a)) => !a.is_empty(),
        Some(serde_json::Value::Object(o)) => !o.is_empty(),
        _ => false,
    };
    if cfg.presets.is_empty() {
        if let Some(rows) = raw.get("envPresets").and_then(|v| v.as_array()) {
            for r in rows {
                let Some(name) = r.get("name").and_then(|v| v.as_str()) else {
                    continue;
                };
                // Retired: presets carry a full Dockerfile now; legacy vars are dropped.
                cfg.presets.push(wire::Preset {
                    name: name.to_string(),
                    labels: Vec::new(),
                    linear_key: String::new(),
                    // Blank = no opinion; a legacy env-only preset never had an account default.
                    claude_account: String::new(),
                    codex_account: String::new(),
                    agent_playbook: String::new(),
                    global_prompt: String::new(),
                    ..Default::default()
                });
            }
        }
    }
    if non_empty("linear") {
        tracing::info!(
            "dropping legacy per-workspace Linear keys (now per-preset — re-enter in Settings)"
        );
    }
    // Retired: the whole Proxmox backend is gone. Scrub any `proxmox` block from disk;
    // carry its `hostnamePrefix` into `docker.hostnamePrefix` when the file predates the
    // Docker backend (no `docker` key), so the operator's clone-name prefix survives.
    // A blank legacy prefix is NOT folded — it would clobber the docker default.
    let has_proxmox = raw.get("proxmox").is_some();
    if has_proxmox {
        tracing::info!("scrubbing retired proxmox settings from config");
        if raw.get("docker").is_none() {
            if let Some(prefix) = raw
                .get("proxmox")
                .and_then(|p| p.get("hostnamePrefix"))
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
            {
                tracing::info!("carrying proxmox.hostnamePrefix into docker.hostnamePrefix");
                cfg.docker.hostname_prefix = prefix.to_string();
            }
        }
    }
    let retired_clone_mcp = raw
        .get("listen")
        .and_then(serde_json::Value::as_object)
        .is_some_and(|listen| listen.contains_key("cloneMcp"));
    let retired_detector_url = raw.get("detectorInferenceUrl").is_some();
    let mut changed = non_empty("envPresets")
        || non_empty("linear")
        || non_empty("cloneAccounts")
        || has_proxmox
        || retired_clone_mcp
        || retired_detector_url;
    // Retired split pool lists → one `groups` list (one-shot, when `groups` is still
    // empty). Same-named pools merge members (deduped); either side alone survives.
    if cfg.groups.is_empty() && (!cfg.clone_groups.is_empty() || !cfg.codex_groups.is_empty()) {
        let mut merged: Vec<wire::CloneGroup> = Vec::new();
        for g in cfg
            .clone_groups
            .drain(..)
            .chain(cfg.codex_groups.drain(..))
        {
            match merged.iter_mut().find(|m| m.name == g.name) {
                Some(m) => {
                    for email in g.accounts {
                        if !m.accounts.contains(&email) {
                            m.accounts.push(email);
                        }
                    }
                }
                None => merged.push(g),
            }
        }
        tracing::info!(
            "folding retired clone_groups/codex_groups into one groups list ({} pool(s))",
            merged.len()
        );
        cfg.groups = merged;
        changed = true;
    }

    // Legacy single `monitors` array → a "Default" layout preset (one-shot). Only when
    // the new `layout_presets` is still empty (don't clobber an already-migrated config).
    if cfg.layout_presets.is_empty() {
        if let Some(mons) = raw.get("monitors").and_then(|m| m.as_array()) {
            if !mons.is_empty() {
                if let Ok(parsed) = serde_json::from_value::<Vec<wire::MonitorSpec>>(
                    serde_json::Value::Array(mons.clone()),
                ) {
                    cfg.layout_presets = vec![wire::LayoutPreset {
                        name: "Default".into(),
                        monitors: parsed,
                    }];
                    if cfg.active_layout.is_empty() {
                        cfg.active_layout = "Default".into();
                    }
                    changed = true;
                }
            }
        }
    }
    changed
}

#[cfg(test)]
mod tests {
    use super::*;

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
                  "dockerfile": "FROM base:x", "startupScript": "echo hi" },
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
        assert_eq!(
            cfg.presets[0].dockerfile,
            "FROM pegasis0/rmng-template:latest"
        );

        // Legacy object-shaped `linear` also counts; existing presets are never clobbered.
        let raw = serde_json::json!({ "linear": { "we": "K1" }, "envPresets": [{ "name": "x" }] });
        let mut cfg = AppConfig::default();
        cfg.presets = vec![wire::Preset {
            name: "kept".into(),
            ..Default::default()
        }];
        assert!(migrate_legacy(&raw, &mut cfg));
        assert_eq!(cfg.presets.len(), 1);
        assert_eq!(cfg.presets[0].name, "kept");

        // Split pool lists fold into one `groups` (same-named merge members, deduped).
        let mut cfg = AppConfig::default();
        cfg.clone_groups = vec![
            wire::CloneGroup { name: "team".into(), accounts: vec!["a@x.com".into()] },
            wire::CloneGroup { name: "solo".into(), accounts: vec!["b@x.com".into()] },
        ];
        cfg.codex_groups = vec![wire::CloneGroup {
            name: "team".into(),
            accounts: vec!["z@o.com".into(), "a@x.com".into()],
        }];
        assert!(migrate_legacy(&serde_json::json!({}), &mut cfg));
        assert_eq!(cfg.groups.len(), 2);
        let team = cfg.groups.iter().find(|g| g.name == "team").unwrap();
        assert_eq!(team.accounts, vec!["a@x.com", "z@o.com"]);
        assert!(cfg.clone_groups.is_empty() && cfg.codex_groups.is_empty());

        // Fully-migrated file → no rewrite.
        let raw = serde_json::json!({ "presets": [{ "name": "p" }] });
        let mut cfg = AppConfig::default();
        assert!(!migrate_legacy(&raw, &mut cfg));
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
        assert_eq!(merged.groups[0].accounts, vec!["a@x.com"]);
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
        let m2 =
            merge_update(&merged, serde_json::json!({ "codex": { "autoReset": true } })).unwrap();
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
    fn migrates_legacy_monitors_into_default_preset() {
        // Simulate an old config.json with a top-level `monitors` array and no presets.
        let raw: serde_json::Value = serde_json::json!({
            "monitors": [
                { "width": 3440, "height": 1440, "x": 0, "y": 0, "primary": true }
            ]
        });
        let mut cfg = AppConfig::default(); // layout_presets empty, active_layout ""
        let changed = migrate_legacy(&raw, &mut cfg);
        assert!(changed);
        assert_eq!(cfg.layout_presets.len(), 1);
        assert_eq!(cfg.layout_presets[0].name, "Default");
        assert_eq!(cfg.layout_presets[0].monitors[0].width, 3440);
        assert_eq!(cfg.active_layout, "Default");
    }

    #[test]
    fn migration_noop_when_presets_present() {
        // Use a non-empty, different monitors array to truly test the anti-clobber guard.
        // If the outer `cfg.layout_presets.is_empty()` guard were removed, this test would fail.
        let raw: serde_json::Value = serde_json::json!({
            "monitors": [
                { "width": 1920, "height": 1080, "x": 0, "y": 0, "primary": true }
            ]
        });
        let mut cfg = AppConfig::default();
        cfg.layout_presets = vec![wire::LayoutPreset {
            name: "X".into(),
            monitors: vec![wire::MonitorSpec {
                width: 800,
                height: 600,
                x: 0,
                y: 0,
                primary: true,
            }],
        }];
        cfg.active_layout = "X".into();
        // Migration must not clobber an already-migrated config.
        let _ = migrate_legacy(&raw, &mut cfg);
        assert_eq!(cfg.layout_presets.len(), 1);
        assert_eq!(cfg.layout_presets[0].name, "X");
        assert_eq!(
            cfg.layout_presets[0].monitors[0].width, 800,
            "existing preset width must not be clobbered"
        );
    }

    #[test]
    fn migrate_scrubs_proxmox() {
        // A legacy config with a proxmox block: it's scrubbed (rewrite flagged), its
        // hostnamePrefix is folded into docker.hostnamePrefix (no docker key present),
        // and setupComplete is NOT grandfathered — it stays false when the key is absent.
        let raw = serde_json::json!({
            "proxmox": { "ssh": "root@node", "storage": "local-lvm", "hostnamePrefix": "clone-" },
        });
        let mut cfg: AppConfig = serde_json::from_value(raw.clone()).unwrap();
        assert!(!cfg.setup_complete); // serde default before migration
        assert!(migrate_legacy(&raw, &mut cfg)); // rewrite flagged
        // The `proxmox` key is gone from the serialized output (AppConfig has no such field).
        let out = serde_json::to_value(&cfg).unwrap();
        assert!(out.get("proxmox").is_none(), "proxmox not scrubbed: {out}");
        // hostnamePrefix folded into docker.
        assert_eq!(cfg.docker.hostname_prefix, "clone-");
        // NOT grandfathered — an ssh target no longer implies setup is done.
        assert!(!cfg.setup_complete);

        // When a `docker` key already exists, the proxmox prefix is NOT folded (the new
        // config's docker settings win); proxmox is still scrubbed (rewrite flagged).
        let raw = serde_json::json!({
            "proxmox": { "hostnamePrefix": "old-" },
            "docker": { "hostnamePrefix": "new-" },
        });
        let mut cfg: AppConfig = serde_json::from_value(raw.clone()).unwrap();
        assert!(migrate_legacy(&raw, &mut cfg));
        assert_eq!(cfg.docker.hostname_prefix, "new-");

        // A blank legacy prefix is NOT folded — the docker default survives
        // (still scrubbed / rewrite flagged, since the proxmox block is present).
        let raw = serde_json::json!({ "proxmox": { "hostnamePrefix": "" } });
        let mut cfg: AppConfig = serde_json::from_value(raw.clone()).unwrap();
        assert!(migrate_legacy(&raw, &mut cfg));
        assert_eq!(cfg.docker.hostname_prefix, "pega-"); // default kept

        // No `proxmox` key and a fully-migrated file → no rewrite from proxmox scrubbing.
        let raw = serde_json::json!({ "docker": { "hostnamePrefix": "keep-" } });
        let mut cfg: AppConfig = serde_json::from_value(raw.clone()).unwrap();
        assert!(!migrate_legacy(&raw, &mut cfg));
        assert_eq!(cfg.docker.hostname_prefix, "keep-");
    }

    #[test]
    fn restart_required_matrix() {
        let base = AppConfig::default();
        // No change → no restart.
        assert!(!restart_required(&base, &base.clone()));

        // The only restart-required trigger flips it true.
        let mut n = base.clone();
        n.chroma = wire::ChromaMode::Yuv444;
        assert!(restart_required(&base, &n));

        // A non-trigger field (immediate-apply) does NOT require a restart.
        let mut n = base.clone();
        n.docker.hostname_prefix = "other-".into();
        assert!(!restart_required(&base, &n));
        // Changing keys alone is live-apply, NOT restart-required.
        let mut n = base.clone();
        n.ssh.authorized_keys = vec!["ssh-ed25519 AAAA x".into()];
        assert!(!restart_required(&base, &n));
    }

    fn ms(w: u32, h: u32) -> wire::MonitorSpec {
        wire::MonitorSpec {
            width: w,
            height: h,
            x: 0,
            y: 0,
            primary: true,
        }
    }

    #[test]
    fn merge_reconciles_active_layout_when_active_preset_removed() {
        let mut base = AppConfig::default();
        base.layout_presets = vec![
            wire::LayoutPreset {
                name: "A".into(),
                monitors: vec![ms(1920, 1080)],
            },
            wire::LayoutPreset {
                name: "B".into(),
                monitors: vec![ms(3840, 2160)],
            },
        ];
        base.active_layout = "B".into();
        // The UI removes preset "B", sending only "A".
        let incoming = serde_json::json!({
            "layoutPresets": [ { "name": "A", "monitors": [
                { "width": 1920, "height": 1080, "x": 0, "y": 0, "primary": true } ] } ]
        });
        let merged = merge_update(&base, incoming).unwrap();
        assert_eq!(merged.layout_presets.len(), 1);
        assert_eq!(merged.active_layout, "A"); // reconciled off the removed "B"
    }

    #[test]
    fn merge_rejects_invalid_layout_presets() {
        let base = AppConfig::default();
        let one = |primary: bool| serde_json::json!({ "width": 1920, "height": 1080, "x": 0, "y": 0, "primary": primary });

        // Two presets sharing a (case-sensitive) name → Err.
        let dup = serde_json::json!({ "layoutPresets": [
            { "name": "A", "monitors": [one(true)] },
            { "name": "A", "monitors": [one(true)] },
        ] });
        let e = merge_update(&base, dup).unwrap_err();
        assert!(e.to_string().contains("duplicate"), "err: {e}");

        // Empty / whitespace name → Err.
        let empty_name = serde_json::json!({ "layoutPresets": [
            { "name": "   ", "monitors": [one(true)] },
        ] });
        assert!(merge_update(&base, empty_name).is_err());

        // Zero-monitor preset → Err.
        let no_mons = serde_json::json!({ "layoutPresets": [
            { "name": "A", "monitors": [] },
        ] });
        assert!(merge_update(&base, no_mons).is_err());

        // Two primaries → Ok, normalized to exactly one (first kept, rest cleared).
        let two_primaries = serde_json::json!({ "layoutPresets": [
            { "name": "A", "monitors": [one(true), one(true)] },
        ] });
        let merged = merge_update(&base, two_primaries).unwrap();
        let mons = &merged.layout_presets[0].monitors;
        assert_eq!(
            mons.iter().filter(|m| m.primary).count(),
            1,
            "exactly one primary"
        );
        assert!(
            mons[0].primary && !mons[1].primary,
            "first primary kept, rest cleared"
        );

        // Zero primaries → Ok, normalized so the first monitor becomes primary.
        let no_primary = serde_json::json!({ "layoutPresets": [
            { "name": "A", "monitors": [one(false), one(false)] },
        ] });
        let merged = merge_update(&base, no_primary).unwrap();
        let mons = &merged.layout_presets[0].monitors;
        assert_eq!(
            mons.iter().filter(|m| m.primary).count(),
            1,
            "exactly one primary"
        );
        assert!(mons[0].primary, "first monitor promoted to primary");

        // An empty layout_presets array is allowed (fresh install has none).
        let none = serde_json::json!({ "layoutPresets": [] });
        assert!(merge_update(&base, none).is_ok());
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
    }
    // Keep active_layout valid after preset edits: if it no longer names a preset,
    // point it at the first (or clear it when there are none).
    if !merged
        .layout_presets
        .iter()
        .any(|p| p.name == merged.active_layout)
    {
        merged.active_layout = merged
            .layout_presets
            .first()
            .map(|p| p.name.clone())
            .unwrap_or_default();
    }
    enforce_categories(base, &merged)?;
    validate_layout_presets(&mut merged.layout_presets)?;
    Ok(merged)
}

/// Validate + normalize the merged `layout_presets` (mirrors the clone-`presets` uniqueness
/// style). Rejects a preset with an empty/whitespace name, two presets sharing a name
/// (case-sensitive), or a preset with zero monitors. NORMALIZES each preset in-place to
/// exactly one primary: zero primaries → the first monitor becomes primary; more than one →
/// keep the first primary, clear the rest. An empty `layout_presets` array is allowed (a
/// fresh install legitimately has none — the "can't delete the last preset" rule is UI-only).
fn validate_layout_presets(presets: &mut [wire::LayoutPreset]) -> Result<()> {
    let mut seen: Vec<String> = Vec::new();
    for p in presets.iter_mut() {
        if p.name.trim().is_empty() {
            bail!("layout preset name must not be empty");
        }
        if seen.contains(&p.name) {
            bail!("duplicate layout preset name {:?}", p.name);
        }
        seen.push(p.name.clone());
        if p.monitors.is_empty() {
            bail!("layout preset {:?} must have at least one monitor", p.name);
        }
        // Normalize to exactly one primary.
        let primaries = p.monitors.iter().filter(|m| m.primary).count();
        if primaries == 0 {
            p.monitors[0].primary = true;
        } else if primaries > 1 {
            let mut kept = false;
            for m in p.monitors.iter_mut() {
                if m.primary {
                    if kept {
                        m.primary = false;
                    } else {
                        kept = true;
                    }
                }
            }
        }
    }
    Ok(())
}

/// Guard the effect-category invariants on a merged config. Once first-run setup has
/// completed (`base.setup_complete`), the **one-time** field (the Docker subnet, baked into
/// the rmng bridge at setup) can't change, and the `setupComplete` latch can't be undone. Blank-string
/// "unchanged" fields are already collapsed by `deep_merge`, so these compare final
/// values — a client re-sending the current value is a no-op, not an error.
fn enforce_categories(base: &AppConfig, merged: &AppConfig) -> Result<()> {
    if base.setup_complete && !merged.setup_complete {
        bail!(
            "setupComplete cannot be turned off — it is a one-way latch set during first-run setup"
        );
    }
    Ok(())
}

/// Whether applying `new` over `old` requires a server restart to take effect. The only
/// restart-required setting left is the chroma mode (wired once at startup). Ports, paths,
/// directories, sockets, subnets, images, and poll intervals are hardcoded now (see
/// `wire`), not settings at all. Everything else applies live. Consumed by web.rs's
/// `PUT /api/config` handler, which surfaces the result as
/// `ConfigPutResponse.restart_required`.
pub fn restart_required(old: &AppConfig, new: &AppConfig) -> bool {
    old.chroma != new.chroma
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
        // Default account selections. Unlike `linearKey` a blank does NOT keep the stored value:
        // blank is a meaningful state here ("no opinion — fall through to the next resolution
        // step"), so the editor must be able to clear one back to it.
        let account = |key: &str| {
            r.get(key)
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .trim()
                .to_string()
        };
        out.push(wire::Preset {
            name,
            labels,
            linear_key,
            claude_account: account("claudeAccount"),
            codex_account: account("codexAccount"),
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
