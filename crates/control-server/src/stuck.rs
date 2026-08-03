//! Is a clone going to make progress on its own, or is it waiting on a person?
//!
//! One question, two answers. **Stuck** means Claude Code will not get further without a
//! human giving it more input. **Working** means something machine-driven will move it
//! along. That definition collapses most of what used to need judgement: an agent sitting
//! at its prompt having finished the task is stuck, because it needs the next instruction;
//! so is one that asked a question, and so is one that started a dev server and handed back,
//! because the server runs forever and will never wake anybody.
//!
//! Three of the four branches are decided from files, with no model involved:
//!
//! | condition | verdict |
//! |---|---|
//! | container down | `Offline` |
//! | no live Claude session | `Stuck`, nothing is running to wake anything |
//! | a live session says `waiting` | `Stuck`, a dialog is up and needs a person |
//! | every live session says `idle` | `Stuck`, they are all sitting at the prompt |
//! | anything else | `Ask` |
//!
//! The `idle` shortcut is safe because Claude Code publishes `shell`, not `idle`, the moment
//! a background task, dialog, or queued request exists. So an all-`idle` clone has nothing
//! that could wake it, and that is the commonest state in a fleet: free and certain.
//!
//! The model gets the remaining case, and it is a narrow question: of the work still
//! outstanding, will any of it finish without the operator doing something? That is mostly a
//! property of a command rather than a reading of intent. `cargo build` returns.
//! `npm run dev` does not. `cat /tmp/ci.fifo` returns only if whatever feeds the pipe is a
//! machine and not the person you would be notifying.
//!
//! [`Verdict::Working`] is never produced here — only [`apply_verdict`] can, from the
//! model's answer. That is what makes "no OpenRouter key" behave correctly with no branch
//! for it: with nothing to answer [`Verdict::Ask`], no clone is ever reported working.
//!
//! Nothing is stored between ticks except the answer cache. Every verdict is recomputed
//! from the session registry, the hook log, and file mtimes, so there is no history to
//! reconcile and nothing to race.
//!
//! Measured against a replay oracle over two runs on a live 32-clone fleet (2955 scorable
//! samples, 25 real stalls): 96.7% accurate against token idle's 90.9%, 44 missed stuck
//! clones against 250, and a median 31s to flag a stall against 331s.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::RwLock as StdRwLock;
use std::time::Duration;

use anyhow::{Result, bail};
use serde::Deserialize;
use serde_json::{Value, json};

/// How fresh a file has to be to count as still producing. The sample window for "is it
/// moving right now", not a policy about how long anything may take.
const MOVING_WINDOW_S: f64 = 60.0;

/// Bounded so one slow provider cannot stall a monitor tick.
const ASK_TIMEOUT: Duration = Duration::from_secs(30);

const OPENROUTER_CHAT: &str = "https://openrouter.ai/api/v1/chat/completions";
const OPENROUTER_KEY_INFO: &str = "https://openrouter.ai/api/v1/key";

/// A clone's answer. `Working` is deliberately absent: see the module docs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    Offline,
    Stuck,
    /// The files cannot decide. Hand this clone to the model.
    Ask,
}

/// The model's one boolean. Anything that is not a clear yes reads as stuck, so a malformed
/// or missing answer never invents progress that is not happening.
pub fn apply_verdict(will_progress: Option<bool>) -> wire::MonitorState {
    if will_progress == Some(true) {
        wire::MonitorState::Working
    } else {
        wire::MonitorState::Idle
    }
}

// ---------------------------------------------------------------------------------------
// Reading a clone
// ---------------------------------------------------------------------------------------

/// One record from `~/.claude/sessions/<pid>.json`, Claude Code's own live session registry.
#[derive(Debug, Clone, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct Session {
    #[serde(default)]
    pub pid: i64,
    #[serde(default)]
    pub session_id: String,
    /// One of `busy`, `shell`, `idle`, `waiting`, or absent.
    ///
    /// Absent is common, not a corner case. The record also carries an `entrypoint`, and only
    /// `"cli"` maintains a status: `"sdk-ts"` (the agent-wrapper) and `"claude-vscode"` (the
    /// Cursor/VS Code extension) write the record once at startup and never touch it again.
    /// Six clones on the pilot fleet ran `claude-vscode`.
    #[serde(default)]
    pub status: Option<String>,
    /// What a `waiting` session wants: `input needed`, `dialog open`, or `permission prompt`.
    #[serde(default)]
    pub waiting_for: Option<String>,
    /// Field 22 of the process's `stat`, which distinguishes this process from a later one
    /// that reused its pid.
    #[serde(default)]
    pub proc_start: String,
    /// Filled in by [`read_sessions`], never by serde.
    #[serde(skip)]
    pub alive: bool,
}

/// The clone's filesystem root as this process sees it, e.g. `/proc/1234/root`.
///
/// Derived from the symlink `homes` already maintains, so it reuses the uid-1000 pid that
/// [`crate::homes::pick_home_pid`] chose. Reading the container's `/proc` matters: a session
/// record stores the pid as the CONTAINER numbers it, so checking `/proc/<pid>/stat` out here
/// would test an unrelated process that happens to hold that number.
pub fn clone_root(data_dir: &str, id: &str) -> Option<PathBuf> {
    let link = crate::homes::hosts_root(data_dir).join(id);
    let target = std::fs::read_link(link).ok()?;
    let s = target.to_str()?;
    // `/proc/<pid>/root/home/rmng` -> `/proc/<pid>/root`
    let cut = s.find("/root/")? + "/root".len();
    Some(PathBuf::from(&s[..cut]))
}

/// Field 22 of `<proc>/<pid>/stat`. The token after the last `)` is field 3, so field 22 is
/// token 19. `comm` can itself contain spaces and parentheses, which is why this splits on
/// the LAST `)` rather than tokenising the whole line.
fn proc_start(proc: &Path, pid: i64) -> Option<String> {
    let raw = std::fs::read_to_string(proc.join(pid.to_string()).join("stat")).ok()?;
    let tail = raw.rsplit_once(')')?.1;
    tail.split_whitespace().nth(19).map(str::to_string)
}

/// Every record in the registry, each tagged with whether its process is still alive.
pub fn read_sessions(root: &Path) -> Vec<Session> {
    let dir = root.join("home/rmng/.claude/sessions");
    let proc = root.join("proc");
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return Vec::new();
    };
    let mut names: Vec<PathBuf> = entries
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "json"))
        .collect();
    names.sort();

    let mut out = Vec::new();
    for path in names {
        // A record that vanished between listing and reading is a clone that just exited,
        // which is a real close, not an error.
        let Ok(raw) = std::fs::read(&path) else {
            continue;
        };
        match serde_json::from_slice::<Session>(&raw) {
            Ok(mut s) => {
                s.alive = proc_start(&proc, s.pid).is_some_and(|live| live == s.proc_start);
                out.push(s);
            }
            Err(_) => {
                // A record that exists but does not parse is a torn read, not an absent
                // session, and the safe reading of "I cannot tell" is that something may
                // still be running. Never actually observed: 1,614,540 reads across live
                // status changes produced zero torn reads.
                out.push(Session {
                    alive: true,
                    status: Some("__unreadable__".into()),
                    ..Default::default()
                });
            }
        }
    }
    out
}

/// The whole decision, for everything that can be settled without asking anyone.
pub fn clone_state(sessions: &[Session], container_up: bool) -> Verdict {
    if !container_up {
        return Verdict::Offline;
    }
    let live: Vec<&Session> = sessions.iter().filter(|s| s.alive).collect();
    if live.is_empty() {
        return Verdict::Stuck;
    }
    // `Option<&str>`, not `&str`. A statusless session has to stay IN this set as a
    // distinct member, or a clone running one idle cli session beside one Cursor session
    // collapses to `{idle}` and gets called stuck while Cursor is mid-turn.
    let statuses: HashSet<Option<&str>> = live.iter().map(|s| s.status.as_deref()).collect();
    if statuses.contains(&Some("waiting")) {
        return Verdict::Stuck;
    }
    if statuses.len() == 1 && statuses.contains(&Some("idle")) {
        return Verdict::Stuck;
    }
    Verdict::Ask
}

/// What each dialog-blocked session is waiting on, for the notification text.
pub fn blocked_reasons(sessions: &[Session]) -> Vec<(String, String)> {
    sessions
        .iter()
        .filter(|s| s.alive && s.status.as_deref() == Some("waiting"))
        .map(|s| {
            (
                s.session_id.chars().take(8).collect(),
                s.waiting_for.clone().unwrap_or_else(|| "unknown".into()),
            )
        })
        .collect()
}

// ---------------------------------------------------------------------------------------
// The hook log
// ---------------------------------------------------------------------------------------

/// One line of `~/.rmng/agent-events.jsonl`, written by the probe the reconciler installs.
#[derive(Debug, Clone, Deserialize, Default)]
pub struct HookEvent {
    #[serde(default)]
    pub hook_event_name: String,
    #[serde(default)]
    pub session_id: Option<String>,
    /// Set when the event belongs to a subagent rather than the session itself.
    ///
    /// Worth knowing if you ever fold these into an open-subagent set: a parent's `Stop` does
    /// NOT retire its agents. Background agents outlive the turn that launched them, and one
    /// parent was observed firing `Stop` immediately after spawning two and leaving both
    /// running for eight minutes. Only their own `SubagentStop` (or a `StopFailure` carrying
    /// their `agent_id`) ends them.
    #[serde(default)]
    pub agent_id: Option<String>,
    #[serde(default)]
    pub tool_name: Option<String>,
    #[serde(default)]
    pub tool_use_id: Option<String>,
    #[serde(default)]
    pub tool_input: Option<String>,
    #[serde(default)]
    pub last_assistant_message: Option<String>,
    #[serde(default)]
    pub background_tasks: Vec<BackgroundTask>,
    #[serde(default)]
    pub reason: Option<String>,
    /// How a Cursor turn ended: `completed`, `aborted`, or `error`. Claude Code splits these
    /// across `Stop` and `StopFailure`; Cursor sends one `stop` and puts the outcome here.
    #[serde(default)]
    pub status: Option<String>,
    /// The clone's own clock. Only ever differenced against another stamp from the same
    /// clone, never compared to this server's clock.
    #[serde(default)]
    pub ts: f64,
    /// Set by [`read_hook_events`] when the line arrived under a Cursor event name. Cursor
    /// conversations are rebuilt from the stream, and a Claude Code session must never be
    /// invented from one.
    #[serde(skip)]
    pub from_cursor: bool,
}

/// Cursor names the same events in its own vocabulary, so one table lets every fold below
/// stay written once. Verified against Cursor 3.11.19's `packages/hooks/src/hook-step.ts`
/// and against a live capture of all 21 native steps.
///
/// Only the events the probe subscribes to appear here. Cursor's other steps
/// (`beforeShellExecution`, `afterAgentThought`, `workspaceOpen`, ...) are left alone: they
/// pass through under their own name and no fold matches them.
fn claude_name(cursor: &str) -> Option<&'static str> {
    Some(match cursor {
        "beforeSubmitPrompt" => "UserPromptSubmit",
        "preToolUse" => "PreToolUse",
        "postToolUse" => "PostToolUse",
        "postToolUseFailure" => "PostToolUseFailure",
        "stop" => "Stop",
        "subagentStart" => "SubagentStart",
        "subagentStop" => "SubagentStop",
        "sessionEnd" => "SessionEnd",
        _ => return None,
    })
}

/// How close two copies of one event land. Cursor loads a hook file once as user config and
/// again as project config whenever the workspace root holds it, and fires both. Observed
/// spread between copies: under 20 ms.
const DUPLICATE_WINDOW_S: f64 = 1.0;

#[derive(Debug, Clone, Deserialize, Default)]
pub struct BackgroundTask {
    #[serde(default)]
    pub id: String,
    #[serde(rename = "type", default)]
    pub kind: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub command: Option<String>,
    #[serde(default)]
    pub agent_type: Option<String>,
}

/// Read the WHOLE log, never a tail. A fold over a truncated log is not the fold: it loses
/// the `SubagentStart` of anything long-running, and it invents open sessions whose
/// `UserPromptSubmit` survived the cut while their `Stop` did not.
///
/// Cursor's event names are translated to Claude Code's here, and duplicate copies of one
/// event are dropped, so that every fold downstream sees one vocabulary and one copy.
pub fn read_hook_events(root: &Path) -> Vec<HookEvent> {
    let path = root.join("home/rmng/.rmng/agent-events.jsonl");
    let Ok(raw) = std::fs::read_to_string(&path) else {
        return Vec::new();
    };
    let mut out: Vec<HookEvent> = Vec::new();
    for line in raw.lines().filter(|l| !l.trim().is_empty()) {
        // A torn final line is normal: the probe appends while this reads. Skip it and
        // pick it up next tick.
        let Ok(mut e) = serde_json::from_str::<HookEvent>(line) else {
            continue;
        };
        if let Some(claude) = claude_name(&e.hook_event_name) {
            e.hook_event_name = claude.to_string();
            e.from_cursor = true;
        }
        // The duplicate always lands within milliseconds of its twin, and two genuine events
        // that agree on session, name AND `tool_use_id` cannot: a tool use id is unique to
        // one call. Scanning backwards over the tail rather than only the last entry, because
        // a clone with Cursor and Claude Code both live interleaves the two streams.
        let dup = out
            .iter()
            .rev()
            .take_while(|p| e.ts - p.ts <= DUPLICATE_WINDOW_S)
            .any(|p| {
                p.hook_event_name == e.hook_event_name
                    && p.session_id == e.session_id
                    && p.tool_use_id == e.tool_use_id
                    && p.agent_id == e.agent_id
            });
        if !dup {
            out.push(e);
        }
    }
    out
}

/// Cursor's main process and the wall-clock instant it claimed the profile, or `None` when
/// Cursor is not running in this clone.
///
/// `~/.config/Cursor/code.lock` holds the main process's pid and is written once at startup.
/// Its mtime is the process start in the same clock the hook timestamps use, which is what
/// makes it usable directly: no boot time, no clock-tick conversion.
///
/// The pid is checked against the container's own `/proc`, never this host's, for the same
/// reason [`read_sessions`] does it. `cmdline` stands in for a `procStart` guard: a reused
/// pid running something else fails it, and Cursor's own renderer children carry a `--type=`
/// argument the main process does not.
fn cursor_process(root: &Path) -> Option<(i64, f64)> {
    let lock = root.join("home/rmng/.config/Cursor/code.lock");
    let meta = std::fs::metadata(&lock).ok()?;
    let started =
        meta.modified().ok()?.duration_since(std::time::UNIX_EPOCH).ok()?.as_secs_f64();
    let pid: i64 = std::fs::read_to_string(&lock).ok()?.trim().parse().ok()?;
    let raw = std::fs::read(root.join("proc").join(pid.to_string()).join("cmdline")).ok()?;
    let mut args = raw.split(|b| *b == 0).filter(|a| !a.is_empty()).map(String::from_utf8_lossy);
    if !args.next()?.ends_with("/cursor") {
        return None;
    }
    if args.any(|a| a.starts_with("--type=")) {
        return None;
    }
    Some((pid, started))
}

/// Cursor's conversations, shaped as sessions so one resolver covers both agents.
///
/// Cursor publishes no registry: there is no `~/.claude/sessions` equivalent to read, and a
/// clone worked only through Cursor has none of those files at all. The hook stream is the
/// whole signal, so a conversation is reconstructed from it:
///
/// - Only a conversation that has received a `UserPromptSubmit` counts. A Cursor subagent
///   runs under its own conversation id and never gets one, and it never fires its own
///   `Stop` either (it ends with a `SubagentStop` on the parent), so counting it would leave
///   a session that reads as mid-turn forever.
/// - A turn that has ended maps to `idle`, the same status Claude Code publishes for it.
///   `aborted` and `error` are ends too: all three want a human before anything else happens.
/// - A turn in flight maps to `busy`, which carries the same two states it does for Claude
///   Code: generating, or blocked inside a tool call. [`build_view`] already separates those
///   two with the in-flight tool set, and that is the right split for Cursor as well.
///
/// Mapping an open turn to no status instead would look more cautious and measures worse.
/// The statusless fallback is transcript mtime, and Cursor writes a conversation's transcript
/// only when the turn ends: through a 25-second generation the file was observed not to move
/// once. A clone was called idle 17 seconds before its turn actually finished because of it.
///
/// Events older than the current Cursor process belong to a previous one. Without that cut a
/// Cursor killed mid-turn leaves a conversation that never sees its `Stop` and reads as
/// working for as long as the log survives.
pub fn read_cursor_sessions(root: &Path, events: &[HookEvent]) -> Vec<Session> {
    let Some((pid, started)) = cursor_process(root) else {
        return Vec::new();
    };
    let stops = latest_live_stop(events);
    let mut order: Vec<&str> = Vec::new();
    let mut ended: HashSet<&str> = HashSet::new();
    for e in events.iter().filter(|e| e.from_cursor && e.ts >= started) {
        let Some(sid) = e.session_id.as_deref() else {
            continue;
        };
        match e.hook_event_name.as_str() {
            "UserPromptSubmit" => {
                if !order.contains(&sid) {
                    order.push(sid);
                }
                ended.remove(sid);
            }
            "SessionEnd" => {
                ended.insert(sid);
            }
            _ => {}
        }
    }
    order
        .into_iter()
        .filter(|sid| !ended.contains(sid))
        .map(|sid| Session {
            pid,
            session_id: sid.to_string(),
            status: Some(
                if stops.contains_key(sid) { "idle" } else { "busy" }.to_string(),
            ),
            alive: true,
            ..Default::default()
        })
        .collect()
}

/// `PreToolUse` minus `PostToolUse`: the tool call each session is sitting inside.
///
/// This is the whole signal on the statusless entrypoints, which publish no `status` at all.
/// It is also what keeps a busy clone from reading as finished: without it the judge sees no
/// in-flight work and no background task and concludes "idle at its prompt". Measured on the
/// 323 pilot views that carry one: 8 false alarms with this evidence, 41 without.
pub fn in_flight_tools(events: &[HookEvent]) -> Vec<&HookEvent> {
    let mut live: Vec<&HookEvent> = Vec::new();
    for e in events {
        let Some(tid) = e.tool_use_id.as_deref() else {
            continue;
        };
        match e.hook_event_name.as_str() {
            "PreToolUse" => live.push(e),
            "PostToolUse" | "PostToolUseFailure" => {
                live.retain(|x| x.tool_use_id.as_deref() != Some(tid));
            }
            _ => {}
        }
    }
    live
}

/// The newest `Stop` per session, dropped once a later `UserPromptSubmit` supersedes it.
///
/// Its `last_assistant_message` goes stale the moment the next turn opens: left in, it
/// described a clone hung inside `curl` as waiting on named pipes, which is what the
/// previous turn had been about.
fn latest_live_stop(events: &[HookEvent]) -> HashMap<&str, &HookEvent> {
    let mut out: HashMap<&str, &HookEvent> = HashMap::new();
    for e in events {
        let Some(sid) = e.session_id.as_deref() else {
            continue;
        };
        match e.hook_event_name.as_str() {
            "Stop" => {
                out.insert(sid, e);
            }
            "UserPromptSubmit" => {
                out.remove(sid);
            }
            _ => {}
        }
    }
    out
}

/// A `StopFailure` matters only while it is the session's last word. Once a normal `Stop`
/// follows it the agent recovered, and showing the error anyway produced confident false
/// alarms off errors twenty minutes stale.
fn current_api_errors(events: &[HookEvent]) -> HashMap<&str, &HookEvent> {
    let mut out: HashMap<&str, &HookEvent> = HashMap::new();
    for e in events {
        let Some(sid) = e.session_id.as_deref() else {
            continue;
        };
        match e.hook_event_name.as_str() {
            "StopFailure" if e.agent_id.is_none() => {
                out.insert(sid, e);
            }
            "Stop" if e.agent_id.is_none() => {
                out.remove(sid);
            }
            _ => {}
        }
    }
    out
}

// ---------------------------------------------------------------------------------------
// The view handed to the model
// ---------------------------------------------------------------------------------------

/// How long each transcript has been untouched. Contents are never read: everything the
/// judge used to get by parsing JSONL now arrives on a hook payload instead.
fn transcript_silence(root: &Path, now: f64) -> HashMap<String, f64> {
    let mut out = HashMap::new();
    // Cursor files its transcripts under `~/.cursor/projects/<workspace>/agent-transcripts/
    // <conversation>/<conversation>.jsonl`, so the stem is the conversation id and the same
    // stem-keyed walk covers both agents.
    let mut stack = vec![
        root.join("home/rmng/.claude/projects"),
        root.join("home/rmng/.cursor/projects"),
    ];
    // Bounded walk: a clone keeps every project it has ever opened, and the tree is shallow.
    let mut budget = 20_000;
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            if budget == 0 {
                return out;
            }
            budget -= 1;
            let path = entry.path();
            if entry.file_type().is_ok_and(|t| t.is_dir()) {
                stack.push(path);
                continue;
            }
            if path.extension().is_none_or(|x| x != "jsonl") {
                continue;
            }
            let Ok(meta) = entry.metadata() else { continue };
            let Ok(mtime) = meta.modified() else { continue };
            let age = now - mtime.duration_since(std::time::UNIX_EPOCH).map_or(now, |d| d.as_secs_f64());
            let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or_default();
            // A subagent transcript is named `agent-<id>.jsonl` under a `subagents/` dir.
            let id = stem.strip_prefix("agent-").unwrap_or(stem).to_string();
            out.entry(id).and_modify(|v| *v = f64::min(*v, age)).or_insert(age);
        }
    }
    out
}

/// Size and staleness of each background task's output file. These live under `/tmp`, not
/// under the home, which is why the whole module works from the container root.
fn background_outputs(root: &Path, now: f64) -> HashMap<String, (u64, f64)> {
    let mut out = HashMap::new();
    let Ok(tmp) = std::fs::read_dir(root.join("tmp")) else {
        return out;
    };
    for claude_dir in tmp.flatten() {
        if !claude_dir.file_name().to_string_lossy().starts_with("claude-") {
            continue;
        }
        // /tmp/claude-*/<slug>/<session>/tasks/*.output
        let Ok(slugs) = std::fs::read_dir(claude_dir.path()) else {
            continue;
        };
        for slug in slugs.flatten() {
            let Ok(sessions) = std::fs::read_dir(slug.path()) else {
                continue;
            };
            for session in sessions.flatten() {
                let Ok(tasks) = std::fs::read_dir(session.path().join("tasks")) else {
                    continue;
                };
                for task in tasks.flatten() {
                    let path = task.path();
                    if path.extension().is_none_or(|x| x != "output") {
                        continue;
                    }
                    let Ok(meta) = task.metadata() else { continue };
                    let Ok(mtime) = meta.modified() else { continue };
                    let age = now
                        - mtime.duration_since(std::time::UNIX_EPOCH).map_or(now, |d| d.as_secs_f64());
                    let id = path.file_stem().and_then(|s| s.to_str()).unwrap_or_default();
                    out.insert(id.to_string(), (meta.len(), age));
                }
            }
        }
    }
    out
}

/// Everything the model is told about one clone.
///
/// Scoping matters. A clone keeps every transcript it has ever written and every event it
/// has ever fired, so an unscoped view hands the judge a graveyard of finished agents and
/// reads it as live trouble. Only live sessions and still-open subagents get in.
pub fn build_view(root: &Path, sessions: &[Session], events: &[HookEvent], now: f64) -> Value {
    let live_ids: HashSet<&str> =
        sessions.iter().filter(|s| s.alive).map(|s| s.session_id.as_str()).collect();

    let tools: Vec<&HookEvent> = in_flight_tools(events)
        .into_iter()
        .filter(|t| t.session_id.as_deref().is_some_and(|s| live_ids.contains(s)))
        .collect();
    // A session sitting inside a tool call is blocked in it, not generating. Both states
    // publish `busy`, so the registry cannot tell them apart and this set is what does.
    let blocked: HashSet<&str> = tools.iter().filter_map(|t| t.session_id.as_deref()).collect();

    let stops = latest_live_stop(events);
    let errors = current_api_errors(events);
    let silence = transcript_silence(root, now);
    let outputs = background_outputs(root, now);

    let quiet = |id: &str| -> f64 { silence.get(id).copied().unwrap_or(f64::MAX) };

    let mut out_sessions = Vec::new();
    let mut out_tasks = Vec::new();
    let mut out_errors = Vec::new();

    for s in sessions.iter().filter(|s| s.alive) {
        let sid = s.session_id.as_str();
        let stop = stops.get(sid);
        // A cli session publishes `status`, and `busy` minus "sitting in a tool call" is
        // generation. A statusless entrypoint falls back to transcript mtime. Reading that
        // fallback from the hook fold instead looks more principled and measured worse: over
        // a Cursor-heavy run it changed four verdicts and was wrong on all four.
        let generating = match s.status.as_deref() {
            Some(status) => status == "busy" && !blocked.contains(sid),
            None => quiet(sid) <= MOVING_WINDOW_S,
        };
        out_sessions.push(json!({
            "id": sid.chars().take(8).collect::<String>(),
            "status": s.status,
            "generating": generating,
            "quiet_for_seconds": quiet(sid).min(1.0e9).round(),
            "agent_last_said": stop.and_then(|e| e.last_assistant_message.clone()),
            // Cursor reports how the turn ended on the stop itself. `aborted` means a person
            // pressed stop and `error` means the turn died, and both are worth the model
            // seeing: neither resumes on its own.
            "turn_ended_with": stop.and_then(|e| e.status.clone()),
        }));
        for task in stop.iter().flat_map(|e| e.background_tasks.iter()) {
            let got = outputs.get(&task.id);
            out_tasks.push(json!({
                "kind": task.kind,
                "what": task.command.clone().or_else(|| task.agent_type.clone()),
                "description": task.description,
                "output_bytes": got.map(|(b, _)| *b),
                "producing_output": got.is_some_and(|(_, age)| *age <= MOVING_WINDOW_S),
                "output_still_for_seconds": got.map_or(0.0, |(_, age)| age.round()),
            }));
        }
        if let Some(err) = errors.get(sid) {
            out_errors.push(json!({
                "session": sid.chars().take(8).collect::<String>(),
                "detail": err.reason,
            }));
        }
    }

    let out_tools: Vec<Value> = tools
        .iter()
        .map(|t| {
            json!({
                "tool": t.tool_name,
                "input": t.tool_input,
                "by": if t.agent_id.is_some() { "subagent" } else { "main agent" },
                // Both stamps come from the clone's own clock, so the difference is sound.
                // Clamped: a duration below zero is not a fact about the world, and shown one
                // the model treated the whole view as untrustworthy and called the clone hung.
                "running_for_seconds": (now - t.ts).max(0.0).round(),
            })
        })
        .collect();

    json!({
        "sessions": out_sessions,
        "background_tasks": out_tasks,
        "in_flight_tool_calls": out_tools,
        "api_errors": out_errors,
    })
}

// ---------------------------------------------------------------------------------------
// Asking the model
// ---------------------------------------------------------------------------------------

/// The question, arrived at over two fleet pilots. Three sentences in here are load-bearing
/// and were each added to fix a measured failure, so read the git history before trimming:
///
/// - "read the WHOLE command" fixes calling `tail -c 120 file` non-terminating because the
///   examples mentioned `tail -f`.
/// - "take it at its word about the writer" is what separates a pipe a nightly job feeds
///   from one the operator must write by hand.
/// - "a background task that finishes wakes the agent" stops a clone with a live task from
///   reading as finished-and-idle.
///
/// One thing deliberately absent: any elapsed-time rule for background tasks. It was tested
/// against all 1167 recorded views and rejected. Misses stayed at exactly 44 while false
/// alarms rose from 35 to 47.
const SYSTEM: &str = r#"You watch coding agents running unattended in containers. Answer one question
about one container.

Will this agent make any further progress WITHOUT a human giving it more input?

Say true only when some machine-driven thing is going to move it along: it is generating
right now, a command it is inside will return, a background task will finish and wake it,
or a subagent will report back.

Say false when nothing will. The common shapes:
  - the agent finished and is sitting at its prompt. Finished still means it needs the
    next instruction from a person, so this is false.
  - it asked a question and stopped.
  - the only thing still running never terminates: a dev server, a file watcher, a REPL.
    The agent left it running on purpose and it will never wake anyone.
  - the only thing still running can only be completed by the operator themselves, such
    as reading a pipe that the person is expected to write into by hand.
  - something it was waiting on already finished and it was never woken.
  - a command that should have returned by now has hung. See below.

in_flight_tool_calls is what the agent is sitting inside right now, with the command and
how long it has been running. The agent can do nothing at all until it returns, so the
whole answer for that session is whether the command itself will return.

A command hangs. That is the case you must not wave through. Ask how long this command
takes when it works, then compare it to running_for_seconds. A command that normally
answers in seconds, still running long after that, is not slow, it is hung, and nothing
is coming: false. Judge against the specific command, never against a fixed number. A
release build or a large dependency install legitimately runs a long time, so a big
running_for_seconds on one of those is still true.

agent_last_said is usually where the agent names what it is waiting on and what will
deliver it. Take it at its word about the writer: if it names a machine, a job, a build,
or another process, that counts as something that will wake it, and no elapsed time on
its own overturns that. If it names the operator, or names nobody, nothing will.

If the agent said how long it expected something to take, and far more time than that has
passed, then whatever was going to wake it is not coming: false. That is the one thing
that does overturn a named writer. quiet_for_seconds is how long the agent has done
nothing, and each task carries how long its output has been still. Do not use those
numbers on their own, only against what the agent itself expected or against what the
task plainly is.

Judge each outstanding item by what it plainly is, and read the WHOLE command before you
decide. The flags decide it far more often than the program name: `tail -c 120 file`
prints and exits, `tail -f file` never does. Same program, opposite answers. `cargo build`
returns. `npm run dev` does not. A pipe read returns if a scheduled job or another process
feeds it, not if a person has to. If a command would return on its own, say true even if
it is slow.

producing_output tells you whether a task is emitting right now. generating tells you
whether the agent itself is writing tokens, which is false while it waits inside a tool
call. Either one being true means true.

A background task that finishes wakes the agent by itself and it takes its next turn. So
a background task that is going to finish is true, even though the agent is doing nothing
while it waits. The agent being at its prompt with a task still running is not the same
as the agent being done.

Reply with only a JSON object:
{"will_progress": true, "reason": "one short sentence"}"#;

/// Edges of the elapsed-time buckets the cache key rounds to, in seconds.
const BUCKETS: [f64; 9] = [30.0, 60.0, 120.0, 300.0, 600.0, 1200.0, 2400.0, 4800.0, 9600.0];

/// Remembers what the model said, so a clone sitting in one state is asked once rather than
/// once per tick.
///
/// The monitor ticks every 4s and a view carries elapsed seconds, so a naive cache would
/// miss on every tick and ask ~900 times an hour per clone. Rounding every `*_seconds` field
/// to a bucket means a clone is re-asked only when it crosses one or when something real
/// changes. On the pilot fleet this held 32 clones to 381 calls over six hours.
#[derive(Default)]
pub struct Judge {
    answers: StdRwLock<HashMap<String, (bool, String)>>,
}

fn bucket(seconds: f64) -> usize {
    BUCKETS.iter().position(|edge| seconds < *edge).unwrap_or(BUCKETS.len())
}

/// The view with every duration replaced by its bucket, rendered canonically. Two views with
/// the same key would get the same answer, so the second one need not be asked.
fn cache_key(view: &Value) -> String {
    fn walk(node: &Value) -> Value {
        match node {
            Value::Object(map) => Value::Object(
                map.iter()
                    .map(|(k, v)| {
                        let v = match v.as_f64() {
                            Some(n) if k.ends_with("_seconds") => json!(bucket(n)),
                            _ => walk(v),
                        };
                        (k.clone(), v)
                    })
                    // serde_json::Map preserves insertion order unless `preserve_order` is
                    // off; collecting into a BTreeMap-backed value would be nicer, but the
                    // view is built with fixed key order so this is already canonical.
                    .collect(),
            ),
            Value::Array(items) => Value::Array(items.iter().map(walk).collect()),
            other => other.clone(),
        }
    }
    walk(view).to_string()
}

#[derive(Deserialize)]
struct Answer {
    #[serde(default)]
    will_progress: Option<bool>,
    #[serde(default)]
    reason: Option<String>,
}

impl Judge {
    pub fn new() -> Self {
        Self::default()
    }

    /// `(state, reason, asked)`. `asked` is false when the cache answered.
    pub async fn resolve(
        &self,
        http: &reqwest::Client,
        cfg: &wire::OpenRouterConfig,
        view: &Value,
    ) -> Result<(wire::MonitorState, String, bool)> {
        let key = cache_key(view);
        if let Some((progress, reason)) = self.answers.read().unwrap().get(&key).cloned() {
            return Ok((apply_verdict(Some(progress)), reason, false));
        }
        let answer = ask(http, cfg, view).await?;
        let progress = answer.will_progress == Some(true);
        let reason = answer.reason.unwrap_or_default();
        self.answers.write().unwrap().insert(key, (progress, reason.clone()));
        Ok((apply_verdict(Some(progress)), reason, true))
    }

    /// Drop cached answers once the fleet has moved on, mirroring `ActivityBus::retain`.
    pub fn prune(&self, max_entries: usize) {
        let mut answers = self.answers.write().unwrap();
        if answers.len() > max_entries {
            answers.clear();
        }
    }
}

async fn ask(http: &reqwest::Client, cfg: &wire::OpenRouterConfig, view: &Value) -> Result<Answer> {
    let body = json!({
        "model": cfg.model,
        "messages": [
            {"role": "system", "content": SYSTEM},
            {"role": "user", "content": serde_json::to_string_pretty(view)?},
        ],
        "max_tokens": 3000,
        "temperature": 0,
        // Off is a cost and latency choice, not an accuracy one: minimal, low, medium, high
        // and a 256-token budget all scored the same on the fixture set.
        "reasoning": {"enabled": false},
    });
    let resp = http
        .post(OPENROUTER_CHAT)
        .timeout(ASK_TIMEOUT)
        .header("Authorization", format!("Bearer {}", cfg.key))
        .json(&body)
        .send()
        .await?;
    let status = resp.status();
    let text = resp.text().await.unwrap_or_default();
    if !status.is_success() {
        bail!("openrouter {}: {}", status.as_u16(), snippet(&text));
    }
    let parsed: Value = serde_json::from_str(&text)?;
    let content = parsed["choices"][0]["message"]["content"].as_str().unwrap_or_default();
    Ok(parse_answer(content))
}

/// Pull the JSON object out of the reply. Models fence it, prefix it, or return it bare, and
/// a reply that yields nothing usable must read as "no" rather than as an error.
fn parse_answer(content: &str) -> Answer {
    let (Some(start), Some(end)) = (content.find('{'), content.rfind('}')) else {
        return Answer { will_progress: None, reason: None };
    };
    serde_json::from_str(&content[start..=end])
        .unwrap_or(Answer { will_progress: None, reason: None })
}

fn snippet(s: &str) -> String {
    s.chars().take(200).collect()
}

/// Does this key work? Used by `POST /api/config/test`.
pub async fn probe_key(http: &reqwest::Client, key: &str) -> (bool, String) {
    if key.is_empty() {
        return (false, "no OpenRouter key set — clones will never read as working".into());
    }
    let resp = http
        .get(OPENROUTER_KEY_INFO)
        .timeout(ASK_TIMEOUT)
        .header("Authorization", format!("Bearer {key}"))
        .send()
        .await;
    match resp {
        Err(e) => (false, format!("OpenRouter unreachable: {e}")),
        Ok(r) if !r.status().is_success() => {
            let code = r.status().as_u16();
            let body = r.text().await.unwrap_or_default();
            (false, format!("OpenRouter rejected the key ({code}): {}", snippet(&body)))
        }
        Ok(r) => {
            let body: Value = r.json().await.unwrap_or_else(|_| json!({}));
            let limit = body["data"]["limit"].as_f64();
            let used = body["data"]["usage"].as_f64().unwrap_or(0.0);
            match limit {
                Some(l) => (true, format!("key works — ${used:.2} used of ${l:.2}")),
                None => (true, format!("key works — ${used:.2} used, no limit set")),
            }
        }
    }
}

// ---------------------------------------------------------------------------------------
// The fleet pass
// ---------------------------------------------------------------------------------------

/// Decide working-vs-stuck for every running clone, in one concurrent pass.
///
/// Reading a clone is blocking file IO (a directory of session records, the hook log, and two
/// shallow walks), so each clone's read happens on the blocking pool; the model calls that
/// follow are ordinary concurrent futures.
///
/// With no OpenRouter key configured this still runs, and every clone that the files cannot
/// settle comes back `Idle`. That is the whole "no key" behaviour: no guess, no model, and no
/// clone ever reported as working. Per-clone token accounting is elsewhere and unaffected.
pub async fn resolve_fleet(
    app: &crate::app::App,
    ids: Vec<String>,
) -> HashMap<String, wire::MonitorState> {
    let cfg = app.config();
    let data_dir = cfg.data_dir.clone();
    let openrouter = cfg.openrouter.clone();

    let reads = ids.into_iter().map(|id| {
        let data_dir = data_dir.clone();
        async move {
            let id2 = id.clone();
            let read = tokio::task::spawn_blocking(move || read_clone(&data_dir, &id2)).await;
            (id, read.ok().flatten())
        }
    });
    let read = futures::future::join_all(reads).await;

    let decided = read.into_iter().map(|(id, snapshot)| {
        let openrouter = openrouter.clone();
        async move {
            // A clone whose home is not readable right now (restarting, or `homes` has not
            // repointed its symlink yet) tells us nothing. Leaving it Idle is the honest
            // reading: we cannot see anything that would wake it.
            let Some((verdict, view, why)) = snapshot else {
                return (id, wire::MonitorState::Idle);
            };
            match verdict {
                Verdict::Offline => (id, wire::MonitorState::Idle),
                Verdict::Stuck => {
                    tracing::debug!(target: "stuck", "clone {id}: idle — {why}");
                    (id, wire::MonitorState::Idle)
                }
                Verdict::Ask if openrouter.key.is_empty() => (id, wire::MonitorState::Idle),
                Verdict::Ask => match app.stuck.resolve(&app.http, &openrouter, &view).await {
                    Ok((state, reason, asked)) => {
                        if asked {
                            tracing::debug!(
                                target: "stuck",
                                "clone {id}: {state:?} — {reason}"
                            );
                        }
                        (id, state)
                    }
                    Err(e) => {
                        // A provider outage must not flip the fleet to working. Idle is the
                        // same answer an unset key gives, and it is the safe one.
                        tracing::warn!(target: "stuck", "clone {id}: asking failed: {e:#}");
                        (id, wire::MonitorState::Idle)
                    }
                },
            }
        }
    });
    let out: HashMap<String, wire::MonitorState> =
        futures::future::join_all(decided).await.into_iter().collect();

    // The cache is keyed by view content, not by clone, so it cannot be pruned per clone.
    // Clearing it wholesale once it outgrows the fleet costs one round of re-asking.
    app.stuck.prune(out.len().saturating_mul(16).max(256));
    out
}

/// One clone's files, read on the blocking pool. `None` when its home is not reachable.
///
/// The third element says why, for a file-decided verdict. An operator asking "why is this
/// clone grey" deserves an answer better than silence, and a dialog nobody noticed is the
/// most actionable of them.
fn read_clone(data_dir: &str, id: &str) -> Option<(Verdict, Value, String)> {
    let root = clone_root(data_dir, id)?;
    let mut sessions = read_sessions(&root);
    // Cursor's conversations come out of the hook log, so a clone running Cursor has to read
    // it before the verdict rather than after. Gated on Cursor actually running, which is two
    // small file reads, so a clone without it keeps the fast path below untouched.
    let mut events = Vec::new();
    if cursor_process(&root).is_some() {
        events = read_hook_events(&root);
        sessions.extend(read_cursor_sessions(&root, &events));
    }
    let verdict = clone_state(&sessions, true);
    if verdict != Verdict::Ask {
        // Nothing else is read: three quarters of a fleet settles here, and the cheapest
        // request is the one not made.
        let why = match blocked_reasons(&sessions).as_slice() {
            [] if sessions.iter().all(|s| !s.alive) => "no live agent session".to_string(),
            [] => "every session is idle at its prompt".to_string(),
            waiting => format!(
                "waiting on {}",
                waiting
                    .iter()
                    .map(|(id, what)| format!("{id} ({what})"))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        };
        return Some((verdict, Value::Null, why));
    }
    if events.is_empty() {
        events = read_hook_events(&root);
    }
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0.0, |d| d.as_secs_f64());
    // Stamped AFTER the log is read, never before. A hook that fires during the read would
    // otherwise carry a timestamp later than `now`, and the tool-call age computed from the
    // pair came out negative. The model noticed before I did, calling a running_for_seconds
    // of -1 "invalid" and reading the clone as hung because of it.
    let now = events.iter().map(|e| e.ts).fold(now, f64::max);
    Some((verdict, build_view(&root, &sessions, &events, now), String::new()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session(status: Option<&str>, alive: bool) -> Session {
        Session {
            session_id: format!("sid-{}", status.unwrap_or("none")),
            status: status.map(str::to_string),
            alive,
            ..Default::default()
        }
    }

    #[test]
    fn a_stopped_container_is_offline_whatever_its_sessions_say() {
        assert_eq!(clone_state(&[session(Some("busy"), true)], false), Verdict::Offline);
        assert_eq!(clone_state(&[], false), Verdict::Offline);
    }

    #[test]
    fn nothing_running_needs_a_person() {
        // No session at all, and a session whose process is gone, are the same thing: there
        // is nothing left that could wake this clone.
        assert_eq!(clone_state(&[], true), Verdict::Stuck);
        assert_eq!(clone_state(&[session(Some("busy"), false)], true), Verdict::Stuck);
    }

    #[test]
    fn a_dialog_needs_a_person_even_beside_a_busy_session() {
        let sessions = [session(Some("busy"), true), session(Some("waiting"), true)];
        assert_eq!(clone_state(&sessions, true), Verdict::Stuck);
    }

    #[test]
    fn every_session_idle_is_decided_without_the_model() {
        // The commonest state in a fleet, and it costs nothing.
        assert_eq!(clone_state(&[session(Some("idle"), true)], true), Verdict::Stuck);
        let two = [session(Some("idle"), true), session(Some("idle"), true)];
        assert_eq!(clone_state(&two, true), Verdict::Stuck);
    }

    #[test]
    fn a_dead_session_does_not_hold_a_clone_out_of_the_idle_shortcut() {
        let sessions = [session(Some("idle"), true), session(Some("busy"), false)];
        assert_eq!(clone_state(&sessions, true), Verdict::Stuck);
    }

    #[test]
    fn one_statusless_session_beside_an_idle_one_still_asks() {
        // The bug this pins: dropping `None` from the status set collapses this to `{idle}`
        // and calls a clone stuck while its Cursor session is mid-turn.
        let sessions = [session(Some("idle"), true), session(None, true)];
        assert_eq!(clone_state(&sessions, true), Verdict::Ask);
    }

    #[test]
    fn busy_and_shell_and_statusless_all_reach_the_model() {
        for status in [Some("busy"), Some("shell"), None] {
            assert_eq!(
                clone_state(&[session(status, true)], true),
                Verdict::Ask,
                "status {status:?} must be asked, not guessed"
            );
        }
    }

    #[test]
    fn an_unreadable_record_is_a_torn_read_not_an_absent_session() {
        let torn = Session { alive: true, status: Some("__unreadable__".into()), ..Default::default() };
        assert_eq!(clone_state(&[torn], true), Verdict::Ask);
    }

    #[test]
    fn only_a_clear_yes_reads_as_working() {
        assert_eq!(apply_verdict(Some(true)), wire::MonitorState::Working);
        assert_eq!(apply_verdict(Some(false)), wire::MonitorState::Idle);
        // A malformed or missing answer must never invent progress.
        assert_eq!(apply_verdict(None), wire::MonitorState::Idle);
    }

    #[test]
    fn blocked_reasons_name_what_each_dialog_wants() {
        let mut s = session(Some("waiting"), true);
        s.session_id = "79650537-2a01-4479".into();
        s.waiting_for = Some("permission prompt".into());
        let got = blocked_reasons(&[s, session(Some("busy"), true)]);
        assert_eq!(got, vec![("79650537".to_string(), "permission prompt".to_string())]);
    }

    // -- the hook fold ------------------------------------------------------------------

    fn event(name: &str, sid: &str, aid: Option<&str>) -> HookEvent {
        HookEvent {
            hook_event_name: name.into(),
            session_id: Some(sid.into()),
            agent_id: aid.map(str::to_string),
            ..Default::default()
        }
    }

    #[test]
    fn in_flight_is_pre_minus_post() {
        let mut pre_a = event("PreToolUse", "s", None);
        pre_a.tool_use_id = Some("t1".into());
        pre_a.tool_name = Some("Bash".into());
        let mut pre_b = event("PreToolUse", "s", None);
        pre_b.tool_use_id = Some("t2".into());
        let mut post_a = event("PostToolUse", "s", None);
        post_a.tool_use_id = Some("t1".into());

        let events = [pre_a, pre_b, post_a];
        let live = in_flight_tools(&events);
        assert_eq!(live.len(), 1);
        assert_eq!(live[0].tool_use_id.as_deref(), Some("t2"));
    }

    #[test]
    fn a_failed_tool_call_is_not_left_in_flight() {
        let mut pre = event("PreToolUse", "s", None);
        pre.tool_use_id = Some("t1".into());
        let mut fail = event("PostToolUseFailure", "s", None);
        fail.tool_use_id = Some("t1".into());
        assert!(in_flight_tools(&[pre, fail]).is_empty());
    }

    #[test]
    fn the_previous_turns_closing_words_are_dropped_once_a_new_turn_opens() {
        // Left in, this described a clone hung inside `curl` as waiting on named pipes.
        let mut stop = event("Stop", "s", None);
        stop.last_assistant_message = Some("both FIFOs are still empty".into());
        let events = [stop, event("UserPromptSubmit", "s", None)];
        assert!(latest_live_stop(&events).is_empty());
    }

    #[test]
    fn an_api_error_the_session_recovered_from_is_not_reported() {
        let events = [
            event("StopFailure", "s", None),
            event("UserPromptSubmit", "s", None),
            event("Stop", "s", None),
        ];
        assert!(current_api_errors(&events).is_empty(), "a later Stop means it recovered");

        let still_broken = [event("Stop", "s", None), event("StopFailure", "s", None)];
        assert_eq!(current_api_errors(&still_broken).len(), 1);
    }

    // -- the answer cache ---------------------------------------------------------------

    #[test]
    fn the_cache_key_ignores_a_clock_tick_but_not_a_bucket_crossing() {
        let at = |secs: f64| json!({"sessions": [{"quiet_for_seconds": secs, "status": "busy"}]});
        // Same bucket: a tick of the clock must not cost a model call.
        assert_eq!(cache_key(&at(61.0)), cache_key(&at(119.0)));
        // Crossing one must.
        assert_ne!(cache_key(&at(119.0)), cache_key(&at(121.0)));
        // A real change must, at the same elapsed time.
        let other = json!({"sessions": [{"quiet_for_seconds": 61.0, "status": "shell"}]});
        assert_ne!(cache_key(&at(61.0)), cache_key(&other));
    }

    #[test]
    fn buckets_are_ordered_and_saturate() {
        assert_eq!(bucket(0.0), 0);
        assert_eq!(bucket(29.9), 0);
        assert_eq!(bucket(30.0), 1);
        assert_eq!(bucket(1.0e9), BUCKETS.len());
    }

    // -- parsing the reply --------------------------------------------------------------

    #[test]
    fn the_answer_survives_fencing_and_prose() {
        assert_eq!(parse_answer(r#"{"will_progress": true, "reason": "x"}"#).will_progress, Some(true));
        let fenced = "```json\n{\"will_progress\": false, \"reason\": \"hung\"}\n```";
        assert_eq!(parse_answer(fenced).will_progress, Some(false));
        let chatty = "Sure! {\"will_progress\": true, \"reason\": \"building\"} hope that helps";
        assert_eq!(parse_answer(chatty).will_progress, Some(true));
    }

    #[test]
    fn an_unusable_reply_reads_as_no_rather_than_as_yes() {
        for reply in ["", "I cannot determine that.", "{oops", "{\"will_progress\": \"yes\"}"] {
            assert_eq!(
                apply_verdict(parse_answer(reply).will_progress),
                wire::MonitorState::Idle,
                "reply {reply:?} must not read as working"
            );
        }
    }

    // -- reading a clone off disk -------------------------------------------------------

    /// A fake clone root: `<root>/home/rmng/.claude/sessions/<pid>.json` plus a `<root>/proc`
    /// carrying the one thing the liveness check reads.
    fn fake_clone(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("rmng-stuck-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("home/rmng/.claude/sessions")).unwrap();
        dir
    }

    /// `/proc/<pid>/stat` as the kernel writes it: pid, `(comm)`, then the numbered fields.
    /// `starttime` is field 22, which is token 19 after the last `)`.
    fn write_proc(root: &Path, pid: i64, starttime: &str) {
        let dir = root.join("proc").join(pid.to_string());
        std::fs::create_dir_all(&dir).unwrap();
        let mut fields = vec!["S".to_string()];
        fields.extend(std::iter::repeat_n("0".to_string(), 18));
        fields.push(starttime.to_string());
        // A comm containing a space and a paren is why the parser splits on the LAST ')'.
        std::fs::write(dir.join("stat"), format!("{pid} (claude (main)) {}\n", fields.join(" "))).unwrap();
    }

    fn write_session(root: &Path, pid: i64, proc_start: &str, status: Option<&str>) {
        let body = json!({
            "pid": pid,
            "sessionId": format!("session-{pid}"),
            "entrypoint": "cli",
            "procStart": proc_start,
            "status": status,
        });
        std::fs::write(
            root.join("home/rmng/.claude/sessions").join(format!("{pid}.json")),
            serde_json::to_vec(&body).unwrap(),
        )
        .unwrap();
    }

    #[test]
    fn liveness_matches_on_proc_start_not_just_on_the_pid() {
        let root = fake_clone("liveness");
        write_session(&root, 9960, "79868452", Some("idle"));
        write_proc(&root, 9960, "79868452");
        let live = read_sessions(&root);
        assert_eq!(live.len(), 1);
        assert!(live[0].alive, "matching procStart is alive");
        assert_eq!(live[0].status.as_deref(), Some("idle"));

        // Same pid, different start time: a DIFFERENT process reused the number.
        write_proc(&root, 9960, "99999999");
        assert!(!read_sessions(&root)[0].alive, "a reused pid must not read as alive");

        // No such process at all.
        std::fs::remove_dir_all(root.join("proc")).unwrap();
        assert!(!read_sessions(&root)[0].alive);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_statusless_record_parses_with_no_status_rather_than_failing() {
        let root = fake_clone("statusless");
        // What the Cursor extension and the agent-wrapper actually write: no `status` key.
        std::fs::write(
            root.join("home/rmng/.claude/sessions/873548.json"),
            br#"{"pid":873548,"sessionId":"864e6977-x","entrypoint":"claude-vscode","procStart":"104543106"}"#,
        )
        .unwrap();
        write_proc(&root, 873548, "104543106");
        let live = read_sessions(&root);
        assert_eq!(live.len(), 1);
        assert!(live[0].alive);
        assert_eq!(live[0].status, None, "no status key at all, not a null one");
        assert_eq!(clone_state(&live, true), Verdict::Ask);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_torn_record_reads_as_maybe_running_rather_than_as_gone() {
        let root = fake_clone("torn");
        std::fs::write(root.join("home/rmng/.claude/sessions/1.json"), b"{\"pid\": 1, \"proc").unwrap();
        let live = read_sessions(&root);
        assert_eq!(live.len(), 1);
        assert!(live[0].alive);
        assert_eq!(clone_state(&live, true), Verdict::Ask);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_clone_with_no_registry_at_all_is_stuck_not_a_crash() {
        let dir = std::env::temp_dir().join(format!("rmng-stuck-empty-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        assert!(read_sessions(&dir).is_empty());
        assert_eq!(clone_state(&read_sessions(&dir), true), Verdict::Stuck);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_hook_log_survives_a_torn_final_line() {
        let root = fake_clone("hooklog");
        std::fs::create_dir_all(root.join("home/rmng/.rmng")).unwrap();
        std::fs::write(
            root.join("home/rmng/.rmng/agent-events.jsonl"),
            "{\"hook_event_name\":\"UserPromptSubmit\",\"session_id\":\"s\",\"ts\":1.0}\n\
             \n\
             {\"hook_event_name\":\"PreToolUse\",\"session_id\":\"s\",\"tool_use",
        )
        .unwrap();
        let events = read_hook_events(&root);
        assert_eq!(events.len(), 1, "the half-written line is skipped, not fatal");
        assert_eq!(events[0].hook_event_name, "UserPromptSubmit");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn the_view_reports_a_blocked_session_as_not_generating() {
        let root = fake_clone("view");
        let mut sessions = vec![session(Some("busy"), true)];
        sessions[0].session_id = "s".into();
        let mut pre = event("PreToolUse", "s", None);
        pre.tool_use_id = Some("t1".into());
        pre.tool_name = Some("Bash".into());
        pre.tool_input = Some(r#"{"command":"curl -sS http://127.0.0.1:9099/health"}"#.into());
        pre.ts = 100.0;

        let view = build_view(&root, &sessions, &[pre], 609.0);
        // `busy` covers both writing tokens and sitting in a tool call. Reading it as
        // generating is what let a hung clone read as alive for as long as it hung.
        assert_eq!(view["sessions"][0]["generating"], json!(false));
        assert_eq!(view["in_flight_tool_calls"][0]["running_for_seconds"], json!(509.0));
        assert_eq!(view["in_flight_tool_calls"][0]["by"], json!("main agent"));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_tool_call_stamped_after_collection_never_reports_a_negative_age() {
        let root = fake_clone("clockrace");
        let mut sessions = vec![session(Some("busy"), true)];
        sessions[0].session_id = "s".into();
        let mut pre = event("PreToolUse", "s", None);
        pre.tool_use_id = Some("t1".into());
        pre.ts = 500.0;
        // The hook fired while the log was being read, so its stamp is ahead of `now`.
        let view = build_view(&root, &sessions, &[pre], 499.0);
        assert_eq!(view["in_flight_tool_calls"][0]["running_for_seconds"], json!(0.0));
        let _ = std::fs::remove_dir_all(&root);
    }

    // -----------------------------------------------------------------------------------
    // Cursor
    // -----------------------------------------------------------------------------------

    /// Cursor running in a fake clone: the lock file naming its pid, and a `/proc` entry
    /// whose `cmdline` is the main process rather than a renderer child.
    fn write_cursor(root: &Path, pid: i64, args: &[&str]) {
        let cfg = root.join("home/rmng/.config/Cursor");
        std::fs::create_dir_all(&cfg).unwrap();
        std::fs::write(cfg.join("code.lock"), format!("{pid}\n")).unwrap();
        let dir = root.join("proc").join(pid.to_string());
        std::fs::create_dir_all(&dir).unwrap();
        let mut raw = Vec::new();
        for arg in args {
            raw.extend_from_slice(arg.as_bytes());
            raw.push(0);
        }
        std::fs::write(dir.join("cmdline"), raw).unwrap();
    }

    fn write_log(root: &Path, lines: &[Value]) {
        let dir = root.join("home/rmng/.rmng");
        std::fs::create_dir_all(&dir).unwrap();
        let body: String =
            lines.iter().map(|l| format!("{l}\n")).collect::<Vec<_>>().concat();
        std::fs::write(dir.join("agent-events.jsonl"), body).unwrap();
    }

    /// Comfortably after the lock file this test just wrote, whose mtime is now.
    fn soon() -> f64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs_f64()
            + 30.0
    }

    #[test]
    fn cursor_event_names_arrive_as_claude_ones() {
        let root = fake_clone("cursorname");
        write_log(
            &root,
            &[
                json!({"hook_event_name": "beforeSubmitPrompt", "session_id": "c1", "ts": 1.0}),
                json!({"hook_event_name": "preToolUse", "session_id": "c1",
                       "tool_use_id": "t1", "ts": 2.0}),
                json!({"hook_event_name": "postToolUseFailure", "session_id": "c1",
                       "tool_use_id": "t1", "ts": 3.0}),
                json!({"hook_event_name": "stop", "session_id": "c1", "status": "aborted",
                       "ts": 4.0}),
            ],
        );
        let events = read_hook_events(&root);
        let names: Vec<&str> = events.iter().map(|e| e.hook_event_name.as_str()).collect();
        assert_eq!(names, ["UserPromptSubmit", "PreToolUse", "PostToolUseFailure", "Stop"]);
        assert!(events.iter().all(|e| e.from_cursor));
        assert_eq!(events[3].status.as_deref(), Some("aborted"));
        // The point of the translation: the existing folds work on Cursor unchanged.
        assert!(in_flight_tools(&events).is_empty());
        assert!(latest_live_stop(&events).contains_key("c1"));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn one_event_registered_twice_is_read_once() {
        let root = fake_clone("cursordupe");
        // Cursor loads a hook file as user config and again as project config when the
        // workspace root holds it, then fires both copies milliseconds apart.
        write_log(
            &root,
            &[
                json!({"hook_event_name": "preToolUse", "session_id": "c1",
                       "tool_use_id": "t1", "ts": 100.000}),
                json!({"hook_event_name": "preToolUse", "session_id": "c1",
                       "tool_use_id": "t1", "ts": 100.011}),
                // A different call, however close, is a different call.
                json!({"hook_event_name": "preToolUse", "session_id": "c1",
                       "tool_use_id": "t2", "ts": 100.020}),
            ],
        );
        let events = read_hook_events(&root);
        assert_eq!(events.len(), 2);
        assert_eq!(in_flight_tools(&events).len(), 2);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_cursor_turn_in_flight_is_a_busy_session() {
        let root = fake_clone("cursorlive");
        write_cursor(&root, 900, &["/usr/share/cursor/cursor"]);
        let t = soon();
        write_log(
            &root,
            &[json!({"hook_event_name": "beforeSubmitPrompt", "session_id": "c1", "ts": t})],
        );
        let events = read_hook_events(&root);
        let sessions = read_cursor_sessions(&root, &events);
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].session_id, "c1");
        // Not statusless: Cursor writes a transcript only at turn end, so the mtime fallback
        // a statusless session gets would read a live generation as silence.
        assert_eq!(sessions[0].status.as_deref(), Some("busy"));
        assert!(sessions[0].alive);
        let view = build_view(&root, &sessions, &events, soon() + 5.0);
        assert_eq!(view["sessions"][0]["generating"], json!(true));
        // A clone worked only through Cursor has no Claude session at all, and used to read
        // as stuck for that reason alone.
        assert_eq!(clone_state(&sessions, true), Verdict::Ask);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_finished_cursor_turn_is_idle_however_it_ended() {
        for status in ["completed", "aborted", "error"] {
            let root = fake_clone(&format!("cursorend-{status}"));
            write_cursor(&root, 900, &["/usr/share/cursor/cursor"]);
            let t = soon();
            write_log(
                &root,
                &[
                    json!({"hook_event_name": "beforeSubmitPrompt", "session_id": "c1",
                           "ts": t}),
                    json!({"hook_event_name": "stop", "session_id": "c1", "status": status,
                           "ts": t + 1.0}),
                ],
            );
            let events = read_hook_events(&root);
            let sessions = read_cursor_sessions(&root, &events);
            assert_eq!(sessions[0].status.as_deref(), Some("idle"), "{status}");
            assert_eq!(clone_state(&sessions, true), Verdict::Stuck, "{status}");
            let _ = std::fs::remove_dir_all(&root);
        }
    }

    #[test]
    fn an_abort_that_closes_its_tool_after_the_stop_still_reads_as_ended() {
        let root = fake_clone("cursorabort");
        write_cursor(&root, 900, &["/usr/share/cursor/cursor"]);
        let t = soon();
        // The real order, measured: Cursor sends `stop` and only then the failure that
        // retires the tool. Anything that took the last event as the state would call this
        // clone mid-turn.
        write_log(
            &root,
            &[
                json!({"hook_event_name": "beforeSubmitPrompt", "session_id": "c1", "ts": t}),
                json!({"hook_event_name": "preToolUse", "session_id": "c1",
                       "tool_use_id": "t1", "ts": t + 1.0}),
                json!({"hook_event_name": "stop", "session_id": "c1", "status": "aborted",
                       "ts": t + 22.0}),
                json!({"hook_event_name": "postToolUseFailure", "session_id": "c1",
                       "tool_use_id": "t1", "ts": t + 22.4}),
            ],
        );
        let events = read_hook_events(&root);
        let sessions = read_cursor_sessions(&root, &events);
        assert_eq!(sessions[0].status.as_deref(), Some("idle"));
        assert_eq!(clone_state(&sessions, true), Verdict::Stuck);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_cursor_subagent_is_not_a_session_of_its_own() {
        let root = fake_clone("cursorsub");
        write_cursor(&root, 900, &["/usr/share/cursor/cursor"]);
        let t = soon();
        // A subagent runs under its own conversation id, never receives a prompt of its own,
        // and never fires its own stop: it ends with a SubagentStop on the parent. Counted as
        // a session it would sit mid-turn forever and no clone would ever read as stuck.
        write_log(
            &root,
            &[
                json!({"hook_event_name": "beforeSubmitPrompt", "session_id": "parent",
                       "ts": t}),
                json!({"hook_event_name": "preToolUse", "session_id": "parent",
                       "tool_use_id": "task1", "ts": t + 1.0}),
                json!({"hook_event_name": "subagentStart", "session_id": "parent", "ts": t + 2.0}),
                json!({"hook_event_name": "preToolUse", "session_id": "sub",
                       "tool_use_id": "s1", "ts": t + 3.0}),
                json!({"hook_event_name": "postToolUse", "session_id": "sub",
                       "tool_use_id": "s1", "ts": t + 4.0}),
                json!({"hook_event_name": "subagentStop", "session_id": "parent", "ts": t + 5.0}),
                json!({"hook_event_name": "postToolUse", "session_id": "parent",
                       "tool_use_id": "task1", "ts": t + 6.0}),
                json!({"hook_event_name": "stop", "session_id": "parent",
                       "status": "completed", "ts": t + 7.0}),
            ],
        );
        let events = read_hook_events(&root);
        let sessions = read_cursor_sessions(&root, &events);
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].session_id, "parent");
        assert_eq!(clone_state(&sessions, true), Verdict::Stuck);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_cursor_turn_sitting_in_a_tool_call_is_blocked_not_generating() {
        let root = fake_clone("cursorblocked");
        write_cursor(&root, 900, &["/usr/share/cursor/cursor"]);
        let t = soon();
        write_log(
            &root,
            &[
                json!({"hook_event_name": "beforeSubmitPrompt", "session_id": "c1", "ts": t}),
                json!({"hook_event_name": "preToolUse", "session_id": "c1",
                       "tool_name": "Shell", "tool_use_id": "t1", "ts": t + 1.0}),
            ],
        );
        let events = read_hook_events(&root);
        let sessions = read_cursor_sessions(&root, &events);
        let view = build_view(&root, &sessions, &events, t + 400.0);
        // `busy` covers generating and waiting on a command. The unmatched tool call is what
        // tells them apart, and the model needs the command itself to judge the second.
        assert_eq!(view["sessions"][0]["generating"], json!(false));
        assert_eq!(view["in_flight_tool_calls"][0]["tool"], json!("Shell"));
        assert_eq!(view["in_flight_tool_calls"][0]["running_for_seconds"], json!(399.0));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_turn_from_a_previous_cursor_does_not_outlive_it() {
        let root = fake_clone("cursorstale");
        write_cursor(&root, 900, &["/usr/share/cursor/cursor"]);
        // Killed mid-turn, so the stop never came. The lock file was rewritten when Cursor
        // came back, which is what dates these events to the process that is gone.
        write_log(
            &root,
            &[
                json!({"hook_event_name": "beforeSubmitPrompt", "session_id": "old", "ts": 1.0}),
                json!({"hook_event_name": "preToolUse", "session_id": "old",
                       "tool_use_id": "t1", "ts": 2.0}),
            ],
        );
        let events = read_hook_events(&root);
        assert!(read_cursor_sessions(&root, &events).is_empty());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn cursor_gone_means_no_cursor_sessions() {
        let root = fake_clone("cursorgone");
        let t = soon();
        write_log(
            &root,
            &[json!({"hook_event_name": "beforeSubmitPrompt", "session_id": "c1", "ts": t})],
        );
        let events = read_hook_events(&root);
        // No lock file at all.
        assert!(read_cursor_sessions(&root, &events).is_empty());

        // A lock naming a pid that a different process now holds.
        write_cursor(&root, 901, &["/usr/bin/python3", "something.py"]);
        assert!(read_cursor_sessions(&root, &events).is_empty());

        // A lock naming one of Cursor's own renderer children rather than the main process.
        write_cursor(&root, 902, &["/usr/share/cursor/cursor", "--type=zygote"]);
        assert!(read_cursor_sessions(&root, &events).is_empty());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn cursor_mid_turn_beside_an_idle_claude_session_is_not_stuck() {
        let root = fake_clone("cursorboth");
        write_cursor(&root, 900, &["/usr/share/cursor/cursor"]);
        let t = soon();
        write_log(
            &root,
            &[json!({"hook_event_name": "beforeSubmitPrompt", "session_id": "c1", "ts": t})],
        );
        let events = read_hook_events(&root);
        let mut sessions = vec![session(Some("idle"), true)];
        sessions.extend(read_cursor_sessions(&root, &events));
        assert_eq!(clone_state(&sessions, true), Verdict::Ask);
        let _ = std::fs::remove_dir_all(&root);
    }
}
