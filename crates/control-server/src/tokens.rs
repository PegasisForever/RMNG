//! Durable per-clone accounting for newly processed model tokens.
//!
//! The proxy observer records only aggregate client-facing usage fields. It never retains
//! request/response bodies, proxy credentials, account identity, or cache buckets. Live totals
//! ride a dedicated SSE bus rather than `ControlState`, because activity changes too often for
//! `state.json` persistence.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::{Notify, broadcast};
use wire::{CloneTokenUsage, Host};

const TOKEN_FILE: &str = "clone-tokens.json";
const TOKEN_INACTIVE_MS: i64 = 5 * 60 * 1000;
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
        let latest = public_map(&file.records);
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

    /// Initialize records for the persisted managed fleet and drop records for hosts that no
    /// longer exist. Existing timestamps retain their real age across a server restart.
    pub fn sync_hosts(&self, hosts: &[Host]) {
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
                frame = mark_dirty_and_refresh(&mut inner);
            }
        }
        if changed {
            self.persist_poke.notify_one();
        }
        if let Some(frame) = frame {
            let _ = self.tx.send(frame);
        }
    }

    /// Create a fresh record only after a clone is present in durable host state.
    pub fn register_host(&self, host_id: &str) {
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
            mark_dirty_and_refresh(&mut inner)
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
    pub fn forget_host(&self, host_id: &str) {
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
                frame = mark_dirty_and_refresh(&mut inner);
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
    /// host has no active managed token record, so no observer should be constructed.
    pub fn capture_epoch(&self, host_id: &str) -> Option<u64> {
        let inner = self.inner.lock().unwrap();
        let lifecycle = inner.file.lifecycle.get(host_id)?;
        (lifecycle.active && inner.file.records.contains_key(host_id)).then_some(lifecycle.epoch)
    }

    pub fn subscribe(&self) -> (String, broadcast::Receiver<String>) {
        let inner = self.inner.lock().unwrap();
        (inner.latest_json.clone(), self.tx.subscribe())
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
                frame = mark_dirty_and_refresh(&mut inner);
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

    pub fn observer(
        self: &Arc<Self>,
        store: Arc<crate::state::StateStore>,
        host_id: String,
        epoch: u64,
        request_path: &str,
        streaming: bool,
    ) -> Option<ResponseObserver> {
        let kind = ResponseKind::for_path(request_path)?;
        Some(ResponseObserver {
            bus: self.clone(),
            store,
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
            working_marked: false,
        })
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
}

fn public_map(records: &HashMap<String, StoredUsage>) -> HashMap<String, CloneTokenUsage> {
    records
        .iter()
        .map(|(id, record)| {
            (
                id.clone(),
                CloneTokenUsage {
                    new_input_tokens: record.new_input_tokens,
                    output_tokens: record.output_tokens,
                    request_count: record.request_count,
                },
            )
        })
        .collect()
}

fn json_for(map: &HashMap<String, CloneTokenUsage>) -> String {
    serde_json::to_string(map).unwrap_or_else(|_| "{}".to_string())
}

/// Marks the file dirty and returns a fresh SSE frame only if the safe browser projection
/// changed. Timestamp-only activity is persisted but intentionally not leaked to the client.
fn mark_dirty_and_refresh(inner: &mut Inner) -> Option<String> {
    inner.revision = inner.revision.saturating_add(1);
    inner.dirty = true;
    let next = public_map(&inner.file.records);
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
pub struct ResponseObserver {
    bus: Arc<TokenBus>,
    store: Arc<crate::state::StateStore>,
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
    working_marked: bool,
}

impl ResponseObserver {
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
        self.account_usage(&value);
        if !self.disabled
            && recognized_output_delta(&value)
            && self
                .bus
                .record(&self.host_id, self.epoch, UsageDelta::default(), true)
        {
            self.mark_working_once();
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
        if self.bus.record(
            &self.host_id,
            self.epoch,
            UsageDelta {
                input,
                output,
                count_request,
            },
            false,
        ) {
            self.mark_working_once();
        }
    }

    fn mark_working_once(&mut self) {
        if self.working_marked {
            return;
        }
        self.working_marked = true;
        let bus = self.bus.clone();
        let store = self.store.clone();
        let host_id = self.host_id.clone();
        let epoch = self.epoch;
        // State persistence is intentionally detached from the transparent proxy stream. Token
        // bytes reach the clone without waiting for a `state.json` write.
        tokio::spawn(async move {
            bus.mark_working_if_current(&store, &host_id, epoch);
        });
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

    #[test]
    fn high_water_usage_never_double_counts() {
        let root = std::env::temp_dir().join("rmng-token-test");
        let mut observer = ResponseObserver {
            bus: Arc::new(TokenBus::load(&root.to_string_lossy())),
            store: Arc::new(
                crate::state::StateStore::load(root.join("state.json"))
                    .expect("test state store"),
            ),
            host_id: "missing".into(),
            epoch: 1,
            kind: ResponseKind::Anthropic,
            streaming: true,
            disabled: false,
            buffer: Vec::new(),
            data_lines: Vec::new(),
            sse_data_len: 0,
            high_water: UsageTotals::default(),
            counted_request: false,
            working_marked: false,
        };
        observer.account_usage(&parse(r#"{"usage":{"input_tokens":9,"output_tokens":2}}"#));
        observer.account_usage(&parse(r#"{"usage":{"input_tokens":9,"output_tokens":5}}"#));
        assert_eq!(
            (observer.high_water.input, observer.high_water.output),
            (9, 5)
        );
    }

    #[test]
    fn inactivity_boundary_is_exact_and_future_timestamps_are_inactive() {
        let bus = TokenBus::load(
            &std::env::temp_dir()
                .join("rmng-token-boundary")
                .to_string_lossy(),
        );
        bus.register_host("h");
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
}
