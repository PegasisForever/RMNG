//! `wire` — the single source of truth for every type that crosses a process
//! boundary in rmng.
//!
//! - [`control`] — `ControlState` and friends, broadcast over `/events` (port 2)
//!   and persisted to `state.json`. JSON shape is **byte-compatible** with the
//!   current `control-server/app/lib/types.ts` so the React frontend is unchanged.
//! - [`config`] — `AppConfig` (+ a redacted view) edited via the Settings UI.
//! - [`socket`] — the clone-daemon ⇄ control-server unix-socket protocol.
//! - [`viewer`] — the native viewer ⇄ control-server protocol (port 1).
//! - [`mcp`] — desktop-tool DTOs + the `/api/hosts/:id/mcp` proxy request.
//! - [`exec`] — the `rmng exec` request/result (`/api/hosts/:id/exec`).
//! - [`net`] — the one IO helper: keepalive tuning both ends of port 1 apply.
//!
//! Control-plane + config types derive `ts-rs::TS` and export TypeScript bindings
//! (see the `export_bindings_*` tests ts-rs generates). Transport types
//! (socket/viewer/mcp) are serde-only.

pub mod avc444;
pub mod config;
pub mod control;
pub mod exec;
pub mod forward;
pub mod mcp;
pub mod net;
pub mod socket;
pub mod viewer;

pub use config::{
    AppConfig, AppConfigRedacted, ChromaMode, ClaudeConfig, CloneGroup, CodexConfig,
    ConfigPutResponse, DockerConfig, EnvCheckRow, EnvVar, ImageInfo, ListenConfig, Preset,
    PresetRedacted, SetupEnv, SshConfig,
};
pub use exec::{ExecRequest, ExecResult};
pub use mcp::McpCallRequest;
pub use control::{
    AgentReport, Chat, ChatMessage, ChatRole, ClaudeSpend, ClaudeUsage, ClaudeUsageWindow,
    CodexResetMark, ContainerStats, ControlState, Host, LayoutPreset, MonitorSpec, MonitorState,
    Operation, OperationKind, OperationStatus, PortForward, Provider, UpdateStatus,
};
