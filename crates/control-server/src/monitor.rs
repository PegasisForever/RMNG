//! Per-host agent-state poller — port of `monitor.server.ts`. Probes each host's
//! agent-wrapper `/status` every 4s and writes a derived `monitorState` onto the
//! host (riding the existing `mutate()` → SSE frame):
//!   unreachable / non-OK → offline ▸ busy → working ▸ else the agent's last
//!   reported verdict (`agentReport`), default idle. Re-derived each tick so a
//!   dropped read self-heals.

use std::collections::HashMap;
use std::time::Duration;

use serde::Deserialize;
use wire::{AgentReport, Host, MonitorState};

use crate::app::App;

const POLL_INTERVAL: Duration = Duration::from_secs(4);
const FETCH_TIMEOUT: Duration = Duration::from_millis(2500);

#[derive(Deserialize)]
struct StatusResp {
    #[serde(default)]
    busy: bool,
}

/// One probe → the host's derived state.
async fn probe_host(http: &reqwest::Client, host: &Host, agent_port: u16) -> MonitorState {
    let url = format!("http://{}:{}/status", host.host, agent_port);
    let busy = async {
        let resp = http.get(&url).timeout(FETCH_TIMEOUT).send().await.ok()?;
        if !resp.status().is_success() {
            return None;
        }
        resp.json::<StatusResp>().await.ok().map(|s| s.busy)
    }
    .await;
    match busy {
        None => MonitorState::Offline,
        Some(true) => MonitorState::Working,
        Some(false) => match host.agent_report {
            Some(AgentReport::Working) => MonitorState::Working,
            _ => MonitorState::Idle,
        },
    }
}

async fn poll_once(app: &App) {
    let hosts = app.store.get().hosts;
    if hosts.is_empty() {
        return;
    }
    let agent_port = app.config().agent_port;
    let http = &app.http;
    // Probe concurrently.
    let probes = futures::future::join_all(
        hosts.iter().map(|h| async move { (h.id.clone(), probe_host(http, h, agent_port).await) }),
    )
    .await;
    let next: HashMap<String, MonitorState> = probes.into_iter().collect();

    // Keep a persistent autonomous-message listener on every reachable host.
    for h in &hosts {
        if next.get(&h.id) != Some(&MonitorState::Offline) {
            crate::chat::ensure_autonomous_listener(app, h);
        }
    }

    // Only persist when something changed.
    let changed = app
        .store
        .get()
        .hosts
        .iter()
        .any(|h| next.get(&h.id).is_some_and(|s| Some(*s) != h.monitor_state));
    if !changed {
        return;
    }
    app.store.mutate(|s| {
        for h in &mut s.hosts {
            if let Some(state) = next.get(&h.id) {
                h.monitor_state = Some(*state);
            }
        }
    });
}

/// Background loop; spawned once at startup.
pub async fn run(app: App) {
    tracing::info!("monitor poller started (every {}s)", POLL_INTERVAL.as_secs());
    loop {
        poll_once(&app).await;
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}
