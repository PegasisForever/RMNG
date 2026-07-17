//! Auto pointer-lock policy: decides WHEN the viewer should hold the Wayland
//! pointer lock, from the clone's cursor visibility (`CursorMeta.hidden` /
//! shape updates latched by the net thread).
//!
//! A cursor-grabbing app (a game world) hides the remote cursor and keeps it
//! hidden; ordinary UIs (including game menus) flap hidden↔shown as the sprite
//! changes. So the policy is debounced both ways: engage only after the cursor
//! has stayed hidden for [`ENGAGE_MS`], release after it has stayed visible for
//! [`RELEASE_MS`] (matching the old gtk-kasmvnc-client's empirically tuned
//! constants).
//!
//! The manual chords act as an override on top: Ctrl+Alt+G flips the effective
//! state, Ctrl+Alt+P forces release. An override clears itself as soon as the
//! debounced auto policy AGREES with it — so a manual release during a grab
//! re-arms once the game shows its cursor again (kasm-style), and a manual
//! engage before a grab hands control back to auto once the grab happens.
//!
//! Pure state machine (no GTK/Wayland): the net thread latches transitions,
//! the GTK tick polls [`AutoLock::want`] and reconciles the actual lock.

use std::time::{Duration, Instant};

/// Engage after the remote cursor stays hidden this long (menu flaps are shorter).
pub const ENGAGE_MS: u64 = 180;
/// Release after the remote cursor stays visible this long.
pub const RELEASE_MS: u64 = 300;

pub struct AutoLock {
    /// Latched remote-cursor visibility (from the last shape-bearing update).
    remote_hidden: bool,
    /// When `remote_hidden` last changed.
    since: Instant,
    /// Debounced automatic desire.
    auto_want: bool,
    /// Manual chord override; cleared when `auto_want` converges with it.
    overridden: Option<bool>,
}

impl AutoLock {
    pub fn new(now: Instant) -> Self {
        AutoLock { remote_hidden: false, since: now, auto_want: false, overridden: None }
    }

    /// Latch a remote cursor visibility transition (shape-bearing updates only;
    /// position-only updates carry no visibility information).
    pub fn on_remote_cursor(&mut self, hidden: bool, now: Instant) {
        if hidden != self.remote_hidden {
            self.remote_hidden = hidden;
            self.since = now;
        }
    }

    /// Manual Ctrl+Alt+G: flip the effective state; returns the new desire.
    pub fn toggle(&mut self, now: Instant) -> bool {
        let flipped = !self.want(now);
        self.overridden = Some(flipped);
        flipped
    }

    /// Manual Ctrl+Alt+P (panic / unstick): force release.
    pub fn force_release(&mut self) {
        self.overridden = Some(false);
    }

    /// Effective desire; poll from the tick. Advances the debounce clock.
    pub fn want(&mut self, now: Instant) -> bool {
        let dwell = now.saturating_duration_since(self.since);
        if self.remote_hidden {
            if dwell >= Duration::from_millis(ENGAGE_MS) {
                self.auto_want = true;
            }
        } else if dwell >= Duration::from_millis(RELEASE_MS) {
            self.auto_want = false;
        }
        // Auto has caught up with the manual override: hand control back, so a
        // manual release re-arms after the game un-grabs (and vice versa).
        if self.overridden == Some(self.auto_want) {
            self.overridden = None;
        }
        self.overridden.unwrap_or(self.auto_want)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `at(ms)` on a shared origin keeps the timelines readable.
    fn clock() -> impl Fn(u64) -> Instant {
        let t0 = Instant::now();
        move |ms| t0 + Duration::from_millis(ms)
    }

    #[test]
    fn engages_after_sustained_hide_and_releases_after_sustained_show() {
        let at = clock();
        let mut al = AutoLock::new(at(0));
        assert!(!al.want(at(0)));

        al.on_remote_cursor(true, at(100));
        assert!(!al.want(at(150)), "hidden only 50ms — below ENGAGE_MS");
        assert!(al.want(at(100 + ENGAGE_MS)), "sustained hide engages");

        al.on_remote_cursor(false, at(1000));
        assert!(al.want(at(1100)), "shown only 100ms — below RELEASE_MS");
        assert!(!al.want(at(1000 + RELEASE_MS)), "sustained show releases");
    }

    #[test]
    fn menu_flapping_never_engages_and_world_flapping_never_releases() {
        let at = clock();
        let mut al = AutoLock::new(at(0));
        // Menus: hidden↔shown alternation faster than ENGAGE_MS never engages.
        for i in 0..20u64 {
            al.on_remote_cursor(i % 2 == 0, at(i * 100));
            assert!(!al.want(at(i * 100 + 50)));
        }
        // In-world: engaged, then brief shows (sprite churn) never release.
        al.on_remote_cursor(true, at(10_000));
        assert!(al.want(at(10_000 + ENGAGE_MS)));
        for i in 0..20u64 {
            let t = 12_000 + i * 100;
            al.on_remote_cursor(i % 2 == 0, at(t));
            assert!(al.want(at(t + 50)));
        }
    }

    #[test]
    fn manual_release_during_grab_rearms_after_cursor_shows() {
        let at = clock();
        let mut al = AutoLock::new(at(0));
        al.on_remote_cursor(true, at(0));
        assert!(al.want(at(ENGAGE_MS)));

        // Ctrl+Alt+G mid-grab: released even though the game still hides the cursor.
        assert!(!al.toggle(at(500)));
        assert!(!al.want(at(5000)), "override holds while the grab persists");

        // Game shows the cursor (menu): auto converges to released → override clears.
        al.on_remote_cursor(false, at(6000));
        assert!(!al.want(at(6000 + RELEASE_MS)));

        // Next grab re-locks automatically.
        al.on_remote_cursor(true, at(8000));
        assert!(al.want(at(8000 + ENGAGE_MS)), "re-armed after show; auto re-engages");
    }

    #[test]
    fn manual_engage_hands_back_to_auto_once_grab_confirms() {
        let at = clock();
        let mut al = AutoLock::new(at(0));

        // Ctrl+Alt+G with a visible remote cursor: engage immediately.
        assert!(al.toggle(at(100)));
        assert!(al.want(at(200)));

        // The game then grabs (auto agrees) → override clears silently…
        al.on_remote_cursor(true, at(1000));
        assert!(al.want(at(1000 + ENGAGE_MS)));
        // …so a later un-grab releases automatically.
        al.on_remote_cursor(false, at(5000));
        assert!(!al.want(at(5000 + RELEASE_MS)));
    }

    #[test]
    fn force_release_is_an_override() {
        let at = clock();
        let mut al = AutoLock::new(at(0));
        al.on_remote_cursor(true, at(0));
        assert!(al.want(at(ENGAGE_MS)));
        al.force_release();
        assert!(!al.want(at(ENGAGE_MS + 100)));
        // Show + re-hide re-arms exactly like the toggle case.
        al.on_remote_cursor(false, at(2000));
        let _ = al.want(at(2000 + RELEASE_MS));
        al.on_remote_cursor(true, at(4000));
        assert!(al.want(at(4000 + ENGAGE_MS)));
    }

    #[test]
    fn position_only_updates_do_not_reset_the_dwell() {
        let at = clock();
        let mut al = AutoLock::new(at(0));
        al.on_remote_cursor(true, at(0));
        // Re-latching the same state (as repeated hide events would) keeps `since`.
        al.on_remote_cursor(true, at(100));
        assert!(al.want(at(ENGAGE_MS)), "second hide event must not restart the clock");
    }
}
