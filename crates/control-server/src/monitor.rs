//! Docker-backed clone maintenance and server-owned lifecycle state.
//!
//! Docker determines whether a managed container is running; passive proxy token activity
//! distinguishes `working` from a Docker-running but inactive (`idle`) clone.

use std::collections::{HashMap, HashSet};
use std::sync::RwLock as StdRwLock;
use std::time::{Duration, Instant};

use tokio::sync::broadcast;
use wire::{ContainerStats, RmngClone, LxcStats, MonitorState};

use crate::app::App;

const POLL_INTERVAL: Duration = Duration::from_secs(4);
const FETCH_TIMEOUT: Duration = Duration::from_millis(2500);
const CGROUP_FETCH_TIMEOUT: Duration = Duration::from_millis(500);
/// CT 105's parent cgroup enforces `cpu.max=1600000 100000`; no other deployment is supported.
const CT105_CPU_CAPACITY: f64 = 16.0;

/// Volatile per-clone resource-usage bus. The monitor samples each running managed clone's
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

/// Volatile resource usage for the whole CT 105 LXC. It is intentionally a separate event from
/// the clone-keyed stats map: the control-server and Docker infrastructure have no clone id.
pub struct LxcStatsBus {
    tx: broadcast::Sender<String>,
    latest: StdRwLock<(Option<LxcStats>, String)>,
}

impl LxcStatsBus {
    pub fn new() -> Self {
        let (tx, _) = broadcast::channel(16);
        Self { tx, latest: StdRwLock::new((None, "null".to_string())) }
    }

    /// The latest CT sample (JSON) plus a live receiver for a new `/events` subscriber.
    pub fn subscribe(&self) -> (String, broadcast::Receiver<String>) {
        (self.latest.read().unwrap().1.clone(), self.tx.subscribe())
    }

    /// Broadcast only a changed CT sample; `None` explicitly clears unavailable readings.
    fn publish(&self, stats: &Option<LxcStats>) {
        let json = {
            let mut latest = self.latest.write().unwrap();
            if latest.0 == *stats {
                return;
            }
            let json = serde_json::to_string(stats).unwrap_or_else(|_| "null".to_string());
            *latest = (stats.clone(), json.clone());
            json
        };
        let _ = self.tx.send(json);
    }
}

impl Default for LxcStatsBus {
    fn default() -> Self {
        Self::new()
    }
}

/// Volatile per-clone "operator last looked at this clone" timestamps (wall-clock ms). Stamped
/// when a clone gains or loses selection (see [`crate::web::activate`]) and read by the monitor
/// to decide whether a `working → idle` slide is still news. Deliberately never persisted: on
/// restart nothing is unread-seeded anyway (the browser baselines silently), so a cold map is the
/// correct starting point.
#[derive(Default)]
pub struct ViewTracker {
    seen: StdRwLock<HashMap<String, i64>>,
}

impl ViewTracker {
    pub fn new() -> Self {
        Self::default()
    }

    /// Record that the operator looked at `host_id` at `now_ms`. Monotonic: an out-of-order
    /// stamp never moves the last-viewed time backwards.
    pub fn mark(&self, host_id: &str, now_ms: i64) {
        let mut seen = self.seen.write().unwrap();
        let entry = seen.entry(host_id.to_string()).or_insert(now_ms);
        *entry = (*entry).max(now_ms);
    }

    pub fn last_viewed(&self, host_id: &str) -> Option<i64> {
        self.seen.read().unwrap().get(host_id).copied()
    }

    /// Drop timestamps for clones no longer in the active managed fleet, so the map cannot grow
    /// unbounded across the life of a long-running server.
    pub fn retain(&self, ids: &HashSet<String>) {
        self.seen.write().unwrap().retain(|id, _| ids.contains(id));
    }
}

#[derive(Clone, Copy)]
struct LxcCpuSample {
    usage_usec: u64,
    sampled_at: Instant,
}

fn lxc_cpu_pct(previous: &mut Option<LxcCpuSample>, usage_usec: u64, now: Instant) -> Option<f64> {
    let previous = previous.replace(LxcCpuSample { usage_usec, sampled_at: now })?;
    let usage_delta = usage_usec.checked_sub(previous.usage_usec)? as f64;
    let elapsed_usec = now.checked_duration_since(previous.sampled_at)?.as_secs_f64() * 1_000_000.0;
    (elapsed_usec > 0.0).then_some((usage_delta / elapsed_usec) * 100.0 / CT105_CPU_CAPACITY)
}

/// One CT 105-wide CPU/RAM/disk sample. Every cgroup input is read through PID 1's root so the
/// result includes the Docker daemon and other LXC processes, not merely managed clones.
async fn sample_lxc(previous_cpu: &mut Option<LxcCpuSample>) -> Option<LxcStats> {
    let (cpu, memory, disk) = tokio::join!(
        tokio::time::timeout(CGROUP_FETCH_TIMEOUT, crate::cgroup::lxc_cpu_usage_usec()),
        tokio::time::timeout(CGROUP_FETCH_TIMEOUT, crate::cgroup::lxc_memory_usage()),
        async { crate::cgroup::lxc_disk_used() },
    );

    let cpu = match cpu {
        Ok(Ok(cpu)) => cpu,
        Ok(Err(e)) => {
            tracing::debug!(error = %e, "CT 105 cgroup-v2 CPU sample unavailable");
            return None;
        }
        Err(_) => {
            tracing::debug!("CT 105 cgroup-v2 CPU sample timed out");
            return None;
        }
    };
    let memory = match memory {
        Ok(Ok(memory)) => memory,
        Ok(Err(e)) => {
            tracing::debug!(error = %e, "CT 105 cgroup-v2 memory sample unavailable");
            return None;
        }
        Err(_) => {
            tracing::debug!("CT 105 cgroup-v2 memory sample timed out");
            return None;
        }
    };
    let disk_used = match disk {
        Ok(disk_used) => Some(disk_used),
        Err(e) => {
            tracing::debug!(error = %e, "CT 105 rootfs disk sample unavailable");
            None
        }
    };

    Some(LxcStats {
        cpu_pct: lxc_cpu_pct(previous_cpu, cpu, Instant::now()),
        mem_used: memory.used,
        mem_limit: memory.limit,
        disk_used,
    })
}

/// One bounded CPU/RAM sample plus the bridge IP from one Docker runtime inspect. The CPU
/// stream and inspect run concurrently; the inspect's PID is the sole source for both cgroup
/// memory and the persisted IP, avoiding a clone-recreate race between separate inspections.
async fn sample_clone(app: &App, host: &RmngClone) -> (Option<ContainerStats>, Option<Option<String>>) {
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

/// Which CPU/RAM reading to publish for one clone this tick. A fresh sample always wins. With no
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

/// Whether a `working → not-working` transition should raise the unread badge + browser
/// notification for a clone. Suppressed when the clone is currently selected (the operator is
/// already looking at it), or — for an **idle** slide specifically — when the operator has
/// viewed the clone at or after its last token activity: they have already seen its final output,
/// so its slide into idle is not news and it simply shows the gray "not working" dot. An
/// **offline** transition (the container died) is always surfaced, even if recently viewed.
fn should_flag_unread(
    next: MonitorState,
    is_selected: bool,
    last_viewed_at: Option<i64>,
    last_token_at: Option<i64>,
) -> bool {
    if is_selected {
        return false;
    }
    if next == MonitorState::Idle {
        if let (Some(viewed), Some(active)) = (last_viewed_at, last_token_at) {
            if viewed >= active {
                return false;
            }
        }
    }
    true
}

async fn poll_once(app: &App, previous_lxc_cpu: &mut Option<LxcCpuSample>) {
    let hosts: Vec<RmngClone> = app
        .store
        .get()
        .hosts
        .into_iter()
        .filter(|host| host.managed && !host.archived)
        .collect();
    if hosts.is_empty() {
        let lxc_stats = sample_lxc(previous_lxc_cpu).await;
        app.stats.publish(&HashMap::new());
        app.lxc_stats.publish(&lxc_stats);
        return;
    }

    let probes = futures::future::join_all(hosts.iter().map(|host| async move {
        // An unavailable Docker daemon leaves the lifecycle unchanged; it is not proof that the
        // container stopped. Only a successful liveness response may write `offline`.
        let running = match tokio::time::timeout(FETCH_TIMEOUT, app.docker.is_running(&host.id)).await {
            Ok(Ok(running)) => Some(running),
            Ok(Err(error)) => {
                tracing::warn!(host = %host.id, "Docker liveness check failed: {error}");
                None
            }
            Err(_) => {
                tracing::warn!(host = %host.id, "Docker liveness check timed out");
                None
            }
        };
        let (stats, ip) = if running == Some(true) {
            match tokio::time::timeout(FETCH_TIMEOUT, sample_clone(app, host)).await {
                Ok(sample) => sample,
                Err(_) => {
                    tracing::debug!(host = %host.id, "clone resource sample timed out");
                    (None, None)
                }
            }
        } else if running == Some(false) {
            (None, Some(None))
        } else {
            (None, None)
        };
        (host.id.clone(), running, stats, ip)
    }));
    let (lxc_stats, probes) = tokio::join!(sample_lxc(previous_lxc_cpu), probes);
    let prev_stats = app.stats.latest_map();

    let now = crate::clone_ops::now_ms();
    let mut next: HashMap<String, MonitorState> = HashMap::with_capacity(probes.len());
    let mut stats_map = HashMap::new();
    let mut ip_updates: HashMap<String, Option<String>> = HashMap::new();
    for (id, running, stats, ip) in probes {
        let Some(running) = running else {
            continue;
        };
        let state = if !running {
            MonitorState::Offline
        } else if app.tokens.is_token_inactive(&id, now) {
            MonitorState::Idle
        } else {
            MonitorState::Working
        };
        if let Some(stats) = pick_stat(stats, state, prev_stats.get(&id)) {
            stats_map.insert(id.clone(), stats);
        }
        if let Some(ip) = ip {
            ip_updates.insert(id.clone(), ip);
        }
        next.insert(id, state);
    }

    // An archive operation may complete while Docker and cgroup calls are in flight. Filter a
    // second time so its intentional stop cannot race into lifecycle, stats, or chat updates.
    let active_clones: Vec<RmngClone> = app
        .store
        .get()
        .hosts
        .into_iter()
        .filter(|host| host.managed && !host.archived)
        .collect();
    let active_ids: HashSet<String> = active_clones.iter().map(|host| host.id.clone()).collect();
    next.retain(|id, _| active_ids.contains(id));
    stats_map.retain(|id, _| active_ids.contains(id));
    ip_updates.retain(|id, _| active_ids.contains(id));
    app.views.retain(&active_ids);

    // Snapshot the two inputs to the unread decision (last-viewed + last-token-activity) before
    // entering `store.mutate`, so we neither hold the view/token locks across the state mutation
    // nor re-lock them per host inside the closure.
    let unread_ctx: HashMap<String, (Option<i64>, Option<i64>)> = active_clones
        .iter()
        .map(|host| {
            (
                host.id.clone(),
                (app.views.last_viewed(&host.id), app.tokens.last_token_at(&host.id)),
            )
        })
        .collect();

    app.stats.publish(&stats_map);
    app.lxc_stats.publish(&lxc_stats);

    for host in &active_clones {
        if next
            .get(&host.id)
            .is_some_and(|state| *state != MonitorState::Offline)
        {
            crate::chat::ensure_autonomous_listener(app, host);
        }
    }

    let changed = app.store.get().hosts.iter().any(|host| {
        !host.archived
            && host.managed
            && (next.get(&host.id).is_some_and(|state| Some(*state) != host.monitor_state)
                || ip_updates.get(&host.id).is_some_and(|ip| *ip != host.local_ip))
    });
    if !changed {
        return;
    }
    app.store.mutate(|state| {
        let selected = state.selected.clone();
        for host in &mut state.hosts {
            if host.archived || !host.managed {
                continue;
            }
            if let Some(&monitor_state) = next.get(&host.id) {
                if host.monitor_state == Some(MonitorState::Working)
                    && monitor_state != MonitorState::Working
                {
                    let (last_viewed, last_token) =
                        unread_ctx.get(&host.id).copied().unwrap_or((None, None));
                    let is_selected = selected.as_deref() == Some(host.id.as_str());
                    if should_flag_unread(monitor_state, is_selected, last_viewed, last_token) {
                        host.unread = true;
                    }
                } else if monitor_state == MonitorState::Working {
                    host.unread = false;
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
    let mut previous_lxc_cpu = None;
    loop {
        poll_once(&app, &mut previous_lxc_cpu).await;
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stat(cpu: f64) -> ContainerStats {
        ContainerStats { cpu_pct: cpu, mem_used: 1 << 30, mem_limit: 8u64 << 30 }
    }

    fn lxc_stat(cpu: Option<f64>) -> LxcStats {
        LxcStats {
            cpu_pct: cpu,
            mem_used: 16u64 << 30,
            mem_limit: 264u64 << 30,
            disk_used: Some(320u64 << 30),
        }
    }

    #[test]
    fn lxc_cpu_uses_elapsed_time_and_ct105_capacity() {
        let start = Instant::now();
        let mut previous = None;
        assert_eq!(lxc_cpu_pct(&mut previous, 1_000, start), None);

        let pct = lxc_cpu_pct(&mut previous, 32_001_000, start + Duration::from_secs(4)).unwrap();
        assert!((pct - 50.0).abs() < f64::EPSILON);

        assert_eq!(lxc_cpu_pct(&mut previous, 10, start + Duration::from_secs(8)), None);
    }

    #[test]
    fn selected_clone_never_flags_unread() {
        // Whatever the timestamps, a clone the operator is currently looking at is not flagged.
        assert!(!should_flag_unread(MonitorState::Idle, true, None, Some(10)));
        assert!(!should_flag_unread(MonitorState::Offline, true, Some(1), Some(10)));
    }

    #[test]
    fn idle_is_suppressed_only_when_viewed_since_last_activity() {
        // Viewed at/after last token activity → operator has seen the output → gray dot, no nag.
        assert!(!should_flag_unread(MonitorState::Idle, false, Some(10), Some(10)));
        assert!(!should_flag_unread(MonitorState::Idle, false, Some(11), Some(10)));
        // Last looked before the clone's final activity → they haven't seen it → flag.
        assert!(should_flag_unread(MonitorState::Idle, false, Some(9), Some(10)));
        // Never viewed, or no recorded activity to compare against → flag (current behavior).
        assert!(should_flag_unread(MonitorState::Idle, false, None, Some(10)));
        assert!(should_flag_unread(MonitorState::Idle, false, Some(10), None));
    }

    #[test]
    fn offline_transition_is_always_flagged_even_if_recently_viewed() {
        // A container that died is surfaced regardless of when it was last viewed.
        assert!(should_flag_unread(MonitorState::Offline, false, Some(99), Some(10)));
    }

    #[test]
    fn view_tracker_is_monotonic_and_prunes() {
        let views = ViewTracker::new();
        assert_eq!(views.last_viewed("a"), None);
        views.mark("a", 100);
        views.mark("a", 50); // out-of-order stamp must not move it backwards
        assert_eq!(views.last_viewed("a"), Some(100));
        views.mark("a", 150);
        assert_eq!(views.last_viewed("a"), Some(150));

        views.mark("b", 7);
        views.retain(&HashSet::from(["a".to_string()]));
        assert_eq!(views.last_viewed("a"), Some(150));
        assert_eq!(views.last_viewed("b"), None);
    }

    #[test]
    fn pick_stat_prefers_a_fresh_sample() {
        let prev = stat(10.0);
        let got = pick_stat(Some(stat(55.0)), MonitorState::Working, Some(&prev));
        assert_eq!(got, Some(stat(55.0)));
    }

    #[test]
    fn pick_stat_carries_prev_forward_for_a_reachable_clone() {
        let prev = stat(33.0);
        for state in [MonitorState::Working, MonitorState::Idle] {
            assert_eq!(pick_stat(None, state, Some(&prev)), Some(stat(33.0)));
        }
    }

    #[test]
    fn pick_stat_drops_an_offline_clone() {
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

    #[test]
    fn lxc_stats_bus_snapshots_dedups_and_clears() {
        let bus = LxcStatsBus::new();
        let (snap, mut rx) = bus.subscribe();
        assert_eq!(snap, "null");

        let sample = Some(lxc_stat(Some(50.0)));
        bus.publish(&sample);
        let frame: serde_json::Value = serde_json::from_str(&rx.try_recv().unwrap()).unwrap();
        assert_eq!(frame["cpuPct"], serde_json::json!(50.0));
        assert_eq!(frame["memUsed"], serde_json::json!(16u64 << 30));
        assert_eq!(frame["memLimit"], serde_json::json!(264u64 << 30));
        assert_eq!(frame["diskUsed"], serde_json::json!(320u64 << 30));

        bus.publish(&sample);
        assert!(rx.try_recv().is_err());
        bus.publish(&None);
        assert_eq!(rx.try_recv().unwrap(), "null");
    }
}
