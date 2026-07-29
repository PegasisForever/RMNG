# macOS Parity After The Merge — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Close the macOS parity gaps the 90-commit Linux merge opened, so the Mac viewer cannot freeze the host cursor, forwards keys the moment it connects, and speaks Mac keyboard conventions.

**Architecture:** Six independent changes in `crates/viewer`. Each extracts the *decision* into a pure, unit-testable function and leaves the GTK/AppKit wiring as a thin caller — the pattern `auto_lock.rs` and `kvk_evdev.rs` already use in this crate, and the only way to test any of this without a display.

**Tech Stack:** Rust (edition 2024), GTK4 0.9 (`gtk4-rs`), pango, GStreamer 0.23, `objc2` / `objc2-app-kit` / `core-graphics` for the macOS layer. Tests are plain `#[cfg(test)] mod tests` inside the binary crate's modules — `cargo test -p viewer` runs them (30 pass at baseline).

## Global Constraints

- **The wire protocol is frozen:** framing, message tags, evdev codes on the wire, AVC444 packing. Nothing in this plan changes bytes on the wire except which evdev *modifier* code a Mac Cmd press produces (Task 4).
- **No server-side change.** Nothing here touches `control-server`, `clone-daemon`, or `media`.
- **Linux behaviour must not regress.** Only Task 1 and Task 2 touch shared (non-cfg-gated) code paths; both align explicit release with what mutter already does implicitly. Task 3's handler-disconnect is shared and fixes a leak on both platforms. Verify with `cargo check -p viewer` and the full test suite after each task.
- **Preserve these invariants in `glunpack.rs`:** the `WRITE|GL` output map and the VAO bind. Task 7 touches only a doc comment there.
- **Baseline to beat:** `cargo test -p viewer` = 30 passed, 0 failed. Every task ends with that number going up and nothing failing.
- **`--glunpack-validate 256 144` must still report max abs err 0** after any task.
- **Commit style:** Conventional Commits, `fix(viewer/macos):` / `feat(viewer/macos):` scope as used by the existing macOS commits (`bc57e32`, `4561a97`). End every commit message body with:
  ```
  Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>
  Claude-Session: https://claude.ai/code/session_0136CAyyXeBQLymr6UZSC2Mh
  ```

## File Structure

| File | Responsibility | Tasks |
| --- | --- | --- |
| `crates/viewer/src/auto_lock.rs` | Pure pointer-lock policy. Gains `LockAction` + `lock_action()` — the "should the lock be held right now" decision, kept next to the existing debounce state machine. | 1 |
| `crates/viewer/src/main.rs` | GTK wiring. Tick calls `lock_action`; `teardown_content` releases the lock and disconnects the focus handler; `install_keyboard` primes the macOS gate and returns its `SignalHandlerId`; `VideoContent` stores it. | 1, 2, 3, 4, 7 |
| `crates/viewer/src/keyboard_macos.rs` | macOS NSEvent keyboard monitor. Gains the Cmd↔Ctrl swap as a pure function applied at the single translate choke point. | 3, 4 |
| `crates/viewer/src/kvk_evdev.rs` | Pure kVK→evdev table. Two entries corrected. | 5 |
| `crates/viewer/src/config.rs` | Persisted config. Gains `cmd_is_ctrl` (serde-defaulted so existing config files still load) and the env-override parser. | 4 |
| `crates/viewer/src/terminal.rs` | Terminal view. `pick_font` becomes a pure, injectable decision; `load_font` supplies the real font-map predicate. | 6 |
| `docs/DEVELOPMENT.md`, `crates/viewer/README.md`, `run-macos-test.sh` | macOS build/run/input documentation and the operator test checklist. | 7 |

---

### Task 1: Make "no engage target" a release condition

The bug: `main.rs` reconciles the pointer lock with `if want { if let Some(active_video_window) { engage } } else if engaged { release }`. When `want` is true but no video window is active, **both** branches are skipped and the lock stays held. On Wayland mutter deactivates a constraint on focus loss, so this is invisible. On macOS `CGAssociateMouseAndMouseCursorPosition(false)` is process-global, so the host cursor stays frozen in whatever app the operator switched to.

**Files:**
- Modify: `crates/viewer/src/auto_lock.rs` (add `LockAction` + `lock_action`, and tests to the existing `mod tests`)
- Modify: `crates/viewer/src/main.rs:709-725` (the tick's step 3)
- Test: `crates/viewer/src/auto_lock.rs` `#[cfg(test)] mod tests`

**Interfaces:**
- Consumes: nothing from other tasks.
- Produces: `pub enum LockAction { Engage, Release, Nothing }` and
  `pub fn lock_action(want: bool, has_target: bool, engaged: bool) -> LockAction` in `auto_lock`.
  Task 2 relies on `LockAction` existing but does not call it.

- [ ] **Step 1: Write the failing tests**

Append to the existing `mod tests` in `crates/viewer/src/auto_lock.rs`:

```rust
    /// The merge's regression: auto-lock wants the lock, the operator alt-tabbed away, so there
    /// is no active video window to target. The old reconciler skipped both branches and left
    /// the lock held — on macOS that freezes the host cursor process-wide.
    #[test]
    fn no_target_while_engaged_releases() {
        assert_eq!(lock_action(true, false, true), LockAction::Release);
    }

    #[test]
    fn no_target_and_not_engaged_does_nothing() {
        assert_eq!(lock_action(true, false, false), LockAction::Nothing);
    }

    #[test]
    fn wanted_with_a_target_engages_and_is_idempotent() {
        assert_eq!(lock_action(true, true, false), LockAction::Engage);
        // Re-engaging is how the Wayland twin re-targets after focus moves windows; the
        // implementation is idempotent per surface, so ask for it either way.
        assert_eq!(lock_action(true, true, true), LockAction::Engage);
    }

    #[test]
    fn unwanted_releases_only_when_engaged() {
        assert_eq!(lock_action(false, true, true), LockAction::Release);
        assert_eq!(lock_action(false, false, true), LockAction::Release);
        assert_eq!(lock_action(false, true, false), LockAction::Nothing);
        assert_eq!(lock_action(false, false, false), LockAction::Nothing);
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p viewer auto_lock`
Expected: FAIL to *compile* — `cannot find function 'lock_action' in this scope` and `cannot find type 'LockAction'`. A compile failure is the correct "red" here.

- [ ] **Step 3: Add the pure decision function**

Insert into `crates/viewer/src/auto_lock.rs`, after the `RELEASE_MS` const and before `pub struct AutoLock`:

```rust
/// What the GTK tick should do with the actual pointer lock this frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LockAction {
    Engage,
    Release,
    Nothing,
}

/// Reconcile the debounced desire ([`AutoLock::want`]) with reality.
///
/// `has_target` is "some video window is currently the active window" — the surface the lock
/// would be attached to. **Wanting the lock with no target is a RELEASE condition, not a
/// no-op:** on Wayland mutter deactivates a pointer constraint when its surface loses focus, but
/// `CGAssociateMouseAndMouseCursorPosition(false)` on macOS is process-global and outlives our
/// focus, so leaving it held freezes the host cursor inside whatever app the operator switched
/// to. Releasing on blur and re-engaging on focus reproduces the compositor's behaviour on both
/// platforms.
pub fn lock_action(want: bool, has_target: bool, engaged: bool) -> LockAction {
    match (want, has_target, engaged) {
        // Idempotent per surface in both backends; also re-targets if focus moved windows.
        (true, true, _) => LockAction::Engage,
        (_, _, true) => LockAction::Release,
        _ => LockAction::Nothing,
    }
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p viewer auto_lock`
Expected: PASS — 6 tests in `auto_lock` (the 6 pre-existing debounce tests) plus the 4 new ones.

- [ ] **Step 5: Wire the tick to use it**

In `crates/viewer/src/main.rs`, replace the whole of step 3 (currently lines 706-725):

```rust
            // 3. Auto pointer-lock: reconcile the actual lock with the policy (remote cursor hidden
            //    ≥180ms → engage; shown ≥300ms → release; manual chords override — see auto_lock.rs).
            //    Engage targets the active video window; with none active we leave the state alone.
            if let Some(pl) = pointer_lock.as_ref() {
                let want = auto.lock().unwrap().want(Instant::now());
                if want {
                    if let Some(mw) = windows
                        .borrow()
                        .values()
                        .find(|w| matches!(w.content, Content::Video(_)) && w.window.is_active())
                    {
                        // Idempotent per surface; re-targets if focus moved windows.
                        if let Some(surface) = mw.window.surface() {
                            pl.engage(&surface);
                        }
                    }
                } else if pl.is_engaged() {
                    pl.release();
                }
            }
```

with:

```rust
            // 3. Auto pointer-lock: reconcile the actual lock with the policy (remote cursor hidden
            //    ≥180ms → engage; shown ≥300ms → release; manual chords override — see auto_lock.rs).
            //    The target is the active video window's surface; NO target releases (see
            //    auto_lock::lock_action — holding a macOS lock through a focus loss freezes the
            //    host cursor process-wide, which mutter prevents for us on Wayland).
            if let Some(pl) = pointer_lock.as_ref() {
                let want = auto.lock().unwrap().want(Instant::now());
                let target = windows
                    .borrow()
                    .values()
                    .find(|w| matches!(w.content, Content::Video(_)) && w.window.is_active())
                    .and_then(|mw| mw.window.surface());
                match auto_lock::lock_action(want, target.is_some(), pl.is_engaged()) {
                    auto_lock::LockAction::Engage => {
                        if let Some(surface) = target.as_ref() {
                            pl.engage(surface);
                        }
                    }
                    auto_lock::LockAction::Release => pl.release(),
                    auto_lock::LockAction::Nothing => {}
                }
            }
```

Note: `target` must be computed into a local *before* the `match`, so the `windows.borrow()` guard is dropped before `pl.engage`/`pl.release` run. Holding that `RefCell` borrow across the engage call risks a re-entrant borrow panic if a GTK signal fires during it.

- [ ] **Step 6: Verify the build and the whole suite**

Run: `cargo test -p viewer 2>&1 | tail -5`
Expected: `test result: ok. 34 passed; 0 failed`

Run: `cargo check -p viewer --message-format=short 2>&1 | tail -5`
Expected: no errors (the pre-existing `unused variable: fps_count` warning is fine).

- [ ] **Step 7: Commit**

```bash
git add crates/viewer/src/auto_lock.rs crates/viewer/src/main.rs
git commit -F - <<'EOF'
fix(viewer/macos): release the pointer lock when no video window is focused

The tick reconciled the lock with `if want { if let Some(active) { engage } }
else if engaged { release }`, so wanting the lock with no active video window
skipped both branches and left it held. On Wayland mutter deactivates a pointer
constraint when its surface loses focus, which hid this. On macOS
CGAssociateMouseAndMouseCursorPosition(false) is process-global and outlives our
focus, so alt-tabbing out of a cursor-hiding remote app froze the host cursor
inside another app — and auto_lock.rs now engages unattended, so it is no longer
opt-in.

Extract the decision as auto_lock::lock_action(want, has_target, engaged) and
make "no target" a release condition. Behaviour-neutral on Linux: it makes
explicit what the compositor already does.

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_0136CAyyXeBQLymr6UZSC2Mh
EOF
```

---

### Task 2: Release the lock when a window stops showing video

Task 1 covers focus. This covers *lifetime*: `reconcile_view` destroys windows (`main.rs:875-880`) and swaps content while the lock is held. A `ViewSpec{monitors:[]}` destroys every window, after which no key controller exists to run the Ctrl+Alt+P escape.

**Files:**
- Modify: `crates/viewer/src/main.rs` — `teardown_content` signature and body; its three call sites in `reconcile_view`

**Interfaces:**
- Consumes: nothing from Task 1 (independent; ordered second only because it touches the same function Task 3 does).
- Produces: `fn teardown_content(mw: &mut MonitorWindow, srcs: &VideoSrcs, pointer_lock: &Option<Rc<PointerLock>>)` — Task 3 adds a fourth behaviour to this same function, so apply Task 2 first.

- [ ] **Step 1: Change the signature and release inside it**

In `crates/viewer/src/main.rs`, replace the `teardown_content` doc comment and signature (currently starting at line 953) — the current text is:

```rust
/// Detach a window's current content so it can host new content: stop a video pipeline, remove its
/// window-level keyboard controller, and drop its appsrc. (The pointer controllers live on the
/// video widget and drop when it is unparented by the next `set_child`.) Leaves the window in a
/// neutral `Placeholder` state; the caller sets the real content next.
fn teardown_content(mw: &mut MonitorWindow, srcs: &VideoSrcs) {
    if let Content::Video(vc) = &mw.content {
        mw.window.remove_controller(&vc.keyboard);
        let _ = vc.pipeline.set_state(gst::State::Null);
        srcs.lock().unwrap().remove(&mw.id);
    }
```

with:

```rust
/// Detach a window's current content so it can host new content: stop a video pipeline, remove its
/// window-level keyboard controller, release any pointer lock it held, and drop its appsrc. (The
/// pointer controllers live on the video widget and drop when it is unparented by the next
/// `set_child`.) Leaves the window in a neutral `Placeholder` state; the caller sets the real
/// content next.
///
/// Releasing the lock here is what makes window teardown safe on macOS: only a video window can
/// hold it, the lock is a single process-wide resource, and once this window is gone (or showing a
/// terminal) there is no key controller left to run the Ctrl+Alt+P escape. The tick re-engages
/// within one frame if the policy still wants it and a video window is focused.
fn teardown_content(mw: &mut MonitorWindow, srcs: &VideoSrcs, pointer_lock: &Option<Rc<PointerLock>>) {
    if let Content::Video(vc) = &mw.content {
        mw.window.remove_controller(&vc.keyboard);
        if let Some(pl) = pointer_lock.as_ref() {
            pl.release(); // idempotent when not engaged
        }
        let _ = vc.pipeline.set_state(gst::State::Null);
        srcs.lock().unwrap().remove(&mw.id);
    }
```

- [ ] **Step 2: Update all four call sites**

`teardown_content` is called four times. Add `, pointer_lock` to each. In `reconcile_view` (`pointer_lock` is already a parameter, so it is in scope):

- in the `gone` destroy loop: `teardown_content(&mut mw, srcs, pointer_lock);`
- in the terminal-mode main-window branch: `teardown_content(mw, srcs, pointer_lock);`
- in the terminal-mode secondary-window branch: `teardown_content(mw, srcs, pointer_lock);`
- in the video branch: `teardown_content(mw, srcs, pointer_lock);`

Run `grep -n 'teardown_content' crates/viewer/src/main.rs` to confirm every site is updated.

- [ ] **Step 3: Verify the build and suite**

Run: `cargo test -p viewer 2>&1 | tail -5`
Expected: `test result: ok. 34 passed; 0 failed`

Run: `cargo check -p viewer --message-format=short 2>&1 | tail -5`
Expected: no errors.

Note there is no new unit test in this task: `teardown_content` is pure GTK plumbing with no extractable decision, and the behaviour it adds (`pl.release()` is idempotent) is asserted by Task 1's `unwanted_releases_only_when_engaged`. It is verified for real by operator checklist item 4 in Task 7.

- [ ] **Step 4: Commit**

```bash
git add crates/viewer/src/main.rs
git commit -F - <<'EOF'
fix(viewer/macos): release the pointer lock when a window leaves video mode

reconcile_view destroyed windows and swapped content with the lock still held.
ViewSpec{monitors:[]} destroys every window, after which no key controller
survives to run the Ctrl+Alt+P escape — so on macOS, where the CoreGraphics
association is process-global, deselecting a clone from the web UI while a
remote app hid its cursor could leave the host cursor frozen with no in-app way
out.

Release in teardown_content, which every destroy and every content swap already
funnels through. Idempotent when not engaged; the tick re-engages within a frame
if the policy still wants it.

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_0136CAyyXeBQLymr6UZSC2Mh
EOF
```

---

### Task 3: Prime and tear down the macOS keyboard gate

`keyboard_macos.rs` forwards keys only while `active_windows > 0`. Its only writer is the `is_active_notify` handler connected in `install_keyboard` (`main.rs:1852`). Two defects:

1. **Never primed.** The counter starts at 0 and only moves on a *transition*. A window whose content becomes video while it is already focused gets no notify, so keys keep going to GTK instead of the remote until a Cmd-Tab round trip.
2. **Handler pile-up.** `teardown_content` removes the key controller but never disconnects this handler, so each `make_video_content` adds another. After N swaps one focus change runs `release_all_input` N times and moves the counter by N.

**Files:**
- Modify: `crates/viewer/src/main.rs` — `VideoContent` struct, `install_keyboard` return type, `make_video_content`, `teardown_content`
- Modify: `crates/viewer/src/keyboard_macos.rs` — log the counter in `note_window_active`

**Interfaces:**
- Consumes: `teardown_content(mw, srcs, pointer_lock)` from Task 2.
- Produces: `install_keyboard(..) -> (gtk4::EventControllerKey, glib::SignalHandlerId)`; `VideoContent.active_notify: Option<glib::SignalHandlerId>`.

- [ ] **Step 1: Store the handler id on `VideoContent`**

In `crates/viewer/src/main.rs`, in `struct VideoContent`, after the `keyboard` field:

```rust
    /// The window's `is-active` handler, connected by `install_keyboard`. Disconnected when this
    /// window leaves video mode: the window shell outlives its content, so without this each
    /// content swap stacks another handler (N× `release_all_input` per focus change, and on macOS
    /// N× the keyboard-gate count). `Option` so `teardown_content` can take it by value —
    /// `GObject::disconnect` consumes the id.
    active_notify: Option<glib::SignalHandlerId>,
```

- [ ] **Step 2: Return the handler id from `install_keyboard`, and prime the gate**

Change the signature (currently `) -> gtk4::EventControllerKey {`) to:

```rust
) -> (gtk4::EventControllerKey, glib::SignalHandlerId) {
```

Then in the `connect_is_active_notify` block, capture the returned id and prime. Replace:

```rust
    {
        let (w, state, window2) = (writer.clone(), state.clone(), window.clone());
        window.connect_is_active_notify(move |win| {
```

with:

```rust
    let active_notify = {
        let (w, state, window2) = (writer.clone(), state.clone(), window.clone());
        window.connect_is_active_notify(move |win| {
```

and change the closing of that block, and the function's tail, from:

```rust
        });
    }
    key
}
```

to:

```rust
        })
    };

    // macOS: prime the keyboard gate. `is-active` only *notifies* on a transition, so a window
    // whose content becomes video while it is already the key window would never arm the monitor
    // — keys would stay local until the operator Cmd-Tabbed away and back. The matching
    // decrement is in teardown_content.
    #[cfg(target_os = "macos")]
    if window.is_active() {
        keyboard_macos::note_window_active(true);
    }

    (key, active_notify)
}
```

- [ ] **Step 3: Update `make_video_content`**

Change:

```rust
    let keyboard = install_keyboard(window, writer, &state, pointer_lock, auto);
```

to:

```rust
    let (keyboard, active_notify) = install_keyboard(window, writer, &state, pointer_lock, auto);
```

and add `active_notify: Some(active_notify),` to the `VideoContent { .. }` literal, after `keyboard,`.

- [ ] **Step 4: Disconnect and decrement in `teardown_content`**

Change the `if let` in `teardown_content` from `&mw.content` to `&mut mw.content` (disjoint field borrows: `mw.window` is a different field, so this compiles), and add the disconnect plus the matching decrement:

```rust
    if let Content::Video(vc) = &mut mw.content {
        mw.window.remove_controller(&vc.keyboard);
        if let Some(id) = vc.active_notify.take() {
            mw.window.disconnect(id);
        }
        // macOS: balance install_keyboard's priming. The handler is gone, so its own decrement
        // will never fire; if this window is the key window right now the gate would stay armed
        // for content that has no remote desktop.
        #[cfg(target_os = "macos")]
        if mw.window.is_active() {
            keyboard_macos::note_window_active(false);
        }
        if let Some(pl) = pointer_lock.as_ref() {
            pl.release(); // idempotent when not engaged
        }
        let _ = vc.pipeline.set_state(gst::State::Null);
        srcs.lock().unwrap().remove(&mw.id);
    }
```

- [ ] **Step 5: Log the counter so the diag script can prove it**

In `crates/viewer/src/keyboard_macos.rs`, in `note_window_active`, replace the body's `if active { .. } else { .. }` with a version that logs the resulting count:

```rust
pub fn note_window_active(active: bool) {
    KB.with(|k| {
        if let Some(s) = &*k.borrow() {
            let count = if active {
                s.active_windows.fetch_add(1, Ordering::Relaxed) + 1
            } else {
                // Saturating decrement: never underflow if notifications are unbalanced.
                s.active_windows
                    .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |c| Some(c.saturating_sub(1)))
                    .map(|prev| prev.saturating_sub(1))
                    .unwrap_or(0)
            };
            tracing::debug!("keyboard gate: active={active} count={count} (forwarding {})",
                if count > 0 { "ON" } else { "OFF" });
        }
    });
}
```

- [ ] **Step 6: Verify**

Run: `cargo test -p viewer 2>&1 | tail -5`
Expected: `test result: ok. 34 passed; 0 failed`

Run: `cargo check -p viewer --message-format=short 2>&1 | tail -5`
Expected: no errors.

This task has no new unit test: the counter lives in a `thread_local` behind `install()`, which needs a live ObjC runtime and an NSEvent monitor. It is verified by the `count=` log line — operator checklist item 1 in Task 7, plus `scripts/diag-keyboard-macos.sh`.

- [ ] **Step 7: Commit**

```bash
git add crates/viewer/src/main.rs crates/viewer/src/keyboard_macos.rs
git commit -F - <<'EOF'
fix(viewer/macos): prime the keyboard gate and disconnect it on teardown

The NSEvent monitor forwards keys only while active_windows > 0, and the only
writer was a connect_is_active_notify handler. `is-active` notifies on a
transition, so a window whose content became video while it was already the key
window never armed the gate: the remote keyboard stayed dead until a Cmd-Tab
round trip. Prime it from window.is_active() at install.

The same handler was never disconnected — the window shell outlives its content,
so every content swap stacked another, running release_all_input N times per
focus change and moving the gate count by N. Store the SignalHandlerId on
VideoContent and disconnect it in teardown_content, with the matching gate
decrement when the window being torn down is the key window.

Also log the resulting gate count so scripts/diag-keyboard-macos.sh can prove
the invariant: 0 with no video focused, >=1 with one focused.

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_0136CAyyXeBQLymr6UZSC2Mh
EOF
```

---

### Task 4: Cmd↔Ctrl swap, default on

Today Cmd forwards as `KEY_LEFTMETA`, so Cmd+L locks the remote session and a bare Cmd tap opens the GNOME overview. Ship a **true swap** — Cmd→Ctrl *and* Control→Meta — because a one-way Cmd→Ctrl map would leave no key on the keyboard producing Super.

**Files:**
- Modify: `crates/viewer/src/config.rs` (new field + env parser + tests)
- Modify: `crates/viewer/src/keyboard_macos.rs` (swap function + `install` signature + 3 call sites + tests)
- Modify: `crates/viewer/src/main.rs:640` (pass the resolved flag)

**Interfaces:**
- Consumes: nothing from earlier tasks.
- Produces: `config::cmd_is_ctrl() -> bool`; `config::parse_cmd_is_ctrl_env(Option<&str>) -> Option<bool>`; `keyboard_macos::install(writer: Writer, cmd_is_ctrl: bool)`.

- [ ] **Step 1: Write the failing config tests**

Append to `crates/viewer/src/config.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn env_override_parses_truthy_and_falsy() {
        assert_eq!(parse_cmd_is_ctrl_env(Some("0")), Some(false));
        assert_eq!(parse_cmd_is_ctrl_env(Some("false")), Some(false));
        assert_eq!(parse_cmd_is_ctrl_env(Some("FALSE")), Some(false));
        assert_eq!(parse_cmd_is_ctrl_env(Some("no")), Some(false));
        assert_eq!(parse_cmd_is_ctrl_env(Some("off")), Some(false));
        assert_eq!(parse_cmd_is_ctrl_env(Some("1")), Some(true));
        assert_eq!(parse_cmd_is_ctrl_env(Some("true")), Some(true));
    }

    /// Unset or blank means "no opinion" — fall through to the persisted config rather than
    /// silently disabling the swap.
    #[test]
    fn env_override_absent_or_blank_has_no_opinion() {
        assert_eq!(parse_cmd_is_ctrl_env(None), None);
        assert_eq!(parse_cmd_is_ctrl_env(Some("")), None);
        assert_eq!(parse_cmd_is_ctrl_env(Some("   ")), None);
    }

    /// A config.json written before this field existed must still load, with the swap on.
    #[test]
    fn legacy_config_without_the_field_still_loads_with_swap_on() {
        let c: Config = serde_json::from_str(r#"{"server_addr":"10.0.0.100:9001"}"#)
            .expect("legacy config must deserialize");
        assert_eq!(c.server_addr, "10.0.0.100:9001");
        assert!(c.cmd_is_ctrl, "the swap defaults on");
    }

    #[test]
    fn default_config_has_the_swap_on() {
        assert!(Config { server_addr: DEFAULT_SERVER_ADDR.to_string(), cmd_is_ctrl: true }.cmd_is_ctrl);
    }
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p viewer config`
Expected: FAIL to compile — `struct 'Config' has no field named 'cmd_is_ctrl'`, `cannot find function 'parse_cmd_is_ctrl_env'`.

- [ ] **Step 3: Implement the config side**

In `crates/viewer/src/config.rs`, change the struct and `Default`, and add the parser:

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    pub server_addr: String,
    /// macOS only: swap Cmd and Control on the wire, so Mac muscle memory (Cmd+C, Cmd+T) reaches
    /// the remote GNOME session as Ctrl. `serde(default)` so a config.json written before this
    /// field existed still loads. Ignored on Linux.
    #[serde(default = "default_cmd_is_ctrl")]
    pub cmd_is_ctrl: bool,
}

fn default_cmd_is_ctrl() -> bool {
    true
}
```

Add `cmd_is_ctrl: default_cmd_is_ctrl(),` to the `Default for Config` literal, then append these two functions:

```rust
/// Parse an `RMNG_CMD_IS_CTRL` value. `None` means "no opinion" (unset or blank) — the caller
/// falls through to the persisted config rather than treating absence as "off".
pub(crate) fn parse_cmd_is_ctrl_env(v: Option<&str>) -> Option<bool> {
    let s = v?.trim();
    if s.is_empty() {
        return None;
    }
    Some(!matches!(s.to_ascii_lowercase().as_str(), "0" | "false" | "no" | "off"))
}

/// The effective Cmd↔Ctrl setting: `RMNG_CMD_IS_CTRL` wins, else the persisted config, which
/// defaults to enabled.
pub fn cmd_is_ctrl() -> bool {
    parse_cmd_is_ctrl_env(std::env::var("RMNG_CMD_IS_CTRL").ok().as_deref())
        .unwrap_or_else(|| load().cmd_is_ctrl)
}
```

- [ ] **Step 4: Run to verify the config tests pass**

Run: `cargo test -p viewer config`
Expected: PASS — 4 new tests.

- [ ] **Step 5: Write the failing keyboard tests**

Append to the existing `mod tests` in `crates/viewer/src/keyboard_macos.rs`:

```rust
    /// A *swap*, not a one-way map: physical Control must still produce Super, or the GNOME
    /// overview and every Super chord become unreachable from a Mac keyboard.
    #[test]
    fn cmd_and_ctrl_swap_both_ways() {
        assert_eq!(swap_cmd_ctrl(125), 29, "KEY_LEFTMETA  → KEY_LEFTCTRL");
        assert_eq!(swap_cmd_ctrl(126), 97, "KEY_RIGHTMETA → KEY_RIGHTCTRL");
        assert_eq!(swap_cmd_ctrl(29), 125, "KEY_LEFTCTRL  → KEY_LEFTMETA");
        assert_eq!(swap_cmd_ctrl(97), 126, "KEY_RIGHTCTRL → KEY_RIGHTMETA");
    }

    /// Applying it twice is the identity — the property that makes it a swap.
    #[test]
    fn swap_is_an_involution() {
        for code in [125u32, 126, 29, 97, 30, 58, 0] {
            assert_eq!(swap_cmd_ctrl(swap_cmd_ctrl(code)), code, "code {code}");
        }
    }

    #[test]
    fn non_modifier_keys_pass_through_the_swap() {
        // kVK_A and kVK_Space: whatever the table says, the swap must not touch them.
        for kvk in [0x00u32, 0x31] {
            let plain = kvk_evdev::translate(kvk);
            assert_eq!(swap_cmd_ctrl(plain), plain, "kVK {kvk:#04x}");
        }
    }

    #[test]
    fn to_evdev_applies_the_swap_only_when_enabled() {
        // kVK_Command / kVK_Control.
        assert_eq!(to_evdev(0x37, false), 125, "swap off: Cmd stays Super");
        assert_eq!(to_evdev(0x37, true), 29, "swap on: Cmd becomes Ctrl");
        assert_eq!(to_evdev(0x3B, false), 29, "swap off: Control stays Ctrl");
        assert_eq!(to_evdev(0x3B, true), 125, "swap on: Control becomes Super");
    }

    /// CapsLock is special-cased by keycode in the FlagsChanged path; the swap must not disturb
    /// it, or the lock-toggle branch stops matching.
    #[test]
    fn capslock_survives_the_swap() {
        assert_eq!(to_evdev(0x39, true), KEY_CAPSLOCK);
    }
```

- [ ] **Step 6: Run to verify failure**

Run: `cargo test -p viewer keyboard_macos`
Expected: FAIL to compile — `cannot find function 'swap_cmd_ctrl'`, `cannot find function 'to_evdev'`.

- [ ] **Step 7: Implement the swap**

In `crates/viewer/src/keyboard_macos.rs`, after the `KEY_CAPSLOCK` const, add:

```rust
/// evdev codes the Cmd↔Ctrl swap exchanges (`input-event-codes.h`).
const KEY_LEFTCTRL: u32 = 29;
const KEY_RIGHTCTRL: u32 = 97;
const KEY_LEFTMETA: u32 = 125;
const KEY_RIGHTMETA: u32 = 126;

/// Exchange Cmd and Control so Mac chords (Cmd+C, Cmd+T) reach the remote GNOME session as Ctrl.
///
/// A **swap**, not a one-way map: physical Control becomes Super, so the overview and every Super
/// chord stay reachable. Applying it twice is the identity. Non-modifier codes pass through, which
/// keeps CapsLock's special case in the `FlagsChanged` path intact.
fn swap_cmd_ctrl(evdev: u32) -> u32 {
    match evdev {
        KEY_LEFTMETA => KEY_LEFTCTRL,
        KEY_RIGHTMETA => KEY_RIGHTCTRL,
        KEY_LEFTCTRL => KEY_LEFTMETA,
        KEY_RIGHTCTRL => KEY_RIGHTMETA,
        other => other,
    }
}

/// The single translate choke point: physical kVK → the evdev code that actually goes on the wire.
/// Every send path routes through this, so the held-set stores exactly what was sent and releases
/// pair with their presses even when the swap is on.
fn to_evdev(kvk: u32, cmd_is_ctrl: bool) -> u32 {
    let code = kvk_evdev::translate(kvk);
    if cmd_is_ctrl { swap_cmd_ctrl(code) } else { code }
}
```

- [ ] **Step 8: Run to verify the keyboard tests pass**

Run: `cargo test -p viewer keyboard_macos`
Expected: PASS — 5 new tests on top of the existing 11.

- [ ] **Step 9: Thread the flag through `install`**

In `crates/viewer/src/keyboard_macos.rs`:

- Add a field to `struct Shared`: `cmd_is_ctrl: bool,` (with a doc line: `/// Resolved once at install; the monitor block captures a copy.`)
- Change the signature to `pub fn install(writer: Writer, cmd_is_ctrl: bool) {`
- No clone is needed for the monitor block: `cmd_is_ctrl: bool` is `Copy`, so the existing `move` closure captures it directly.
- Replace all three `kvk_evdev::translate(kc)` calls inside the monitor block with `to_evdev(kc, cmd_is_ctrl)`. Confirm with `grep -n 'kvk_evdev::translate' crates/viewer/src/keyboard_macos.rs` — after this step the only remaining occurrence is inside `to_evdev` (plus the test module).
- Add `cmd_is_ctrl` to the `Shared { .. }` literal.
- Extend the install log line to record the mode:
  ```rust
  tracing::info!(
      "macOS keyboard monitor installed (physical keys → remote; Cmd↔Ctrl swap {})",
      if cmd_is_ctrl { "ON" } else { "off" }
  );
  ```

In `crates/viewer/src/main.rs`, replace line 640:

```rust
    keyboard_macos::install(writer.clone());
```

with:

```rust
    keyboard_macos::install(writer.clone(), config::cmd_is_ctrl());
```

and extend the comment block above it with:

```rust
    // Cmd↔Ctrl is swapped by default so Mac chords reach the remote GNOME session as Ctrl
    // (physical Control becomes Super, so overview chords stay reachable). Disable with
    // RMNG_CMD_IS_CTRL=0 or "cmd_is_ctrl": false in the viewer config.
```

- [ ] **Step 10: Verify**

Run: `cargo test -p viewer 2>&1 | tail -5`
Expected: `test result: ok. 43 passed; 0 failed`

Run: `cargo check -p viewer --message-format=short 2>&1 | tail -5`
Expected: no errors.

- [ ] **Step 11: Commit**

```bash
git add crates/viewer/src/config.rs crates/viewer/src/keyboard_macos.rs crates/viewer/src/main.rs
git commit -F - <<'EOF'
feat(viewer/macos): swap Cmd and Control on the wire, default on

Cmd forwarded as KEY_LEFTMETA, so Cmd+L locked the remote session and a bare Cmd
tap opened the GNOME overview. Swap Cmd and Control instead — a true swap, so
physical Control produces Super and overview chords stay reachable; a one-way
Cmd->Ctrl map would leave no key producing Super at all.

Applied at a single choke point (to_evdev) after kvk_evdev::translate, so that
table stays a pure physical-identity map with its tests intact, and the held-set
stores exactly the codes that went on the wire — presses and releases still
pair. Local chords are unaffected: F11 / Ctrl+Alt+G / Ctrl+Alt+P are detected
from raw NSEvent.modifierFlags and GDK state, neither of which this touches.

Off via RMNG_CMD_IS_CTRL=0 or "cmd_is_ctrl": false. The config field is
serde-defaulted so config.json files written before it existed still load.

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_0136CAyyXeBQLymr6UZSC2Mh
EOF
```

---

### Task 5: Two kVK table corrections

**Files:**
- Modify: `crates/viewer/src/kvk_evdev.rs` — table entries `0x6E` and `0x72`, the header comment's unassigned list, and the tests

**Interfaces:**
- Consumes: nothing. Produces: nothing new (behaviour change only). Independent of every other task.

- [ ] **Step 1: Write the failing tests**

Append to the existing `mod tests` in `crates/viewer/src/kvk_evdev.rs`:

```rust
    /// 0x72 is Help on a 1990s Apple keyboard and **Insert** on every PC keyboard attached to a
    /// Mac; Chromium's dom_code_data.inc maps kVK 0x72 → INSERT. KEY_HELP(138) made Shift+Insert
    /// and Ctrl+Insert unreachable, which is how paste works in a lot of remote software.
    #[test]
    fn help_key_is_insert() {
        assert_eq!(translate(0x72), 110, "kVK_Help/Insert → KEY_INSERT");
    }

    /// Apple's HIToolbox/Events.h defines kVK_ContextualMenu = 0x6E — the PC Menu key. Linux
    /// delivers that key as KEY_COMPOSE(127), not KEY_MENU(139).
    #[test]
    fn contextual_menu_is_compose() {
        assert_eq!(translate(0x6E), 127, "kVK_ContextualMenu → KEY_COMPOSE");
    }
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p viewer kvk_evdev`
Expected: FAIL — `help_key_is_insert` gets `138`, `contextual_menu_is_compose` gets `0`.

- [ ] **Step 3: Fix the two table entries**

In `crates/viewer/src/kvk_evdev.rs`, replace:

```rust
    /*0x6E*/  U, // (unassigned)
```

with:

```rust
    /*0x6E*/127, // kVK_ContextualMenu    KEY_COMPOSE  (PC "Menu" key; Linux calls it Compose)
```

and replace:

```rust
    /*0x72*/138, // kVK_Help              KEY_HELP
```

with:

```rust
    /*0x72*/110, // kVK_Help              KEY_INSERT   (Insert on any PC keyboard; Chromium agrees)
```

- [ ] **Step 4: Fix the header comment**

Replace this line in the module doc:

```rust
//! - `0x6C`, `0x6E`, `0x70`, `0x7F` — unassigned
```

with:

```rust
//! - `0x6C`, `0x70`, `0x7F` — unassigned
```

- [ ] **Step 5: Run to verify the tests pass**

Run: `cargo test -p viewer kvk_evdev`
Expected: PASS. If `special_keys` or `sentinels` now fail, they assert the old values — update those assertions to `110` / `127` and keep their comments accurate.

- [ ] **Step 6: Verify the whole suite**

Run: `cargo test -p viewer 2>&1 | tail -5`
Expected: `test result: ok. 45 passed; 0 failed`

- [ ] **Step 7: Commit**

```bash
git add crates/viewer/src/kvk_evdev.rs
git commit -F - <<'EOF'
fix(viewer/macos): map kVK 0x72 to KEY_INSERT and 0x6E to KEY_COMPOSE

0x72 was KEY_HELP(138). It is Help on a 1990s Apple keyboard and Insert on every
PC keyboard attached to a Mac; Chromium's dom_code_data.inc maps it to INSERT.
As KEY_HELP, Shift+Insert and Ctrl+Insert were unreachable.

0x6E was sentineled "unassigned" and silently swallowed, but Apple's own
HIToolbox/Events.h on this machine defines kVK_ContextualMenu = 0x6E — the PC
Menu key. Linux delivers it as KEY_COMPOSE(127), not KEY_MENU(139).

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_0136CAyyXeBQLymr6UZSC2Mh
EOF
```

---

### Task 6: Terminal font must resolve to a real monospace family

Verified on the target Mac: Homebrew installs `gsettings-desktop-schemas` as a GStreamer dependency, so `interface_settings()` returns `Some` on macOS and yields `'Adwaita Mono 11'` — a font that is **not installed**. The guard at `terminal.rs:80` checks `fd.family().is_some()`, which passes, because a descriptor for a missing family still carries the name. Pango then substitutes a proportional face:

```
Adwaita Mono 11 → w("iiiiiiiiii")= 50  w("MMMMMMMMMM")=140   proportional
Menlo 11        → w("iiiiiiiiii")=110  w("MMMMMMMMMM")=110   monospace
Monospace 11    → w("iiiiiiiiii")=110  w("MMMMMMMMMM")=110   monospace  (never reached)
```

**Files:**
- Modify: `crates/viewer/src/terminal.rs` — `FALLBACK_FONT`, new `pick_font`, `load_font`, its one call site in `ensure_metrics`
- Test: `crates/viewer/src/terminal.rs` new `#[cfg(test)] mod tests`

**Interfaces:**
- Consumes: nothing. Produces: `fn pick_font(requested: Option<&str>, is_monospace_family: impl Fn(&str) -> bool) -> pango::FontDescription`; `fn load_font(settings: Option<&gio::Settings>, ctx: &pango::Context) -> pango::FontDescription` (signature change — one caller).

- [ ] **Step 1: Write the failing tests**

`pango::FontDescription::from_string` is pure pango — it needs no GTK init and no display, so these run headless. Append to `crates/viewer/src/terminal.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    /// The macOS failure this task fixes: the desktop setting names a family that is not
    /// installed, so pango would substitute a proportional face and the cell metrics would be
    /// measured from it. A descriptor for a missing family still reports `family() == Some(..)`,
    /// so only a font-map check can catch it.
    #[test]
    fn unresolvable_family_falls_back() {
        let fd = pick_font(Some("Adwaita Mono 11"), |_| false);
        assert_eq!(fd.to_str(), pango::FontDescription::from_string(FALLBACK_FONT).to_str());
    }

    #[test]
    fn resolvable_monospace_family_is_honoured() {
        let fd = pick_font(Some("Menlo 13"), |f| f == "Menlo");
        assert_eq!(fd.family().as_deref(), Some("Menlo"));
        assert_eq!(fd.size(), 13 * pango::SCALE);
    }

    /// A family that resolves but is *proportional* must be rejected too — that is the actual
    /// substitution failure mode, not just a missing font.
    #[test]
    fn resolvable_but_proportional_family_falls_back() {
        let fd = pick_font(Some("Helvetica 11"), |f| f != "Helvetica");
        assert_eq!(fd.to_str(), pango::FontDescription::from_string(FALLBACK_FONT).to_str());
    }

    #[test]
    fn missing_or_blank_setting_falls_back() {
        for requested in [None, Some(""), Some("   ")] {
            let fd = pick_font(requested, |_| true);
            assert_eq!(fd.to_str(), pango::FontDescription::from_string(FALLBACK_FONT).to_str());
        }
    }

    /// The fallback itself must be monospace on the platform it targets, or the guard just
    /// swaps one broken grid for another.
    #[test]
    fn fallback_names_a_family() {
        let fd = pango::FontDescription::from_string(FALLBACK_FONT);
        assert!(fd.family().is_some(), "FALLBACK_FONT must name a family");
        assert!(fd.size() > 0, "FALLBACK_FONT must carry a point size");
    }
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p viewer terminal`
Expected: FAIL to compile — `cannot find function 'pick_font'`.

- [ ] **Step 3: Make the fallback per-OS and add `pick_font`**

In `crates/viewer/src/terminal.rs`, replace:

```rust
/// Font used when the GNOME `monospace-font-name` setting isn't available (non-GNOME / macOS).
/// A point size (not `px`), so it scales with DPI + text-scaling like the rest of the desktop.
const FALLBACK_FONT: &str = "Monospace 11";
```

with:

```rust
/// Font used when the desktop's `monospace-font-name` is unset, or names a family that does not
/// resolve to an installed monospace face. A point size (not `px`), so it scales with DPI +
/// text-scaling like the rest of the desktop.
///
/// macOS gets an explicit family: fontconfig's generic `Monospace` alias does resolve there, but
/// naming the system terminal font is both faster to resolve and what the platform expects.
#[cfg(target_os = "macos")]
const FALLBACK_FONT: &str = "Menlo 11";
#[cfg(not(target_os = "macos"))]
const FALLBACK_FONT: &str = "Monospace 11";
```

Then replace the whole of `load_font` with `pick_font` plus a rewired `load_font`:

```rust
/// Choose the terminal font: the desktop's `requested` monospace font when it resolves to an
/// installed monospace family, else [`FALLBACK_FONT`].
///
/// The family MUST be checked against the font map, not merely parsed. A `FontDescription` built
/// from a missing family still reports `family() == Some(..)`, and pango then silently substitutes
/// a **proportional** face — so the old `fd.family().is_some()` guard could not catch it. That is
/// exactly what happens on macOS: Homebrew pulls in `gsettings-desktop-schemas` as a GStreamer
/// dependency, so the GNOME schema *is* readable and yields `'Adwaita Mono 11'` for a font that is
/// not installed. Measuring cell metrics from the substituted proportional face garbles the grid
/// and ships a wrong column count to the real tmux PTYs. Linux is protected by the same check.
///
/// `is_monospace_family` is injected so the decision is testable without a display.
fn pick_font(
    requested: Option<&str>,
    is_monospace_family: impl Fn(&str) -> bool,
) -> pango::FontDescription {
    if let Some(name) = requested {
        if !name.trim().is_empty() {
            let fd = pango::FontDescription::from_string(name);
            if let Some(family) = fd.family() {
                if is_monospace_family(&family) {
                    return fd;
                }
                tracing::warn!(
                    "terminal font {family:?} is not an installed monospace family; \
                     falling back to {FALLBACK_FONT}"
                );
            }
        }
    }
    pango::FontDescription::from_string(FALLBACK_FONT)
}

/// The terminal font: the GNOME `monospace-font-name` (e.g. `"Monaspace Neon Frozen 11"` — a
/// family plus a point size) when it resolves to an installed monospace family, else
/// [`FALLBACK_FONT`]. DPI and text-scaling are applied later by the widget's pango context, so
/// this matches how the rest of the desktop sizes the same font.
fn load_font(settings: Option<&gio::Settings>, ctx: &pango::Context) -> pango::FontDescription {
    let requested = settings.map(|s| s.string("monospace-font-name").to_string());
    pick_font(requested.as_deref(), |family| {
        ctx.font_map().is_some_and(|fm| {
            fm.list_families()
                .iter()
                .any(|f| f.name().eq_ignore_ascii_case(family) && f.is_monospace())
        })
    })
}
```

- [ ] **Step 4: Rewire the one caller**

In `ensure_metrics`, the pango context is currently built *after* `load_font`. Reorder so it can be passed in. Replace:

```rust
            if self.font.borrow().is_none() {
                let fd = load_font(self.settings.borrow().as_ref());
                gtk4::glib::g_debug!("rmng-term", "terminal font: {}", fd.to_str());
                let ctx = self.obj().pango_context();
                let m = ctx.metrics(Some(&fd), None);
```

with:

```rust
            if self.font.borrow().is_none() {
                let ctx = self.obj().pango_context();
                let fd = load_font(self.settings.borrow().as_ref(), &ctx);
                gtk4::glib::g_debug!("rmng-term", "terminal font: {}", fd.to_str());
                let m = ctx.metrics(Some(&fd), None);
```

- [ ] **Step 5: Run to verify the tests pass**

Run: `cargo test -p viewer terminal`
Expected: PASS — 5 new tests.

If `list_families` / `is_monospace` / `font_map` do not resolve against gtk4 0.9's pango bindings, check the trait import: `pango::prelude::*` may be needed in scope (the file already imports `gtk4::prelude::*` and `gtk4::pango`). Add `use gtk4::pango::prelude::*;` if the compiler asks for `FontMapExt` / `FontFamilyExt`.

- [ ] **Step 6: Verify the whole suite**

Run: `cargo test -p viewer 2>&1 | tail -5`
Expected: `test result: ok. 50 passed; 0 failed`

Run: `cargo check -p viewer --message-format=short 2>&1 | tail -5`
Expected: no errors.

- [ ] **Step 7: Commit**

```bash
git add crates/viewer/src/terminal.rs
git commit -F - <<'EOF'
fix(viewer): require the terminal font to resolve to a real monospace family

The guard was fd.family().is_some(), which a descriptor for an *uninstalled*
family also passes — pango then silently substitutes a proportional face, and
the cell metrics get measured from it. On macOS that is the normal path, not an
edge case: Homebrew installs gsettings-desktop-schemas as a GStreamer
dependency, so the GNOME schema is readable and yields 'Adwaita Mono 11' for a
font that is not installed. Measured on the target Mac, "iiiiiiiiii" renders 50px
wide against 140px for "MMMMMMMMMM" — a garbled grid, and a wrong column count
shipped to the real tmux PTYs.

Check the resolved family against the font map and require is_monospace(),
falling back to Menlo on macOS / Monospace elsewhere. Linux gets the same
protection against a monospace-font-name naming a missing family. The decision
is a pure function with an injected predicate so it tests without a display.

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_0136CAyyXeBQLymr6UZSC2Mh
EOF
```

---

### Task 7: macOS documentation, three corrected comments, and the operator checklist

**Files:**
- Modify: `docs/DEVELOPMENT.md` (Prerequisites section, ~line 73)
- Modify: `crates/viewer/README.md` (per-OS backend table + macOS input notes)
- Modify: `crates/viewer/src/main.rs` (the `GSK_RENDERER` comment block, lines 103-115)
- Modify: `crates/viewer/src/glunpack.rs` (the `validate` doc comment, lines 647-650)
- Modify: `run-macos-test.sh` (operator checklist — untracked helper; keep it untracked)

**Interfaces:** none — documentation only.

- [ ] **Step 1: Add the macOS section to `docs/DEVELOPMENT.md`**

After the existing Prerequisites paragraph (which ends `see [Publishing the template](DEPLOY.md#publishing-the-template).`), add:

```markdown
<a id="macos"></a>
### macOS (Apple Silicon) — viewer only

Only the **viewer** builds and runs on macOS; the capture/encode/server side is Linux-only by
design. Verified on macOS 26.4 / Apple M-series with Homebrew:

```sh
brew install gtk4 gstreamer pkgconf     # verified: gtk4 4.22.4, gstreamer 1.28.4, pkgconf 2.5.1
cargo build -p viewer --release         # → target/release/rmng-viewer
```

**Build the viewer package, not the workspace root.** `cargo build` / `check` / `test` / `clippy`
at the root fail on macOS inside `libspa-sys`: `clone-daemon` depends on pipewire
unconditionally, and the workspace declares no `default-members`. Everything you need on a Mac is
`-p viewer` (add `-p cli -p control-client` if you want the `rmng` CLI, which builds clean and is
HTTP-only).

**Never mix GTK providers.** The official `GStreamer.framework` `.pkg` bundles *its own* GTK4 for
the gtk4 plugin. Use Homebrew for **both** GStreamer and GTK4 (one GTK in the process) or the
framework for both — mixing them produces link-time and runtime chaos. No environment variables
are needed with an all-Homebrew stack at the default `/opt/homebrew` prefix.

Sanity-check the stack before debugging any video problem:

```sh
gst-inspect-1.0 vtdec_hw            # VideoToolbox HW decoder (applemedia)
gst-inspect-1.0 gtk4paintablesink   # GL zero-copy sink
cargo run -p viewer --release -- --glunpack-validate 256 144   # expect max abs err 0
```

**macOS input notes.** **Cmd and Control are swapped on the wire by default**, so Mac chords
(Cmd+C, Cmd+T) reach the remote GNOME session as Ctrl and physical Control produces Super —
disable with `RMNG_CMD_IS_CTRL=0` or `"cmd_is_ctrl": false` in
`~/.config/rmng-viewer/config.json`. The F-row needs `fn` on a default MacBook (or turn on "Use F1,
F2 etc. as standard function keys"). **Cmd-Tab and Cmd-Space cannot be forwarded** — the Wayland
`inhibit_system_shortcuts` protocol that `grab_keys()` uses does not exist on macOS, so that call
is a silent no-op there; capturing those would need a permission-gated `CGEventTap`, which the
viewer deliberately avoids (an `NSEvent` local monitor needs no Input Monitoring grant).
`RMNG_NO_POINTER_LOCK=1` disables pointer lock entirely.
```

- [ ] **Step 2: Add the per-OS backend table to `crates/viewer/README.md`**

The README documents VA-API + dmabuf + Wayland as if they were the only backends, which
contradicts five shipped macOS modules. Immediately after the "What it does" numbered list, insert:

```markdown
## Per-OS backends

One toolkit-free core, two platform backends. The transport, the wire protocol and the AVC444
packing are identical; only these pieces differ.

| | Linux | macOS (Apple Silicon) |
| --- | --- | --- |
| H.264 decode | `vah264dec` (VA-API) | `vtdec_hw` (VideoToolbox) |
| GL import | `glupload` → 2D `GLMemory` | `vtdec_hw` emits IOSurface-backed `GLMemory` directly; `glupload` drops out |
| Texture target | `2D` | `rectangle` (Apple's `CGLTexImageIOSurface2D` accepts only `GL_TEXTURE_RECTANGLE`) |
| 4:2:0 sink path | `glupload ! gtk4paintablesink` | `vtdec_hw ! glcolorconvert ! gtk4paintablesink` (the sink takes RGBA 2D only) |
| 4:4:4 unpack | `rmngavc444unpack`, `sampler2D`, `#version 300 es` | `rmngavc444unpack`, `sampler2DRect`, desktop GLSL (Apple has no `ARB_ES3_compatibility`) |
| GL platform | EGL | CGL (desktop GL 4.1 over Metal) |
| Keyboard | GTK `EventControllerKey`, `evdev = hardware_keycode − 8` | raw `NSEvent` local monitor + `kvk_evdev` table ([`keyboard_macos.rs`](src/keyboard_macos.rs)); GDK-swallowed keys still come via GTK |
| Pointer lock | `zwp_pointer_constraints` + `zwp_relative_pointer` ([`pointer_lock.rs`](src/pointer_lock.rs)) | `CGAssociateMouseAndMouseCursorPosition` + `NSEvent` deltas ([`pointer_lock_macos.rs`](src/pointer_lock_macos.rs)) |
| Titlebar | GTK `HeaderBar` | real `NSWindow` titlebar + `NSButton` accessories ([`native_titlebar.rs`](src/native_titlebar.rs)) |
| GSK renderer | `gl` pinned (stale-texture workaround) | `ngl` — the legacy `gl` renderer was removed in GTK 4.18, so no pin is available |

Build and run instructions: [DEVELOPMENT.md § macOS](../../docs/DEVELOPMENT.md#macos).
```

- [ ] **Step 3: Correct the `GSK_RENDERER` comment**

The comment explains a stale-frame bug, pins `gl` on Linux, and says only "macOS: the legacy `gl`
renderer was removed in GTK ≥ 4.18; the pin is Linux-only" — without noting that macOS therefore
runs the *affected* renderer with no pin available. In `crates/viewer/src/main.rs`, replace:

```rust
    // macOS: the legacy `gl` renderer was removed in GTK ≥ 4.18; the pin is Linux-only.
```

with:

```rust
    // macOS: the legacy `gl` renderer was removed in GTK ≥ 4.18, so the pin is Linux-only — which
    // means macOS runs `ngl`, the renderer this workaround exists for, with no escape available.
    // The symptom has never actually been observed on a Mac (Apple's compositor path differs), so
    // we do not pay for a mitigation up front. If an old frame ever flashes there — most likely in
    // a downscaled window, since ngl caches a scaled intermediate — the manual escape is
    // `GSK_RENDERER=cairo` (correct, slower), and the designed fix is rendering the latest frame
    // ourselves via a GtkGLArea fed by `appsink max-buffers=1 drop=true` instead of for_paintable.
```

- [ ] **Step 4: Correct the `validate` doc comment**

`validate()` claims to compile the rectangle shader variant, but its body never does — the only
occurrence of "rect" in the whole function is that sentence, and macOS's Yuv444 path is the
rectangle path. In `crates/viewer/src/glunpack.rs`, replace:

```rust
/// This validates the 2D path (glupload yields 2D textures) and, on desktop GL, also compiles the
/// rectangle-texture shader variant to catch syntax errors early (the rect path is exercised live
/// against vtdec_hw, not via this harness, since raw sysmem upload produces 2D textures).
```

with:

```rust
/// This validates the **2D path only** — `glupload` from raw sysmem always yields 2D textures, so
/// the harness cannot reach the rectangle-sampler variant. That is the variant macOS actually uses
/// in Yuv444 (vtdec_hw emits IOSurface-backed rectangle textures), and it is covered only by
/// running live against a server in `RMNG_CHROMA=yuv444`. Exercising it here would mean building a
/// rectangle-texture source by hand; worth doing if the rect path ever regresses.
```

- [ ] **Step 5: Rewrite the operator checklist in `run-macos-test.sh`**

This file is untracked (a personal helper) — keep it that way; do not `git add` it. Replace its
checklist block so it matches what this branch changed, ordered so the pointer-lock test (the only
one that can freeze the cursor) runs last and deliberately:

```sh
echo " 1. KEYBOARD GATE — type into a remote editor IMMEDIATELY after connecting, with no"
echo "    Cmd-Tab first. Keys must reach the remote. Then check the log:"
echo "      grep 'keyboard gate' $LOG    # count>=1 while a video window is focused, 0 otherwise"
echo " 2. CMD<->CTRL (default ON) — Cmd+C / Cmd+T / Cmd+A behave as Ctrl in the remote session;"
echo "    physical Control opens the GNOME overview (it is Super now). Ctrl+Alt+G and F11 must"
echo "    still work as LOCAL chords. Re-run with RMNG_CMD_IS_CTRL=0 to confirm the old behaviour."
echo " 3. INSERT / MENU — only with a PC keyboard attached: Shift+Insert pastes in the remote,"
echo "    the Menu key opens a context menu."
echo " 4. VIDEO / RESIZE / SCROLL / CLIPBOARD — regression sweep: desktop renders, ~60fps under"
echo "    motion, shrink the window well below 1:1 and drag remote windows (watch for an OLD"
echo "    frame flashing), trackpad + wheel scroll, copy/paste both directions."
echo " 5. POINTER LOCK — run this LAST, it can freeze the cursor. Engage in a cursor-hiding remote"
echo "    app, then Cmd-Tab away: the Mac cursor MUST unfreeze and reappear. Cmd-Tab back: it"
echo "    re-engages. Then deselect the clone in the web UI while locked: cursor must be released"
echo "    as the windows are destroyed. Abort switch: relaunch with RMNG_NO_POINTER_LOCK=1."
```

- [ ] **Step 6: Verify nothing in the code changed behaviour**

Run: `cargo test -p viewer 2>&1 | tail -5`
Expected: `test result: ok. 50 passed; 0 failed`

Run: `cargo build -p viewer --release 2>&1 | tail -3`
Expected: `Finished` with no errors.

Run: `cargo run -p viewer --release -- --glunpack-validate 256 144 2>&1 | tail -3`
Expected: max abs err 0 (unchanged from baseline).

- [ ] **Step 7: Commit**

```bash
git add docs/DEVELOPMENT.md crates/viewer/README.md crates/viewer/src/main.rs crates/viewer/src/glunpack.rs
git commit -F - <<'EOF'
docs(macos): build/run/input guide, per-OS backend table, honest comments

README.md advertises a macOS viewer but no macOS instruction existed anywhere in
the tree — the only brew recipe and the two-GTK-provider hazard warning were
deleted with MACOS_PORT.md and lived solely in git history. Add a macOS section
to DEVELOPMENT.md with the versions verified on the target machine, the
`-p viewer` requirement (root cargo build dies in libspa-sys: clone-daemon
depends on pipewire unconditionally and the workspace has no default-members),
and the input notes: Cmd<->Ctrl default, fn for the F-row, why Cmd-Tab/Cmd-Space
cannot be forwarded.

crates/viewer/README.md documented VA-API + dmabuf + Wayland as the only
backends, contradicting five shipped macOS modules — add a per-OS backend table.

Two comments were actively misleading. The GSK_RENDERER pin explained a
stale-frame bug and gated the fix to Linux without noting that macOS therefore
runs the affected renderer with no pin available; record that, the manual
GSK_RENDERER=cairo escape, and why we are not paying for a mitigation yet.
--glunpack-validate claimed to compile the rectangle shader variant, but its body
never does — and rectangle is the only variant macOS uses in Yuv444.

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_0136CAyyXeBQLymr6UZSC2Mh
EOF
```

---

## Out of scope (from the spec's §4)

Do **not** implement these; they are recorded in
`docs/superpowers/specs/2026-07-25-macos-parity-after-merge-design.md` with file:line.

Terminal-view work beyond Task 6 (DPI cell metrics, pixel-vs-notch trackpad scroll, Cmd+C/Cmd+V
in the terminal, Dark Mode palette) — the Mac is a video-only client for now. GCMouse
unaccelerated deltas. `.app` bundling, Info.plist identity, codesigning, notarisation. The
`~/Library/Application Support` config move. GitHub Actions CI. Any `default-members` change to
the workspace (documented instead, so root `cargo build` keeps working as the upstream Linux
developer expects).
