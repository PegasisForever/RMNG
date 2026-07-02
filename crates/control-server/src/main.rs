//! control-server — the four-port hub (see ../README.md).
//!
//! Phase 1 brings up **port 2** (web API + SSE + static frontend) on top of the
//! state store. Ports 1 (video), 3 (per-clone MCP), and 4 (global MCP) are wired
//! as explicit "not yet" log lines until Phases 4/6 fill them in.

mod app;
mod chat;
mod claude;
mod config;
mod docker;
mod embed;
mod files;
mod jobs;
mod linear;
mod mcp;
mod mediaplane;
mod monitor;
mod mounts;
mod orchestrate;
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

    // Background loops: Claude usage poller, group-rotation loop, per-host agent-state
    // poller, sshfs mounts.
    tokio::spawn(claude::run_poller(app.clone()));
    tokio::spawn(claude::run_rotator(app.clone()));
    tokio::spawn(monitor::run(app.clone()));
    tokio::spawn(mounts::run(app.clone()));

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
