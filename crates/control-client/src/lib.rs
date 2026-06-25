//! Thin HTTP + SSE client for the control-server port-2 API, typed against
//! [`wire`]. Used by integration tests (the operator CLI is replaced by port 4).
//!
//! Fleshed out alongside Phase 2; for now it exposes a minimal typed `/events`
//! reader and JSON helpers.

use anyhow::Result;
use wire::ControlState;

/// A connected control-server client.
#[derive(Clone)]
pub struct Client {
    base: String,
    http: reqwest::Client,
}

impl Client {
    pub fn new(base: impl Into<String>) -> Self {
        Self { base: base.into(), http: reqwest::Client::new() }
    }

    /// Fetch the current state by reading the first `/events` frame.
    pub async fn state_once(&self) -> Result<ControlState> {
        let body = self
            .http
            .get(format!("{}/events", self.base))
            .header("accept", "text/event-stream")
            .send()
            .await?
            .text()
            .await?;
        // First `data:` line is the initial full-state frame.
        let line = body
            .lines()
            .find_map(|l| l.strip_prefix("data:"))
            .ok_or_else(|| anyhow::anyhow!("no data frame"))?;
        Ok(serde_json::from_str(line.trim())?)
    }
}
