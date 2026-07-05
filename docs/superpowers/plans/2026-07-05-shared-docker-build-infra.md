# Shared Docker Build Infra Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Give every clone a shared Docker Hub pull-through cache and a shared build-layer cache, auto-started by the control-server and live-migrated onto existing clones, with zero operator action.

**Architecture:** The control-server ensures two long-lived infra containers on the existing `rmng` bridge — `rmng-registry` (a `registry:2` pull-through cache for Docker Hub) and `rmng-buildkit` (a shared `moby/buildkit` daemon). A new `buildinfra` reconciler (modeled on `ssh.rs`) writes each running clone's `/etc/docker/daemon.json` mirror config (SIGHUP-reloaded, no downtime) and registers a `--driver remote` buildx builder pointing at `rmng-buildkit`. Both clone-side artifacts persist in the clone's writable layer/home, so migration is apply-once.

**Tech Stack:** Rust (control-server, bollard Docker client, tokio, serde_json), `registry:2`, `moby/buildkit`, buildx remote driver. Design spec: `docs/superpowers/specs/2026-07-05-shared-docker-build-infra-design.md`.

## Global Constraints

- **No-env-settings invariant:** all configuration is in `config.json` via `DockerConfig`; never add `-e`/env flags. (compose.yaml header.)
- **Mirror is Docker Hub only:** dockerd `registry-mirrors` applies to `docker.io` only; do not attempt to mirror ghcr/gcr.
- **HTTP mirror requires the insecure entry:** always write BOTH `registry-mirrors: ["http://rmng-registry:5000"]` and `insecure-registries: ["rmng-registry:5000"]`, else dockerd attempts HTTPS and fails.
- **Infra containers are labeled `rmng.infra=1`, never `rmng.managed=1`** — so they are excluded from clone sweeps (`list_managed_containers`) and the boot reconcile "unknown managed container" warning.
- **Non-fatal + bounded startup:** the infra-ensure at boot must never block or crash the server (same posture as `ensure_network`/`self_setup`): time-bound it and log-and-continue on error.
- **Idempotent everywhere:** every ensure re-runs cleanly; the mirror merge sends no SIGHUP when already applied; the builder setup no-ops when the builder exists.
- **Image tags:** readable pinned version tags, config-overridable (`registry:2.8.3`, `moby/buildkit:v0.17.2`). Confirm/bump the exact current-stable tags during the E2E task.
- **Reconciler cadence:** 30 s, mirroring `ssh.rs`'s loop shape; a clone is configured apply-once and then skipped.

---

### Task 1: `DockerConfig` fields for the build infra

**Files:**
- Modify: `crates/wire/src/config.rs:186-257` (struct fields, default fns, `Default` impl)
- Test: `crates/wire/src/config.rs:559` (existing `#[cfg(test)] mod tests`)

**Interfaces:**
- Produces: `DockerConfig.build_infra_enabled: bool`, `.registry_image: String`, `.buildkit_image: String`, `.buildkit_cache_gb: u32` — consumed by Tasks 3 & 4.

- [ ] **Step 1: Write the failing test**

Add to the `tests` module at `crates/wire/src/config.rs:560`:

```rust
    #[test]
    fn docker_config_build_infra_defaults_when_absent() {
        // An older config.json (no build-infra fields) must load with the feature ON.
        let json = r#"{
            "socket": "/var/run/docker.sock",
            "subnet": "10.99.0.0/24",
            "hostnamePrefix": "pega-",
            "cloneCpus": 16,
            "cloneMemoryMb": 32768,
            "templateReference": "pegasis0/rmng-template:latest",
            "serverImage": "pegasis0/rmng:latest"
        }"#;
        let cfg: DockerConfig = serde_json::from_str(json).unwrap();
        assert!(cfg.build_infra_enabled, "feature defaults on");
        assert_eq!(cfg.registry_image, "registry:2.8.3");
        assert_eq!(cfg.buildkit_image, "moby/buildkit:v0.17.2");
        assert_eq!(cfg.buildkit_cache_gb, 40);
    }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p wire docker_config_build_infra_defaults_when_absent`
Expected: FAIL to compile — `no field build_infra_enabled on type DockerConfig`.

- [ ] **Step 3: Add the fields, default fns, and Default entries**

In `DockerConfig` (after the `server_image` field at `config.rs:220`):

```rust
    /// Master switch for the shared Docker build infra (pull-through Hub mirror + remote
    /// BuildKit). When true (default), the control-server ensures the `rmng-registry` /
    /// `rmng-buildkit` containers at startup and the `buildinfra` reconciler applies the
    /// mirror + remote builder to every running clone. When false, none of that runs and
    /// already-created infra / already-migrated clones are left in place (a pure "stop
    /// managing" — no destructive teardown). Immediate-apply (read fresh each tick).
    #[serde(default = "default_build_infra_enabled")]
    pub build_infra_enabled: bool,
    /// Image for the pull-through Docker Hub cache container (`rmng-registry`). Overridable
    /// (an operator may pin a digest); a change triggers a recreate at next boot.
    #[serde(default = "default_registry_image")]
    pub registry_image: String,
    /// Image for the shared BuildKit daemon container (`rmng-buildkit`). Overridable; a
    /// change triggers a recreate at next boot.
    #[serde(default = "default_buildkit_image")]
    pub buildkit_image: String,
    /// BuildKit cache GC ceiling in GiB (`keepBytes`). Caps the shared layer cache so it
    /// cannot grow unbounded. A change triggers a `rmng-buildkit` recreate at next boot.
    #[serde(default = "default_buildkit_cache_gb")]
    pub buildkit_cache_gb: u32,
```

After `default_server_image` (`config.rs:243`):

```rust
fn default_build_infra_enabled() -> bool {
    true
}
fn default_registry_image() -> String {
    "registry:2.8.3".into()
}
fn default_buildkit_image() -> String {
    "moby/buildkit:v0.17.2".into()
}
fn default_buildkit_cache_gb() -> u32 {
    40
}
```

In `impl Default for DockerConfig` (after `server_image: default_server_image(),` at `config.rs:254`):

```rust
            build_infra_enabled: default_build_infra_enabled(),
            registry_image: default_registry_image(),
            buildkit_image: default_buildkit_image(),
            buildkit_cache_gb: default_buildkit_cache_gb(),
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p wire docker_config_build_infra_defaults_when_absent`
Expected: PASS.

- [ ] **Step 5: Regenerate the TS binding + build**

Run: `cargo test -p wire && cargo build -p wire`
Expected: PASS; `frontend/app/lib/wire/DockerConfig.ts` gains the four camelCase fields (ts-rs export runs under test). Commit the regenerated `.ts` too.

- [ ] **Step 6: Commit**

```bash
git add crates/wire/src/config.rs frontend/app/lib/wire/DockerConfig.ts
git commit -m "feat(config): DockerConfig fields for shared build infra (mirror + buildkit)"
```

---

### Task 2: `buildinfra` pure helpers (daemon.json merge + buildkitd.toml + consts)

**Files:**
- Create: `crates/control-server/src/buildinfra.rs`
- Modify: `crates/control-server/src/main.rs:10` (add `mod buildinfra;` in alphabetical position, between `mod app;`/`mod chat;` — insert after `mod assets;` at line 9)
- Test: `crates/control-server/src/buildinfra.rs` (`#[cfg(test)] mod tests`)

**Interfaces:**
- Produces (consumed by Tasks 3 & 4):
  - `pub const REGISTRY_CONTAINER: &str = "rmng-registry"`, `pub const BUILDKIT_CONTAINER: &str = "rmng-buildkit"`
  - `pub const REGISTRY_DATA_VOL: &str`, `pub const BUILDKIT_CACHE_VOL: &str`
  - `pub const REGISTRY_ADDR: &str = "rmng-registry:5000"`, `pub const BUILDKIT_ENDPOINT: &str = "tcp://rmng-buildkit:1234"`, `pub const BUILDER_NAME: &str = "rmng"`
  - `pub fn merge_mirror_daemon_json(existing: &str) -> anyhow::Result<Option<String>>`
  - `pub fn render_buildkitd_toml(gc_gb: u32) -> String`

- [ ] **Step 1: Write the failing tests**

Create `crates/control-server/src/buildinfra.rs` with the constants + function signatures stubbed to `unimplemented!()` and this test module:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merge_into_empty_adds_both_keys() {
        let out = merge_mirror_daemon_json("").unwrap().expect("empty file must produce a write");
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["registry-mirrors"][0], "http://rmng-registry:5000");
        assert_eq!(v["insecure-registries"][0], "rmng-registry:5000");
    }

    #[test]
    fn merge_preserves_unrelated_keys() {
        let existing = r#"{"log-driver":"json-file","registry-mirrors":["http://other:5000"]}"#;
        let out = merge_mirror_daemon_json(existing).unwrap().expect("must write");
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["log-driver"], "json-file", "unrelated key preserved");
        let mirrors = v["registry-mirrors"].as_array().unwrap();
        assert!(mirrors.iter().any(|m| m == "http://other:5000"), "existing mirror kept");
        assert!(mirrors.iter().any(|m| m == "http://rmng-registry:5000"), "ours appended");
        assert_eq!(v["insecure-registries"][0], "rmng-registry:5000");
    }

    #[test]
    fn merge_is_noop_when_already_applied() {
        let existing = r#"{"registry-mirrors":["http://rmng-registry:5000"],"insecure-registries":["rmng-registry:5000"]}"#;
        assert!(
            merge_mirror_daemon_json(existing).unwrap().is_none(),
            "already-applied config must produce no write (⇒ no SIGHUP)"
        );
    }

    #[test]
    fn buildkitd_toml_has_scaled_keep_bytes() {
        // 40 GiB → 42949672960 bytes must appear verbatim; a mis-scaled cap is a silent
        // unbounded-cache bug.
        let out = render_buildkitd_toml(40);
        assert!(out.contains("42949672960"), "keepBytes for 40 GiB:\n{out}");
        assert!(out.contains("gc = true"), "GC must be enabled:\n{out}");
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p control-server buildinfra::tests`
Expected: FAIL — `unimplemented!()` panics (or link error if not yet wired). If the module doesn't compile because `mod buildinfra;` is missing, add it now (this task's Step 3 covers it).

- [ ] **Step 3: Implement the constants + pure helpers**

Replace the stub body of `crates/control-server/src/buildinfra.rs` above the test module with:

```rust
//! Shared Docker build infra for the clone fleet: a pull-through Docker Hub cache
//! (`rmng-registry`) and a shared BuildKit daemon (`rmng-buildkit`), plus the reconciler
//! that migrates the mirror config + a remote buildx builder onto every running clone.
//! Mirrors `crate::ssh`'s reconciler shape. The two infra containers are ensured by
//! `DockerCtl::ensure_build_infra` (see `docker.rs`); this module owns the pure config
//! rendering and the per-clone apply loop.

use std::collections::HashSet;
use std::time::Duration;

use anyhow::{Context, Result};

use crate::app::App;
use crate::docker::TarEntry;

/// Pull-through cache container name (DNS-resolvable by clones on the `rmng` bridge).
pub const REGISTRY_CONTAINER: &str = "rmng-registry";
/// Shared BuildKit daemon container name.
pub const BUILDKIT_CONTAINER: &str = "rmng-buildkit";
/// Named volume holding the pull-through cache's blobs.
pub const REGISTRY_DATA_VOL: &str = "rmng-registry-data";
/// Named volume holding the shared BuildKit layer cache.
pub const BUILDKIT_CACHE_VOL: &str = "rmng-buildkit-cache";
/// The registry address clones put in `daemon.json` (container DNS name : port).
pub const REGISTRY_ADDR: &str = "rmng-registry:5000";
/// The BuildKit GRPC endpoint the clones' remote buildx builder connects to (plaintext on
/// the trusted bridge).
pub const BUILDKIT_ENDPOINT: &str = "tcp://rmng-buildkit:1234";
/// The buildx builder name registered in each clone.
pub const BUILDER_NAME: &str = "rmng";

/// Merge the pull-through mirror settings into a clone's existing `daemon.json` content
/// (empty/whitespace ⇒ start from `{}`). Adds `registry-mirrors: ["http://rmng-registry:5000"]`
/// and `insecure-registries: ["rmng-registry:5000"]` (the HTTP mirror *requires* the insecure
/// entry). Idempotent: returns `Ok(None)` when both keys already carry our values (⇒ caller
/// writes nothing and sends no SIGHUP); otherwise `Ok(Some(pretty_json))`. All other keys are
/// preserved. Pure — unit-tested.
pub fn merge_mirror_daemon_json(existing: &str) -> Result<Option<String>> {
    use serde_json::{Map, Value};
    let mirror = format!("http://{REGISTRY_ADDR}");
    let mut root: Value = if existing.trim().is_empty() {
        Value::Object(Map::new())
    } else {
        serde_json::from_str(existing).context("parsing existing daemon.json")?
    };
    let obj = root.as_object_mut().context("daemon.json is not a JSON object")?;

    let has = |obj: &Map<String, Value>, key: &str, val: &str| {
        obj.get(key)
            .and_then(|v| v.as_array())
            .map(|a| a.iter().any(|v| v.as_str() == Some(val)))
            .unwrap_or(false)
    };
    if has(obj, "registry-mirrors", &mirror) && has(obj, "insecure-registries", REGISTRY_ADDR) {
        return Ok(None);
    }

    merge_into_string_array(obj, "registry-mirrors", &mirror);
    merge_into_string_array(obj, "insecure-registries", REGISTRY_ADDR);
    Ok(Some(serde_json::to_string_pretty(&root)?))
}

fn merge_into_string_array(obj: &mut serde_json::Map<String, serde_json::Value>, key: &str, val: &str) {
    use serde_json::Value;
    let arr = obj.entry(key.to_string()).or_insert_with(|| Value::Array(Vec::new()));
    match arr.as_array_mut() {
        Some(a) if a.iter().any(|v| v.as_str() == Some(val)) => {}
        Some(a) => a.push(Value::String(val.to_string())),
        None => *arr = Value::Array(vec![Value::String(val.to_string())]), // malformed → replace
    }
}

/// The `buildkitd.toml` for `rmng-buildkit`: a GC policy capping the shared layer cache at
/// `gc_gb` GiB. Pure — unit-tested (a mis-scaled cap is a silent unbounded-cache bug).
///
/// NOTE (E2E): confirm `keepBytes`/`[[worker.oci.gcpolicy]]` against the pinned buildkit
/// version during Task 6; older versions use `gckeepstorage`. Adjust here if the pinned tag
/// wants the legacy key.
pub fn render_buildkitd_toml(gc_gb: u32) -> String {
    let keep_bytes = gc_gb as u64 * 1024 * 1024 * 1024;
    format!(
        "# Rendered by rmng control-server — do not edit.\n\
         root = \"/var/lib/buildkit\"\n\
         [worker.oci]\n\
         enabled = true\n\
         gc = true\n\
         [[worker.oci.gcpolicy]]\n\
         keepBytes = {keep_bytes}\n\
         all = true\n"
    )
}
```

Then add `mod buildinfra;` to `crates/control-server/src/main.rs` after `mod assets;` (line 9), keeping alphabetical order.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p control-server buildinfra::tests`
Expected: PASS (4 tests). Unused-import warnings for `HashSet`/`Duration`/`App`/`TarEntry` are expected until Task 4 — leave them (Task 4 consumes them); do not delete.

- [ ] **Step 5: Commit**

```bash
git add crates/control-server/src/buildinfra.rs crates/control-server/src/main.rs
git commit -m "feat(buildinfra): pure daemon.json merge + buildkitd.toml renderers + consts"
```

---

### Task 3: `ensure_build_infra` — auto-start the two infra containers at boot

**Files:**
- Modify: `crates/control-server/src/docker.rs` (new `LABEL_INFRA` const near `LABEL_MANAGED` at `docker.rs:73`; new methods near `ensure_network` at `docker.rs:754` and `ensure_volume` at `docker.rs:1337`)
- Modify: `crates/control-server/src/main.rs:97` (startup ensure call, after the `self_setup` block)

**Interfaces:**
- Consumes: `buildinfra::{REGISTRY_CONTAINER, BUILDKIT_CONTAINER, REGISTRY_DATA_VOL, BUILDKIT_CACHE_VOL, render_buildkitd_toml}` (Task 2); `DockerConfig` build-infra fields (Task 1); existing `ensure_volume`, `start_container`, `stop_container`, `remove_container`, `upload_tar`, `pull_image`, `daemon()`, `TarEntry`.
- Produces: `pub async fn DockerCtl::ensure_build_infra(&self, cfg: &wire::DockerConfig) -> Result<()>` — consumed by `main.rs`.

- [ ] **Step 1: Add the `LABEL_INFRA` const**

At `docker.rs:74` (immediately after the `LABEL_MANAGED` const):

```rust
/// Label stamped on RMNG shared-infra containers (`rmng-registry`, `rmng-buildkit`).
/// Deliberately NOT `rmng.managed` — infra is excluded from clone sweeps
/// (`list_managed_containers`) and the boot reconcile's "unknown managed container" warning,
/// exactly like the `rmng-self-upgrade` helper.
pub const LABEL_INFRA: &str = "rmng.infra";
```

- [ ] **Step 2: Implement `ensure_build_infra` + helpers**

Add these methods inside `impl DockerCtl` (place near `ensure_network`, e.g. after it ends at `docker.rs:804`). They reuse the same bollard imports `create_clone_container` already uses (`Mount`, `MountTypeEnum`, `HostConfig`, `RestartPolicy`, `RestartPolicyNameEnum`, `ContainerCreateBody`, `NetworkingConfig`, `EndpointSettings`, `CreateContainerOptionsBuilder`, `HashMap`):

```rust
    /// Ensure the shared build-infra containers + volumes exist and run: `rmng-registry`
    /// (pull-through Docker Hub cache) and `rmng-buildkit` (shared BuildKit daemon), both on
    /// the `rmng` bridge, labeled `rmng.infra=1`, `restart: unless-stopped`. Idempotent:
    /// create-if-absent, start-if-stopped, recreate-if-image-drifted (cache volumes survive a
    /// recreate). MUST run after `ensure_network` (the containers attach to `NETWORK`).
    pub async fn ensure_build_infra(&self, cfg: &wire::DockerConfig) -> Result<()> {
        self.ensure_volume(crate::buildinfra::REGISTRY_DATA_VOL).await?;
        self.ensure_volume(crate::buildinfra::BUILDKIT_CACHE_VOL).await?;

        self.ensure_infra_container(InfraSpec {
            name: crate::buildinfra::REGISTRY_CONTAINER,
            image: cfg.registry_image.clone(),
            cmd: None,
            env: vec!["REGISTRY_PROXY_REMOTEURL=https://registry-1.docker.io".to_string()],
            mounts: vec![Mount {
                target: Some("/var/lib/registry".to_string()),
                source: Some(crate::buildinfra::REGISTRY_DATA_VOL.to_string()),
                typ: Some(MountTypeEnum::VOLUME),
                ..Default::default()
            }],
            privileged: false,
            files: vec![],
        })
        .await?;

        self.ensure_infra_container(InfraSpec {
            name: crate::buildinfra::BUILDKIT_CONTAINER,
            image: cfg.buildkit_image.clone(),
            // moby/buildkit's ENTRYPOINT is `buildkitd`; these are its args.
            cmd: Some(vec![
                "--addr".to_string(),
                "tcp://0.0.0.0:1234".to_string(),
                "--config".to_string(),
                "/etc/buildkit/buildkitd.toml".to_string(),
            ]),
            env: vec![],
            mounts: vec![Mount {
                target: Some("/var/lib/buildkit".to_string()),
                source: Some(crate::buildinfra::BUILDKIT_CACHE_VOL.to_string()),
                typ: Some(MountTypeEnum::VOLUME),
                ..Default::default()
            }],
            privileged: true,
            files: vec![TarEntry {
                path: "etc/buildkit/buildkitd.toml".to_string(),
                data: crate::buildinfra::render_buildkitd_toml(cfg.buildkit_cache_gb).into_bytes(),
                mode: 0o644,
                uid: 0,
                gid: 0,
            }],
        })
        .await?;
        Ok(())
    }

    /// Ensure one infra container matches `spec`: create-if-absent (dropping `spec.files` in
    /// before start), start-if-stopped, recreate-if-image-drifted. Best-effort image pull
    /// first. Cache volumes are external (survive the recreate).
    async fn ensure_infra_container(&self, spec: InfraSpec) -> Result<()> {
        let docker = self.daemon()?;
        match docker
            .inspect_container(spec.name, None::<bollard::query_parameters::InspectContainerOptions>)
            .await
        {
            Ok(info) => {
                let running = info.state.as_ref().and_then(|s| s.running).unwrap_or(false);
                let cur_image =
                    info.config.as_ref().and_then(|c| c.image.clone()).unwrap_or_default();
                if cur_image != spec.image {
                    tracing::info!(
                        target: "docker",
                        "{}: image {cur_image:?} → {:?}, recreating",
                        spec.name, spec.image
                    );
                    self.stop_container(spec.name).await.ok();
                    self.remove_container(spec.name).await.ok();
                    // fall through to (re)create
                } else if running {
                    return Ok(()); // present, correct image, running
                } else {
                    self.start_container(spec.name).await?; // present + correct but stopped
                    return Ok(());
                }
            }
            Err(BollardError::DockerResponseServerError { status_code: 404, .. }) => {} // absent
            Err(e) => return Err(anyhow!("inspecting infra container {}: {e}", spec.name)),
        }

        self.pull_if_absent(&spec.image).await?;

        let host_config = HostConfig {
            privileged: Some(spec.privileged),
            mounts: Some(spec.mounts.clone()),
            restart_policy: Some(RestartPolicy {
                name: Some(RestartPolicyNameEnum::UNLESS_STOPPED),
                ..Default::default()
            }),
            ..Default::default()
        };
        let body = ContainerCreateBody {
            image: Some(spec.image.clone()),
            cmd: spec.cmd.clone(),
            env: if spec.env.is_empty() { None } else { Some(spec.env.clone()) },
            labels: Some(HashMap::from([(LABEL_INFRA.to_string(), "1".to_string())])),
            host_config: Some(host_config),
            networking_config: Some(NetworkingConfig {
                endpoints_config: Some(HashMap::from([(
                    NETWORK.to_string(),
                    EndpointSettings::default(),
                )])),
            }),
            ..Default::default()
        };
        let opts = CreateContainerOptionsBuilder::new().name(spec.name).build();
        let id = docker
            .create_container(Some(opts), body)
            .await
            .with_context(|| format!("creating infra container {}", spec.name))?
            .id;
        if !spec.files.is_empty() {
            // upload_tar works on a created-but-stopped container.
            self.upload_tar(&id, spec.files).await?;
        }
        self.start_container(&id).await?;
        tracing::info!(target: "docker", "ensured infra container {} ({})", spec.name, spec.image);
        Ok(())
    }

    /// Pull `reference` only if the daemon doesn't already have it (infra images are pinned;
    /// no need to re-pull each boot). Streams events into the void — infra pulls are silent.
    async fn pull_if_absent(&self, reference: &str) -> Result<()> {
        if self.daemon()?.inspect_image(reference).await.is_ok() {
            return Ok(());
        }
        tracing::info!(target: "docker", "pulling infra image {reference}");
        self.pull_image(reference, |_| {}).await
    }
```

And add the `InfraSpec` struct near `CreateSpec` (`docker.rs:246`):

```rust
/// A desired shared-infra container, the input to [`DockerCtl::ensure_infra_container`].
struct InfraSpec {
    name: &'static str,
    image: String,
    /// Args appended to the image ENTRYPOINT (`None` = image default).
    cmd: Option<Vec<String>>,
    env: Vec<String>,
    mounts: Vec<Mount>,
    privileged: bool,
    /// Files dropped into the created-but-not-started container (e.g. `buildkitd.toml`).
    files: Vec<TarEntry>,
}
```

- [ ] **Step 3: Wire the startup ensure in `main.rs`**

Immediately after the `self_setup` block closes (`main.rs:97`, before the `update::reconcile_pending` call):

```rust
    // Shared build infra (pull-through Hub mirror + remote BuildKit): ensure the two infra
    // containers exist + run. Gated on setup-complete + the master toggle; runs after
    // `self_setup` (which ensured the `rmng` network). Non-fatal + bounded — a down/slow
    // daemon (or a first-run image pull) logs and retries next boot, same posture as
    // `ensure_network`. 120 s covers a cold pull of registry + buildkit.
    {
        let cfg = app.config();
        if cfg.setup_complete && cfg.docker.build_infra_enabled {
            match tokio::time::timeout(
                std::time::Duration::from_secs(120),
                app.docker.ensure_build_infra(&cfg.docker),
            )
            .await
            {
                Ok(Ok(())) => {}
                Ok(Err(e)) => tracing::warn!("build-infra ensure failed: {e:#} (retries next boot)"),
                Err(_) => tracing::warn!("build-infra ensure timed out after 120s (retries next boot)"),
            }
        }
    }
```

- [ ] **Step 4: Build + existing tests + clippy**

Run: `cargo build -p control-server && cargo test -p control-server && cargo clippy -p control-server -- -D warnings`
Expected: compiles; all existing + Task 2 tests pass; no clippy errors. (Runtime behavior — containers actually coming up — is verified in Task 6; there is no unit test for daemon container ops.)

- [ ] **Step 5: Commit**

```bash
git add crates/control-server/src/docker.rs crates/control-server/src/main.rs
git commit -m "feat(docker): ensure_build_infra — auto-start rmng-registry + rmng-buildkit at boot"
```

---

### Task 4: `buildinfra` reconciler — migrate mirror + builder onto clones

**Files:**
- Modify: `crates/control-server/src/buildinfra.rs` (reconciler + per-clone ensures + `apply_to_clone`)
- Modify: `crates/control-server/src/main.rs:169` (spawn `buildinfra::run` next to `ssh::run`)
- Modify: `crates/control-server/src/provision.rs:301` (inline apply in the `Ok(())` arm)

**Interfaces:**
- Consumes: `App`, `app.config().docker.build_infra_enabled` (Task 1), `app.store.get().hosts` (`wire::Host`), `app.docker.{is_running, exec_script, upload_tar}`, `TarEntry`, the Task 2 consts + `merge_mirror_daemon_json`.
- Produces: `pub async fn run(app: App)` (reconciler loop); `pub async fn apply_to_clone(app: &App, clone_id: &str)` (best-effort one-shot, used by provision).

- [ ] **Step 1: Implement the reconciler + per-clone ensures**

Append to `crates/control-server/src/buildinfra.rs` (above the `#[cfg(test)] mod tests`). This consumes the `HashSet`/`Duration`/`App`/`TarEntry` imports added in Task 2:

```rust
/// How often to sweep running clones and apply the mirror + builder. A clone is configured
/// apply-once, then skipped (the artifacts persist in its writable layer / home).
const RECONCILE_INTERVAL: Duration = Duration::from_secs(30);

/// Sweep running managed clones forever, applying the mirror + remote builder to any not yet
/// confirmed. Idempotent + best-effort; disabled clones/off-toggle are simply skipped. Never
/// returns. Mirrors `ssh::run`'s loop shape.
pub async fn run(app: App) {
    let mut done: HashSet<String> = HashSet::new();
    loop {
        tokio::time::sleep(RECONCILE_INTERVAL).await;
        if !app.config().docker.build_infra_enabled {
            continue;
        }
        for host in app.store.get().hosts.into_iter().filter(|h| h.managed) {
            if done.contains(&host.id) {
                continue;
            }
            if app.docker.is_running(&host.id).await.unwrap_or(false) && try_apply(&app, &host.id).await {
                done.insert(host.id);
            }
        }
    }
}

/// Best-effort one-shot used by the provision path so a fresh clone is configured immediately
/// rather than waiting a reconcile tick. No-op when the feature is off. The reconciler is the
/// backstop if the clone's inner dockerd isn't up yet here.
pub async fn apply_to_clone(app: &App, clone_id: &str) {
    if !app.config().docker.build_infra_enabled {
        return;
    }
    let _ = try_apply(app, clone_id).await;
}

/// Apply both the mirror and the remote builder to one clone; returns true only if BOTH
/// succeeded (so the reconciler stops retrying). Failures log at `debug` — the inner dockerd
/// may simply not be up yet, and the reconciler will retry — while success logs `info`.
async fn try_apply(app: &App, clone_id: &str) -> bool {
    let mirror = ensure_clone_mirror(app, clone_id).await;
    if let Err(e) = &mirror {
        tracing::debug!(target: "buildinfra", "clone {clone_id}: mirror apply deferred: {e}");
    }
    let builder = ensure_clone_builder(app, clone_id).await;
    if let Err(e) = &builder {
        tracing::debug!(target: "buildinfra", "clone {clone_id}: builder apply deferred: {e}");
    }
    mirror.is_ok() && builder.is_ok()
}

/// Read the clone's `/etc/docker/daemon.json` (absent ⇒ empty), and if the mirror keys are
/// missing, write the merged file back and SIGHUP the inner dockerd. `registry-mirrors` +
/// `insecure-registries` are SIGHUP-reloadable, so no container or in-flight build is dropped.
async fn ensure_clone_mirror(app: &App, clone_id: &str) -> Result<()> {
    let mut current = String::new();
    app.docker
        .exec_script(
            clone_id,
            "cat /etc/docker/daemon.json 2>/dev/null || true\n",
            &[],
            &[],
            |stream, line| {
                if stream == "out" {
                    current.push_str(line);
                    current.push('\n');
                }
            },
        )
        .await
        .context("reading clone daemon.json")?;

    let Some(merged) = merge_mirror_daemon_json(&current)? else {
        return Ok(()); // already applied — no write, no SIGHUP
    };

    app.docker
        .upload_tar(
            clone_id,
            vec![TarEntry {
                path: "etc/docker/daemon.json".to_string(),
                data: merged.into_bytes(),
                mode: 0o644,
                uid: 0,
                gid: 0,
            }],
        )
        .await
        .context("writing clone daemon.json")?;

    // Reload the inner dockerd. Prefer its pidfile; fall back to pkill.
    let code = app
        .docker
        .exec_script(
            clone_id,
            "kill -HUP \"$(cat /run/docker.pid 2>/dev/null)\" 2>/dev/null || pkill -HUP dockerd\n",
            &[],
            &[],
            |_, line| tracing::debug!(target: "buildinfra", "hup: {line}"),
        )
        .await
        .context("reloading clone dockerd")?;
    if code != 0 {
        tracing::warn!(
            target: "buildinfra",
            "clone {clone_id}: dockerd HUP exited {code} (mirror written; a full inner-dockerd restart may be needed)"
        );
    }
    tracing::info!(target: "buildinfra", "clone {clone_id}: applied Hub mirror {REGISTRY_ADDR}");
    Ok(())
}

/// Register (as the uid-1000 clone user) a `--driver remote` buildx builder pointing at
/// `rmng-buildkit`, if not already present. `default-load=true` keeps `docker build && docker
/// run` transparent (the remote driver otherwise leaves the image only in BuildKit). Run via
/// `su - rmng` so buildx state lands in `~rmng/.docker` with the right HOME + docker-group.
async fn ensure_clone_builder(app: &App, clone_id: &str) -> Result<()> {
    let inner = format!(
        "docker buildx inspect {BUILDER_NAME} >/dev/null 2>&1 || \
         docker buildx create --name {BUILDER_NAME} --driver remote \
         --driver-opt default-load=true --use {BUILDKIT_ENDPOINT}"
    );
    let script = format!("set -e\nsu - {CLONE_USER} -c '{inner}'\n", CLONE_USER = crate::docker::CLONE_USER);
    let code = app
        .docker
        .exec_script(clone_id, &script, &[], &[], |_, line| {
            tracing::debug!(target: "buildinfra", "buildx: {line}")
        })
        .await
        .context("registering clone buildx builder")?;
    if code != 0 {
        anyhow::bail!("buildx builder setup exited {code}");
    }
    tracing::info!(target: "buildinfra", "clone {clone_id}: remote buildx builder → {BUILDKIT_ENDPOINT}");
    Ok(())
}
```

- [ ] **Step 2: Spawn the reconciler in `main.rs`**

At `main.rs:169`, immediately after `tokio::spawn(ssh::run(app.clone()));`:

```rust
    tokio::spawn(buildinfra::run(app.clone()));
```

- [ ] **Step 3: Inline apply in provision's success arm**

In `crates/control-server/src/provision.rs`, change the `Ok(())` arm of the `clone_container_after_create` match (`provision.rs:302`) from `Ok(()) => Ok(reference),` to:

```rust
        Ok(()) => {
            // Shared build infra: optimistically apply the Hub mirror + remote buildx builder
            // now (idempotent, best-effort; the buildinfra reconciler is the backstop if the
            // inner dockerd isn't up yet). No-op when the feature is off.
            crate::buildinfra::apply_to_clone(app, &container).await;
            Ok(reference)
        }
```

- [ ] **Step 4: Build + tests + clippy**

Run: `cargo build -p control-server && cargo test -p control-server && cargo clippy -p control-server -- -D warnings`
Expected: compiles clean (the Task 2 imports are now all consumed — no unused-import warnings remain); all tests pass.

- [ ] **Step 5: Commit**

```bash
git add crates/control-server/src/buildinfra.rs crates/control-server/src/main.rs crates/control-server/src/provision.rs
git commit -m "feat(buildinfra): reconciler + provision hook to migrate mirror + remote builder onto clones"
```

---

### Task 5: Documentation

**Files:**
- Modify: `docs/DEPLOY.md` (new "Shared build cache & Docker Hub mirror" section)
- Modify: `docs/PROXMOX-LXC.md:104-107` (note the mirror as the Hub rate-limit fix; clarify the shared cache is via BuildKit/registry, not shared `/var/lib/docker`)

**Interfaces:** none (docs only).

- [ ] **Step 1: Add the DEPLOY.md section**

Append a section to `docs/DEPLOY.md` (place near the clone-resource/limits material):

```markdown
## Shared build cache & Docker Hub mirror

The control-server automatically runs two shared infra containers on the `rmng` bridge
(labeled `rmng.infra=1`, started at boot, `restart: unless-stopped`):

- **`rmng-registry`** — a pull-through cache for Docker Hub. Every clone's base-image pulls
  are served from here after the first fleet-wide fetch, so `docker.io` rate limits stop
  biting. Cache lives in the `rmng-registry-data` volume.
- **`rmng-buildkit`** — a shared BuildKit daemon. Each clone's `docker build` transparently
  routes to it (via a `--driver remote` buildx builder named `rmng`), so identical layers are
  built once and reused across the whole fleet. Cache lives in `rmng-buildkit-cache`, capped
  at `docker.buildkitCacheGb` GiB (default 40).

No setup is needed — a fresh control-server ensures both containers, and a background
reconciler migrates the mirror config + builder onto every running clone within ~30 s (live,
no clone restart). Existing clones created before the upgrade are migrated the same way.

**Operator notes**
- Builds run on the shared `rmng-buildkit` daemon, so a build is **not** bounded by a single
  clone's CPU/memory limit and competes fleet-wide (fine for a small trusted fleet).
- If `rmng-buildkit` is down, in-clone `docker build` fails until it is back; the clone's local
  `default` builder remains as a manual fallback: `docker buildx use default`.
- Turn the whole feature off with `docker.buildInfraEnabled = false` in config.json (or
  Settings). This is a pure "stop managing": the reconciler stops touching clones and no infra
  is ensured; already-created infra containers and already-migrated clone config are left in
  place (remove them manually if you want them gone).
```

- [ ] **Step 2: Update the PROXMOX-LXC.md gotcha note**

At `docs/PROXMOX-LXC.md:107` (end of §3, after the `rmng-dind` overlay note), append:

```markdown

The fleet's Docker Hub pulls are de-duplicated by the shared `rmng-registry` pull-through cache
(the fix for `docker.io` rate limits), and build layers are shared via the `rmng-buildkit`
daemon — **not** by sharing `/var/lib/docker`, which concurrent daemons cannot do (hence the
per-clone `rmng-dind-*` / `rmng-ctd-*` volumes remain fully isolated). See DEPLOY.md → "Shared
build cache & Docker Hub mirror".
```

- [ ] **Step 3: Commit**

```bash
git add docs/DEPLOY.md docs/PROXMOX-LXC.md
git commit -m "docs: shared build cache + Hub mirror (DEPLOY + PROXMOX-LXC notes)"
```

---

### Task 6: End-to-end on a fresh Proxmox LXC (the gate)

**Files:** none (manual verification; capture the transcript into this task's checkboxes as you go).

**Prereq:** the branch's control-server image built and reachable (build + push per `docs/DEPLOY.md`, or `docker save`/`scp` the image tar to the CT). Proxmox host is `root@10.0.0.100`.

This is the pass/fail gate — it exercises first-boot auto-start and provisioning from a clean host (no warm state). Do NOT skip it; it cannot be faked in a unit test.

- [ ] **Step 1: Create a fresh unprivileged LXC per `docs/PROXMOX-LXC.md`**

On `root@10.0.0.100` (pick an unused CTID, e.g. `991`; adjust storage/template names to what the node has):

```bash
# host keyring quotas (PROXMOX-LXC.md §1b) — once per host, harmless to re-apply
cat >> /etc/sysctl.d/99-rmng-keys.conf <<EOF
kernel.keys.maxkeys = 20000
kernel.keys.maxbytes = 2000000
EOF
sysctl --system

pct create 991 local:vztmpl/ubuntu-26.04-standard_*_amd64.tar.zst \
  --hostname rmng-e2e --cores 16 --memory 32768 --swap 8192 \
  --rootfs local-lvm:64 --net0 name=eth0,bridge=vmbr0,ip=dhcp \
  --features nesting=1,keyctl=1,fuse=1 --unprivileged 1

# Append the passthrough + AppArmor lines PROXMOX-LXC.md §1 requires:
cat >> /etc/pve/lxc/991.conf <<'EOF'
dev0: /dev/dri/renderD128,mode=0666
lxc.apparmor.profile: unconfined
lxc.mount.entry: /dev/null sys/module/apparmor/parameters/enabled none bind,optional 0 0
lxc.mount.auto: cgroup:mixed proc:rw sys:mixed
EOF

pct start 991
```

Expected: `pct status 991` → `running`.

- [ ] **Step 2: Install + verify Docker in the CT (PROXMOX-LXC.md §2/§3)**

```bash
pct exec 991 -- bash -lc 'apt-get update && apt-get install -y curl ca-certificates && curl -fsSL https://get.docker.com | sh'
pct exec 991 -- bash -lc 'docker info | grep -i "storage driver"; ls -l /dev/dri/renderD128; docker run --rm hello-world'
```

Expected: storage driver `overlay2`/`overlayfs` (NOT `vfs`); render node present; `hello-world` runs.

- [ ] **Step 3: Deploy this branch's control-server + run the wizard**

Load the branch image into the CT and start it (per DEPLOY.md — `docker compose up -d` or the `docker run` one-liner), then open `http://<ct-ip>:9000` and complete the setup wizard (subnet, prefix, monitors, limits, template reference).

- [ ] **Step 4: Assert auto-start from zero**

```bash
pct exec 991 -- docker ps --filter label=rmng.infra=1 --format '{{.Names}} {{.Image}} {{.Status}}'
```

Expected: both `rmng-registry` and `rmng-buildkit` listed, `Up …`. Confirm they are on the bridge:
`pct exec 991 -- docker inspect -f '{{json .NetworkSettings.Networks}}' rmng-buildkit` shows the `rmng` network.
Confirm the GC config landed: `pct exec 991 -- docker exec rmng-buildkit cat /etc/buildkit/buildkitd.toml` shows the `keepBytes` you configured. (If buildkit failed to start on the `keepBytes`/`gcpolicy` key, switch `render_buildkitd_toml` to the legacy `gckeepstorage` form per its NOTE and redeploy.)

- [ ] **Step 5: Transparent build via shared BuildKit (clone A)**

Create a clone in the UI (call it A). Then, inside clone A:

```bash
# from the control-server host: exec into clone A's inner docker
pct exec 991 -- docker exec <cloneA> su - rmng -c '
  docker buildx ls;                                   # shows the rmng remote builder, *=default
  printf "FROM alpine\nRUN echo hi > /x\n" > /tmp/Dockerfile;
  docker build -t e2e:a /tmp && docker run --rm e2e:a cat /x'
```

Expected: `docker buildx ls` lists `rmng` (remote, current); the build routes to buildkit; `docker run` prints `hi` (proves `default-load`). Cross-check `pct exec 991 -- docker logs rmng-buildkit` shows the build.

- [ ] **Step 6: Cross-clone layer-cache hit (clone B)**

Create clone B, run the **identical** Dockerfile build:

Expected: the `RUN echo hi` step reports `CACHED` (near-instant) — proof the layer cache is shared across clones via `rmng-buildkit`.

- [ ] **Step 7: Pull-through mirror hit**

```bash
pct exec 991 -- docker exec <cloneA> su - rmng -c 'docker pull ubuntu:26.04'
pct exec 991 -- docker exec <cloneB> su - rmng -c 'docker pull ubuntu:26.04'
pct exec 991 -- docker logs rmng-registry | tail
```

Expected: clone B's pull is served by `rmng-registry` (registry logs show the blob served from cache; no fresh Hub round-trip). Confirm each clone's `daemon.json`:
`pct exec 991 -- docker exec <cloneA> cat /etc/docker/daemon.json` shows both `registry-mirrors` and `insecure-registries`.

- [ ] **Step 8: Live migration of a pre-existing clone (no drop)**

Simulate an upgrade over a clone with running inner work: create clone C, start a long-lived inner container in it, and record it:

```bash
pct exec 991 -- docker exec <cloneC> su - rmng -c 'docker run -d --name keepme alpine sleep 3600; docker ps'
# Redeploy / restart the control-server container (docker compose up -d, or restart rmng)
# Wait ~30s for the reconciler, then:
pct exec 991 -- docker exec <cloneC> su - rmng -c 'docker ps; docker buildx ls; cat /etc/docker/daemon.json'
```

Expected: `keepme` is **still running** throughout (the SIGHUP reload did not restart the inner dockerd); the `rmng` builder and the mirror `daemon.json` are now present on clone C.

- [ ] **Step 9: Toggle off**

Set `docker.buildInfraEnabled = false` (Settings/config.json), restart the control-server, create a new clone D.

Expected: clone D gets **no** mirror/builder (reconciler inert); the `rmng-registry`/`rmng-buildkit` containers are left in place (pure stop-managing, per the documented semantics).

- [ ] **Step 10: Teardown**

```bash
pct stop 991 && pct destroy 991
```

Leave `10.0.0.100` clean. Record any deviations (buildkit toml key, exact image tags) back into Task 2 / the spec's open items and re-commit if changed.

---

## Self-Review

**Spec coverage:**
- Pull-through mirror container (`rmng-registry`) → Task 3 (`ensure_build_infra`), clone-side in Task 4 (`ensure_clone_mirror`). ✓
- Shared BuildKit (`rmng-buildkit`) + remote builder → Task 3 + Task 4 (`ensure_clone_builder`). ✓
- Auto-start at boot, gated + non-fatal → Task 3 Step 3. ✓
- Live migration reconciler + provision inline → Task 4. ✓
- `rmng.infra=1` (not `rmng.managed=1`) → Task 3 Step 1 + `LABEL_INFRA` usage. ✓
- Config fields + defaults + no-env invariant → Task 1. ✓
- `daemon.json` merge (empty/existing/already-applied) → Task 2 tests + `merge_mirror_daemon_json`. ✓
- HTTP mirror requires insecure entry → enforced in `merge_mirror_daemon_json`; asserted by `merge_into_empty_adds_both_keys`. ✓
- SIGHUP-reload no-drop → Task 4 `ensure_clone_mirror` + E2E Step 8. ✓
- GC cap → Task 2 `render_buildkitd_toml` + Task 3 wiring + E2E Step 4. ✓
- `build_infra_enabled=false` semantics (stop-managing, no teardown) → gates in Tasks 3/4 + DEPLOY.md + E2E Step 9. ✓
- Docs (DEPLOY + PROXMOX-LXC) → Task 5. ✓
- Fresh-LXC E2E gate → Task 6. ✓

**Deliberate deviations from the spec's testing list:** the spec listed a unit test for infra-container drift; the drift check is a plain image-ref `!=` comparison (`ensure_infra_container`), so it is verified via the E2E config-change path (Task 6 Step 4's redeploy / a `registry_image` change) rather than a vacuous `assert_ne!` unit test.

**Placeholder scan:** the only intentional placeholder is the exact `moby/buildkit` patch tag (`v0.17.2`) and the `keepBytes`-vs-`gckeepstorage` toml key — both flagged in Global Constraints, the `render_buildkitd_toml` NOTE, and confirmed/bumped in Task 6. No `TODO`/"add error handling"/uncoded steps remain.

**Type consistency:** `merge_mirror_daemon_json`, `render_buildkitd_toml`, `ensure_build_infra`, `ensure_infra_container`, `pull_if_absent`, `apply_to_clone`, `run`, `ensure_clone_mirror`, `ensure_clone_builder`, `InfraSpec`, `LABEL_INFRA`, and the `REGISTRY_*`/`BUILDKIT_*`/`BUILDER_NAME` consts are named identically at every definition and call site across Tasks 2–4. `crate::docker::CLONE_USER` (existing, `docker.rs:62`) is reused for the `su - rmng` step.
