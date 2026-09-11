//! `wire` — the single source of truth for every type that crosses a process
//! boundary in rmng.
//!
//! - [`control`] — `ControlState` and friends, broadcast over `/events` (port 2)
//!   and persisted to `state.json`. JSON shape is **byte-compatible** with the
//!   current `control-server/app/lib/types.ts` so the React frontend is unchanged.
//! - [`config`] — `AppConfig` (+ a redacted view) edited via the Settings UI.
//! - [`socket`] — the clone-daemon ⇄ control-server unix-socket protocol.
//! - [`holder`] — the clone-daemon ⇄ session-holder protocol, inside one clone.
//! - [`viewer`] — the native viewer ⇄ control-server protocol (port 1).
//! - [`mcp`] — desktop-tool DTOs + the `/api/hosts/:id/mcp` proxy request.
//! - [`exec`] — the `rmng exec` request/result (`/api/hosts/:id/exec`).
//! - [`ledger`] — the distilled transcript record and the two `/api/ledger/*` answers.
//! - [`net`] — the one IO helper: keepalive tuning both ends of port 1 apply.
//!
//! Control-plane + config types derive `ts-rs::TS` and export TypeScript bindings
//! (see the `export_bindings_*` tests ts-rs generates). Transport types
//! (socket/viewer/mcp) are serde-only.

pub mod avc444;
pub mod board;
pub mod config;
pub mod control;
pub mod exec;
pub mod forward;
pub mod holder;
pub mod ledger;
pub mod mcp;
pub mod net;
pub mod socket;
pub mod viewer;

pub use config::{
    AGENT_PORT, BUILDKIT_CACHE_GB, BUILDKIT_IMAGE, BUILD_INFRA_ENABLED, CLAUDE_POLL_SECS,
    CLONE_SOCKET, CODEX_POLL_SECS, CODEX_USAGE_POLLING, DATA_DIR, DOCKER_SOCKET, DOCKER_SUBNET,
    PORT_BASTION, PORT_DAEMON_MCP, PORT_FORWARD, PORT_VIDEO, PORT_WEB, REGISTRY_IMAGE,
    SERVER_IMAGE, AppConfig, AppConfigRedacted, ChromaMode, ClaudeConfig, CloneGroup,
    CodexConfig, ConfigPutResponse, DockerConfig, EnvCheckRow, EnvVar, ImageInfo, JudgeConfig,
    Preset, PresetRedacted, SetupEnv, SshConfig,
};
pub use control::{
    BoardColumn, Chat, ChatMessage, ChatRole, ClaudeSpend, ClaudeUsage, ClaudeUsageWindow,
    CloneRequest, CloneTokens, CodexResetMark, ContainerStats, ControlState, LayoutPreset,
    LinearMeta, LxcStats, MonitorSpec, MonitorState, Operation, OperationKind, OperationStatus,
    PortForward, Provider, RmngClone, ScheduledMessage, UpdateStatus,
};
pub use exec::{ExecRequest, ExecResult};
pub use ledger::{LedgerHit, LedgerRange, LedgerRecord, LedgerSearch};
pub use mcp::McpCallRequest;
