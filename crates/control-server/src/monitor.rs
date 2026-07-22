//! Per-host agent-state poller — port of `monitor.server.ts`. Probes each host's
//! agent-wrapper `/status` every 4s and writes a derived `monitorState` onto the host.

use std::collections::HashMap;
use std::sync::RwLock as StdRwLock;
use std::time::Duration;

use serde::Deserialize;
use tokio::sync::broadcast;
use wire::{AgentReport, ContainerStats, Host, MonitorState};

use crate::app::App;

const POLL_INTERVAL: Duration = Duration::from_secs(4);
const FETCH_TIMEOUT: Duration = Duration::from_millis(2500);
const CGROUP_FETCH_TIMEOUT: Duration = Duration::from_millis(500);

/// Volatile per-host resource-usage bus. The monitor samples each running managed clone's
/// CPU/RAM every tick and publishes the whole `{ hostId: ContainerStats }` map as a named SSE
/// event. It stays out of `ControlState` / `state.json`: these numbers move every tick.
pub struct StatsBus {
    tx: broadcast::Sender<String>,
    /// The latest map plus its serialization. Equality is on the map rather than JSON bytes:
    /// fresh `HashMap`s can serialize equal content in a different key order.
    latest: StdRwLock<(HashMap<String, ContainerStats>, String)>,
}

impl StatsBus {
    pub fn new() -> Self {
        let (tx, _) = broadcast::channel(16);
        Self { tx, latest: StdRwLock::new((HashMap::new(), "{}".to_string())) }
    }

    /// The latest published map (JSON) plus a live receiver for a new `/events` subscriber.
    pub fn subscribe(&self) -> (String, broadcast::Receiver<String>) {
        (self.latest.read().unwrap().1.clone(), self.tx.subscribe())
    }

    fn latest_map(&self) -> HashMap<String, ContainerStats> {
        self.latest.read().unwrap().0.clone()
    }

    /// Broadcast only a logically changed map, so an idle fleet does not wake SSE clients.
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

/// One bounded CPU/RAM sample plus the bridge IP from one Docker runtime inspect. The CPU
/// stream and inspect run concurrently; the inspect's PID is the sole source for both cgroup
/// memory and the persisted IP, avoiding a clone-recreate race between separate inspections.
async fn sample_host(app: &App, host: &Host) -> (Option<ContainerStats>, Option<Option<String>>) {
    if !host.managed {
        return (None, None);
    }

    let (cpu, runtime) = tokio::join!(
        app.docker.container_cpu_pct(&host.id),
        app.docker.inspect_runtime(&host.id),
    );
    let runtime = match runtime {
        Ok(runtime) => runtime,
        Err(e) => {
            tracing::debug!(host = %host.id, error = %e, "clone runtime inspect unavailable");
            return (None, None);
        }
    };
    let ip = Some(runtime.ip);
    let Some(pid) = runtime.pid else {
        return (None, ip);
    };
    let memory = match tokio::time::timeout(CGROUP_FETCH_TIMEOUT, crate::cgroup::memory_usage(pid)).await {
        Ok(Ok(memory)) => memory,
        Ok(Err(e)) => {
            tracing::debug!(host = %host.id, pid, error = %e, "clone cgroup-v2 memory sample unavailable");
            return (None, ip);
        }
        Err(_) => {
            tracing::debug!(host = %host.id, pid, "clone cgroup-v2 memory sample timed out");
            return (None, ip);
        }
    };
    let Some(cpu_pct) = cpu else {
        return (None, ip);
    };

    (
        Some(ContainerStats { cpu_pct, mem_used: memory.used, mem_limit: memory.limit }),
        ip,
    )
}

/// Which CPU/RAM reading to publish for one host this tick. A fresh sample always wins. With no
/// fresh sample, a still-reachable clone keeps its prior reading across a transient sampling gap;
/// an offline clone drops it so its numbers clear.
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
        app.stats.publish(&HashMap::new());
        return;
    }

    let agent_port = app.config().agent_port;
    let probes = futures::future::join_all(hosts.iter().map(|host| async move {
        let (state, sample) = tokio::join!(
            probe_host(app, host, agent_port),
            tokio::time::timeout(FETCH_TIMEOUT, sample_host(app, host)),
        );
        let (stats, ip) = match sample {
            Ok(sample) => sample,
            Err(_) => {
                tracing::debug!(host = %host.id, "clone resource sample timed out");
                (None, None)
            }
        };
        (host.id.clone(), state, stats, ip)
    }))
    .await;
    let prev_stats = app.stats.latest_map();

    let mut next: HashMap<String, MonitorState> = HashMap::with_capacity(probes.len());
    let mut stats_map = HashMap::new();
    // Only hosts with a definitive runtime inspect this tick; absent means leave IP untouched.
    let mut ip_updates: HashMap<String, Option<String>> = HashMap::new();
    for (id, state, stats, ip) in probes {
        if let Some(stats) = pick_stat(stats, state, prev_stats.get(&id)) {
            stats_map.insert(id.clone(), stats);
        }
        if let Some(ip) = ip {
            ip_updates.insert(id.clone(), ip);
        }
        next.insert(id, state);
    }

    app.stats.publish(&stats_map);

    for host in &hosts {
        if next.get(&host.id) != Some(&MonitorState::Offline) {
            crate::chat::ensure_autonomous_listener(app, host);
        }
    }

    let changed = app.store.get().hosts.iter().any(|host| {
        next.get(&host.id).is_some_and(|state| Some(*state) != host.monitor_state)
            || ip_updates.get(&host.id).is_some_and(|ip| *ip != host.local_ip)
    });
    if !changed {
        return;
    }
    app.store.mutate(|state| {
        let selected = state.selected.clone();
        for host in &mut state.hosts {
            if let Some(&monitor_state) = next.get(&host.id) {
                if host.monitor_state == Some(MonitorState::Working)
                    && monitor_state != MonitorState::Working
                    && selected.as_deref() != Some(host.id.as_str())
                {
                    host.unread = true;
                }
                host.monitor_state = Some(monitor_state);
            }
            if let Some(ip) = ip_updates.get(&host.id) {
                host.local_ip = ip.clone();
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

#[cfg(test)]
mod tests {
    use super::*;

    fn stat(cpu: f64) -> ContainerStats {
        ContainerStats { cpu_pct: cpu, mem_used: 1 << 30, mem_limit: 8u64 << 30 }
    }

    #[test]
    fn pick_stat_prefers_a_fresh_sample() {
        let prev = stat(10.0);
        let got = pick_stat(Some(stat(55.0)), MonitorState::Working, Some(&prev));
        assert_eq!(got, Some(stat(55.0)));
    }

    #[test]
    fn pick_stat_carries_prev_forward_for_a_reachable_host() {
        let prev = stat(33.0);
        for state in [MonitorState::Working, MonitorState::Idle] {
            assert_eq!(pick_stat(None, state, Some(&prev)), Some(stat(33.0)));
        }
    }

    #[test]
    fn pick_stat_drops_an_offline_host() {
        assert_eq!(pick_stat(None, MonitorState::Offline, Some(&stat(33.0))), None);
    }

    #[test]
    fn stats_bus_new_subscriber_gets_empty_snapshot() {
        let bus = StatsBus::new();
        let (snap, _rx) = bus.subscribe();
        assert_eq!(snap, "{}");
    }

    #[test]
    fn stats_bus_serializes_only_cpu_and_memory_fields() {
        let bus = StatsBus::new();
        let (_snap, mut rx) = bus.subscribe();
        bus.publish(&HashMap::from([("h1".to_string(), stat(120.0))]));
        let frame: serde_json::Value = serde_json::from_str(&rx.try_recv().unwrap()).unwrap();
        let host = frame["h1"].as_object().unwrap();
        assert_eq!(host.len(), 3);
        assert_eq!(host["cpuPct"], serde_json::json!(120.0));
        assert_eq!(host["memUsed"], serde_json::json!(1 << 30));
        assert_eq!(host["memLimit"], serde_json::json!(8u64 << 30));
        assert!(host.get("dockerDiskUsed").is_none());
    }

    #[test]
    fn stats_bus_dedups_equal_maps_regardless_of_key_order() {
        let bus = StatsBus::new();
        let (_snap, mut rx) = bus.subscribe();
        let a: HashMap<String, ContainerStats> =
            (0..8).map(|i| (format!("h{i}"), stat(i as f64))).collect();
        let b: HashMap<String, ContainerStats> =
            (0..8).rev().map(|i| (format!("h{i}"), stat(i as f64))).collect();
        assert_eq!(a, b);
        bus.publish(&a);
        assert!(rx.try_recv().is_ok());
        bus.publish(&b);
        assert!(rx.try_recv().is_err());

        let mut changed = a.clone();
        changed.insert("h0".to_string(), stat(99.0));
        bus.publish(&changed);
        assert!(rx.try_recv().is_ok());
    }
}
