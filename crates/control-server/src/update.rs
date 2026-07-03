//! Control-server self-update: the `self-upgrade` helper subcommand + boot reconcile.
//!
//! A container can't `docker rm -f` and recreate itself atomically. So `jobs::run_update`
//! pulls the new image, captures our run-spec, writes a [`Handoff`] into the `/data` volume,
//! and launches a DETACHED helper container from the NEW image running `rmng-control-server
//! self-upgrade <handoff>`. That helper ([`self_upgrade_main`]) stops+removes the old
//! container and recreates it from the new image. On the next boot the fresh server runs
//! [`reconcile_pending`] to resolve the surviving update Operation.

use std::path::Path;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::app::App;
use crate::docker::SelfSpec;

/// The handoff file, in the `/data` volume so it survives the container swap.
pub const HANDOFF_PATH: &str = "/data/update-handoff.json";

/// Everything the `self-upgrade` helper + the post-reboot reconcile need.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Handoff {
    pub spec: SelfSpec,
    /// The `Operation` id to resolve on the next boot.
    pub op_id: String,
    /// The remote digest we pulled — reconcile compares the new running image against it.
    #[serde(default)]
    pub target_digest: Option<String>,
}

/// Serialize + write the handoff atomically (temp + rename) into the `/data` volume.
pub fn write_handoff(h: &Handoff) -> Result<()> {
    let tmp = format!("{HANDOFF_PATH}.tmp");
    std::fs::write(&tmp, serde_json::to_vec_pretty(h)?).context("writing handoff temp")?;
    std::fs::rename(&tmp, HANDOFF_PATH).context("renaming handoff into place")?;
    Ok(())
}

/// Remove the handoff file if present (idempotent).
pub fn clear_handoff() {
    let _ = std::fs::remove_file(HANDOFF_PATH);
}

/// The `self-upgrade` subcommand entry — runs on the process's existing tokio runtime
/// (main is `#[tokio::main]`) and never returns to normal server boot. A fresh process from
/// the NEW image, launched detached by `jobs::run_update`. Reads the handoff, stops+removes
/// the old container, and recreates it from the new image. On a create/start failure it
/// recreates the OLD image so the host is never left with nothing running. The process exits
/// when done.
pub async fn self_upgrade_main(handoff_path: &str) -> ! {
    let code = run_helper(handoff_path).await;
    std::process::exit(code);
}

async fn run_helper(handoff_path: &str) -> i32 {
    tracing::info!(target: "update", "self-upgrade helper starting ({handoff_path})");
    let raw = match std::fs::read(handoff_path) {
        Ok(r) => r,
        Err(e) => {
            tracing::error!(target: "update", "reading handoff {handoff_path}: {e}");
            return 1;
        }
    };
    let handoff: Handoff = match serde_json::from_slice(&raw) {
        Ok(h) => h,
        Err(e) => {
            tracing::error!(target: "update", "parsing handoff: {e}");
            return 1;
        }
    };
    // Build a bollard client from config (respects a custom docker.socket). config.json is in
    // the /data volume, which is mounted into the helper too, so config::load() works here.
    let cfg = match crate::config::load() {
        Ok(c) => c,
        Err(e) => {
            tracing::error!(target: "update", "loading config in helper: {e}");
            return 1;
        }
    };
    let docker = crate::docker::DockerCtl::connect(&cfg.docker);
    let spec = &handoff.spec;

    // Stop + remove the old container (frees the name + published ports).
    if let Err(e) = docker.stop_container(&spec.container_name).await {
        tracing::warn!(target: "update", "stopping old container: {e}");
    }
    if let Err(e) = docker.remove_container(&spec.container_name).await {
        tracing::error!(target: "update", "removing old container: {e}");
        return 1;
    }

    // Recreate from the NEW image.
    match docker.create_and_start_from_spec(spec).await {
        Ok(id) => {
            tracing::info!(target: "update", "recreated {} on new image ({})", spec.container_name, id);
            0
        }
        Err(e) => {
            tracing::error!(target: "update", "recreate on new image failed: {e} — rolling back to old image");
            // Create-error fallback: recreate from the OLD image so the host isn't bricked.
            let mut fallback = spec.clone();
            fallback.new_image_ref = spec.old_image_id.clone();
            match docker.create_and_start_from_spec(&fallback).await {
                Ok(_) => tracing::warn!(target: "update", "rolled back to old image"),
                Err(e2) => tracing::error!(target: "update", "rollback ALSO failed: {e2}"),
            }
            1
        }
    }
}

/// Boot-time resolution of a surviving update Operation. Runs BEFORE `jobs::fail_stale_ops`
/// so it isn't clobbered as "interrupted". Reads the handoff, compares our new running image
/// digest to the target, marks the op Done/Error, and clears the handoff. Best-effort: a
/// missing handoff (normal boot) is a no-op; a daemon-unreachable boot completes optimistically.
pub async fn reconcile_pending(app: &App) {
    if !Path::new(HANDOFF_PATH).exists() {
        return;
    }
    let raw = match std::fs::read(HANDOFF_PATH) {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!(target: "update", "reading handoff at boot: {e}");
            clear_handoff();
            return;
        }
    };
    let handoff: Handoff = match serde_json::from_slice(&raw) {
        Ok(h) => h,
        Err(e) => {
            tracing::warn!(target: "update", "parsing handoff at boot: {e}");
            clear_handoff();
            return;
        }
    };
    let op_id = handoff.op_id.clone();
    let self_id = app.docker.env().await.self_container;
    let (repo, _tag) = crate::docker::split_reference(&handoff.spec.new_image_ref);

    // Determine the running image digest (best-effort).
    let now_digest = match self_id.as_deref() {
        Some(id) => app.docker.self_image_info(id, &repo).await.ok().and_then(|i| {
            i.repo_digest.map(|rd| rd.split_once('@').map(|(_, d)| d.to_string()).unwrap_or(rd))
        }),
        None => None,
    };
    let (done, msg) = match (&handoff.target_digest, &now_digest) {
        (Some(target), Some(now)) if now == target => (true, "update complete".to_string()),
        (Some(target), Some(now)) => (
            false,
            format!("update did not take effect (running {now}, expected {target})"),
        ),
        // Couldn't verify (daemon down / no repo digest): optimistic, since this new binary
        // is running at all.
        _ => (true, "update complete (digest unverified)".to_string()),
    };

    app.store.mutate(|s| {
        if let Some(op) = s.operations.iter_mut().find(|o| o.id == op_id) {
            op.status = if done { wire::OperationStatus::Done } else { wire::OperationStatus::Error };
            op.step = "done".into();
            op.pct = 100.0;
            op.message = msg.clone();
            op.log.push(msg.clone());
            op.finished_at = Some(chrono_now_ms());
        }
    });
    tracing::info!(target: "update", "reconciled update op {op_id}: {msg}");
    clear_handoff();
    // Best-effort: remove the leftover helper container.
    let _ = app.docker.remove_container("rmng-self-upgrade").await;
}

/// Milliseconds since epoch (matches jobs.rs `now_ms`, which is private to that module).
fn chrono_now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn handoff_round_trips() {
        let json = r#"{
            "Id": "abc", "Name": "/rmng", "Image": "sha256:old",
            "Config": {}, "HostConfig": {}, "NetworkSettings": { "Networks": {} }
        }"#;
        let resp: bollard::models::ContainerInspectResponse = serde_json::from_str(json).unwrap();
        let spec = SelfSpec::from_inspect(&resp, "pegasis0/rmng:latest").unwrap();
        let h = Handoff { spec, op_id: "op_1".into(), target_digest: Some("sha256:new".into()) };
        let s = serde_json::to_string(&h).unwrap();
        let back: Handoff = serde_json::from_str(&s).unwrap();
        assert_eq!(back.op_id, "op_1");
        assert_eq!(back.target_digest.as_deref(), Some("sha256:new"));
        assert_eq!(back.spec.new_image_ref, "pegasis0/rmng:latest");
    }

    #[test]
    fn handoff_defaults_target_digest_when_absent() {
        // The `#[serde(default)]` omission path: a handoff serialized with `None`, and an
        // external payload that omits the key entirely, both deserialize to `target_digest:
        // None` while preserving the nested SelfSpec.
        let json = r#"{
            "Id": "abc", "Name": "/rmng-server", "Image": "sha256:old",
            "Config": {}, "HostConfig": {}, "NetworkSettings": { "Networks": {} }
        }"#;
        let resp: bollard::models::ContainerInspectResponse = serde_json::from_str(json).unwrap();
        let spec = SelfSpec::from_inspect(&resp, "pegasis0/rmng:latest").unwrap();

        // Round-trip through our own serializer with `None`.
        let h = Handoff { spec, op_id: "op_2".into(), target_digest: None };
        let back: Handoff = serde_json::from_str(&serde_json::to_string(&h).unwrap()).unwrap();
        assert_eq!(back.op_id, "op_2");
        assert_eq!(back.target_digest, None);
        assert_eq!(back.spec.container_name, "rmng-server");
        assert_eq!(back.spec.old_image_id, "sha256:old");

        // An external payload that omits `target_digest` altogether still parses (default).
        let minimal = r#"{
            "spec": {
                "container_name": "rmng-server", "new_image_ref": "pegasis0/rmng:latest",
                "old_image_id": "sha256:old", "config": {}, "host_config": {}, "networks": {}
            },
            "op_id": "op_3"
        }"#;
        let parsed: Handoff = serde_json::from_str(minimal).unwrap();
        assert_eq!(parsed.op_id, "op_3");
        assert_eq!(parsed.target_digest, None);
        assert_eq!(parsed.spec.old_image_id, "sha256:old");
    }
}
