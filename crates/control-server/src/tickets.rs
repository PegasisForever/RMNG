//! The Linear ticket poller behind the board's ticket column.
//!
//! The server holds the keys, so the server does the asking: every preset's Linear key is
//! queried for its own owner's open issues, the answers are merged, and the result lands in
//! `ControlState.tickets`. From there it rides the same `/events` stream as everything else,
//! which is what makes a ticket somebody moves in Linear appear or disappear in the browser
//! on its own. The browser never sees a key and has no refresh button to press.
//!
//! One poll per interval for the whole fleet, not one per clone: the query is scoped to the
//! key's viewer, and the answer is the same whoever is looking at it.

use std::collections::HashSet;
use std::time::Duration;

use crate::app::App;
use crate::linear;

/// How often to ask Linear. Slow enough to be invisible against Linear's rate limits
/// (1500 requests/hour per key), fast enough that a ticket someone closes is gone before you
/// wonder why it is still there.
const POLL_INTERVAL: Duration = Duration::from_secs(60);

/// How long to wait after a failure. Linear being down is not urgent — the column keeps
/// drawing the last good list — so this backs off rather than hammering.
const ERROR_INTERVAL: Duration = Duration::from_secs(5 * 60);

/// Every distinct Linear key on the presets, in config order.
///
/// Deduplicated because two presets sharing one key would otherwise return the same person's
/// issues twice, and the merge below would have to undo it.
fn keys(app: &App) -> Vec<String> {
    let mut seen = HashSet::new();
    app.config()
        .presets
        .iter()
        .map(|p| p.linear_key.trim().to_string())
        .filter(|k| !k.is_empty() && seen.insert(k.clone()))
        .collect()
}

/// Poll every key once and publish the union.
///
/// Returns whether anything failed. A key that errors does not sink the poll: the others'
/// tickets are still published, and the error rides alongside them, because a fleet with one
/// stale key should still see the rest of its work.
pub async fn poll_once(app: &App) -> bool {
    let keys = keys(app);
    if keys.is_empty() {
        publish(app, Vec::new(), None);
        return false;
    }

    let mut tickets: Vec<wire::LinearTicket> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    let mut errors: Vec<String> = Vec::new();

    for key in &keys {
        match linear::fetch_open_assigned(&app.http, key).await {
            Ok(found) => {
                // First key to report an issue owns its position. Two people assigned to the
                // same issue is unusual but not impossible, and the column should draw it
                // once either way.
                for ticket in found {
                    if seen.insert(ticket.id.to_ascii_lowercase()) {
                        tickets.push(ticket);
                    }
                }
            }
            Err(e) => errors.push(e.to_string()),
        }
    }

    // Every key failed and nothing came back: keep whatever the column already has rather
    // than blanking it, and say why. A partial answer still replaces the list, because the
    // keys that did answer are authoritative for their own owners.
    if tickets.is_empty() && !errors.is_empty() {
        let message = errors.join("; ");
        app.store.mutate(|s| s.tickets_error = Some(message));
        return true;
    }

    let failed = !errors.is_empty();
    publish(app, tickets, (!errors.is_empty()).then(|| errors.join("; ")));
    failed
}

/// Write the list, and only when it changed. Every `mutate` broadcasts the whole state and
/// rewrites `state.json`, so a poll that found nothing new must be silent or the fleet gets
/// a full-state frame every minute for no reason.
fn publish(app: &App, tickets: Vec<wire::LinearTicket>, error: Option<String>) {
    let current = app.store.get();
    if current.tickets == tickets && current.tickets_error == error {
        return;
    }
    app.store.mutate(|s| {
        s.tickets = tickets;
        s.tickets_error = error;
    });
}

pub async fn run_poller(app: App) {
    loop {
        let failed = poll_once(&app).await;
        tokio::time::sleep(if failed { ERROR_INTERVAL } else { POLL_INTERVAL }).await;
    }
}
