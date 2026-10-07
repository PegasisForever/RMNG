//! Per-clone chat with the assistant: a pi-web server (Settings → Presets → Assistant). Each clone has
//! one chat there, created by its first message; the chat's id is kept in
//! `data/chats/<id>.json`. The assistant owns the conversation. RMNG keeps one event-stream
//! subscription per chat ([`ensure_listener`]) and folds it into the
//! `{ busy, activity, messages, scheduled }` frame the panel reads, on a per-clone SSE fan-out
//! (message bodies never touch the global `/events` frame). Messages typed into the same chat
//! somewhere else, such as the assistant's own web page, arrive the same way.
//!
//! The first message of a new chat opens with a header: the RMNG server and the clone the chat
//! is for, and the playbook (global + preset append). The panel hides that header again.

use std::collections::{HashMap, HashSet};
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use futures::StreamExt;
use serde::Serialize;
use serde_json::{Value, json};
use wire::{Chat, ChatMessage, ChatRole, RmngClone, ScheduledMessage};

use crate::app::App;
use crate::files::is_safe_id;

const ACTIVITY_MAX: usize = 200;
/// Opens the first message of every new chat. The panel shows only what follows
/// [`MESSAGE_MARK`].
const HEADER_MARK: &str = "[From RMNG]";
const MESSAGE_MARK: &str = "\n\n[Message]\n";
/// How long a sent message counts as "busy" before the assistant's stream confirms it.
const SENDING_GRACE: Duration = Duration::from_secs(20);

/// Per-clone chat fan-out + the assistant chat's live state.
#[derive(Default)]
pub struct ChatState {
    senders: Mutex<HashMap<String, tokio::sync::broadcast::Sender<String>>>,
    /// The assistant chat as its event stream last described it, per clone.
    live: Mutex<HashMap<String, Live>>,
    /// Clones whose message was sent but not yet confirmed by the stream. Keeps the panel
    /// busy across the gap between the POST and the assistant's first event.
    sending: Mutex<HashMap<String, Instant>>,
    listeners: Mutex<HashSet<String>>,
    /// Serialises the read-modify-write of `data/schedules/<id>.json`. The HTTP handlers and
    /// the scheduler tick both mutate those files, and a lost update there means a message the
    /// operator queued silently never fires (or fires twice). One process-wide lock is plenty:
    /// the critical sections are a few-KB file rewrite.
    schedule_io: Mutex<()>,
}

#[derive(Default)]
struct Live {
    /// User and final assistant messages, oldest first. Tool steps are left out.
    messages: Vec<ChatMessage>,
    busy: bool,
    activity: Option<String>,
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

fn short_id() -> String {
    let t = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("{:08x}", (t as u64) & 0xFFFF_FFFF)
}

/// The assistant's origin, or the sentence the panel shows when none is set.
fn assistant_url(app: &App) -> Result<String, String> {
    let url = app
        .config()
        .assistant
        .url
        .trim()
        .trim_end_matches('/')
        .to_string();
    if url.is_empty() {
        return Err("No assistant is set. Add its URL in Settings → Presets → Assistant.".into());
    }
    Ok(url)
}

// --- chat storage (mirrors notes) ------------------------------------------

fn chat_path(data_dir: &str, id: &str) -> Option<std::path::PathBuf> {
    is_safe_id(id).then(|| {
        std::path::Path::new(data_dir)
            .join("chats")
            .join(format!("{id}.json"))
    })
}

pub fn load_chat(data_dir: &str, id: &str) -> Chat {
    chat_path(data_dir, id)
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

pub fn save_chat(data_dir: &str, id: &str, chat: &Chat) {
    let Some(path) = chat_path(data_dir, id) else {
        return;
    };
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

/// Forget the clone's chat. The assistant keeps its own copy of the conversation; the
/// clone's listener sees the file gone and stops.
pub fn delete_chat(data_dir: &str, id: &str) {
    if let Some(p) = chat_path(data_dir, id) {
        let _ = std::fs::remove_file(p);
    }
}

/// Add a line RMNG wrote itself (never sent to the assistant) to the clone's thread.
fn push_notice(app: &App, host_id: &str, text: String) {
    let data_dir = app.data_dir();
    let mut chat = load_chat(&data_dir, host_id);
    chat.notices.push(ChatMessage {
        id: format!("n{}", short_id()),
        role: ChatRole::Assistant,
        text,
        ts: now_ms(),
    });
    save_chat(&data_dir, host_id, &chat);
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
    is_safe_id(id).then(|| {
        std::path::Path::new(data_dir)
            .join("schedules")
            .join(format!("{id}.json"))
    })
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
    let Some(path) = schedule_path(data_dir, id) else {
        return;
    };
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
    Ok(ScheduledMessage {
        id: short_id(),
        text: text.to_string(),
        at,
        created_at: now,
    })
}

/// Queue a message for later delivery to `host_id`. Rejects past times and blank text.
pub fn schedule_message(
    app: &App,
    host_id: &str,
    text: &str,
    at: i64,
) -> Result<ScheduledMessage, String> {
    let mut msg = build_scheduled(text, at, now_ms())?;
    let data_dir = app.data_dir();
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
    let data_dir = app.data_dir();
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
fn due_messages(
    list: &[ScheduledMessage],
    now: i64,
) -> (Vec<ScheduledMessage>, Vec<ScheduledMessage>) {
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

/// The `{ busy, activity, messages, scheduled }` snapshot as JSON: the assistant chat merged
/// with RMNG's own notices by time, plus the live working state.
pub fn snapshot_json(app: &App, host_id: &str) -> String {
    let data_dir = app.data_dir();
    let chat = load_chat(&data_dir, host_id);
    let (mut messages, live_busy, activity) = match app.chat.live.lock().unwrap().get(host_id) {
        Some(l) => (l.messages.clone(), l.busy, l.activity.clone()),
        None => (Vec::new(), false, None),
    };
    messages.extend(chat.notices);
    messages.sort_by_key(|m| m.ts);
    let busy = live_busy || is_sending(app, host_id);
    let snap = ChatSnapshot {
        busy,
        activity: activity.filter(|_| busy),
        messages,
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

fn is_sending(app: &App, host_id: &str) -> bool {
    app.chat
        .sending
        .lock()
        .unwrap()
        .get(host_id)
        .is_some_and(|t| t.elapsed() < SENDING_GRACE)
}

fn clear_sending(app: &App, host_id: &str) {
    app.chat.sending.lock().unwrap().remove(host_id);
}

pub fn is_busy(app: &App, host_id: &str) -> bool {
    is_sending(app, host_id)
        || app
            .chat
            .live
            .lock()
            .unwrap()
            .get(host_id)
            .is_some_and(|l| l.busy)
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

// --- the first message's header ---------------------------------------------

/// What opens a new chat: the server and clone it is for, and the playbook. Errs when this
/// server's own address is not set, because without it the assistant cannot reach the clone.
fn chat_header(cfg: &wire::AppConfig, host: &RmngClone) -> Result<String, String> {
    let server = cfg.assistant.server_url.trim().trim_end_matches('/');
    if server.is_empty() {
        return Err(
            "This server's address is not set. Add it in Settings → Presets → Assistant.".into(),
        );
    }
    let c = &host.id;
    let mut h = format!(
        "{HEADER_MARK} This chat is for one RMNG clone.\nRMNG server: {server}\nClone: {c}"
    );
    if let Some(title) = host
        .display_name
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        h += &format!(" ({title})");
    }
    h += "\n\n";
    h += &if host.headless {
        headless_rules(server, c)
    } else {
        desktop_rules(server, c)
    };
    if let Some(url) = host
        .linear_ticket_url
        .as_deref()
        .filter(|s| !s.trim().is_empty())
    {
        h += &format!("\n\nTicket: {url}");
    }
    let preset = crate::clone_reconcile::preset_for_clone(cfg, host);
    let playbook = crate::web::compose_playbook(cfg, preset);
    if !playbook.is_empty() {
        h += &format!("\n\nPlaybook for this clone, from the RMNG settings:\n\n{playbook}");
    }
    Ok(h)
}

/// How the assistant works a headed clone: through its desktop, the way a person at the
/// screen would, and nothing else. Said in the first message rather than left to the
/// assistant's own instructions, because it overrides playbooks written for the old in-clone
/// agent (which ran inside the clone, with a shell). Without it the assistant measured
/// screenshots with Python, searched the ledger, and did the steps in a shell.
fn desktop_rules(server: &str, c: &str) -> String {
    let r = format!("rmng --server {server} desktop {c}");
    format!(
        "Work this clone through its desktop, as a person at the screen would. These are \
         the only commands to use:\n\
         - `{r} screenshot`: prints the path of a 1920×1080 JPEG. Read that file.\n\
         - `{r} click X Y` (also `double-click`, `right-click`): pixels in that screenshot.\n\
         - `{r} type \"text\"`\n\
         - `{r} key \"ctrl+l\"`: X key names, case-sensitive (`Return`, `Escape`, `Tab`, \
         `BackSpace`, `Up`, `F5`), joined with `ctrl`, `shift`, `alt`, `super`.\n\
         - `{r} scroll N X Y`: N notches, positive is down.\n\
         - `{r} windows`: the open windows.\n\
         \n\
         Rules. They take precedence over the playbook below:\n\
         - Do every step on the desktop: click, type, press keys. To run a shell command, \
         open a terminal on the desktop (or VS Code's terminal) and type it there. Where the \
         playbook says to use the `desktop` tool or `mcp__desktop__*`, use the commands \
         above; where it says to use a shell, `setsid`, or the command line, use the desktop.\n\
         - Use no other command: no `rmng clone exec`, `rmng ledger`, `rmng guide`, and no \
         code of your own. Besides these commands, only read the screenshot files.\n\
         - Read each screenshot yourself and click what you see. Never crop, zoom, or \
         measure a screenshot with code (Python, PIL, ImageMagick). If you are not sure of \
         a small target, click your best estimate, then correct it.\n\
         - Every action prints the path of a screenshot taken after it. Read it to check the \
         result. Take a new screenshot only when the screen was still loading.\n\
         - Keep replies short: the person watches the same screen."
    )
}

/// A headless clone has no desktop, so its shell is the only way in.
fn headless_rules(server: &str, c: &str) -> String {
    format!(
        "The clone is headless: it has no desktop. Work it only with \
         `rmng --server {server} clone exec {c} -- <command>`. Where the playbook says to \
         use the `desktop` tool, there is none: do that step in the shell. Use no other \
         `rmng` command. Keep replies short."
    )
}

/// A user message as the panel shows it: without the header of the chat's first message.
fn visible_user_text(text: &str) -> &str {
    if text.starts_with(HEADER_MARK) {
        if let Some(i) = text.find(MESSAGE_MARK) {
            return &text[i + MESSAGE_MARK.len()..];
        }
    }
    text
}

// --- talking to the assistant (pi-web HTTP API) -----------------------------

async fn post_json(app: &App, url: &str, body: Value) -> Result<Value, String> {
    let resp = app
        .http
        .post(url)
        .json(&body)
        .send()
        .await
        .map_err(|e| e.to_string())?;
    let status = resp.status();
    let text = resp.text().await.unwrap_or_default();
    if !status.is_success() {
        return Err(format!("HTTP {}: {}", status.as_u16(), text.trim()));
    }
    Ok(serde_json::from_str(&text).unwrap_or(Value::Null))
}

/// Send a message to the clone's assistant chat, creating the chat (with its header) on the
/// first one. Returns once the checks pass; delivery runs detached and its reply arrives over
/// the clone's chat stream. A delivery failure becomes a notice in the thread.
pub fn send_chat(app: &App, host: &RmngClone, text: &str) -> Result<(), String> {
    let text = text.trim();
    if text.is_empty() {
        return Err("empty message".into());
    }
    if is_busy(app, &host.id) {
        return Err("the assistant is still working on this clone's last message".into());
    }
    let base = assistant_url(app)?;
    let session = load_chat(&app.data_dir(), &host.id).session_id;
    let message = match session {
        Some(_) => text.to_string(),
        None => format!("{}{MESSAGE_MARK}{text}", chat_header(&app.config(), host)?),
    };
    app.chat
        .sending
        .lock()
        .unwrap()
        .insert(host.id.clone(), Instant::now());
    broadcast(app, &host.id);
    let (app, host, text) = (app.clone(), host.clone(), text.to_string());
    tokio::spawn(async move {
        if let Err(e) = deliver(&app, &host, &base, session, message).await {
            tracing::warn!(
                "chat: message for {} did not reach the assistant: {e}",
                host.id
            );
            clear_sending(&app, &host.id);
            push_notice(
                &app,
                &host.id,
                format!("⚠ This message did not reach the assistant at {base}: {e}\n\n{text}"),
            );
            broadcast(&app, &host.id);
        }
    });
    Ok(())
}

async fn deliver(
    app: &App,
    host: &RmngClone,
    base: &str,
    session: Option<String>,
    message: String,
) -> Result<(), String> {
    match session {
        Some(sid) => {
            post_json(
                app,
                &format!("{base}/api/sessions/{sid}/message"),
                json!({ "message": message }),
            )
            .await?;
        }
        None => {
            let name = match host
                .display_name
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
            {
                Some(title) => format!("RMNG {}: {title}", host.id),
                None => format!("RMNG {}", host.id),
            };
            let created = post_json(
                app,
                &format!("{base}/api/sessions"),
                json!({ "name": name, "message": message }),
            )
            .await?;
            let sid = created["id"]
                .as_str()
                .filter(|s| !s.is_empty())
                .ok_or("the assistant created a chat but sent back no chat id")?;
            let data_dir = app.data_dir();
            let mut chat = load_chat(&data_dir, &host.id);
            chat.session_id = Some(sid.to_string());
            save_chat(&data_dir, &host.id, &chat);
        }
    }
    ensure_listener(app, host);
    Ok(())
}

/// Interrupt the assistant's current run on this clone's chat (best-effort).
pub async fn abort_chat(app: &App, host: &RmngClone) {
    let (Ok(base), Some(sid)) = (
        assistant_url(app),
        load_chat(&app.data_dir(), &host.id).session_id,
    ) else {
        return;
    };
    if let Err(e) = post_json(app, &format!("{base}/api/sessions/{sid}/abort"), json!({})).await {
        tracing::warn!("chat: abort for {} failed: {e}", host.id);
    }
}

// --- kickoff (post-clone first message) ------------------------------------

/// What the kickoff sends: the ticket URL, else the plain first message, plus the two
/// instruction overrides.
#[derive(Default)]
pub struct KickoffOpts {
    pub ticket_url: Option<String>,
    pub message: Option<String>,
    pub agent_instructions: Option<String>,
    pub claude_instructions: Option<String>,
}

/// After a clone, send the assistant its first message (ticket URL or plain first message +
/// optional instruction overrides). Called only for a create that asked for it
/// ([`wire::CloneRequest::kickoff`]).
pub async fn kickoff_agent(app: App, host: RmngClone, opts: KickoffOpts) {
    let mut msg = opts
        .ticket_url
        .clone()
        .or(opts.message.clone())
        .unwrap_or_default()
        .trim()
        .to_string();
    if msg.is_empty() {
        return;
    }
    // The assistant starts with a screenshot, so give a headed clone's desktop up to 90 s to
    // come up (its daemon registering) before it gets the message.
    let deadline = Instant::now() + Duration::from_secs(90);
    while !host.headless && !app.media.is_connected(&host.id) && Instant::now() < deadline {
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
    if let Some(a) = opts
        .agent_instructions
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        msg += &format!(
            "\n\nAdditional assistant instructions (these take precedence — merge them with your procedure):\n{a}"
        );
    }
    if let Some(c) = opts
        .claude_instructions
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        msg += &format!(
            "\n\nAdditional Claude Code instructions (these take precedence — merge them into the prompt you give Claude Code):\n{c}"
        );
    }
    if let Err(e) = send_chat(&app, &host, &msg) {
        tracing::warn!("kickoff_agent: could not send to {}: {e}", host.id);
        push_notice(
            &app,
            &host.id,
            format!("⚠ The first message was not sent: {e}\n\n{msg}"),
        );
        broadcast(&app, &host.id);
    }
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
    let data_dir = app.data_dir();
    let dir = std::path::Path::new(&data_dir).join("schedules");
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return;
    };
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
                push_notice(app, &id, expired_notice(m, now));
            }
        }
        let mut fired: Vec<String> = expired.iter().map(|m| m.id.clone()).collect();

        match host {
            None => {
                tracing::warn!(
                    "dropping {} scheduled message(s) for unknown clone {id}",
                    due.len()
                );
                fired.extend(due.iter().map(|m| m.id.clone()));
            }
            Some(host) if host.archived => {
                tracing::debug!(
                    "clone {id} is archived; {} scheduled message(s) wait",
                    due.len()
                );
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

// --- the assistant chat's event stream ----------------------------------------

/// Idempotent: keep one subscription to the clone's assistant chat, if it has one. The
/// monitor calls this for running clones and the panel's stream for any clone, so a dropped
/// listener comes back on the next call. A chat with no running agent costs the assistant no
/// process: pi-web serves it from its file.
pub fn ensure_listener(app: &App, host: &RmngClone) {
    if app.chat.listeners.lock().unwrap().contains(&host.id) {
        return;
    }
    if load_chat(&app.data_dir(), &host.id).session_id.is_none() {
        return;
    }
    if !app.chat.listeners.lock().unwrap().insert(host.id.clone()) {
        return;
    }
    let (app, id) = (app.clone(), host.id.clone());
    tokio::spawn(async move {
        run_listener(&app, &id).await;
        app.chat.listeners.lock().unwrap().remove(&id);
        app.chat.live.lock().unwrap().remove(&id);
        broadcast(&app, &id);
    });
}

/// The clone still exists and `sid` is still its chat.
fn still_current(app: &App, id: &str, sid: &str) -> bool {
    load_chat(&app.data_dir(), id).session_id.as_deref() == Some(sid)
        && app.store.get().hosts.iter().any(|h| h.id == id)
}

/// Follow the chat until the clone or its chat goes away, reconnecting with backoff.
async fn run_listener(app: &App, id: &str) {
    let mut backoff = Duration::from_secs(2);
    loop {
        let Some(sid) = load_chat(&app.data_dir(), id).session_id else {
            return;
        };
        if !app.store.get().hosts.iter().any(|h| h.id == id) {
            return;
        }
        let Ok(base) = assistant_url(app) else {
            return;
        };
        if follow(app, id, &base, &sid).await {
            backoff = Duration::from_secs(2);
        }
        if !still_current(app, id, &sid) {
            continue; // re-read: a new chat, or nothing left to follow
        }
        tokio::time::sleep(backoff).await;
        backoff = (backoff * 2).min(Duration::from_secs(60));
    }
}

/// One connection to the chat's event stream. Returns whether it connected at all.
async fn follow(app: &App, id: &str, base: &str, sid: &str) -> bool {
    let resp = match app
        .http
        .get(format!("{base}/api/sessions/{sid}/events"))
        .header("accept", "text/event-stream")
        .send()
        .await
    {
        Ok(r) if r.status().is_success() => r,
        Ok(r) => {
            tracing::debug!(
                "chat: events for {id} answered HTTP {}",
                r.status().as_u16()
            );
            return false;
        }
        Err(e) => {
            tracing::debug!("chat: events for {id}: {e}");
            return false;
        }
    };
    let mut stream = resp.bytes_stream();
    let mut buf: Vec<u8> = Vec::new();
    let mut checked = Instant::now();
    loop {
        // pi-web pings every 25 s, so this check runs about that often on a quiet chat.
        if checked.elapsed() > Duration::from_secs(30) {
            checked = Instant::now();
            if !still_current(app, id, sid) {
                return true;
            }
        }
        let chunk = match tokio::time::timeout(Duration::from_secs(60), stream.next()).await {
            Ok(Some(Ok(b))) => b,
            _ => return true,
        };
        buf.extend_from_slice(&chunk);
        while let Some(pos) = find_subslice(&buf, b"\n\n") {
            let frame: Vec<u8> = buf.drain(..pos + 2).collect();
            let Some((event, data)) = parse_frame(&frame[..frame.len() - 2]) else {
                continue;
            };
            match event.as_str() {
                "snapshot" => apply_snapshot(app, id, &data),
                "pi" => apply_event(app, id, &data),
                "error" => return true,
                _ => {}
            }
        }
    }
}

fn find_subslice(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

/// `(event, data)` of one SSE frame; `None` for a comment (ping) or a frame with no data.
fn parse_frame(frame: &[u8]) -> Option<(String, String)> {
    let s = std::str::from_utf8(frame).ok()?;
    let mut event = "message".to_string();
    let mut data = String::new();
    for line in s.lines() {
        if let Some(e) = line.strip_prefix("event:") {
            event = e.trim().to_string();
        } else if let Some(d) = line.strip_prefix("data:") {
            data.push_str(d.strip_prefix(' ').unwrap_or(d));
        }
    }
    (!data.is_empty()).then_some((event, data))
}

/// The text blocks of a pi message's `content` (a string, or an array of blocks).
fn content_text(content: &Value) -> String {
    match content {
        Value::String(s) => s.clone(),
        Value::Array(blocks) => blocks
            .iter()
            .filter(|b| b["type"] == "text")
            .filter_map(|b| b["text"].as_str())
            .collect::<Vec<_>>()
            .join("\n\n"),
        _ => String::new(),
    }
}

/// A pi message as a panel bubble: user messages (without the chat header) and each run's
/// final assistant answer. Tool steps, tool results, and other roles show nothing.
fn bubble(m: &Value) -> Option<(ChatRole, String, i64)> {
    let ts = m["timestamp"].as_i64().unwrap_or_else(now_ms);
    match m["role"].as_str()? {
        "user" => {
            let text = content_text(&m["content"]);
            let text = visible_user_text(&text).trim();
            (!text.is_empty()).then(|| (ChatRole::User, text.to_string(), ts))
        }
        "assistant" => {
            let text = content_text(&m["content"]).trim().to_string();
            let text = match m["stopReason"].as_str() {
                Some("toolUse") => return None,
                Some("error") => format!(
                    "⚠ {}",
                    m["errorMessage"]
                        .as_str()
                        .unwrap_or("The assistant stopped with an error.")
                ),
                Some("aborted") if text.is_empty() => "⚠ Stopped.".to_string(),
                Some("aborted") => format!("{text}\n\n⚠ Stopped."),
                _ if text.is_empty() => return None,
                _ => text,
            };
            Some((ChatRole::Assistant, text, ts))
        }
        _ => None,
    }
}

/// Add a bubble unless the same message is already shown. A chat's first message is in the
/// snapshot the stream opens with and can also arrive as the run's own `message_start` right
/// after it; pi stamps both with one timestamp, so role, time and text name one message.
fn push_bubble(live: &mut Live, (role, text, ts): (ChatRole, String, i64)) {
    if live
        .messages
        .iter()
        .any(|m| m.role == role && m.ts == ts && m.text == text)
    {
        return;
    }
    let id = format!("a{}", live.messages.len());
    live.messages.push(ChatMessage { id, role, text, ts });
}

/// A full picture of the chat: on connect, and again when a stored chat comes back to life.
fn apply_snapshot(app: &App, id: &str, data: &str) {
    let Ok(v) = serde_json::from_str::<Value>(data) else {
        return;
    };
    // pi-web nests the list: `messages: { messages: [...] }`.
    let list = v["messages"]["messages"]
        .as_array()
        .or_else(|| v["messages"].as_array());
    let busy = v["state"]["isStreaming"].as_bool().unwrap_or(false);
    let mut live = Live {
        busy,
        ..Default::default()
    };
    for m in list.into_iter().flatten() {
        if let Some(b) = bubble(m) {
            push_bubble(&mut live, b);
        }
    }
    app.chat.live.lock().unwrap().insert(id.to_string(), live);
    if busy {
        clear_sending(app, id);
        app.activity.mark(id, crate::clone_ops::now_ms());
    }
    broadcast(app, id);
}

/// One pi event. Only the few that change what the panel shows are acted on.
fn apply_event(app: &App, id: &str, data: &str) {
    let Ok(ev) = serde_json::from_str::<Value>(data) else {
        return;
    };
    let role = ev["message"]["role"].as_str();
    let busy = {
        let mut all = app.chat.live.lock().unwrap();
        let live = all.entry(id.to_string()).or_default();
        match ev["type"].as_str().unwrap_or("") {
            "agent_start" => live.busy = true,
            // `agent_settled` is pi's settle signal and `agent_end` the documented end of a
            // run: either one ends it. A background job's notice starts a new run later.
            "agent_end" | "agent_settled" => {
                live.busy = false;
                live.activity = None;
            }
            "tool_execution_start" => {
                let a = &ev["args"];
                let arg = ["command", "path", "file_path", "pattern"]
                    .iter()
                    .find_map(|k| a[*k].as_str().map(str::to_string))
                    .unwrap_or_else(|| a.to_string());
                let tool = ev["toolName"].as_str().unwrap_or("tool");
                live.activity = Some(clip_activity(&format!("⚙ {tool}: {arg}")));
            }
            // User messages land on start (a steered message has no later event of its own),
            // assistant messages on end, when their text is final.
            "message_start" if role == Some("user") => {
                if let Some(b) = bubble(&ev["message"]) {
                    push_bubble(live, b);
                }
            }
            "message_end" if role == Some("assistant") => match bubble(&ev["message"]) {
                Some(b) => push_bubble(live, b),
                None => {
                    let text = content_text(&ev["message"]["content"]);
                    if !text.trim().is_empty() {
                        live.activity = Some(clip_activity(&text));
                    }
                }
            },
            _ => return,
        }
        live.busy
    };
    clear_sending(app, id);
    if busy {
        app.activity.mark(id, crate::clone_ops::now_ms());
    }
    broadcast(app, id);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn msg(id: &str, at: i64) -> ScheduledMessage {
        ScheduledMessage {
            id: id.into(),
            text: "hi".into(),
            at,
            created_at: 0,
        }
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
        assert_eq!(
            expired.iter().map(|m| m.id.as_str()).collect::<Vec<_>>(),
            vec!["expired"]
        );
        // A message right on the grace boundary is still deliverable.
        let (due, expired) = due_messages(&[msg("edge", now - SCHEDULE_GRACE)], now);
        assert_eq!(due.len(), 1);
        assert!(expired.is_empty());
    }

    #[test]
    fn schedule_round_trips_through_disk_and_snapshot() {
        let app = App::test_app();
        let dd = app.data_dir();
        let now = now_ms();
        let a = schedule_message(&app, "c1", "later", now + 3_600_000).unwrap();
        let b = schedule_message(&app, "c1", "sooner", now + 60_000).unwrap();
        assert_ne!(a.id, b.id);

        // Reloaded from disk (not from memory) and ordered soonest-first.
        let loaded = load_schedules(&dd, "c1");
        assert_eq!(
            loaded.iter().map(|m| m.id.as_str()).collect::<Vec<_>>(),
            vec![&b.id, &a.id]
        );
        assert_eq!(loaded[0].text, "sooner");
        assert_eq!(loaded[1].at, now + 3_600_000);

        // ...and it reaches the SSE frame the frontend reads, camelCased.
        let snap: serde_json::Value = serde_json::from_str(&snapshot_json(&app, "c1")).unwrap();
        assert_eq!(snap["scheduled"].as_array().unwrap().len(), 2);
        assert_eq!(snap["scheduled"][0]["text"], "sooner");
        assert!(
            snap["scheduled"][0]["createdAt"].is_i64(),
            "createdAt must be camelCase"
        );

        assert!(cancel_schedule(&app, "c1", &b.id));
        assert!(
            !cancel_schedule(&app, "c1", &b.id),
            "second cancel is a no-op"
        );
        assert_eq!(
            load_schedules(&dd, "c1")
                .iter()
                .map(|m| m.id.clone())
                .collect::<Vec<_>>(),
            vec![a.id.clone()]
        );

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
        assert!(load_schedules(wire::DATA_DIR, "../evil").is_empty());
        assert!(schedule_path(wire::DATA_DIR, "../evil").is_none());
    }

    #[tokio::test]
    async fn tick_drops_schedules_for_unknown_clones_but_keeps_archived_ones() {
        let app = App::test_app();
        let dd = app.data_dir();
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
        assert!(
            load_schedules(&dd, "ghost").is_empty(),
            "unknown clone → dropped"
        );
        assert_eq!(
            load_schedules(&dd, "sleeping").len(),
            1,
            "archived clone → still queued"
        );
    }

    #[tokio::test]
    async fn expired_message_leaves_a_notice_in_the_transcript() {
        let app = App::test_app();
        let dd = app.data_dir();
        app.store.mutate(|s| {
            s.hosts.push(RmngClone {
                id: "wedged".into(),
                host: "wedged".into(),
                ..Default::default()
            });
        });
        schedule_message(&app, "wedged", "deploy the thing", now_ms() + 1_000).unwrap();
        // Push it well past the grace window, as a clone that was down all day would be.
        let mut list = load_schedules(&dd, "wedged");
        list[0].at = now_ms() - SCHEDULE_GRACE - 6 * 3_600_000;
        save_schedules(&dd, "wedged", &list);

        tick_schedules(&app);

        assert!(
            load_schedules(&dd, "wedged").is_empty(),
            "expired message is dropped"
        );
        // ...but the operator finds out where they'd actually look, with their text intact.
        let msgs = load_chat(&dd, "wedged").notices;
        let notice = msgs
            .last()
            .expect("an expiry notice must be written to the transcript");
        assert_eq!(notice.role, ChatRole::Assistant);
        assert!(
            notice.text.contains("deploy the thing"),
            "quotes the undelivered text: {}",
            notice.text
        );
        assert!(
            notice.text.contains("NOT sent"),
            "says plainly it did not go: {}",
            notice.text
        );
    }

    #[tokio::test]
    async fn tick_leaves_due_message_queued_while_the_clone_is_busy() {
        let app = App::test_app();
        let dd = app.data_dir();
        app.store.mutate(|s| {
            s.hosts.push(RmngClone {
                id: "worker".into(),
                host: "worker".into(),
                ..Default::default()
            });
        });
        schedule_message(&app, "worker", "queued", now_ms() + 1_000).unwrap();
        let mut list = load_schedules(&dd, "worker");
        list[0].at = now_ms() - 5_000;
        save_schedules(&dd, "worker", &list);

        app.chat
            .sending
            .lock()
            .unwrap()
            .insert("worker".into(), Instant::now());
        tick_schedules(&app);
        assert_eq!(
            load_schedules(&dd, "worker").len(),
            1,
            "busy clone must not lose the message"
        );
    }

    /// A headed clone's chat is told to use the desktop and nothing else, over whatever the
    /// playbook says; a headless one, which has no desktop, gets its shell instead.
    #[test]
    fn the_header_keeps_the_assistant_on_the_desktop() {
        let mut cfg = wire::AppConfig::default();
        cfg.assistant.server_url = "http://10.0.0.129:9000/".into();
        let mut host = RmngClone {
            id: "pega-we-142".into(),
            display_name: Some("Fix login".into()),
            linear_ticket_url: Some("https://linear.app/x/issue/WE-142".into()),
            ..Default::default()
        };
        let h = chat_header(&cfg, &host).unwrap();
        assert!(h.starts_with(HEADER_MARK));
        assert!(h.contains("Clone: pega-we-142 (Fix login)"), "{h}");
        assert!(
            h.contains("`rmng --server http://10.0.0.129:9000 desktop pega-we-142 click X Y`"),
            "{h}"
        );
        assert!(h.contains("take precedence over the playbook"), "{h}");
        assert!(h.contains("no `rmng clone exec`, `rmng ledger`"), "{h}");
        assert!(
            h.contains("Never crop, zoom, or measure a screenshot"),
            "{h}"
        );
        assert!(
            h.contains("Ticket: https://linear.app/x/issue/WE-142"),
            "{h}"
        );

        host.headless = true;
        let h = chat_header(&cfg, &host).unwrap();
        assert!(h.contains("clone exec pega-we-142 -- <command>"), "{h}");
        assert!(!h.contains("desktop pega-we-142 click"), "{h}");

        cfg.assistant.server_url = " ".into();
        assert!(chat_header(&cfg, &host).is_err());
    }

    /// The first message shows once, though it arrives in the snapshot and as an event.
    #[test]
    fn a_message_in_the_snapshot_and_an_event_shows_once() {
        let mut live = Live::default();
        push_bubble(&mut live, (ChatRole::User, "go".into(), 7));
        push_bubble(&mut live, (ChatRole::User, "go".into(), 7));
        assert_eq!(live.messages.len(), 1);
        // The same words sent again later are a new message.
        push_bubble(&mut live, (ChatRole::User, "go".into(), 9));
        assert_eq!(live.messages.len(), 2);
    }

    #[test]
    fn the_panel_hides_the_chat_header() {
        let first = format!("{HEADER_MARK} header\nClone: c1\n\nPlaybook{MESSAGE_MARK}fix the bug");
        assert_eq!(visible_user_text(&first), "fix the bug");
        assert_eq!(visible_user_text("plain"), "plain");
        // Only a message that opens with the header is cut.
        let quoted = format!("see{MESSAGE_MARK}this");
        assert_eq!(visible_user_text(&quoted), quoted);
    }

    #[test]
    fn bubbles_keep_user_messages_and_final_answers_only() {
        let user = json!({"role":"user","timestamp":5,"content":[{"type":"text","text":"hi"}]});
        assert_eq!(bubble(&user), Some((ChatRole::User, "hi".into(), 5)));
        let step = json!({"role":"assistant","stopReason":"toolUse","content":[{"type":"text","text":"looking"},{"type":"toolCall","id":"t","name":"bash"}]});
        assert_eq!(bubble(&step), None);
        let answer = json!({"role":"assistant","stopReason":"stop","timestamp":9,"content":[{"type":"thinking","thinking":"x"},{"type":"text","text":"done"}]});
        assert_eq!(
            bubble(&answer),
            Some((ChatRole::Assistant, "done".into(), 9))
        );
        let failed = json!({"role":"assistant","stopReason":"error","timestamp":1,"errorMessage":"quota","content":[]});
        assert_eq!(
            bubble(&failed),
            Some((ChatRole::Assistant, "⚠ quota".into(), 1))
        );
        assert_eq!(bubble(&json!({"role":"toolResult","content":"x"})), None);
    }

    #[test]
    fn a_snapshot_then_events_build_the_thread() {
        let app = App::test_app();
        let snap = json!({
            "state": {"isStreaming": true},
            "messages": {"messages": [
                {"role":"user","timestamp":1,"content":format!("{HEADER_MARK} h{MESSAGE_MARK}go")},
                {"role":"assistant","stopReason":"toolUse","timestamp":2,"content":[{"type":"toolCall","id":"t","name":"bash"}]},
            ]}
        });
        apply_snapshot(&app, "c1", &snap.to_string());
        assert!(is_busy(&app, "c1"));
        apply_event(&app, "c1", &json!({"type":"tool_execution_start","toolName":"bash","args":{"command":"rmng desktop c1 screenshot"}}).to_string());
        let v: Value = serde_json::from_str(&snapshot_json(&app, "c1")).unwrap();
        assert_eq!(v["activity"], "⚙ bash: rmng desktop c1 screenshot");
        apply_event(&app, "c1", &json!({"type":"message_end","message":{"role":"assistant","stopReason":"stop","timestamp":3,"content":[{"type":"text","text":"it is open"}]}}).to_string());
        apply_event(&app, "c1", &json!({"type":"agent_end"}).to_string());
        let v: Value = serde_json::from_str(&snapshot_json(&app, "c1")).unwrap();
        assert_eq!(v["busy"], false);
        assert!(v.get("activity").is_none());
        let texts: Vec<&str> = v["messages"]
            .as_array()
            .unwrap()
            .iter()
            .map(|m| m["text"].as_str().unwrap())
            .collect();
        assert_eq!(texts, vec!["go", "it is open"]);
    }

    #[test]
    fn sse_frames_parse_event_and_data() {
        assert_eq!(
            parse_frame(b"event: pi\ndata: {\"type\":\"agent_start\"}"),
            Some(("pi".into(), "{\"type\":\"agent_start\"}".into()))
        );
        assert_eq!(parse_frame(b": ping"), None);
    }
}
