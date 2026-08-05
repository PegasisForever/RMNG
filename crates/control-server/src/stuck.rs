//! Is a clone going to make progress on its own, or is it waiting on a person?
//!
//! One question, two answers. **Stuck** means Claude Code will not get further without a
//! human giving it more input. **Working** means something machine-driven will move it
//! along. That definition collapses most of what used to need judgement: an agent sitting
//! at its prompt having finished the task is stuck, because it needs the next instruction;
//! so is one that asked a question, and so is one that started a dev server and handed back,
//! because the server runs forever and will never wake anybody.
//!
//! **The unit of judgement is one session, never a clone.** A clone runs as many agent
//! sessions as its operator opened, they are independent, and the clone is working when any
//! single one of them is. So each session is decided on its own evidence and the clone takes
//! the OR ([`resolve_fleet`]). Deciding a clone from the union of its sessions is what let one
//! abandoned dialog mark a busy clone stuck, and one interrupted tool call mark it hung.
//!
//! Most sessions are decided from files, with no model involved:
//!
//! | condition | verdict |
//! |---|---|
//! | container down | `Offline`, for the whole clone |
//! | no live agent session | `Stuck`, for the whole clone: nothing is running to wake anything |
//! | this session says `waiting` | `Stuck`, a dialog is up and needs a person |
//! | this session says `idle` | `Stuck`, it is sitting at its prompt |
//! | anything else | `Ask` |
//!
//! The `idle` shortcut is safe because Claude Code publishes `shell`, not `idle`, the moment
//! a background task, dialog, or queued request exists. So an `idle` session has nothing that
//! could wake it, and that is the commonest state in a fleet: free and certain.
//!
//! Every fold over the hook log is scoped to an owner, which is `(session, agent)`, and one
//! rule retires all of them: a turn boundary on a session (`Stop`, `UserPromptSubmit`,
//! `SessionEnd`) ends that session's main-agent evidence, and `SubagentStop` or `StopFailure`
//! ends that subagent's. Evidence that outlives its owner is the failure this module has hit
//! three separate times, most recently a tool call the operator interrupted that went on
//! reading as in flight for three hours.
//!
//! The model gets the remaining case, and it is a narrow question: of the work still
//! outstanding, will any of it finish without the operator doing something? That is mostly a
//! property of a command rather than a reading of intent. `cargo build` returns.
//! `npm run dev` does not. `cat /tmp/ci.fifo` returns only if whatever feeds the pipe is a
//! machine and not the person you would be notifying.
//!
//! GPT answers it, on an imported Codex account's ChatGPT plan. There is no key to set and no
//! provider to pick: the server already holds that account's token to run the clones.
//!
//! [`Verdict::Working`] is never produced here — only [`apply_verdict`] can, from the
//! model's answer. That is what makes "no Codex account imported" behave correctly with no
//! branch for it: with nothing to answer [`Verdict::Ask`], no clone is ever reported working.
//!
//! Nothing here is read back between ticks except the answer cache. Every verdict is recomputed
//! from the session registry, the hook log, and file mtimes, so there is no history to reconcile
//! and nothing to race. [`crate::stucklog`] writes each decision down as it is made, which no
//! tick ever reads: that file exists to tell an operator afterwards what this module believed at
//! the time, which is the one thing recomputation can never recover.
//!
//! Measured against a replay oracle over two runs on a live 32-clone fleet (2955 scorable
//! samples, 25 real stalls): 96.7% accurate against token idle's 90.9%, 44 missed stuck
//! clones against 250, and a median 31s to flag a stall against 331s. That was the earlier
//! design, which pooled a clone's sessions into one view and asked one question about the
//! pool, and those figures were taken against clone-level truth.
//!
//! Deciding per session was then measured against pooling on 2303 samples rebuilt from those
//! same two runs, with truth taken per session and a clone's truth the OR of its sessions'.
//! Both arms ran `gpt-5.6-luna` at medium effort, each with its own prompt:
//!
//! | | accuracy | missed a stuck clone | false alarm |
//! |---|---|---|---|
//! | pooled | 97.8% | 12 | 38 |
//! | per session | 98.2% | 14 | 27 |
//!
//! They disagree on 15 samples and the per-session answer is right on 12 (sign test p=0.035).
//! It costs nothing: the same number of calls, over 467 distinct questions against 517,
//! because a view carrying no session id is shared by every session in the same state.
//!
//! What that corpus cannot measure is the retirement rule below. It holds no interrupted tool
//! call at all, so the fold change is worth exactly zero points there. It was found on a live
//! clone instead, where one such call had been reading as an `ssh` hung for 3.4 hours.

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

/// The Responses endpoint a ChatGPT plan reaches GPT through: the same one the Codex CLI
/// calls, with the same OAuth access token the server already holds for the clones.
const CODEX_RESPONSES: &str = "https://chatgpt.com/backend-api/codex/responses";
/// Reasoning effort for the GPT path. Measured at 3.5s per call on `gpt-5.6-luna`, well
/// inside [`ASK_TIMEOUT`].
const CODEX_EFFORT: &str = "medium";

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

/// One session's verdict, for everything that can be settled without asking anyone.
///
/// A session, never a clone. What the OTHER sessions in the same clone are doing is not an
/// input here and must never become one: an operator who leaves one session at a dialog and
/// works in another has a working clone.
pub fn session_state(session: &Session) -> Verdict {
    match session.status.as_deref() {
        // A dialog is up, so this session needs a person however busy the rest of the clone is.
        Some("waiting") => Verdict::Stuck,
        // At its prompt with nothing pending. See the module docs for why `idle` is certain.
        Some("idle") => Verdict::Stuck,
        // `busy`, `shell`, and the statusless entrypoints all need the evidence read.
        _ => Verdict::Ask,
    }
}

/// Whether a clone needs the model at all, from its sessions' own verdicts.
///
/// This combines verdicts, never evidence. `Ask` as soon as one session needs asking, because
/// that one session could still be working. `Stuck` only when every live session is stuck.
pub fn clone_state(sessions: &[Session], container_up: bool) -> Verdict {
    if !container_up {
        return Verdict::Offline;
    }
    let mut live = sessions.iter().filter(|s| s.alive).peekable();
    if live.peek().is_none() {
        return Verdict::Stuck;
    }
    match live.any(|s| session_state(s) == Verdict::Ask) {
        true => Verdict::Ask,
        false => Verdict::Stuck,
    }
}

/// Why a file-decided session reads as stuck. An operator asking "why is this clone grey"
/// deserves an answer better than silence, and a dialog nobody noticed is the most actionable.
fn session_why(session: &Session) -> String {
    match session.status.as_deref() {
        Some("waiting") => format!(
            "waiting on {}",
            session.waiting_for.clone().unwrap_or_else(|| "unknown".into())
        ),
        _ => "idle at its prompt".to_string(),
    }
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

/// The owner a Cursor event gets when it arrives carrying no conversation id.
///
/// Cursor does that, and often. Measured on a live clone: 488 of 3804 events came through with
/// `session_id: ""`, every one of them a tool call (156 reads, 38 greps, 38 writes, 12 shells),
/// and every event in the ten minutes an operator watched it work was one of them. Nothing in
/// this module could see any of it, because every fold below keys on the owner, so the clone
/// read idle through the whole run and flipped to working only for the two and a half minutes
/// between the one prompt Cursor did name and its stop.
///
/// Naming them here rather than at each fold is what keeps that a one-line problem: downstream
/// they are an ordinary session with an unusual id.
const CURSOR_UNOWNED: &str = "cursor-unattributed";

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
            // Cursor sends the field empty rather than omitting it, so both shapes land here.
            // Named at the door, before any fold has a chance to skip it. See [`CURSOR_UNOWNED`].
            if e.session_id.as_deref().unwrap_or_default().is_empty() {
                e.session_id = Some(CURSOR_UNOWNED.to_string());
            }
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
///
/// The work Cursor does not name gets one session of its own, [`CURSOR_UNOWNED`], and that one
/// is bounded by time rather than by a `Stop`, because nothing ever sends a `Stop` for a
/// conversation that was never named. It exists only while its events keep arriving, so it
/// cannot read busy forever the way an unbounded one would. `MOVING_WINDOW_S` is the bound and
/// it is comfortable: across 3.3 hours of one clone's unowned stream, 2 of 487 gaps between
/// consecutive events exceeded it, and both were real pauses between stretches of work.
pub fn read_cursor_sessions(root: &Path, events: &[HookEvent], now: f64) -> Vec<Session> {
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
    // Fresh enough to still be work. Same `stops` rule as a named conversation, so a `Stop`
    // that ever does arrive unowned ends this one too.
    let unowned = unowned_last_seen(events, started)
        .filter(|at| now - at <= MOVING_WINDOW_S)
        .map(|_| CURSOR_UNOWNED);

    order
        .into_iter()
        .filter(|sid| !ended.contains(sid))
        .chain(unowned)
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

/// The clone's clock for this pass: this server's, raised to the newest hook stamp.
///
/// Stamped AFTER the log is read, never before. A hook that fires during the read would
/// otherwise carry a timestamp later than `now`, and the tool-call age computed from the pair
/// came out negative. The model noticed before I did, calling a `running_for_seconds` of -1
/// "invalid" and reading the clone as hung because of it.
fn clone_now(events: &[HookEvent]) -> f64 {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0.0, |d| d.as_secs_f64());
    events.iter().map(|e| e.ts).fold(now, f64::max)
}

/// When the Cursor work nobody named was last seen, counting only this Cursor process's own.
pub fn unowned_last_seen(events: &[HookEvent], started: f64) -> Option<f64> {
    events
        .iter()
        .filter(|e| e.ts >= started && e.session_id.as_deref() == Some(CURSOR_UNOWNED))
        .map(|e| e.ts)
        .fold(None, |best: Option<f64>, ts| Some(best.map_or(ts, |b| b.max(ts))))
}

/// `PreToolUse` minus everything that ends it: the tool call each owner is sitting inside.
///
/// This is the whole signal on the statusless entrypoints, which publish no `status` at all.
/// It is also what keeps a busy session from reading as finished: without it the judge sees no
/// in-flight work and no background task and concludes "idle at its prompt". Measured on the
/// 323 pilot views that carry one: 8 false alarms with this evidence, 41 without.
///
/// A matching `PostToolUse` is not the only thing that ends a call, and believing it was cost
/// three hours of one clone reading as hung. **Interrupting a tool call fires no
/// `PostToolUse`**: the operator presses escape, types something else, and the call is over
/// with nothing to say so. Only the turn boundary that follows says it, which is why the
/// retirement rule is stated in terms of owners rather than pairs.
pub fn in_flight_tools(events: &[HookEvent]) -> Vec<&HookEvent> {
    let mut live: Vec<&HookEvent> = Vec::new();
    for e in events {
        match e.hook_event_name.as_str() {
            "PreToolUse" if e.tool_use_id.is_some() => live.push(e),
            "PostToolUse" | "PostToolUseFailure" => {
                if let Some(tid) = e.tool_use_id.as_deref() {
                    live.retain(|x| x.tool_use_id.as_deref() != Some(tid));
                }
            }
            // A turn boundary ends that session's MAIN-AGENT calls: the agent cannot be
            // inside a tool call and have finished its turn. Subagent calls are left alone
            // here, because a background agent outlives the turn that launched it.
            "Stop" | "UserPromptSubmit" | "SessionEnd" if e.agent_id.is_none() => {
                live.retain(|x| !(x.agent_id.is_none() && x.session_id == e.session_id));
            }
            // A subagent's own close ends whatever it was sitting inside.
            "SubagentStop" | "StopFailure" => {
                if let Some(aid) = e.agent_id.as_deref() {
                    live.retain(|x| x.agent_id.as_deref() != Some(aid));
                }
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

/// Seconds since each session's newest `UserPromptSubmit`, on the clone's own clock.
///
/// Nothing in the verdict uses this. It exists for [`crate::stucklog`], and it is the one number
/// that separates "the agent carried on by itself" from "somebody typed something": a session
/// that comes back to life carrying a prompt age older than the stretch it spent reported idle
/// was never prompted, so whatever called it idle was wrong. Reconstructing that from the
/// transcript alone is not possible after the clone is gone, and the hook log dies with it.
///
/// Both stamps are the clone's, so the difference is sound. Clamped at zero for the same reason
/// [`build_session_view`] clamps its tool ages: a hook that fires during the read lands ahead of
/// `now`, and a negative age is not a fact about anything.
fn prompt_ages(events: &[HookEvent], now: f64) -> HashMap<&str, f64> {
    let mut out = HashMap::new();
    for e in events.iter().filter(|e| e.hook_event_name == "UserPromptSubmit") {
        if let Some(sid) = e.session_id.as_deref() {
            out.insert(sid, (now - e.ts).max(0.0));
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

/// Everything read or folded once per clone, whatever number of sessions then read from it.
///
/// The folds are clone-wide because the event log is: one file holds every session's stream.
/// Scoping happens where the view is built, by owner, never here.
pub struct CloneFacts<'a> {
    /// How long each transcript has been untouched, keyed by session or conversation id.
    silence: HashMap<String, f64>,
    /// Size and staleness of each background task's output file, keyed by task id.
    outputs: HashMap<String, (u64, f64)>,
    /// Every tool call still open, each carrying the owner that will close it.
    tools: Vec<&'a HookEvent>,
    /// The newest live `Stop` per session.
    stops: HashMap<&'a str, &'a HookEvent>,
    /// The `StopFailure` each session is still sitting on, if any.
    errors: HashMap<&'a str, &'a HookEvent>,
}

impl<'a> CloneFacts<'a> {
    pub fn read(root: &Path, events: &'a [HookEvent], now: f64) -> Self {
        let mut silence = transcript_silence(root, now);
        // The work Cursor never named has no transcript to be silent for, and no entry here
        // reads as `quiet_for_seconds: 1000000000`, which the judge is right to call a dead
        // agent. How long since it last fired is the same quantity, so that is what it gets.
        if let Some(at) = unowned_last_seen(events, f64::NEG_INFINITY) {
            silence.insert(CURSOR_UNOWNED.to_string(), (now - at).max(0.0));
        }
        Self {
            silence,
            outputs: background_outputs(root, now),
            tools: in_flight_tools(events),
            stops: latest_live_stop(events),
            errors: current_api_errors(events),
        }
    }
}

/// Everything the model is told about ONE session.
///
/// Scoping is the whole design. A clone keeps every transcript it has ever written and every
/// event it has ever fired, and it runs several independent agents at once, so a view that
/// pools them hands the judge evidence belonging to nobody. Asked one question over that pool,
/// the model has no way to say "this session is working and that one is wedged", and any one
/// stale item vetoes every live one. Only this session's own evidence gets in.
///
/// A subagent's work counts as its parent's, because the parent is what it reports back to.
pub fn build_session_view(session: &Session, facts: &CloneFacts, now: f64) -> Value {
    let sid = session.session_id.as_str();
    let tools: Vec<&&HookEvent> =
        facts.tools.iter().filter(|t| t.session_id.as_deref() == Some(sid)).collect();
    // Sitting inside a tool call is blocked in it, not generating. Both states publish `busy`,
    // so the registry cannot tell them apart and this is what does. A subagent's call does not
    // block its parent: the parent is inside its own `Task` call, which is counted here.
    let blocked = tools.iter().any(|t| t.agent_id.is_none());

    let quiet = facts.silence.get(sid).copied().unwrap_or(f64::MAX);
    let stop = facts.stops.get(sid).copied();

    // A cli session publishes `status`, and `busy` minus "sitting in a tool call" is
    // generation. A statusless entrypoint falls back to transcript mtime. Reading that
    // fallback from the hook fold instead looks more principled and measured worse: over
    // a Cursor-heavy run it changed four verdicts and was wrong on all four.
    let generating = match session.status.as_deref() {
        Some(status) => status == "busy" && !blocked,
        None => quiet <= MOVING_WINDOW_S,
    };

    let out_tasks: Vec<Value> = stop
        .iter()
        .flat_map(|e| e.background_tasks.iter())
        .map(|task| {
            let got = facts.outputs.get(&task.id);
            json!({
                "kind": task.kind,
                "what": task.command.clone().or_else(|| task.agent_type.clone()),
                "description": task.description,
                "output_bytes": got.map(|(b, _)| *b),
                "producing_output": got.is_some_and(|(_, age)| *age <= MOVING_WINDOW_S),
                // Null, never zero, when there is no output file to age. Zero reads as
                // "it wrote something a moment ago", which is the opposite of what an
                // absent file means, and it reads that way in the direction of `working`.
                "output_still_for_seconds": got.map(|(_, age)| age.round()),
            })
        })
        .collect();

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

    let out_errors: Vec<Value> = facts
        .errors
        .get(sid)
        .map(|err| json!({"detail": err.reason}))
        .into_iter()
        .collect();

    // No session id anywhere in here. It identifies nothing the judge can use, and it made
    // every view unique, so two sessions in the same state never shared a cached answer.
    json!({
        "session": {
            "status": session.status,
            "generating": generating,
            "quiet_for_seconds": quiet.min(1.0e9).round(),
            "agent_last_said": stop.and_then(|e| e.last_assistant_message.clone()),
            // Cursor reports how the turn ended on the stop itself. `aborted` means a person
            // pressed stop and `error` means the turn died, and both are worth the model
            // seeing: neither resumes on its own.
            "turn_ended_with": stop.and_then(|e| e.status.clone()),
            // How long the agent has been parked on whatever the turn left outstanding, from
            // the `Stop` itself rather than from a file. `quiet_for_seconds` is transcript
            // mtime and answers a different question badly here: a clone parked on three dead
            // wait loops for 2h23m reported 303 seconds of quiet, because something else in
            // the tree was still being touched. Named to end in `_seconds` so [`cache_key`]
            // buckets it; an unbucketed duration would miss the cache on every four-second
            // tick and ask the model ~900 times an hour per session.
            "turn_over_for_seconds": stop.map(|e| (now - e.ts).max(0.0).round()),
        },
        "background_tasks": out_tasks,
        "in_flight_tool_calls": out_tools,
        "api_errors": out_errors,
    })
}

// ---------------------------------------------------------------------------------------
// Asking the model
// ---------------------------------------------------------------------------------------

/// The question, arrived at over two fleet pilots. Six passages in here are load-bearing and
/// were each added to fix a measured failure, so read the git history before trimming:
///
/// - "read the WHOLE command" fixes calling `tail -c 120 file` non-terminating because the
///   examples mentioned `tail -f`.
/// - "take it at its word about the writer" is what separates a pipe a nightly job feeds
///   from one the operator must write by hand.
/// - "a background task that finishes wakes the agent" stops a clone with a live task from
///   reading as finished-and-idle.
/// - "not inside its first minute" stops GPT declaring a 3-second `ssh` and a 7-second
///   `grep` hung. It was doing that on 24 of its 43 false alarms.
/// - "that writer has to show up in the rest of the view" stops the opposite failure, where
///   an agent says it dispatched a worker, nothing in the view corresponds to it, and the
///   clone sits there. One such stall accounted for 33 missed views.
/// - "a polling wait finishes only if the thing it waits for actually happens" is the newest,
///   and unlike the five above it comes from one logged failure rather than a corpus. See
///   below.
///
/// The fourth and fifth are what took `gpt-5.6-luna` at medium effort from 89.6% to 94.5% over
/// the 1167 recorded pooled views (misses 78 to 43, false alarms 43 to 21). Measured, not
/// guessed: a dev/holdout split by situation, then confirmed on the whole set. Higher reasoning
/// effort was tried instead and bought 0.4 points, so the wording is what matters here, not
/// thinking time.
///
/// The polling passage has no such backing, because the corpus predates it and holds no case of
/// the kind. What it has instead is one fully recorded failure, from [`crate::stucklog`] on
/// CT 105 on 2026-08-05. Clone `pega-rmng-development-3` sat on three shells of the form
/// `until ! pgrep -f "publish-server.sh"; do sleep 20; done`, each of which matched its own
/// command line through `pgrep -f` and so was waiting for itself to exit. All three had
/// produced zero bytes, for between 3.9 and 5.1 hours. The model did not miss the evidence: on
/// the same view minutes apart it answered "only self-matching pgrep wait loops remain, so they
/// will not finish" and then "background watcher tasks are still running and will wake the
/// agent". It was a coin flip, and the answer cache pinned whichever came up. The passage
/// exists to settle that flip, and `turn_over_for_seconds` was added to the view with it,
/// because nothing in the view had said how long the agent had actually been parked.
///
/// Both arms were then counted on their own decision logs, over every decision taken while a
/// dead loop was outstanding:
///
/// | | decisions | said working |
/// |---|---|---|
/// | before, CT 105, one session over 2h23m | 21 | 3 |
/// | after, CT 120, one session over 10m | 7 | 0 |
///
/// The second arm is a deliberate reproduction rather than a found case: a real interactive
/// `claude` in a tmux pane, told to leave `until ! pgrep -f rmng-judge-probe; do sleep 20; done`
/// running and then stop. Driving it through the agent-wrapper instead would have proved
/// nothing, because that entrypoint publishes no `status` and never reaches the `shell` state
/// this failure lives in. Seven samples is small and says so.
///
/// One thing deliberately absent: any elapsed-time rule for background tasks. It was tested
/// against all 1167 recorded views and rejected. Misses stayed at exactly 44 while false
/// alarms rose from 35 to 47.
const SYSTEM: &str = r#"You watch coding agents running unattended in containers. Answer one question
about one agent session. Other sessions may be running beside it in the same container.
Everything below belongs to this one and is all of what it has.

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
  - the only thing still running polls for a condition that is not coming. See below.
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

Do not call anything hung inside its first minute. Under a minute you cannot tell a hung
command from a slow one, and the cost of guessing wrong is a person being told to look at
a clone that was fine. Work that leaves the machine is slower still: ssh, git fetch,
docker, a remote exec, or anything pulling over the network routinely takes tens of
seconds, and several of them in one command line add up.

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

A polling wait finishes only if the thing it waits for actually happens. `until <test>; do
sleep N; done`, `while ! <test>; do sleep N; done`, and any loop around a pgrep, a lock
file, a pid, or an HTTP probe all have this shape, and every one of them prints nothing
until it exits. So output_bytes of 0 is what a healthy one looks like too, and that number
settles nothing on its own. turn_over_for_seconds is what settles it: that is how long the
agent has been parked waiting for exactly these. A polling wait still outstanding hours
after the turn ended is waiting on something that already happened or that never will, and
nothing is coming: false.

One trap in particular, because it has cost hours of a clone reading as working. `pgrep -f`
matches the full command line of every process, the shell running the loop included, so
`until ! pgrep -f "deploy.sh"; do sleep 20; done` matches itself and waits forever. If a
pgrep pattern appears in the loop's own command, that loop can never end: false.

producing_output tells you whether a task is emitting right now. generating tells you
whether the agent itself is writing tokens, which is false while it waits inside a tool
call. Either one being true means true.

A background task that finishes wakes the agent by itself and it takes its next turn. So
a background task that is going to finish is true, even though the agent is doing nothing
while it waits. The agent being at its prompt with a task still running is not the same
as the agent being done.

One check on the writer it names. That writer has to show up in the rest of the view, as a
background task, an in-flight tool call, or a subagent. If the agent says it dispatched
something and nothing here corresponds to it, then whatever it started is already over and
it was not woken: false.

Reply with only a JSON object:
{"will_progress": true, "reason": "one short sentence"}"#;

/// Edges of the elapsed-time buckets the cache key rounds to, in seconds.
const BUCKETS: [f64; 9] = [30.0, 60.0, 120.0, 300.0, 600.0, 1200.0, 2400.0, 4800.0, 9600.0];

/// Remembers what the model said, so a clone sitting in one state is asked once rather than
/// once per tick.
///
/// The monitor ticks every 4s and a view carries elapsed seconds, so a naive cache would
/// miss on every tick and ask ~900 times an hour per session. Rounding every `*_seconds`
/// field to a bucket means a session is re-asked only when it crosses one or when something
/// real changes. On the pilot fleet this held 32 clones to 381 calls over six hours.
///
/// Asking per session shares better rather than worse, because a view identifies no session:
/// over the rebuilt corpus the same samples came to 467 distinct questions where the pooled
/// design needed 517.
///
/// Keyed on the view alone, never on who answered. Changing provider therefore keeps the
/// answers already given until each clone's view moves, which is right: the question is about
/// the clone, and the two providers are answering the same one. A failed call is not cached
/// at all, so a provider that comes back is asked again on the next tick.
#[derive(Default)]
pub struct Judge {
    answers: StdRwLock<HashMap<String, (bool, String)>>,
    /// The last "nothing can answer" line logged. A judge nobody configured is a standing
    /// condition, and the fleet pass runs every few seconds, so it is said once and again
    /// only when it changes.
    last_gap: StdRwLock<String>,
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

#[derive(Debug, Deserialize)]
struct Answer {
    #[serde(default)]
    will_progress: Option<bool>,
    #[serde(default)]
    reason: Option<String>,
}

/// What answers [`Verdict::Ask`]: one imported Codex account's credentials, resolved once per
/// fleet pass because the access token in it has to be a fresh one.
///
/// No `Debug`: it holds a live access token, and a derived one would print it the first time
/// anything logged a backend.
#[derive(Clone)]
pub struct Backend {
    token: String,
    account_id: String,
    model: String,
}

/// Which imported Codex account pays, or the line that says why none.
///
/// Both the fleet pass and the settings Test button ask this, and they have to give the same
/// answer: "your account is gone" and "you have no accounts" are different problems.
fn judge_account(app: &crate::app::App, want: &str) -> Result<String, String> {
    crate::codex::judge_email(app, want).ok_or_else(|| match want.is_empty() {
        true => "no Codex account is imported".to_string(),
        false => format!("no imported Codex account for '{want}'"),
    })
}

/// The account the judge will bill, with its token refreshed if it was due.
///
/// Called at most once per fleet pass, and only when some clone actually needs asking. A rig
/// with no Codex account gets `None` plus a line saying so, which the caller logs once rather
/// than once per clone per tick, and every undecided clone reads idle.
pub async fn backend(app: &crate::app::App) -> (Option<Backend>, String) {
    let cfg = app.config();
    let email = match judge_account(app, &cfg.judge.codex_email.unwrap_or_default()) {
        Ok(email) => email,
        Err(why) => return (None, why),
    };
    match crate::codex::fresh_access_token(app, &email).await {
        Ok((acct, _)) => (
            Some(Backend {
                token: acct.access_token,
                account_id: acct.account_id,
                model: cfg.judge.codex_model,
            }),
            String::new(),
        ),
        Err(e) => (None, format!("{email}: {e:#}")),
    }
}

impl Judge {
    pub fn new() -> Self {
        Self::default()
    }

    /// `(state, reason, asked)`. `asked` is false when the cache answered.
    pub async fn resolve(
        &self,
        http: &reqwest::Client,
        backend: &Backend,
        view: &Value,
    ) -> Result<(wire::MonitorState, String, bool)> {
        let key = cache_key(view);
        if let Some((progress, reason)) = self.answers.read().unwrap().get(&key).cloned() {
            return Ok((apply_verdict(Some(progress)), reason, false));
        }
        let answer = ask(http, backend, view).await?;
        let progress = answer.will_progress == Some(true);
        let reason = answer.reason.unwrap_or_default();
        self.answers.write().unwrap().insert(key, (progress, reason.clone()));
        Ok((apply_verdict(Some(progress)), reason, true))
    }

    /// Whether `why` is worth logging: true the first time it is seen and again whenever it
    /// changes. Pass an empty string once the judge answers again, so a fault that comes back
    /// is reported again.
    pub fn note_gap(&self, why: &str) -> bool {
        let mut last = self.last_gap.write().unwrap();
        if *last == why {
            return false;
        }
        *last = why.to_string();
        !why.is_empty()
    }

    /// Drop cached answers once the fleet has moved on, mirroring `ActivityBus::retain`.
    pub fn prune(&self, max_entries: usize) {
        let mut answers = self.answers.write().unwrap();
        if answers.len() > max_entries {
            answers.clear();
        }
    }
}

async fn ask(http: &reqwest::Client, backend: &Backend, view: &Value) -> Result<Answer> {
    ask_codex(http, &backend.token, &backend.account_id, &backend.model, view).await
}

/// Ask GPT over the Codex CLI's own endpoint, on an imported account's ChatGPT plan.
///
/// Three things about this endpoint, all measured against it rather than assumed:
/// `stream: false` is refused with `400 Stream must be set to true`, `store` must be false,
/// and the `response.completed` event carries an EMPTY `output` array, so the answer has to
/// be picked out of the item events instead ([`codex_answer_text`]). The whole stream is
/// ~14KB, so it is read as one body rather than consumed incrementally.
async fn ask_codex(
    http: &reqwest::Client,
    token: &str,
    account_id: &str,
    model: &str,
    view: &Value,
) -> Result<Answer> {
    let body = json!({
        "model": model,
        "instructions": SYSTEM,
        "input": [{
            "type": "message",
            "role": "user",
            "content": [{"type": "input_text", "text": serde_json::to_string_pretty(view)?}],
        }],
        "reasoning": {"effort": CODEX_EFFORT},
        "store": false,
        "stream": true,
    });
    let resp = http
        .post(CODEX_RESPONSES)
        .timeout(ASK_TIMEOUT)
        .header("Authorization", format!("Bearer {token}"))
        .header("chatgpt-account-id", account_id)
        .header("OpenAI-Beta", "responses=experimental")
        .header("originator", "codex_cli_rs")
        .header("Accept", "text/event-stream")
        .json(&body)
        .send()
        .await?;
    let status = resp.status();
    let text = resp.text().await.unwrap_or_default();
    if !status.is_success() {
        bail!("codex {}: {}", status.as_u16(), snippet(&text));
    }
    Ok(parse_answer(&codex_answer_text(&text)?))
}

/// The assistant's text, out of a Responses SSE body.
///
/// Read in the order the events are trustworthy: the finished message item carries the whole
/// text, `response.output_text.done` repeats it, and the deltas are the last resort if the
/// backend ever stops sending either. A stream that failed part way names its error rather
/// than reading as an empty answer, which would be indistinguishable from "no".
fn codex_answer_text(body: &str) -> Result<String> {
    let mut message = String::new();
    let mut done = String::new();
    let mut delta = String::new();
    for line in body.lines() {
        let Some(data) = line.strip_prefix("data: ") else {
            continue;
        };
        let Ok(event) = serde_json::from_str::<Value>(data) else {
            continue;
        };
        match event["type"].as_str().unwrap_or_default() {
            "response.output_item.done" if event["item"]["type"] == "message" => {
                message = event["item"]["content"]
                    .as_array()
                    .map(|parts| {
                        parts
                            .iter()
                            .filter_map(|p| p["text"].as_str())
                            .collect::<String>()
                    })
                    .unwrap_or_default();
            }
            "response.output_text.done" => done = event["text"].as_str().unwrap_or_default().into(),
            "response.output_text.delta" => {
                delta.push_str(event["delta"].as_str().unwrap_or_default())
            }
            "response.failed" | "error" => {
                let why = event["response"]["error"]["message"]
                    .as_str()
                    .or_else(|| event["error"]["message"].as_str())
                    .or_else(|| event["message"].as_str())
                    .unwrap_or("no message");
                bail!("codex stream failed: {}", snippet(why));
            }
            _ => {}
        }
    }
    Ok([message, done, delta].into_iter().find(|t| !t.is_empty()).unwrap_or_default())
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

/// Does the GPT path work end to end? Used by `POST /api/config/test`.
///
/// Asks the real question against a fixture view rather than pinging something cheaper, which
/// is the point: one call exercises the account lookup, the token refresh, the endpoint and
/// the model name together, and those are the four things that break.
pub async fn probe_codex(app: &crate::app::App, email: &str, model: &str) -> (bool, String) {
    let email = match judge_account(app, email) {
        Ok(email) => email,
        Err(why) => return (false, why),
    };
    let acct = match crate::codex::fresh_access_token(app, &email).await {
        Ok((acct, _)) => acct,
        Err(e) => return (false, format!("{email}: {e:#}")),
    };
    let view = json!({
        "sessions": [{"status": "shell", "quiet_for_seconds": 40}],
        "background_tasks": [{"command": "cargo build --release", "running_for_seconds": 90}],
    });
    match ask_codex(&app.http, &acct.access_token, &acct.account_id, model, &view).await {
        Err(e) => (false, format!("{model} on {email}: {e:#}")),
        Ok(a) => match a.will_progress {
            // The fixture describes a release build still running, so a judge that is working
            // says true. Anything else means the model answered but not the question.
            Some(true) => (true, format!("{model} answers on {email}")),
            _ => (
                false,
                format!("{model} on {email} gave an unusable answer: {}", a.reason.unwrap_or_default()),
            ),
        },
    }
}

// ---------------------------------------------------------------------------------------
// The fleet pass
// ---------------------------------------------------------------------------------------

/// Decide working-vs-stuck for every running clone, in one concurrent pass.
///
/// Each clone is the OR of its live sessions: working when any one of them is working, idle
/// only when every one of them is. Sessions inside a clone are walked in order and the walk
/// stops at the first working answer, so the common one-busy-session clone still costs one
/// call. Clones run concurrently.
///
/// Reading a clone is blocking file IO (a directory of session records, the hook log, and two
/// shallow walks), so each clone's read happens on the blocking pool. The model calls that
/// follow are ordinary futures.
///
/// With no judge configured this still runs, and every session that the files cannot settle
/// comes back idle. That is the whole unconfigured behaviour: no guess, no model, and no
/// clone ever reported as working. Per-clone token accounting is elsewhere and unaffected.
pub async fn resolve_fleet(
    app: &crate::app::App,
    ids: Vec<String>,
) -> HashMap<String, wire::MonitorState> {
    let data_dir = app.config().data_dir.clone();

    let reads = ids.into_iter().map(|id| {
        let data_dir = data_dir.clone();
        async move {
            let id2 = id.clone();
            let read = tokio::task::spawn_blocking(move || read_clone(&data_dir, &id2)).await;
            (id, read.ok().flatten())
        }
    });
    let read = futures::future::join_all(reads).await;

    // Only now, and only if some clone actually needs asking: the Codex path refreshes an
    // access token to build this, which a fleet that settled from files alone should not pay
    // for every four seconds.
    let asking = read
        .iter()
        .flat_map(|(_, cases)| cases.iter().flatten())
        .any(|c| c.verdict == Verdict::Ask);
    let backend = match asking {
        false => None,
        true => {
            let (backend, why) = backend(app).await;
            if app.stuck.note_gap(&why) {
                tracing::warn!(target: "stuck", "no clone can read as working: {why}");
            }
            backend
        }
    };

    let decided = read.into_iter().map(|(id, snapshot)| {
        let backend = backend.clone();
        let log = app.stucklog.clone();
        async move {
            // A clone whose home is not readable right now (restarting, or `homes` has not
            // repointed its symlink yet) tells us nothing. Leaving it Idle is the honest
            // reading: we cannot see anything that would wake it.
            let Some(cases) = snapshot else {
                return (id, wire::MonitorState::Idle);
            };
            // Only for a clone that was actually read: an unreachable home has no live set, and
            // treating that as an empty one would close out every session the clone still has.
            let live: Vec<String> = cases.iter().map(|c| c.session.clone()).collect();
            log.retire(&id, &live);
            if cases.is_empty() {
                tracing::debug!(target: "stuck", "clone {id}: idle — no live agent session");
                return (id, wire::MonitorState::Idle);
            }
            // The clone is working when ANY of its sessions is. Walked in order and stopped at
            // the first one that answers working, because no later answer could change the
            // clone's, and each one costs a model call. Sessions after that one go unevaluated,
            // so they get no decision line either, and their last one stands.
            for case in &cases {
                let sid = short(&case.session);
                match case.verdict {
                    Verdict::Offline | Verdict::Stuck => {
                        tracing::debug!(target: "stuck", "clone {id} {sid}: idle — {}", case.why);
                        let why = case.why.clone();
                        log.record(
                            case.decision(&id, wire::MonitorState::Idle, "files", why),
                            false,
                        );
                    }
                    Verdict::Ask => {
                        let Some(backend) = backend.as_ref() else {
                            log.record(
                                case.decision(
                                    &id,
                                    wire::MonitorState::Idle,
                                    "no-judge",
                                    "nothing is configured to answer, so no clone reads as \
                                     working"
                                        .to_string(),
                                ),
                                false,
                            );
                            continue;
                        };
                        match app.stuck.resolve(&app.http, backend, &case.view).await {
                            Ok((state, reason, asked)) => {
                                if asked {
                                    tracing::debug!(
                                        target: "stuck",
                                        "clone {id} {sid}: {state:?} — {reason}"
                                    );
                                }
                                // Forced on a live call even when the state held: that line is
                                // the only record of a question the cache had not answered
                                // already, and the prompt is tuned against exactly those.
                                let by = if asked { "model" } else { "cache" };
                                log.record(case.decision(&id, state, by, reason), asked);
                                if state == wire::MonitorState::Working {
                                    return (id, wire::MonitorState::Working);
                                }
                            }
                            Err(e) => {
                                // An outage at the provider must not flip the fleet to
                                // working. Idle is the same answer an unconfigured judge
                                // gives, and it is the safe one.
                                tracing::warn!(
                                    target: "stuck",
                                    "clone {id} {sid}: asking failed: {e:#}"
                                );
                                let why = format!("asking failed: {e:#}");
                                log.record(
                                    case.decision(
                                        &id,
                                        wire::MonitorState::Idle,
                                        "ask-failed",
                                        why,
                                    ),
                                    false,
                                );
                            }
                        }
                    }
                }
            }
            (id, wire::MonitorState::Idle)
        }
    });
    let out: HashMap<String, wire::MonitorState> =
        futures::future::join_all(decided).await.into_iter().collect();

    // The cache is keyed by view content, not by clone, so it cannot be pruned per clone.
    // Clearing it wholesale once it outgrows the fleet costs one round of re-asking.
    app.stuck.prune(out.len().saturating_mul(16).max(256));

    // One append for the whole pass, awaited rather than detached so two passes cannot be
    // writing the same file at once. A pass that decided nothing new writes nothing at all.
    let log = app.stucklog.clone();
    log.retain(&out.keys().cloned().collect());
    let _ = tokio::task::spawn_blocking(move || log.flush(&data_dir)).await;
    out
}

/// One live session, decided as far as its files can decide it.
struct SessionCase {
    /// The session's own id, in full. Abbreviated only where it is logged.
    session: String,
    verdict: Verdict,
    /// The judge's question, built only for [`Verdict::Ask`].
    view: Value,
    /// Why a file-decided session reads as stuck.
    why: String,
    /// The registry status this was read from, for the decision log. A file-settled session has
    /// no view, so this and `waiting_for` are the whole of what decided it.
    status: Option<String>,
    waiting_for: Option<String>,
    /// Seconds since this session's newest `UserPromptSubmit`, on the CLONE's clock. `None`
    /// whenever the hook log was not read, which is every session the registry settled.
    prompt_age: Option<f64>,
}

/// First eight characters, which is how a session appears in a `stuck` tracing line. The whole
/// id goes to the decision log, so the two can be matched up.
fn short(id: &str) -> String {
    id.chars().take(8).collect()
}

impl SessionCase {
    /// This case as a decision-log line. `ts` and `was` are filled in by the recorder.
    ///
    /// The view is attached only when there was one. A file-settled session gets `status` and
    /// `waiting_for` instead, and those really are everything [`session_state`] looked at.
    fn decision(
        &self,
        clone: &str,
        state: wire::MonitorState,
        decided_by: &str,
        why: String,
    ) -> crate::stucklog::Decision {
        crate::stucklog::Decision {
            clone: clone.to_string(),
            session: self.session.clone(),
            state: match state {
                wire::MonitorState::Working => "working",
                wire::MonitorState::Idle => "idle",
                wire::MonitorState::Offline => "offline",
            }
            .to_string(),
            decided_by: decided_by.to_string(),
            why,
            status: self.status.clone(),
            waiting_for: self.waiting_for.clone(),
            prompt_age_seconds: self.prompt_age.map(f64::round),
            view: (!self.view.is_null()).then(|| self.view.clone()),
            ..Default::default()
        }
    }
}

/// Every live session in one clone, read on the blocking pool.
///
/// `None` when the home is not reachable. An empty vec when nothing is running in there,
/// which is a decided clone rather than an unreadable one.
fn read_clone(data_dir: &str, id: &str) -> Option<Vec<SessionCase>> {
    let root = clone_root(data_dir, id)?;
    let mut sessions = read_sessions(&root);
    // Cursor's conversations come out of the hook log, so a clone running Cursor has to read
    // it before the verdict rather than after. Gated on Cursor actually running, which is two
    // small file reads, so a clone without it keeps the fast path below untouched.
    let mut events = Vec::new();
    if cursor_process(&root).is_some() {
        events = read_hook_events(&root);
        let now = clone_now(&events);
        sessions.extend(read_cursor_sessions(&root, &events, now));
    }
    let live: Vec<Session> = sessions.into_iter().filter(|s| s.alive).collect();

    let settled = |s: &Session| SessionCase {
        session: s.session_id.clone(),
        verdict: session_state(s),
        view: Value::Null,
        why: session_why(s),
        status: s.status.clone(),
        waiting_for: s.waiting_for.clone(),
        prompt_age: None,
    };
    // Nothing else is read when every session settles from its own status: three quarters of
    // a fleet stops here, and the cheapest request is the one not made. An empty clone lands
    // here too, as an empty vec.
    if clone_state(&live, true) != Verdict::Ask {
        return Some(live.iter().map(settled).collect());
    }

    if events.is_empty() {
        events = read_hook_events(&root);
    }
    let now = clone_now(&events);
    // Read and folded once for the whole clone, however many sessions read from it.
    let facts = CloneFacts::read(&root, &events, now);
    let ages = prompt_ages(&events, now);
    Some(
        live.iter()
            .map(|s| match session_state(s) {
                Verdict::Ask => SessionCase {
                    session: s.session_id.clone(),
                    verdict: Verdict::Ask,
                    view: build_session_view(s, &facts, now),
                    why: String::new(),
                    status: s.status.clone(),
                    waiting_for: s.waiting_for.clone(),
                    prompt_age: ages.get(s.session_id.as_str()).copied(),
                },
                _ => settled(s),
            })
            .collect(),
    )
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

    /// One session's view, with the clone-wide reads and folds done for it.
    fn view_of(root: &Path, s: &Session, events: &[HookEvent], now: f64) -> Value {
        build_session_view(s, &CloneFacts::read(root, events, now), now)
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
    fn a_dialog_stops_its_own_session_and_nobody_elses() {
        // The regression this pins: one abandoned dialog used to mark the whole clone stuck,
        // however busy the session next to it was. An operator working in one session with a
        // permission prompt left up in another has a working clone.
        let waiting = session(Some("waiting"), true);
        let busy = session(Some("busy"), true);
        assert_eq!(session_state(&waiting), Verdict::Stuck);
        assert_eq!(session_state(&busy), Verdict::Ask);
        assert_eq!(clone_state(&[busy, waiting], true), Verdict::Ask);
    }

    #[test]
    fn a_clone_is_stuck_only_when_every_session_is() {
        let idle = || session(Some("idle"), true);
        let waiting = || session(Some("waiting"), true);
        assert_eq!(clone_state(&[idle(), waiting()], true), Verdict::Stuck);
        assert_eq!(clone_state(&[idle(), waiting(), session(None, true)], true), Verdict::Ask);
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
        // A statusless session is a Cursor or wrapper session that may well be mid-turn. The
        // idle one beside it settles for free and says nothing about it.
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
    fn a_dialog_says_what_it_wants() {
        let mut s = session(Some("waiting"), true);
        s.waiting_for = Some("permission prompt".into());
        assert_eq!(session_why(&s), "waiting on permission prompt");
        s.waiting_for = None;
        assert_eq!(session_why(&s), "waiting on unknown");
        assert_eq!(session_why(&session(Some("idle"), true)), "idle at its prompt");
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

    fn prompt(sid: &str, ts: f64) -> HookEvent {
        HookEvent { ts, ..event("UserPromptSubmit", sid, None) }
    }

    #[test]
    fn the_prompt_age_is_the_newest_prompt_per_session() {
        // This is what a decision line carries so a later reader can tell an agent that carried
        // on by itself from one somebody typed at. An older prompt must never win.
        let events = [
            prompt("s1", 100.0),
            prompt("s2", 150.0),
            prompt("s1", 200.0),
            HookEvent { ts: 210.0, ..event("Stop", "s1", None) },
        ];
        let ages = prompt_ages(&events, 500.0);
        assert_eq!(ages.get("s1").copied(), Some(300.0));
        assert_eq!(ages.get("s2").copied(), Some(350.0));
        assert_eq!(ages.get("s3"), None, "a session nobody prompted has no age");
    }

    #[test]
    fn a_prompt_that_lands_during_the_read_ages_zero_rather_than_negative() {
        // Same clamp `build_session_view` applies to tool ages, for the same reason: a hook
        // fired while the log was being read sits ahead of `now`.
        assert_eq!(prompt_ages(&[prompt("s1", 501.0)], 500.0).get("s1").copied(), Some(0.0));
    }

    #[test]
    fn an_interrupted_tool_call_is_over_at_the_next_turn_boundary() {
        // Measured on a live clone: escape during a tool call fires no `PostToolUse`, and the
        // prompt the operator types next is the only thing that says the call ended. Waiting
        // for a matching post let one `ssh` age for 3.4 hours, and the judge read the session
        // as hung for every one of them.
        let mut pre = event("PreToolUse", "s", None);
        pre.tool_use_id = Some("t1".into());
        pre.ts = 100.0;
        assert_eq!(in_flight_tools(std::slice::from_ref(&pre)).len(), 1);
        for closer in ["UserPromptSubmit", "Stop", "SessionEnd"] {
            let mut end = event(closer, "s", None);
            end.ts = 114.0;
            assert!(
                in_flight_tools(&[pre.clone(), end]).is_empty(),
                "{closer} ends the call the session was sitting inside"
            );
        }
        // Another session's boundary says nothing about this call.
        let mut elsewhere = event("Stop", "other", None);
        elsewhere.ts = 114.0;
        assert_eq!(in_flight_tools(&[pre, elsewhere]).len(), 1);
    }

    #[test]
    fn a_subagents_call_outlives_the_parent_turn_and_ends_on_its_own_close() {
        // A parent that launches background agents fires `Stop` at once and leaves them
        // running. Sweeping them out with the parent would call a busy clone finished.
        let mut sub = event("PreToolUse", "s", Some("a1"));
        sub.tool_use_id = Some("t1".into());
        sub.ts = 100.0;
        let mut stop = event("Stop", "s", None);
        stop.ts = 101.0;
        assert_eq!(in_flight_tools(&[sub.clone(), stop.clone()]).len(), 1);
        for closer in ["SubagentStop", "StopFailure"] {
            let mut end = event(closer, "s", Some("a1"));
            end.ts = 200.0;
            assert!(in_flight_tools(&[sub.clone(), stop.clone(), end]).is_empty(), "{closer}");
        }
    }

    #[test]
    fn one_sessions_work_never_appears_in_anothers_view() {
        let root = fake_clone("perview");
        let mut mine = session(Some("busy"), true);
        mine.session_id = "a".into();
        let mut theirs = event("PreToolUse", "b", None);
        theirs.tool_use_id = Some("t1".into());
        theirs.tool_name = Some("Bash".into());
        theirs.ts = 100.0;

        let view = view_of(&root, &mine, &[theirs], 12_000.0);
        // The regression this pins: a stale call belonging to the session next door used to
        // land in this one's evidence, and 3.3 hours of it read as a hung command.
        assert_eq!(view["in_flight_tool_calls"], json!([]));
        assert_eq!(view["session"]["generating"], json!(true));
        // No session id anywhere, so two sessions in one state share a cached answer.
        assert!(!view.to_string().contains("\"id\""), "{view}");
        let _ = std::fs::remove_dir_all(&root);
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
        let mut mine = session(Some("busy"), true);
        mine.session_id = "s".into();
        let mut pre = event("PreToolUse", "s", None);
        pre.tool_use_id = Some("t1".into());
        pre.tool_name = Some("Bash".into());
        pre.tool_input = Some(r#"{"command":"curl -sS http://127.0.0.1:9099/health"}"#.into());
        pre.ts = 100.0;

        let view = view_of(&root, &mine, &[pre], 609.0);
        // `busy` covers both writing tokens and sitting in a tool call. Reading it as
        // generating is what let a hung clone read as alive for as long as it hung.
        assert_eq!(view["session"]["generating"], json!(false));
        assert_eq!(view["in_flight_tool_calls"][0]["running_for_seconds"], json!(509.0));
        assert_eq!(view["in_flight_tool_calls"][0]["by"], json!("main agent"));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_tool_call_stamped_after_collection_never_reports_a_negative_age() {
        let root = fake_clone("clockrace");
        let mut mine = session(Some("busy"), true);
        mine.session_id = "s".into();
        let mut pre = event("PreToolUse", "s", None);
        pre.tool_use_id = Some("t1".into());
        pre.ts = 500.0;
        // The hook fired while the log was being read, so its stamp is ahead of `now`.
        let view = view_of(&root, &mine, &[pre], 499.0);
        assert_eq!(view["in_flight_tool_calls"][0]["running_for_seconds"], json!(0.0));
        let _ = std::fs::remove_dir_all(&root);
    }

    /// A `Stop` carrying one background task, with no output file written for it.
    fn stop_with_task(command: &str, at: f64) -> HookEvent {
        HookEvent {
            ts: at,
            background_tasks: vec![BackgroundTask {
                id: "task-1".into(),
                kind: "shell".into(),
                command: Some(command.into()),
                ..Default::default()
            }],
            ..event("Stop", "s", None)
        }
    }

    #[test]
    fn a_task_with_no_output_file_reports_an_unknown_age_not_a_fresh_one() {
        // Zero read as "it wrote something a moment ago", which is the opposite of what a
        // missing file means, and it read that way in the direction of `working`.
        let root = fake_clone("nooutputfile");
        let mut mine = session(Some("shell"), true);
        mine.session_id = "s".into();
        let view = view_of(&root, &mine, &[stop_with_task("sleep 30", 100.0)], 200.0);
        let task = &view["background_tasks"][0];
        assert_eq!(task["output_bytes"], json!(null));
        assert_eq!(task["output_still_for_seconds"], json!(null));
        assert_eq!(task["producing_output"], json!(false));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn the_view_says_how_long_the_turn_has_been_over() {
        // The number that settles a polling wait. Transcript mtime answers a different
        // question and answered it badly: a clone parked on three dead wait loops for 2h23m
        // reported 303 seconds of quiet, because something else in the tree was still moving.
        let root = fake_clone("turnover");
        let mut mine = session(Some("shell"), true);
        mine.session_id = "s".into();
        let view = view_of(&root, &mine, &[stop_with_task("until ! pgrep -f x; do sleep 20; done", 100.0)], 8680.0);
        assert_eq!(view["session"]["turn_over_for_seconds"], json!(8580.0));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_session_that_never_stopped_has_no_turn_over_age() {
        let root = fake_clone("turnovernone");
        let mut mine = session(Some("busy"), true);
        mine.session_id = "s".into();
        let view = view_of(&root, &mine, &[], 200.0);
        assert_eq!(view["session"]["turn_over_for_seconds"], json!(null));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn the_turn_over_age_is_bucketed_by_the_cache_key() {
        // It has to end in `_seconds` or every four-second tick mints a fresh question, which
        // is ~900 model calls an hour per session.
        let root = fake_clone("turnoverbucket");
        let mut mine = session(Some("shell"), true);
        mine.session_id = "s".into();
        let stop = stop_with_task("until ! pgrep -f x; do sleep 20; done", 100.0);
        let a = view_of(&root, &mine, std::slice::from_ref(&stop), 5000.0);
        let b = view_of(&root, &mine, &[stop], 5004.0);
        assert_ne!(a, b, "the raw views differ by the four seconds between ticks");
        assert_eq!(cache_key(&a), cache_key(&b), "one bucket, so one question");
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
    fn cursor_work_with_no_conversation_id_is_still_a_live_session() {
        // Measured on a live clone: Cursor fires tool calls carrying `session_id: ""`, 488 of
        // 3804 events over 3.3 hours, and for ten minutes they were the ONLY evidence that
        // clone was working. Every fold here keys on the owner, so all of it was skipped and
        // the clone read idle throughout.
        let root = fake_clone("cursorunowned");
        write_cursor(&root, 900, &["/usr/share/cursor/cursor"]);
        let t = soon();
        write_log(
            &root,
            &[
                json!({"hook_event_name": "preToolUse", "session_id": "", "tool_name": "Write",
                       "tool_use_id": "t1", "ts": t}),
                json!({"hook_event_name": "postToolUse", "session_id": "", "tool_name": "Write",
                       "tool_use_id": "t1", "ts": t + 1.0}),
            ],
        );
        let events = read_hook_events(&root);
        assert_eq!(events[0].session_id.as_deref(), Some(CURSOR_UNOWNED), "named at the door");

        let sessions = read_cursor_sessions(&root, &events, t + 5.0);
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].session_id, CURSOR_UNOWNED);
        assert_eq!(sessions[0].status.as_deref(), Some("busy"));
        assert_eq!(session_state(&sessions[0]), Verdict::Ask);

        // And the judge is told how long since it last moved, not that it has been silent for
        // an age: without an entry of its own it would inherit the no-transcript fallback.
        let view = view_of(&root, &sessions[0], &events, t + 5.0);
        assert_eq!(view["session"]["quiet_for_seconds"], json!(4.0));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn unnamed_cursor_work_stops_being_a_session_once_it_goes_quiet() {
        // The bound that keeps it honest. Nothing ever sends a `Stop` for a conversation
        // Cursor never named, so without this it would read busy for as long as the log lived,
        // which is the exact failure a Cursor subagent used to cause.
        let root = fake_clone("cursorunownedstale");
        write_cursor(&root, 900, &["/usr/share/cursor/cursor"]);
        let t = soon();
        write_log(
            &root,
            &[json!({"hook_event_name": "preToolUse", "session_id": "", "tool_use_id": "t1",
                     "ts": t})],
        );
        let events = read_hook_events(&root);
        // Either side of the window, not exactly on it: `t` is a real epoch stamp, where the
        // f64 spacing is about 2.4e-7, so `(t + 60.0) - t` is not reliably 60.0 and a test
        // written on the boundary flickers.
        assert_eq!(read_cursor_sessions(&root, &events, t + MOVING_WINDOW_S - 1.0).len(), 1);
        assert!(read_cursor_sessions(&root, &events, t + MOVING_WINDOW_S + 1.0).is_empty());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn only_cursors_events_are_given_the_unnamed_owner() {
        // Claude Code always names its session. An event of its own arriving without one is a
        // torn read or something new, and inventing an owner for it would fold two unrelated
        // things into one session.
        let root = fake_clone("cursorunownedclaude");
        write_cursor(&root, 900, &["/usr/share/cursor/cursor"]);
        let t = soon();
        write_log(
            &root,
            &[json!({"hook_event_name": "PreToolUse", "session_id": "", "tool_use_id": "t1",
                     "ts": t})],
        );
        let events = read_hook_events(&root);
        assert_eq!(events[0].session_id.as_deref(), Some(""));
        assert!(read_cursor_sessions(&root, &events, t + 1.0).is_empty());
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
        let sessions = read_cursor_sessions(&root, &events, clone_now(&events));
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].session_id, "c1");
        // Not statusless: Cursor writes a transcript only at turn end, so the mtime fallback
        // a statusless session gets would read a live generation as silence.
        assert_eq!(sessions[0].status.as_deref(), Some("busy"));
        assert!(sessions[0].alive);
        let view = view_of(&root, &sessions[0], &events, soon() + 5.0);
        assert_eq!(view["session"]["generating"], json!(true));
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
            let sessions = read_cursor_sessions(&root, &events, clone_now(&events));
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
        let sessions = read_cursor_sessions(&root, &events, clone_now(&events));
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
        let sessions = read_cursor_sessions(&root, &events, clone_now(&events));
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
        let sessions = read_cursor_sessions(&root, &events, clone_now(&events));
        let view = view_of(&root, &sessions[0], &events, t + 400.0);
        // `busy` covers generating and waiting on a command. The unmatched tool call is what
        // tells them apart, and the model needs the command itself to judge the second.
        assert_eq!(view["session"]["generating"], json!(false));
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
        assert!(read_cursor_sessions(&root, &events, clone_now(&events)).is_empty());
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
        assert!(read_cursor_sessions(&root, &events, clone_now(&events)).is_empty());

        // A lock naming a pid that a different process now holds.
        write_cursor(&root, 901, &["/usr/bin/python3", "something.py"]);
        assert!(read_cursor_sessions(&root, &events, clone_now(&events)).is_empty());

        // A lock naming one of Cursor's own renderer children rather than the main process.
        write_cursor(&root, 902, &["/usr/share/cursor/cursor", "--type=zygote"]);
        assert!(read_cursor_sessions(&root, &events, clone_now(&events)).is_empty());
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
        sessions.extend(read_cursor_sessions(&root, &events, clone_now(&events)));
        assert_eq!(clone_state(&sessions, true), Verdict::Ask);
        let _ = std::fs::remove_dir_all(&root);
    }

    // --- the GPT path ---------------------------------------------------------------------

    /// Trimmed from a real `gpt-5.6-luna` reply: the same events in the same order, with the
    /// reasoning blobs cut. Note `response.completed` carrying an empty `output`, which is
    /// why the answer has to be read off the item events.
    const CODEX_STREAM: &str = r#"event: response.created
data: {"type":"response.created","response":{"id":"resp_1","status":"in_progress","output":[]}}

event: response.output_item.done
data: {"type":"response.output_item.done","item":{"id":"rs_1","type":"reasoning","content":[],"encrypted_content":"gAAAAAB","summary":[]},"output_index":0}

event: response.output_text.delta
data: {"type":"response.output_text.delta","content_index":0,"delta":"{\"will_progress\":","item_id":"msg_1","output_index":1}

event: response.output_text.delta
data: {"type":"response.output_text.delta","content_index":0,"delta":"true}","item_id":"msg_1","output_index":1}

event: response.output_text.done
data: {"type":"response.output_text.done","content_index":0,"item_id":"msg_1","output_index":1,"text":"{\"will_progress\":true,\"reason\":\"the release build is still running\"}"}

event: response.output_item.done
data: {"type":"response.output_item.done","item":{"id":"msg_1","type":"message","status":"completed","content":[{"type":"output_text","annotations":[],"text":"{\"will_progress\":true,\"reason\":\"the release build is still running\"}"}],"role":"assistant"},"output_index":1}

event: response.completed
data: {"type":"response.completed","response":{"id":"resp_1","status":"completed","output":[],"usage":{"input_tokens":67,"output_tokens":66}}}

"#;

    #[test]
    fn the_answer_is_read_out_of_the_finished_message() {
        let text = codex_answer_text(CODEX_STREAM).expect("a stream that completed");
        let answer = parse_answer(&text);
        assert_eq!(answer.will_progress, Some(true));
        assert_eq!(answer.reason.as_deref(), Some("the release build is still running"));
    }

    /// The deltas are the last resort, for a backend that stops sending either finished form.
    #[test]
    fn deltas_alone_still_answer() {
        let deltas: String = CODEX_STREAM
            .lines()
            .filter(|l| !l.contains("output_text.done") && !l.contains(r#""type":"message""#))
            .collect::<Vec<_>>()
            .join("\n");
        assert_eq!(codex_answer_text(&deltas).unwrap(), r#"{"will_progress":true}"#);
    }

    /// A stream that died part way through has to be an error. Read as an empty answer it
    /// would parse to "no", which is a real verdict about the clone rather than about us.
    #[test]
    fn a_failed_stream_is_an_error_not_a_no() {
        let stream = "event: response.failed\ndata: {\"type\":\"response.failed\",\"response\":{\"error\":{\"message\":\"rate limit reached\"}}}\n";
        let e = codex_answer_text(stream).expect_err("a failed stream");
        assert!(format!("{e:#}").contains("rate limit reached"), "{e:#}");
    }

    /// The GPT path against the real endpoint.
    ///
    /// Everything above replays a captured stream, which cannot answer whether the request is
    /// still shaped the way ChatGPT wants it: the backend refuses `stream: false` outright,
    /// the model name has to exist, and the answer has to survive the whole prompt rather than
    /// the two-line one a probe would use. So this one is real, and `#[ignore]`d for it.
    ///
    ///     RMNG_CODEX_AUTH=$HOME/.codex/auth.json cargo test -p control-server \
    ///       gpt_answers_the_real_question -- --ignored --nocapture
    ///
    /// Two short calls on that account's ChatGPT plan. It reads the auth file and never writes
    /// it, so the refresh token it holds is not touched.
    #[tokio::test]
    #[ignore = "talks to chatgpt.com, needs RMNG_CODEX_AUTH pointing at a codex auth.json"]
    async fn gpt_answers_the_real_question() {
        let path = std::env::var("RMNG_CODEX_AUTH").expect("RMNG_CODEX_AUTH");
        let raw = std::fs::read_to_string(path).expect("a readable codex auth.json");
        let auth: Value = serde_json::from_str(&raw).unwrap();
        let token = auth["tokens"]["access_token"].as_str().expect("an access token");
        let account = auth["tokens"]["account_id"].as_str().expect("an account id");
        let http = reqwest::Client::new();

        // A release build that is still running: something machine-driven will finish it.
        let building = json!({
            "sessions": [{"status": "shell", "quiet_for_seconds": 40}],
            "background_tasks": [{"command": "cargo build --release", "running_for_seconds": 90}],
        });
        let answer = ask_codex(&http, token, account, "gpt-5.6-luna", &building).await.unwrap();
        assert_eq!(answer.will_progress, Some(true), "{answer:?}");
        println!("building: {answer:?}");

        // Finished, with a dev server it left running. Nothing there will ever wake anybody.
        let served = json!({
            "sessions": [{"status": "idle", "quiet_for_seconds": 600}],
            "background_tasks": [{"command": "npm run dev", "running_for_seconds": 900}],
            "agent_last_said": "The dev server is up on port 3000. Let me know what to build next.",
        });
        let answer = ask_codex(&http, token, account, "gpt-5.6-luna", &served).await.unwrap();
        assert_eq!(answer.will_progress, Some(false), "{answer:?}");
        println!("served: {answer:?}");
    }

}
