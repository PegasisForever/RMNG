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
//! which is Cursor's. Subagent transcripts sit deeper in both trees and are deliberately out of
//! scope: on this machine's fleet they outnumber main sessions seven to one, and they record a
//! delegated sub-task rather than the conversation the operator had.
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
const MAX_READ_PER_CLONE: u64 = 64 * 1024 * 1024;

/// Longest tool input kept, in characters. Enough for a command line, a file path plus a small
/// patch, or the first screen of a query.
const MAX_TOOL_INPUT_CHARS: usize = 800;

/// Longest tool result kept, in characters. Results are the bulkiest thing worth keeping at all:
/// a file read, a test run, a `git log`. The head is where the answer usually is.
const MAX_TOOL_RESULT_CHARS: usize = 2000;

/// Longest message text kept, in characters. Human prompts and model replies are the point of the
/// ledger, so this is generous and only ever fires on something pathological.
const MAX_TEXT_CHARS: usize = 20_000;

/// Most transcripts enumerated for one clone in one pass. A clone is root in its own sandbox and
/// can create as many files as it likes under that path.
const MAX_SESSION_FILES: usize = 4096;

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
    match s.char_indices().nth(max) {
        None => s.to_string(),
        Some((at, _)) => format!("{} [clipped, +{} bytes]", &s[..at], s.len() - at),
    }
}

/// What an image block leaves behind: its type, never its bytes.
fn image_marker(src: Option<&ImageSource>) -> String {
    let media = src.and_then(|s| s.media_type.as_deref()).unwrap_or("image");
    let bytes = src.and_then(|s| s.data.as_deref()).map_or(0, str::len);
    format!("[{media}, {bytes} base64 bytes dropped]")
}

/// The text of a `tool_result`'s content, which is a string or a list of blocks.
fn tool_result_text(content: Option<&serde_json::Value>) -> String {
    let Some(value) = content else { return String::new() };
    if let Some(s) = value.as_str() {
        return clip(s, MAX_TOOL_RESULT_CHARS);
    }
    let Ok(blocks) = serde_json::from_value::<Vec<Block>>(value.clone()) else {
        return clip(&value.to_string(), MAX_TOOL_RESULT_CHARS);
    };
    let mut parts: Vec<String> = Vec::new();
    for b in &blocks {
        match b.kind.as_deref() {
            Some("image") => parts.push(image_marker(b.source.as_ref())),
            _ => {
                if let Some(t) = b.text.as_deref().filter(|t| !t.is_empty()) {
                    parts.push(t.to_string());
                }
            }
        }
    }
    clip(&parts.join("\n"), MAX_TOOL_RESULT_CHARS)
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
        sidechain: raw.sidechain.unwrap_or(false),
        ..Default::default()
    };
    let emit = |kind: &str, text: String| LedgerRecord {
        kind: kind.to_string(),
        text,
        ..base.clone()
    };

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
            Some("image") => said.push(image_marker(b.source.as_ref())),
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

/// Every main-session transcript under `home`, sorted, at most `cap` of them.
///
/// One level below `.claude/projects`, which is where the CLI writes the session the operator is
/// actually talking to. `entry.file_type()` reports a symlink as a symlink, so a symlinked
/// directory is not descended into.
fn session_files(home: &Path, cap: usize) -> Vec<PathBuf> {
    let root = home.join(".claude/projects");
    let mut out: Vec<PathBuf> = Vec::new();
    let Ok(slugs) = std::fs::read_dir(&root) else { return out };
    for slug in slugs.flatten() {
        if !slug.file_type().map(|t| t.is_dir()).unwrap_or(false) {
            continue;
        }
        let Ok(files) = std::fs::read_dir(slug.path()) else { continue };
        for f in files.flatten() {
            if out.len() >= cap {
                tracing::warn!(
                    target: "ledger",
                    "{} holds more than {cap} transcripts; the rest are not being tailed",
                    root.display()
                );
                out.sort_unstable();
                return out;
            }
            let path = f.path();
            if f.file_type().map(|t| t.is_file()).unwrap_or(false)
                && path.extension().is_some_and(|e| e == "jsonl")
            {
                out.push(path);
            }
        }
    }
    out.sort_unstable();
    out
}

/// Every Cursor conversation transcript under `home`, sorted, at most `cap` of them.
///
/// Cursor keeps one directory per conversation and names the transcript after it:
/// `~/.cursor/projects/<workspace>/agent-transcripts/<conversation>/<conversation>.jsonl`. Taking
/// only the file whose stem is its own directory's name is what skips the `subagents/` directory
/// beside it, and skipping that matches what this module already does for Claude Code: the
/// ledger keeps the conversation somebody had, not every worker it spawned.
fn cursor_session_files(home: &Path, cap: usize) -> Vec<PathBuf> {
    let root = home.join(".cursor/projects");
    let mut out: Vec<PathBuf> = Vec::new();
    let Ok(workspaces) = std::fs::read_dir(&root) else { return out };
    for ws in workspaces.flatten() {
        if !ws.file_type().map(|t| t.is_dir()).unwrap_or(false) {
            continue;
        }
        let Ok(convs) = std::fs::read_dir(ws.path().join("agent-transcripts")) else { continue };
        for conv in convs.flatten() {
            if out.len() >= cap {
                out.sort_unstable();
                return out;
            }
            if !conv.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                continue;
            }
            let name = conv.file_name();
            let path = conv.path().join(format!("{}.jsonl", name.to_string_lossy()));
            if path.is_file() {
                out.push(path);
            }
        }
    }
    out.sort_unstable();
    out
}

/// Append the ledger lines for whatever has been added to `src` since its cursor, returning the
/// bytes of transcript consumed.
///
/// The cursor advances only after the append succeeds, so a full disk costs a retry rather than a
/// hole. Only whole lines are consumed: a partial trailing line means the CLI is mid-append, and
/// it is read complete on the next pass.
fn tail_file(clone: &str, src: &Path, dir: &Path, offsets: &mut Offsets) -> u64 {
    use std::os::unix::fs::MetadataExt;
    let Some(name) = ledger_name(src) else { return 0 };
    let Some(key) = src.file_name().and_then(|n| n.to_str()).map(str::to_string) else { return 0 };
    let key = match src.parent().and_then(|p| p.file_name()).and_then(|n| n.to_str()) {
        Some(slug) => format!("{slug}/{key}"),
        None => key,
    };
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
        for rec in distill(clone, &session, text, &mut st, &now) {
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
    let cursor = cursor_session_files(home, MAX_SESSION_FILES.saturating_sub(claude.len()));
    for src in claude.into_iter().chain(cursor) {
        if budget == 0 {
            break; // Backlog drains over the next few passes.
        }
        budget = budget.saturating_sub(tail_file(clone, &src, dir, &mut offsets));
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
    fn cursor_transcripts_are_found_and_subagents_are_not() {
        let home = std::env::temp_dir().join(format!("rmng-ledger-cursor-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        let conv = home.join(".cursor/projects/home-rmng-Dev/agent-transcripts/e52bd0c6");
        std::fs::create_dir_all(conv.join("subagents")).unwrap();
        std::fs::write(conv.join("e52bd0c6.jsonl"), "{}\n").unwrap();
        std::fs::write(conv.join("subagents/853bf50b.jsonl"), "{}\n").unwrap();
        // A directory whose transcript has not been written yet contributes nothing.
        std::fs::create_dir_all(
            home.join(".cursor/projects/home-rmng-Dev/agent-transcripts/empty"),
        )
        .unwrap();

        let found = cursor_session_files(&home, 4096);
        assert_eq!(found, vec![conv.join("e52bd0c6.jsonl")]);
        assert!(cursor_session_files(&home, 0).is_empty(), "the cap is respected");
        let _ = std::fs::remove_dir_all(&home);
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
        assert!(recs[0].text.chars().count() < MAX_TOOL_INPUT_CHARS + 40);
        assert!(recs[0].text.contains("clipped"));

        let line = format!(
            r#"{{"type":"user","sessionId":"s1","timestamp":"2026-08-01T10:00:00.000Z","message":{{"content":[{{"type":"tool_result","tool_use_id":"t","content":"{big}"}}]}}}}"#
        );
        let recs = distill("c", "s1", &line, &mut st, READ_AT);
        assert!(recs[0].text.chars().count() < MAX_TOOL_RESULT_CHARS + 40);
        assert!(recs[0].text.contains("clipped"));
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
