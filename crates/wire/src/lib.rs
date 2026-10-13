//! `wire` — the single source of truth for every type that crosses a process
//! boundary in rmng.
//!
//! - [`control`] — `ControlState` and friends, broadcast over `/events` (port 9000)
//!   and persisted to `state.json`. Its TypeScript bindings are generated into
//!   `frontend/app/lib/wire/`.
//! - [`config`] — `AppConfig` (+ a redacted view) edited via the Settings UI.
//! - [`socket`] — the clone-daemon ⇄ control-server unix-socket protocol.
//! - [`holder`] — the clone-daemon ⇄ session-holder protocol, inside one clone.
//! - [`browser`] — the browser viewer ⇄ control-server protocol (a WebSocket on the web port).
//! - [`mcp`] — desktop-tool DTOs + the `/api/hosts/:id/mcp` proxy request.
//! - [`exec`] — the `rmng exec` request/result (`/api/hosts/:id/exec`).
//! - [`ledger`] — the distilled transcript record and the two `/api/ledger/*` answers.
//!
//! Control-plane + config types derive `ts-rs::TS` and export TypeScript bindings
//! (see the `export_bindings_*` tests ts-rs generates). Transport types
//! (socket/mcp) are serde-only, except the ones the browser viewer's [`browser`]
//! messages carry.

pub mod board;
pub mod browser;
pub mod config;
pub mod control;
pub mod exec;
pub mod holder;
pub mod ledger;
pub mod mcp;
pub mod socket;

pub use config::{
    AppConfig, AppConfigRedacted, AssistantConfig, BUILDKIT_CACHE_GB,
    BUILDKIT_IMAGE, CLAUDE_POLL_SECS, CLONE_SOCKET, CODEX_POLL_SECS,
    ClaudeConfig, CloneGroup, CodexConfig, ConfigPutResponse, DATA_DIR, DOCKER_SOCKET,
    DOCKER_SUBNET, DockerConfig, EnvCheckRow, EnvVar, GroupMember, JudgeConfig, JudgeProvider,
    PORT_BASTION, PORT_DAEMON_MCP, PORT_WEB, Preset, PresetRedacted,
    REGISTRY_IMAGE, SERVER_IMAGE, SetupEnv, SshConfig,
};
pub use control::{
    BoardColumn, Chat, ChatMessage, ChatRole, ClaudeSpend, ClaudeUsage, ClaudeUsageWindow,
    CloneRequest, CloneTokens, CodexResetMark, ContainerStats, ControlState, LayoutPreset,
    LinearMeta, LxcStats, MonitorSpec, MonitorState, Operation, OperationKind, OperationStatus,
    Provider, RmngClone, ScheduledMessage, UpdateStatus,
};
pub use exec::{ExecRequest, ExecResult};
pub use ledger::{LedgerHit, LedgerRange, LedgerRecord, LedgerSearch};
pub use mcp::McpCallRequest;
