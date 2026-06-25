//! Per-host chat with the in-container agent-wrapper. Ports `agent.server.ts` +
//! `chats.server.ts` + `chatbus.server.ts`.
//!
//! Each host has its own conversation (`data/chats/<id>.json`) and its own SSE
//! fan-out (keyed by host id — message bodies never touch the global `/events`
//! frame). A turn runs **detached** from the POST request (it can take minutes; a
//! browser refresh must not kill it). The server owns the "busy" flag so the
//! working indicator + the eventual reply survive a reconnect. Watchdogs abort a
//! stalled (no activity for 3m) or over-long (30m) turn.

use std::collections::{HashMap, HashSet};
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use futures::StreamExt;
use serde::{Deserialize, Serialize};
use wire::{Chat, ChatMessage, ChatRole, Host};

use crate::app::App;
use crate::files::is_safe_id;

const IDLE_TIMEOUT: Duration = Duration::from_secs(3 * 60);
const MAX_TURN: Duration = Duration::from_secs(30 * 60);
const ACTIVITY_MAX: usize = 200;

/// Per-host chat fan-out + in-flight state.
#[derive(Default)]
pub struct ChatState {
    senders: Mutex<HashMap<String, tokio::sync::broadcast::Sender<String>>>,
    busy: Mutex<HashSet<String>>,
    activity: Mutex<HashMap<String, String>>,
    listeners: Mutex<HashSet<String>>,
}

fn now_ms() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0)
}

fn short_id() -> String {
    let t = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
    format!("{:08x}", (t as u64) & 0xFFFF_FFFF)
}

fn base_url(app: &App, host: &Host) -> String {
    format!("http://{}:{}", host.host, app.config().agent_port)
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

// --- chat bus --------------------------------------------------------------

#[derive(Serialize)]
struct ChatSnapshot {
    busy: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    activity: Option<String>,
    messages: Vec<ChatMessage>,
}

fn snapshot_json(app: &App, host_id: &str) -> String {
    let snap = ChatSnapshot {
        busy: app.chat.busy.lock().unwrap().contains(host_id),
        activity: app.chat.activity.lock().unwrap().get(host_id).cloned(),
        messages: load_chat(&app.config().data_dir, host_id).messages,
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
    /// false ⇒ autonomous (monitoring) message, not the answer to a /prompt.
    #[serde(default)]
    solicited: Option<bool>,
    #[serde(default)]
    error: Option<String>,
}

async fn post_abort(app: &App, base: &str) {
    let _ = app.http.post(format!("{base}/abort")).timeout(Duration::from_secs(5)).send().await;
}

/// Persist the user message, kick off the turn detached, return the new chat.
pub fn send_chat(app: &App, host: &Host, text: &str) -> Result<(), String> {
    let text = text.trim();
    if text.is_empty() {
        return Err("empty message".into());
    }
    if is_busy(app, &host.id) {
        return Err("a message is already being processed for this host".into());
    }
    push_message(app, &host.id, ChatRole::User, text.to_string());
    set_busy(app, &host.id, true);
    let (app2, host2, text2) = (app.clone(), host.clone(), text.to_string());
    tokio::spawn(async move { run_turn(app2, host2, text2).await });
    Ok(())
}

async fn run_turn(app: App, host: Host, text: String) {
    let base = base_url(&app, &host);
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

/// Interrupt the host's in-flight turn (best-effort).
pub async fn abort_chat(app: &App, host: &Host) {
    post_abort(app, &base_url(app, host)).await;
}

// --- kickoff (post-clone first message) ------------------------------------

#[derive(Default)]
pub struct KickoffOpts {
    pub ticket_url: Option<String>,
    pub message: Option<String>,
    pub agent_instructions: Option<String>,
    pub claude_instructions: Option<String>,
}

/// After a clone, wait for the wrapper to come up + be idle, then send the kickoff
/// message (ticket URL or plain first message + optional instruction overrides).
pub async fn kickoff_agent(app: App, host: Host, opts: KickoffOpts) {
    let mut msg = opts.ticket_url.clone().or(opts.message.clone()).unwrap_or_default().trim().to_string();
    if msg.is_empty() {
        return;
    }
    let deadline = Instant::now() + Duration::from_secs(90);
    while Instant::now() < deadline {
        // probe /status; break when idle
        let url = format!("{}/status", base_url(&app, &host));
        let idle = async {
            let r = app.http.get(&url).timeout(Duration::from_secs(4)).send().await.ok()?;
            let v: serde_json::Value = r.json().await.ok()?;
            Some(v.get("busy").and_then(|b| b.as_bool()) == Some(false))
        }
        .await;
        if idle == Some(true) {
            break;
        }
        tokio::time::sleep(Duration::from_secs(4)).await;
    }
    if let Some(a) = opts.agent_instructions.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        msg += &format!(
            "\n\nAdditional host-agent instructions (these take precedence — merge them with your procedure):\n{a}"
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

// --- autonomous (monitoring) message listener ------------------------------

/// Idempotent: keep one persistent `/events` subscription per host that persists
/// UNSOLICITED assistant messages into the chat (the monitor poller calls this for
/// reachable hosts; a dropped listener is restarted on the next tick).
pub fn ensure_autonomous_listener(app: &App, host: &Host) {
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

async fn run_autonomous_listener(app: &App, host: &Host) -> Result<(), ()> {
    let base = base_url(app, host);
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
