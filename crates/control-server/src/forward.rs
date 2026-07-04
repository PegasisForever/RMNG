//! Volatile port-forward runtime status. Mirrors [`crate::monitor::StatsBus`]: an
//! in-memory `host_id → (rule_id → ForwardRuntime)` map broadcast to `/events` as a
//! named `forwards` SSE event. Never persisted — config lives on `Host.forwards`.

use std::collections::HashMap;
use std::sync::RwLock;
use std::sync::atomic::{AtomicUsize, Ordering};

use tokio::sync::broadcast;
use wire::forward::{ForwardRuntime, ForwardState, ForwardStatusMsg};

pub struct ForwardBus {
    tx: broadcast::Sender<String>,
    inner: RwLock<HashMap<String, HashMap<String, ForwardRuntime>>>,
    /// Number of currently-connected viewers. Runtime status is a union of what the
    /// viewers report; it is cleared only when this returns to zero so one viewer
    /// disconnecting does not blank a rule another viewer is still serving.
    viewers: AtomicUsize,
}

impl ForwardBus {
    pub fn new() -> Self {
        let (tx, _) = broadcast::channel(16);
        Self { tx, inner: RwLock::new(HashMap::new()), viewers: AtomicUsize::new(0) }
    }

    /// Current snapshot (JSON `Record<hostId, ForwardRuntime[]>`) + a live receiver.
    pub fn subscribe(&self) -> (String, broadcast::Receiver<String>) {
        (self.snapshot_json(), self.tx.subscribe())
    }

    fn snapshot_json(&self) -> String {
        let inner = self.inner.read().unwrap();
        let by_host: HashMap<&String, Vec<&ForwardRuntime>> =
            inner.iter().map(|(h, m)| (h, m.values().collect())).collect();
        serde_json::to_string(&by_host).unwrap_or_else(|_| "{}".to_string())
    }

    fn broadcast(&self) {
        let _ = self.tx.send(self.snapshot_json());
    }

    /// Apply a viewer-reported status change (keeps the rule's `active_conns`).
    pub fn report(&self, msg: ForwardStatusMsg) {
        {
            let mut inner = self.inner.write().unwrap();
            let host = inner.entry(msg.host_id).or_default();
            let e = host.entry(msg.id.clone()).or_insert_with(|| ForwardRuntime {
                id: msg.id.clone(),
                state: ForwardState::Offline,
                error: None,
                active_conns: 0,
            });
            e.state = msg.state;
            e.error = msg.error;
        }
        self.broadcast();
    }

    pub fn conn_opened(&self, host_id: &str, id: &str) {
        self.bump(host_id, id, 1);
    }

    pub fn conn_closed(&self, host_id: &str, id: &str) {
        self.bump(host_id, id, -1);
    }

    fn bump(&self, host_id: &str, id: &str, delta: i64) {
        {
            let mut inner = self.inner.write().unwrap();
            let host = inner.entry(host_id.to_string()).or_default();
            let e = host.entry(id.to_string()).or_insert_with(|| ForwardRuntime {
                id: id.to_string(),
                state: ForwardState::Listening,
                error: None,
                active_conns: 0,
            });
            e.active_conns = (e.active_conns as i64 + delta).max(0) as u32;
        }
        self.broadcast();
    }

    /// Drop all status (viewer disconnected → every rule reverts to offline in the UI).
    pub fn clear(&self) {
        self.inner.write().unwrap().clear();
        self.broadcast();
    }

    /// A viewer connected: it will start reporting its forward listeners.
    pub fn viewer_joined(&self) {
        self.viewers.fetch_add(1, Ordering::SeqCst);
    }

    /// A viewer disconnected. Clear all runtime status only when the last one leaves
    /// (its listeners are gone, and no other viewer remains to keep the union alive).
    pub fn viewer_left(&self) {
        // Saturating decrement: never underflow if called spuriously.
        let prev = self
            .viewers
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |v| Some(v.saturating_sub(1)))
            .unwrap();
        if prev <= 1 {
            self.clear();
        }
    }
}

impl Default for ForwardBus {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn report_then_snapshot_reflects_state() {
        let bus = ForwardBus::new();
        let (seed, mut rx) = bus.subscribe();
        assert_eq!(seed, "{}");
        bus.report(ForwardStatusMsg {
            host_id: "h".into(),
            id: "f8080".into(),
            state: ForwardState::Listening,
            error: None,
        });
        let got = rx.try_recv().expect("broadcast on report");
        assert!(got.contains("\"h\""));
        assert!(got.contains("\"state\":\"listening\""));
        assert!(got.contains("\"activeConns\":0"));
    }

    #[test]
    fn conn_count_and_clear() {
        let bus = ForwardBus::new();
        let (_seed, mut rx) = bus.subscribe();
        bus.conn_opened("h", "f8080");
        assert!(rx.try_recv().unwrap().contains("\"activeConns\":1"));
        bus.conn_closed("h", "f8080");
        assert!(rx.try_recv().unwrap().contains("\"activeConns\":0"));
        bus.clear();
        assert_eq!(rx.try_recv().unwrap(), "{}");
    }

    #[test]
    fn viewer_refcount_clears_only_when_last_leaves() {
        let bus = ForwardBus::new();
        let (_seed, mut rx) = bus.subscribe();
        // Two viewers connect; one reports a listening rule.
        bus.viewer_joined();
        bus.viewer_joined();
        bus.report(ForwardStatusMsg {
            host_id: "h".into(),
            id: "f8080".into(),
            state: ForwardState::Listening,
            error: None,
        });
        let _ = rx.try_recv(); // drain the report broadcast

        // First viewer leaves — status must persist (another viewer remains).
        bus.viewer_left();
        assert!(bus.snapshot_json().contains("\"f8080\""), "status cleared too early");

        // Last viewer leaves — status is now cleared.
        bus.viewer_left();
        assert_eq!(bus.snapshot_json(), "{}", "status not cleared after last viewer left");

        // Underflow guard: an extra leave with no viewers must not panic or clear-loop.
        bus.viewer_left();
        assert_eq!(bus.snapshot_json(), "{}");
    }
}
