//! A durable, distilled copy of every clone's Claude Code transcripts.
//!
//! **Why.** A clone's transcript is the only record of how a piece of work was actually done:
//! what was tried, what broke, what the operator corrected. It lives in the clone's own home and
//! dies with the container. An assistant that has to answer "how did we do this last time" needs
//! that history to outlive the clone that produced it.
//!
//! **How it reads.** [`crate::homes`] already maintains `<data_dir>/hosts/<id>` as a symlink to
//! the clone's home, so this is a plain file read from the server process. No daemon runs inside
//! a clone, nothing is installed there, and it works on clones that are already running. Source
//! shape is `<data_dir>/hosts/<id>/.claude/projects/<slug>/<session>.jsonl`, which is the CLI's
//! main-session transcript, and `.cursor/projects/<workspace>/agent-transcripts/<id>/<id>.jsonl`,
//! which is Cursor's.
//!
//! **Subagents count too.** Both agents write a delegated task to `subagents/` beside the session
//! that spawned it, and on this fleet that is where most of the work is: on four live clones the
//! subagent files held 16.6, 70.2, 73.8 and 96.7 percent of all transcript bytes. A clone running
//! a review loop or a judge panel puts almost everything there. Those records are distilled into
//! the spawning session's ledger file, carry `sidechain: true` and name their `agentId`, so a
//! search can ask for the conversation alone, for one subagent's whole run, or for both.
//!
//! The two agents write the same content blocks under different line keys, so one distiller
//! reads both. Cursor names a line `role` where Claude Code names it `type`, and stamps no line
//! with a time, which is the only thing this module has to make up: a line carrying no timestamp
//! anywhere takes the moment the pass read it.
//!
//! **What it writes.** One NDJSON record per event under
//! `<data_dir>/ledger/<clone>/<session>.ndjson`. Every record repeats the clone, the session, the
//! timestamp and the kind, so a grep hit is readable on its own without its file header.
//! Extraction is deterministic and no model runs here.
//!
//! **Why it shrinks so much.** Measured on this machine's own three-day session: 132,486,201 raw
//! bytes distil to 2,875,456 in 3,879 records, which is 2.2 percent. Almost all of what goes is
//! base64 screenshot data (122,065,934 bytes of that file sat in `user` records carrying tool
//! results). Dropping the base64, dropping the model's thinking, and clipping tool inputs and
//! results is what turns a transcript into something greppable.
//!
//! **Why the ledger directory is also a name registry.** A ledger outlives its clone, so
//! `<data_dir>/ledger/` lists every clone name ever used. Reusing one would merge two unrelated
//! histories into a single bucket, so [`reserved_names`] feeds both hostname derivation and the
//! exact-hostname create path (see [`crate::jobs`]).

use std::collections::{BTreeMap, HashSet};
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::app::App;

/// The record shape, the two answers, and nothing else: `wire` owns them, so the on-disk
/// NDJSON line and what the `rmng` CLI parses out of the API are one definition.
pub use wire::{LedgerHit as Hit, LedgerRange as Range, LedgerRecord, LedgerSearch as SearchResult};

/// How often the fleet is tailed.
///
/// Slower than [`crate::agentlog`]'s 15s walk because nothing waits on this: the ledger is read
/// by a search API, not by anything on screen. What a longer interval really costs is how much a
/// clone can write between passes, and the final pass before delete (see [`tail_once`]) closes
/// that gap where it matters.
const TAIL_INTERVAL: Duration = Duration::from_secs(30);

/// Cap on bytes read from one transcript in one pass. The remainder is picked up next tick.
///
/// This is what keeps a first sight of a large retained transcript from stalling the pass. Unlike
/// [`crate::agentlog`], a file seen for the first time is read from byte 0 rather than cursored
/// at its end: the history IS the product here.
const MAX_READ_PER_FILE: u64 = 8 * 1024 * 1024;

/// Cap on bytes read across one clone in one pass.
///
/// [`MAX_READ_PER_FILE`] alone bounds nothing useful on first run, when a clone with a hundred
/// retained transcripts would read eight hundred megabytes in a single pass. Backlog is drained
/// over several ticks instead.
///
/// Subagent transcripts made that backlog several times larger: the largest clone measured holds
/// 645 MB of them, which is about eleven passes, or five and a half minutes, before a first sight
/// of it is caught up. That is the intended trade. Raising the budget would let one clone's
/// history block the fleet's live tail, and the pass walks main transcripts before subagent ones
/// (see [`session_files`]) so the conversation an operator had is never the part left waiting.
const MAX_READ_PER_CLONE: u64 = 64 * 1024 * 1024;

/// Longest tool input kept, in characters. Enough for a command line, a file path plus a small
/// patch, or the first screen of a query.
const MAX_TOOL_INPUT_CHARS: usize = 800;

/// Longest head of a tool result kept, in characters. Results are the bulkiest thing worth keeping
/// at all: a file read, a test run, a `git log`. The head is where the answer usually is.
const MAX_TOOL_RESULT_CHARS: usize = 2000;

/// Characters kept from the end of a tool result that ran past [`MAX_TOOL_RESULT_CHARS`].
///
/// A tool result is long exactly when something happened, and what happened is usually last: a
/// test runner prints its counts after the log, a build prints the error after the output. The
/// head alone cannot reach that at any cap, because the end moves with the length.
///
/// Measured over 38,396 tool results on one clone, of which 13,149 ran past the head cap. Scoring
/// the kept text for an outcome word (`fail`, `passed`, `traceback`, `exit code N` and the like),
/// the head alone carries one for 5,799 of those and head plus a 500-character tail for 6,791.
/// Nothing is lost, because the head is untouched. Per character spent it beats doubling the head,
/// which recovers 1.01 records per 100 characters against this scheme's 1.98.
const MAX_TOOL_RESULT_TAIL_CHARS: usize = 500;

/// Longest message text kept, in characters.
///
/// Sized for a subagent's final report, which arrives as a single message rather than as a
/// conversation. On two measured clones the p99 assistant message is 15,717 and 16,453 characters
/// inside subagent transcripts against 2,791 and 3,023 in the main loop, and the longest is
/// 81,564. The old 20,000 was sized against main-loop chat and clipped those reports at p99.
///
/// The ceiling on this is [`MAX_LINE_BYTES`]: a record longer than that is skipped by every
/// search, so a cap that no longer fits well under it would quietly make the report unfindable
/// rather than merely truncated.
const MAX_TEXT_CHARS: usize = 100_000;

/// Most transcripts enumerated for one clone in one pass. A clone is root in its own sandbox and
/// can create as many files as it likes under that path.
///
/// Subagent transcripts are what this now bounds: a session's main transcript is one file, and one
/// measured clone had 1,077 subagent files against it. The cost of the cap is one `offsets.json`
/// entry per file, rewritten whenever any file in the clone advances, so a few hundred kilobytes
/// at the numbers seen and a few megabytes at this limit.
const MAX_SESSION_FILES: usize = 16_384;

/// How long one clone's tail may take before the pass abandons it.
///
/// A clone can mount a FUSE filesystem under `~/.claude/projects/` that never answers, which
/// parks a read in uninterruptible sleep forever. The timeout does not cancel that syscall, but
/// it does keep one wedged clone from stopping the rest of the fleet.
const CLONE_TAIL_TIMEOUT: Duration = Duration::from_secs(20);

/// Most bytes one search reads before it gives up and says so.
const MAX_SEARCH_BYTES: u64 = 512 * 1024 * 1024;

/// Longest ledger line a search will look at. Everything this module writes is clipped far below
/// it; a longer line means the file was hand-edited or corrupted.
const MAX_LINE_BYTES: usize = 1024 * 1024;

/// Most hits one search returns when the caller names no limit.
const DEFAULT_SEARCH_LIMIT: usize = 50;

/// Hard cap on hits per search, whatever the caller asks for.
const MAX_SEARCH_LIMIT: usize = 500;

/// Most bytes one range read returns.
const MAX_READ_RANGE: u64 = 1024 * 1024;

// --- on-disk layout ---------------------------------------------------------------------------

/// `<data_dir>/ledger` — one directory per clone that has ever existed.
pub fn ledger_root(data_dir: &str) -> PathBuf {
    Path::new(data_dir).join("ledger")
}

/// `<data_dir>/ledger/<clone>`, or `None` if the id is not a safe path component.
fn clone_dir(data_dir: &str, clone: &str) -> Option<PathBuf> {
    crate::files::is_safe_id(clone).then(|| ledger_root(data_dir).join(clone))
}

/// Every clone name the ledger has a directory for, live clones included.
///
/// This is the registry of names already used. It is read by [`crate::jobs::next_free_hostname`]
/// and by [`crate::jobs::start_clone`], so a retired clone's name is never handed to a new clone
/// whose history would then be filed under it.
pub fn reserved_names(data_dir: &str) -> HashSet<String> {
    let Ok(rd) = std::fs::read_dir(ledger_root(data_dir)) else { return HashSet::new() };
    rd.flatten()
        .filter(|e| e.file_type().map(|t| t.is_dir()).unwrap_or(false))
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| crate::files::is_safe_id(n))
        .collect()
}

/// The ledger file name for a transcript: its stem plus `.ndjson`.
///
/// A stem that is not a plain identifier is rejected rather than sanitized. Session file names are
/// UUIDs, so anything else under that directory was put there by hand or by something hostile,
/// and mapping two different stems onto one ledger file would interleave two sessions.
fn ledger_name(src: &Path) -> Option<String> {
    let stem = src.file_stem()?.to_str()?;
    let plain = !stem.is_empty()
        && stem.len() <= 128
        && stem.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
    plain.then(|| format!("{stem}.ndjson"))
}

/// Where a clone's per-transcript cursors are kept. Named without a leading dot so it sits beside
/// the session files it describes; the `.ndjson` extension is what tells the two apart.
fn offsets_path(dir: &Path) -> PathBuf {
    dir.join("offsets.json")
}

/// What the tailer remembers about one transcript between passes.
///
/// `inode` and `offset` together detect the two ways a cursor goes stale: the file was replaced
/// (new inode) or truncated (`offset` past the end). Either restarts it from 0. `last_ts` and
/// `last_title` are carried because the records that need them arrive without one: an `ai-title`
/// line carries no timestamp of its own, and the CLI rewrites the title on almost every turn.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
struct FileState {
    inode: u64,
    offset: u64,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    last_ts: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    last_title: String,
}

/// One clone's cursors, keyed by the transcript's path relative to the clone's home.
///
/// A `BTreeMap` so the file rewrites byte-identically when nothing moved. Entries are never
/// pruned for a transcript that has disappeared: the ledger is append-only, so wrongly dropping a
/// live cursor would re-read a whole transcript and append a second copy of it. Growth is one
/// small entry per session the clone has ever run.
type Offsets = BTreeMap<String, FileState>;

fn load_offsets(dir: &Path) -> Offsets {
    std::fs::read_to_string(offsets_path(dir))
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

fn save_offsets(dir: &Path, offsets: &Offsets) -> std::io::Result<()> {
    let path = offsets_path(dir);
    let mut body = serde_json::to_string_pretty(offsets).unwrap_or_default();
    body.push('\n');
    let tmp = path.with_extension(format!("tmp.{}", std::process::id()));
    std::fs::write(&tmp, body.as_bytes())?;
    std::fs::rename(&tmp, &path)
}

// --- the transcript, as it is written ----------------------------------------------------------

/// The fields of a transcript line this module reads. Everything else the CLI writes
/// (`attachment`, `last-prompt`, `queue-operation`, `file-history-*`, `mode`, `permission-mode`,
/// `agent-name`) is either noise or a duplicate of a record kept here.
#[derive(Debug, Deserialize)]
struct RawLine {
    /// `type` is what Claude Code names a line with. Cursor names the same thing `role`, and
    /// carries no `type` on a message at all, so one field reads both. A Cursor line that does
    /// carry a `type` (`turn_ended`) matches nothing below and contributes no record, which is
    /// right: the turn boundary is already implied by what surrounds it.
    #[serde(rename = "type", alias = "role", default)]
    kind: Option<String>,
    #[serde(default)]
    subtype: Option<String>,
    #[serde(rename = "sessionId", default)]
    session_id: Option<String>,
    #[serde(default)]
    timestamp: Option<String>,
    #[serde(rename = "aiTitle", default)]
    ai_title: Option<String>,
    #[serde(rename = "isSidechain", default)]
    sidechain: Option<bool>,
    /// Codex wraps every record in an envelope and puts the record itself here. Claude Code
    /// and Cursor write no such key, so its presence is what selects the Codex reading.
    #[serde(default)]
    payload: Option<serde_json::Value>,
    /// The subagent this line belongs to. Written on every line of a subagent transcript and on
    /// nothing in a main one.
    #[serde(rename = "agentId", default)]
    agent_id: Option<String>,
    #[serde(default)]
    message: Option<RawMessage>,
    #[serde(rename = "compactMetadata", default)]
    compact: Option<CompactMeta>,
}

#[derive(Debug, Deserialize)]
struct RawMessage {
    /// A string on a plain typed prompt, an array of content blocks otherwise.
    #[serde(default)]
    content: Option<serde_json::Value>,
}

#[derive(Debug, Deserialize)]
struct CompactMeta {
    #[serde(default)]
    trigger: Option<String>,
    #[serde(rename = "preTokens", default)]
    pre_tokens: Option<i64>,
    #[serde(rename = "postTokens", default)]
    post_tokens: Option<i64>,
}

/// One content block of a message.
#[derive(Debug, Deserialize)]
struct Block {
    #[serde(rename = "type", default)]
    kind: Option<String>,
    #[serde(default)]
    text: Option<String>,
    /// The tool's name, on a `tool_use`.
    #[serde(default)]
    name: Option<String>,
    /// The tool's arguments, on a `tool_use`.
    #[serde(default)]
    input: Option<serde_json::Value>,
    /// The `toolu_…` id a `tool_use` mints.
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    tool_use_id: Option<String>,
    /// A string or an array of blocks, on a `tool_result`.
    #[serde(default)]
    content: Option<serde_json::Value>,
    /// The base64 payload of an `image`, which is exactly what never reaches the ledger.
    #[serde(default)]
    source: Option<ImageSource>,
    /// Codex writes an image as one `data:` URI under this key instead of as a typed block.
    #[serde(default)]
    image_url: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ImageSource {
    #[serde(default)]
    media_type: Option<String>,
    #[serde(default)]
    data: Option<String>,
}

// --- distillation ------------------------------------------------------------------------------

/// `s` cut to `max` characters, with the dropped byte count spelled out.
///
/// Cutting on a character boundary rather than a byte one, so the result is still valid UTF-8 and
/// still serializes.
fn clip(s: &str, max: usize) -> String {
    clip_ends(s, max, 0)
}

/// `s` cut to its first `head` characters and its last `tail`, with the dropped byte count spelled
/// out between them. A `tail` of zero is [`clip`].
///
/// The tail is what makes a long tool result searchable for its verdict (see
/// [`MAX_TOOL_RESULT_TAIL_CHARS`]). A string short enough that the two ends would meet is kept
/// whole, which is never more than the budget the two caps already allow.
fn clip_ends(s: &str, head: usize, tail: usize) -> String {
    let Some((cut, _)) = s.char_indices().nth(head) else { return s.to_string() };
    if tail == 0 {
        return format!("{} [clipped, +{} bytes]", &s[..cut], s.len() - cut);
    }
    // Walking back from the end rather than counting the whole string, which is megabytes on the
    // results this fires for.
    let Some((from, _)) = s.char_indices().nth_back(tail - 1) else { return s.to_string() };
    if from <= cut {
        return s.to_string();
    }
    format!("{} [clipped, +{} bytes] {}", &s[..cut], from - cut, &s[from..])
}

/// What an image block leaves behind: its type, never its bytes.
fn image_marker(block: &Block) -> String {
    // Claude Code types the block and carries the payload under `source`. Codex writes one
    // `data:<media>;base64,<payload>` URI, so the same two facts are split by the comma.
    if let Some(uri) = block.image_url.as_deref() {
        let (head, data) = uri.split_once(',').unwrap_or((uri, ""));
        let media = head.strip_prefix("data:").unwrap_or(head);
        let media = media.strip_suffix(";base64").unwrap_or(media);
        let media = match media.is_empty() {
            true => "image",
            false => media,
        };
        return format!("[{media}, {} base64 bytes dropped]", data.len());
    }
    let src = block.source.as_ref();
    let media = src.and_then(|s| s.media_type.as_deref()).unwrap_or("image");
    let bytes = src.and_then(|s| s.data.as_deref()).map_or(0, str::len);
    format!("[{media}, {bytes} base64 bytes dropped]")
}

/// The text of a `tool_result`'s content, which is a string or a list of blocks.
fn tool_result_text(content: Option<&serde_json::Value>) -> String {
    let clip_result =
        |s: &str| clip_ends(s, MAX_TOOL_RESULT_CHARS, MAX_TOOL_RESULT_TAIL_CHARS);
    let Some(value) = content else { return String::new() };
    if let Some(s) = value.as_str() {
        return clip_result(s);
    }
    let Ok(blocks) = serde_json::from_value::<Vec<Block>>(value.clone()) else {
        return clip_result(&value.to_string());
    };
    let mut parts: Vec<String> = Vec::new();
    for b in &blocks {
        match b.kind.as_deref() {
            Some("image") | Some("input_image") => parts.push(image_marker(b)),
            _ => {
                if let Some(t) = b.text.as_deref().filter(|t| !t.is_empty()) {
                    parts.push(t.to_string());
                }
            }
        }
    }
    clip_result(&parts.join("\n"))
}

/// Turn one raw transcript line into the records it contributes, updating the transcript's
/// carried-forward timestamp and title along the way.
///
/// `session` is the fallback session id for a line that names none, which is the transcript's own
/// file stem.
fn distill(
    clone: &str,
    session: &str,
    line: &str,
    st: &mut FileState,
    now: &str,
) -> Vec<LedgerRecord> {
    let Ok(raw) = serde_json::from_str::<RawLine>(line) else { return Vec::new() };
    if let Some(ts) = raw.timestamp.as_deref().filter(|t| !t.is_empty()) {
        st.last_ts = ts.to_string();
    }
    let base = LedgerRecord {
        clone: clone.to_string(),
        session: raw
            .session_id
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| session.to_string()),
        // Claude Code stamps its lines and carries the stamp forward to the few that have none.
        // Cursor stamps none of them, so those get the time this pass read them, which keeps
        // `--since` working and is never off by more than one tail interval for a live clone.
        // Deliberately not written back into `last_ts`: it would freeze at the first line the
        // ledger ever saw and every later record would claim that time.
        ts: match st.last_ts.is_empty() {
            true => now.to_string(),
            false => st.last_ts.clone(),
        },
        agent_id: raw.agent_id.unwrap_or_default(),
        sidechain: raw.sidechain.unwrap_or(false),
        ..Default::default()
    };
    let emit = |kind: &str, text: String| LedgerRecord {
        kind: kind.to_string(),
        text,
        ..base.clone()
    };

    // Codex puts the record inside an envelope, so the envelope is what says which agent
    // wrote this line. Checked before the Claude Code arms because its outer `type` names the
    // envelope (`event_msg`, `response_item`) rather than anything those arms know.
    if let Some(payload) = raw.payload {
        return distill_codex(raw.kind.as_deref().unwrap_or_default(), payload, &emit);
    }

    match raw.kind.as_deref() {
        // The title the CLI keeps refining as the session goes on. It is rewritten on almost
        // every turn, so only a change is worth a record.
        Some("ai-title") => {
            let title = raw.ai_title.unwrap_or_default();
            if title.is_empty() || title == st.last_title {
                return Vec::new();
            }
            st.last_title = title.clone();
            vec![emit("title", title)]
        }
        // Compaction rewrites nothing: it appends a boundary and carries on in the same file. The
        // marker is what explains a sudden discontinuity to whoever reads the ledger later.
        Some("system") if raw.subtype.as_deref() == Some("compact_boundary") => {
            let meta = raw.compact.unwrap_or(CompactMeta {
                trigger: None,
                pre_tokens: None,
                post_tokens: None,
            });
            let text = format!(
                "conversation compacted ({}): {} tokens before, {} after",
                meta.trigger.unwrap_or_else(|| "unknown".into()),
                meta.pre_tokens.unwrap_or(0),
                meta.post_tokens.unwrap_or(0)
            );
            vec![emit("compact", text)]
        }
        Some(role @ ("user" | "assistant")) => {
            let Some(content) = raw.message.and_then(|m| m.content) else { return Vec::new() };
            distill_message(role, content, &emit)
        }
        _ => Vec::new(),
    }
}

/// The tool-call payloads Codex writes, each closed by an output carrying the same `call_id`.
///
/// Codex names a call after how the model addressed it. Reading only one of these would keep
/// the call and drop the answer, or the reverse.
const CODEX_TOOL_CALLS: [&str; 4] =
    ["custom_tool_call", "function_call", "local_shell_call", "web_search_call"];

/// The records one Codex rollout line contributes.
///
/// **Which records, and why so few.** A rollout writes the same turn twice: once as an
/// `event_msg`, which is what the operator saw, and once as a `response_item`, which is what
/// the model was sent. The second copy also carries the developer preamble, the workspace
/// dump and the base instructions, tens of thousands of characters repeated at every session
/// start. So the words come from `event_msg` and the tool calls from `response_item`, which is
/// the only place they appear, and nothing is recorded twice.
///
/// | Ledger kind | Codex record |
/// |---|---|
/// | `user` | `event_msg` / `user_message` |
/// | `assistant` | `event_msg` / `agent_message` |
/// | `toolUse` | `response_item` / `custom_tool_call`, `function_call`, `local_shell_call` |
/// | `toolResult` | the matching `…_call_output` |
///
/// Dropped for the same reason their Claude Code counterparts are: `reasoning`, which is the
/// model's scratch work and arrives as a wall of encrypted bytes, and `world_state`,
/// `turn_context` and `token_count`, which describe the request rather than the work.
fn distill_codex(
    envelope: &str,
    payload: serde_json::Value,
    emit: &impl Fn(&str, String) -> LedgerRecord,
) -> Vec<LedgerRecord> {
    let Some(kind) = payload.get("type").and_then(|v| v.as_str()) else { return Vec::new() };
    let text_at = |key: &str| {
        payload.get(key).and_then(|v| v.as_str()).unwrap_or_default().to_string()
    };
    match (envelope, kind) {
        ("event_msg", role @ ("user_message" | "agent_message")) => {
            let said = text_at("message");
            if said.is_empty() {
                return Vec::new();
            }
            let named = match role {
                "user_message" => "user",
                _ => "assistant",
            };
            vec![emit(named, clip(&said, MAX_TEXT_CHARS))]
        }
        ("response_item", call) if CODEX_TOOL_CALLS.contains(&call) => {
            // `input` on a custom tool call, `arguments` on a function call.
            let asked = match text_at("input") {
                s if s.is_empty() => text_at("arguments"),
                s => s,
            };
            let mut rec = emit("toolUse", clip(&asked, MAX_TOOL_INPUT_CHARS));
            rec.tool = text_at("name");
            rec.tool_id = text_at("call_id");
            vec![rec]
        }
        ("response_item", out) if out.ends_with("_call_output") => {
            let mut rec = emit("toolResult", tool_result_text(payload.get("output")));
            rec.tool_id = text_at("call_id");
            vec![rec]
        }
        _ => Vec::new(),
    }
}

/// The records one message's content contributes, in the order the blocks appear.
///
/// Text and image blocks accumulate into a single record for the message. A tool call and a tool
/// result each get their own, because each is independently worth finding.
fn distill_message(
    role: &str,
    content: serde_json::Value,
    emit: &impl Fn(&str, String) -> LedgerRecord,
) -> Vec<LedgerRecord> {
    if let Some(s) = content.as_str() {
        if s.is_empty() {
            return Vec::new();
        }
        return vec![emit(role, clip(s, MAX_TEXT_CHARS))];
    }
    let Ok(blocks) = serde_json::from_value::<Vec<Block>>(content) else { return Vec::new() };

    let mut out: Vec<LedgerRecord> = Vec::new();
    let mut said: Vec<String> = Vec::new();
    let flush = |said: &mut Vec<String>, out: &mut Vec<LedgerRecord>| {
        if !said.is_empty() {
            out.push(emit(role, clip(&said.join("\n"), MAX_TEXT_CHARS)));
            said.clear();
        }
    };

    for b in blocks {
        match b.kind.as_deref() {
            Some("text") => {
                if let Some(t) = b.text.filter(|t| !t.is_empty()) {
                    said.push(t);
                }
            }
            Some("image") | Some("input_image") => said.push(image_marker(&b)),
            // The model's own scratch work. It is the bulk of an assistant line and it describes
            // what was considered rather than what was done.
            Some("thinking") | Some("redacted_thinking") => {}
            Some("tool_use") => {
                flush(&mut said, &mut out);
                let input = b.input.map(|v| v.to_string()).unwrap_or_default();
                let mut rec = emit("toolUse", clip(&input, MAX_TOOL_INPUT_CHARS));
                rec.tool = b.name.unwrap_or_default();
                rec.tool_id = b.id.unwrap_or_default();
                out.push(rec);
            }
            Some("tool_result") => {
                flush(&mut said, &mut out);
                let mut rec = emit("toolResult", tool_result_text(b.content.as_ref()));
                rec.tool_id = b.tool_use_id.unwrap_or_default();
                out.push(rec);
            }
            _ => {}
        }
    }
    flush(&mut said, &mut out);
    out
}

// --- tailing -----------------------------------------------------------------------------------

/// One transcript file the pass will tail.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Transcript {
    path: PathBuf,
    /// This file's cursor key, unique within the clone. It has to stay stable for the life of the
    /// file: a key that changes re-reads the transcript from byte 0 and appends a second copy of
    /// everything already in the ledger.
    key: String,
    /// The ledger file the records land in. A subagent's records land in the session that spawned
    /// it, which is the directory its own file sits under, so one session reads as one story.
    name: String,
    /// This file is a subagent's, whatever its lines say about themselves.
    sidechain: bool,
    /// The subagent's id, from the file name. The lines carry it too, and theirs wins.
    agent: String,
}

/// The subagent id a transcript file name stands for: `agent-a1b2c3.jsonl` is `a1b2c3`.
fn agent_id_of(path: &Path) -> String {
    path.file_stem()
        .and_then(|s| s.to_str())
        .map(|s| s.strip_prefix("agent-").unwrap_or(s).to_string())
        .unwrap_or_default()
}

/// A directory entry's name, or `None` when it is not UTF-8.
fn entry_name(entry: &std::fs::DirEntry) -> Option<String> {
    entry.file_name().to_str().map(str::to_string)
}

/// Every subagent transcript under `dir/subagents`, filed against session `name`.
///
/// Both agents write a delegated task here, next to the transcript of the session that spawned it.
/// A directory with none, or one that is not a session directory at all, reads as empty.
fn subagent_files(dir: &Path, prefix: &str, name: &str, out: &mut Vec<Transcript>, cap: usize) {
    let Ok(files) = std::fs::read_dir(dir.join("subagents")) else { return };
    for f in files.flatten() {
        if out.len() >= cap {
            return;
        }
        let path = f.path();
        let Some(file) = entry_name(&f) else { continue };
        if !f.file_type().map(|t| t.is_file()).unwrap_or(false)
            || path.extension().is_none_or(|e| e != "jsonl")
        {
            continue;
        }
        out.push(Transcript {
            key: format!("{prefix}/subagents/{file}"),
            name: name.to_string(),
            agent: agent_id_of(&path),
            path,
            sidechain: true,
        });
    }
}

/// Every Claude Code transcript under `home`, at most `cap` of them.
///
/// One level below `.claude/projects` is the session the operator is talking to, and
/// `<session>/subagents/` beside it holds what that session delegated. `entry.file_type()` reports
/// a symlink as a symlink, so a symlinked directory is not descended into.
///
/// Main transcripts sort first, subagent ones after, and by path within each. That ordering is
/// what [`MAX_READ_PER_CLONE`] spends its budget in, so a clone whose backlog is mostly delegated
/// work still gets the operator's own conversation into the ledger on the first pass.
fn session_files(home: &Path, cap: usize) -> Vec<Transcript> {
    let root = home.join(".claude/projects");
    let mut out: Vec<Transcript> = Vec::new();
    let Ok(slugs) = std::fs::read_dir(&root) else { return out };
    'walk: for slug in slugs.flatten() {
        if !slug.file_type().map(|t| t.is_dir()).unwrap_or(false) {
            continue;
        }
        let Some(slug_name) = entry_name(&slug) else { continue };
        let Ok(entries) = std::fs::read_dir(slug.path()) else { continue };
        for e in entries.flatten() {
            if out.len() >= cap {
                break 'walk;
            }
            let path = e.path();
            let Ok(kind) = e.file_type() else { continue };
            let Some(file) = entry_name(&e) else { continue };
            let Some(name) = ledger_name(&path) else { continue };
            if kind.is_file() {
                if path.extension().is_some_and(|x| x == "jsonl") {
                    out.push(Transcript {
                        key: format!("{slug_name}/{file}"),
                        path,
                        name,
                        sidechain: false,
                        agent: String::new(),
                    });
                }
            } else if kind.is_dir() {
                // `<session>/subagents/`. A directory holding no such child (`memory/`, say)
                // contributes nothing.
                let prefix = format!("{slug_name}/{file}");
                subagent_files(&path, &prefix, &name, &mut out, cap);
            }
        }
    }
    if out.len() >= cap {
        tracing::warn!(
            target: "ledger",
            "{} holds more than {cap} transcripts; the rest are not being tailed",
            root.display()
        );
    }
    out.sort_unstable_by(|a, b| (a.sidechain, &a.path).cmp(&(b.sidechain, &b.path)));
    out
}

/// Every Cursor transcript under `home`, at most `cap` of them.
///
/// Cursor keeps one directory per conversation and names the transcript after it:
/// `~/.cursor/projects/<workspace>/agent-transcripts/<conversation>/<conversation>.jsonl`, with
/// that conversation's delegated work in `subagents/` beside it. Same two levels as Claude Code
/// under different names, and kept the same way, so a search reads one corpus rather than two.
fn cursor_session_files(home: &Path, cap: usize) -> Vec<Transcript> {
    let root = home.join(".cursor/projects");
    let mut out: Vec<Transcript> = Vec::new();
    let Ok(workspaces) = std::fs::read_dir(&root) else { return out };
    'walk: for ws in workspaces.flatten() {
        if !ws.file_type().map(|t| t.is_dir()).unwrap_or(false) {
            continue;
        }
        let Ok(convs) = std::fs::read_dir(ws.path().join("agent-transcripts")) else { continue };
        for conv in convs.flatten() {
            if out.len() >= cap {
                break 'walk;
            }
            if !conv.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                continue;
            }
            let Some(id) = entry_name(&conv) else { continue };
            let dir = conv.path();
            let path = dir.join(format!("{id}.jsonl"));
            let Some(name) = ledger_name(&path) else { continue };
            if path.is_file() {
                out.push(Transcript {
                    key: format!("{id}/{id}.jsonl"),
                    path,
                    name: name.clone(),
                    sidechain: false,
                    agent: String::new(),
                });
            }
            subagent_files(&dir, &id, &name, &mut out, cap);
        }
    }
    out.sort_unstable_by(|a, b| (a.sidechain, &a.path).cmp(&(b.sidechain, &b.path)));
    out
}

/// Every Codex transcript under `home`, at most `cap` of them.
///
/// Codex calls a transcript a rollout and files one per session under a dated path:
/// `~/.codex/sessions/2026/08/08/rollout-2026-08-08T02-59-54-<session>.jsonl`. The date levels
/// carry nothing this module needs, so they are walked and thrown away, and the file name is
/// what names the session.
///
/// Codex has no `subagents/` directory. A thread it spawns gets a rollout of its own beside
/// every other, and the only record of which thread spawned it lives in `~/.codex/state_5.
/// sqlite`, not in either file. So a spawned thread is filed as its own session here rather
/// than under a parent this module cannot see, and `sidechain` is set from the rollout's own
/// `thread_source` instead (see [`session_files`] for what the Claude Code side can do with a
/// directory to read it out of).
fn codex_session_files(home: &Path, cap: usize) -> Vec<Transcript> {
    let root = home.join(".codex/sessions");
    let mut out: Vec<Transcript> = Vec::new();
    // Three dated levels, so a bounded walk rather than a recursive one.
    let mut stack = vec![root];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else { continue };
        for e in entries.flatten() {
            if out.len() >= cap {
                out.sort_unstable_by(|a, b| a.path.cmp(&b.path));
                return out;
            }
            let path = e.path();
            let Ok(kind) = e.file_type() else { continue };
            if kind.is_dir() {
                stack.push(path);
                continue;
            }
            let Some(file) = entry_name(&e) else { continue };
            if !kind.is_file() || !file.starts_with("rollout-") || !file.ends_with(".jsonl") {
                continue;
            }
            let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else { continue };
            let Some(id) = crate::stuck::codex_session_id(stem) else { continue };
            out.push(Transcript {
                // The dated directories are part of the key: two days can hold a file of the
                // same name only if Codex reuses a session id, and the key must not merge them.
                key: match path.parent().and_then(|p| p.strip_prefix(home).ok()) {
                    Some(rel) => format!("{}/{file}", rel.display()),
                    None => file,
                },
                name: format!("{id}.ndjson"),
                path,
                sidechain: false,
                agent: String::new(),
            });
        }
    }
    out.sort_unstable_by(|a, b| a.path.cmp(&b.path));
    out
}

/// Append the ledger lines for whatever has been added to `src` since its cursor, returning the
/// bytes of transcript consumed.
///
/// The cursor advances only after the append succeeds, so a full disk costs a retry rather than a
/// hole. Only whole lines are consumed: a partial trailing line means the CLI is mid-append, and
/// it is read complete on the next pass.
fn tail_file(clone: &str, t: &Transcript, dir: &Path, offsets: &mut Offsets) -> u64 {
    use std::os::unix::fs::MetadataExt;
    let src = t.path.as_path();
    let name = t.name.clone();
    let key = t.key.clone();
    let Ok(meta) = std::fs::metadata(src) else { return 0 };
    // Regular files only. A clone can point one of these paths at a FIFO, and opening a FIFO with
    // no writer blocks forever inside the serial pass.
    if !meta.is_file() {
        return 0;
    }
    let inode = meta.ino();
    let len = meta.len();

    let mut st = match offsets.get(&key) {
        Some(prev) if prev.inode == inode && prev.offset <= len => prev.clone(),
        // Replaced or truncated: nothing about the old cursor describes this file.
        _ => FileState { inode, offset: 0, ..Default::default() },
    };
    st.inode = inode;
    if st.offset >= len {
        offsets.insert(key, st);
        return 0;
    }

    let Ok(mut file) = std::fs::File::open(src) else { return 0 };
    if file.seek(SeekFrom::Start(st.offset)).is_err() {
        return 0;
    }
    let want = (len - st.offset).min(MAX_READ_PER_FILE);
    let mut buf: Vec<u8> = Vec::new();
    // `take` plus `read_to_end` rather than one `read`, which is free to come back short.
    if file.take(want).read_to_end(&mut buf).is_err() {
        return 0;
    }

    let Some(last_nl) = buf.iter().rposition(|b| *b == b'\n') else {
        // No line break in a full window means there is no terminator within the cap, and leaving
        // the cursor put would re-read the same eight megabytes forever. Skip the region instead.
        if want >= MAX_READ_PER_FILE {
            tracing::warn!(
                target: "ledger",
                "no line break in {want} bytes of {}; skipping the region",
                src.display()
            );
            st.offset += want;
            offsets.insert(key, st);
            return want;
        }
        return 0;
    };
    let consumed = last_nl + 1;

    let session = name.trim_end_matches(".ndjson").to_string();
    // One stamp for the pass, for the lines that carry none of their own. See `distill`.
    let now = crate::docker::epoch_to_rfc3339(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs() as i64),
    );
    let mut body = String::new();
    for line in buf[..consumed].split(|b| *b == b'\n') {
        let Ok(text) = std::str::from_utf8(line) else { continue };
        if text.trim().is_empty() {
            continue;
        }
        for mut rec in distill(clone, &session, text, &mut st, &now) {
            // Where the file sits is the authority on whose turn this is. A subagent transcript
            // carries `isSidechain` on every line, but nothing forces it to, and the file name is
            // the only place the agent id is guaranteed.
            if t.sidechain {
                rec.sidechain = true;
                if rec.agent_id.is_empty() {
                    rec.agent_id.clone_from(&t.agent);
                }
            }
            if let Ok(json) = serde_json::to_string(&rec) {
                body.push_str(&json);
                body.push('\n');
            }
        }
    }

    if !body.is_empty() {
        if let Err(e) = append(&dir.join(&name), body.as_bytes()) {
            tracing::warn!(target: "ledger", "appending to {}/{name}: {e}", dir.display());
            return 0; // Cursor stays put, so the same bytes are distilled again next pass.
        }
    }
    st.offset += consumed as u64;
    offsets.insert(key, st);
    consumed as u64
}

fn append(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let mut f = std::fs::OpenOptions::new().create(true).append(true).open(path)?;
    f.write_all(bytes)
}

/// One clone's tail pass. Creates the clone's ledger directory whether or not it has transcripts,
/// because that directory is what reserves the name (see [`reserved_names`]).
fn tail_clone(clone: &str, home: &Path, dir: &Path) {
    if let Err(e) = std::fs::create_dir_all(dir) {
        tracing::warn!(target: "ledger", "creating {}: {e}", dir.display());
        return;
    }
    if !home.is_dir() {
        return; // Stopped, archived, or still booting. The name stays reserved either way.
    }
    let before = load_offsets(dir);
    let mut offsets = before.clone();
    let mut budget = MAX_READ_PER_CLONE;
    let claude = session_files(home, MAX_SESSION_FILES);
    let mut left = MAX_SESSION_FILES.saturating_sub(claude.len());
    let cursor = cursor_session_files(home, left);
    left = left.saturating_sub(cursor.len());
    let codex = codex_session_files(home, left);
    for src in claude.iter().chain(cursor.iter()).chain(codex.iter()) {
        if budget == 0 {
            break; // Backlog drains over the next few passes.
        }
        budget = budget.saturating_sub(tail_file(clone, src, dir, &mut offsets));
    }
    if offsets != before {
        if let Err(e) = save_offsets(dir, &offsets) {
            // The cursors are lost, so the next pass re-reads and appends a second copy of
            // whatever this one wrote. Say so rather than letting it look like a distiller bug.
            tracing::warn!(
                target: "ledger",
                "saving {} cursors: {e} (the next pass will re-append what this one wrote)",
                clone
            );
        }
    }
}

/// Tail one clone now, off the timer.
///
/// Called at the start of a delete and an archive. A container's home symlink disappears the
/// moment it stops, so anything not captured while the clone runs is gone for good, and a worker
/// clone can delete itself seconds after the last correction is typed. The pass is awaited rather
/// than spawned for exactly that reason: spawning it would race the teardown it exists to beat.
/// It never fails the caller and it is bounded by [`CLONE_TAIL_TIMEOUT`].
pub async fn tail_once(app: &App, clone: &str) {
    let cfg = app.config();
    let Some(dir) = clone_dir(&cfg.data_dir, clone) else { return };
    let home = crate::homes::hosts_root(&cfg.data_dir).join(clone);
    let id = clone.to_string();
    let work = tokio::task::spawn_blocking(move || tail_clone(&id, &home, &dir));
    if tokio::time::timeout(CLONE_TAIL_TIMEOUT, work).await.is_err() {
        tracing::warn!(target: "ledger", "final tail of {clone} timed out");
    }
}

/// One pass across the fleet.
async fn tail_fleet(app: &App) {
    let cfg = app.config();
    let root = crate::homes::hosts_root(&cfg.data_dir);
    let hosts: Vec<String> = app
        .store
        .get()
        .hosts
        .into_iter()
        .filter(|h| h.managed && !h.archived)
        .map(|h| h.id)
        .collect();
    for id in hosts {
        let Some(dir) = clone_dir(&cfg.data_dir, &id) else { continue };
        let home = root.join(&id);
        let name = id.clone();
        let work = tokio::task::spawn_blocking(move || tail_clone(&name, &home, &dir));
        match tokio::time::timeout(CLONE_TAIL_TIMEOUT, work).await {
            Ok(Ok(())) => {}
            Ok(Err(e)) => tracing::warn!(target: "ledger", "tail of {id} panicked: {e}"),
            Err(_) => tracing::warn!(target: "ledger", "tail of {id} timed out"),
        }
    }
}

/// The background tailer, spawned once at startup after [`crate::homes`].
pub async fn run(app: App) {
    tracing::info!("transcript ledger tailer started (every {}s)", TAIL_INTERVAL.as_secs());
    loop {
        tail_fleet(&app).await;
        tokio::time::sleep(TAIL_INTERVAL).await;
    }
}

// --- querying ------------------------------------------------------------------------------

/// What to look for. `pattern` is a case-insensitive substring, matched against the whole ledger
/// line, so it reaches the text, the tool name and the kind alike.
#[derive(Debug, Clone, Default)]
pub struct SearchQuery {
    pub pattern: String,
    /// One clone, or every clone when absent.
    pub clone: Option<String>,
    /// Epoch milliseconds. A record whose timestamp cannot be parsed passes both bounds, because
    /// dropping it would hide real work over a formatting detail.
    pub since_ms: Option<i64>,
    pub until_ms: Option<i64>,
    /// `Some(true)` keeps only subagent turns, `Some(false)` only the conversation. Absent keeps
    /// both, which on a delegating session is mostly subagents.
    pub sidechain: Option<bool>,
    /// One subagent's id, as the record's `agentId`. Reads back a single delegated task.
    pub agent: Option<String>,
    pub limit: usize,
}

/// Epoch milliseconds for an RFC3339 stamp, sharing the account pollers' parser.
fn ts_ms(ts: &str) -> Option<i64> {
    crate::claude::parse_rfc3339_utc_secs(ts).map(|secs| secs * 1000)
}

/// The ledger files to search, newest first.
///
/// Newest first is what makes a hit limit useful: the answer to "how did we do this" is usually
/// the most recent time it was done, and a limit that cut oldest-first would bury it.
fn search_files(data_dir: &str, clone: Option<&str>) -> Vec<(String, PathBuf)> {
    let mut clones: Vec<String> = match clone {
        Some(id) if crate::files::is_safe_id(id) => vec![id.to_string()],
        Some(_) => return Vec::new(),
        None => reserved_names(data_dir).into_iter().collect(),
    };
    clones.sort_unstable();
    let mut dated: Vec<(std::time::SystemTime, String, PathBuf)> = Vec::new();
    for id in clones {
        let Some(dir) = clone_dir(data_dir, &id) else { continue };
        let Ok(rd) = std::fs::read_dir(&dir) else { continue };
        for entry in rd.flatten() {
            let path = entry.path();
            if path.extension().is_some_and(|e| e == "ndjson") {
                let at = entry
                    .metadata()
                    .and_then(|m| m.modified())
                    .unwrap_or(std::time::UNIX_EPOCH);
                dated.push((at, id.clone(), path));
            }
        }
    }
    dated.sort_unstable_by(|a, b| b.0.cmp(&a.0).then_with(|| a.2.cmp(&b.2)));
    dated.into_iter().map(|(_, id, path)| (id, path)).collect()
}

/// Search the ledger, server-side, so a caller gets matches back instead of a corpus.
pub fn search(data_dir: &str, q: &SearchQuery) -> SearchResult {
    let limit = q.limit.clamp(1, MAX_SEARCH_LIMIT);
    let needle = q.pattern.to_lowercase();
    let mut hits: Vec<Hit> = Vec::new();
    let mut scanned: u64 = 0;
    let mut truncated = false;

    'files: for (clone, path) in search_files(data_dir, q.clone.as_deref()) {
        let Ok(file) = std::fs::File::open(&path) else { continue };
        let mut reader = BufReader::new(file);
        let mut offset: u64 = 0;
        let mut raw: Vec<u8> = Vec::new();
        loop {
            raw.clear();
            let Ok(read) = reader.read_until(b'\n', &mut raw) else { continue 'files };
            if read == 0 {
                break;
            }
            let start = offset;
            offset += read as u64;
            scanned += read as u64;
            if scanned >= MAX_SEARCH_BYTES {
                truncated = true;
                break 'files;
            }
            // Longer than anything this module writes, so the file was hand-edited or corrupted.
            if read > MAX_LINE_BYTES {
                continue;
            }
            let Ok(text) = std::str::from_utf8(&raw) else { continue };
            if !text.to_lowercase().contains(&needle) {
                continue;
            }
            let Ok(rec) = serde_json::from_str::<LedgerRecord>(text.trim_end()) else { continue };
            if q.sidechain.is_some_and(|want| rec.sidechain != want) {
                continue;
            }
            if q.agent.as_ref().is_some_and(|id| rec.agent_id != *id) {
                continue;
            }
            let at = ts_ms(&rec.ts);
            if let (Some(at), Some(since)) = (at, q.since_ms) {
                if at < since {
                    continue;
                }
            }
            if let (Some(at), Some(until)) = (at, q.until_ms) {
                if at > until {
                    continue;
                }
            }
            hits.push(Hit {
                clone: clone.clone(),
                session: rec.session,
                ts: rec.ts,
                kind: rec.kind,
                offset: start,
                len: read as u64,
                line: text.trim_end().to_string(),
            });
            if hits.len() >= limit {
                truncated = true;
                break 'files;
            }
        }
    }

    // Newest first across the whole result, not merely across the files it came from.
    hits.sort_by(|a, b| b.ts.cmp(&a.ts));
    SearchResult { hits, scanned_bytes: scanned, truncated }
}

/// Read `len` bytes of `<clone>/<session>.ndjson` from `offset`, snapped outward to whole lines.
pub fn read_range(
    data_dir: &str,
    clone: &str,
    session: &str,
    offset: u64,
    len: u64,
) -> Result<Range, String> {
    let Some(dir) = clone_dir(data_dir, clone) else { return Err(format!("invalid clone '{clone}'")) };
    let Some(name) = ledger_name(Path::new(session)) else {
        return Err(format!("invalid session '{session}'"));
    };
    let path = dir.join(&name);
    let meta = std::fs::metadata(&path).map_err(|_| format!("no ledger for {clone}/{session}"))?;
    let size = meta.len();
    if offset >= size {
        return Ok(Range {
            clone: clone.to_string(),
            session: session.to_string(),
            offset: size,
            len: 0,
            size,
            text: String::new(),
        });
    }

    let want = len.clamp(1, MAX_READ_RANGE);
    // Snap the start back to the previous line break, and the end forward past the next one, so
    // the caller always gets parseable NDJSON. Both snaps are bounded by one line.
    let head = offset.saturating_sub(MAX_LINE_BYTES as u64);
    let tail = (offset + want + MAX_LINE_BYTES as u64).min(size);
    let mut file = std::fs::File::open(&path).map_err(|e| e.to_string())?;
    file.seek(SeekFrom::Start(head)).map_err(|e| e.to_string())?;
    let mut buf: Vec<u8> = Vec::new();
    file.take(tail - head).read_to_end(&mut buf).map_err(|e| e.to_string())?;

    let rel_start = (offset - head) as usize;
    let start = match buf[..rel_start.min(buf.len())].iter().rposition(|b| *b == b'\n') {
        Some(nl) => nl + 1,
        None if head == 0 => 0,
        // The window opened mid-line and found no break: give up on the leading fragment.
        None => rel_start.min(buf.len()),
    };
    let rel_end = ((offset + want - head) as usize).min(buf.len());
    // A range that already stops on a line boundary is complete. Extending it anyway would hand
    // back one more line than was asked for, which turns a one-line read of a hit into two.
    let end = if rel_end == 0 || buf.get(rel_end - 1) == Some(&b'\n') {
        rel_end
    } else {
        match buf[rel_end..].iter().position(|b| *b == b'\n') {
            Some(nl) => rel_end + nl + 1,
            None => buf.len(),
        }
    };
    let slice = &buf[start.min(end)..end];
    Ok(Range {
        clone: clone.to_string(),
        session: session.to_string(),
        offset: head + start as u64,
        len: slice.len() as u64,
        size,
        text: String::from_utf8_lossy(slice).into_owned(),
    })
}

/// The default hit limit, for a caller that names none.
pub fn default_limit() -> usize {
    DEFAULT_SEARCH_LIMIT
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The stamp a line with no timestamp of its own gets: the moment the ledger read it.
    const READ_AT: &str = "2026-08-04T23:30:00Z";

    #[test]
    fn a_cursor_message_distills_the_same_way_a_claude_one_does() {
        // Cursor names the line `role` where Claude Code names it `type`, and stamps nothing.
        // Below that the two agree: the content blocks are the same shape.
        let mut st = FileState::default();
        let line = r#"{"role":"assistant","message":{"content":[
            {"type":"text","text":"Done. The dropdown is gone."},
            {"type":"tool_use","name":"Grep","input":{"pattern":"RegisteredDoctorSelect"}}]}}"#;
        let recs = distill("pega-dev-262", "e52bd0c6", line, &mut st, READ_AT);
        assert_eq!(recs.len(), 2);
        assert_eq!(recs[0].kind, "assistant");
        assert_eq!(recs[0].text, "Done. The dropdown is gone.");
        assert_eq!(recs[1].kind, "toolUse");
        // The transcript names no session and no time, so it takes the file's own id and the
        // moment the pass read it.
        assert_eq!(recs[0].session, "e52bd0c6");
        assert_eq!(recs[0].ts, READ_AT);
    }

    #[test]
    fn a_cursor_turn_boundary_contributes_nothing() {
        // `{"type":"turn_ended"}` is the one Cursor line that does carry a `type`. It says
        // nothing the surrounding records do not already say.
        let mut st = FileState::default();
        let line = r#"{"type":"turn_ended","status":"success"}"#;
        assert!(distill("c", "s1", line, &mut st, READ_AT).is_empty());
    }

    #[test]
    fn a_claude_line_still_wins_its_own_timestamp() {
        // The read-time fallback must never displace a stamp the transcript supplies, nor the
        // one carried forward to the lines that have none.
        let mut st = FileState::default();
        let stamped = r#"{"type":"user","timestamp":"2026-08-04T09:00:00Z",
            "message":{"content":"hello"}}"#;
        let recs = distill("c", "s1", stamped, &mut st, READ_AT);
        assert_eq!(recs[0].ts, "2026-08-04T09:00:00Z");
        let untimed = r#"{"type":"ai-title","aiTitle":"a title"}"#;
        let recs = distill("c", "s1", untimed, &mut st, READ_AT);
        assert_eq!(recs[0].ts, "2026-08-04T09:00:00Z", "carried, not the read time");
    }

    #[test]
    fn cursor_transcripts_and_their_subagents_are_both_found() {
        let home = std::env::temp_dir().join(format!("rmng-ledger-cursor-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        let conv = home.join(".cursor/projects/home-rmng-Dev/agent-transcripts/e52bd0c6");
        std::fs::create_dir_all(conv.join("subagents")).unwrap();
        std::fs::write(conv.join("e52bd0c6.jsonl"), "{}\n").unwrap();
        std::fs::write(conv.join("subagents/853bf50b.jsonl"), "{}\n").unwrap();
        // A directory whose transcript has not been written yet still gives up its subagents.
        std::fs::create_dir_all(
            home.join(".cursor/projects/home-rmng-Dev/agent-transcripts/empty"),
        )
        .unwrap();

        let found = cursor_session_files(&home, 4096);
        let paths: Vec<&Path> = found.iter().map(|t| t.path.as_path()).collect();
        assert_eq!(paths, vec![conv.join("e52bd0c6.jsonl"), conv.join("subagents/853bf50b.jsonl")]);
        // The conversation first, and the subagent filed under the conversation it belongs to.
        assert!(!found[0].sidechain && found[1].sidechain);
        assert!(found.iter().all(|t| t.name == "e52bd0c6.ndjson"));
        assert_eq!(found[1].agent, "853bf50b");
        assert!(cursor_session_files(&home, 0).is_empty(), "the cap is respected");
        let _ = std::fs::remove_dir_all(&home);
    }

    /// A Codex rollout, at the dated path the CLI files one under.
    fn write_rollout(home: &Path, day: &str, id: &str, lines: &[&str]) -> PathBuf {
        let dir = home.join(".codex/sessions").join(day);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(format!("rollout-2026-08-08T02-59-54-{id}.jsonl"));
        let mut body = String::new();
        for l in lines {
            body.push_str(l);
            body.push('\n');
        }
        std::fs::write(&path, body).unwrap();
        path
    }

    #[test]
    fn a_codex_rollout_is_found_by_its_session_id_under_its_dated_path() {
        let root = temp_dir("codex-walk");
        let home = root.join("home");
        let id = "019fe02b-cb2c-7ec0-8a48-77d87c7f057f";
        write_rollout(&home, "2026/08/08", id, &["{}"]);
        // Codex writes other things beside the rollouts, and none of them are transcripts.
        std::fs::write(home.join(".codex/sessions/2026/08/08/notes.jsonl"), "{}\n").unwrap();

        let found = codex_session_files(&home, MAX_SESSION_FILES);
        assert_eq!(found.len(), 1, "{found:#?}");
        assert_eq!(found[0].name, format!("{id}.ndjson"), "the ledger file is the session");
        // The dated directories are in the key, so one id reused on two days stays two files.
        assert_eq!(
            found[0].key,
            format!(".codex/sessions/2026/08/08/rollout-2026-08-08T02-59-54-{id}.jsonl")
        );
        assert!(!found[0].sidechain);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_codex_turn_distills_to_the_same_records_a_claude_one_does() {
        // Verbatim shapes from Codex CLI 0.144.4, clone `claude-test2`, 2026-08-08.
        let mut st = FileState::default();
        let say = |line: &str| {
            let mut st2 = FileState::default();
            distill("c", "019fe02b", line, &mut st2, READ_AT)
        };

        let asked = r#"{"timestamp":"2026-08-08T07:00:15.962Z","type":"event_msg","payload":{"type":"user_message","message":"Run the shell command: echo PROBE-MARKER-ALPHA.","images":[]}}"#;
        let recs = distill("c", "019fe02b", asked, &mut st, READ_AT);
        assert_eq!(recs.len(), 1);
        assert_eq!(recs[0].kind, "user");
        assert_eq!(recs[0].session, "019fe02b", "the file names the session, no line does");
        assert_eq!(recs[0].ts, "2026-08-08T07:00:15.962Z");
        assert!(recs[0].text.starts_with("Run the shell command"));

        let said = r#"{"timestamp":"2026-08-08T07:02:21.177Z","type":"event_msg","payload":{"type":"agent_message","message":"done","phase":"final_answer"}}"#;
        assert_eq!(say(said)[0].kind, "assistant");
        assert_eq!(say(said)[0].text, "done");

        let call = r#"{"timestamp":"2026-08-08T07:00:20.293Z","type":"response_item","payload":{"type":"custom_tool_call","status":"completed","call_id":"call_7q2","name":"exec","input":"const r = await tools.exec_command({\"cmd\":\"echo PROBE-MARKER-ALPHA\"})"}}"#;
        let recs = say(call);
        assert_eq!(recs[0].kind, "toolUse");
        assert_eq!(recs[0].tool, "exec");
        assert_eq!(recs[0].tool_id, "call_7q2");
        assert!(recs[0].text.contains("PROBE-MARKER-ALPHA"));

        // A function call names its arguments differently and pairs the same way.
        let func = r#"{"timestamp":"2026-08-08T07:02:19.697Z","type":"response_item","payload":{"type":"function_call","name":"wait","arguments":"{\"cell_id\":\"2\"}","call_id":"call_1JS"}}"#;
        assert_eq!(say(func)[0].tool, "wait");
        assert!(say(func)[0].text.contains("cell_id"));

        // An output arrives as a string on one call and as blocks on another. Both are text.
        let out_blocks = r#"{"timestamp":"2026-08-08T07:02:19.712Z","type":"response_item","payload":{"type":"function_call_output","call_id":"call_1JS","output":[{"type":"input_text","text":"Script completed"},{"type":"input_text","text":"PROBE-MARKER-ALPHA"}]}}"#;
        let recs = say(out_blocks);
        assert_eq!(recs[0].kind, "toolResult");
        assert_eq!(recs[0].tool_id, "call_1JS");
        assert_eq!(recs[0].text, "Script completed\nPROBE-MARKER-ALPHA");
        let out_string = r#"{"timestamp":"2026-08-08T07:02:16.117Z","type":"response_item","payload":{"type":"custom_tool_call_output","call_id":"call_eDx","output":"Wall time 111.7 seconds"}}"#;
        assert_eq!(say(out_string)[0].text, "Wall time 111.7 seconds");
    }

    #[test]
    fn what_a_codex_rollout_repeats_or_never_meant_is_dropped() {
        let mut st = FileState::default();
        let gone = |line: &str| distill("c", "s", line, &mut FileState::default(), READ_AT);

        // The model's scratch work, which arrives as a wall of encrypted bytes.
        let thinking = r#"{"timestamp":"2026-08-08T07:02:19.267Z","type":"response_item","payload":{"type":"reasoning","id":"rs_0d3","summary":[],"encrypted_content":"gAAAAABqdtR7ybEdlyw8INXHCedhkhRJ"}}"#;
        assert!(gone(thinking).is_empty());
        assert!(!format!("{:?}", gone(thinking)).contains("gAAAAAB"));

        // The second copy of every message, the one the model was sent. It repeats what
        // `event_msg` already said and carries the developer preamble besides.
        let echoed = r#"{"timestamp":"2026-08-08T07:00:15.920Z","type":"response_item","payload":{"type":"message","role":"developer","content":[{"type":"input_text","text":"<permissions instructions> …"}]}}"#;
        assert!(gone(echoed).is_empty());
        let assistant_copy = r#"{"timestamp":"2026-08-08T07:02:21.179Z","type":"response_item","payload":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"done"}]}}"#;
        assert!(gone(assistant_copy).is_empty());

        // Records about the request rather than about the work.
        for line in [
            r#"{"timestamp":"2026-08-08T07:00:15.826Z","type":"session_meta","payload":{"session_id":"019fe02b","cwd":"/","originator":"codex-tui"}}"#,
            r#"{"timestamp":"2026-08-08T07:00:15.922Z","type":"world_state","payload":{"full":true,"state":{"agents_md":{"text":"…"}}}}"#,
            r#"{"timestamp":"2026-08-08T07:00:15.922Z","type":"turn_context","payload":{"turn_id":"019fe02c","cwd":"/"}}"#,
            r#"{"timestamp":"2026-08-08T07:00:20.635Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"total_tokens":18621}}}}"#,
            r#"{"timestamp":"2026-08-08T07:00:15.828Z","type":"event_msg","payload":{"type":"task_started","turn_id":"019fe02c"}}"#,
        ] {
            assert!(distill("c", "s", line, &mut st, READ_AT).is_empty(), "kept {line}");
        }
    }

    #[test]
    fn a_codex_image_leaves_its_type_and_not_its_bytes() {
        // Codex writes one `data:` URI where Claude Code writes a typed block. Both have to
        // leave the same thing behind, which is the fact that a picture was there.
        let payload = "A".repeat(400_000);
        let line = format!(
            r#"{{"timestamp":"2026-08-08T07:00:20.635Z","type":"response_item","payload":{{"type":"custom_tool_call_output","call_id":"c1","output":[{{"type":"input_image","image_url":"data:image/png;base64,{payload}"}}]}}}}"#
        );
        let recs = distill("c", "s", &line, &mut FileState::default(), READ_AT);
        assert_eq!(recs[0].kind, "toolResult");
        assert_eq!(recs[0].text, "[image/png, 400000 base64 bytes dropped]");
        assert!(!recs[0].text.contains("AAAA"));
    }

    #[test]
    fn a_codex_session_tails_into_one_ledger_file_named_for_it() {
        let root = temp_dir("codex-tail");
        let home = root.join("home");
        let dir = root.join("ledger/c1");
        let id = "019fe02b-cb2c-7ec0-8a48-77d87c7f057f";
        let src = write_rollout(
            &home,
            "2026/08/08",
            id,
            &[
                r#"{"timestamp":"2026-08-08T07:00:15.962Z","type":"event_msg","payload":{"type":"user_message","message":"map the encoder"}}"#,
            ],
        );
        tail_clone("c1", &home, &dir);
        let out = dir.join(format!("{id}.ndjson"));
        let body = std::fs::read_to_string(&out).unwrap();
        assert_eq!(body.lines().count(), 1);
        assert!(body.contains("map the encoder"));

        // The tail resumes rather than repeating, the same as every other transcript.
        append(
            &src,
            br#"{"timestamp":"2026-08-08T07:02:21.177Z","type":"event_msg","payload":{"type":"agent_message","message":"the encoder is at src/enc.rs:40"}}"#,
        )
        .unwrap();
        append(&src, b"\n").unwrap();
        tail_clone("c1", &home, &dir);
        let body = std::fs::read_to_string(&out).unwrap();
        assert_eq!(body.lines().count(), 2);
        assert!(body.contains("src/enc.rs:40"));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_claude_session_gives_up_its_transcript_and_its_subagents() {
        let root = temp_dir("walk");
        let home = root.join("home");
        write_transcript(&home, "-home-rmng-RMNG", "aaaa-bbbb", &["{}"]);
        let subs = home.join(".claude/projects/-home-rmng-RMNG/aaaa-bbbb/subagents");
        std::fs::create_dir_all(&subs).unwrap();
        std::fs::write(subs.join("agent-a18ea2842ca67cf6e.jsonl"), "{}\n").unwrap();
        std::fs::write(subs.join("notes.txt"), "not a transcript\n").unwrap();
        // A sibling directory that is not a session, which is what `~/.claude/projects/<slug>`
        // holds beside its sessions.
        std::fs::create_dir_all(home.join(".claude/projects/-home-rmng-RMNG/memory")).unwrap();

        let found = session_files(&home, MAX_SESSION_FILES);
        assert_eq!(found.len(), 2, "{found:#?}");
        // The conversation first: it is what a bounded pass must not leave waiting.
        assert_eq!(found[0].key, "-home-rmng-RMNG/aaaa-bbbb.jsonl");
        assert!(!found[0].sidechain);
        assert_eq!(found[1].key, "-home-rmng-RMNG/aaaa-bbbb/subagents/agent-a18ea2842ca67cf6e.jsonl");
        assert!(found[1].sidechain);
        assert_eq!(found[1].agent, "a18ea2842ca67cf6e");
        // Both land in the session's own ledger file.
        assert!(found.iter().all(|t| t.name == "aaaa-bbbb.ndjson"));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_subagents_turn_is_filed_under_the_session_that_spawned_it() {
        let root = temp_dir("sidechain");
        let home = root.join("home");
        let dir = root.join("ledger/c1");
        write_transcript(
            &home,
            "p",
            "sess",
            &[r#"{"type":"user","sessionId":"sess","timestamp":"2026-08-07T10:00:00.000Z","message":{"content":"review the diff"}}"#],
        );
        let subs = home.join(".claude/projects/p/sess/subagents");
        std::fs::create_dir_all(&subs).unwrap();
        // The file's own lines name the session that spawned it, not the subagent.
        std::fs::write(
            subs.join("agent-a1d14cec3da0aa973.jsonl"),
            "{\"type\":\"assistant\",\"sessionId\":\"sess\",\"isSidechain\":true,\"agentId\":\"a1d14cec3da0aa973\",\"timestamp\":\"2026-08-07T10:01:00.000Z\",\"message\":{\"content\":[{\"type\":\"text\",\"text\":\"the encoder path is fine\"}]}}\n",
        )
        .unwrap();
        // A line that forgot to flag itself is still a subagent's: the file says so.
        std::fs::write(
            subs.join("agent-unflagged.jsonl"),
            "{\"type\":\"assistant\",\"sessionId\":\"sess\",\"timestamp\":\"2026-08-07T10:02:00.000Z\",\"message\":{\"content\":[{\"type\":\"text\",\"text\":\"second opinion\"}]}}\n",
        )
        .unwrap();

        tail_clone("c1", &home, &dir);
        let body = std::fs::read_to_string(dir.join("sess.ndjson")).unwrap();
        let recs: Vec<LedgerRecord> =
            body.lines().map(|l| serde_json::from_str(l).unwrap()).collect();
        assert_eq!(recs.len(), 3, "{body}");
        assert!(recs.iter().all(|r| r.session == "sess"));
        assert!(!recs[0].sidechain, "the operator's own turn");

        let review = recs.iter().find(|r| r.text.contains("encoder path")).unwrap();
        assert!(review.sidechain);
        assert_eq!(review.agent_id, "a1d14cec3da0aa973");

        let second = recs.iter().find(|r| r.text.contains("second opinion")).unwrap();
        assert!(second.sidechain, "the file's location decides, not the line");
        assert_eq!(second.agent_id, "unflagged", "the id falls back to the file name");

        // A second pass repeats nothing, which is the cursor keying by full path working.
        tail_clone("c1", &home, &dir);
        assert_eq!(std::fs::read_to_string(dir.join("sess.ndjson")).unwrap().lines().count(), 3);
        let _ = std::fs::remove_dir_all(&root);
    }

    fn temp_dir(tag: &str) -> PathBuf {
        use std::sync::atomic::{AtomicU32, Ordering};
        static N: AtomicU32 = AtomicU32::new(0);
        let dir = std::env::temp_dir().join(format!(
            "rmng-ledger-{tag}-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn write_transcript(home: &Path, slug: &str, session: &str, lines: &[&str]) -> PathBuf {
        let dir = home.join(".claude/projects").join(slug);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(format!("{session}.jsonl"));
        let mut body = String::new();
        for l in lines {
            body.push_str(l);
            body.push('\n');
        }
        std::fs::write(&path, body).unwrap();
        path
    }

    #[test]
    fn clip_cuts_on_a_char_boundary_and_says_how_much_it_dropped() {
        assert_eq!(clip("hello", 10), "hello");
        assert_eq!(clip("hello", 5), "hello");
        // Multi-byte input: cutting at byte 3 would split the character.
        assert_eq!(clip("héllo", 2), "hé [clipped, +3 bytes]");
    }

    #[test]
    fn a_clipped_result_keeps_its_last_line_as_well_as_its_first() {
        // What a long tool result is worth finding for is usually at the end: the counts a test
        // runner prints, the error a build finishes on.
        assert_eq!(clip_ends("abcdefghij", 2, 3), "ab [clipped, +5 bytes] hij");
        // Short enough that the two ends would meet: keep the whole thing rather than mark it.
        assert_eq!(clip_ends("abcde", 2, 3), "abcde");
        assert_eq!(clip_ends("abcdef", 2, 3), "ab [clipped, +1 bytes] def");
        // Both ends land on character boundaries.
        assert_eq!(clip_ends("héllo wörld", 2, 2), "hé [clipped, +8 bytes] ld");
    }

    #[test]
    fn a_typed_prompt_becomes_one_user_record() {
        let mut st = FileState::default();
        let line = r#"{"type":"user","sessionId":"s1","timestamp":"2026-08-01T10:00:00.000Z","message":{"content":"implement PER-26"}}"#;
        let recs = distill("pega-we-1", "s1", line, &mut st, READ_AT);
        assert_eq!(recs.len(), 1);
        assert_eq!(recs[0].clone, "pega-we-1");
        assert_eq!(recs[0].session, "s1");
        assert_eq!(recs[0].ts, "2026-08-01T10:00:00.000Z");
        assert_eq!(recs[0].kind, "user");
        assert_eq!(recs[0].text, "implement PER-26");
        // The timestamp is carried, for the records that arrive without one.
        assert_eq!(st.last_ts, "2026-08-01T10:00:00.000Z");
    }

    #[test]
    fn base64_image_data_never_reaches_the_ledger() {
        let mut st = FileState::default();
        let payload = "A".repeat(400_000);
        let line = format!(
            r#"{{"type":"user","sessionId":"s1","timestamp":"2026-08-01T10:00:00.000Z","message":{{"content":[{{"type":"tool_result","tool_use_id":"toolu_9","content":[{{"type":"image","source":{{"type":"base64","media_type":"image/jpeg","data":"{payload}"}}}}]}}]}}}}"#
        );
        let recs = distill("c", "s1", &line, &mut st, READ_AT);
        assert_eq!(recs.len(), 1);
        assert_eq!(recs[0].kind, "toolResult");
        assert_eq!(recs[0].tool_id, "toolu_9");
        assert_eq!(recs[0].text, "[image/jpeg, 400000 base64 bytes dropped]");
        assert!(!recs[0].text.contains("AAAA"));
    }

    #[test]
    fn an_assistant_turn_keeps_its_words_and_its_tool_calls_but_not_its_thinking() {
        let mut st = FileState::default();
        let line = r#"{"type":"assistant","sessionId":"s1","timestamp":"2026-08-01T10:00:01.000Z","message":{"content":[
            {"type":"thinking","thinking":"let me work out what to do here"},
            {"type":"text","text":"Reading the config first."},
            {"type":"tool_use","id":"toolu_1","name":"Bash","input":{"command":"cat config.json"}}
        ]}}"#;
        let recs = distill("c", "s1", line, &mut st, READ_AT);
        assert_eq!(recs.len(), 2);
        assert_eq!(recs[0].kind, "assistant");
        assert_eq!(recs[0].text, "Reading the config first.");
        assert_eq!(recs[1].kind, "toolUse");
        assert_eq!(recs[1].tool, "Bash");
        assert_eq!(recs[1].tool_id, "toolu_1");
        assert!(recs[1].text.contains("cat config.json"));
        assert!(recs.iter().all(|r| !r.text.contains("work out what to do")));
    }

    #[test]
    fn a_tool_input_and_a_tool_result_are_clipped() {
        let mut st = FileState::default();
        let big = "x".repeat(10_000);
        let line = format!(
            r#"{{"type":"assistant","sessionId":"s1","timestamp":"2026-08-01T10:00:00.000Z","message":{{"content":[{{"type":"tool_use","id":"t","name":"Write","input":{{"content":"{big}"}}}}]}}}}"#
        );
        let recs = distill("c", "s1", &line, &mut st, READ_AT);
        // A tool input is a command line or a path, so the head really is the whole of it.
        assert!(recs[0].text.chars().count() < MAX_TOOL_INPUT_CHARS + 40);
        assert!(recs[0].text.contains("clipped"));

        let line = format!(
            r#"{{"type":"user","sessionId":"s1","timestamp":"2026-08-01T10:00:00.000Z","message":{{"content":[{{"type":"tool_result","tool_use_id":"t","content":"{big}"}}]}}}}"#
        );
        let recs = distill("c", "s1", &line, &mut st, READ_AT);
        let budget = MAX_TOOL_RESULT_CHARS + MAX_TOOL_RESULT_TAIL_CHARS;
        assert!(recs[0].text.chars().count() < budget + 40);
        assert!(recs[0].text.contains("clipped"));
    }

    #[test]
    fn a_long_tool_result_keeps_the_verdict_it_ends_on() {
        // The shape this is for: a test runner that logs for pages and prints its counts last.
        let mut st = FileState::default();
        let noise = "compiling crate\\n".repeat(500);
        let line = format!(
            r#"{{"type":"user","sessionId":"s1","timestamp":"2026-08-01T10:00:00.000Z","message":{{"content":[{{"type":"tool_result","tool_use_id":"t","content":"{noise}test result: FAILED. 411 passed; 1 failed"}}]}}}}"#
        );
        let recs = distill("c", "s1", &line, &mut st, READ_AT);
        assert!(recs[0].text.starts_with("compiling crate"), "the head is untouched");
        assert!(recs[0].text.contains("clipped"));
        assert!(recs[0].text.ends_with("411 passed; 1 failed"), "{}", recs[0].text);
    }

    #[test]
    fn a_subagents_whole_report_fits_in_one_record() {
        // A subagent's output arrives as one message rather than as a conversation, and the
        // longest measured is 81,564 characters.
        let mut st = FileState::default();
        let report = "R".repeat(60_000);
        let line = format!(
            r#"{{"type":"assistant","sessionId":"s1","isSidechain":true,"agentId":"a1","timestamp":"2026-08-01T10:00:00.000Z","message":{{"content":[{{"type":"text","text":"{report}"}}]}}}}"#
        );
        let recs = distill("c", "s1", &line, &mut st, READ_AT);
        assert_eq!(recs[0].text.chars().count(), 60_000);
        assert!(!recs[0].text.contains("clipped"));
        assert_eq!(recs[0].agent_id, "a1");
        // The record still has to fit the longest line a search will look at.
        assert!(serde_json::to_string(&recs[0]).unwrap().len() < MAX_LINE_BYTES);
    }

    #[test]
    fn only_a_changed_title_is_recorded_and_it_inherits_the_last_timestamp() {
        let mut st =
            FileState { last_ts: "2026-08-01T10:00:00.000Z".into(), ..Default::default() };
        let line = r#"{"type":"ai-title","sessionId":"s1","aiTitle":"Wire up the ledger"}"#;
        let first = distill("c", "s1", line, &mut st, READ_AT);
        assert_eq!(first.len(), 1);
        assert_eq!(first[0].kind, "title");
        assert_eq!(first[0].ts, "2026-08-01T10:00:00.000Z");
        // The CLI rewrites the title on nearly every turn; only a change is news.
        assert!(distill("c", "s1", line, &mut st, READ_AT).is_empty());
        let changed = r#"{"type":"ai-title","sessionId":"s1","aiTitle":"Ship the ledger"}"#;
        assert_eq!(distill("c", "s1", changed, &mut st, READ_AT).len(), 1);
    }

    #[test]
    fn a_compaction_boundary_is_kept_and_the_rest_of_system_is_not() {
        let mut st = FileState::default();
        let line = r#"{"type":"system","subtype":"compact_boundary","sessionId":"s1","timestamp":"2026-08-01T11:14:05.352Z","compactMetadata":{"trigger":"manual","preTokens":791517,"postTokens":11833}}"#;
        let recs = distill("c", "s1", line, &mut st, READ_AT);
        assert_eq!(recs.len(), 1);
        assert_eq!(recs[0].kind, "compact");
        assert_eq!(recs[0].text, "conversation compacted (manual): 791517 tokens before, 11833 after");
        let noise = r#"{"type":"system","subtype":"turn_duration","sessionId":"s1","timestamp":"2026-08-01T11:14:05.352Z","durationMs":12}"#;
        assert!(distill("c", "s1", noise, &mut st, READ_AT).is_empty());
    }

    #[test]
    fn the_noisy_record_types_are_dropped() {
        let mut st = FileState::default();
        for line in [
            r#"{"type":"attachment","sessionId":"s1","attachment":{"type":"total_tokens_reminder"}}"#,
            r#"{"type":"last-prompt","sessionId":"s1","lastPrompt":"do the thing"}"#,
            r#"{"type":"queue-operation","sessionId":"s1","operation":"add","content":"later"}"#,
            r#"{"type":"file-history-snapshot","messageId":"m"}"#,
            "not json at all",
        ] {
            assert!(distill("c", "s1", line, &mut st, READ_AT).is_empty(), "kept {line}");
        }
    }

    #[test]
    fn a_sidechain_turn_is_flagged() {
        let mut st = FileState::default();
        let line = r#"{"type":"assistant","sessionId":"s1","isSidechain":true,"timestamp":"2026-08-01T10:00:00.000Z","message":{"content":[{"type":"text","text":"subagent говорит"}]}}"#;
        let recs = distill("c", "s1", line, &mut st, READ_AT);
        assert!(recs[0].sidechain);
        // The flag is omitted rather than written as false on an ordinary turn.
        let plain = r#"{"type":"assistant","sessionId":"s1","timestamp":"2026-08-01T10:00:00.000Z","message":{"content":[{"type":"text","text":"hi"}]}}"#;
        let recs = distill("c", "s1", plain, &mut st, READ_AT);
        let json = serde_json::to_string(&recs[0]).unwrap();
        assert!(!json.contains("sidechain"), "{json}");
    }

    #[test]
    fn every_record_names_its_clone_session_timestamp_and_kind() {
        let mut st = FileState::default();
        let line = r#"{"type":"user","sessionId":"s1","timestamp":"2026-08-01T10:00:00.000Z","message":{"content":"hello"}}"#;
        let recs = distill("pega-we-1", "s1", line, &mut st, READ_AT);
        let json = serde_json::to_string(&recs[0]).unwrap();
        // Leading fields, in this order, are what make a grep hit readable on its own.
        assert!(
            json.starts_with(r#"{"clone":"pega-we-1","session":"s1","ts":"2026-08-01T10:00:00.000Z","kind":"user""#),
            "{json}"
        );
    }

    #[test]
    fn a_tail_resumes_where_it_stopped_and_never_repeats_itself() {
        let root = temp_dir("tail");
        let home = root.join("home");
        let dir = root.join("ledger/c1");
        write_transcript(
            &home,
            "-home-rmng-RMNG",
            "aaaa-bbbb",
            &[r#"{"type":"user","sessionId":"aaaa-bbbb","timestamp":"2026-08-01T10:00:00.000Z","message":{"content":"first"}}"#],
        );
        tail_clone("c1", &home, &dir);
        let out = dir.join("aaaa-bbbb.ndjson");
        assert_eq!(std::fs::read_to_string(&out).unwrap().lines().count(), 1);

        // A second pass with nothing new appends nothing.
        tail_clone("c1", &home, &dir);
        assert_eq!(std::fs::read_to_string(&out).unwrap().lines().count(), 1);

        // Appending to the transcript adds exactly the new record.
        let src = home.join(".claude/projects/-home-rmng-RMNG/aaaa-bbbb.jsonl");
        append(
            &src,
            b"{\"type\":\"user\",\"sessionId\":\"aaaa-bbbb\",\"timestamp\":\"2026-08-01T10:05:00.000Z\",\"message\":{\"content\":\"second\"}}\n",
        )
        .unwrap();
        tail_clone("c1", &home, &dir);
        let body = std::fs::read_to_string(&out).unwrap();
        assert_eq!(body.lines().count(), 2);
        assert!(body.contains("first") && body.contains("second"));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_partial_trailing_line_waits_for_its_newline() {
        let root = temp_dir("partial");
        let home = root.join("home");
        let dir = root.join("ledger/c1");
        let src = write_transcript(&home, "p", "sess", &[]);
        std::fs::write(
            &src,
            r#"{"type":"user","sessionId":"sess","timestamp":"2026-08-01T10:00:00.000Z","message":{"content":"half"#,
        )
        .unwrap();
        tail_clone("c1", &home, &dir);
        assert!(!dir.join("sess.ndjson").exists());

        // The CLI finishes the line and the whole record lands.
        append(&src, b"way\"}}\n").unwrap();
        tail_clone("c1", &home, &dir);
        let body = std::fs::read_to_string(dir.join("sess.ndjson")).unwrap();
        assert_eq!(body.lines().count(), 1);
        assert!(body.contains("halfway"));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_clone_with_no_transcripts_still_reserves_its_name() {
        let root = temp_dir("reserve");
        let data = root.to_string_lossy().into_owned();
        let dir = clone_dir(&data, "pega-we-9").unwrap();
        tail_clone("pega-we-9", &root.join("nothing-here"), &dir);
        assert!(reserved_names(&data).contains("pega-we-9"));
        // A path-unsafe id never reaches the filesystem.
        assert!(clone_dir(&data, "../escape").is_none());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn a_fleet_pass_tails_the_running_clones_and_a_final_pass_catches_the_rest() {
        let app = crate::app::App::test_app();
        let data = app.config().data_dir.clone();
        let root = Path::new(&data);
        std::fs::create_dir_all(crate::homes::hosts_root(&data)).unwrap();

        // Two clones, each with a transcript, reached the way `homes` presents them: a symlink
        // under `hosts/` pointing at the clone's home.
        for id in ["live-1", "frozen-1"] {
            let home = root.join(format!("home-{id}"));
            write_transcript(
                &home,
                "p",
                "sess",
                &[&format!(
                    r#"{{"type":"user","sessionId":"sess","timestamp":"2026-08-01T10:00:00.000Z","message":{{"content":"work on {id}"}}}}"#
                )],
            );
            std::os::unix::fs::symlink(&home, crate::homes::hosts_root(&data).join(id)).unwrap();
        }
        app.store.mutate(|s| {
            s.hosts.push(wire::RmngClone {
                id: "live-1".into(),
                host: "live-1".into(),
                managed: true,
                ..Default::default()
            });
            s.hosts.push(wire::RmngClone {
                id: "frozen-1".into(),
                host: "frozen-1".into(),
                managed: true,
                archived: true,
                ..Default::default()
            });
        });

        tail_fleet(&app).await;
        let ledger_of = |id: &str| ledger_root(&data).join(id).join("sess.ndjson");
        assert!(ledger_of("live-1").exists());
        // An archived clone is off the timer: its container is stopped and `homes` has dropped
        // the link, so a pass over it would read nothing anyway.
        assert!(!ledger_of("frozen-1").exists());

        // The pass that runs at the start of an archive or a delete, while the clone is still up.
        tail_once(&app, "frozen-1").await;
        let body = std::fs::read_to_string(ledger_of("frozen-1")).unwrap();
        assert_eq!(body.lines().count(), 1);
        assert!(body.contains("work on frozen-1"));

        // Both names are spent from here on, deleted or not.
        let names = reserved_names(&data);
        assert!(names.contains("live-1") && names.contains("frozen-1"));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn search_filters_by_clone_and_by_time_and_reads_back_a_range() {
        let root = temp_dir("search");
        let data = root.to_string_lossy().into_owned();
        for (clone, home_tag) in [("c1", "one"), ("c2", "two")] {
            let home = root.join(home_tag);
            write_transcript(
                &home,
                "p",
                &format!("sess-{clone}"),
                &[
                    &format!(r#"{{"type":"user","sessionId":"sess-{clone}","timestamp":"2026-08-01T10:00:00.000Z","message":{{"content":"fix the encoder"}}}}"#),
                    &format!(r#"{{"type":"assistant","sessionId":"sess-{clone}","timestamp":"2026-08-02T10:00:00.000Z","message":{{"content":[{{"type":"text","text":"the encoder is fixed"}}]}}}}"#),
                ],
            );
            tail_clone(clone, &home, &clone_dir(&data, clone).unwrap());
        }

        let all = search(&data, &SearchQuery { pattern: "encoder".into(), limit: 50, ..Default::default() });
        assert_eq!(all.hits.len(), 4);
        assert!(!all.truncated);
        // Newest first.
        assert!(all.hits[0].ts > all.hits[3].ts);

        let one = search(
            &data,
            &SearchQuery { pattern: "encoder".into(), clone: Some("c1".into()), limit: 50, ..Default::default() },
        );
        assert_eq!(one.hits.len(), 2);
        assert!(one.hits.iter().all(|h| h.clone == "c1"));

        // Case-insensitive, and the pattern reaches the kind as well as the text.
        assert_eq!(
            search(&data, &SearchQuery { pattern: "FIX THE".into(), limit: 50, ..Default::default() }).hits.len(),
            2
        );

        // Everything from 2026-08-02 onward is the assistant half.
        let late = search(
            &data,
            &SearchQuery {
                pattern: "encoder".into(),
                since_ms: Some(1785628800000), // 2026-08-02T00:00:00Z
                limit: 50,
                ..Default::default()
            },
        );
        assert_eq!(late.hits.len(), 2);
        assert!(late.hits.iter().all(|h| h.kind == "assistant"));

        // A hit's offset reads back as whole NDJSON lines.
        let hit = one.hits.iter().find(|h| h.kind == "user").unwrap();
        let range = read_range(&data, "c1", "sess-c1", hit.offset, hit.len).unwrap();
        assert!(range.text.ends_with('\n'));
        assert_eq!(range.text.lines().count(), 1);
        assert!(range.text.contains("fix the encoder"));
        assert!(range.size > 0);

        // An offset mid-line snaps back to the line's start rather than returning a fragment.
        let mid = read_range(&data, "c1", "sess-c1", hit.offset + 5, 10).unwrap();
        assert!(mid.text.starts_with('{'), "{}", mid.text);

        // Reading past the end is empty, not an error; a bad id is an error.
        assert_eq!(read_range(&data, "c1", "sess-c1", 1_000_000, 10).unwrap().len, 0);
        assert!(read_range(&data, "../etc", "sess-c1", 0, 10).is_err());
        assert!(read_range(&data, "c1", "../../secrets", 0, 10).is_err());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn search_separates_the_conversation_from_the_subagents_it_spawned() {
        let root = temp_dir("sidechain-search");
        let data = root.to_string_lossy().into_owned();
        let home = root.join("home");
        write_transcript(
            &home,
            "p",
            "sess",
            &[r#"{"type":"user","sessionId":"sess","timestamp":"2026-08-07T10:00:00.000Z","message":{"content":"review the encoder"}}"#],
        );
        let subs = home.join(".claude/projects/p/sess/subagents");
        std::fs::create_dir_all(&subs).unwrap();
        for (agent, verdict) in [("a1", "the encoder is fine"), ("a2", "the encoder leaks")] {
            std::fs::write(
                subs.join(format!("agent-{agent}.jsonl")),
                format!(
                    "{{\"type\":\"assistant\",\"sessionId\":\"sess\",\"isSidechain\":true,\"agentId\":\"{agent}\",\"timestamp\":\"2026-08-07T10:01:00.000Z\",\"message\":{{\"content\":[{{\"type\":\"text\",\"text\":\"{verdict}\"}}]}}}}\n"
                ),
            )
            .unwrap();
        }
        tail_clone("c1", &home, &clone_dir(&data, "c1").unwrap());

        let find = |q: SearchQuery| search(&data, &q).hits;
        let base = SearchQuery { pattern: "encoder".into(), limit: 50, ..Default::default() };
        assert_eq!(find(base.clone()).len(), 3, "unfiltered, both halves");

        // The mirror of the old problem: a delegating session's own words, without the fan-out.
        let main_only = find(SearchQuery { sidechain: Some(false), ..base.clone() });
        assert_eq!(main_only.len(), 1);
        assert!(main_only[0].line.contains("review the encoder"));

        assert_eq!(find(SearchQuery { sidechain: Some(true), ..base.clone() }).len(), 2);

        // One subagent's run, read back on its own.
        let one = find(SearchQuery { agent: Some("a2".into()), ..base.clone() });
        assert_eq!(one.len(), 1);
        assert!(one[0].line.contains("leaks"));
        assert!(find(SearchQuery { agent: Some("a9".into()), ..base }).is_empty());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn search_reports_that_it_stopped_at_the_limit() {
        let root = temp_dir("limit");
        let data = root.to_string_lossy().into_owned();
        let home = root.join("home");
        let lines: Vec<String> = (0..10)
            .map(|i| format!(r#"{{"type":"user","sessionId":"s","timestamp":"2026-08-0{}T10:00:00.000Z","message":{{"content":"needle {i}"}}}}"#, i % 9 + 1))
            .collect();
        let refs: Vec<&str> = lines.iter().map(String::as_str).collect();
        write_transcript(&home, "p", "s", &refs);
        tail_clone("c1", &home, &clone_dir(&data, "c1").unwrap());

        let capped = search(&data, &SearchQuery { pattern: "needle".into(), limit: 3, ..Default::default() });
        assert_eq!(capped.hits.len(), 3);
        assert!(capped.truncated);
        let _ = std::fs::remove_dir_all(&root);
    }
}


