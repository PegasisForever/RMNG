//! Docker-backed clone maintenance and server-owned lifecycle state.
//!
//! Docker determines whether a managed container is running; [`crate::stuck`] decides whether
//! a running one is `working` or `idle`, by reading Claude Code's own session registry and
//! agent hooks and, for the cases those cannot settle, asking a cheap model one question.

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


/// Volatile per-clone "agent was last busy" timestamps (wall-clock ms) — the signal behind
/// `working` vs `idle`.
///
/// **Why this exists.** Activity used to be inferred from tokens passing through the `/cc`
/// proxy, which no longer sits in the data path (agents now talk to Anthropic directly, so the
/// server never sees their traffic). The original pre-proxy signal — polling the agent-wrapper's
/// `GET /status` — no longer exists either; that endpoint was removed when token accounting
/// landed.
///
/// So we take it from the one channel that survived both: the agent-wrapper's `/events` SSE
/// stream emits `{busy: true}` when a turn starts and `{busy: false}` when it ends, and
/// [`crate::chat::run_autonomous_listener`] is already subscribed to it for every running managed
/// clone. Stamping those frames here costs no new connection, no clone-side change, and — unlike
/// reading `ChatState.busy` — covers autonomous background work, not just operator-solicited
/// turns.
///
/// Deliberately never persisted: a cold map after a restart reads as `idle` until the clone next
/// works, which is the correct default (the browser re-baselines anyway).
#[derive(Default)]
pub struct ActivityBus {
    last_active: StdRwLock<HashMap<String, i64>>,
}

impl ActivityBus {
    pub fn new() -> Self {
        Self::default()
    }

    /// Record agent activity for `clone_id` at `now_ms`. Monotonic, like [`ViewTracker::mark`]:
    /// an out-of-order frame never moves the timestamp backwards.
    pub fn mark(&self, clone_id: &str, now_ms: i64) {
        let mut map = self.last_active.write().unwrap();
        let entry = map.entry(clone_id.to_string()).or_insert(now_ms);
        *entry = (*entry).max(now_ms);
    }

    /// Wall-clock ms of this clone's last observed activity, if any.
    pub fn last_active_at(&self, clone_id: &str) -> Option<i64> {
        self.last_active.read().unwrap().get(clone_id).copied()
    }

    /// Drop entries for clones no longer in the active managed fleet, so the map cannot grow
    /// unbounded across the life of a long-running server.
    pub fn retain(&self, ids: &HashSet<String>) {
        self.last_active.write().unwrap().retain(|id, _| ids.contains(id));
    }
}

/// One clone the daemon answered for this tick: id, running, its stats sample, and its IP
/// (the outer `Option` is "did we look", the inner one is "does it have one").
type SettledProbe = (String, bool, Option<ContainerStats>, Option<Option<String>>);

#[derive(Clone, Copy)]
struct CpuSample {
    usage_usec: u64,
    sampled_at: Instant,
}

/// Turn a cumulative cgroup `usage_usec` counter into a percentage of CT 105's enforced capacity,
/// against the caller's previous sample (which this replaces).
///
/// Shared by the CT-wide gauge and every per-clone row so both sit on one basis: a clone reading
/// 50% and the CT reading 50% mean the same eight busy cores. That is the whole reason per-clone
/// CPU does not come from Docker's stats API — see [`crate::cgroup`] for why its denominator is
/// wrong here.
///
/// `None` until two samples establish a rate, and again whenever the counter moves backwards —
/// a container restart resets it to zero, and a stale delta would read as a spike.
fn cpu_pct(previous: &mut Option<CpuSample>, usage_usec: u64, now: Instant) -> Option<f64> {
    let previous = previous.replace(CpuSample { usage_usec, sampled_at: now })?;
    let usage_delta = usage_usec.checked_sub(previous.usage_usec)? as f64;
    let elapsed_usec = now.checked_duration_since(previous.sampled_at)?.as_secs_f64() * 1_000_000.0;
    (elapsed_usec > 0.0).then_some((usage_delta / elapsed_usec) * 100.0 / CT105_CPU_CAPACITY)
}

/// One CT 105-wide CPU/RAM/disk sample. Every cgroup input is read through PID 1's root so the
/// result includes the Docker daemon and other LXC processes, not merely managed clones.
async fn sample_lxc(previous_cpu: &mut Option<CpuSample>) -> Option<LxcStats> {
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
        cpu_pct: cpu_pct(previous_cpu, cpu, Instant::now()),
        mem_used: memory.used,
        mem_limit: memory.limit,
        disk_used,
    })
}

/// One bounded CPU/RAM sample plus the bridge IP from one Docker runtime inspect. CPU and memory
/// both come from the clone's own cgroup through the inspect's PID, which is also the sole source
/// for the persisted IP — avoiding a clone-recreate race between separate inspections.
///
/// `previous_cpu` carries this clone's prior CPU counter across ticks, so the first sample after a
/// clone appears yields no CPU reading (the CT-wide gauge behaves the same way).
async fn sample_clone(
    app: &App, host: &RmngClone, previous_cpu: &mut Option<CpuSample>,
) -> (Option<ContainerStats>, Option<Option<String>>) {
    if !host.managed {
        return (None, None);
    }

    let runtime = match app.docker.inspect_runtime(&host.id).await {
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
    let (memory, cpu) = tokio::join!(
        tokio::time::timeout(CGROUP_FETCH_TIMEOUT, crate::cgroup::memory_usage(pid)),
        tokio::time::timeout(CGROUP_FETCH_TIMEOUT, crate::cgroup::cpu_usage_usec(pid)),
    );
    let memory = match memory {
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
    let cpu = match cpu {
        Ok(Ok(cpu)) => cpu,
        Ok(Err(e)) => {
            tracing::debug!(host = %host.id, pid, error = %e, "clone cgroup-v2 CPU sample unavailable");
            return (None, ip);
        }
        Err(_) => {
            tracing::debug!(host = %host.id, pid, "clone cgroup-v2 CPU sample timed out");
            return (None, ip);
        }
    };
    let Some(cpu_pct) = cpu_pct(previous_cpu, cpu, Instant::now()) else {
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

/// Turn a parent clone's `idle` back into `working` while any of its sub clones is working.
///
/// Activity is observed per clone, from that clone's own proxy traffic and its own agent logs. A
/// parent that handed work to a sub clone and is waiting on the result produces none of either,
/// so on its own signals it reads idle while the work it started is plainly still running. That
/// is wrong twice over: the sidebar greys out a row whose group is busy, and the idle transition
/// fires an unread badge for output nobody has finished producing. A parent now goes idle only
/// once everything under it has.
///
/// Three rules the callers depend on:
///
/// * `offline` is never lifted. That state is about the container, and a stopped parent is
///   stopped whatever its sub clones are doing.
/// * A sub clone this tick could not reach is absent from `next`, so its stored `monitor_state`
///   decides. That is the state the operator is still being shown, and a failed liveness probe is
///   not evidence the sub clone stopped working.
/// * Parentage is one level deep by construction ([`RmngClone::parent`]), so one pass carries
///   everything. There is no grandchild whose state would need a second round.
fn lift_sub_clone_activity(next: &mut HashMap<String, MonitorState>, clones: &[RmngClone]) {
    let busy_parents: HashSet<&str> = clones
        .iter()
        .filter(|clone| {
            next.get(&clone.id).copied().or(clone.monitor_state) == Some(MonitorState::Working)
        })
        .filter_map(|clone| clone.parent.as_deref())
        .collect();
    if busy_parents.is_empty() {
        return;
    }
    for (id, state) in next.iter_mut() {
        if *state == MonitorState::Idle && busy_parents.contains(id.as_str()) {
            *state = MonitorState::Working;
        }
    }
}

/// How long a new activity state has to stand before it is published.
const DEBOUNCE: Duration = Duration::from_secs(60);

/// Hold every `working`/`idle` change back until it has stood for [`DEBOUNCE`].
///
/// This sits outside every rule that decides a state: per-session verdicts, the model, the
/// first-minute floor, the blind-home hold, and [`lift_sub_clone_activity`] have all run by the
/// time it sees a clone. It knows nothing about why a state changed, only that it did, so a
/// change that reverses inside the window is never shown at all. Over a recent 15.5 hours across
/// CT 105 and CT 106, 36% of changes reversed inside 30 seconds.
///
/// Both directions are held, which is what makes it a debounce rather than a one-sided delay,
/// and the cost is real: a clone that starts working lights up a minute late. Holding only the
/// slide into idle would keep that responsiveness, and is a one-line change here.
///
/// `offline` is never held, in or out. That state is about whether the container exists, not
/// about what an agent is doing, and an operator watching a clone die should not wait a minute
/// to see it. Neither is a clone's first reading, which has nothing to be a change from.
fn debounce(
    next: &mut HashMap<String, MonitorState>,
    clones: &[RmngClone],
    pending: &mut HashMap<String, (MonitorState, Instant)>,
    now: Instant,
) {
    let shown: HashMap<&str, Option<MonitorState>> =
        clones.iter().map(|c| (c.id.as_str(), c.monitor_state)).collect();
    for (id, proposed) in next.iter_mut() {
        // Nothing to hold against: a clone we do not show, one with no state yet, a move in or
        // out of `offline`, or a tick that proposed what is already up.
        let settled = match shown.get(id.as_str()).copied().flatten() {
            None => true,
            Some(shown) => {
                shown == *proposed
                    || shown == MonitorState::Offline
                    || *proposed == MonitorState::Offline
            }
        };
        if settled {
            pending.remove(id);
            continue;
        }
        let shown = shown[id.as_str()].expect("settled covers the None case");
        match pending.get(id) {
            // It has stood long enough. Let it through and stop tracking it.
            Some((held, since)) if held == proposed && now.duration_since(*since) >= DEBOUNCE => {
                pending.remove(id);
            }
            // Same change, still too new: keep showing what is up.
            Some((held, _)) if held == proposed => *proposed = shown,
            // A new change, or one that replaced a different pending change. Start its clock.
            _ => {
                pending.insert(id.clone(), (*proposed, now));
                *proposed = shown;
            }
        }
    }
    pending.retain(|id, _| next.contains_key(id));
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

async fn poll_once(
    app: &App, previous_lxc_cpu: &mut Option<CpuSample>,
    previous_clone_cpu: &mut HashMap<String, CpuSample>,
    pending_state: &mut HashMap<String, (MonitorState, Instant)>,
) {
    let hosts: Vec<RmngClone> = app
        .store
        .get()
        .hosts
        .into_iter()
        .filter(|host| host.managed && !host.archived)
        .collect();
    if hosts.is_empty() {
        let lxc_stats = sample_lxc(previous_lxc_cpu).await;
        previous_clone_cpu.clear();
        app.stats.publish(&HashMap::new());
        app.lxc_stats.publish(&lxc_stats);
        return;
    }

    // Each probe owns its clone's prior CPU counter for the duration of the tick and hands the
    // updated one back, so the concurrent futures need no shared lock over the map.
    let probes = futures::future::join_all(hosts.iter().map(|host| {
        let mut cpu_sample = previous_clone_cpu.get(&host.id).copied();
        async move {
            // An unavailable Docker daemon leaves the lifecycle unchanged; it is not proof that
            // the container stopped. Only a successful liveness response may write `offline`.
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
                match tokio::time::timeout(FETCH_TIMEOUT, sample_clone(app, host, &mut cpu_sample)).await {
                    Ok(sample) => sample,
                    Err(_) => {
                        tracing::debug!(host = %host.id, "clone resource sample timed out");
                        (None, None)
                    }
                }
            } else if running == Some(false) {
                // A stopped container's counter is gone; drop the sample so a later restart rates
                // from its fresh zero rather than against a pre-stop total.
                cpu_sample = None;
                (None, Some(None))
            } else {
                (None, None)
            };
            (host.id.clone(), running, stats, ip, cpu_sample)
        }
    }));
    let (lxc_stats, probes) = tokio::join!(sample_lxc(previous_lxc_cpu), probes);
    let prev_stats = app.stats.latest_map();

    let mut next: HashMap<String, MonitorState> = HashMap::with_capacity(probes.len());
    let mut stats_map = HashMap::new();
    let mut ip_updates: HashMap<String, Option<String>> = HashMap::new();
    // Clones whose liveness Docker actually answered for. An unreachable daemon leaves a
    // clone out entirely, which is what keeps its stored state untouched.
    let mut settled: Vec<SettledProbe> = Vec::with_capacity(probes.len());
    for (id, running, stats, ip, cpu_sample) in probes {
        match cpu_sample {
            Some(sample) => {
                previous_clone_cpu.insert(id.clone(), sample);
            }
            None => {
                previous_clone_cpu.remove(&id);
            }
        }
        let Some(running) = running else {
            continue;
        };
        settled.push((id, running, stats, ip));
    }

    // Deciding working-vs-stuck reads each clone's files and may ask a model, so it happens
    // once for the whole fleet, concurrently, rather than inline per clone.
    let states = crate::stuck::resolve_fleet(
        app,
        settled.iter().filter(|(_, up, _, _)| *up).map(|(id, ..)| id.clone()).collect(),
    )
    .await;

    for (id, running, stats, ip) in settled {
        let state = if running {
            states.get(&id).copied().unwrap_or(MonitorState::Idle)
        } else {
            MonitorState::Offline
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
    lift_sub_clone_activity(&mut next, &active_clones);
    // Last of all, so a change that reverses inside a minute is never shown. See [`debounce`].
    debounce(&mut next, &active_clones, pending_state, Instant::now());
    stats_map.retain(|id, _| active_ids.contains(id));
    ip_updates.retain(|id, _| active_ids.contains(id));
    // Bound the CPU-sample map to the live fleet, so archived and deleted clones cannot
    // accumulate in it across the life of a long-running server.
    previous_clone_cpu.retain(|id, _| active_ids.contains(id));
    app.views.retain(&active_ids);
    app.activity.retain(&active_ids);

    // Snapshot the two inputs to the unread decision (last-viewed + last-token-activity) before
    // entering `store.mutate`, so we neither hold the view/token locks across the state mutation
    // nor re-lock them per host inside the closure.
    let unread_ctx: HashMap<String, (Option<i64>, Option<i64>)> = active_clones
        .iter()
        .map(|host| {
            (
                host.id.clone(),
                (app.views.last_viewed(&host.id), app.activity.last_active_at(&host.id)),
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
    let mut previous_clone_cpu = HashMap::new();
    let mut pending_state = HashMap::new();
    loop {
        poll_once(&app, &mut previous_lxc_cpu, &mut previous_clone_cpu, &mut pending_state).await;
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
        assert_eq!(cpu_pct(&mut previous, 1_000, start), None);

        let pct = cpu_pct(&mut previous, 32_001_000, start + Duration::from_secs(4)).unwrap();
        assert!((pct - 50.0).abs() < f64::EPSILON);

        assert_eq!(cpu_pct(&mut previous, 10, start + Duration::from_secs(8)), None);
    }

    #[test]
    fn clone_cpu_is_rated_against_the_same_capacity_as_the_ct_gauge() {
        // The bug this guards: Docker's stats divide by `system_cpu_usage`, which counts all 32
        // threads CT 105 can see, while its cgroup enforces only 16 cores — halving every
        // per-clone reading. One clone burning 8 of the 16 cores is 50%, on the same basis the
        // CT-wide gauge uses, so the rows and the total are directly comparable.
        let start = Instant::now();
        let mut previous = None;
        assert_eq!(cpu_pct(&mut previous, 0, start), None, "one sample cannot make a rate");

        // 8 cores busy for 4s == 32s of CPU time.
        let pct = cpu_pct(&mut previous, 32_000_000, start + Duration::from_secs(4)).unwrap();
        assert!((pct - 50.0).abs() < f64::EPSILON, "expected 50%, got {pct}");

        // A fully idle clone reads zero rather than carrying the previous rate forward.
        let idle = cpu_pct(&mut previous, 32_000_000, start + Duration::from_secs(8)).unwrap();
        assert_eq!(idle, 0.0);
    }

    #[test]
    fn a_restarted_clones_counter_reset_is_not_a_spike() {
        // A container restart zeroes `usage_usec`. Rating the new total against the pre-restart
        // one would underflow; this must yield no reading until two fresh samples land.
        let start = Instant::now();
        let mut previous = None;
        assert_eq!(cpu_pct(&mut previous, 900_000_000, start), None);
        assert_eq!(
            cpu_pct(&mut previous, 1_000, start + Duration::from_secs(4)),
            None,
            "a backwards counter must not produce a reading"
        );
        // The reset sample is retained, so the next tick rates normally from it.
        let pct = cpu_pct(&mut previous, 6_401_000, start + Duration::from_secs(8)).unwrap();
        assert!((pct - 10.0).abs() < 1e-9, "expected 10%, got {pct}");
    }

    /// A clone row as the lifter sees it: id, parent, and the state already stored on it.
    fn clone_row(id: &str, parent: Option<&str>, stored: Option<MonitorState>) -> RmngClone {
        RmngClone {
            id: id.to_string(),
            managed: true,
            parent: parent.map(str::to_string),
            monitor_state: stored,
            ..Default::default()
        }
    }

    /// One debounce pass: what `next` says after a tick at `now`.
    fn settle(
        proposed: MonitorState,
        shown: Option<MonitorState>,
        pending: &mut HashMap<String, (MonitorState, Instant)>,
        now: Instant,
    ) -> MonitorState {
        let mut next = HashMap::from([("c1".to_string(), proposed)]);
        debounce(&mut next, &[clone_row("c1", None, shown)], pending, now);
        next["c1"]
    }

    #[test]
    fn a_change_is_held_until_it_has_stood_for_a_minute() {
        let t0 = Instant::now();
        let mut pending = HashMap::new();
        // Proposed idle against a shown working: keep showing working while the clock runs.
        for at in [t0, t0 + Duration::from_secs(30), t0 + DEBOUNCE - Duration::from_millis(1)] {
            assert_eq!(settle(MonitorState::Idle, Some(MonitorState::Working), &mut pending, at),
                       MonitorState::Working);
        }
        assert_eq!(settle(MonitorState::Idle, Some(MonitorState::Working), &mut pending, t0 + DEBOUNCE),
                   MonitorState::Idle);
        assert!(pending.is_empty(), "once through, it stops being tracked");
    }

    #[test]
    fn a_change_that_reverses_inside_the_window_is_never_shown() {
        // The whole point. 36% of changes reversed inside 30 seconds on the live fleet.
        let t0 = Instant::now();
        let mut pending = HashMap::new();
        assert_eq!(settle(MonitorState::Idle, Some(MonitorState::Working), &mut pending, t0),
                   MonitorState::Working);
        // It comes back before the minute is up, so the pending change is dropped...
        assert_eq!(settle(MonitorState::Working, Some(MonitorState::Working), &mut pending,
                          t0 + Duration::from_secs(20)), MonitorState::Working);
        assert!(pending.is_empty());
        // ...and a later idle starts its own fresh minute rather than inheriting the old clock.
        assert_eq!(settle(MonitorState::Idle, Some(MonitorState::Working), &mut pending,
                          t0 + Duration::from_secs(30)), MonitorState::Working);
        assert_eq!(settle(MonitorState::Idle, Some(MonitorState::Working), &mut pending,
                          t0 + Duration::from_secs(89)), MonitorState::Working);
        assert_eq!(settle(MonitorState::Idle, Some(MonitorState::Working), &mut pending,
                          t0 + Duration::from_secs(90)), MonitorState::Idle);
    }

    #[test]
    fn both_directions_are_held() {
        let t0 = Instant::now();
        let mut pending = HashMap::new();
        assert_eq!(settle(MonitorState::Working, Some(MonitorState::Idle), &mut pending, t0),
                   MonitorState::Idle);
        assert_eq!(settle(MonitorState::Working, Some(MonitorState::Idle), &mut pending, t0 + DEBOUNCE),
                   MonitorState::Working);
    }

    #[test]
    fn offline_is_never_held_in_either_direction() {
        // Container existence is not activity, and an operator watching a clone die should not
        // wait a minute to see it.
        let t0 = Instant::now();
        let mut pending = HashMap::new();
        assert_eq!(settle(MonitorState::Offline, Some(MonitorState::Working), &mut pending, t0),
                   MonitorState::Offline);
        assert_eq!(settle(MonitorState::Working, Some(MonitorState::Offline), &mut pending, t0),
                   MonitorState::Working);
        assert!(pending.is_empty());
    }

    #[test]
    fn a_clones_first_reading_is_shown_at_once() {
        // Nothing to be a change from, so nothing to hold.
        let mut pending = HashMap::new();
        assert_eq!(settle(MonitorState::Working, None, &mut pending, Instant::now()),
                   MonitorState::Working);
        assert!(pending.is_empty());
    }

    #[test]
    fn a_clone_that_left_the_fleet_stops_being_tracked() {
        let t0 = Instant::now();
        let mut pending = HashMap::new();
        settle(MonitorState::Idle, Some(MonitorState::Working), &mut pending, t0);
        assert_eq!(pending.len(), 1);
        let mut empty = HashMap::new();
        debounce(&mut empty, &[], &mut pending, t0);
        assert!(pending.is_empty());
    }

    #[test]
    fn one_clone_being_held_does_not_hold_another() {
        let t0 = Instant::now();
        let mut pending = HashMap::new();
        let clones = [
            clone_row("held", None, Some(MonitorState::Working)),
            clone_row("free", None, Some(MonitorState::Working)),
        ];
        let mut next = HashMap::from([
            ("held".to_string(), MonitorState::Idle),
            ("free".to_string(), MonitorState::Working),
        ]);
        debounce(&mut next, &clones, &mut pending, t0);
        assert_eq!(next["held"], MonitorState::Working, "its change is still young");
        assert_eq!(next["free"], MonitorState::Working, "it proposed no change at all");
        assert_eq!(pending.keys().collect::<Vec<_>>(), vec!["held"]);
    }

    #[test]
    fn a_parent_stays_working_while_a_sub_clone_works() {
        // The parent's own signals are quiet: it dispatched the work and is waiting on it.
        let clones = [
            clone_row("parent", None, None),
            clone_row("sub-a", Some("parent"), None),
            clone_row("sub-b", Some("parent"), None),
        ];
        let mut next = HashMap::from([
            ("parent".to_string(), MonitorState::Idle),
            ("sub-a".to_string(), MonitorState::Working),
            ("sub-b".to_string(), MonitorState::Idle),
        ]);
        lift_sub_clone_activity(&mut next, &clones);
        assert_eq!(next["parent"], MonitorState::Working);
        assert_eq!(next["sub-b"], MonitorState::Idle, "a sibling is not lifted");
    }

    #[test]
    fn a_parent_goes_idle_once_every_sub_clone_is_idle() {
        let clones =
            [clone_row("parent", None, None), clone_row("sub-a", Some("parent"), None)];
        let mut next = HashMap::from([
            ("parent".to_string(), MonitorState::Idle),
            ("sub-a".to_string(), MonitorState::Idle),
        ]);
        lift_sub_clone_activity(&mut next, &clones);
        assert_eq!(next["parent"], MonitorState::Idle);
    }

    #[test]
    fn an_offline_parent_is_not_lifted_by_a_working_sub_clone() {
        // Offline is about the container, and the row must not claim a stopped clone is working.
        let clones =
            [clone_row("parent", None, None), clone_row("sub-a", Some("parent"), None)];
        let mut next = HashMap::from([
            ("parent".to_string(), MonitorState::Offline),
            ("sub-a".to_string(), MonitorState::Working),
        ]);
        lift_sub_clone_activity(&mut next, &clones);
        assert_eq!(next["parent"], MonitorState::Offline);
    }

    #[test]
    fn an_unreachable_sub_clone_holds_its_parent_by_its_stored_state() {
        // A failed liveness probe leaves the sub clone out of this tick's map. It is still shown
        // as working, so it still holds its parent working.
        let clones = [
            clone_row("parent", None, None),
            clone_row("sub-a", Some("parent"), Some(MonitorState::Working)),
        ];
        let mut next = HashMap::from([("parent".to_string(), MonitorState::Idle)]);
        lift_sub_clone_activity(&mut next, &clones);
        assert_eq!(next["parent"], MonitorState::Working);
    }

    #[test]
    fn a_top_level_clones_state_is_untouched() {
        let clones = [clone_row("solo", None, None), clone_row("other", None, None)];
        let mut next = HashMap::from([
            ("solo".to_string(), MonitorState::Idle),
            ("other".to_string(), MonitorState::Working),
        ]);
        lift_sub_clone_activity(&mut next, &clones);
        assert_eq!(next["solo"], MonitorState::Idle);
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
    fn activity_bus_is_monotonic_and_prunes() {
        let act = ActivityBus::new();
        assert_eq!(act.last_active_at("a"), None);
        act.mark("a", 100);
        act.mark("a", 50); // out-of-order frame must not move it backwards
        assert_eq!(act.last_active_at("a"), Some(100));
        act.mark("a", 150);
        assert_eq!(act.last_active_at("a"), Some(150));

        act.mark("b", 7);
        act.retain(&HashSet::from(["a".to_string()]));
        assert_eq!(act.last_active_at("a"), Some(150));
        assert_eq!(act.last_active_at("b"), None);
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
