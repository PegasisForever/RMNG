//! Waiting on an [`wire::Operation`] over the `/events` SSE stream.
//!
//! Why SSE and not polling: finished ops are PRUNED from state shortly after they
//! settle (8s after Done, 60s after Error) — a poll loop can miss the terminal
//! frame entirely. Every terminal transition is broadcast as a state frame before
//! the prune, so a subscriber always sees it… unless the broadcast channel lags
//! (slow consumer) and drops frames, which is what [`WaitOutcome::Vanished`] covers.

use anyhow::Result;
use control_client::Client;
use futures::StreamExt;
use wire::{ControlState, Operation, OperationStatus};

#[derive(Debug)]
pub enum WaitOutcome {
    Done(Operation),
    Failed(Operation),
    /// The op disappeared without us observing a terminal frame (or was already
    /// pruned before the first frame). Overwhelmingly the Done-prune corner —
    /// callers should treat it as success-with-a-warning.
    Vanished {
        ever_seen: bool,
    },
    TimedOut,
}

/// Pure per-frame state machine, driven by [`wait_for_op`] and unit-tested on
/// synthetic frames.
pub struct WaitMachine {
    op_id: String,
    seen: bool,
    last_progress: Option<(String, u32)>,
}

impl WaitMachine {
    pub fn new(op_id: impl Into<String>) -> Self {
        Self {
            op_id: op_id.into(),
            seen: false,
            last_progress: None,
        }
    }

    /// Feed one state frame; `Some(outcome)` ends the wait.
    pub fn observe(&mut self, st: &ControlState) -> Option<WaitOutcome> {
        match st.operations.iter().find(|o| o.id == self.op_id) {
            Some(op) => {
                self.seen = true;
                match op.status {
                    OperationStatus::Done => Some(WaitOutcome::Done(op.clone())),
                    OperationStatus::Error => Some(WaitOutcome::Failed(op.clone())),
                    OperationStatus::Running => None,
                }
            }
            None => Some(WaitOutcome::Vanished {
                ever_seen: self.seen,
            }),
        }
    }

    /// A progress line for the op in this frame, only when it changed (step or
    /// whole-percent) since the last one — keeps `--wait` output readable.
    pub fn progress(&mut self, st: &ControlState) -> Option<String> {
        let op = st.operations.iter().find(|o| o.id == self.op_id)?;
        let key = (op.step.clone(), op.pct as u32);
        if self.last_progress.as_ref() == Some(&key) {
            return None;
        }
        self.last_progress = Some(key);
        Some(format!(
            "[{}] {} {:>3.0}%  {}",
            op.id, op.step, op.pct, op.message
        ))
    }
}

/// Watch `/events` until the op settles or `timeout_secs` elapses. Progress lines go
/// to stderr (stdout stays clean for `--json`).
pub async fn wait_for_op(client: &Client, op_id: &str, timeout_secs: u64) -> Result<WaitOutcome> {
    let fut = async {
        let mut events = client.events().await?;
        let mut machine = WaitMachine::new(op_id);
        while let Some(frame) = events.next().await {
            let st = frame?;
            if let Some(line) = machine.progress(&st) {
                eprintln!("{line}");
            }
            if let Some(outcome) = machine.observe(&st) {
                return Ok(outcome);
            }
        }
        // Stream ended (server restart mid-op — e.g. `server update`): report as
        // vanished rather than erroring; the caller decides how loud to be.
        Ok(WaitOutcome::Vanished { ever_seen: true })
    };
    match tokio::time::timeout(std::time::Duration::from_secs(timeout_secs), fut).await {
        Ok(r) => r,
        Err(_) => Ok(WaitOutcome::TimedOut),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn op(id: &str, status: OperationStatus, step: &str, pct: f64) -> Operation {
        Operation {
            id: id.into(),
            kind: wire::OperationKind::Clone,
            target: "w1".into(),
            source: None,
            status,
            step: step.into(),
            pct,
            message: format!("{step}…"),
            log: vec![],
            started_at: 0,
            finished_at: None,
        }
    }

    fn frame(ops: Vec<Operation>) -> ControlState {
        ControlState {
            operations: ops,
            ..Default::default()
        }
    }

    #[test]
    fn wait_machine_outcomes() {
        // Running then done resolves with the operation.
        let mut m = WaitMachine::new("op_1");
        assert!(
            m.observe(&frame(vec![op(
                "op_1",
                OperationStatus::Running,
                "create",
                20.0
            )]))
            .is_none()
        );
        match m.observe(&frame(vec![op(
            "op_1",
            OperationStatus::Done,
            "done",
            100.0,
        )])) {
            Some(WaitOutcome::Done(o)) => assert_eq!(o.id, "op_1"),
            other => panic!("expected Done, got {other:?}"),
        }
        // An error resolves as failed, not done.
        let mut m = WaitMachine::new("op_1");
        match m.observe(&frame(vec![op(
            "op_1",
            OperationStatus::Error,
            "create",
            20.0,
        )])) {
            Some(WaitOutcome::Failed(o)) => assert_eq!(o.id, "op_1"),
            other => panic!("expected Failed, got {other:?}"),
        }
        // Vanishing after being seen counts as done (ops prune after landing).
        let mut m = WaitMachine::new("op_1");
        m.observe(&frame(vec![op(
            "op_1",
            OperationStatus::Running,
            "create",
            20.0,
        )]));
        match m.observe(&frame(vec![])) {
            Some(WaitOutcome::Vanished { ever_seen: true }) => {}
            other => panic!("expected Vanished(seen), got {other:?}"),
        }
        // Vanishing before ever being seen reports never-seen.
        let mut m = WaitMachine::new("op_1");
        match m.observe(&frame(vec![op(
            "other",
            OperationStatus::Running,
            "create",
            0.0,
        )])) {
            Some(WaitOutcome::Vanished { ever_seen: false }) => {}
            other => panic!("expected Vanished(never seen), got {other:?}"),
        }
    }

    #[test]
    fn progress_only_on_change() {
        let mut m = WaitMachine::new("op_1");
        let f1 = frame(vec![op("op_1", OperationStatus::Running, "create", 20.0)]);
        assert!(m.progress(&f1).is_some());
        assert!(
            m.progress(&f1).is_none(),
            "identical frame must not re-print"
        );
        let f2 = frame(vec![op("op_1", OperationStatus::Running, "inject", 35.0)]);
        assert!(m.progress(&f2).is_some());
    }
}
