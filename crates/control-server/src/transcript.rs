//! The agent CLIs' own transcript trees: where they sit, how one is walked, and what a single
//! line of each format says.
//!
//! **Why this is one module.** Two readers walk the same files for two different questions.
//! [`crate::stuck`] asks whether a clone is working or waiting on a person, and folds these
//! records into hook events and silence windows. [`crate::agentlog`] asks what a clone has
//! spent, and folds the same records into token deltas. Each used to own a private copy of the
//! tree layout, the walk, the caps and the line structs — including two different structs both
//! named `CodexLine`, with different fields, so a Codex CLI format change had to be found twice
//! and nothing in either file said so.
//!
//! **What is honestly shared is the reading, never the folding**: where the files are, how far
//! down to look, how much of a clone's filesystem one pass may touch, and what one line
//! deserialises to. The two folds stay with their consumers, because they read different facts
//! out of the same bytes — `stuck` wants tool-call pairing and interruption, `agentlog` wants
//! per-turn token deltas against a byte cursor, and nothing is gained by pretending those are
//! one operation.
//!
//! **Nothing here reads a clone's files with any authority.** A clone is a sandbox someone (or
//! some agent) has root in, so every path below is attacker-writable in the ordinary case, and
//! the bounds on the walk are what keep a pass finite. See [`walk_jsonl`].

use std::path::Path;

use serde::Deserialize;
use serde_json::Value;

// ---------------------------------------------------------------------------------------
// Where the trees are
// ---------------------------------------------------------------------------------------

/// The clone's home inside the clone's own filesystem root.
///
/// The two consumers reach a clone from different ends, which is why the tree names below are
/// written relative to the home rather than to either base. [`crate::agentlog`] reaches a clone
/// through `<data_dir>/hosts/<id>`, a symlink that already points AT the home, so it joins a
/// tree name directly. [`crate::stuck`] works from the container root (`/proc/<pid>/root`),
/// because the background-task outputs it also reads live under `/tmp`, outside the home, so it
/// joins this first.
pub(crate) const HOME: &str = "home/rmng";

/// Where Claude Code files a session's transcript, under the clone's home.
pub(crate) const CLAUDE_PROJECTS: &str = ".claude/projects";

/// Where Cursor files a conversation's transcript, under the clone's home.
///
/// `<workspace>/agent-transcripts/<conversation>/<conversation>.jsonl`, so the file stem is the
/// conversation id and the same stem-keyed walk covers this tree and Claude Code's.
pub(crate) const CURSOR_PROJECTS: &str = ".cursor/projects";

/// Where the Codex CLI files a session's transcript, under the clone's home.
///
/// One JSONL "rollout" per session, in dated directories:
/// `~/.codex/sessions/2026/08/08/rollout-2026-08-08T02-59-54-<session-id>.jsonl`. The name
/// carries both the start time and the id, and the id is its last 36 characters.
pub(crate) const CODEX_SESSIONS: &str = ".codex/sessions";

/// How far below [`CLAUDE_PROJECTS`] a transcript can sit.
///
/// Claude writes three shapes, and a walk that reaches only the first two silently drops the
/// third. Counted across the CT 105 and CT 106 fleets, with the depth each needs:
///
/// | Shape | Depth | Files |
/// |---|---|---|
/// | `<slug>/<session>.jsonl` | 1 | 796 |
/// | `<slug>/<session>/subagents/agent-*.jsonl` | 3 | 5,553 |
/// | `<slug>/<session>/subagents/workflows/<run>/agent-*.jsonl` | 5 | 2,564 |
///
/// The third is what a workflow's agents write, one directory per run. On the clone that
/// surfaced it, `haoran-dev-270`, it was 624 of 1,038 transcripts and every one of the files
/// being appended to at the time: the clone had an agent working in front of someone's eyes and
/// read `idle`, because nothing it was writing was in range.
///
/// 5 covers all three exactly, with no headroom, which is deliberate. Depth is what keeps this
/// walk inside the CLI's own tree instead of descending into whatever a clone parks under it, and
/// a shape this misses is a visible bug rather than a silent wrong number. Breadth is bounded
/// separately by the walk's budget.
pub(crate) const CLAUDE_WALK_DEPTH: usize = 5;

/// How far below [`CODEX_SESSIONS`] a rollout can sit: `YYYY/MM/DD/rollout-*.jsonl`, three
/// levels of date directory. Codex writes one file per session and nests nothing under it.
pub(crate) const CODEX_WALK_DEPTH: usize = 3;

/// Hard stop on how many transcripts one provider's walk will enumerate for the token scan.
///
/// [`crate::agentlog`]'s own read cap cannot be the walk's stopping point: choosing the newest
/// files means seeing all of them first. This is the backstop that keeps "see all of them" finite
/// for a clone that creates millions of `.jsonl` files, which it can, being root in its own
/// sandbox. Paths past it are dropped with a warning.
pub(crate) const MAX_ENUM_FILES_PER_PROVIDER: usize = 32_768;

/// Hard stop on how many transcripts one walk on the monitor's four-second tick enumerates.
///
/// A separate number from [`MAX_ENUM_FILES_PER_PROVIDER`] and deliberately smaller: these walks
/// run on the monitor's four-second tick rather than the token scanner's fifteen-second one, and
/// they read mtimes rather than bytes. A clone keeps every project it has ever opened, and the
/// tree is shallow.
pub(crate) const MAX_TICK_WALK_FILES: usize = 20_000;

/// The session id inside a rollout's file name, which is the trailing UUID.
///
/// `rollout-2026-08-08T02-59-54-019fe02b-cb2c-7ec0-8a48-77d87c7f057f` is one id, not five
/// dash-separated fields, so this counts characters from the end rather than splitting.
pub(crate) fn codex_session_id(stem: &str) -> Option<&str> {
    let id = stem.get(stem.len().checked_sub(36)?..)?;
    let shaped = id.len() == 36
        && id.chars().all(|c| c.is_ascii_hexdigit() || c == '-')
        && [8, 13, 18, 23].iter().all(|i| id.as_bytes()[*i] == b'-');
    shaped.then_some(id)
}

// ---------------------------------------------------------------------------------------
// The walk
// ---------------------------------------------------------------------------------------

/// The shape of one walk: how deep it may go and which directory names it refuses to enter.
///
/// Separate from the budget, which is passed alongside, because the budget is the one thing a
/// caller threads across several roots.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Walk {
    /// How many levels below the root to descend, or `None` for no bound. A bounded depth is
    /// what keeps a walk inside the CLI's own tree shape rather than wandering into a checkout
    /// of someone's dataset; see [`CLAUDE_WALK_DEPTH`] for the one that is measured.
    pub depth: Option<usize>,
    /// Directory names never entered, whatever the depth allows.
    pub skip_dirs: &'static [&'static str],
}

impl Walk {
    /// A walk with no depth bound and nothing skipped.
    pub fn all() -> Self {
        Self {
            depth: None,
            skip_dirs: &[],
        }
    }

    /// A walk bounded to `depth` levels below the root, skipping nothing.
    pub fn to_depth(depth: usize) -> Self {
        Self {
            depth: Some(depth),
            skip_dirs: &[],
        }
    }

    /// The same walk with `skip_dirs` set.
    pub fn skipping(self, skip_dirs: &'static [&'static str]) -> Self {
        Self { skip_dirs, ..self }
    }
}

/// Every `*.jsonl` under `root` within `walk`, handed to `visit`, up to `budget` files.
///
/// Bounded in BOTH dimensions, because a clone's home is writable by someone with root inside
/// the sandbox. Depth keeps the walk inside the known tree shapes rather than wandering into a
/// checkout of someone's dataset. Breadth matters just as much and is easier to miss: without
/// it, a clone that creates millions of `.jsonl` files costs a `metadata` syscall each per tick
/// and, for the token scan, a permanent `PathBuf` in the cursor map.
///
/// `budget` counts **files handed to `visit`**, and it is decremented across the whole walk,
/// siblings and subdirectories alike, so the total is capped rather than the total per
/// directory. It is a `&mut` rather than a value because a caller with several roots decides
/// whether they share one allowance or each get their own — and that is a decision with an
/// incident behind it. [`crate::agentlog`] gives each provider its OWN budget: sharing one meant
/// a clone with many Claude transcripts consumed the whole allowance before the walk reached
/// `.codex`, and its Codex sessions then counted for nothing.
///
/// `visit` is handed the directory entry rather than a path, so a caller that wants the mtime
/// pays for that `metadata` call and a caller that does not, does not.
///
/// Symlinks are followed only in the sense that `entry.file_type()` reports the LINK's type, so
/// a symlinked directory is not descended into and a symlinked file is not visited. Escaping the
/// clone is separately impossible: `/proc/<pid>/root` resolves paths with chroot-like semantics,
/// so an absolute link inside the clone lands inside the clone and `..` cannot climb past its
/// root.
pub(crate) fn walk_jsonl(
    root: &Path,
    walk: Walk,
    budget: &mut usize,
    visit: &mut dyn FnMut(&std::fs::DirEntry),
) {
    if *budget == 0 {
        return;
    }
    let Ok(rd) = std::fs::read_dir(root) else {
        return;
    };
    for entry in rd.flatten() {
        if *budget == 0 {
            return;
        }
        let Ok(ft) = entry.file_type() else { continue };
        let name = entry.file_name();
        if ft.is_dir() {
            if walk.skip_dirs.iter().any(|s| name.as_os_str() == *s) {
                continue;
            }
            let deeper = match walk.depth {
                Some(0) => continue,
                Some(n) => Walk {
                    depth: Some(n - 1),
                    ..walk
                },
                None => walk,
            };
            walk_jsonl(&entry.path(), deeper, budget, visit);
        } else if ft.is_file()
            && Path::new(&name)
                .extension()
                .is_some_and(|ext| ext == "jsonl")
        {
            *budget -= 1;
            visit(&entry);
        }
    }
}

/// The last non-empty line of `path`, or `None` if there is not one to be had.
///
/// Read from the end, so a 200 MB transcript costs the same as a small one. A last record too
/// large for the window reads as absent: the callers all have to fail on the side of believing
/// less rather than more.
pub(crate) fn last_record(path: &Path) -> Option<String> {
    use std::io::{Read, Seek, SeekFrom};
    /// Comfortably past an interrupt record, which is about 500 bytes.
    const WINDOW: u64 = 16 * 1024;

    let mut file = std::fs::File::open(path).ok()?;
    let len = file.metadata().map(|m| m.len()).ok()?;
    let from = len.saturating_sub(WINDOW);
    file.seek(SeekFrom::Start(from)).ok()?;
    let mut buf: Vec<u8> = Vec::new();
    file.take(len - from).read_to_end(&mut buf).ok()?;
    let text = String::from_utf8_lossy(&buf);
    // The window can open mid-line, and that leading fragment is dropped by taking the last.
    text.lines()
        .rfind(|l| !l.trim().is_empty())
        .map(str::to_string)
}

// ---------------------------------------------------------------------------------------
// Timestamps
// ---------------------------------------------------------------------------------------

/// Epoch ms from an RFC3339 timestamp.
///
/// Reuses the account pollers' parser, which already handles both the `Z` form these logs
/// use and the `±HH:MM` form (a clone in a non-UTC zone). Sub-second precision is dropped —
/// irrelevant here, where the coarsest consumer is a 5-minute activity window.
pub(crate) fn ts_ms(s: &str) -> Option<i64> {
    crate::pool::parse_rfc3339_utc_secs(s).map(|secs| secs * 1000)
}

/// Epoch seconds from an RFC3339 timestamp, for the folds that difference stamps against each
/// other rather than against this host's clock.
pub(crate) fn ts_secs(s: &str) -> Option<f64> {
    crate::pool::parse_rfc3339_utc_secs(s).map(|secs| secs as f64)
}

// ---------------------------------------------------------------------------------------
// One line of each format
// ---------------------------------------------------------------------------------------

/// A token count as it appears in a log.
///
/// `Option<u64>` rather than `#[serde(default)] u64` on purpose. These fields sit inside a
/// nested struct, so serde failing on ONE of them fails the whole line, and a caller's
/// `let Ok(rec) = … else { return }` then drops a real response silently. `null` — which
/// `#[serde(default)]` does *not* accept for a bare `u64` — is a shape these CLIs really do
/// emit. Being tolerant per field turns "lose the entire record" into "treat one field as
/// absent". Whether a figure is *plausible* is the consumer's question, not this one's.
pub(crate) type TokenField = Option<u64>;

// --- Claude Code (and Cursor) transcript records --------------------------------------------

/// One line of a Claude Code or Cursor transcript.
///
/// Both CLIs write the same record shape, which is why one struct reads both trees. They differ
/// on one key and [`ClaudeRecord::role`] is where.
#[derive(Debug, Default, Deserialize)]
pub(crate) struct ClaudeRecord {
    /// The record's own kind — `assistant`, `user`, and the rest. Claude Code puts the speaker
    /// here; Cursor puts it in `role`.
    #[serde(rename = "type", default)]
    pub kind: Option<String>,
    /// Cursor's name for what Claude Code calls `type`.
    #[serde(default)]
    pub role: Option<String>,
    #[serde(default)]
    pub timestamp: Option<String>,
    /// The API request this line reports on. Present on a small minority of lines, and on none
    /// of the subagent transcripts sampled — see [`crate::agentlog`]'s response key.
    #[serde(rename = "requestId", default)]
    pub request_id: Option<String>,
    #[serde(default)]
    pub message: Option<ClaudeMessage>,
}

impl ClaudeRecord {
    pub fn parse(line: &str) -> Option<Self> {
        serde_json::from_str(line).ok()
    }

    /// Who spoke this record, whichever key the CLI wrote it under.
    ///
    /// Cursor names this `role` where Claude Code names it `type`. Reading only one of them is
    /// how an assistant line merely quoting a marker gets mistaken for the user having typed it.
    pub fn speaker(&self) -> Option<&str> {
        self.kind.as_deref().or(self.role.as_deref())
    }

    /// The text of this record's message, for the one reader that cares what was said.
    ///
    /// `content` is a bare string on some lines and a list of content blocks on others; this is
    /// the first block's text in the second case, which is where a turn-ending marker lands.
    pub fn said(&self) -> String {
        let Some(content) = self.message.as_ref().and_then(|m| m.content.as_ref()) else {
            return String::new();
        };
        match content.as_str() {
            Some(s) => s.to_string(),
            None => content
                .as_array()
                .and_then(|blocks| blocks.first())
                .and_then(|b| b.get("text"))
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
        }
    }
}

#[derive(Debug, Default, Deserialize)]
pub(crate) struct ClaudeMessage {
    /// The API response id (`msg_…`). Every line carrying a `usage` block carries one, and all
    /// the lines of one response share it, which is what makes it the counting key.
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub usage: Option<ClaudeUsage>,
    /// What the speaker said, in either of the two shapes [`ClaudeRecord::said`] handles.
    #[serde(default)]
    pub content: Option<Value>,
}

#[derive(Debug, Default, Deserialize)]
pub(crate) struct ClaudeUsage {
    #[serde(default)]
    pub input_tokens: TokenField,
    #[serde(default)]
    pub output_tokens: TokenField,
    #[serde(default)]
    pub cache_creation_input_tokens: TokenField,
    // `cache_read_input_tokens` is deliberately NOT deserialized: it is excluded from the
    // count, and naming it here would invite someone to add it in.
}

// --- Codex rollout records -----------------------------------------------------------------

/// One rollout line. Codex wraps everything in `{type, timestamp, payload}` and the payload
/// carries its own `type`, so the pair is what identifies a record.
///
/// The payload stays a [`Value`]. Its shape varies per record kind far more than it repeats —
/// a tool call carries `name`/`call_id`/`input`, a completion carries `last_agent_message`, a
/// token count carries a nested usage object — and typing the union would be a struct whose
/// fields are wrong for every record but one. The typed accessors below cover what is actually
/// read; anything else goes through [`CodexRecord::str_at`].
#[derive(Debug, Default, Deserialize)]
pub(crate) struct CodexRecord {
    #[serde(rename = "type", default)]
    pub kind: Option<String>,
    #[serde(default)]
    pub timestamp: Option<String>,
    #[serde(default)]
    pub payload: Option<Value>,
}

impl CodexRecord {
    pub fn parse(line: &str) -> Option<Self> {
        serde_json::from_str(line).ok()
    }

    /// The payload's own `type`, which with [`CodexRecord::kind`] identifies the record.
    pub fn payload_kind(&self) -> Option<&str> {
        self.payload.as_ref()?.get("type")?.as_str()
    }

    /// A string field of the payload.
    pub fn str_at(&self, key: &str) -> Option<String> {
        self.payload
            .as_ref()?
            .get(key)
            .and_then(Value::as_str)
            .map(str::to_string)
    }

    /// The per-turn token delta carried by a `token_count` event.
    ///
    /// Deliberately the payload's `info.last_token_usage` rather than the sibling
    /// `total_token_usage`, which is cumulative *within the session* — summing that across
    /// events would multiply-count every earlier turn.
    pub fn last_token_usage(&self) -> Option<CodexUsage> {
        let usage = self.payload.as_ref()?.pointer("/info/last_token_usage")?;
        serde_json::from_value(usage.clone()).ok()
    }
}

#[derive(Debug, Default, Deserialize)]
pub(crate) struct CodexUsage {
    #[serde(default)]
    pub input_tokens: TokenField,
    #[serde(default)]
    pub cached_input_tokens: TokenField,
    #[serde(default)]
    pub output_tokens: TokenField,
    #[serde(default)]
    pub reasoning_output_tokens: TokenField,
}
