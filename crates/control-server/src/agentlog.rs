//! Per-clone token accounting and agent-activity detection, read straight out of the agent
//! CLIs' own session logs.
//!
//! **Why the logs.** Both signals used to come from the `/cc` proxy sitting in the data path.
//! It no longer does — agents talk to Anthropic and OpenAI directly, so the server never sees
//! their traffic. What survives is that each CLI writes a JSONL transcript of its own session,
//! and it writes it **whoever launched the agent**: RMNG's chat, the autonomous loop, or a
//! human who SSH'd in and typed `claude`. That last case is the whole point. The agent-wrapper
//! SSE stream (see [`crate::chat::run_autonomous_listener`]) only ever knew about turns RMNG
//! itself drove, so a hand-run agent left its clone reading `idle` while it was visibly working.
//!
//! **Why it needs no clone-side cooperation.** [`crate::homes`] already maintains
//! `<data_dir>/hosts/<clone-id>` as a symlink to `/proc/<pid>/root/home/rmng` for every running
//! managed clone (the control-server runs with `pid: "host"`). So these are plain file reads
//! from this process — no `docker exec`, no new connection, no agent-wrapper rebuild, and it
//! works on clones that are already running.
//!
//! **What was measured rather than assumed** (CT 120, both CLIs run by hand in a real clone):
//!
//! - The logs are appended **during** a turn, not flushed at exit (observed a file grow
//!   11135 → 11249 → 17183 bytes while the turn ran). Activity detection depends on this;
//!   a flush-at-exit format would light the dot exactly when the work finished.
//! - A single API response can be logged **twice** — same `message.id` and `requestId`,
//!   different line `uuid`, 14s apart. Summing lines naively double-counts it. Hence
//!   [`Dedup`].
//! - Claude records the model on every assistant line; Codex records it in `turn_context`,
//!   not in the usage event. So the Fable badge keys off the Claude log only (Fable is a
//!   Claude model family anyway).

use std::collections::{HashMap, HashSet, VecDeque};
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::Deserialize;
use wire::{CloneTokens, RmngClone};

use crate::app::App;

/// How often to walk the log trees.
///
/// Matched to [`crate::homes`]'s own reconcile cadence: the symlinks this walk depends on are
/// refreshed at that rate, so scanning faster cannot surface anything newer. The monitor's 4s
/// tick would be too hot for a filesystem walk across a 41-clone fleet.
const SCAN_INTERVAL: Duration = Duration::from_secs(15);

/// How long after a clone's last Fable response the badge stays lit. Matches
/// [`crate::monitor::ActivityBus`]'s inactivity window, so "recently" means the same thing
/// in both places.
const FABLE_ACTIVE_MS: i64 = 5 * 60 * 1000;

/// How many recent `requestId`s to remember per clone for duplicate suppression.
///
/// The duplicate pair observed on CT 120 sat 2 records apart. 256 is three orders of magnitude
/// of headroom while staying trivially small in memory, and bounds the set so a long-lived
/// clone cannot grow it without limit.
const DEDUP_CAPACITY: usize = 256;

/// Cap on bytes read from one file in one pass, so a pathological append (or a first sight of
/// a huge retained log) cannot stall the scan loop. The remainder is picked up next tick.
const MAX_READ_PER_FILE: u64 = 8 * 1024 * 1024;

/// A byte cursor into one log file.
///
/// `inode` and `len` together detect the two ways a cursor can go stale: the file was replaced
/// (new inode — rotation, or a clone recreated with the same id) or truncated (`len` moved
/// backwards). Either means "re-read from 0"; a plain offset alone would silently skip content
/// or read garbage from the middle of a line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Cursor {
    inode: u64,
    offset: u64,
}

/// Bounded FIFO of recently-seen request ids, for suppressing the duplicate-response records
/// Claude Code sometimes writes.
#[derive(Debug, Default)]
struct Dedup {
    seen: HashSet<String>,
    order: VecDeque<String>,
}

impl Dedup {
    /// Record `id`, returning true if it is new. An id already present is a duplicate of a
    /// response we have already counted.
    fn insert(&mut self, id: &str) -> bool {
        if !self.seen.insert(id.to_string()) {
            return false;
        }
        self.order.push_back(id.to_string());
        if self.order.len() > DEDUP_CAPACITY {
            if let Some(old) = self.order.pop_front() {
                self.seen.remove(&old);
            }
        }
        true
    }
}

/// Everything the scanner remembers about one clone between passes. Volatile by design: the
/// cumulative token totals live in `state.json` (durable), while cursors and dedup rings are
/// cheap to rebuild — see [`CloneScan::seeded`] for what a cold start does instead of
/// back-filling.
#[derive(Debug, Default)]
struct CloneScan {
    cursors: HashMap<PathBuf, Cursor>,
    dedup: Dedup,
    last_fable_ms: Option<i64>,
    /// False until this clone's first pass has completed.
    ///
    /// On first sight every existing log file is cursored at its current end WITHOUT being
    /// counted. A clone that has been running for weeks would otherwise book its entire
    /// retained history the moment the feature ships — an arbitrary number, since both CLIs
    /// prune old sessions, so it would represent "whatever happens to be left on disk" rather
    /// than anything meaningful. Counting starts from zero and climbs with real traffic.
    seeded: bool,
}

/// One clone's newly-observed activity in a single pass.
#[derive(Debug, Default, PartialEq, Eq)]
struct Delta {
    input_tokens: u64,
    output_tokens: u64,
    /// Newest record timestamp seen (epoch ms), or `None` if the pass found nothing.
    last_activity_ms: Option<i64>,
    /// Newest Fable-model response seen (epoch ms).
    last_fable_ms: Option<i64>,
}

impl Delta {
    fn is_empty(&self) -> bool {
        *self == Delta::default()
    }

    fn observe(&mut self, ts_ms: Option<i64>) {
        if let Some(ts) = ts_ms {
            self.last_activity_ms = Some(self.last_activity_ms.map_or(ts, |cur| cur.max(ts)));
        }
    }

    fn observe_fable(&mut self, ts_ms: Option<i64>) {
        if let Some(ts) = ts_ms {
            self.last_fable_ms = Some(self.last_fable_ms.map_or(ts, |cur| cur.max(ts)));
        }
    }
}

// --- Claude transcript records -------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct ClaudeLine {
    #[serde(default)]
    timestamp: Option<String>,
    /// The API request this line reports on. Two lines sharing one `requestId` are two
    /// records of the SAME response and must be counted once.
    #[serde(rename = "requestId", default)]
    request_id: Option<String>,
    #[serde(default)]
    message: Option<ClaudeMessage>,
}

#[derive(Debug, Deserialize)]
struct ClaudeMessage {
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    usage: Option<ClaudeUsageRec>,
}

#[derive(Debug, Default, Deserialize)]
struct ClaudeUsageRec {
    #[serde(default)]
    input_tokens: u64,
    #[serde(default)]
    output_tokens: u64,
    #[serde(default)]
    cache_creation_input_tokens: u64,
    // `cache_read_input_tokens` is deliberately NOT deserialized: it is excluded from the
    // count, and naming it here would invite someone to add it in.
}

// --- Codex rollout records -----------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct CodexLine {
    #[serde(default)]
    timestamp: Option<String>,
    #[serde(default)]
    payload: Option<CodexPayload>,
}

#[derive(Debug, Deserialize)]
struct CodexPayload {
    #[serde(rename = "type", default)]
    kind: Option<String>,
    #[serde(default)]
    info: Option<CodexInfo>,
}

#[derive(Debug, Deserialize)]
struct CodexInfo {
    /// The per-turn delta. Deliberately used instead of the sibling `total_token_usage`,
    /// which is cumulative *within the session* — summing that across events would
    /// multiply-count every earlier turn.
    #[serde(default)]
    last_token_usage: Option<CodexUsageRec>,
}

#[derive(Debug, Default, Deserialize)]
struct CodexUsageRec {
    #[serde(default)]
    input_tokens: u64,
    #[serde(default)]
    cached_input_tokens: u64,
    #[serde(default)]
    output_tokens: u64,
    #[serde(default)]
    reasoning_output_tokens: u64,
}

// --- parsing --------------------------------------------------------------------------------

/// Epoch ms from an RFC3339 timestamp.
///
/// Reuses the account pollers' parser, which already handles both the `Z` form these logs
/// use and the `±HH:MM` form (a clone in a non-UTC zone). Sub-second precision is dropped —
/// irrelevant here, where the coarsest consumer is a 5-minute activity window.
fn parse_ts_ms(s: &str) -> Option<i64> {
    crate::claude::parse_rfc3339_utc_secs(s).map(|secs| secs * 1000)
}

/// Whether a model id names the Fable family.
///
/// Substring rather than an exact id, so dated variants (`claude-fable-5-20260714`) match
/// without this needing an update. Verified against a real run in a clone, which wrote
/// `claude-fable-5`.
fn is_fable(model: &str) -> bool {
    model.to_ascii_lowercase().contains("fable")
}

/// Fold one Claude transcript line into `delta`.
///
/// Skips anything without a usage block, and the `<synthetic>`-model lines the CLI writes for
/// non-API events (they carry all-zero usage, so counting them is harmless but pointless —
/// stamping *activity* off them is not, since they are written without a model turn having
/// happened).
fn fold_claude_line(line: &str, dedup: &mut Dedup, delta: &mut Delta) {
    let Ok(rec) = serde_json::from_str::<ClaudeLine>(line) else { return };
    let Some(msg) = rec.message else { return };
    let Some(usage) = msg.usage else { return };
    let model = msg.model.unwrap_or_default();
    if model.is_empty() || model.starts_with('<') {
        return; // `<synthetic>` and friends: not an API response
    }
    // Suppress the duplicate-record case. A line with no request id cannot be checked, so it
    // is counted — under-counting a real response is worse than the rare double-count of a
    // record the CLI chose not to identify.
    if let Some(id) = rec.request_id.as_deref() {
        if !dedup.insert(id) {
            return;
        }
    }
    let ts = rec.timestamp.as_deref().and_then(parse_ts_ms);
    delta.input_tokens += usage.input_tokens + usage.cache_creation_input_tokens;
    delta.output_tokens += usage.output_tokens;
    delta.observe(ts);
    if is_fable(&model) {
        delta.observe_fable(ts);
    }
}

/// Fold one Codex rollout line into `delta`.
///
/// No request-id dedup: each `token_count` event is appended exactly once, and the byte cursor
/// already guarantees a given event is read at most once.
fn fold_codex_line(line: &str, delta: &mut Delta) {
    let Ok(rec) = serde_json::from_str::<CodexLine>(line) else { return };
    let Some(payload) = rec.payload else { return };
    if payload.kind.as_deref() != Some("token_count") {
        return;
    }
    let Some(usage) = payload.info.and_then(|i| i.last_token_usage) else { return };
    // Cached input is the analogue of Claude's cache reads — excluded for the same reason.
    // `saturating_sub` because the two fields come from different accounting paths and a
    // future format change must not underflow into a nonsense total.
    delta.input_tokens += usage.input_tokens.saturating_sub(usage.cached_input_tokens);
    delta.output_tokens += usage.output_tokens + usage.reasoning_output_tokens;
    delta.observe(rec.timestamp.as_deref().and_then(parse_ts_ms));
}

/// Which parser a path needs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Flavor {
    Claude,
    Codex,
}

/// Read the bytes appended to `path` since `cursor` and fold them into `delta`, returning the
/// new cursor.
///
/// Only whole lines are consumed: a partial trailing line (the writer is mid-append) leaves
/// the cursor before it, so it is read complete on the next pass rather than parsed as a
/// truncated fragment.
fn scan_file(
    path: &Path,
    prev: Option<Cursor>,
    flavor: Flavor,
    dedup: &mut Dedup,
    delta: &mut Delta,
    count: bool,
) -> Option<Cursor> {
    use std::os::unix::fs::MetadataExt;
    let meta = std::fs::metadata(path).ok()?;
    let inode = meta.ino();
    let len = meta.len();
    // A replaced or truncated file restarts from 0; anything else resumes where we stopped.
    let start = match prev {
        Some(c) if c.inode == inode && c.offset <= len => c.offset,
        _ => 0,
    };
    if start >= len {
        return Some(Cursor { inode, offset: start.min(len) });
    }
    if !count {
        // Seeding pass: adopt the current end without reading. See `CloneScan::seeded`.
        return Some(Cursor { inode, offset: len });
    }
    let mut file = std::fs::File::open(path).ok()?;
    file.seek(SeekFrom::Start(start)).ok()?;
    let want = (len - start).min(MAX_READ_PER_FILE);
    let mut buf = vec![0u8; want as usize];
    let read = file.read(&mut buf).ok()?;
    buf.truncate(read);
    // Consume only up to the last newline: everything after it is an incomplete line.
    let Some(last_nl) = buf.iter().rposition(|b| *b == b'\n') else {
        return Some(Cursor { inode, offset: start });
    };
    let consumed = last_nl + 1;
    for line in buf[..consumed].split(|b| *b == b'\n') {
        if line.is_empty() {
            continue;
        }
        let Ok(text) = std::str::from_utf8(line) else { continue };
        match flavor {
            Flavor::Claude => fold_claude_line(text, dedup, delta),
            Flavor::Codex => fold_codex_line(text, delta),
        }
    }
    Some(Cursor { inode, offset: start + consumed as u64 })
}

/// Every `*.jsonl` under `root`, recursing to `depth` more levels.
///
/// Bounded rather than unlimited: the two trees have known shapes
/// (`projects/<slug>/<uuid>.jsonl` and `sessions/YYYY/MM/DD/rollout-*.jsonl`), and a clone's
/// home is operator-writable — an unbounded walk there is an invitation to wander into a
/// checkout of someone's dataset.
fn collect_jsonl(root: &Path, depth: usize, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(root) else { return };
    for entry in rd.flatten() {
        let path = entry.path();
        let Ok(ft) = entry.file_type() else { continue };
        if ft.is_dir() {
            if depth > 0 {
                collect_jsonl(&path, depth - 1, out);
            }
        } else if path.extension().is_some_and(|e| e == "jsonl") {
            out.push(path);
        }
    }
}

/// The log files to scan for one clone home, paired with the parser each needs.
fn log_files(home: &Path) -> Vec<(PathBuf, Flavor)> {
    let mut out = Vec::new();
    let mut claude = Vec::new();
    // `projects/<cwd-slug>/<session-uuid>.jsonl` — one level of slug directory.
    collect_jsonl(&home.join(".claude/projects"), 1, &mut claude);
    out.extend(claude.into_iter().map(|p| (p, Flavor::Claude)));
    let mut codex = Vec::new();
    // `sessions/YYYY/MM/DD/rollout-*.jsonl` — three levels of date directory.
    collect_jsonl(&home.join(".codex/sessions"), 3, &mut codex);
    out.extend(codex.into_iter().map(|p| (p, Flavor::Codex)));
    out
}

/// Scan one clone's logs, returning what changed since the last pass.
fn scan_clone(home: &Path, scan: &mut CloneScan) -> Delta {
    let mut delta = Delta::default();
    let files = log_files(home);
    let count = scan.seeded;
    let mut live: HashSet<PathBuf> = HashSet::with_capacity(files.len());
    for (path, flavor) in files {
        let prev = scan.cursors.get(&path).copied();
        // A file we have never seen, on a clone that IS seeded, is a genuinely new session:
        // count it from the start. Only the clone's very first pass seeds wholesale.
        if let Some(next) = scan_file(&path, prev, flavor, &mut scan.dedup, &mut delta, count) {
            scan.cursors.insert(path.clone(), next);
        }
        live.insert(path);
    }
    // Drop cursors for files that have gone (the CLIs prune old sessions), so the map tracks
    // the live set rather than growing for the life of the server.
    scan.cursors.retain(|p, _| live.contains(p));
    scan.seeded = true;
    delta
}

/// One pass across the fleet: scan every running managed clone, stamp activity, and fold new
/// tokens into the persisted totals.
async fn scan_once(app: &App, scans: &mut HashMap<String, CloneScan>) {
    let cfg = app.config();
    let root = crate::homes::hosts_root(&cfg.data_dir);
    let all = app.store.get().hosts;
    // Only a RUNNING clone has logs to read — an archived one's container is stopped, so
    // `homes` has removed its symlink.
    let hosts: Vec<RmngClone> =
        all.iter().filter(|h| h.managed && !h.archived).cloned().collect();
    // …but a total is retained for every clone that still EXISTS, archived included: the work
    // already happened, and archiving is explicitly a retain operation. Only an actually
    // deleted clone loses its figure. Scoping this to the running set instead would silently
    // zero a clone's history the moment an operator archived it.
    let known_ids: HashSet<String> = all.iter().map(|h| h.id.clone()).collect();
    let scanning_ids: HashSet<String> = hosts.iter().map(|h| h.id.clone()).collect();

    let now = crate::clone_ops::now_ms();
    let mut updates: HashMap<String, CloneTokens> = HashMap::new();
    let existing = app.store.get().clone_tokens;

    for host in &hosts {
        let home = root.join(&host.id);
        // The link is absent while a clone is stopped or still booting: its counter simply
        // holds at the persisted value rather than regressing.
        if !home.exists() {
            continue;
        }
        let scan = scans.entry(host.id.clone()).or_default();
        let delta = tokio::task::block_in_place(|| scan_clone(&home, scan));
        if let Some(ts) = delta.last_fable_ms {
            scan.last_fable_ms = Some(scan.last_fable_ms.map_or(ts, |cur| cur.max(ts)));
        }
        // Activity: the same bus the agent-wrapper SSE path feeds. Whichever observes work
        // first wins; this one additionally covers agents RMNG did not launch.
        if let Some(ts) = delta.last_activity_ms {
            app.activity.mark(&host.id, ts);
        }
        let fable_active =
            scan.last_fable_ms.is_some_and(|at| now.saturating_sub(at) < FABLE_ACTIVE_MS);
        let prev = existing.get(&host.id).copied().unwrap_or_default();
        let next = CloneTokens {
            input_tokens: prev.input_tokens + delta.input_tokens,
            output_tokens: prev.output_tokens + delta.output_tokens,
            fable_active,
        };
        // Republish only a genuine change, so an idle fleet never writes state.json or wakes
        // SSE clients. `fable_active` decaying to false counts as a change — that is the
        // badge going out.
        if next != prev {
            updates.insert(host.id.clone(), next);
        }
    }

    // Cursors and dedup rings only make sense for a clone we are still scanning; an archived
    // clone re-seeds when it comes back, which is correct (its logs may have been pruned
    // meanwhile, and its persisted total is unaffected either way).
    scans.retain(|id, _| scanning_ids.contains(id));

    let stale: Vec<String> =
        existing.keys().filter(|id| !known_ids.contains(*id)).cloned().collect();
    if updates.is_empty() && stale.is_empty() {
        return;
    }
    app.store.mutate(|s| {
        for (id, tokens) in &updates {
            s.clone_tokens.insert(id.clone(), *tokens);
        }
        // A deleted clone's total goes with it: the row is gone from the sidebar, and its home
        // (hence the logs the figure came from) no longer exists.
        for id in &stale {
            s.clone_tokens.remove(id);
        }
    });
}

/// Background loop: scan every clone's agent logs on [`SCAN_INTERVAL`].
pub async fn run_scanner(app: App) {
    tracing::info!(
        "agent-log scanner started (tokens + activity, every {}s)",
        SCAN_INTERVAL.as_secs()
    );
    let mut scans: HashMap<String, CloneScan> = HashMap::new();
    loop {
        scan_once(&app, &mut scans).await;
        tokio::time::sleep(SCAN_INTERVAL).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn tmpdir(tag: &str) -> PathBuf {
        use std::sync::atomic::{AtomicU32, Ordering};
        static N: AtomicU32 = AtomicU32::new(0);
        let dir = std::env::temp_dir().join(format!(
            "rmng-agentlog-{tag}-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn claude_line(req: &str, model: &str, input: u64, cache_create: u64, cache_read: u64, output: u64) -> String {
        format!(
            r#"{{"type":"assistant","timestamp":"2026-07-29T13:10:25.021Z","requestId":"{req}","message":{{"model":"{model}","usage":{{"input_tokens":{input},"cache_creation_input_tokens":{cache_create},"cache_read_input_tokens":{cache_read},"output_tokens":{output}}}}}}}"#
        )
    }

    fn fold_claude(lines: &[String]) -> (Delta, Dedup) {
        let mut dedup = Dedup::default();
        let mut delta = Delta::default();
        for l in lines {
            fold_claude_line(l, &mut dedup, &mut delta);
        }
        (delta, dedup)
    }

    #[test]
    fn cache_reads_are_excluded_and_cache_creation_is_counted() {
        // Real shape from a clone: 1 real input token against 210,735 cache reads. Counting
        // reads would make the figure meaningless.
        let (d, _) = fold_claude(&[claude_line("r1", "claude-opus-5", 1, 961, 210_735, 887)]);
        assert_eq!(d.input_tokens, 962, "input = input + cache_creation, never cache_read");
        assert_eq!(d.output_tokens, 887);
    }

    #[test]
    fn duplicate_request_id_is_counted_once() {
        // Observed on CT 120: one response written twice, same requestId, different line uuid.
        let line = claude_line("req_dup", "claude-opus-4-8", 2, 5491, 15_002, 985);
        let (d, _) = fold_claude(&[line.clone(), line]);
        assert_eq!(d.input_tokens, 5493, "the second record is the same response");
        assert_eq!(d.output_tokens, 985);
    }

    #[test]
    fn distinct_request_ids_both_count() {
        let (d, _) = fold_claude(&[
            claude_line("r1", "claude-opus-5", 10, 0, 999, 5),
            claude_line("r2", "claude-opus-5", 20, 0, 999, 7),
        ]);
        assert_eq!(d.input_tokens, 30);
        assert_eq!(d.output_tokens, 12);
    }

    #[test]
    fn synthetic_model_lines_are_skipped() {
        // The CLI writes these for non-API events; they carry all-zero usage and no turn.
        let (d, _) = fold_claude(&[claude_line("r1", "<synthetic>", 0, 0, 0, 0)]);
        assert!(d.is_empty(), "a synthetic line is not a model turn: {d:?}");
    }

    #[test]
    fn lines_without_usage_are_ignored() {
        let mut dedup = Dedup::default();
        let mut delta = Delta::default();
        fold_claude_line(r#"{"type":"user","message":{"role":"user"}}"#, &mut dedup, &mut delta);
        fold_claude_line("not json at all", &mut dedup, &mut delta);
        assert!(delta.is_empty());
    }

    #[test]
    fn fable_model_is_detected_by_substring() {
        assert!(is_fable("claude-fable-5"));
        assert!(is_fable("claude-fable-5-20260714"), "dated variants must match");
        assert!(!is_fable("claude-opus-5"));
        let (d, _) = fold_claude(&[claude_line("r1", "claude-fable-5", 1, 0, 0, 1)]);
        assert!(d.last_fable_ms.is_some(), "a fable response stamps the badge clock");
        let (d, _) = fold_claude(&[claude_line("r2", "claude-opus-5", 1, 0, 0, 1)]);
        assert!(d.last_fable_ms.is_none(), "a non-fable response must not");
    }

    #[test]
    fn dedup_ring_is_bounded_and_evicts_oldest_first() {
        let mut d = Dedup::default();
        for i in 0..DEDUP_CAPACITY + 10 {
            assert!(d.insert(&format!("r{i}")));
        }
        assert_eq!(d.seen.len(), DEDUP_CAPACITY, "the set must stay bounded");
        assert!(d.insert("r0"), "the oldest ids are evicted, so r0 reads as new again");
        assert!(!d.insert(&format!("r{}", DEDUP_CAPACITY + 9)), "the newest is still held");
    }

    #[test]
    fn codex_uses_last_not_total_usage() {
        // `total_token_usage` is cumulative within the session; summing it across events
        // would multiply-count every earlier turn.
        let mut d = Delta::default();
        let ev = |last_in: u64, cached: u64, out: u64, reason: u64, total_in: u64| {
            format!(
                r#"{{"timestamp":"2026-07-29T13:12:02.327Z","type":"event_msg","payload":{{"type":"token_count","info":{{"total_token_usage":{{"input_tokens":{total_in},"cached_input_tokens":0,"output_tokens":99,"reasoning_output_tokens":0}},"last_token_usage":{{"input_tokens":{last_in},"cached_input_tokens":{cached},"output_tokens":{out},"reasoning_output_tokens":{reason}}}}}}}}}"#
            )
        };
        fold_codex_line(&ev(100, 0, 5, 0, 100), &mut d);
        fold_codex_line(&ev(50, 20, 7, 3, 150), &mut d);
        assert_eq!(d.input_tokens, 130, "100 + (50 - 20 cached)");
        assert_eq!(d.output_tokens, 15, "5 + 7 + 3 reasoning");
    }

    #[test]
    fn codex_non_token_events_are_ignored() {
        let mut d = Delta::default();
        fold_codex_line(
            r#"{"timestamp":"2026-07-29T13:12:02.327Z","type":"event_msg","payload":{"type":"task_started"}}"#,
            &mut d,
        );
        fold_codex_line(r#"{"type":"turn_context","payload":{"model":"gpt-5.6-sol"}}"#, &mut d);
        assert!(d.is_empty());
    }

    #[test]
    fn scan_reads_only_appended_bytes() {
        let dir = tmpdir("append");
        let path = dir.join("s.jsonl");
        let mut f = std::fs::File::create(&path).unwrap();
        writeln!(f, "{}", claude_line("r1", "claude-opus-5", 10, 0, 0, 1)).unwrap();
        f.flush().unwrap();

        let mut dedup = Dedup::default();
        let mut d1 = Delta::default();
        let c1 = scan_file(&path, None, Flavor::Claude, &mut dedup, &mut d1, true).unwrap();
        assert_eq!(d1.input_tokens, 10);

        // Second pass with no new bytes must contribute nothing.
        let mut d2 = Delta::default();
        let c2 = scan_file(&path, Some(c1), Flavor::Claude, &mut dedup, &mut d2, true).unwrap();
        assert!(d2.is_empty(), "a re-scan must not re-count: {d2:?}");
        assert_eq!(c1, c2);

        // Appending is picked up, and only the appended part.
        writeln!(f, "{}", claude_line("r2", "claude-opus-5", 7, 0, 0, 2)).unwrap();
        f.flush().unwrap();
        let mut d3 = Delta::default();
        scan_file(&path, Some(c2), Flavor::Claude, &mut dedup, &mut d3, true).unwrap();
        assert_eq!(d3.input_tokens, 7, "only the new line");
    }

    #[test]
    fn partial_trailing_line_is_not_consumed() {
        let dir = tmpdir("partial");
        let path = dir.join("s.jsonl");
        let mut f = std::fs::File::create(&path).unwrap();
        // A complete line plus a fragment, as seen mid-append by the writer.
        write!(f, "{}\n{{\"type\":\"assis", claude_line("r1", "claude-opus-5", 10, 0, 0, 1)).unwrap();
        f.flush().unwrap();

        let mut dedup = Dedup::default();
        let mut d = Delta::default();
        let c = scan_file(&path, None, Flavor::Claude, &mut dedup, &mut d, true).unwrap();
        assert_eq!(d.input_tokens, 10);

        // Completing the fragment yields its tokens exactly once.
        write!(f, "tant\",\"timestamp\":\"2026-07-29T13:10:25.021Z\",\"requestId\":\"r2\",\"message\":{{\"model\":\"claude-opus-5\",\"usage\":{{\"input_tokens\":5,\"output_tokens\":1}}}}}}\n").unwrap();
        f.flush().unwrap();
        let mut d2 = Delta::default();
        scan_file(&path, Some(c), Flavor::Claude, &mut dedup, &mut d2, true).unwrap();
        assert_eq!(d2.input_tokens, 5, "the completed line is read whole, once");
    }

    #[test]
    fn truncation_and_replacement_restart_the_cursor() {
        let dir = tmpdir("trunc");
        let path = dir.join("s.jsonl");
        std::fs::write(&path, format!("{}\n", claude_line("r1", "claude-opus-5", 100, 0, 0, 1)))
            .unwrap();
        let mut dedup = Dedup::default();
        let mut d = Delta::default();
        let c = scan_file(&path, None, Flavor::Claude, &mut dedup, &mut d, true).unwrap();
        assert_eq!(d.input_tokens, 100);

        // Replace with a SHORTER file: the old offset would sit past the end, or worse,
        // mid-line. It must restart from 0.
        std::fs::write(&path, format!("{}\n", claude_line("r9", "claude-opus-5", 3, 0, 0, 1)))
            .unwrap();
        let mut d2 = Delta::default();
        scan_file(&path, Some(c), Flavor::Claude, &mut dedup, &mut d2, true).unwrap();
        assert_eq!(d2.input_tokens, 3, "a shrunken file is re-read from the start");
    }

    #[test]
    fn first_pass_seeds_without_counting_history() {
        // A clone that has been running for weeks must not book its whole retained log the
        // moment the feature ships.
        let dir = tmpdir("seed");
        let proj = dir.join(".claude/projects/-home-rmng");
        std::fs::create_dir_all(&proj).unwrap();
        std::fs::write(
            proj.join("old.jsonl"),
            format!("{}\n", claude_line("r_old", "claude-opus-5", 999_999, 0, 0, 42)),
        )
        .unwrap();

        let mut scan = CloneScan::default();
        let d = scan_clone(&dir, &mut scan);
        assert!(d.is_empty(), "history must be adopted, not counted: {d:?}");
        assert!(scan.seeded);

        // New traffic after seeding IS counted.
        std::fs::write(
            proj.join("new.jsonl"),
            format!("{}\n", claude_line("r_new", "claude-opus-5", 12, 0, 0, 3)),
        )
        .unwrap();
        let d2 = scan_clone(&dir, &mut scan);
        assert_eq!(d2.input_tokens, 12);
        assert_eq!(d2.output_tokens, 3);
    }

    #[test]
    fn scan_clone_reads_both_providers_and_prunes_dead_cursors() {
        let dir = tmpdir("both");
        let proj = dir.join(".claude/projects/-home-rmng");
        let sess = dir.join(".codex/sessions/2026/07/29");
        std::fs::create_dir_all(&proj).unwrap();
        std::fs::create_dir_all(&sess).unwrap();
        let mut scan = CloneScan { seeded: true, ..Default::default() };

        std::fs::write(
            proj.join("a.jsonl"),
            format!("{}\n", claude_line("r1", "claude-opus-5", 10, 0, 500, 2)),
        )
        .unwrap();
        std::fs::write(
            sess.join("rollout-x.jsonl"),
            "{\"timestamp\":\"2026-07-29T13:12:02.327Z\",\"payload\":{\"type\":\"token_count\",\"info\":{\"last_token_usage\":{\"input_tokens\":100,\"cached_input_tokens\":40,\"output_tokens\":5,\"reasoning_output_tokens\":1}}}}\n",
        )
        .unwrap();

        let d = scan_clone(&dir, &mut scan);
        assert_eq!(d.input_tokens, 70, "claude 10 + codex (100-40)");
        assert_eq!(d.output_tokens, 8, "claude 2 + codex (5+1)");
        assert_eq!(scan.cursors.len(), 2);

        // A pruned session file drops its cursor rather than accumulating forever.
        std::fs::remove_file(sess.join("rollout-x.jsonl")).unwrap();
        scan_clone(&dir, &mut scan);
        assert_eq!(scan.cursors.len(), 1, "cursors track the live file set");
    }

    #[test]
    fn walk_depth_is_bounded_to_the_known_tree_shapes() {
        let dir = tmpdir("depth");
        // Claude: projects/<slug>/<uuid>.jsonl is in range; one level deeper is not.
        let deep = dir.join(".claude/projects/slug/nested/deeper");
        std::fs::create_dir_all(&deep).unwrap();
        std::fs::write(dir.join(".claude/projects/slug/ok.jsonl"), "").unwrap();
        std::fs::write(deep.join("too-deep.jsonl"), "").unwrap();
        // Codex: sessions/YYYY/MM/DD/rollout-*.jsonl.
        let day = dir.join(".codex/sessions/2026/07/29");
        std::fs::create_dir_all(&day).unwrap();
        std::fs::write(day.join("rollout-x.jsonl"), "").unwrap();

        let found = log_files(&dir);
        let names: Vec<String> =
            found.iter().map(|(p, _)| p.file_name().unwrap().to_string_lossy().into_owned()).collect();
        assert!(names.contains(&"ok.jsonl".to_string()));
        assert!(names.contains(&"rollout-x.jsonl".to_string()));
        assert!(!names.contains(&"too-deep.jsonl".to_string()), "the walk must stay bounded");
    }

    /// Build an app whose state holds the given clones and token totals.
    fn app_with(
        clones: &[(&str, bool)], // (id, archived)
        tokens: &[(&str, u64)],
    ) -> App {
        let app = App::test_app();
        app.store.mutate(|s| {
            s.hosts = clones
                .iter()
                .map(|(id, archived)| wire::RmngClone {
                    id: (*id).to_string(),
                    managed: true,
                    archived: *archived,
                    ..Default::default()
                })
                .collect();
            s.clone_tokens = tokens
                .iter()
                .map(|(id, n)| {
                    (
                        (*id).to_string(),
                        CloneTokens { input_tokens: *n, output_tokens: *n, fable_active: false },
                    )
                })
                .collect();
        });
        app
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn archiving_keeps_a_total_but_deleting_drops_it() {
        // `kept` is archived — its container is stopped, so there are no logs to read, but the
        // work already happened and the row is still in the sidebar. `gone` is not in the
        // fleet at all: deleted, so its home and its figure go together.
        let app = app_with(&[("live", false), ("kept", true)], &[("live", 5), ("kept", 7), ("gone", 9)]);
        let mut scans = HashMap::new();
        scan_once(&app, &mut scans).await;

        let after = app.store.get().clone_tokens;
        assert_eq!(after.get("kept").map(|t| t.input_tokens), Some(7), "archiving retains");
        assert_eq!(after.get("live").map(|t| t.input_tokens), Some(5), "running is untouched");
        assert!(!after.contains_key("gone"), "a deleted clone's total is dropped");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_clone_with_no_home_link_holds_its_total() {
        // No `hosts/<id>` symlink: the clone is stopped or still booting. Its counter must
        // freeze rather than reset.
        let app = app_with(&[("boot", false)], &[("boot", 42)]);
        let mut scans = HashMap::new();
        scan_once(&app, &mut scans).await;
        assert_eq!(app.store.get().clone_tokens.get("boot").map(|t| t.input_tokens), Some(42));
    }

    #[test]
    fn timestamps_parse_in_both_rfc3339_forms() {
        // Sub-second precision is dropped; the consumers work in minutes.
        let z = parse_ts_ms("2026-07-29T13:10:25.021Z").expect("Z form parses");
        // The same instant written with an offset must land on the same millisecond.
        assert_eq!(parse_ts_ms("2026-07-29T09:10:25.021-04:00"), Some(z));
        assert_eq!(z % 1000, 0, "seconds resolution");
        assert_eq!(parse_ts_ms("nonsense"), None);
    }
}
