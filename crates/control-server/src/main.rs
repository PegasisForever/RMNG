//! control-server — the fleet hub (see ../README.md).
//!
//! One tokio service binding the video plane, web API + SSE + static frontend, port-forward
//! data plane (9005), and SSH bastion; `smbd` serves retained clone homes on port 445.

mod app;
mod assets;
mod boot;
mod buildinfra;
mod cgroup;
mod chat;
mod claude;
mod clone_ops;
mod clonekey;
mod clone_reconcile;
mod codex;
mod config;
mod docker;
mod files;
mod forward;
mod homes;
mod jobs;
mod linear;
mod mediaplane;
mod monitor;
mod provision;
mod shm;
mod smb;
mod ssh;
mod state;
mod termplane;
mod token_unmigrate;
mod update;
mod web;

use std::sync::Arc;

use anyhow::Result;

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                // `clip` (the clipboard broker) logs debug by default: copy/paste-driven
                // only (sparse), and the go-to trail for cross-machine clipboard issues.
                .unwrap_or_else(|_| {
                    tracing_subscriber::EnvFilter::new("info,tower_http=warn,clip=debug")
                }),
        )
        .init();

    // Self-upgrade helper mode (detached container from the NEW image). A container launched by
    // `jobs::run_update` runs `rmng-control-server self-upgrade <handoff>`: it stops+removes the
    // old container and recreates it, then exits. Diverges — never becomes the normal server.
    // Placed after tracing init (so the helper gets logging) and before config::load().
    let argv: Vec<String> = std::env::args().collect();
    if argv.get(1).map(String::as_str) == Some("self-upgrade") {
        let handoff = argv
            .get(2)
            .cloned()
            .unwrap_or_else(|| update::HANDOFF_PATH.to_string());
        update::self_upgrade_main(&handoff).await; // diverges
    }

    let cfg = config::load()?;
    let store = Arc::new(state::StateStore::load(config::state_path(&cfg))?);
    state::spawn_watcher(store.clone());

    // Snapshot each clone's retired `group` binding BEFORE anything mutates the state store.
    // `RmngClone` has no such field any more, so the first `store.mutate` below persists
    // `state.json` without it and the binding is unrecoverable — see `read_raw_clone_pools`.
    let pools_before = token_unmigrate::read_raw_clone_pools(&cfg);

    let app = app::App::new(store, cfg);

    // Seed ControlState with the config's active layout + preset names so the sidebar
    // switcher renders correctly on a fresh boot, before any `/api/config` PUT or
    // `/api/layout/activate` call runs.
    web::mirror_layout_to_state(&app);

    // Probe the Docker environment (daemon reachable, self-container detection, sock mount,
    // render node) and cache the report so `GET /api/setup/env` + the wizard can render it.
    // Non-fatal: a down daemon / failed check must NOT stop the server booting — the wizard
    // is exactly where the operator fixes those. `ensure_network` only runs here once setup
    // is latched complete (the network is lazy).
    // Bounded: the shared bollard client's request timeout is 1 h (a base-image commit
    // legitimately runs that long), so a wedged-but-connectable daemon would otherwise
    // block THIS await — and with it the whole server boot — for up to an hour.
    // Runs BEFORE `reconcile_pending`: self_setup is what populates the cached env report with
    // the detected self-container id, and reconcile needs that id to inspect the running image
    // and verify the update digest. (Daemon down/unresponsive → this times out, the env cache
    // stays default, and reconcile correctly falls back to the optimistic "digest unverified".)
    {
        let setup_complete = app.config().setup_complete;
        match tokio::time::timeout(
            std::time::Duration::from_secs(30),
            app.docker.self_setup(setup_complete),
        )
        .await
        {
            Ok(report) if report.required_ok() => {}
            Ok(_) => tracing::error!(
                "Docker self-setup reported failing required checks; the server is up so the \
                 setup wizard can show the details (GET /api/setup/env)"
            ),
            Err(_) => tracing::error!(
                "Docker self-setup timed out after 30s (daemon connected but unresponsive?); \
                 booting anyway — retry via the wizard's env checklist"
            ),
        }
    }

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
                Ok(Err(e)) => {
                    tracing::warn!("build-infra ensure failed: {e:#} (retries next boot)")
                }
                Err(_) => {
                    tracing::warn!("build-infra ensure timed out after 120s (retries next boot)")
                }
            }
        }
    }

    // A persisted `Running` operation is a corpse from a server that crashed/was killed
    // mid-op (an `Operation` lives only while its driving task runs). Mark such ops `Error`
    // + prune them, so a same-named clone/pull/commit isn't blocked forever by the in-flight
    // guards. State-only — safe with Docker down.
    //
    // Resolve a surviving self-update Operation FIRST, before fail_stale_ops would clobber it as
    // "interrupted". self_setup above already populated the env cache with our self-container id,
    // so reconcile's running-image digest check can actually run. Best-effort; a no-op when the
    // handoff is absent (normal boot).
    update::reconcile_pending(&app).await;
    jobs::fail_stale_ops(&app);

    // Boot reconciliation: `state.json` is authoritative for clone rows, but the daemon is
    // authoritative for what actually exists — diff them once so drift is visible instead
    // of silent. Orphan rows (managed clone, no container — someone `docker rm`ed it behind
    // the server) and unknown managed containers (a container with our label but no row —
    // e.g. a build worker left over from a crashed bootstrap) are LOGGED, not auto-fixed:
    // deleting either side automatically could destroy something the operator wanted.
    // Best-effort + bounded — a down/wedged daemon skips this (same posture as self-setup).
    {
        match tokio::time::timeout(
            std::time::Duration::from_secs(10),
            app.docker.list_managed_containers(),
        )
        .await
        {
            Ok(Ok(live)) => {
                let live_names: std::collections::HashSet<&str> =
                    live.iter().map(|c| c.name.as_str()).collect();
                let hosts = app.store.get().hosts;
                for h in hosts.iter().filter(|h| h.managed) {
                    if !live_names.contains(h.id.as_str()) {
                        tracing::warn!(
                            "reconcile: managed clone '{}' has no container on the daemon \
                             (removed behind the server?) — delete the row in the UI or \
                             recreate the clone",
                            h.id
                        );
                    }
                }
                let known: std::collections::HashSet<&str> =
                    hosts.iter().map(|h| h.id.as_str()).collect();
                for c in live.iter().filter(|c| !known.contains(c.name.as_str())) {
                    tracing::warn!(
                        "reconcile: managed container '{}' (image {}, {}) has no clone row — \
                         a leftover from a crashed operation? Remove it with `docker rm`",
                        c.name,
                        c.image,
                        if c.running { "running" } else { "stopped" }
                    );
                }
            }
            Ok(Err(e)) => tracing::warn!("reconcile: listing managed containers failed: {e:#}"),
            Err(_) => tracing::warn!("reconcile: listing managed containers timed out after 10s"),
        }
    }

    // One-shot reverse token migration: recover the OAuth credentials the retired group-proxy
    // era left in the per-group CLIProxyAPI `auth-dir`s and write them back into the RMNG-owned
    // stores (claude-accounts.json / codex-accounts.json + cloneGroups/codexGroups), so an
    // upgraded deployment carries every account across with no operator re-login. Stamp-gated
    // (runs once) and best-effort: it must NOT block boot, so any panic is caught and logged.
    //
    // ORDERING IS LOAD-BEARING. The `rmng-cliproxy` sidecar is torn down FIRST: while it runs it
    // keeps per-group CLIProxyAPI processes alive, and those refresh OAuth tokens on their own
    // schedule. Since a refresh token is single-use, a rotation landing after we copy a
    // credential would invalidate the copy — leaving dead tokens in the stores and forcing a
    // re-login of every account, exactly what this migration exists to avoid.
    app.docker.remove_retired_group_proxy().await;
    if let Err(e) = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        token_unmigrate::unmigrate_group_proxy_tokens(&app, &pools_before)
    })) {
        tracing::error!("group-proxy token reverse-migration panicked (booting anyway): {e:?}");
    }

    // GStreamer init MUST finish before smb/ssh (and any other child
    // spawners). Those supervisors otherwise inherit gst-plugin-scanner pipes and
    // hang media init forever — web/video/forward never bind. See
    // docs/superpowers/specs/2026-07-24-gstreamer-init-before-children-design.md.
    let app_for_bg = app.clone();
    let app_for_media = app.clone();
    boot::run_late_boot(
        mediaplane::init,
        move || {
            // Background loops: the per-clone agent-state monitor poller, the clone-home reconciler
            // (the Docker-port successor to the Proxmox-era sshfs mount loop — it symlinks
            // data/hosts/<id> → /proc/<uid-1000-pid>/root/home/rmng so every clone's home is browsable
            // in one place; needs the container's `pid: "host"`), the smbd supervisor that serves that
            // same directory as the `clones` SMB share (port 445), so the homes are browsable over
            // `smb://<host>/clones` too, and the /dev/shm reconciler that keeps each running clone's
            // shared memory at LXC parity (~50% of RAM) so Chromium/Electron apps don't exhaust
            // Docker's 64 MB default (also needs `pid: "host"`).
            tokio::spawn(monitor::run(app_for_bg.clone()));
            tokio::spawn(clone_reconcile::run(app_for_bg.clone()));
            tokio::spawn(homes::run(app_for_bg.clone()));
            tokio::spawn(shm::run(app_for_bg.clone()));
            tokio::spawn(buildinfra::run(app_for_bg.clone()));
            // Scheduled chat delivery: fires operator-queued messages once their time passes.
            // Disk-backed, so anything that came due during a restart goes out on the first tick.
            tokio::spawn(chat::run_scheduler(app_for_bg.clone()));
            tokio::spawn(smb::run(app_for_bg.clone()));
            tokio::spawn(ssh::run(app_for_bg.clone()));
            // Claude + Codex account subsystems. Each provider runs a usage poller (which
            // also refreshes tokens and fans any rotation out to the clones running that
            // account) and a rotator (which re-balances group-bound clones off an exhausted
            // account). Together they are the only thing that writes a clone's credential
            // files — the reconciler deliberately has no token logic.
            tokio::spawn(claude::run_poller(app_for_bg.clone()));
            tokio::spawn(claude::run_rotator(app_for_bg.clone()));
            tokio::spawn(codex::run_poller(app_for_bg.clone()));
            tokio::spawn(codex::run_rotator(app_for_bg.clone()));
        },
        move |media_init| {
            // Port 1 (video) — ingest clone dmabufs, VA-API encode, serve the viewer.
            mediaplane::spawn(app_for_media, media_init);
        },
    );

    web::serve(app).await
}
