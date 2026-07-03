//! Volatile port-forward runtime status. Mirrors [`crate::monitor::StatsBus`]: an
//! in-memory `host_id → (rule_id → ForwardRuntime)` map broadcast to `/events` as a
//! named `forwards` SSE event. Never persisted — config lives on `Host.forwards`.

use std::collections::HashMap;
use std::sync::RwLock;

use tokio::sync::broadcast;
use wire::forward::{ForwardRuntime, ForwardState, ForwardStatusMsg};

pub struct ForwardBus {
    tx: broadcast::Sender<String>,
    inner: RwLock<HashMap<String, HashMap<String, ForwardRuntime>>>,
}

impl ForwardBus {
    pub fn new() -> Self {
        let (tx, _) = broadcast::channel(16);
        Self { tx, inner: RwLock::new(HashMap::new()) }
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
}
