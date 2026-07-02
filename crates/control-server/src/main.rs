//! control-server — the four-port hub (see ../README.md).
//!
//! Phase 1 brings up **port 2** (web API + SSE + static frontend) on top of the
//! state store. Ports 1 (video), 3 (per-clone MCP), and 4 (global MCP) are wired
//! as explicit "not yet" log lines until Phases 4/6 fill them in.

mod app;
mod assets;
mod chat;
mod claude;
mod config;
mod docker;
mod files;
mod homes;
mod jobs;
mod linear;
mod mcp;
mod mediaplane;
mod monitor;
mod provision;
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

    // Background loops: Claude usage poller, group-rotation loop, per-host agent-state
    // poller, and the clone-home reconciler (the Docker-port successor to the Proxmox-era
    // sshfs mount loop — it symlinks data/hosts/<id> → /proc/<clone-pid>/root/home/rmng so
    // every clone's home is browsable in one place; needs the container's `pid: "host"`).
    tokio::spawn(claude::run_poller(app.clone()));
    tokio::spawn(claude::run_rotator(app.clone()));
    tokio::spawn(monitor::run(app.clone()));
    tokio::spawn(homes::run(app.clone()));

    // Port 1 (video) — ingest clone dmabufs, VA-API encode, serve the viewer.
    mediaplane::spawn(app.clone());

    // Ports 3 (per-clone MCP, IP-routed) + 4 (global MCP).
    {
        let cfg = app.config();
        tokio::spawn(mcp::serve(app.clone(), cfg.listen.clone_mcp, mcp::Scope::PerClone));
        tokio::spawn(mcp::serve(app.clone(), cfg.listen.global_mcp, mcp::Scope::Global));
    }

    web::serve(app).await
}
