//! Shared Docker build infra for the clone fleet: a pull-through Docker Hub cache
//! (`rmng-registry`) and a shared BuildKit daemon (`rmng-buildkit`), plus the reconciler
//! that migrates the mirror config + a remote buildx builder onto every running clone.
//! Mirrors `crate::homes`'s reconciler shape. The two infra containers are ensured by
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

/// How often to sweep running clones and apply the mirror + builder. A clone is configured
/// apply-once, then skipped (the artifacts persist in its writable layer / home).
const RECONCILE_INTERVAL: Duration = Duration::from_secs(30);

/// Sweep running managed clones forever, applying the mirror + remote builder to any not yet
/// confirmed. Idempotent + best-effort; disabled clones/off-toggle are simply skipped. Never
/// returns. Mirrors `homes::run`'s loop shape.
pub async fn run(app: App) {
    let mut done: HashSet<String> = HashSet::new();
    loop {
        tokio::time::sleep(RECONCILE_INTERVAL).await;
        if !app.config().docker.build_infra_enabled {
            continue;
        }
        let managed: Vec<_> = app.store.get().hosts.into_iter().filter(|h| h.managed).collect();
        for host in &managed {
            if done.contains(&host.id) {
                continue;
            }
            if app.docker.is_running(&host.id).await.unwrap_or(false) && try_apply(&app, &host.id).await {
                done.insert(host.id.clone());
            }
        }
        // Keep the once-applied set bounded to clones that still exist + are managed.
        let current_managed_ids: HashSet<String> = managed.iter().map(|h| h.id.clone()).collect();
        done.retain(|id| current_managed_ids.contains(id));
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
    fn merge_replaces_malformed_scalar_with_array() {
        // `registry-mirrors` present but a scalar string (not an array) → the malformed value
        // is replaced by a proper array carrying our mirror.
        let existing = r#"{"registry-mirrors":"http://legacy:5000"}"#;
        let out = merge_mirror_daemon_json(existing).unwrap().expect("must write");
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        let mirrors = v["registry-mirrors"].as_array().expect("scalar replaced by array");
        assert!(mirrors.iter().any(|m| m == "http://rmng-registry:5000"), "ours present");
        assert_eq!(v["insecure-registries"][0], "rmng-registry:5000");
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
