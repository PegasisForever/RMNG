//! Per-host agent-state poller — port of `monitor.server.ts`. Probes each host's
//! agent-wrapper `/status` every 4s and writes a derived `monitorState` onto the
//! host (riding the existing `mutate()` → SSE frame):
//!   unreachable / non-OK → offline ▸ busy → working ▸ else the agent's last
//!   reported verdict (`agentReport`), default idle. Re-derived each tick so a
//!   dropped read self-heals.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::RwLock as StdRwLock;
use std::time::Duration;

use serde::Deserialize;
use tokio::sync::broadcast;
use wire::{AgentReport, ContainerStats, Host, MonitorState};

use crate::app::App;

const POLL_INTERVAL: Duration = Duration::from_secs(4);
const FETCH_TIMEOUT: Duration = Duration::from_millis(2500);
const DISK_POLL_INTERVAL: Duration = Duration::from_secs(10 * 60);
const DISK_FETCH_TIMEOUT: Duration = Duration::from_secs(30);

/// Volatile per-host resource-usage bus. The monitor poller samples each running managed
/// clone's CPU/RAM every tick and publishes the whole `{ hostId: ContainerStats }` map
/// here; the `/events` handler fans it out to the frontend as a named `stats` SSE event.
///
/// Deliberately OUT of `ControlState` / `state.json`: these numbers move every tick, and
/// every `ControlState` mutation persists the file atomically (see `state.rs::mutate`), so
/// carrying stats there would rewrite `state.json` every 4 seconds. The bus is SSE-only —
/// it never touches disk. A new subscriber gets the `latest` snapshot immediately, then
/// live deltas; `publish` dedups logically-equal maps so a drained/idle fleet stops waking
/// subscribers.
pub struct StatsBus {
    tx: broadcast::Sender<String>,
    docker_disk_used: AtomicU64,
    /// The latest published map + its serialization (the snapshot new subscribers get;
    /// `"{}"` until the first tick). The dedupe compares the MAP (`HashMap: PartialEq`),
    /// never the JSON bytes: `poll_once` builds a fresh `HashMap` each tick and
    /// `RandomState` seeds iteration order per instance, so two logically-equal maps
    /// routinely serialize with different key orders.
    latest: StdRwLock<(HashMap<String, ContainerStats>, String)>,
}

impl StatsBus {
    pub fn new() -> Self {
        let (tx, _) = broadcast::channel(16);
        Self {
            tx,
            docker_disk_used: AtomicU64::new(0),
            latest: StdRwLock::new((HashMap::new(), "{}".to_string())),
        }
    }

    /// The latest published map (JSON) + a live receiver, for a new `/events` subscriber.
    pub fn subscribe(&self) -> (String, broadcast::Receiver<String>) {
        (self.latest.read().unwrap().1.clone(), self.tx.subscribe())
    }

    /// A clone of the last published stats map. `poll_once` reads it to carry a still-
    /// reachable host's previous reading forward across a transient sampling gap, so a
    /// one-off `docker stats` timeout doesn't blank that clone's CPU/RAM in the UI.
    fn latest_map(&self) -> HashMap<String, ContainerStats> {
        self.latest.read().unwrap().0.clone()
    }

    fn docker_disk_used(&self) -> u64 {
        self.docker_disk_used.load(Ordering::Relaxed)
    }

    fn set_docker_disk_used(&self, bytes: u64) {
        self.docker_disk_used.store(bytes, Ordering::Relaxed);
    }

    /// Publish a stats map, but only broadcast when it differs from the last one — an
    /// empty/unchanged map (no running managed hosts) doesn't wake anyone. Equality is
    /// on the map itself (order-independent), not its serialization.
    fn publish(&self, map: &HashMap<String, ContainerStats>) {
        let json = {
            let mut latest = self.latest.write().unwrap();
            if latest.0 == *map {
                return;
            }
            let json = serde_json::to_string(map).unwrap_or_else(|_| "{}".to_string());
            *latest = (map.clone(), json.clone());
            json
        };
        let _ = self.tx.send(json);
    }
}

impl Default for StatsBus {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Deserialize)]
struct StatusResp {
    #[serde(default)]
    busy: bool,
}

/// One probe → the host's derived state.
async fn probe_host(app: &App, host: &Host, agent_port: u16) -> MonitorState {
    let url = format!("http://{}:{}/status", app.dial_host(host).await, agent_port);
    let busy = async {
        let resp = app.http.get(&url).timeout(FETCH_TIMEOUT).send().await.ok()?;
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

/// Which CPU/RAM reading to publish for one host this tick. A fresh sample always wins.
/// With no fresh sample, a still-reachable clone keeps its `prev` reading — a transient
/// `docker stats` timeout under daemon load shouldn't blank a running clone's metrics —
/// while an offline clone (stopped/gone container) drops to `None` so its numbers clear.
/// Returns `None` too when a reachable host has no prior reading yet (nothing to show).
fn pick_stat(
    fresh: Option<ContainerStats>,
    state: MonitorState,
    prev: Option<&ContainerStats>,
) -> Option<ContainerStats> {
    match fresh {
        Some(s) => Some(s),
        None if state != MonitorState::Offline => prev.cloned(),
        None => None,
    }
}

async fn poll_once(app: &App) {
    let hosts = app.store.get().hosts;
    if hosts.is_empty() {
        // Clear any stale stats so a drained fleet stops showing numbers (deduped: a no-op
        // once already empty).
        app.stats.publish(&HashMap::new());
        return;
    }
    let agent_port = app.config().agent_port;
    // Per host, probe the agent-wrapper `/status` AND sample container stats, all
    // concurrently. Stats are sampled only for managed clones (a container named after the
    // host id backs them); a stopped/missing container yields `None`. A `None` for a still-
    // reachable clone is treated as a transient gap and its last reading is carried forward
    // (see the loop below); only an offline/unmanaged host contributes no entry. Cost: one
    // `docker stats` call per managed host per tick — cheap, one-shot, and concurrent.
    //
    // Two bounds keep stats from ever holding the monitor state hostage:
    // - the stats call gets the same FETCH_TIMEOUT as the /status probe — the shared
    //   bollard client's own timeout is 1 h (sized for base-image commits), so an
    //   unbounded call against a wedged container/daemon would freeze this `join_all`
    //   (and with it ALL monitor-state updates) for up to an hour;
    // - probe and stats run concurrently (`join!`), not sequentially, so even a healthy
    //   tick's state application never waits on the daemon's ~1 s two-cycle sampling
    //   (`one_shot=false` collects two CPU cycles before answering).
    let probes_fut = futures::future::join_all(hosts.iter().map(|h| async move {
        let stats_fut = async {
            if !h.managed {
                return None;
            }
            tokio::time::timeout(FETCH_TIMEOUT, app.docker.container_stats(&h.id))
                .await
                .ok() // timed out → no sample this tick
                .flatten()
        };
        // The clone's bridge IP, refreshed each tick alongside stats. `Some(_)` is a
        // definitive inspect result (`Some(ip)` running & attached, `None` gone/detached);
        // the outer `None` means "no reading this tick" (unmanaged, timed out, or a daemon
        // hiccup) and leaves the persisted value untouched so a transient failure can't
        // churn `state.json` by flapping the IP to empty and back.
        let ip_fut = async {
            if !h.managed {
                return None;
            }
            match tokio::time::timeout(FETCH_TIMEOUT, app.docker.inspect_ip(&h.id)).await {
                Ok(Ok(ip)) => Some(ip),
                _ => None,
            }
        };
        let (state, stats, ip) =
            tokio::join!(probe_host(app, h, agent_port), stats_fut, ip_fut);
        (h.id.clone(), state, stats, ip)
    }));
    let probes = probes_fut.await;
    let docker_disk_used = app.stats.docker_disk_used();
    // Last tick's published readings, to bridge a transient gap (see below).
    let prev_stats = app.stats.latest_map();

    let mut next: HashMap<String, MonitorState> = HashMap::with_capacity(probes.len());
    let mut stats_map: HashMap<String, ContainerStats> = HashMap::new();
    // Only hosts with a definitive IP reading this tick; absent = "leave as-is".
    let mut ip_updates: HashMap<String, Option<String>> = HashMap::new();
    for (id, state, stats, ip) in probes {
        if let Some(mut s) = pick_stat(stats, state, prev_stats.get(&id)) {
            s.docker_disk_used = docker_disk_used;
            stats_map.insert(id.clone(), s);
        }
        if let Some(ip) = ip {
            ip_updates.insert(id.clone(), ip);
        }
        next.insert(id, state);
    }

    // Volatile stats ride the SSE-only bus — never `state.json`.
    app.stats.publish(&stats_map);

    // Keep a persistent autonomous-message listener on every reachable host.
    for h in &hosts {
        if next.get(&h.id) != Some(&MonitorState::Offline) {
            crate::chat::ensure_autonomous_listener(app, h);
        }
    }

    // Only persist when the monitor state or a clone's IP actually changed.
    let changed = app.store.get().hosts.iter().any(|h| {
        next.get(&h.id).is_some_and(|s| Some(*s) != h.monitor_state)
            || ip_updates.get(&h.id).is_some_and(|ip| *ip != h.local_ip)
    });
    if !changed {
        return;
    }
    app.store.mutate(|s| {
        let sel = s.selected.clone();
        for h in &mut s.hosts {
            if let Some(&state) = next.get(&h.id) {
                // Light the unread dot when a clone drops out of `working`
                // (→ idle/offline), unless the operator is already viewing it.
                if h.monitor_state == Some(MonitorState::Working)
                    && state != MonitorState::Working
                    && sel.as_deref() != Some(h.id.as_str())
                {
                    h.unread = true;
                }
                h.monitor_state = Some(state);
            }
            if let Some(ip) = ip_updates.get(&h.id) {
                h.local_ip = ip.clone();
            }
        }
    });
}

async fn poll_docker_disk_once(app: &App) {
    match tokio::time::timeout(DISK_FETCH_TIMEOUT, app.docker.docker_disk_used()).await {
        Ok(Some(bytes)) => app.stats.set_docker_disk_used(bytes),
        Ok(None) => tracing::debug!("docker disk usage unavailable this tick"),
        Err(_) => tracing::warn!(
            "docker disk usage sample timed out after {}s",
            DISK_FETCH_TIMEOUT.as_secs()
        ),
    }
}

async fn run_docker_disk_poller(app: App) {
    tracing::info!(
        "docker disk usage poller started (every {}s, timeout {}s)",
        DISK_POLL_INTERVAL.as_secs(),
        DISK_FETCH_TIMEOUT.as_secs()
    );
    loop {
        poll_docker_disk_once(&app).await;
        tokio::time::sleep(DISK_POLL_INTERVAL).await;
    }
}

/// Background loop; spawned once at startup.
pub async fn run(app: App) {
    tracing::info!("monitor poller started (every {}s)", POLL_INTERVAL.as_secs());
    tokio::spawn(run_docker_disk_poller(app.clone()));
    loop {
        poll_once(&app).await;
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stat(cpu: f64) -> ContainerStats {
        ContainerStats {
            cpu_pct: cpu,
            mem_used: 1 << 30,
            mem_limit: 8u64 << 30,
            docker_disk_used: 0,
        }
    }

    #[test]
    fn pick_stat_prefers_a_fresh_sample() {
        // A fresh reading always wins, whatever the state or the prior value.
        let prev = stat(10.0);
        let got = pick_stat(Some(stat(55.0)), MonitorState::Working, Some(&prev));
        assert_eq!(got, Some(stat(55.0)));
    }

    #[test]
    fn pick_stat_carries_prev_forward_for_a_reachable_host() {
        // No sample this tick (transient docker-stats gap) but the clone is still
        // reachable → keep its last reading so the UI doesn't flicker to blank.
        let prev = stat(33.0);
        for state in [MonitorState::Working, MonitorState::Idle] {
            assert_eq!(pick_stat(None, state, Some(&prev)), Some(stat(33.0)));
        }
    }

    #[test]
    fn pick_stat_drops_an_offline_host() {
        // Offline (stopped/gone container) → clear its numbers even if we had a prior.
        let prev = stat(33.0);
        assert_eq!(pick_stat(None, MonitorState::Offline, Some(&prev)), None);
    }

    #[test]
    fn pick_stat_reachable_host_with_no_prior_stays_empty() {
        // A just-started clone whose first sample missed has nothing to carry forward.
        assert_eq!(pick_stat(None, MonitorState::Working, None), None);
    }

    #[test]
    fn stats_bus_new_subscriber_gets_empty_snapshot() {
        let bus = StatsBus::new();
        let (snap, _rx) = bus.subscribe();
        assert_eq!(snap, "{}");
    }

    #[test]
    fn stats_bus_tracks_cached_docker_disk_usage() {
        let bus = StatsBus::new();
        assert_eq!(bus.docker_disk_used(), 0);
        bus.set_docker_disk_used(42);
        assert_eq!(bus.docker_disk_used(), 42);
    }

    #[test]
    fn stats_bus_publish_broadcasts_and_seeds_latest() {
        let bus = StatsBus::new();
        let (_snap, mut rx) = bus.subscribe();
        let map = HashMap::from([("h1".to_string(), stat(120.0))]);
        bus.publish(&map);
        // Existing subscriber receives the frame...
        let got = rx.try_recv().expect("a frame was broadcast");
        assert!(got.contains("\"h1\""), "frame: {got}");
        assert!(got.contains("\"cpuPct\":120"), "frame: {got}");
        // ...and a fresh subscriber now snapshots the same published map.
        let (snap2, _rx2) = bus.subscribe();
        assert_eq!(snap2, got);
    }

    #[test]
    fn stats_bus_dedups_equal_maps_regardless_of_key_order() {
        // This dedup is what stops an idle fleet from waking SSE subscribers every tick.
        // Crucially it must be ORDER-INDEPENDENT: `poll_once` builds a fresh `HashMap`
        // each tick, and `RandomState` seeds iteration order per instance, so two
        // logically-equal multi-key maps routinely serialize with different key orders —
        // a byte-level JSON compare would never dedupe them. Hence: two separately
        // constructed maps (opposite insertion orders) with equal content, and the second
        // publish must not wake anyone.
        let bus = StatsBus::new();
        let (_snap, mut rx) = bus.subscribe();
        let a: HashMap<String, ContainerStats> =
            (0..8).map(|i| (format!("h{i}"), stat(i as f64))).collect();
        let b: HashMap<String, ContainerStats> =
            (0..8).rev().map(|i| (format!("h{i}"), stat(i as f64))).collect();
        assert_eq!(a, b);
        bus.publish(&a);
        assert!(rx.try_recv().is_ok(), "first publish must broadcast");
        bus.publish(&b); // equal content, freshly built → must NOT broadcast
        assert!(rx.try_recv().is_err(), "logically-equal map must not re-broadcast");

        // A changed map still gets through after the dedupe.
        let mut c = a.clone();
        c.insert("h0".to_string(), stat(99.0));
        bus.publish(&c);
        assert!(rx.try_recv().is_ok(), "a genuinely changed map must broadcast");

        // An empty map over the never-populated empty latest is also a no-op.
        let bus2 = StatsBus::new();
        let (_s, mut rx2) = bus2.subscribe();
        bus2.publish(&HashMap::new());
        assert!(rx2.try_recv().is_err(), "empty republish over empty latest must not broadcast");
    }
}
