//! Docker-backed clone maintenance and server-owned lifecycle state.
//!
//! Docker determines whether a managed container is running; [`crate::stuck`] decides whether
//! a running one is `working` or `idle`, by reading Claude Code's own session registry and
//! agent hooks and, for the cases those cannot settle, asking a cheap model one question.
//!
//! The poller is two halves. [`FleetPoll::tick`] owns the tick's state and the fixed order it
//! folds readings in — which is where every incident recorded in this file happened — and
//! reaches Docker, the kernel and the judge only through [`FleetProbe`]. [`LiveProbe`] is that
//! interface's production implementation and the test module holds a scripted one, so a sequence
//! of ticks is an ordinary unit test. Everything else here is the volatile buses a tick
//! publishes on.

use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::sync::RwLock as StdRwLock;
use std::time::{Duration, Instant};

use tokio::sync::broadcast;
use wire::{ContainerStats, LxcStats, MonitorState, RmngClone};

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
        Self {
            tx,
            latest: StdRwLock::new((HashMap::new(), "{}".to_string())),
        }
    }

    /// The latest published map (JSON) plus a live receiver for a new `/events` subscriber.
    pub fn subscribe(&self) -> (String, broadcast::Receiver<String>) {
        (self.latest.read().unwrap().1.clone(), self.tx.subscribe())
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
        Self {
            tx,
            latest: StdRwLock::new((None, "null".to_string())),
        }
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
        self.last_active
            .write()
            .unwrap()
            .retain(|id, _| ids.contains(id));
    }
}

/// One clone Docker actually answered for this tick: id, running, the reading its raw sample
/// rated to, and its IP. A [`Probe`] whose `running` is `None` never becomes one of these, which
/// is what leaves an unreachable clone's stored state untouched.
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
    let previous = previous.replace(CpuSample {
        usage_usec,
        sampled_at: now,
    })?;
    let usage_delta = usage_usec.checked_sub(previous.usage_usec)? as f64;
    let elapsed_usec = now
        .checked_duration_since(previous.sampled_at)?
        .as_secs_f64()
        * 1_000_000.0;
    (elapsed_usec > 0.0).then_some((usage_delta / elapsed_usec) * 100.0 / CT105_CPU_CAPACITY)
}

/// One CT 105-wide CPU/RAM/disk sample. Every cgroup input is read through PID 1's root so the
/// result includes the Docker daemon and other LXC processes, not merely managed clones.
///
/// Raw counters, not a rate: [`FleetPoll::rate_lxc`] turns the CPU counter into a percentage on
/// the tick's own clock, so the CT gauge and every clone row are rated against one instant.
async fn sample_lxc() -> Option<LxcUsage> {
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
            tracing::debug!(error = %e, "CT disk sample unavailable");
            None
        }
    };

    Some(LxcUsage {
        usage_usec: cpu,
        mem_used: memory.used,
        mem_limit: memory.limit,
        disk_used,
    })
}

/// One bounded CPU/RAM sample plus the bridge IP from one Docker runtime inspect. CPU and memory
/// both come from the clone's own cgroup through the inspect's PID, which is also the sole source
/// for the persisted IP — avoiding a clone-recreate race between separate inspections.
///
/// Raw counters, like [`sample_lxc`]. The clone's prior counter lives on [`FleetPoll`], one side
/// of the probe seam away, so the first sample after a clone appears yields no CPU reading (the
/// CT-wide gauge behaves the same way) and a restart rates from its fresh zero.
async fn sample_clone(app: &App, host: &RmngClone) -> (Option<CloneUsage>, Option<Option<String>>) {
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

    (
        Some(CloneUsage {
            usage_usec: cpu,
            mem_used: memory.used,
            mem_limit: memory.limit,
        }),
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

/// How long a clone has to keep reading idle before that is published.
const DEBOUNCE: Duration = Duration::from_secs(60);

/// Hold a `working → idle` slide back until it has stood for [`DEBOUNCE`].
///
/// This sits outside every rule that decides a state: per-session verdicts, the model, the
/// first-minute floor, the blind-home hold, and [`lift_sub_clone_activity`] have all run by the
/// time it sees a clone. It knows nothing about why a state changed, only that it did, so a
/// slide that reverses inside the window is never shown at all. Over a recent 15.5 hours across
/// CT 105 and CT 106, 36% of changes reversed inside 30 seconds.
///
/// One direction only. A clone that starts working shows it on the next tick, because that is
/// news an operator wants at once and a late-lit dot would make short turns invisible. Only a
/// slide INTO `idle` waits — from `working`, or from `unknown` on the tick a judge comes back —
/// which is the direction that raises the unread badge and the browser notification, and the one
/// a flapping verdict makes noisy. A slide into `unknown` is not held: it raises nothing, so
/// there is nothing to be noisy with, and an operator should learn at once that the reading
/// stopped being trustworthy.
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
    let shown: HashMap<&str, Option<MonitorState>> = clones
        .iter()
        .map(|c| (c.id.as_str(), c.monitor_state))
        .collect();
    for (id, proposed) in next.iter_mut() {
        // Only a working clone going idle waits. A clone we do not show, one with no state yet,
        // one that is not currently working, anything touching `offline`, and a tick that
        // proposed what is already up all go straight through.
        // A clone leaving `unknown` is held on the same terms as one leaving `working`: the
        // recovery tick is the one where a verdict is most likely to flap (36% of changes
        // reverse inside 30 seconds), and it is also the tick that spends the replay baseline
        // and raises the badge. `unknown → offline` stays unheld: a container that died
        // mid-outage is news the moment we can see it again.
        let stored = shown.get(id.as_str()).copied().flatten();
        let held = matches!(
            stored,
            Some(MonitorState::Working) | Some(MonitorState::Unknown)
        ) && *proposed == MonitorState::Idle;
        if !held {
            pending.remove(id);
            continue;
        }
        // Hold at whatever is actually up rather than substituting `working`: showing
        // `working` for a clone we have told the operator we cannot read would be a new lie.
        // `held` already proved this is `Some`.
        let Some(shown) = stored else { continue };
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

/// Whether `activity_unknown` disagrees with the reading this tick proposes.
///
/// A function, and called from the change gate rather than inlined there, because the pair can
/// legitimately be out of step while `monitor_state` itself is not: `Unknown` is stored as
/// itself and serialized as `idle`, so a state.json written during an outage reloads as `Idle`
/// with the flag still set. A gate watching only `monitor_state` sees no change, skips the whole
/// mutation, and leaves the fleet reading "no reading" against a judge that is answering — on
/// disk, across restarts. Extracted so a test can hold the real predicate rather than a copy of
/// it, which is the failure mode this module has already hit twice.
fn flag_is_stale(proposed: MonitorState, host: &RmngClone) -> bool {
    (proposed == MonitorState::Unknown) != host.activity_unknown
}

/// What this clone last read as before the judge went dark, and the bookkeeping that keeps it.
///
/// `unread` fires on `working → not-working`. Once a clone sits at `unknown` its stored state is
/// no longer `working`, so the plain transition test would never fire again and a clone that
/// really did stop mid-outage would be swallowed in silence. The pre-outage reading is taken on
/// the way in, held across however many blind ticks follow, and spent on the way out.
///
/// A function rather than an inline match because it is the whole of the replay contract, and
/// the only way a test can exercise it is by calling it: reimplementing these arms in a test
/// asserts against a copy, and the copy stays green while the original breaks.
fn replay_baseline(
    blinded: &mut HashMap<String, MonitorState>,
    id: &str,
    stored: Option<MonitorState>,
    next: MonitorState,
) -> Option<MonitorState> {
    let before = match (stored, next) {
        // Already blind: the reading in hand says nothing, so the held one stands.
        (Some(MonitorState::Unknown), _) => blinded.get(id).copied(),
        // Going blind: remember what we are giving up. A clone with no reading yet has
        // nothing to remember and must not invent one.
        (was, MonitorState::Unknown) => {
            // `or_insert`, never `insert`: an external edit to state.json collapses an
            // in-memory `Unknown` back to `Idle`, and overwriting here would replace a real
            // `Working` baseline with that `Idle` and swallow the stop it was holding.
            if let Some(was) = was {
                blinded.entry(id.to_string()).or_insert(was);
            }
            blinded.get(id).copied().or(was)
        }
        // The held baseline outranks `was` here for the same reason `or_insert` outranks
        // `insert` above: a reload collapses an in-memory `Unknown` to `Idle`, so `was` is the
        // collapsed value while `blinded` still holds what the clone really was. Taking `was`
        // would drop a `Working` baseline on the floor and swallow the stop it was holding —
        // a badge `main` would have raised. Inert in healthy operation: an entry only exists
        // while the clone reads `Unknown`.
        (was, _) => blinded.get(id).copied().or(was),
    };
    // Spent on any real reading, so a replay can fire once and only once.
    if next != MonitorState::Unknown {
        blinded.remove(id);
    }
    before
}

/// Whether a `working → not-working` transition should raise the unread badge + browser
/// notification for a clone. Suppressed when the clone is currently selected (the operator is
/// already looking at it), or — for an **idle** slide specifically — when the operator has
/// viewed the clone at or after its last token activity: they have already seen its final output,
/// so its slide into idle is not news and it simply shows the gray "not working" dot. An
/// **offline** transition (the container died) is always surfaced, even if recently viewed. A
/// slide into **unknown** never raises anything at all: that state is about the judge being
/// unreachable, not about the agent, and it is the one case where we do not know.
fn should_flag_unread(
    next: MonitorState,
    is_selected: bool,
    last_viewed_at: Option<i64>,
    last_token_at: Option<i64>,
) -> bool {
    // `unknown` says the judge could not be reached, which is news about US. Raising a badge
    // for it would tell an operator their agents stopped, on the one occasion we specifically
    // do not know that. The transition is silent and the real answer keeps until the judge is
    // back — see the replay in [`poll_once`].
    if next == MonitorState::Unknown {
        return false;
    }
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

/// One clone's cgroup reading for one tick, exactly as the kernel handed it over.
///
/// Raw counters rather than a rate: `usage_usec` is cumulative, so turning it into a percentage
/// needs the previous tick's sample. That sample belongs to [`FleetPoll`], not to the probe, which
/// is what puts the counter-reset rule on the testable side of [`FleetProbe`] — a container
/// restart is a thing a test can script.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct CloneUsage {
    /// Cumulative CPU time for this clone's cgroup, in microseconds.
    pub usage_usec: u64,
    pub mem_used: u64,
    pub mem_limit: u64,
}

/// The CT 105-wide reading for one tick, raw for the same reason [`CloneUsage`] is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct LxcUsage {
    pub usage_usec: u64,
    pub mem_used: u64,
    pub mem_limit: u64,
    pub disk_used: Option<u64>,
}

/// One clone's reading for one tick, as the probes found it.
#[derive(Debug, Clone)]
pub(crate) struct Probe {
    pub id: String,
    /// What Docker said about the container. `None` is "Docker did not answer" — a daemon
    /// hiccup or a timeout — and is emphatically NOT `Some(false)`: an unavailable daemon
    /// leaves the lifecycle unchanged, and only a successful liveness response may write
    /// `offline`.
    pub running: Option<bool>,
    pub usage: Option<CloneUsage>,
    /// The outer `Option` is "did we look", the inner one is "does it have one".
    pub ip: Option<Option<String>>,
}

/// Everything one sweep of the fleet found: a reading per clone plus the CT-wide sample.
pub(crate) struct FleetReading {
    pub clones: Vec<Probe>,
    pub lxc: Option<LxcUsage>,
}

/// The two things a tick cannot do in a test: ask Docker and the kernel what the fleet looks
/// like, and ask [`crate::stuck`] whether a running clone is working.
///
/// Everything else a tick does is arithmetic over what comes back through here — what a silent
/// daemon means, what an archive completing mid-flight means, the sub-clone lift, the debounce,
/// the outage replay — so scripting this interface makes a sequence of ticks an ordinary unit
/// test. [`LiveProbe`] is the production implementation and the test module holds a scripted
/// one; two implementations, so the seam is real.
///
/// Declared as `Sync` with explicit `Send` futures rather than as plain `async fn`s because the
/// poller runs under `tokio::spawn`: an `async fn` in a trait promises nothing about `Send`, and
/// the spawn in `main` would stop compiling.
pub(crate) trait FleetProbe: Sync {
    /// One concurrent sweep: Docker liveness, the cgroup sample and the bridge IP for every
    /// host, plus the CT-wide sample. The concurrency lives in here rather than in the tick, so
    /// the tick reads as the fixed sequence it is.
    fn probe(&self, app: &App, hosts: &[RmngClone]) -> impl Future<Output = FleetReading> + Send;

    /// Working-vs-idle for the clones Docker said are up. One call for the whole fleet: deciding
    /// this reads each clone's files and may ask a model, so it happens once, concurrently,
    /// rather than inline per clone.
    fn resolve(
        &self,
        app: &App,
        ids: Vec<String>,
    ) -> impl Future<Output = HashMap<String, MonitorState>> + Send;
}

/// The production implementation: Docker, cgroup-v2 and the real judge.
pub(crate) struct LiveProbe;

impl FleetProbe for LiveProbe {
    async fn probe(&self, app: &App, hosts: &[RmngClone]) -> FleetReading {
        let clones = futures::future::join_all(hosts.iter().map(|host| async move {
            // An unavailable Docker daemon leaves the lifecycle unchanged; it is not proof that
            // the container stopped. Only a successful liveness response may write `offline`.
            let running =
                match tokio::time::timeout(FETCH_TIMEOUT, app.docker.is_running(&host.id)).await {
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
            let (usage, ip) = if running == Some(true) {
                match tokio::time::timeout(FETCH_TIMEOUT, sample_clone(app, host)).await {
                    Ok(sample) => sample,
                    Err(_) => {
                        tracing::debug!(host = %host.id, "clone resource sample timed out");
                        (None, None)
                    }
                }
            } else if running == Some(false) {
                // A stopped container has no bridge IP. Saying so is a reading, unlike the
                // silence above it.
                (None, Some(None))
            } else {
                (None, None)
            };
            Probe {
                id: host.id.clone(),
                running,
                usage,
                ip,
            }
        }));
        let (lxc, clones) = tokio::join!(sample_lxc(), clones);
        FleetReading { clones, lxc }
    }

    async fn resolve(&self, app: &App, ids: Vec<String>) -> HashMap<String, MonitorState> {
        crate::stuck::resolve_fleet(app, ids).await
    }
}

/// What one tick decided: what to publish, and what to write onto each clone's row.
///
/// Deciding and writing are deliberately two steps. Everything in here is settled by arithmetic
/// over [`FleetProbe`] answers, which is what a test can hold; [`Self::apply`] is the single
/// place the monitor touches the store.
pub(crate) struct FleetUpdate {
    /// The state each clone settled on this tick, after the sub-clone lift and the debounce.
    /// A clone Docker did not answer for is ABSENT, which is what leaves its stored reading
    /// alone — and a clone archived while the probes were in flight is absent too.
    pub states: HashMap<String, MonitorState>,
    /// The bridge IP read for each clone this tick looked at.
    pub ips: HashMap<String, Option<String>>,
    /// The unread badge this tick decides: `true` raises it, `false` clears it, absent leaves it
    /// where it is. Empty whenever [`Self::changed`] is false — see the gate in
    /// [`FleetPoll::tick`].
    pub unread: HashMap<String, bool>,
    /// Whether any of the above differs from what is stored. False means the whole write is
    /// skipped, so an idle fleet never rewrites `state.json`.
    pub changed: bool,
    /// The per-clone CPU/RAM map to publish, already pruned to the live fleet.
    pub stats: HashMap<String, ContainerStats>,
    /// The CT 105-wide sample to publish; `None` explicitly clears an unavailable reading.
    pub lxc: Option<LxcStats>,
    /// The live managed fleet as re-read AFTER the probes returned — the rows the chat
    /// listeners are spawned against.
    pub active: Vec<RmngClone>,
}

impl FleetUpdate {
    /// Write this tick's decision. The only place the monitor mutates the store.
    fn apply(&self, app: &App) {
        if !self.changed {
            return;
        }
        app.store.mutate(|state| {
            for host in &mut state.hosts {
                // Checked here as well as in the tick's second filter: an archive can also
                // complete between that read and this write.
                if host.archived || !host.managed {
                    continue;
                }
                if let Some(&monitor_state) = self.states.get(&host.id) {
                    if let Some(&unread) = self.unread.get(&host.id) {
                        host.unread = unread;
                    }
                    host.monitor_state = Some(monitor_state);
                    // The wire says `idle` for an unreachable judge, which is what every client
                    // showed before this existed; this is what lets a client that knows better
                    // say "no reading" instead. See `wire::MonitorState`.
                    host.activity_unknown = monitor_state == MonitorState::Unknown;
                }
                if let Some(ip) = self.ips.get(&host.id) {
                    host.local_ip = ip.clone();
                }
            }
        });
    }
}

/// The monitor tick, and everything it has to remember between ticks.
///
/// These five maps used to live as `&mut` arguments owned by the poll loop, which meant nothing
/// could be run tick-after-tick without rebuilding all of them — and the tick's ORDER, which is
/// where every incident in this file happened, was the one part with no test at all. They are
/// fields now, so a test constructs one poll and drives it.
pub(crate) struct FleetPoll {
    previous_lxc_cpu: Option<CpuSample>,
    /// Each clone's last CPU counter, so a rate can be taken against it. Bounded to the live
    /// fleet every tick, so archived and deleted clones cannot accumulate here across the life
    /// of a long-running server.
    previous_clone_cpu: HashMap<String, CpuSample>,
    /// Slides into `idle` that are waiting out [`DEBOUNCE`], and when each started.
    pending_state: HashMap<String, (MonitorState, Instant)>,
    /// What each clone read as before an outage blinded it, so a stop that happened while the
    /// judge was down still surfaces once it is back. See [`replay_baseline`].
    blinded: HashMap<String, MonitorState>,
    /// The stats map this poll produced last tick — what [`pick_stat`] carries forward across a
    /// transient sampling gap. Kept here rather than read back off [`StatsBus`]: the bus is a
    /// broadcast, not the tick's memory, and a poll that owns its own previous map is one a test
    /// can run without publishing anything.
    last_stats: HashMap<String, ContainerStats>,
}

impl FleetPoll {
    pub(crate) fn new() -> Self {
        Self {
            previous_lxc_cpu: None,
            previous_clone_cpu: HashMap::new(),
            pending_state: HashMap::new(),
            blinded: HashMap::new(),
            last_stats: HashMap::new(),
        }
    }

    /// One tick: probes in, the decided update out. Pure given its probes.
    ///
    /// The order below is load-bearing, and the reason for each step sits on the step. `now` is
    /// the tick's single clock — every CPU rate and the debounce read it — so one tick rates the
    /// whole fleet against one instant instead of against however long each probe happened to
    /// take. Nothing is lost over a long run: this tick's `now` is also the start of the next
    /// tick's interval.
    pub(crate) async fn tick(
        &mut self,
        app: &App,
        probe: &impl FleetProbe,
        now: Instant,
    ) -> FleetUpdate {
        let hosts: Vec<RmngClone> = app
            .store
            .get()
            .hosts
            .into_iter()
            .filter(|host| host.managed && !host.archived)
            .collect();

        let FleetReading { clones, lxc } = probe.probe(app, &hosts).await;
        let lxc = self.rate_lxc(lxc, now);

        // Clones whose liveness Docker actually answered for. An unreachable daemon leaves a
        // clone out entirely, which is what keeps its stored state untouched.
        let mut settled: Vec<SettledProbe> = Vec::with_capacity(clones.len());
        for Probe {
            id,
            running,
            usage,
            ip,
        } in clones
        {
            let stats = match (running, usage) {
                // A stopped container's counter is gone; drop the sample so a later restart
                // rates from its fresh zero rather than against a pre-stop total.
                (Some(false), _) => {
                    self.previous_clone_cpu.remove(&id);
                    None
                }
                (_, Some(usage)) => self.rate(&id, usage, now),
                (_, None) => None,
            };
            let Some(running) = running else {
                continue;
            };
            settled.push((id, running, stats, ip));
        }

        // Only clones Docker says are UP are judged: a stopped one is `offline` whatever its
        // files say, and one we could not reach is not asked about at all.
        let states = probe
            .resolve(
                app,
                settled
                    .iter()
                    .filter(|(_, up, _, _)| *up)
                    .map(|(id, ..)| id.clone())
                    .collect(),
            )
            .await;

        let mut next: HashMap<String, MonitorState> = HashMap::with_capacity(settled.len());
        let mut stats_map = HashMap::new();
        let mut ips: HashMap<String, Option<String>> = HashMap::new();
        for (id, running, stats, ip) in settled {
            let state = if running {
                states.get(&id).copied().unwrap_or(MonitorState::Idle)
            } else {
                MonitorState::Offline
            };
            if let Some(stats) = pick_stat(stats, state, self.last_stats.get(&id)) {
                stats_map.insert(id.clone(), stats);
            }
            if let Some(ip) = ip {
                ips.insert(id.clone(), ip);
            }
            next.insert(id, state);
        }

        // An archive operation may complete while Docker and cgroup calls are in flight. Read
        // the fleet a second time so its intentional stop cannot race into lifecycle, stats, or
        // chat updates.
        let snapshot = app.store.get();
        let selected = snapshot.selected;
        let active: Vec<RmngClone> = snapshot
            .hosts
            .into_iter()
            .filter(|host| host.managed && !host.archived)
            .collect();
        let active_ids: HashSet<String> = active.iter().map(|host| host.id.clone()).collect();
        next.retain(|id, _| active_ids.contains(id));
        lift_sub_clone_activity(&mut next, &active);
        // Last of all, so a change that reverses inside a minute is never shown. See [`debounce`].
        debounce(&mut next, &active, &mut self.pending_state, now);
        stats_map.retain(|id, _| active_ids.contains(id));
        ips.retain(|id, _| active_ids.contains(id));
        // Bound every per-clone map to the live fleet, so archived and deleted clones cannot
        // accumulate in them across the life of a long-running server.
        self.previous_clone_cpu
            .retain(|id, _| active_ids.contains(id));
        self.blinded.retain(|id, _| active_ids.contains(id));
        app.views.retain(&active_ids);
        app.activity.retain(&active_ids);

        let changed = active.iter().any(|host| {
            next.get(&host.id).is_some_and(|state| Some(*state) != host.monitor_state)
                // `activity_unknown` is derived from the same reading, but it does NOT move
                // with `monitor_state`: `Unknown` is stored as itself and serialized as `idle`,
                // so a state.json written during an outage reloads as `Idle` + the flag set.
                // The first healthy tick then proposes `Idle`, which equals the stored state,
                // and without this the whole mutation is skipped — leaving every clone saying
                // "no reading" against a judge that is answering fine, persisted, and
                // surviving restarts until something else about a clone happens to change.
                || next.get(&host.id).is_some_and(|s| flag_is_stale(*s, host))
                || ips.get(&host.id).is_some_and(|ip| *ip != host.local_ip)
        });

        // The unread decision, and the replay bookkeeping behind it, run only when something is
        // actually going to be written — the same gate they sat behind when they lived inside
        // `store.mutate`. A tick that changes nothing cannot be the tick a held stop comes out
        // on: a held stop means the stored state is `unknown`, and any real reading out of
        // `unknown` is itself a change.
        //
        // Both inputs to the decision (last-viewed and last-token-activity) are read here,
        // outside the store mutation, so neither of their locks is held across it and neither is
        // re-locked per host inside it.
        let mut unread = HashMap::new();
        if changed {
            for host in &active {
                let Some(&state) = next.get(&host.id) else {
                    continue;
                };
                // What this clone last read as before the judge went dark. While a clone sits at
                // `unknown` its stored state is no longer `working`, so the plain transition
                // test below would never fire and a clone that really did stop during an outage
                // would be swallowed silently. Remembered on the way in, spent here.
                let before =
                    replay_baseline(&mut self.blinded, &host.id, host.monitor_state, state);
                if before == Some(MonitorState::Working) && state != MonitorState::Working {
                    let is_selected = selected.as_deref() == Some(host.id.as_str());
                    if should_flag_unread(
                        state,
                        is_selected,
                        app.views.last_viewed(&host.id),
                        app.activity.last_active_at(&host.id),
                    ) {
                        unread.insert(host.id.clone(), true);
                    }
                } else if state == MonitorState::Working {
                    unread.insert(host.id.clone(), false);
                }
            }
        }

        self.last_stats = stats_map.clone();
        FleetUpdate {
            states: next,
            ips,
            unread,
            changed,
            stats: stats_map,
            lxc,
            active,
        }
    }

    /// Turn one clone's raw counter into a publishable reading, against its previous sample.
    fn rate(&mut self, id: &str, usage: CloneUsage, now: Instant) -> Option<ContainerStats> {
        let mut previous = self.previous_clone_cpu.get(id).copied();
        let pct = cpu_pct(&mut previous, usage.usage_usec, now);
        // `cpu_pct` leaves this tick's sample behind whether or not it could rate it, which is
        // what lets the tick after a counter reset rate from the fresh zero.
        if let Some(sample) = previous {
            self.previous_clone_cpu.insert(id.to_string(), sample);
        }
        Some(ContainerStats {
            cpu_pct: pct?,
            mem_used: usage.mem_used,
            mem_limit: usage.mem_limit,
        })
    }

    /// The same conversion for the CT-wide gauge, so a clone reading 50% and the CT reading 50%
    /// mean the same eight busy cores.
    fn rate_lxc(&mut self, usage: Option<LxcUsage>, now: Instant) -> Option<LxcStats> {
        let usage = usage?;
        Some(LxcStats {
            cpu_pct: cpu_pct(&mut self.previous_lxc_cpu, usage.usage_usec, now),
            mem_used: usage.mem_used,
            mem_limit: usage.mem_limit,
            disk_used: usage.disk_used,
        })
    }
}

/// One tick, published and written. Only the two edges the tick deliberately does not own live
/// here: the SSE publishes and the store write.
async fn poll_once(poll: &mut FleetPoll, app: &App, probe: &impl FleetProbe) {
    let update = poll.tick(app, probe, Instant::now()).await;

    app.stats.publish(&update.stats);
    app.lxc_stats.publish(&update.lxc);

    for host in &update.active {
        if update
            .states
            .get(&host.id)
            .is_some_and(|state| *state != MonitorState::Offline)
        {
            crate::chat::ensure_autonomous_listener(app, host);
        }
    }

    update.apply(app);
}

/// Background loop; spawned once at startup.
pub async fn run(app: App) {
    tracing::info!(
        "monitor poller started (every {}s)",
        POLL_INTERVAL.as_secs()
    );
    let mut poll = FleetPoll::new();
    let probe = LiveProbe;
    loop {
        poll_once(&mut poll, &app, &probe).await;
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
        }
    }

    fn lxc_stat(cpu: Option<f64>) -> LxcStats {
        LxcStats {
            cpu_pct: cpu,
            mem_used: 16u64 << 30,
            mem_limit: 264u64 << 30,
            disk_used: Some(320u64 << 30),
        }
    }

    // ---------------------------------------------------------------------------------------
    // The tick itself.
    //
    // Every pure helper above has had a test since the day it was extracted. The ORDER they run
    // in did not, and the comments in this file are the record of what that cost: a silent
    // daemon read as `offline`, an archive completing mid-flight racing into the lifecycle, a
    // flapping verdict shown to the operator, a stop swallowed by an outage, a restarted
    // container's counter read as a spike. Those are sequences, not single calls, so they are
    // tested as sequences — one [`FleetPoll`], scripted probes, tick after tick.
    // ---------------------------------------------------------------------------------------

    /// A fleet whose Docker answers and judge verdicts the test writes.
    ///
    /// The second implementation of [`FleetProbe`], which is what makes that interface a seam
    /// rather than a shape: a tick driven through this one touches no daemon, no cgroup and no
    /// model, so a sequence of ticks is an ordinary unit test.
    #[derive(Default)]
    struct Scripted {
        /// What Docker says per clone id. An id with no entry answers "up, nothing sampled".
        docker: StdRwLock<HashMap<String, Probe>>,
        /// What the judge says per clone id. An id with no entry is not answered for at all,
        /// which the tick reads as `idle` exactly as the real judge's silence is read.
        verdicts: StdRwLock<HashMap<String, MonitorState>>,
        lxc: StdRwLock<Option<LxcUsage>>,
        /// The ids the last tick asked the judge about, sorted.
        asked: StdRwLock<Vec<String>>,
        /// Runs while the tick's probes are "in flight" — the window the second fleet read
        /// exists for.
        during: StdRwLock<Option<Box<dyn FnOnce(&App) + Send + Sync>>>,
    }

    impl Scripted {
        fn answer(
            &self,
            id: &str,
            running: Option<bool>,
            usage: Option<CloneUsage>,
            ip: Option<Option<String>>,
        ) -> &Self {
            self.docker.write().unwrap().insert(
                id.to_string(),
                Probe {
                    id: id.to_string(),
                    running,
                    usage,
                    ip,
                },
            );
            self
        }

        /// Docker answers "up", with no cgroup sample to go with it.
        fn up(&self, id: &str) -> &Self {
            self.answer(id, Some(true), None, None)
        }

        /// Docker answers "up" and the cgroup hands back this cumulative CPU counter.
        fn usage(&self, id: &str, usage_usec: u64) -> &Self {
            self.answer(
                id,
                Some(true),
                Some(CloneUsage {
                    usage_usec,
                    mem_used: 1 << 30,
                    mem_limit: 8u64 << 30,
                }),
                None,
            )
        }

        /// Docker answers "stopped". A stopped container has no bridge IP, and saying so is a
        /// reading.
        fn down(&self, id: &str) -> &Self {
            self.answer(id, Some(false), None, Some(None))
        }

        /// Docker does not answer at all: a dead daemon or a timed-out call. NOT a stopped
        /// container — that distinction is the whole point of the `Option`.
        fn silent(&self, id: &str) -> &Self {
            self.answer(id, None, None, None)
        }

        /// The judge's verdict for `id`, from this tick on.
        fn verdict(&self, id: &str, state: MonitorState) -> &Self {
            self.verdicts.write().unwrap().insert(id.to_string(), state);
            self
        }

        /// Do this while the next tick's probes are in flight.
        fn during(&self, f: impl FnOnce(&App) + Send + Sync + 'static) {
            *self.during.write().unwrap() = Some(Box::new(f));
        }

        fn asked(&self) -> Vec<String> {
            self.asked.read().unwrap().clone()
        }
    }

    impl FleetProbe for Scripted {
        async fn probe(&self, app: &App, hosts: &[RmngClone]) -> FleetReading {
            let during = self.during.write().unwrap().take();
            if let Some(during) = during {
                during(app);
            }
            let scripted = self.docker.read().unwrap();
            let clones = hosts
                .iter()
                .map(|host| {
                    scripted.get(&host.id).cloned().unwrap_or(Probe {
                        id: host.id.clone(),
                        running: Some(true),
                        usage: None,
                        ip: None,
                    })
                })
                .collect();
            FleetReading {
                clones,
                lxc: *self.lxc.read().unwrap(),
            }
        }

        async fn resolve(&self, _app: &App, ids: Vec<String>) -> HashMap<String, MonitorState> {
            let mut asked = ids.clone();
            asked.sort();
            *self.asked.write().unwrap() = asked;
            let verdicts = self.verdicts.read().unwrap();
            ids.into_iter()
                .filter_map(|id| verdicts.get(&id).map(|state| (id, *state)))
                .collect()
        }
    }

    /// An app holding one managed clone per id, as a fresh fleet reads: no stored state yet.
    fn fleet(ids: &[&str]) -> App {
        let app = App::test_app();
        app.store.mutate(|state| {
            state.hosts = ids.iter().map(|id| clone_row(id, None, None)).collect();
        });
        app
    }

    fn stored(app: &App, id: &str) -> RmngClone {
        app.store
            .get()
            .hosts
            .into_iter()
            .find(|host| host.id == id)
            .expect("clone is still in the fleet")
    }

    /// One production tick minus the SSE publishes: decide, then write.
    async fn run_tick(
        poll: &mut FleetPoll,
        app: &App,
        probe: &Scripted,
        now: Instant,
    ) -> FleetUpdate {
        let update = poll.tick(app, probe, now).await;
        update.apply(app);
        update
    }

    fn at(t0: Instant, secs: u64) -> Instant {
        t0 + Duration::from_secs(secs)
    }

    #[tokio::test]
    async fn a_silent_docker_daemon_leaves_the_reading_where_it_was() {
        // An unavailable daemon is news about US. Reading it as `offline` turns one Docker
        // hiccup into a fleet of dead clones on the operator's screen, and writes that to disk.
        let app = fleet(&["c"]);
        let probe = Scripted::default();
        let mut poll = FleetPoll::new();
        let t0 = Instant::now();

        probe.verdict("c", MonitorState::Working);
        let update = run_tick(&mut poll, &app, &probe, t0).await;
        assert_eq!(update.states["c"], MonitorState::Working);
        assert_eq!(stored(&app, "c").monitor_state, Some(MonitorState::Working));

        probe.silent("c");
        let update = run_tick(&mut poll, &app, &probe, at(t0, 4)).await;
        assert!(
            !update.states.contains_key("c"),
            "no liveness answer is no reading"
        );
        assert!(!update.changed, "and nothing to write");
        assert_eq!(
            stored(&app, "c").monitor_state,
            Some(MonitorState::Working),
            "the stored reading stands; it is not offline"
        );
        assert!(
            probe.asked().is_empty(),
            "a clone we could not reach is not put to the judge either"
        );

        // And it comes back on the tick the daemon does.
        probe.up("c");
        let update = run_tick(&mut poll, &app, &probe, at(t0, 8)).await;
        assert_eq!(update.states["c"], MonitorState::Working);
        assert_eq!(probe.asked(), vec!["c".to_string()]);
    }

    #[tokio::test]
    async fn a_clone_archived_mid_tick_is_dropped_from_the_update() {
        // An archive operation may complete while the Docker and cgroup calls are in flight.
        // Its stop is intentional, and must not race into the lifecycle as a reading.
        let app = fleet(&["a", "b"]);
        let probe = Scripted::default();
        probe.verdict("a", MonitorState::Working);
        probe.verdict("b", MonitorState::Working).usage("b", 1_000);
        let mut poll = FleetPoll::new();

        probe.during(|app| {
            app.store.mutate(|state| {
                for host in &mut state.hosts {
                    if host.id == "b" {
                        host.archived = true;
                    }
                }
            });
        });
        let update = run_tick(&mut poll, &app, &probe, Instant::now()).await;

        assert_eq!(update.states["a"], MonitorState::Working);
        assert!(!update.states.contains_key("b"), "it left the fleet");
        assert!(!update.stats.contains_key("b"));
        assert!(
            !update.active.iter().any(|host| host.id == "b"),
            "and no chat listener is spawned against it"
        );
        assert_eq!(
            stored(&app, "b").monitor_state,
            None,
            "an intentional stop is not a reading"
        );
        assert!(
            !poll.previous_clone_cpu.contains_key("b"),
            "its CPU sample goes with it, so the map cannot grow unbounded"
        );
    }

    #[tokio::test]
    async fn a_flip_flop_is_never_shown_but_a_real_slide_is() {
        // 36% of changes reversed inside 30 seconds on the live fleet. Held one minute, a
        // reversal inside the window costs the operator nothing at all.
        let app = fleet(&["c"]);
        let probe = Scripted::default();
        let mut poll = FleetPoll::new();
        let t0 = Instant::now();

        probe.verdict("c", MonitorState::Working);
        run_tick(&mut poll, &app, &probe, t0).await;
        assert_eq!(stored(&app, "c").monitor_state, Some(MonitorState::Working));

        // It reads idle for two ticks, then comes back.
        probe.verdict("c", MonitorState::Idle);
        for secs in [4, 8] {
            assert_eq!(
                run_tick(&mut poll, &app, &probe, at(t0, secs)).await.states["c"],
                MonitorState::Working,
                "held while the clock runs"
            );
        }
        probe.verdict("c", MonitorState::Working);
        assert_eq!(
            run_tick(&mut poll, &app, &probe, at(t0, 12)).await.states["c"],
            MonitorState::Working
        );
        assert!(
            poll.pending_state.is_empty(),
            "the reversal drops the pending change rather than letting it age"
        );
        assert!(
            !stored(&app, "c").unread,
            "nothing was ever shown, so nothing was flagged"
        );

        // Now it really stops. The slide waits out its own fresh minute — it does not inherit
        // the clock of the change that reversed.
        probe.verdict("c", MonitorState::Idle);
        for secs in [16, 20, 60, 75] {
            assert_eq!(
                run_tick(&mut poll, &app, &probe, at(t0, secs)).await.states["c"],
                MonitorState::Working
            );
        }
        let update = run_tick(&mut poll, &app, &probe, at(t0, 76)).await;
        assert_eq!(update.states["c"], MonitorState::Idle);
        assert_eq!(update.unread.get("c"), Some(&true));
        assert_eq!(stored(&app, "c").monitor_state, Some(MonitorState::Idle));
        assert!(
            stored(&app, "c").unread,
            "a stop the operator has not seen raises the badge"
        );
    }

    #[tokio::test]
    async fn a_stop_during_an_outage_surfaces_on_the_tick_the_judge_returns() {
        // The replay, end to end. `unread` fires on `working → not-working`; once a clone sits
        // at `unknown` its stored state is no longer `working`, so without the baseline the
        // plain transition test never fires again and a clone that really did stop mid-outage
        // is swallowed in silence.
        let app = fleet(&["c"]);
        let probe = Scripted::default();
        let mut poll = FleetPoll::new();
        let t0 = Instant::now();

        probe.verdict("c", MonitorState::Working);
        run_tick(&mut poll, &app, &probe, t0).await;

        // The judge goes dark. That slide is not held — an operator should learn at once that
        // the reading stopped being trustworthy — and it raises nothing, because this is the
        // one case where we do not know.
        probe.verdict("c", MonitorState::Unknown);
        let update = run_tick(&mut poll, &app, &probe, at(t0, 4)).await;
        assert_eq!(update.states["c"], MonitorState::Unknown);
        assert!(update.unread.is_empty());
        assert!(
            stored(&app, "c").activity_unknown,
            "the row says 'no reading'"
        );
        assert!(!stored(&app, "c").unread);

        // However long the outage lasts, and whatever else ticks past.
        for secs in [8, 12, 300] {
            let update = run_tick(&mut poll, &app, &probe, at(t0, secs)).await;
            assert_eq!(update.states["c"], MonitorState::Unknown);
            assert!(!update.changed, "nothing to rewrite while it is dark");
            assert!(!stored(&app, "c").unread);
        }

        // The judge answers again, and says the clone is idle: it stopped in the dark. The
        // recovery tick is held like any other slide into idle — held at `unknown`, never at a
        // `working` it is not.
        probe.verdict("c", MonitorState::Idle);
        let update = run_tick(&mut poll, &app, &probe, at(t0, 304)).await;
        assert_eq!(update.states["c"], MonitorState::Unknown);
        assert!(!stored(&app, "c").unread);

        // When it stands, the stop replays.
        let update = run_tick(&mut poll, &app, &probe, at(t0, 364)).await;
        assert_eq!(update.states["c"], MonitorState::Idle);
        assert_eq!(update.unread.get("c"), Some(&true));
        assert!(stored(&app, "c").unread);
        assert!(
            !stored(&app, "c").activity_unknown,
            "and the row has a reading again"
        );

        // Exactly once: the baseline is spent, not re-fired every quiet tick afterwards.
        app.store.mutate(|state| {
            for host in &mut state.hosts {
                host.unread = false;
            }
        });
        let update = run_tick(&mut poll, &app, &probe, at(t0, 368)).await;
        assert!(update.unread.is_empty());
        assert!(!stored(&app, "c").unread);
    }

    #[tokio::test]
    async fn a_restarted_containers_counter_reset_is_never_published_as_a_spike() {
        let app = fleet(&["c"]);
        let probe = Scripted::default();
        let mut poll = FleetPoll::new();
        let t0 = Instant::now();
        probe.verdict("c", MonitorState::Working);

        probe.usage("c", 0);
        assert!(
            !run_tick(&mut poll, &app, &probe, t0)
                .await
                .stats
                .contains_key("c"),
            "one sample cannot make a rate"
        );

        // 8 of CT 105's 16 cores, for 4 seconds.
        probe.usage("c", 32_000_000);
        let update = run_tick(&mut poll, &app, &probe, at(t0, 4)).await;
        assert!((update.stats["c"].cpu_pct - 50.0).abs() < 1e-9);

        // A tick that could not sample keeps the last reading rather than blanking the row.
        probe.up("c");
        let update = run_tick(&mut poll, &app, &probe, at(t0, 8)).await;
        assert!((update.stats["c"].cpu_pct - 50.0).abs() < 1e-9);

        // The container stops. Its numbers clear, and its counter goes with it: a stopped
        // container's `usage_usec` is gone, so a later restart must rate from its fresh zero
        // rather than against the pre-stop total.
        probe.down("c");
        let update = run_tick(&mut poll, &app, &probe, at(t0, 12)).await;
        assert_eq!(update.states["c"], MonitorState::Offline);
        assert!(
            !update.stats.contains_key("c"),
            "an offline clone shows none"
        );
        assert!(!poll.previous_clone_cpu.contains_key("c"));

        // It comes back with a fresh counter: the first tick after has nothing to rate against,
        // and publishes no figure at all rather than a delta over a counter that no longer
        // exists.
        probe.usage("c", 1_000);
        let update = run_tick(&mut poll, &app, &probe, at(t0, 16)).await;
        assert_eq!(update.states["c"], MonitorState::Working);
        assert!(!update.stats.contains_key("c"));

        // The next one rates normally, from the restarted counter.
        probe.usage("c", 6_401_000);
        let update = run_tick(&mut poll, &app, &probe, at(t0, 20)).await;
        assert!(
            (update.stats["c"].cpu_pct - 10.0).abs() < 1e-9,
            "expected 10%, got {}",
            update.stats["c"].cpu_pct
        );
    }

    #[tokio::test]
    async fn a_parent_stays_working_past_the_debounce_while_its_sub_clone_works() {
        // A parent that handed work to a sub clone and is waiting produces no signals of its
        // own, so its own judge calls it idle. The lift is what keeps it working, and the point
        // of driving it across a whole minute is that the debounce alone could not: a hold
        // expires, and the row would then grey out while its group is plainly busy.
        let app = App::test_app();
        app.store.mutate(|state| {
            state.hosts = vec![
                clone_row("parent", None, None),
                clone_row("sub", Some("parent"), None),
            ];
        });
        let probe = Scripted::default();
        probe.verdict("parent", MonitorState::Working);
        probe.verdict("sub", MonitorState::Working);
        let mut poll = FleetPoll::new();
        let t0 = Instant::now();
        run_tick(&mut poll, &app, &probe, t0).await;

        // The parent dispatches and goes quiet. Long past the debounce window, it still reads
        // working — and nothing is being held back to make that true.
        probe.verdict("parent", MonitorState::Idle);
        for secs in [4, 64, 300] {
            let update = run_tick(&mut poll, &app, &probe, at(t0, secs)).await;
            assert_eq!(update.states["parent"], MonitorState::Working);
            assert!(
                poll.pending_state.is_empty(),
                "lifted, not held: a hold would have expired by now"
            );
        }

        // It goes idle once the work under it has, on the usual terms.
        probe.verdict("sub", MonitorState::Idle);
        let update = run_tick(&mut poll, &app, &probe, at(t0, 304)).await;
        assert_eq!(update.states["parent"], MonitorState::Working, "held now");
        let update = run_tick(&mut poll, &app, &probe, at(t0, 364)).await;
        assert_eq!(update.states["parent"], MonitorState::Idle);
        assert_eq!(update.states["sub"], MonitorState::Idle);
    }

    /// The replay contract, driven through the real function rather than a copy of it.
    ///
    /// The previous version of this test re-implemented the transition rules in a local closure
    /// and asserted against its own copy, which proved nothing about the code that ships: the
    /// mutation it names (hoisting the `blinded.remove` above the lookup) would have destroyed
    /// every replay with the whole suite green.
    #[test]
    fn a_stop_during_an_outage_surfaces_when_the_judge_returns() {
        let mut blinded = HashMap::new();

        // working → unknown: silent, and the baseline is taken.
        assert_eq!(
            replay_baseline(
                &mut blinded,
                "c",
                Some(MonitorState::Working),
                MonitorState::Unknown
            ),
            Some(MonitorState::Working)
        );
        assert!(!should_flag_unread(
            MonitorState::Unknown,
            false,
            None,
            None
        ));

        // ...however long the outage lasts.
        for _ in 0..5 {
            assert_eq!(
                replay_baseline(
                    &mut blinded,
                    "c",
                    Some(MonitorState::Unknown),
                    MonitorState::Unknown
                ),
                Some(MonitorState::Working),
                "the baseline must not erode"
            );
        }

        // unknown → idle on recovery: the stop replays, exactly once.
        assert_eq!(
            replay_baseline(
                &mut blinded,
                "c",
                Some(MonitorState::Unknown),
                MonitorState::Idle
            ),
            Some(MonitorState::Working)
        );
        assert!(should_flag_unread(MonitorState::Idle, false, None, None));
        assert!(
            blinded.is_empty(),
            "and the entry is spent, so it cannot fire twice"
        );

        // idle → unknown → idle replays nothing: recovery is not a burst of noise.
        assert_eq!(
            replay_baseline(
                &mut blinded,
                "c",
                Some(MonitorState::Idle),
                MonitorState::Unknown
            ),
            Some(MonitorState::Idle)
        );
        assert_eq!(
            replay_baseline(
                &mut blinded,
                "c",
                Some(MonitorState::Unknown),
                MonitorState::Idle
            ),
            Some(MonitorState::Idle)
        );

        // A clone whose FIRST ever reading is unknown has no baseline to invent.
        assert_eq!(
            replay_baseline(&mut blinded, "n", None, MonitorState::Unknown),
            None
        );
        assert!(blinded.is_empty());

        // A container that dies mid-outage is not swallowed either.
        assert_eq!(
            replay_baseline(
                &mut blinded,
                "c",
                Some(MonitorState::Working),
                MonitorState::Unknown
            ),
            Some(MonitorState::Working)
        );
        assert_eq!(
            replay_baseline(
                &mut blinded,
                "c",
                Some(MonitorState::Unknown),
                MonitorState::Offline
            ),
            Some(MonitorState::Working)
        );
    }

    /// The pair can desync, which is the whole hazard of a derived field: `Unknown` is stored
    /// as itself but serialized as `idle`, so a state.json written during an outage reloads as
    /// `Idle` + the flag set. The first healthy tick then proposes `Idle` — equal to the stored
    /// state — and a gate watching only `monitor_state` skips the write, pinning the fleet on
    /// "no reading" against a judge that is answering fine.
    #[test]
    fn a_reloaded_outage_flag_still_counts_as_a_change() {
        let host = |flag: bool| RmngClone {
            id: "c".to_string(),
            managed: true,
            monitor_state: Some(MonitorState::Idle),
            activity_unknown: flag,
            ..Default::default()
        };

        // The reload case: the state matches, the flag does not. Must still count as news.
        assert!(
            flag_is_stale(MonitorState::Idle, &host(true)),
            "the flag is stale"
        );
        // Steady state on a healthy rig: nothing to do.
        assert!(!flag_is_stale(MonitorState::Idle, &host(false)));
        // Entering an outage from a clean flag, and sitting in one with it already set.
        assert!(flag_is_stale(MonitorState::Unknown, &host(false)));
        assert!(!flag_is_stale(MonitorState::Unknown, &host(true)));
    }

    #[test]
    fn a_slide_into_unknown_raises_nothing() {
        // The one case where we do NOT know the agent stopped. Raising a badge here tells an
        // operator their fleet died at the exact moment we lost the ability to say.
        assert!(!should_flag_unread(
            MonitorState::Unknown,
            false,
            None,
            None
        ));
        assert!(!should_flag_unread(
            MonitorState::Unknown,
            false,
            Some(1),
            Some(2)
        ));
        // Everything else is unchanged.
        assert!(should_flag_unread(MonitorState::Idle, false, None, None));
        assert!(should_flag_unread(
            MonitorState::Offline,
            false,
            Some(9),
            Some(1)
        ));
    }

    /// Leaving `unknown` is the tick that spends the replay baseline and raises the badge, and
    /// it is also where a fresh verdict is most likely to flap. It gets the same hold `working`
    /// does — but held at `unknown`, not at `working`, which would be a new lie.
    #[test]
    fn leaving_unknown_is_held_but_a_death_is_not() {
        let mut pending = HashMap::new();
        let clones = [clone_row("c", None, Some(MonitorState::Unknown))];

        let t0 = Instant::now();
        let mut next = HashMap::from([("c".to_string(), MonitorState::Idle)]);
        debounce(&mut next, &clones, &mut pending, t0);
        assert_eq!(
            next["c"],
            MonitorState::Unknown,
            "held at what is actually up"
        );

        // And it must LET GO. A hold that never expires is the most damaging thing this arm
        // could do: the clone would sit on "no reading" forever after the judge came back.
        let mut next = HashMap::from([("c".to_string(), MonitorState::Idle)]);
        debounce(&mut next, &clones, &mut pending, t0 + DEBOUNCE);
        assert_eq!(
            next["c"],
            MonitorState::Idle,
            "the hold expires like any other"
        );

        // A container that died mid-outage is news immediately.
        let mut pending = HashMap::new();
        let mut next = HashMap::from([("c".to_string(), MonitorState::Offline)]);
        debounce(&mut next, &clones, &mut pending, Instant::now());
        assert_eq!(next["c"], MonitorState::Offline);
    }

    #[test]
    fn the_debounce_does_not_hold_a_slide_into_unknown() {
        // The debounce exists to stop a flapping working/idle verdict. `unknown` is latched by
        // the judge's own health, cannot flap, and is silent anyway — so it goes straight up
        // and an operator sees the fleet lose its reading at once.
        let mut pending = HashMap::new();
        let clones = [clone_row("c", None, Some(MonitorState::Working))];
        let mut next = HashMap::from([("c".to_string(), MonitorState::Unknown)]);
        debounce(&mut next, &clones, &mut pending, Instant::now());
        assert_eq!(next["c"], MonitorState::Unknown);
        assert!(pending.is_empty(), "nothing to hold back");
    }

    #[test]
    fn lxc_cpu_uses_elapsed_time_and_ct105_capacity() {
        let start = Instant::now();
        let mut previous = None;
        assert_eq!(cpu_pct(&mut previous, 1_000, start), None);

        let pct = cpu_pct(&mut previous, 32_001_000, start + Duration::from_secs(4)).unwrap();
        assert!((pct - 50.0).abs() < f64::EPSILON);

        assert_eq!(
            cpu_pct(&mut previous, 10, start + Duration::from_secs(8)),
            None
        );
    }

    #[test]
    fn clone_cpu_is_rated_against_the_same_capacity_as_the_ct_gauge() {
        // The bug this guards: Docker's stats divide by `system_cpu_usage`, which counts all 32
        // threads CT 105 can see, while its cgroup enforces only 16 cores — halving every
        // per-clone reading. One clone burning 8 of the 16 cores is 50%, on the same basis the
        // CT-wide gauge uses, so the rows and the total are directly comparable.
        let start = Instant::now();
        let mut previous = None;
        assert_eq!(
            cpu_pct(&mut previous, 0, start),
            None,
            "one sample cannot make a rate"
        );

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
        for at in [
            t0,
            t0 + Duration::from_secs(30),
            t0 + DEBOUNCE - Duration::from_millis(1),
        ] {
            assert_eq!(
                settle(
                    MonitorState::Idle,
                    Some(MonitorState::Working),
                    &mut pending,
                    at
                ),
                MonitorState::Working
            );
        }
        assert_eq!(
            settle(
                MonitorState::Idle,
                Some(MonitorState::Working),
                &mut pending,
                t0 + DEBOUNCE
            ),
            MonitorState::Idle
        );
        assert!(pending.is_empty(), "once through, it stops being tracked");
    }

    #[test]
    fn a_change_that_reverses_inside_the_window_is_never_shown() {
        // The whole point. 36% of changes reversed inside 30 seconds on the live fleet.
        let t0 = Instant::now();
        let mut pending = HashMap::new();
        assert_eq!(
            settle(
                MonitorState::Idle,
                Some(MonitorState::Working),
                &mut pending,
                t0
            ),
            MonitorState::Working
        );
        // It comes back before the minute is up, so the pending change is dropped...
        assert_eq!(
            settle(
                MonitorState::Working,
                Some(MonitorState::Working),
                &mut pending,
                t0 + Duration::from_secs(20)
            ),
            MonitorState::Working
        );
        assert!(pending.is_empty());
        // ...and a later idle starts its own fresh minute rather than inheriting the old clock.
        assert_eq!(
            settle(
                MonitorState::Idle,
                Some(MonitorState::Working),
                &mut pending,
                t0 + Duration::from_secs(30)
            ),
            MonitorState::Working
        );
        assert_eq!(
            settle(
                MonitorState::Idle,
                Some(MonitorState::Working),
                &mut pending,
                t0 + Duration::from_secs(89)
            ),
            MonitorState::Working
        );
        assert_eq!(
            settle(
                MonitorState::Idle,
                Some(MonitorState::Working),
                &mut pending,
                t0 + Duration::from_secs(90)
            ),
            MonitorState::Idle
        );
    }

    #[test]
    fn a_clone_that_starts_working_shows_it_at_once() {
        // One direction only. Waiting here would make every turn shorter than a minute
        // invisible, and starting work is news an operator wants immediately.
        let mut pending = HashMap::new();
        assert_eq!(
            settle(
                MonitorState::Working,
                Some(MonitorState::Idle),
                &mut pending,
                Instant::now()
            ),
            MonitorState::Working
        );
        assert!(pending.is_empty());
    }

    #[test]
    fn offline_is_never_held_in_either_direction() {
        // Container existence is not activity, and an operator watching a clone die should not
        // wait a minute to see it.
        let t0 = Instant::now();
        let mut pending = HashMap::new();
        assert_eq!(
            settle(
                MonitorState::Offline,
                Some(MonitorState::Working),
                &mut pending,
                t0
            ),
            MonitorState::Offline
        );
        assert_eq!(
            settle(
                MonitorState::Working,
                Some(MonitorState::Offline),
                &mut pending,
                t0
            ),
            MonitorState::Working
        );
        assert!(pending.is_empty());
    }

    #[test]
    fn a_clones_first_reading_is_shown_at_once() {
        // Nothing to be a change from, so nothing to hold.
        let mut pending = HashMap::new();
        assert_eq!(
            settle(MonitorState::Working, None, &mut pending, Instant::now()),
            MonitorState::Working
        );
        assert!(pending.is_empty());
    }

    #[test]
    fn a_clone_that_left_the_fleet_stops_being_tracked() {
        let t0 = Instant::now();
        let mut pending = HashMap::new();
        settle(
            MonitorState::Idle,
            Some(MonitorState::Working),
            &mut pending,
            t0,
        );
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
        assert_eq!(
            next["held"],
            MonitorState::Working,
            "its change is still young"
        );
        assert_eq!(
            next["free"],
            MonitorState::Working,
            "it proposed no change at all"
        );
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
        let clones = [
            clone_row("parent", None, None),
            clone_row("sub-a", Some("parent"), None),
        ];
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
        let clones = [
            clone_row("parent", None, None),
            clone_row("sub-a", Some("parent"), None),
        ];
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
        let clones = [
            clone_row("solo", None, None),
            clone_row("other", None, None),
        ];
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
        assert!(!should_flag_unread(
            MonitorState::Idle,
            true,
            None,
            Some(10)
        ));
        assert!(!should_flag_unread(
            MonitorState::Offline,
            true,
            Some(1),
            Some(10)
        ));
    }

    #[test]
    fn idle_is_suppressed_only_when_viewed_since_last_activity() {
        // Viewed at/after last token activity → operator has seen the output → gray dot, no nag.
        assert!(!should_flag_unread(
            MonitorState::Idle,
            false,
            Some(10),
            Some(10)
        ));
        assert!(!should_flag_unread(
            MonitorState::Idle,
            false,
            Some(11),
            Some(10)
        ));
        // Last looked before the clone's final activity → they haven't seen it → flag.
        assert!(should_flag_unread(
            MonitorState::Idle,
            false,
            Some(9),
            Some(10)
        ));
        // Never viewed, or no recorded activity to compare against → flag (current behavior).
        assert!(should_flag_unread(
            MonitorState::Idle,
            false,
            None,
            Some(10)
        ));
        assert!(should_flag_unread(
            MonitorState::Idle,
            false,
            Some(10),
            None
        ));
    }

    #[test]
    fn offline_transition_is_always_flagged_even_if_recently_viewed() {
        // A container that died is surfaced regardless of when it was last viewed.
        assert!(should_flag_unread(
            MonitorState::Offline,
            false,
            Some(99),
            Some(10)
        ));
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
        assert_eq!(
            pick_stat(None, MonitorState::Offline, Some(&stat(33.0))),
            None
        );
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
        let b: HashMap<String, ContainerStats> = (0..8)
            .rev()
            .map(|i| (format!("h{i}"), stat(i as f64)))
            .collect();
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
