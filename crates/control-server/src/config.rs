//! Config loading. `./config.json` is the single source of truth: it holds every
//! setting incl. secrets (no `RMNG_*` env overrides); missing → defaults. The Settings
//! UI (`/api/config`) is the intended editor — this is load/save + merge/category logic.

use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use wire::AppConfig;

pub fn config_path() -> PathBuf {
    PathBuf::from("config.json")
}

pub fn load() -> Result<AppConfig> {
    let path = config_path();
    let cfg = match std::fs::read_to_string(&path) {
        Ok(s) => {
            let mut cfg: AppConfig = serde_json::from_str(&s)
                .with_context(|| format!("parsing {}", path.display()))?;
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
                    agent_playbook: String::new(),
                });
            }
        }
    }
    if non_empty("linear") {
        tracing::info!("dropping legacy per-workspace Linear keys (now per-preset — re-enter in Settings)");
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
    let mut changed = non_empty("envPresets")
        || non_empty("linear")
        || non_empty("cloneAccounts")
        || has_proxmox;
    // Legacy single `monitors` array → a "Default" layout preset (one-shot). Only when
    // the new `layout_presets` is still empty (don't clobber an already-migrated config).
    if cfg.layout_presets.is_empty() {
        if let Some(mons) = raw.get("monitors").and_then(|m| m.as_array()) {
            if !mons.is_empty() {
                if let Ok(parsed) = serde_json::from_value::<Vec<wire::MonitorSpec>>(
                    serde_json::Value::Array(mons.clone()),
                ) {
                    cfg.layout_presets = vec![wire::LayoutPreset { name: "Default".into(), monitors: parsed }];
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
    use wire::CloneGroup;

    #[test]
    fn merge_preserves_blank_secrets_and_applies_changes() {
        let mut base = AppConfig::default();
        base.detector_inference_url = "http://infer:8080".into();
        // The UI sends back a blank for an unchanged scalar, plus real changes.
        let incoming = serde_json::json!({
            "listen": { "web": 9100 },
            "detectorInferenceUrl": "",
            "docker": { "hostnamePrefix": "clone-" },
        });
        let merged = merge_update(&base, incoming).unwrap();
        assert_eq!(merged.listen.web, 9100); // changed
        assert_eq!(merged.listen.video, 9001); // untouched (merge kept it)
        assert_eq!(merged.detector_inference_url, "http://infer:8080"); // blank = unchanged
        assert_eq!(merged.docker.hostname_prefix, "clone-"); // non-secret changed
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
    fn merge_carries_preset_agent_playbook() {
        let base = AppConfig::default();
        let incoming = serde_json::json!({
            "presets": [{ "name": "p", "agentPlaybook": "extra for p", "linearKey": "" }],
        });
        let merged = merge_update(&base, incoming).unwrap();
        assert_eq!(merged.presets.len(), 1);
        assert_eq!(merged.presets[0].agent_playbook, "extra for p");
    }

    #[test]
    fn merge_sets_global_agent_playbook() {
        let base = AppConfig::default();
        let incoming = serde_json::json!({ "agentPlaybook": "NEW GLOBAL" });
        let merged = merge_update(&base, incoming).unwrap();
        assert_eq!(merged.agent_playbook, "NEW GLOBAL");
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

    #[test]
    fn merge_replaces_codex_groups_and_config() {
        use wire::CodexConfig;
        let mut base = AppConfig::default();
        base.codex_groups = vec![CloneGroup { name: "old".into(), accounts: vec!["a@o".into()] }];
        base.codex = CodexConfig { poll_secs: 600, usage_polling: true, ..Default::default() };
        // Editor sends the full group list + a codex config patch.
        let incoming = serde_json::json!({
            "codexGroups": [{ "name": "team", "accounts": ["a@o", "b@o"] }],
            "codex": { "pollSecs": 300, "usagePolling": false },
        });
        let merged = merge_update(&base, incoming).unwrap();
        assert_eq!(merged.codex_groups.len(), 1);
        assert_eq!(merged.codex_groups[0].name, "team");
        assert_eq!(merged.codex_groups[0].accounts, vec!["a@o".to_string(), "b@o".to_string()]);
        assert_eq!(merged.codex.poll_secs, 300);
        assert!(!merged.codex.usage_polling);
        // An empty array clears all codex groups.
        let cleared = merge_update(&merged, serde_json::json!({ "codexGroups": [] })).unwrap();
        assert!(cleared.codex_groups.is_empty());
        // Claude groups are untouched by a codex-only patch.
        let mut with_claude = base.clone();
        with_claude.clone_groups = vec![CloneGroup { name: "cl".into(), accounts: vec!["c@a".into()] }];
        let m2 = merge_update(&with_claude, serde_json::json!({ "codexGroups": [] })).unwrap();
        assert_eq!(m2.clone_groups.len(), 1, "codex patch must not disturb claude groups");
    }

    /// A base config that has finished first-run setup (one-time fields locked).
    fn setup_done() -> AppConfig {
        let mut base = AppConfig::default();
        base.setup_complete = true;
        base.data_dir = "data".into();
        base.clone_socket = "/srv/rmng-sock/clones.sock".into();
        base.docker.subnet = "10.99.0.0/24".into();
        base
    }

    #[test]
    fn one_time_fields_rejected_after_setup() {
        let base = setup_done();
        // data_dir
        let e = merge_update(&base, serde_json::json!({ "dataDir": "other" })).unwrap_err();
        assert!(e.to_string().contains("dataDir"), "err: {e}");
        assert!(e.to_string().contains("first-run"), "err: {e}");
        // cloneSocket
        let e = merge_update(&base, serde_json::json!({ "cloneSocket": "/tmp/other.sock" }))
            .unwrap_err();
        assert!(e.to_string().contains("cloneSocket"), "err: {e}");
        assert!(e.to_string().contains("first-run"), "err: {e}");
        // docker.subnet
        let e = merge_update(&base, serde_json::json!({ "docker": { "subnet": "10.42.0.0/24" } }))
            .unwrap_err();
        assert!(e.to_string().contains("subnet"), "err: {e}");
        assert!(e.to_string().contains("first-run"), "err: {e}");
        // A no-op resend of the same values is fine (final value == base value).
        let ok = merge_update(
            &base,
            serde_json::json!({ "dataDir": "data", "cloneSocket": "/srv/rmng-sock/clones.sock", "docker": { "subnet": "10.99.0.0/24" } }),
        )
        .unwrap();
        assert_eq!(ok.data_dir, "data");
        assert_eq!(ok.clone_socket, "/srv/rmng-sock/clones.sock");
        // Blank strings are unchanged (deep-merge protects them) — never an error.
        let ok = merge_update(
            &base,
            serde_json::json!({ "dataDir": "", "docker": { "subnet": "" } }),
        )
        .unwrap();
        assert_eq!(ok.docker.subnet, "10.99.0.0/24");
    }

    #[test]
    fn one_time_fields_editable_before_setup() {
        // Before setup completes, the one-time fields are freely editable.
        let base = AppConfig::default(); // setup_complete == false
        let merged = merge_update(
            &base,
            serde_json::json!({
                "dataDir": "elsewhere",
                "cloneSocket": "/run/other/clones.sock",
                "docker": { "subnet": "10.42.0.0/24" },
            }),
        )
        .unwrap();
        assert_eq!(merged.data_dir, "elsewhere");
        assert_eq!(merged.clone_socket, "/run/other/clones.sock");
        assert_eq!(merged.docker.subnet, "10.42.0.0/24");
    }

    #[test]
    fn subnet_validated_at_merge() {
        let base = AppConfig::default(); // pre-setup: subnet is editable, but must be valid
        let set = |s: &str| serde_json::json!({ "docker": { "subnet": s } });
        // Valid CIDRs across the allowed prefix range are accepted.
        for good in ["10.99.0.0/24", "172.30.0.0/16", "192.168.0.0/20"] {
            let ok = merge_update(&base, set(good)).unwrap();
            assert_eq!(ok.docker.subnet, good);
        }
        // Bad format / bad prefix / non-IP are all rejected, naming the field.
        for bad in [
            "10.99.0.0",      // no prefix
            "10.99.0.0/",     // empty prefix
            "10.99.0.0/8",    // prefix too wide (<16)
            "10.99.0.0/25",   // prefix too narrow (>24)
            "10.99.0/24",     // not a full IPv4 address
            "banana/24",      // non-IP
            "fd00::/24",      // IPv6 not supported
        ] {
            let e = merge_update(&base, set(bad)).unwrap_err();
            assert!(e.to_string().contains("docker.subnet"), "subnet {bad:?} err: {e}");
        }
        // Blank = unchanged (deep-merge collapses it before validation) — never an error.
        let ok = merge_update(&base, set("")).unwrap();
        assert_eq!(ok.docker.subnet, base.docker.subnet);
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
            monitors: vec![wire::MonitorSpec { width: 800, height: 600, x: 0, y: 0, primary: true }],
        }];
        cfg.active_layout = "X".into();
        // Migration must not clobber an already-migrated config.
        let _ = migrate_legacy(&raw, &mut cfg);
        assert_eq!(cfg.layout_presets.len(), 1);
        assert_eq!(cfg.layout_presets[0].name, "X");
        assert_eq!(cfg.layout_presets[0].monitors[0].width, 800, "existing preset width must not be clobbered");
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

        // Each restart-required trigger flips it true.
        let mut n = base.clone();
        n.listen.web = 8080;
        assert!(restart_required(&base, &n));
        let mut n = base.clone();
        n.listen.video = 8081;
        assert!(restart_required(&base, &n));
        let mut n = base.clone();
        n.listen.clone_mcp = 8082;
        assert!(restart_required(&base, &n));
        let mut n = base.clone();
        n.clone_socket = "/tmp/other.sock".into();
        assert!(restart_required(&base, &n));
        let mut n = base.clone();
        n.docker.socket = "/run/docker.sock".into();
        assert!(restart_required(&base, &n));
        let mut n = base.clone();
        n.static_dir = "frontend/build/client".into();
        assert!(restart_required(&base, &n));
        let mut n = base.clone();
        n.chroma = wire::ChromaMode::Yuv444;
        assert!(restart_required(&base, &n));

        // A non-trigger field (immediate-apply) does NOT require a restart.
        let mut n = base.clone();
        n.docker.hostname_prefix = "other-".into();
        assert!(!restart_required(&base, &n));
    }

    fn ms(w: u32, h: u32) -> wire::MonitorSpec {
        wire::MonitorSpec { width: w, height: h, x: 0, y: 0, primary: true }
    }

    #[test]
    fn merge_reconciles_active_layout_when_active_preset_removed() {
        let mut base = AppConfig::default();
        base.layout_presets = vec![
            wire::LayoutPreset { name: "A".into(), monitors: vec![ms(1920, 1080)] },
            wire::LayoutPreset { name: "B".into(), monitors: vec![ms(3840, 2160)] },
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
        let one = |primary: bool| {
            serde_json::json!({ "width": 1920, "height": 1080, "x": 0, "y": 0, "primary": primary })
        };

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
        assert_eq!(mons.iter().filter(|m| m.primary).count(), 1, "exactly one primary");
        assert!(mons[0].primary && !mons[1].primary, "first primary kept, rest cleared");

        // Zero primaries → Ok, normalized so the first monitor becomes primary.
        let no_primary = serde_json::json!({ "layoutPresets": [
            { "name": "A", "monitors": [one(false), one(false)] },
        ] });
        let merged = merge_update(&base, no_primary).unwrap();
        let mons = &merged.layout_presets[0].monitors;
        assert_eq!(mons.iter().filter(|m| m.primary).count(), 1, "exactly one primary");
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
            vec!["ssh-ed25519 NEW b".to_string(), "ssh-ed25519 NEW c".to_string()]
        );
    }

    #[test]
    fn merge_can_clear_ssh_authorized_keys() {
        let mut base = AppConfig::default();
        base.ssh.authorized_keys = vec!["ssh-ed25519 OLD a".into()];
        let merged = merge_update(&base, serde_json::json!({ "ssh": { "authorizedKeys": [] } })).unwrap();
        assert!(merged.ssh.authorized_keys.is_empty());
    }

    #[test]
    fn restart_required_flips_on_bastion_port() {
        let base = AppConfig::default();
        let mut n = base.clone();
        n.listen.bastion = 2200;
        assert!(restart_required(&base, &n));
        // Changing keys alone is live-apply, NOT restart-required.
        let mut k = base.clone();
        k.ssh.authorized_keys = vec!["ssh-ed25519 AAAA x".into()];
        assert!(!restart_required(&base, &k));
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
        assert_eq!(merged.ssh.authorized_keys, vec!["ssh-ed25519 AAAA x".to_string()]);
    }
}

/// Resolve the state.json path: always `<data_dir>/state.json`.
pub fn state_path(cfg: &AppConfig) -> PathBuf {
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
    // Keep active_layout valid after preset edits: if it no longer names a preset,
    // point it at the first (or clear it when there are none).
    if !merged.layout_presets.iter().any(|p| p.name == merged.active_layout) {
        merged.active_layout =
            merged.layout_presets.first().map(|p| p.name.clone()).unwrap_or_default();
    }
    enforce_categories(base, &merged)?;
    validate_docker_subnet(&merged.docker.subnet)?;
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

/// Reject a `docker.subnet` that isn't an IPv4 CIDR with a `/16`–`/24` prefix (the
/// design range for the `rmng` bridge: room for the `.1` gateway / `.2` control-server /
/// `.10+` clone pool without an absurdly large network). Validated on the merged value,
/// so a bad subnet can never be saved; blank-string "unchanged" is already collapsed by
/// `deep_merge`, so this always sees a concrete value (the default is valid).
fn validate_docker_subnet(subnet: &str) -> Result<()> {
    let ok = subnet.split_once('/').is_some_and(|(ip, prefix)| {
        ip.parse::<std::net::Ipv4Addr>().is_ok()
            && prefix.parse::<u8>().is_ok_and(|p| (16..=24).contains(&p))
    });
    if !ok {
        bail!("docker.subnet must be an IPv4 CIDR with a /16–/24 prefix (e.g. 10.99.0.0/24), got {subnet:?}");
    }
    Ok(())
}

/// Guard the effect-category invariants on a merged config. Once first-run setup has
/// completed (`base.setup_complete`), the **one-time** fields (baked into clones at
/// provision) can't change, and the `setupComplete` latch can't be undone. Blank-string
/// "unchanged" fields are already collapsed by `deep_merge`, so these compare final
/// values — a client re-sending the current value is a no-op, not an error.
fn enforce_categories(base: &AppConfig, merged: &AppConfig) -> Result<()> {
    if base.setup_complete && !merged.setup_complete {
        bail!("setupComplete cannot be turned off — it is a one-way latch set during first-run setup");
    }
    if base.setup_complete {
        if merged.data_dir != base.data_dir {
            bail!("dataDir is a one-time setting (set during first-run setup) and cannot be changed after setup");
        }
        if merged.clone_socket != base.clone_socket {
            bail!("cloneSocket is a one-time setting (set during first-run setup) and cannot be changed after setup");
        }
        if merged.docker.subnet != base.docker.subnet {
            bail!("docker.subnet is a one-time setting (baked into the rmng network at first-run setup) and cannot be changed after setup");
        }
    }
    Ok(())
}

/// Whether applying `new` over `old` requires a server restart to take effect. The
/// restart-required settings are the ones wired once at startup: the four listen ports,
/// the clone-daemon unix socket, the Docker daemon socket (the bollard client is built
/// at startup), the static-file directory, and the chroma mode. Everything else applies
/// live. Consumed by web.rs's `PUT /api/config` handler, which surfaces the result as
/// `ConfigPutResponse.restart_required`.
pub fn restart_required(old: &AppConfig, new: &AppConfig) -> bool {
    old.listen.web != new.listen.web
        || old.listen.video != new.listen.video
        || old.listen.clone_mcp != new.listen.clone_mcp
        || old.listen.bastion != new.listen.bastion
        || old.clone_socket != new.clone_socket
        || old.docker.socket != new.docker.socket
        || old.static_dir != new.static_dir
        || old.chroma != new.chroma
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
        let agent_playbook =
            r.get("agentPlaybook").and_then(|v| v.as_str()).unwrap_or("").to_string();
        out.push(wire::Preset { name, labels, linear_key, vars, agent_playbook });
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

