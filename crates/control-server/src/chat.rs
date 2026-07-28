//! Per-clone chat with the in-container agent-wrapper. Ports `agent.server.ts` +
//! `chats.server.ts` + `chatbus.server.ts`.
//!
//! Each clone has its own conversation (`data/chats/<id>.json`) and its own SSE
//! fan-out (keyed by clone id — message bodies never touch the global `/events`
//! frame). A turn runs **detached** from the POST request (it can take minutes; a
//! browser refresh must not kill it). The server owns the "busy" flag so the
//! working indicator + the eventual reply survive a reconnect. Watchdogs abort a
//! stalled (no activity for 3m) or over-long (30m) turn.

use std::collections::{HashMap, HashSet};
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use futures::StreamExt;
use serde::{Deserialize, Serialize};
use wire::{Chat, ChatMessage, ChatRole, RmngClone, ScheduledMessage};

use crate::app::App;
use crate::files::is_safe_id;

const IDLE_TIMEOUT: Duration = Duration::from_secs(3 * 60);
const MAX_TURN: Duration = Duration::from_secs(30 * 60);
const ACTIVITY_MAX: usize = 200;

/// Per-clone chat fan-out + in-flight state.
#[derive(Default)]
pub struct ChatState {
    senders: Mutex<HashMap<String, tokio::sync::broadcast::Sender<String>>>,
    busy: Mutex<HashSet<String>>,
    activity: Mutex<HashMap<String, String>>,
    listeners: Mutex<HashSet<String>>,
    /// Serialises the read-modify-write of `data/schedules/<id>.json`. The HTTP handlers and
    /// the scheduler tick both mutate those files, and a lost update there means a message the
    /// operator queued silently never fires (or fires twice). One process-wide lock is plenty:
    /// the critical sections are a few-KB file rewrite.
    schedule_io: Mutex<()>,
}

fn now_ms() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0)
}

fn short_id() -> String {
    let t = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
    format!("{:08x}", (t as u64) & 0xFFFF_FFFF)
}

async fn base_url(app: &App, host: &RmngClone) -> String {
    format!("http://{}:{}", app.dial_clone(host).await, app.config().agent_port)
}

// --- chat storage (mirrors notes) ------------------------------------------

fn chat_path(data_dir: &str, id: &str) -> Option<std::path::PathBuf> {
    is_safe_id(id).then(|| std::path::Path::new(data_dir).join("chats").join(format!("{id}.json")))
}

pub fn load_chat(data_dir: &str, id: &str) -> Chat {
    chat_path(data_dir, id)
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

pub fn save_chat(data_dir: &str, id: &str, chat: &Chat) {
    let Some(path) = chat_path(data_dir, id) else { return };
    if let Some(d) = path.parent() {
        let _ = std::fs::create_dir_all(d);
    }
    let tmp = path.with_extension(format!("tmp.{}", std::process::id()));
    if let Ok(mut body) = serde_json::to_string_pretty(chat) {
        body.push('\n');
        if std::fs::write(&tmp, body).is_ok() {
            let _ = std::fs::rename(&tmp, &path);
        }
    }
}

pub fn delete_chat(data_dir: &str, id: &str) {
    if let Some(p) = chat_path(data_dir, id) {
        let _ = std::fs::remove_file(p);
    }
}

// --- scheduled-message storage ---------------------------------------------
//
// Queued-but-not-yet-delivered messages live in `data/schedules/<id>.json` (same atomic
// temp+rename write as the chat itself). Disk is the source of truth rather than an
// in-memory timer wheel, so a restart loses nothing: the scheduler simply re-reads the
// files on its next tick and anything that came due while the server was down fires then
// (late, but delivered — see `due_messages`).

/// How long past its `at` a message may keep waiting for a busy/offline clone before the
/// scheduler gives up on it. Without a bound, a clone that is wedged mid-turn (or was
/// deleted while the server was down) would accumulate an ever-retrying backlog that fires
/// as a surprise burst hours later. We drop instead of firing late-and-unbounded, and log
/// loudly — a message the operator timed for "in 20 minutes" is rarely still wanted a day
/// later, and a silent forever-queue is worse than a visible drop.
const SCHEDULE_GRACE: i64 = 60 * 60 * 1000;

fn schedule_path(data_dir: &str, id: &str) -> Option<std::path::PathBuf> {
    is_safe_id(id)
        .then(|| std::path::Path::new(data_dir).join("schedules").join(format!("{id}.json")))
}

pub fn load_schedules(data_dir: &str, id: &str) -> Vec<ScheduledMessage> {
    let mut list: Vec<ScheduledMessage> = schedule_path(data_dir, id)
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default();
    list.sort_by_key(|m| m.at);
    list
}

fn save_schedules(data_dir: &str, id: &str, list: &[ScheduledMessage]) {
    let Some(path) = schedule_path(data_dir, id) else { return };
    // An empty queue is the common steady state; removing the file keeps `data/schedules/`
    // from filling with `[]` stubs for every clone that ever scheduled anything once.
    if list.is_empty() {
        let _ = std::fs::remove_file(&path);
        return;
    }
    if let Some(d) = path.parent() {
        let _ = std::fs::create_dir_all(d);
    }
    let tmp = path.with_extension(format!("tmp.{}", std::process::id()));
    if let Ok(mut body) = serde_json::to_string_pretty(list) {
        body.push('\n');
        if std::fs::write(&tmp, body).is_ok() {
            let _ = std::fs::rename(&tmp, &path);
        }
    }
}

pub fn delete_schedules(data_dir: &str, id: &str) {
    if let Some(p) = schedule_path(data_dir, id) {
        let _ = std::fs::remove_file(p);
    }
}

/// Validate an operator-supplied `(text, at)` into a `ScheduledMessage` relative to `now`.
///
/// Pure so the rules are testable without a filesystem: non-empty trimmed text, and a
/// delivery time strictly in the future. `now` is threaded in rather than read from the
/// clock for the same reason.
fn build_scheduled(text: &str, at: i64, now: i64) -> Result<ScheduledMessage, String> {
    let text = text.trim();
    if text.is_empty() {
        return Err("empty message".into());
    }
    if at <= now {
        return Err("scheduled time must be in the future".into());
    }
    Ok(ScheduledMessage { id: short_id(), text: text.to_string(), at, created_at: now })
}

/// Queue a message for later delivery to `host_id`. Rejects past times and blank text.
pub fn schedule_message(app: &App, host_id: &str, text: &str, at: i64) -> Result<ScheduledMessage, String> {
    let mut msg = build_scheduled(text, at, now_ms())?;
    let data_dir = app.config().data_dir;
    {
        let _guard = app.chat.schedule_io.lock().unwrap();
        let mut list = load_schedules(&data_dir, host_id);
        // `short_id` is a truncated nanosecond clock; two schedules created in the same tick
        // would otherwise share an id and the cancel button would remove the wrong one.
        while list.iter().any(|m| m.id == msg.id) {
            msg.id = format!("{}{:x}", msg.id, list.len());
        }
        list.push(msg.clone());
        save_schedules(&data_dir, host_id, &list);
    }
    broadcast(app, host_id);
    Ok(msg)
}

/// Cancel a pending scheduled message. `false` when no such id is queued (already fired,
/// or already cancelled from another tab).
pub fn cancel_schedule(app: &App, host_id: &str, sid: &str) -> bool {
    let data_dir = app.config().data_dir;
    let removed = {
        let _guard = app.chat.schedule_io.lock().unwrap();
        let mut list = load_schedules(&data_dir, host_id);
        let before = list.len();
        list.retain(|m| m.id != sid);
        let removed = list.len() != before;
        if removed {
            save_schedules(&data_dir, host_id, &list);
        }
        removed
    };
    if removed {
        broadcast(app, host_id);
    }
    removed
}

/// The transcript bubble left behind when a scheduled message expires undelivered.
///
/// Lateness is rendered in whole hours rather than a wall-clock date: the workspace carries
/// no date library, and "6h late" is the fact the operator needs anyway — the frontend
/// already renders absolute times in their own locale. The original text is quoted in full
/// so it can be copied back into the composer and re-sent.
fn expired_notice(m: &ScheduledMessage, now: i64) -> String {
    let hours = (now - m.at) / 3_600_000;
    format!(
        "⚠ A scheduled message was never delivered — this clone stayed unavailable for {hours}h \
         past the time you picked, so it was dropped rather than sent arbitrarily late. \
         It was NOT sent:\n\n{}",
        m.text
    )
}

/// Split a queue into `(due, expired)` at `now`: messages whose time has passed and are
/// still inside the grace window, and those so far past it that the scheduler should drop
/// them. Pure — this is the piece the scheduler's correctness hinges on, so it is tested
/// directly rather than through the tick loop.
fn due_messages(list: &[ScheduledMessage], now: i64) -> (Vec<ScheduledMessage>, Vec<ScheduledMessage>) {
    let mut due = Vec::new();
    let mut expired = Vec::new();
    for m in list.iter().filter(|m| m.at <= now) {
        if now - m.at > SCHEDULE_GRACE {
            expired.push(m.clone());
        } else {
            due.push(m.clone());
        }
    }
    (due, expired)
}

// --- chat bus --------------------------------------------------------------

#[derive(Serialize)]
struct ChatSnapshot {
    busy: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    activity: Option<String>,
    messages: Vec<ChatMessage>,
    /// Pending scheduled messages, soonest first. Riding the existing chat frame keeps the
    /// queue live across tabs (a cancel in one is reflected in the other) with no second stream.
    scheduled: Vec<ScheduledMessage>,
}

/// The `{ busy, activity, messages, scheduled }` snapshot as JSON — the chat history plus the
/// clone agent's live working state. Used by the SSE bus and the fleet MCP `read_chat`.
pub fn snapshot_json(app: &App, host_id: &str) -> String {
    let data_dir = app.config().data_dir;
    let snap = ChatSnapshot {
        busy: app.chat.busy.lock().unwrap().contains(host_id),
        activity: app.chat.activity.lock().unwrap().get(host_id).cloned(),
        messages: load_chat(&data_dir, host_id).messages,
        scheduled: load_schedules(&data_dir, host_id),
    };
    serde_json::to_string(&snap).unwrap_or_else(|_| "{}".into())
}

fn sender_for(app: &App, host_id: &str) -> tokio::sync::broadcast::Sender<String> {
    app.chat
        .senders
        .lock()
        .unwrap()
        .entry(host_id.to_string())
        .or_insert_with(|| tokio::sync::broadcast::channel(32).0)
        .clone()
}

fn broadcast(app: &App, host_id: &str) {
    let _ = sender_for(app, host_id).send(snapshot_json(app, host_id));
}

/// A new SSE subscriber: current snapshot + a live receiver.
pub fn subscribe(app: &App, host_id: &str) -> (String, tokio::sync::broadcast::Receiver<String>) {
    let rx = sender_for(app, host_id).subscribe();
    (snapshot_json(app, host_id), rx)
}

pub fn is_busy(app: &App, host_id: &str) -> bool {
    app.chat.busy.lock().unwrap().contains(host_id)
}

fn set_busy(app: &App, host_id: &str, busy: bool) {
    if busy {
        app.chat.busy.lock().unwrap().insert(host_id.to_string());
    } else {
        app.chat.busy.lock().unwrap().remove(host_id);
    }
    app.chat.activity.lock().unwrap().remove(host_id); // only meaningful during a turn
    broadcast(app, host_id);
}

fn set_activity(app: &App, host_id: &str, activity: String) {
    if !is_busy(app, host_id) {
        return; // late event after the turn ended
    }
    app.chat.activity.lock().unwrap().insert(host_id.to_string(), activity);
    broadcast(app, host_id);
}

/// Broadcast that the persisted chat changed (e.g. an autonomous message landed).
pub fn chat_changed(app: &App, host_id: &str) {
    broadcast(app, host_id);
}

fn clip_activity(s: &str) -> String {
    let one_line = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if one_line.chars().count() > ACTIVITY_MAX {
        let mut t: String = one_line.chars().take(ACTIVITY_MAX - 1).collect();
        t.push('…');
        t
    } else {
        one_line
    }
}

fn push_message(app: &App, host_id: &str, role: ChatRole, text: String) {
    let data_dir = app.config().data_dir;
    let mut chat = load_chat(&data_dir, host_id);
    chat.messages.push(ChatMessage { id: short_id(), role, text, ts: now_ms() });
    save_chat(&data_dir, host_id, &chat);
}

// --- agent-wrapper protocol ------------------------------------------------

#[derive(Deserialize)]
struct TurnFrame {
    #[serde(default)]
    activity: Option<String>,
    #[serde(default)]
    reply: Option<String>,
    /// false ⇒ autonomous background-task message, not the answer to a /prompt.
    #[serde(default)]
    solicited: Option<bool>,
    #[serde(default)]
    error: Option<String>,
    /// The agent-wrapper's turn-liveness flag: `true` when a turn starts, `false` when it ends
    /// (and once as a snapshot when the SSE stream opens). This is the fleet's `working`/`idle`
    /// signal — see [`crate::monitor::ActivityBus`] for why it is read from here.
    #[serde(default)]
    busy: Option<bool>,
}

async fn post_abort(app: &App, base: &str) {
    let _ = app.http.post(format!("{base}/abort")).timeout(Duration::from_secs(5)).send().await;
}

/// Persist the user message, kick off the turn detached, return the new chat.
pub fn send_chat(app: &App, host: &RmngClone, text: &str) -> Result<(), String> {
    let text = text.trim();
    if text.is_empty() {
        return Err("empty message".into());
    }
    if is_busy(app, &host.id) {
        return Err("a message is already being processed for this clone".into());
    }
    push_message(app, &host.id, ChatRole::User, text.to_string());
    set_busy(app, &host.id, true);
    let (app2, host2, text2) = (app.clone(), host.clone(), text.to_string());
    tokio::spawn(async move { run_turn(app2, host2, text2).await });
    Ok(())
}

async fn run_turn(app: App, host: RmngClone, text: String) {
    let base = base_url(&app, &host).await;
    let reply = run_turn_inner(&app, &host.id, &base, &text).await;
    push_message(&app, &host.id, ChatRole::Assistant, reply);
    set_busy(&app, &host.id, false);
}

/// Open the wrapper's event stream, prompt once a subscriber is live, relay
/// activity, and return the reply text (or a ⚠ message on failure/timeout).
async fn run_turn_inner(app: &App, host_id: &str, base: &str, text: &str) -> String {
    let resp = match app
        .http
        .get(format!("{base}/events"))
        .header("accept", "text/event-stream")
        .send()
        .await
    {
        Ok(r) if r.status().is_success() => r,
        Ok(r) => return format!("⚠ agent events HTTP {}", r.status().as_u16()),
        Err(e) => return format!("⚠ {e}"),
    };

    let mut stream = resp.bytes_stream();
    let mut buf: Vec<u8> = Vec::new();
    let mut prompted = false;
    let start = Instant::now();

    loop {
        if start.elapsed() > MAX_TURN {
            post_abort(app, base).await;
            return "⚠ The turn exceeded the time limit and was stopped.".into();
        }
        let next = tokio::time::timeout(IDLE_TIMEOUT, stream.next()).await;
        let chunk = match next {
            Err(_) => {
                post_abort(app, base).await;
                return "⚠ The agent stalled (no output for a while) and was stopped.".into();
            }
            Ok(None) => return "⚠ event stream ended".into(),
            Ok(Some(Err(e))) => return format!("⚠ {e}"),
            Ok(Some(Ok(b))) => b,
        };
        buf.extend_from_slice(&chunk);

        while let Some(pos) = find_subslice(&buf, b"\n\n") {
            let frame: Vec<u8> = buf.drain(..pos + 2).collect();
            let Some(json) = extract_data_line(&frame[..frame.len() - 2]) else { continue };

            // Any data frame ⇒ the subscriber is live ⇒ safe to prompt.
            if !prompted {
                prompted = true;
                if let Err(msg) = post_prompt(app, base, text).await {
                    return msg;
                }
            }
            let Ok(f) = serde_json::from_str::<TurnFrame>(&json) else { continue };
            if let Some(a) = f.activity {
                set_activity(app, host_id, clip_activity(&a));
            } else if let Some(r) = f.reply {
                if f.solicited == Some(false) {
                    continue; // autonomous → the persistent listener handles it
                }
                let r = r.trim();
                return if r.is_empty() { "(no response)".into() } else { r.to_string() };
            } else if let Some(e) = f.error {
                return format!("⚠ {e}");
            }
        }
    }
}

async fn post_prompt(app: &App, base: &str, text: &str) -> Result<(), String> {
    match app
        .http
        .post(format!("{base}/prompt"))
        .json(&serde_json::json!({ "text": text }))
        .send()
        .await
    {
        Ok(r) if r.status().as_u16() == 202 || r.status().is_success() => Ok(()),
        Ok(r) if r.status().as_u16() == 409 => {
            Err("⚠ the agent is already processing a turn".into())
        }
        Ok(r) => Err(format!("⚠ agent prompt HTTP {}", r.status().as_u16())),
        Err(e) => Err(format!("⚠ {e}")),
    }
}

fn find_subslice(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

/// Extract the JSON after the first `data:` line of an SSE frame.
fn extract_data_line(frame: &[u8]) -> Option<String> {
    let s = std::str::from_utf8(frame).ok()?;
    s.lines().find_map(|l| l.strip_prefix("data:")).map(|j| j.trim().to_string()).filter(|j| !j.is_empty())
}

/// Interrupt the clone's in-flight turn (best-effort).
pub async fn abort_chat(app: &App, host: &RmngClone) {
    post_abort(app, &base_url(app, host).await).await;
}

// --- kickoff (post-clone first message) ------------------------------------

#[derive(Default)]
pub struct KickoffOpts {
    pub ticket_url: Option<String>,
    pub message: Option<String>,
    pub agent_instructions: Option<String>,
    pub claude_instructions: Option<String>,
}

/// After a clone, wait for the wrapper to accept its event stream, then send the kickoff
/// message (ticket URL or plain first message + optional instruction overrides).
pub async fn kickoff_agent(app: App, host: RmngClone, opts: KickoffOpts) {
    let mut msg = opts.ticket_url.clone().or(opts.message.clone()).unwrap_or_default().trim().to_string();
    if msg.is_empty() {
        return;
    }
    let deadline = Instant::now() + Duration::from_secs(90);
    while Instant::now() < deadline {
        // Opening the SSE response verifies wrapper readiness without reviving a clone-status
        // endpoint or creating a duplicate chat turn. Dropping the response immediately closes
        // this probe subscriber.
        let url = format!("{}/events", base_url(&app, &host).await);
        let ready = app
            .http
            .get(&url)
            .header("accept", "text/event-stream")
            .timeout(Duration::from_secs(4))
            .send()
            .await
            .is_ok_and(|response| response.status().is_success());
        if ready {
            break;
        }
        tokio::time::sleep(Duration::from_secs(4)).await;
    }
    if let Some(a) = opts.agent_instructions.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        msg += &format!(
            "\n\nAdditional clone-agent instructions (these take precedence — merge them with your procedure):\n{a}"
        );
    }
    if let Some(c) = opts.claude_instructions.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        msg += &format!(
            "\n\nAdditional Claude Code instructions (these take precedence — merge them into the prompt you give Claude Code):\n{c}"
        );
    }
    if let Err(e) = send_chat(&app, &host, &msg) {
        tracing::warn!("kickoff_agent: could not send to {}: {e}", host.id);
    }
}

// --- autonomous background-task message listener ----------------------------

/// Idempotent: keep one persistent `/events` subscription per clone that persists
/// UNSOLICITED assistant messages into the chat (the Docker maintenance poller starts this for
/// running managed clones; a dropped listener is restarted on the next tick).
pub fn ensure_autonomous_listener(app: &App, host: &RmngClone) {
    {
        let mut l = app.chat.listeners.lock().unwrap();
        if !l.insert(host.id.clone()) {
            return; // already running
        }
    }
    let (app, host) = (app.clone(), host.clone());
    tokio::spawn(async move {
        let _ = run_autonomous_listener(&app, &host).await;
        app.chat.listeners.lock().unwrap().remove(&host.id);
    });
}

// --- scheduled-message delivery loop ---------------------------------------

const SCHEDULE_TICK: Duration = Duration::from_secs(10);

/// Deliver scheduled messages as they come due.
///
/// Ticking a short interval against the on-disk queues (rather than arming a timer per
/// message) is what makes this survive a restart for free: whatever is on disk at boot is
/// simply evaluated on the first tick, so a message that came due while the server was down
/// fires immediately instead of being lost with the process. The 10s granularity is far
/// finer than the human-scale intent behind "send this at 3pm".
pub async fn run_scheduler(app: App) {
    loop {
        tick_schedules(&app);
        tokio::time::sleep(SCHEDULE_TICK).await;
    }
}

/// One sweep: for every clone with a queue, fire what is due and prune what can never fire.
///
/// Three ways a due message does *not* get delivered:
/// - **the clone is mid-turn** — left queued and retried next tick; scheduling *while* busy is
///   an explicitly supported case, so dropping here would defeat the feature. Bounded by
///   `SCHEDULE_GRACE` so a permanently wedged clone can't queue forever.
/// - **the clone is archived** — same treatment: archiving is reversible, so the message waits
///   (within the grace window) for an unarchive rather than vanishing.
/// - **the clone no longer exists** — unrecoverable; dropped with a warning.
fn tick_schedules(app: &App) {
    let data_dir = app.config().data_dir;
    let dir = std::path::Path::new(&data_dir).join("schedules");
    let Ok(entries) = std::fs::read_dir(&dir) else { return };
    let ids: Vec<String> = entries
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            name.strip_suffix(".json").map(str::to_string)
        })
        .filter(|id| is_safe_id(id))
        .collect();

    for id in ids {
        let hosts = app.store.get().hosts;
        let host = hosts.iter().find(|h| h.id == id).cloned();
        let now = now_ms();
        let (due, expired) = due_messages(&load_schedules(&data_dir, &id), now);
        if due.is_empty() && expired.is_empty() {
            continue;
        }
        for m in &expired {
            tracing::warn!(
                "scheduled message {} for clone {id} expired undelivered ({}m late): {:?}",
                m.id,
                (now - m.at) / 60_000,
                m.text.chars().take(80).collect::<String>()
            );
            // A server log is invisible to the operator who queued this and walked away, and
            // silently swallowing their message is the one outcome scheduling must not have.
            // Leave a marker in the transcript itself so the drop is discoverable where they
            // will actually look. Only for expiry — an unknown clone has no transcript to
            // write to (and is handled below).
            if host.is_some() {
                push_message(app, &id, ChatRole::Assistant, expired_notice(m, now));
            }
        }
        let mut fired: Vec<String> = expired.iter().map(|m| m.id.clone()).collect();

        match host {
            None => {
                tracing::warn!("dropping {} scheduled message(s) for unknown clone {id}", due.len());
                fired.extend(due.iter().map(|m| m.id.clone()));
            }
            Some(host) if host.archived => {
                tracing::debug!("clone {id} is archived; {} scheduled message(s) wait", due.len());
            }
            Some(host) => {
                // One per tick: `send_chat` refuses while a turn is in flight, so the rest of
                // the queue is naturally retried on later ticks in `at` order.
                if let Some(m) = due.first() {
                    match send_chat(app, &host, &m.text) {
                        Ok(()) => fired.push(m.id.clone()),
                        Err(e) => {
                            tracing::debug!("scheduled message {} for {id} deferred: {e}", m.id)
                        }
                    }
                }
            }
        }

        if fired.is_empty() {
            continue;
        }
        {
            let _guard = app.chat.schedule_io.lock().unwrap();
            let mut list = load_schedules(&data_dir, &id);
            list.retain(|m| !fired.contains(&m.id));
            save_schedules(&data_dir, &id, &list);
        }
        broadcast(app, &id);
    }
}

async fn run_autonomous_listener(app: &App, host: &RmngClone) -> Result<(), ()> {
    let base = base_url(app, host).await;
    let resp = app
        .http
        .get(format!("{base}/events"))
        .header("accept", "text/event-stream")
        .send()
        .await
        .map_err(|_| ())?;
    if !resp.status().is_success() {
        return Err(());
    }
    let mut stream = resp.bytes_stream();
    let mut buf: Vec<u8> = Vec::new();
    while let Some(item) = stream.next().await {
        let chunk = item.map_err(|_| ())?;
        buf.extend_from_slice(&chunk);
        while let Some(pos) = find_subslice(&buf, b"\n\n") {
            let frame: Vec<u8> = buf.drain(..pos + 2).collect();
            let Some(json) = extract_data_line(&frame[..frame.len() - 2]) else { continue };
            let Ok(f) = serde_json::from_str::<TurnFrame>(&json) else { continue };
            // Turn liveness → the monitor's working/idle signal.
            //
            // `busy: true` marks a turn STARTING and `activity` lines stream throughout it, so
            // both are stamped: a turn running longer than the inactivity window would otherwise
            // slide to `idle` mid-work, because nothing else would have touched the clock since
            // the start frame. `busy: false` is deliberately NOT stamped — it marks the END of
            // work, and treating it as activity would hold a finished clone at `working` for a
            // further full window.
            if f.busy == Some(true) || f.activity.is_some() {
                app.activity.mark(&host.id, crate::clone_ops::now_ms());
            }
            if let Some(r) = f.reply {
                if f.solicited == Some(false) {
                    let r = r.trim();
                    if !r.is_empty() {
                        push_message(app, &host.id, ChatRole::Assistant, r.to_string());
                        chat_changed(app, &host.id);
                    }
                }
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn msg(id: &str, at: i64) -> ScheduledMessage {
        ScheduledMessage { id: id.into(), text: "hi".into(), at, created_at: 0 }
    }

    #[test]
    fn build_scheduled_rejects_past_and_blank() {
        let now = 1_000_000i64;
        assert!(build_scheduled("hello", now + 60_000, now).is_ok());
        // Exactly now is already too late — the tick that would fire it may have just run.
        let past = build_scheduled("hello", now, now).unwrap_err();
        assert!(past.contains("future"), "msg: {past}");
        assert!(build_scheduled("hello", now - 1, now).is_err());
        assert!(build_scheduled("   \n ", now + 60_000, now).is_err());
        // Text is stored trimmed, and the queue time is what the caller asked for.
        let ok = build_scheduled("  spaced  ", now + 5, now).unwrap();
        assert_eq!(ok.text, "spaced");
        assert_eq!(ok.at, now + 5);
        assert_eq!(ok.created_at, now);
    }

    #[test]
    fn due_messages_splits_at_now_and_grace() {
        let now = 10_000_000i64;
        let list = vec![
            msg("future", now + 1),
            msg("exactly-now", now),
            msg("late", now - 60_000),
            msg("expired", now - SCHEDULE_GRACE - 1),
        ];
        let (due, expired) = due_messages(&list, now);
        let due_ids: Vec<&str> = due.iter().map(|m| m.id.as_str()).collect();
        assert_eq!(due_ids, vec!["exactly-now", "late"]);
        assert_eq!(expired.iter().map(|m| m.id.as_str()).collect::<Vec<_>>(), vec!["expired"]);
        // A message right on the grace boundary is still deliverable.
        let (due, expired) = due_messages(&[msg("edge", now - SCHEDULE_GRACE)], now);
        assert_eq!(due.len(), 1);
        assert!(expired.is_empty());
    }

    #[test]
    fn schedule_round_trips_through_disk_and_snapshot() {
        let app = App::test_app();
        let dd = app.config().data_dir;
        let now = now_ms();
        let a = schedule_message(&app, "c1", "later", now + 3_600_000).unwrap();
        let b = schedule_message(&app, "c1", "sooner", now + 60_000).unwrap();
        assert_ne!(a.id, b.id);

        // Reloaded from disk (not from memory) and ordered soonest-first.
        let loaded = load_schedules(&dd, "c1");
        assert_eq!(loaded.iter().map(|m| m.id.as_str()).collect::<Vec<_>>(), vec![&b.id, &a.id]);
        assert_eq!(loaded[0].text, "sooner");
        assert_eq!(loaded[1].at, now + 3_600_000);

        // ...and it reaches the SSE frame the frontend reads, camelCased.
        let snap: serde_json::Value = serde_json::from_str(&snapshot_json(&app, "c1")).unwrap();
        assert_eq!(snap["scheduled"].as_array().unwrap().len(), 2);
        assert_eq!(snap["scheduled"][0]["text"], "sooner");
        assert!(snap["scheduled"][0]["createdAt"].is_i64(), "createdAt must be camelCase");

        assert!(cancel_schedule(&app, "c1", &b.id));
        assert!(!cancel_schedule(&app, "c1", &b.id), "second cancel is a no-op");
        assert_eq!(load_schedules(&dd, "c1").iter().map(|m| m.id.clone()).collect::<Vec<_>>(), vec![a.id.clone()]);

        // Emptying the queue removes the file rather than leaving an `[]` stub.
        assert!(cancel_schedule(&app, "c1", &a.id));
        assert!(load_schedules(&dd, "c1").is_empty());
        assert!(!schedule_path(&dd, "c1").unwrap().exists());
    }

    #[test]
    fn schedule_rejects_unsafe_clone_id() {
        let app = App::test_app();
        // A traversal id has no valid path, so nothing is written and nothing loads back.
        let _ = schedule_message(&app, "../evil", "x", now_ms() + 60_000);
        assert!(load_schedules(&app.config().data_dir, "../evil").is_empty());
        assert!(schedule_path(&app.config().data_dir, "../evil").is_none());
    }

    #[tokio::test]
    async fn tick_drops_schedules_for_unknown_clones_but_keeps_archived_ones() {
        let app = App::test_app();
        let dd = app.config().data_dir;
        app.store.mutate(|s| {
            s.hosts.push(RmngClone {
                id: "sleeping".into(),
                host: "sleeping".into(),
                managed: true,
                archived: true,
                ..Default::default()
            });
        });
        // Both are due, but only the one whose clone still exists survives the sweep.
        schedule_message(&app, "ghost", "gone", now_ms() + 1_000).unwrap();
        schedule_message(&app, "sleeping", "wait", now_ms() + 1_000).unwrap();
        for id in ["ghost", "sleeping"] {
            let mut list = load_schedules(&dd, id);
            list[0].at = now_ms() - 5_000;
            save_schedules(&dd, id, &list);
        }

        tick_schedules(&app);
        assert!(load_schedules(&dd, "ghost").is_empty(), "unknown clone → dropped");
        assert_eq!(load_schedules(&dd, "sleeping").len(), 1, "archived clone → still queued");
    }

    #[tokio::test]
    async fn expired_message_leaves_a_notice_in_the_transcript() {
        let app = App::test_app();
        let dd = app.config().data_dir;
        app.store.mutate(|s| {
            s.hosts.push(RmngClone { id: "wedged".into(), host: "wedged".into(), ..Default::default() });
        });
        schedule_message(&app, "wedged", "deploy the thing", now_ms() + 1_000).unwrap();
        // Push it well past the grace window, as a clone that was down all day would be.
        let mut list = load_schedules(&dd, "wedged");
        list[0].at = now_ms() - SCHEDULE_GRACE - 6 * 3_600_000;
        save_schedules(&dd, "wedged", &list);

        tick_schedules(&app);

        assert!(load_schedules(&dd, "wedged").is_empty(), "expired message is dropped");
        // ...but the operator finds out where they'd actually look, with their text intact.
        let msgs = load_chat(&dd, "wedged").messages;
        let notice = msgs.last().expect("an expiry notice must be written to the transcript");
        assert_eq!(notice.role, ChatRole::Assistant);
        assert!(notice.text.contains("deploy the thing"), "quotes the undelivered text: {}", notice.text);
        assert!(notice.text.contains("NOT sent"), "says plainly it did not go: {}", notice.text);
    }

    #[tokio::test]
    async fn tick_leaves_due_message_queued_while_the_clone_is_busy() {
        let app = App::test_app();
        let dd = app.config().data_dir;
        app.store.mutate(|s| {
            s.hosts.push(RmngClone { id: "worker".into(), host: "worker".into(), ..Default::default() });
        });
        schedule_message(&app, "worker", "queued", now_ms() + 1_000).unwrap();
        let mut list = load_schedules(&dd, "worker");
        list[0].at = now_ms() - 5_000;
        save_schedules(&dd, "worker", &list);

        app.chat.busy.lock().unwrap().insert("worker".into());
        tick_schedules(&app);
        assert_eq!(load_schedules(&dd, "worker").len(), 1, "busy clone must not lose the message");
    }
}
