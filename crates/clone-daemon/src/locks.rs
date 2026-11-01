//! The clone's Caps Lock kept in step with the operator's keyboard.
//!
//! The browser cannot know the clone's lock, and forwarding the Caps Lock key as a press only
//! flips it: once the two sides disagree (the key went to another window, another device or
//! the agent typed, a keyup was lost) every later press keeps them apart. So the browser sends
//! its own lock state instead ([`wire::socket::LockState`]), and this reads the clone's from
//! gnome-shell (`org.gnome.Shell.Eval`, the keymap gnome-shell's own Caps Lock warning reads)
//! and taps Caps Lock when the two differ.
//!
//! A tap goes to Mutter without a reply and lands on its input thread a moment later, so a read
//! right after one can still see the old state. For [`SETTLE`] after a tap the state it made is
//! taken as the clone's, and the clone is not read.

use std::time::{Duration, Instant};

use wire::socket::InputMsg;

/// How long a tap's state is trusted before the clone is read again.
const SETTLE: Duration = Duration::from_millis(300);
/// The longest a read may take; past it the lock is left as it is.
const READ_TIMEOUT: Duration = Duration::from_millis(250);
/// evdev `KEY_CAPSLOCK`.
const KEY_CAPSLOCK: u32 = 58;

/// The keymap gnome-shell's own Caps Lock warning reads, reached through `global` (the one name
/// `Eval`'s scope is sure to have, as in `crate::windows`): the backend's seat (GNOME 45+), or
/// the stage's Clutter backend's on older shells.
const JS_CAPS: &str = "(() => { const b = global.backend; \
     const seat = b && b.get_default_seat ? b.get_default_seat() \
       : global.stage.get_context().get_backend().get_default_seat(); \
     return seat.get_keymap().get_caps_lock_state(); })()";

pub struct CapsSync {
    rt: tokio::runtime::Handle,
    conn: Option<zbus::Connection>,
    /// The state the last tap made, and when.
    tapped: Option<(bool, Instant)>,
}

impl CapsSync {
    pub fn new(rt: tokio::runtime::Handle) -> Self {
        Self {
            rt,
            conn: None,
            tapped: None,
        }
    }

    /// The input that brings the clone's Caps Lock to `want`: nothing, or one tap. Blocks for
    /// the read (a D-Bus call to gnome-shell, at most [`READ_TIMEOUT`]) so the tap is queued
    /// before the keys that follow it.
    pub fn sync(&mut self, want: bool) -> Vec<InputMsg> {
        let now = Instant::now();
        let fresh = self
            .tapped
            .filter(|(_, at)| now.duration_since(*at) < SETTLE)
            .map(|(s, _)| s);
        let clone = fresh.or_else(|| self.read());
        if !needs_tap(clone, want) {
            return Vec::new();
        }
        self.tapped = Some((want, now));
        vec![
            InputMsg::KeyCode {
                keycode: KEY_CAPSLOCK,
                pressed: true,
            },
            InputMsg::KeyCode {
                keycode: KEY_CAPSLOCK,
                pressed: false,
            },
        ]
    }

    /// The clone's Caps Lock now; None when gnome-shell does not answer in time.
    fn read(&mut self) -> Option<bool> {
        let rt = self.rt.clone();
        let conn = match &self.conn {
            Some(c) => c.clone(),
            None => {
                let c = rt
                    .block_on(async {
                        tokio::time::timeout(READ_TIMEOUT, zbus::Connection::session()).await
                    })
                    .ok()?
                    .ok()?;
                self.conn = Some(c.clone());
                c
            }
        };
        let r = rt.block_on(async {
            tokio::time::timeout(READ_TIMEOUT, crate::windows::eval(&conn, JS_CAPS)).await
        });
        match r {
            Ok(Ok(s)) => parse(&s),
            Ok(Err(e)) => {
                tracing::warn!("reading the clone's Caps Lock: {e}");
                None
            }
            Err(_) => None,
        }
    }
}

/// Whether a tap is needed: the clone's lock is known and is not the one wanted. Unknown, it is
/// left alone rather than flipped blind.
fn needs_tap(clone: Option<bool>, want: bool) -> bool {
    clone.is_some_and(|c| c != want)
}

/// gnome-shell's `Eval` answer for a boolean.
fn parse(s: &str) -> Option<bool> {
    match s.trim() {
        "true" => Some(true),
        "false" => Some(false),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_tap_only_when_the_clone_is_known_to_differ() {
        assert!(needs_tap(Some(false), true));
        assert!(needs_tap(Some(true), false));
        assert!(!needs_tap(Some(true), true));
        assert!(!needs_tap(None, true));
    }

    /// Against a live GNOME session (a clone's own): the read answers. `--ignored` to run.
    #[test]
    #[ignore]
    fn reads_the_live_desktops_caps_lock() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let mut caps = CapsSync::new(rt.handle().clone());
        assert!(caps.read().is_some());
        // Wanting what it already has: no tap
        let now = caps.read().unwrap();
        assert!(caps.sync(now).is_empty());
    }

    #[test]
    fn evals_boolean_answer() {
        assert_eq!(parse("true"), Some(true));
        assert_eq!(parse(" false\n"), Some(false));
        assert_eq!(parse("undefined"), None);
    }
}
