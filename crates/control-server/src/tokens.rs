//! Durable per-clone accounting for newly processed model tokens.
//!
//! The proxy observer records only aggregate client-facing usage fields. It never retains
//! request/response bodies, proxy credentials, account identity, or cache buckets. Live totals
//! ride a dedicated SSE bus rather than `ControlState`, because activity changes too often for
//! `state.json` persistence.
//!
//! **Process split.** The `/cc` proxy that feeds this now runs in the separate `rmng-cliproxy`
//! container (see [`crate::groupproxy`]), so the observer and the durable store no longer share
//! a process. Two processes must not both write `clone-tokens.json`, so ownership is split:
//!   - [`ResponseObserver`] (parsing + high-water accounting) runs wherever the proxy runs and
//!     talks to an abstract [`UsageSink`];
//!   - [`TokenBus`] — persistence, the SSE projection, and the `capture_epoch` lifecycle guard —
//!     stays owned solely by the control-server, which applies deltas arriving over the internal
//!     HTTP channel via [`LocalSink`].
//!
//! The epoch guard crosses the boundary as DATA, not as an RPC: the group-proxy reads the same
//! `clone-tokens.json` from the shared `/data` volume and takes each clone's active lifecycle
//! epoch from it ([`read_lifecycle_epochs`]). A delta carrying a stale epoch is rejected by
//! [`TokenBus::record`] exactly as an in-process stale observer was.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::{Notify, broadcast};
use wire::{CloneTokenUsage, RmngClone};

const TOKEN_FILE: &str = "clone-tokens.json";
const TOKEN_INACTIVE_MS: i64 = 5 * 60 * 1000;
/// How long after a clone's last observed use of the Fable model it still counts as
/// "recently used Fable" for the sidebar badge. Matches the token-inactivity window.
const FABLE_ACTIVE_MS: i64 = 5 * 60 * 1000;
/// Cadence for re-projecting `fable_active`: without token traffic the flag would otherwise
/// never flip back to false, so a lightweight ticker recomputes the browser view on this beat.
const FABLE_TICK: Duration = Duration::from_secs(30);
const MAX_CAPTURE_BYTES: usize = 256 * 1024;
const FLUSH_DELAY: Duration = Duration::from_millis(750);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct StoredUsage {
    new_input_tokens: u64,
    output_tokens: u64,
    request_count: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    last_token_at: Option<i64>,
    /// Wall-clock ms of this clone's most recent response served by the Fable model. Private
    /// like `last_token_at`; only its derived `fable_active` recency reaches the browser.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    last_fable_at: Option<i64>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Lifecycle {
    epoch: u64,
    last_activity_epoch: u64,
    active: bool,
}

impl Lifecycle {
    fn active(epoch: u64, has_activity: bool) -> Self {
        Self {
            epoch,
            last_activity_epoch: if has_activity { epoch } else { 0 },
            active: true,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct TokenFile {
    #[serde(default)]
    records: HashMap<String, StoredUsage>,
    /// Private lifecycle metadata makes a late stream from before archive/unarchive unable to
    /// revive current activity. It is intentionally absent from the browser DTO.
    #[serde(default)]
    lifecycle: HashMap<String, Lifecycle>,
}

struct Inner {
    file: TokenFile,
    data_dir: String,
    latest: HashMap<String, CloneTokenUsage>,
    latest_json: String,
    revision: u64,
    dirty: bool,
}

/// A durable token map plus an SSE-only view containing only safe accumulated totals.
pub struct TokenBus {
    tx: broadcast::Sender<String>,
    inner: Mutex<Inner>,
    persist_poke: Notify,
}

impl TokenBus {
    pub fn load(data_dir: &str) -> Self {
        let path = Self::state_path(data_dir);
        let file: TokenFile = std::fs::read(&path)
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default();
        let latest = public_map(&file.records, crate::clone_ops::now_ms());
        let latest_json = json_for(&latest);
        let (tx, _) = broadcast::channel(32);
        Self {
            tx,
            inner: Mutex::new(Inner {
                file,
                data_dir: data_dir.to_string(),
                latest,
                latest_json,
                revision: 0,
                dirty: false,
            }),
            persist_poke: Notify::new(),
        }
    }

    fn state_path(data_dir: &str) -> PathBuf {
        PathBuf::from(data_dir).join(TOKEN_FILE)
    }

    /// Initialize records for the persisted managed fleet and drop records for clones that no
    /// longer exist. Existing timestamps retain their real age across a server restart.
    pub fn sync_clones(&self, hosts: &[RmngClone]) {
        let managed: HashMap<&str, bool> = hosts
            .iter()
            .filter(|host| host.managed)
            .map(|host| (host.id.as_str(), !host.archived))
            .collect();
        let mut changed = false;
        let mut frame = None;
        {
            let mut inner = self.inner.lock().unwrap();
            let before = inner.file.records.len();
            inner
                .file
                .records
                .retain(|id, _| managed.contains_key(id.as_str()));
            inner
                .file
                .lifecycle
                .retain(|id, _| managed.contains_key(id.as_str()));
            changed |= before != inner.file.records.len();
            for (id, active) in managed {
                let id = id.to_string();
                let has_activity = inner
                    .file
                    .records
                    .get(&id)
                    .and_then(|record| record.last_token_at)
                    .is_some();
                if !inner.file.records.contains_key(&id) {
                    inner
                        .file
                        .records
                        .insert(id.clone(), StoredUsage::default());
                    changed = true;
                }
                if !inner.file.lifecycle.contains_key(&id) {
                    inner
                        .file
                        .lifecycle
                        .insert(id.clone(), Lifecycle::active(1, has_activity));
                    changed = true;
                }
                let lifecycle = inner
                    .file
                    .lifecycle
                    .get_mut(&id)
                    .expect("lifecycle inserted");
                if lifecycle.active != active {
                    lifecycle.epoch = lifecycle.epoch.saturating_add(1).max(1);
                    lifecycle.active = active;
                    changed = true;
                }
            }
            if changed {
                frame = mark_dirty_and_refresh(&mut inner, crate::clone_ops::now_ms());
            }
        }
        if changed {
            self.persist_poke.notify_one();
        }
        if let Some(frame) = frame {
            let _ = self.tx.send(frame);
        }
    }

    /// Create a fresh record only after a clone is present in durable clone state.
    pub fn register_clone(&self, host_id: &str) {
        let frame = {
            let mut inner = self.inner.lock().unwrap();
            let next_epoch = inner
                .file
                .lifecycle
                .get(host_id)
                .map(|l| l.epoch.saturating_add(1).max(1))
                .unwrap_or(1);
            inner
                .file
                .records
                .insert(host_id.to_string(), StoredUsage::default());
            inner
                .file
                .lifecycle
                .insert(host_id.to_string(), Lifecycle::active(next_epoch, false));
            mark_dirty_and_refresh(&mut inner, crate::clone_ops::now_ms())
        };
        self.persist_poke.notify_one();
        if let Some(frame) = frame {
            let _ = self.tx.send(frame);
        }
    }

    /// Retain totals while invalidating every in-flight response from before an archive or
    /// unarchive transition. Archived records are never eligible for activity updates.
    pub fn set_archived(&self, host_id: &str, archived: bool) {
        let mut changed = false;
        {
            let mut inner = self.inner.lock().unwrap();
            if let Some(lifecycle) = inner.file.lifecycle.get_mut(host_id) {
                let active = !archived;
                if lifecycle.active != active {
                    lifecycle.epoch = lifecycle.epoch.saturating_add(1).max(1);
                    lifecycle.active = active;
                    inner.revision = inner.revision.saturating_add(1);
                    inner.dirty = true;
                    changed = true;
                }
            }
        }
        if changed {
            self.persist_poke.notify_one();
        }
    }

    /// Purge totals after container deletion succeeds. Retain an inactive generation tombstone
    /// only for this process lifetime, so a late response from the deleted clone cannot match a
    /// newly registered clone that reuses the same id.
    pub fn forget_clone(&self, host_id: &str) {
        let mut frame = None;
        let mut changed = false;
        {
            let mut inner = self.inner.lock().unwrap();
            let removed_record = inner.file.records.remove(host_id).is_some();
            let lifecycle = inner.file.lifecycle.get(host_id).copied();
            if removed_record || lifecycle.is_some_and(|lifecycle| lifecycle.active) {
                let next_epoch = lifecycle
                    .map(|lifecycle| lifecycle.epoch.saturating_add(1).max(1))
                    .unwrap_or(1);
                inner.file.lifecycle.insert(
                    host_id.to_string(),
                    Lifecycle {
                        epoch: next_epoch,
                        last_activity_epoch: 0,
                        active: false,
                    },
                );
                changed = true;
            }
            if changed {
                frame = mark_dirty_and_refresh(&mut inner, crate::clone_ops::now_ms());
            }
        }
        if changed {
            self.persist_poke.notify_one();
        }
        if let Some(frame) = frame {
            let _ = self.tx.send(frame);
        }
    }

    /// The generation a response must carry to be allowed to update a clone. `None` means the
    /// clone has no active managed token record, so no observer should be constructed.
    ///
    /// The `/cc` proxy moved out of process, so its caller is now
    /// [`read_lifecycle_epochs`] reading the same fact off the shared `clone-tokens.json`.
    /// This stays as the canonical in-process definition that file-backed read must match, and
    /// as the fixture the boundary tests pin against.
    #[allow(dead_code)] // canonical epoch definition; exercised by the boundary tests
    pub fn capture_epoch(&self, host_id: &str) -> Option<u64> {
        let inner = self.inner.lock().unwrap();
        let lifecycle = inner.file.lifecycle.get(host_id)?;
        (lifecycle.active && inner.file.records.contains_key(host_id)).then_some(lifecycle.epoch)
    }

    pub fn subscribe(&self) -> (String, broadcast::Receiver<String>) {
        let inner = self.inner.lock().unwrap();
        (inner.latest_json.clone(), self.tx.subscribe())
    }

    /// Wall-clock ms of this clone's most recent token/output activity, if any. Server-private
    /// like the timestamp itself (never leaked to the browser); the monitor uses it to decide
    /// whether an idle transition is still news to the operator (see `monitor::should_flag_unread`).
    pub fn last_token_at(&self, host_id: &str) -> Option<i64> {
        let inner = self.inner.lock().unwrap();
        inner.file.records.get(host_id).and_then(|record| record.last_token_at)
    }

    /// Server-owned stuckness. It has no state mutation and is intentionally not sent to the
    /// browser. A timestamp before/after an invalid lifecycle is not activity for this epoch.
    pub fn is_token_inactive(&self, host_id: &str, now_ms: i64) -> bool {
        let inner = self.inner.lock().unwrap();
        let Some(lifecycle) = inner.file.lifecycle.get(host_id) else {
            return true;
        };
        let Some(record) = inner.file.records.get(host_id) else {
            return true;
        };
        let Some(last) = record.last_token_at else {
            return true;
        };
        !lifecycle.active
            || lifecycle.last_activity_epoch != lifecycle.epoch
            || last > now_ms
            || now_ms.saturating_sub(last) >= TOKEN_INACTIVE_MS
    }

    /// Called by the response observer. A stale epoch is ignored without affecting totals,
    /// timestamp, persistence, or SSE output.
    fn record(&self, host_id: &str, epoch: u64, delta: UsageDelta, output_activity: bool) -> bool {
        let now = crate::clone_ops::now_ms();
        let activity = delta.input > 0 || delta.output > 0 || output_activity;
        let mut changed = false;
        let mut frame = None;
        {
            let mut inner = self.inner.lock().unwrap();
            let valid_epoch = inner
                .file
                .lifecycle
                .get(host_id)
                .is_some_and(|lifecycle| lifecycle.active && lifecycle.epoch == epoch);
            if !valid_epoch {
                return false;
            }
            let Some(record) = inner.file.records.get_mut(host_id) else {
                return false;
            };
            if delta.input > 0 {
                record.new_input_tokens = record.new_input_tokens.saturating_add(delta.input);
                changed = true;
            }
            if delta.output > 0 {
                record.output_tokens = record.output_tokens.saturating_add(delta.output);
                changed = true;
            }
            if delta.count_request {
                record.request_count = record.request_count.saturating_add(1);
                changed = true;
            }
            if delta.input > 0 || delta.output > 0 || output_activity {
                if record.last_token_at != Some(now) {
                    record.last_token_at = Some(now);
                    changed = true;
                }
            }
            if activity {
                if let Some(lifecycle) = inner.file.lifecycle.get_mut(host_id) {
                    lifecycle.last_activity_epoch = epoch;
                }
            }
            if changed {
                frame = mark_dirty_and_refresh(&mut inner, now);
            }
        }
        if changed {
            self.persist_poke.notify_one();
        }
        if let Some(frame) = frame {
            let _ = self.tx.send(frame);
        }
        activity
    }

    /// Record that this clone's most recent response was served by the Fable model, stamping
    /// the private `last_fable_at` so `fable_active` reads true for the next window. A stale or
    /// archived epoch is ignored, exactly like [`Self::record`].
    fn record_fable(&self, host_id: &str, epoch: u64) {
        let now = crate::clone_ops::now_ms();
        let frame = {
            let mut inner = self.inner.lock().unwrap();
            let valid_epoch = inner
                .file
                .lifecycle
                .get(host_id)
                .is_some_and(|lifecycle| lifecycle.active && lifecycle.epoch == epoch);
            if !valid_epoch {
                return;
            }
            let Some(record) = inner.file.records.get_mut(host_id) else {
                return;
            };
            if record.last_fable_at == Some(now) {
                return;
            }
            record.last_fable_at = Some(now);
            mark_dirty_and_refresh(&mut inner, now)
        };
        self.persist_poke.notify_one();
        if let Some(frame) = frame {
            let _ = self.tx.send(frame);
        }
    }

    /// Apply ONE delta that arrived from the out-of-process `/cc` proxy (the group-proxy
    /// container's `POST /internal/tokens`). Identical semantics to the in-process observer
    /// path: a stale/archived epoch is silently dropped by [`Self::record`], and a delta that
    /// counted as activity also promotes the clone to `Working`. Kept here rather than in
    /// `web.rs` so `record`/`record_fable`/`mark_working_if_current` stay private to this module.
    pub fn apply_remote_delta(
        self: &Arc<Self>,
        store: &Arc<crate::state::StateStore>,
        delta: &TokenDelta,
    ) {
        if delta.fable {
            self.record_fable(&delta.host_id, delta.epoch);
        }
        let activity = self.record(
            &delta.host_id,
            delta.epoch,
            UsageDelta {
                input: delta.input,
                output: delta.output,
                count_request: delta.count_request,
            },
            delta.output_activity,
        );
        if activity {
            self.mark_working_if_current(store, &delta.host_id, delta.epoch);
        }
    }

    /// Set the volatile lifecycle state only while the originating response epoch remains active.
    /// Holding the token lifecycle lock through the state mutation makes archive/unarchive
    /// invalidation and the detached observer update ordered with one another.
    fn mark_working_if_current(
        &self,
        store: &crate::state::StateStore,
        host_id: &str,
        epoch: u64,
    ) {
        let inner = self.inner.lock().unwrap();
        let valid_epoch = inner
            .file
            .lifecycle
            .get(host_id)
            .is_some_and(|lifecycle| lifecycle.active && lifecycle.epoch == epoch)
            && inner.file.records.contains_key(host_id);
        if !valid_epoch {
            return;
        }
        let current = store.get();
        let Some(host) = current.hosts.iter().find(|host| host.id == host_id) else {
            return;
        };
        if !host.managed
            || host.archived
            || host.monitor_state == Some(wire::MonitorState::Working)
        {
            return;
        }
        store.mutate(|state| {
            if let Some(host) = state.hosts.iter_mut().find(|host| host.id == host_id) {
                if host.managed && !host.archived {
                    host.monitor_state = Some(wire::MonitorState::Working);
                    host.unread = false;
                }
            }
        });
        drop(inner);
    }

    fn persist_if_dirty(&self) {
        let (path, file, revision) = {
            let inner = self.inner.lock().unwrap();
            if !inner.dirty {
                return;
            }
            (
                Self::state_path(&inner.data_dir),
                inner.file.clone(),
                inner.revision,
            )
        };
        let tmp = path.with_extension(format!("tmp.{}", std::process::id()));
        let result = (|| -> std::io::Result<()> {
            let body = serde_json::to_vec_pretty(&file).map_err(std::io::Error::other)?;
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(&tmp, body)?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600))?;
            }
            std::fs::rename(&tmp, &path)
        })();
        match result {
            Ok(()) => {
                let mut inner = self.inner.lock().unwrap();
                if inner.revision == revision {
                    inner.dirty = false;
                }
            }
            Err(error) => {
                tracing::error!(target: "tokens", "persist clone token totals: {error}");
                let _ = std::fs::remove_file(tmp);
                // Keep retrying dirty data after transient filesystem failures even if no later
                // request happens to generate another token update.
                self.persist_poke.notify_one();
            }
        }
    }

    /// Bounded-delay durability worker. The proxy path changes only memory and wakes this task;
    /// response forwarding never blocks on filesystem I/O.
    pub async fn run_persister(self: Arc<Self>) {
        loop {
            self.persist_poke.notified().await;
            tokio::time::sleep(FLUSH_DELAY).await;
            let bus = self.clone();
            if let Err(error) = tokio::task::spawn_blocking(move || bus.persist_if_dirty()).await {
                tracing::error!(target: "tokens", "token persistence worker failed: {error}");
            }
        }
    }

    /// Periodically re-project the browser view so `fable_active` decays back to false a bounded
    /// time after the last Fable response, even when no further token traffic arrives to trigger
    /// a refresh. Touches no persistence state; emits an SSE frame only when the view changed.
    pub async fn run_fable_ticker(self: Arc<Self>) {
        let mut interval = tokio::time::interval(FABLE_TICK);
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            interval.tick().await;
            let frame = {
                let mut inner = self.inner.lock().unwrap();
                refresh_public(&mut inner, crate::clone_ops::now_ms())
            };
            if let Some(frame) = frame {
                let _ = self.tx.send(frame);
            }
        }
    }
}

fn public_map(
    records: &HashMap<String, StoredUsage>,
    now: i64,
) -> HashMap<String, CloneTokenUsage> {
    records
        .iter()
        .map(|(id, record)| {
            (
                id.clone(),
                CloneTokenUsage {
                    new_input_tokens: record.new_input_tokens,
                    output_tokens: record.output_tokens,
                    request_count: record.request_count,
                    fable_active: fable_active(record.last_fable_at, now),
                },
            )
        })
        .collect()
}

/// Whether a clone's last Fable response falls inside the active window ending at `now`.
/// A future timestamp (clock skew) is treated as not active, mirroring `is_token_inactive`.
fn fable_active(last_fable_at: Option<i64>, now: i64) -> bool {
    last_fable_at.is_some_and(|last| last <= now && now.saturating_sub(last) < FABLE_ACTIVE_MS)
}

fn json_for(map: &HashMap<String, CloneTokenUsage>) -> String {
    serde_json::to_string(map).unwrap_or_else(|_| "{}".to_string())
}

/// Marks the file dirty and returns a fresh SSE frame only if the safe browser projection
/// changed. Timestamp-only activity is persisted but intentionally not leaked to the client.
fn mark_dirty_and_refresh(inner: &mut Inner, now: i64) -> Option<String> {
    inner.revision = inner.revision.saturating_add(1);
    inner.dirty = true;
    refresh_public(inner, now)
}

/// Re-project the safe browser view at `now` and return a fresh SSE frame only if it changed.
/// Unlike `mark_dirty_and_refresh` this touches no persistence state, so the periodic
/// `fable_active` recompute never rewrites `clone-tokens.json`.
fn refresh_public(inner: &mut Inner, now: i64) -> Option<String> {
    let next = public_map(&inner.file.records, now);
    if next == inner.latest {
        return None;
    }
    let json = json_for(&next);
    inner.latest = next;
    inner.latest_json = json.clone();
    Some(json)
}

#[derive(Clone, Copy, Default)]
struct UsageTotals {
    input: u64,
    output: u64,
}

#[derive(Clone, Copy, Default)]
struct UsageDelta {
    input: u64,
    output: u64,
    count_request: bool,
}

// --- the observer → bus boundary -------------------------------------------------------

/// One accounting increment for a clone, as emitted by a [`ResponseObserver`]. Additive by
/// construction (the observer already reduced provider high-water totals to deltas), which is
/// what lets the remote sink coalesce a burst of stream events into a single POST and lets a
/// retried POST be applied without special-casing.
///
/// Carries `epoch` — the clone's token lifecycle generation, captured before the upstream
/// request — so a delta produced by a response that started before an archive/unarchive is
/// rejected on arrival, exactly as an in-process stale observer was.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TokenDelta {
    pub host_id: String,
    pub epoch: u64,
    #[serde(default)]
    pub input: u64,
    #[serde(default)]
    pub output: u64,
    /// Count this as one completed request (set on the first usage object of a response).
    #[serde(default)]
    pub count_request: bool,
    /// A recognized output delta proving the stream is alive before any usage arrives.
    #[serde(default)]
    pub output_activity: bool,
    /// This response was served by the Fable model (stamps the sidebar's recency badge).
    #[serde(default)]
    pub fable: bool,
}

impl TokenDelta {
    fn for_clone(host_id: &str, epoch: u64) -> Self {
        Self { host_id: host_id.to_string(), epoch, ..Default::default() }
    }

    /// Fold `other` into `self`. Only valid for deltas of the same `(host_id, epoch)` — the
    /// coalescer keys its map on exactly that pair.
    pub fn merge(&mut self, other: &TokenDelta) {
        self.input = self.input.saturating_add(other.input);
        self.output = self.output.saturating_add(other.output);
        self.count_request |= other.count_request;
        self.output_activity |= other.output_activity;
        self.fable |= other.fable;
    }
}

/// Where a [`ResponseObserver`]'s increments go. In the control-server that is the local
/// [`TokenBus`] ([`LocalSink`]); in the group-proxy container it is a buffered HTTP forwarder
/// back to the control-server (`groupproxy::RemoteSink`).
pub trait UsageSink: Send + Sync {
    fn submit(&self, delta: TokenDelta);
}

/// Every ACTIVE clone's current token-lifecycle epoch, read straight from
/// `<data_dir>/clone-tokens.json`. This is how the epoch guard crosses the process boundary:
/// the group-proxy container has no [`TokenBus`], so instead of an RPC per request it reads the
/// same file the control-server persists (poll cadence in [`crate::groupproxy`]) and stamps each
/// delta with the epoch it saw. The control-server re-validates on arrival, so a stale read here
/// costs at most a dropped delta — never a misattributed one.
///
/// Archived clones and clones with no usage record are omitted, which is exactly
/// [`TokenBus::capture_epoch`]'s `None` — the proxy then attaches no observer at all.
pub fn read_lifecycle_epochs(data_dir: &str) -> HashMap<String, u64> {
    let Ok(bytes) = std::fs::read(TokenBus::state_path(data_dir)) else {
        return HashMap::new();
    };
    let Ok(file) = serde_json::from_slice::<TokenFile>(&bytes) else {
        return HashMap::new();
    };
    file.lifecycle
        .iter()
        .filter(|(id, lifecycle)| lifecycle.active && file.records.contains_key(*id))
        .map(|(id, lifecycle)| (id.clone(), lifecycle.epoch))
        .collect()
}

#[derive(Clone, Copy)]
enum ResponseKind {
    Anthropic,
    OpenAiChat,
    OpenAiResponses,
    Gemini,
    GeminiInteractions,
}

impl ResponseKind {
    fn for_path(path: &str) -> Option<Self> {
        let path = path.to_ascii_lowercase();
        if path.contains("count_tokens")
            || path.contains("count-tokens")
            || path.contains("tokenize")
        {
            return None;
        }
        if path.contains("/v1beta/interactions") {
            return Some(Self::GeminiInteractions);
        }
        if path.contains("/v1/messages") {
            return Some(Self::Anthropic);
        }
        if path.contains("/v1/chat/completions") || path.ends_with("/v1/completions") {
            return Some(Self::OpenAiChat);
        }
        if path.contains("/v1/responses") || path.contains("/backend-api/codex/responses") {
            return Some(Self::OpenAiResponses);
        }
        if path.contains("generatecontent") {
            return Some(Self::Gemini);
        }
        None
    }

    fn usage(self, value: &Value) -> Option<UsageTotals> {
        match self {
            Self::Anthropic => anthopic_usage(value),
            Self::OpenAiChat => openai_chat_usage(value),
            Self::OpenAiResponses => openai_responses_usage(value),
            Self::Gemini => gemini_usage(value),
            Self::GeminiInteractions => gemini_interactions_usage(value),
        }
    }
}

/// Passive, bounded response observer. `feed` is deliberately synchronous and swallow-only so
/// malformed accounting data can never influence what the proxy delivers to the clone.
///
/// Sink-agnostic since the process split: it emits [`TokenDelta`]s into a [`UsageSink`], which
/// is the local [`TokenBus`] in the control-server and a buffered HTTP forwarder in the
/// group-proxy container. All the parsing, high-water, and per-response latching below is
/// identical in both.
pub struct ResponseObserver {
    sink: Arc<dyn UsageSink>,
    host_id: String,
    epoch: u64,
    kind: ResponseKind,
    streaming: bool,
    disabled: bool,
    buffer: Vec<u8>,
    data_lines: Vec<Vec<u8>>,
    sse_data_len: usize,
    high_water: UsageTotals,
    counted_request: bool,
    /// Set once this response has been attributed to Fable, so a long stream stamps the clock
    /// a single time rather than on every event carrying the model id.
    fable_marked: bool,
}

impl ResponseObserver {
    /// Build an observer for a response, or `None` when the request path isn't one whose
    /// bodies carry usage (`/v1/messages`, `/v1/chat/completions`, …; token-count preflights
    /// are deliberately excluded). Shared by both entrypoints.
    pub fn new(
        sink: Arc<dyn UsageSink>,
        host_id: String,
        epoch: u64,
        request_path: &str,
        streaming: bool,
    ) -> Option<Self> {
        let kind = ResponseKind::for_path(request_path)?;
        Some(Self {
            sink,
            host_id,
            epoch,
            kind,
            streaming,
            disabled: false,
            buffer: Vec::new(),
            data_lines: Vec::new(),
            sse_data_len: 0,
            high_water: UsageTotals::default(),
            counted_request: false,
            fable_marked: false,
        })
    }

    pub fn feed(&mut self, chunk: &[u8]) {
        if self.disabled {
            return;
        }
        if self.streaming {
            self.feed_sse(chunk);
        } else {
            self.feed_json(chunk);
        }
    }

    fn feed_json(&mut self, chunk: &[u8]) {
        if self.buffer.len().saturating_add(chunk.len()) > MAX_CAPTURE_BYTES {
            self.disabled = true;
            self.buffer.clear();
            return;
        }
        self.buffer.extend_from_slice(chunk);
        if let Ok(value) = serde_json::from_slice::<Value>(&self.buffer) {
            self.note_model(&value);
            self.account_usage(&value);
            self.disabled = true;
            self.buffer.clear();
        }
    }

    fn feed_sse(&mut self, chunk: &[u8]) {
        if self.buffer.len().saturating_add(chunk.len()) > MAX_CAPTURE_BYTES {
            self.disabled = true;
            self.buffer.clear();
            self.data_lines.clear();
            self.sse_data_len = 0;
            return;
        }
        self.buffer.extend_from_slice(chunk);
        while let Some(newline) = self.buffer.iter().position(|byte| *byte == b'\n') {
            let mut line: Vec<u8> = self.buffer.drain(..=newline).collect();
            line.pop();
            if line.last() == Some(&b'\r') {
                line.pop();
            }
            if line.is_empty() {
                self.dispatch_sse_event();
                if self.disabled {
                    return;
                }
                continue;
            }
            if line.starts_with(b":") {
                continue;
            }
            if let Some(mut data) = line.strip_prefix(b"data:") {
                if data.first() == Some(&b' ') {
                    data = &data[1..];
                }
                let separator = usize::from(!self.data_lines.is_empty());
                if self
                    .sse_data_len
                    .saturating_add(separator)
                    .saturating_add(data.len())
                    > MAX_CAPTURE_BYTES
                {
                    self.disabled = true;
                    self.buffer.clear();
                    self.data_lines.clear();
                    self.sse_data_len = 0;
                    return;
                }
                self.sse_data_len = self
                    .sse_data_len
                    .saturating_add(separator)
                    .saturating_add(data.len());
                self.data_lines.push(data.to_vec());
            }
        }
    }

    fn dispatch_sse_event(&mut self) {
        if self.data_lines.is_empty() {
            return;
        }
        self.sse_data_len = 0;
        let mut data = Vec::new();
        for (index, line) in self.data_lines.drain(..).enumerate() {
            if index > 0 {
                data.push(b'\n');
            }
            data.extend_from_slice(&line);
        }
        if data == b"[DONE]" {
            return;
        }
        let Ok(value) = serde_json::from_slice::<Value>(&data) else {
            self.disabled = true;
            return;
        };
        self.note_model(&value);
        self.account_usage(&value);
        if !self.disabled && recognized_output_delta(&value) {
            let mut delta = TokenDelta::for_clone(&self.host_id, self.epoch);
            delta.output_activity = true;
            self.sink.submit(delta);
        }
    }

    fn account_usage(&mut self, value: &Value) {
        let Some(usage) = self.kind.usage(value) else {
            return;
        };
        let input = usage.input.saturating_sub(self.high_water.input);
        let output = usage.output.saturating_sub(self.high_water.output);
        self.high_water.input = self.high_water.input.max(usage.input);
        self.high_water.output = self.high_water.output.max(usage.output);
        let count_request = !self.counted_request;
        self.counted_request = true;
        let mut delta = TokenDelta::for_clone(&self.host_id, self.epoch);
        delta.input = input;
        delta.output = output;
        delta.count_request = count_request;
        self.sink.submit(delta);
    }

    /// Stamp Fable activity the first time this response reveals the Fable model. The model id
    /// appears in an early event (Anthropic `message_start`) or the non-streaming body, so this
    /// runs on every parsed value independent of whether that value also carries a usage object.
    fn note_model(&mut self, value: &Value) {
        if self.fable_marked || !mentions_fable(value) {
            return;
        }
        self.fable_marked = true;
        let mut delta = TokenDelta::for_clone(&self.host_id, self.epoch);
        delta.fable = true;
        self.sink.submit(delta);
    }
}

fn obj_at<'a>(value: &'a Value, paths: &[&[&str]]) -> Option<&'a serde_json::Map<String, Value>> {
    paths.iter().find_map(|path| {
        path.iter()
            .try_fold(value, |current, key| current.get(*key))
            .and_then(Value::as_object)
    })
}

fn number(object: &serde_json::Map<String, Value>, key: &str) -> u64 {
    object.get(key).and_then(Value::as_u64).unwrap_or(0)
}

fn nested_number(object: &serde_json::Map<String, Value>, section: &str, key: &str) -> u64 {
    object
        .get(section)
        .and_then(Value::as_object)
        .and_then(|value| value.get(key))
        .and_then(Value::as_u64)
        .unwrap_or(0)
}

fn first_number<'a>(
    object: &serde_json::Map<String, Value>,
    keys: &'a [&'a str],
) -> Option<(&'a str, u64)> {
    keys.iter().find_map(|key| {
        object
            .get(*key)
            .and_then(Value::as_u64)
            .map(|value| (*key, value))
    })
}

fn anthopic_usage(value: &Value) -> Option<UsageTotals> {
    let usage = obj_at(value, &[&["message", "usage"], &["usage"]])?;
    Some(UsageTotals {
        input: number(usage, "input_tokens")
            .saturating_add(number(usage, "cache_creation_input_tokens")),
        output: number(usage, "output_tokens"),
    })
}

fn openai_chat_usage(value: &Value) -> Option<UsageTotals> {
    let usage = obj_at(value, &[&["usage"]])?;
    let input = number(usage, "prompt_tokens").saturating_sub(nested_number(
        usage,
        "prompt_tokens_details",
        "cached_tokens",
    ));
    Some(UsageTotals {
        input,
        output: number(usage, "completion_tokens"),
    })
}

fn openai_responses_usage(value: &Value) -> Option<UsageTotals> {
    let usage = obj_at(value, &[&["usage"], &["response", "usage"]])?;
    let input = number(usage, "input_tokens").saturating_sub(nested_number(
        usage,
        "input_tokens_details",
        "cached_tokens",
    ));
    Some(UsageTotals {
        input,
        output: number(usage, "output_tokens"),
    })
}

fn gemini_usage(value: &Value) -> Option<UsageTotals> {
    let usage = obj_at(
        value,
        &[
            &["usageMetadata"],
            &["usage_metadata"],
            &["response", "usageMetadata"],
        ],
    )?;
    Some(UsageTotals {
        input: number(usage, "promptTokenCount")
            .saturating_sub(number(usage, "cachedContentTokenCount")),
        output: number(usage, "candidatesTokenCount")
            .saturating_add(number(usage, "thoughtsTokenCount")),
    })
}

fn gemini_interactions_usage(value: &Value) -> Option<UsageTotals> {
    let usage = obj_at(
        value,
        &[
            &["usage"],
            &["total_usage"],
            &["metadata", "total_usage"],
            &["usageMetadata"],
            &["interaction", "usage"],
        ],
    )?;
    let input = first_number(
        usage,
        &["input_tokens", "prompt_tokens", "total_input_tokens"],
    )
    .map(|(_, value)| value)
    .unwrap_or(0);
    let cached = first_number(
        usage,
        &[
            "cached_tokens",
            "cachedContentTokenCount",
            "total_cached_tokens",
            "cache_read_tokens",
            "cacheReadTokens",
        ],
    )
    .map(|(_, value)| value)
    .unwrap_or(0);
    let (output_name, output) = first_number(
        usage,
        &["output_tokens", "completion_tokens", "total_output_tokens"],
    )
    .unwrap_or(("", 0));
    let reasoning = first_number(
        usage,
        &[
            "reasoning_tokens",
            "thoughtsTokenCount",
            "total_thought_tokens",
        ],
    )
    .map(|(_, value)| value)
    .unwrap_or(0);
    // `total_output_tokens` and any explicit output detail are totals. Other interaction
    // aliases report visible output separately, so a distinct reasoning bucket is new work.
    let output_includes_reasoning = output_name == "total_output_tokens"
        || usage.get("output_tokens_details").is_some()
        || usage.get("completion_tokens_details").is_some();
    Some(UsageTotals {
        input: input.saturating_sub(cached),
        output: output.saturating_add(if output_includes_reasoning {
            0
        } else {
            reasoning
        }),
    })
}

/// Whether a response value names the Fable model. The model id is echoed by every provider
/// shape in one of these locations (Anthropic `message.model` / top-level `model`, OpenAI
/// `model`, Gemini `modelVersion`); a substring match on "fable" catches `claude-fable-5` and
/// any dated variant without hard-coding an exact id.
fn mentions_fable(value: &Value) -> bool {
    const MODEL_PATHS: &[&[&str]] = &[
        &["model"],
        &["message", "model"],
        &["response", "model"],
        &["modelVersion"],
        &["response", "modelVersion"],
    ];
    MODEL_PATHS.iter().any(|path| {
        path.iter()
            .try_fold(value, |current, key| current.get(*key))
            .and_then(Value::as_str)
            .is_some_and(|model| model.to_ascii_lowercase().contains("fable"))
    })
}

/// Recognize model-output deltas that prove a stream remains alive before a final `usage`
/// snapshot arrives. This deliberately accepts only text, reasoning, and tool-argument payloads.
fn recognized_output_delta(value: &Value) -> bool {
    const DELTA_FIELDS: &[&str] = &[
        "text",
        "content",
        "thinking",
        "reasoning",
        "reasoning_content",
        "partial_json",
        "arguments",
        "input_json_delta",
    ];
    match value {
        Value::Object(object) => {
            if gemini_candidate_has_output(object) {
                return true;
            }
            let is_delta = object
                .get("type")
                .and_then(Value::as_str)
                .is_some_and(|kind| kind.ends_with("_delta") || kind.ends_with(".delta"));
            if is_delta
                && DELTA_FIELDS.iter().any(|field| {
                    object
                        .get(*field)
                        .and_then(Value::as_str)
                        .is_some_and(|text| !text.is_empty())
                })
            {
                return true;
            }
            if let Some(delta) = object.get("delta").and_then(Value::as_object) {
                if DELTA_FIELDS.iter().any(|field| {
                    delta
                        .get(*field)
                        .and_then(Value::as_str)
                        .is_some_and(|text| !text.is_empty())
                }) {
                    return true;
                }
            }
            object.values().any(recognized_output_delta)
        }
        Value::Array(values) => values.iter().any(recognized_output_delta),
        _ => false,
    }
}

fn gemini_candidate_has_output(object: &serde_json::Map<String, Value>) -> bool {
    object
        .get("candidates")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|candidate| candidate.get("content"))
        .filter_map(|content| content.get("parts"))
        .filter_map(Value::as_array)
        .flatten()
        .any(|part| {
            part.get("text")
                .and_then(Value::as_str)
                .is_some_and(|text| !text.is_empty())
                || part
                    .get("thought")
                    .and_then(Value::as_str)
                    .is_some_and(|thought| !thought.is_empty())
                || ["functionCall", "function_call", "toolCall", "tool_call"]
                    .iter()
                    .filter_map(|field| part.get(*field))
                    .filter_map(|call| call.get("args"))
                    .any(nonempty_json)
        })
}

fn nonempty_json(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::String(value) => !value.is_empty(),
        Value::Array(value) => !value.is_empty(),
        Value::Object(value) => !value.is_empty(),
        Value::Bool(_) | Value::Number(_) => true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(s: &str) -> Value {
        serde_json::from_str(s).unwrap()
    }

    #[test]
    fn anthropic_counts_cache_creation_but_not_cache_reads() {
        let usage = anthopic_usage(&parse(
            r#"{"usage":{"input_tokens":10,"cache_creation_input_tokens":20,"cache_read_input_tokens":99,"output_tokens":4}}"#,
        ))
        .unwrap();
        assert_eq!((usage.input, usage.output), (30, 4));
    }

    #[test]
    fn openai_and_gemini_subtract_cached_input_once() {
        let openai = openai_chat_usage(&parse(
            r#"{"usage":{"prompt_tokens":100,"completion_tokens":11,"prompt_tokens_details":{"cached_tokens":80}}}"#,
        ))
        .unwrap();
        assert_eq!((openai.input, openai.output), (20, 11));
        let gemini = gemini_usage(&parse(
            r#"{"usageMetadata":{"promptTokenCount":100,"cachedContentTokenCount":80,"candidatesTokenCount":11,"thoughtsTokenCount":4}}"#,
        ))
        .unwrap();
        assert_eq!((gemini.input, gemini.output), (20, 15));
    }

    #[test]
    fn response_routes_exclude_count_preflights() {
        assert!(ResponseKind::for_path("/v1/messages/count_tokens").is_none());
        assert!(ResponseKind::for_path("/v1/chat/completions").is_some());
        assert!(ResponseKind::for_path("/v1beta/interactions").is_some());
    }

    #[test]
    fn gemini_stream_parts_refresh_activity() {
        assert!(recognized_output_delta(&parse(
            r#"{"candidates":[{"content":{"parts":[{"text":"hello"},{"functionCall":{"args":{"path":"src/lib.rs"}}}]}}]}"#,
        )));
    }

    /// A sink that just records everything submitted, so observer behavior can be asserted
    /// without a TokenBus/StateStore behind it.
    #[derive(Default)]
    struct CapturingSink(Mutex<Vec<TokenDelta>>);

    impl UsageSink for CapturingSink {
        fn submit(&self, delta: TokenDelta) {
            self.0.lock().unwrap().push(delta);
        }
    }

    #[test]
    fn high_water_usage_never_double_counts() {
        let sink = Arc::new(CapturingSink::default());
        let mut observer = ResponseObserver::new(
            sink.clone(),
            "missing".into(),
            1,
            "/v1/messages",
            true,
        )
        .expect("/v1/messages is an accountable route");
        observer.account_usage(&parse(r#"{"usage":{"input_tokens":9,"output_tokens":2}}"#));
        observer.account_usage(&parse(r#"{"usage":{"input_tokens":9,"output_tokens":5}}"#));
        assert_eq!(
            (observer.high_water.input, observer.high_water.output),
            (9, 5)
        );
        // The emitted deltas are the *increments*, not the provider's cumulative totals: the
        // second usage object contributes only the 3 new output tokens.
        let deltas = sink.0.lock().unwrap();
        assert_eq!(deltas.len(), 2);
        assert_eq!((deltas[0].input, deltas[0].output), (9, 2));
        assert_eq!((deltas[1].input, deltas[1].output), (0, 3));
        assert!(deltas[0].count_request, "the first usage object counts the request");
        assert!(!deltas[1].count_request, "…and only the first");
    }

    /// Deltas are additive, so a coalescing sink can fold a burst into one payload without
    /// changing the totals the control-server applies.
    #[test]
    fn token_deltas_merge_additively() {
        let mut a = TokenDelta { host_id: "h".into(), epoch: 3, input: 5, output: 1, ..Default::default() };
        a.merge(&TokenDelta {
            host_id: "h".into(),
            epoch: 3,
            input: 2,
            output: 4,
            count_request: true,
            output_activity: true,
            fable: true,
        });
        assert_eq!((a.input, a.output), (7, 5));
        assert!(a.count_request && a.output_activity && a.fable, "flags are sticky under merge");
        assert_eq!((a.host_id.as_str(), a.epoch), ("h", 3));
    }

    /// The epoch guard crossing the process boundary: the group-proxy reads active lifecycle
    /// epochs straight out of `clone-tokens.json`. Archived clones must be absent (an absent
    /// entry is what makes the proxy attach no observer at all).
    #[test]
    fn lifecycle_epochs_read_back_active_clones_only() {
        let dir = std::env::temp_dir().join(format!("rmng-token-epochs-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let data_dir = dir.to_string_lossy().into_owned();

        let bus = Arc::new(TokenBus::load(&data_dir));
        bus.register_clone("live");
        bus.register_clone("gone");
        bus.set_archived("gone", true);
        bus.persist_if_dirty();

        let epochs = read_lifecycle_epochs(&data_dir);
        assert_eq!(epochs.get("live"), bus.capture_epoch("live").as_ref());
        assert!(!epochs.contains_key("gone"), "archived clones carry no usable epoch");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn inactivity_boundary_is_exact_and_future_timestamps_are_inactive() {
        let bus = TokenBus::load(
            &std::env::temp_dir()
                .join("rmng-token-boundary")
                .to_string_lossy(),
        );
        bus.register_clone("h");
        {
            let mut inner = bus.inner.lock().unwrap();
            inner.file.records.get_mut("h").unwrap().last_token_at = Some(1_000);
            inner
                .file
                .lifecycle
                .get_mut("h")
                .unwrap()
                .last_activity_epoch = 1;
        }
        assert!(!bus.is_token_inactive("h", 300_999));
        assert!(bus.is_token_inactive("h", 301_000));
        assert!(bus.is_token_inactive("h", 999));
    }

    #[test]
    fn detects_fable_across_provider_shapes() {
        assert!(mentions_fable(&parse(
            r#"{"type":"message_start","message":{"model":"claude-fable-5"}}"#
        )));
        assert!(mentions_fable(&parse(r#"{"model":"claude-fable-5-20251001"}"#)));
        assert!(mentions_fable(&parse(r#"{"response":{"model":"CLAUDE-FABLE-5"}}"#)));
        assert!(!mentions_fable(&parse(
            r#"{"message":{"model":"claude-opus-4-8"}}"#
        )));
        assert!(!mentions_fable(&parse(r#"{"model":"gpt-5.5"}"#)));
        assert!(!mentions_fable(&parse(r#"{"usage":{"output_tokens":3}}"#)));
    }

    #[test]
    fn fable_active_window_is_exclusive_and_rejects_future() {
        assert!(fable_active(Some(1_000), 1_000));
        assert!(fable_active(Some(1_000), 1_000 + FABLE_ACTIVE_MS - 1));
        assert!(!fable_active(Some(1_000), 1_000 + FABLE_ACTIVE_MS));
        assert!(!fable_active(Some(2_000), 1_000));
        assert!(!fable_active(None, 5_000));
    }
}
