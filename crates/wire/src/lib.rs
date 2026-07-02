//! `wire` — the single source of truth for every type that crosses a process
//! boundary in rmng.
//!
//! - [`control`] — `ControlState` and friends, broadcast over `/events` (port 2)
//!   and persisted to `state.json`. JSON shape is **byte-compatible** with the
//!   current `control-server/app/lib/types.ts` so the React frontend is unchanged.
//! - [`config`] — `AppConfig` (+ a redacted view) edited via the Settings UI.
//! - [`socket`] — the clone-daemon ⇄ control-server unix-socket protocol.
//! - [`viewer`] — the native viewer ⇄ control-server protocol (port 1).
//! - [`mcp`] — desktop-tool DTOs shared by the per-clone (port 3) and global
//!   (port 4) MCP servers.
//! - [`net`] — the one IO helper: keepalive tuning both ends of port 1 apply.
//!
//! Control-plane + config types derive `ts-rs::TS` and export TypeScript bindings
//! (see the `export_bindings_*` tests ts-rs generates). Transport types
//! (socket/viewer/mcp) are serde-only.

pub mod avc444;
pub mod config;
pub mod control;
pub mod mcp;
pub mod net;
pub mod socket;
pub mod viewer;

pub use config::{
    AppConfig, AppConfigRedacted, ChromaMode, ClaudeConfig, CloneAccount, CloneGroup, EnvPreset,
    EnvVar, LinearConfig, LinearKey, ListenConfig, ProxmoxConfig,
};
pub use control::{
    AgentReport, Chat, ChatMessage, ChatRole, ClaudeSpend, ClaudeUsage, ClaudeUsageWindow,
    ControlState, Host, MonitorSpec, MonitorState, Operation, OperationKind,
    OperationStatus, Provider,
};
