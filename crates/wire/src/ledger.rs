//! Transcript-ledger DTOs: the distilled record the control-server writes for every clone,
//! and the two answers its query API returns.
//!
//! [`LedgerRecord`] is both the on-disk NDJSON line under `data/ledger/<clone>/<session>.ndjson`
//! and the shape a search hit carries, so the file format and the API can never drift apart.
//!
//! Serde-only, like [`crate::exec`]. The readers are the control-server, the `rmng` CLI, and
//! anything else holding a key to the web API; the browser draws none of this.

use serde::{Deserialize, Serialize};

/// One distilled event out of a clone's Claude Code transcript.
///
/// The first four fields are what make a grep hit self-describing, so they are always written
/// and always in this order.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LedgerRecord {
    /// The clone whose transcript this came from.
    pub clone: String,
    /// The Claude Code session id, which is also this record's file name.
    pub session: String,
    /// RFC3339, as the CLI wrote it. A record with no timestamp of its own inherits the newest
    /// one seen before it in the same transcript.
    pub ts: String,
    /// `user`, `assistant`, `toolUse`, `toolResult`, `title` or `compact`.
    pub kind: String,
    /// The tool's name, on a `toolUse` record.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub tool: String,
    /// The `toolu_…` id, which ties a `toolResult` back to the `toolUse` that asked for it.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub tool_id: String,
    /// The subagent that produced the event, on a `sidechain` record. One subagent's whole run
    /// carries one id, so this is how a delegated task is read back as a unit.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub agent_id: String,
    /// True when the event belongs to a subagent running inside this session rather than to
    /// the conversation itself.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub sidechain: bool,
    /// The distilled text: a message, a clipped tool input, a clipped tool result.
    pub text: String,
}

/// One matching ledger line, with what a caller needs to read around it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LedgerHit {
    pub clone: String,
    pub session: String,
    pub ts: String,
    pub kind: String,
    /// Byte offset of the line within `<clone>/<session>.ndjson`, for a range read.
    pub offset: u64,
    /// Length of the line in bytes, its newline included.
    pub len: u64,
    /// The whole matching line, which is a serialized [`LedgerRecord`].
    pub line: String,
}

/// What `GET /api/ledger/search` answers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LedgerSearch {
    pub hits: Vec<LedgerHit>,
    pub scanned_bytes: u64,
    /// The search stopped early, on the hit limit or the byte budget. There are more matches.
    pub truncated: bool,
}

/// What `GET /api/ledger/read` answers: a byte range of one session's ledger.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LedgerRange {
    pub clone: String,
    pub session: String,
    /// Where the returned text starts, snapped back to a line start.
    pub offset: u64,
    pub len: u64,
    /// The whole file's size, so a caller can tell how much is left on either side.
    pub size: u64,
    /// Whole NDJSON lines, never a fragment of one.
    pub text: String,
}
