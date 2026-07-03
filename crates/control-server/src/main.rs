//! control-server — the fleet hub (see ../README.md).
//!
//! One tokio service binding five listen ports — port 1 (video), port 2 (web API + SSE +
//! static frontend), port 3 (per-clone MCP), port 4 (fleet MCP), and the forward data plane
//! (9005) — plus an smbd `clones` share on 445 that surfaces every clone's home. All live.

mod app;
mod assets;
mod binswap;
mod chat;
mod claude;
mod config;
mod docker;
mod files;
mod forward;
mod homes;
mod jobs;
mod linear;
mod mcp;
mod mediaplane;
mod monitor;
mod provision;
mod smb;
mod state;
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
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info,tower_http=warn,clip=debug")),
        )
        .init();

    let cfg = config::load()?;
    let store = Arc::new(state::StateStore::load(config::state_path(&cfg))?);
    state::spawn_watcher(store.clone());

    let app = app::App::new(store, cfg);

    // A persisted `Running` operation is a corpse from a server that crashed/was killed
    // mid-op (an `Operation` lives only while its driving task runs). Mark such ops `Error`
    // + prune them, so a same-named clone/pull/commit isn't blocked forever by the in-flight
    // guards. State-only — safe with Docker down.
    jobs::fail_stale_ops(&app);

    // Probe the Docker environment (daemon reachable, self-container detection, sock mount,
    // render node) and cache the report so `GET /api/setup/env` + the wizard can render it.
    // Non-fatal: a down daemon / failed check must NOT stop the server booting — the wizard
    // is exactly where the operator fixes those. `ensure_network` only runs here once setup
    // is latched complete (the network is lazy).
    // Bounded: the shared bollard client's request timeout is 1 h (a base-image commit
    // legitimately runs that long), so a wedged-but-connectable daemon would otherwise
    // block THIS await — and with it the whole server boot — for up to an hour.
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

    // Boot reconciliation: `state.json` is authoritative for host rows, but the daemon is
    // authoritative for what actually exists — diff them once so drift is visible instead
    // of silent. Orphan rows (managed host, no container — someone `docker rm`ed it behind
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
                            "reconcile: managed host '{}' has no container on the daemon \
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
                        "reconcile: managed container '{}' (image {}, {}) has no host row — \
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

    // Background loops: Claude usage poller, group-rotation loop, per-host agent-state
    // poller, the clone-home reconciler (the Docker-port successor to the Proxmox-era sshfs
    // mount loop — it symlinks data/hosts/<id> → /proc/<uid-1000-pid>/root/home/rmng so
    // every clone's home is browsable in one place; needs the container's `pid: "host"`),
    // and the smbd supervisor that serves that same directory as the `clones` SMB share
    // (port 445), so the homes are browsable over `smb://<host>/clones` too.
    tokio::spawn(claude::run_poller(app.clone()));
    tokio::spawn(claude::run_rotator(app.clone()));
    tokio::spawn(monitor::run(app.clone()));
    tokio::spawn(homes::run(app.clone()));
    tokio::spawn(smb::run(app.clone()));

    // Automatic hash-based binary hot-swap engine: warm the expected payload hashes, then
    // run the worker + sweep loops. Spawned BEFORE the media plane so its enqueue channel
    // is live before the first clone `Hello` (a later task fires `request_check` from there).
    binswap::spawn(app.clone());

    // Port 1 (video) — ingest clone dmabufs, VA-API encode, serve the viewer.
    mediaplane::spawn(app.clone());

    // Ports 3 (per-clone MCP, header-routed via `x-rmng-clone`) + 4 (global MCP).
    {
        let cfg = app.config();
        tokio::spawn(mcp::serve(app.clone(), cfg.listen.clone_mcp, mcp::Scope::PerClone));
        tokio::spawn(mcp::serve(app.clone(), cfg.listen.global_mcp, mcp::Scope::Global));
    }

    web::serve(app).await
}
