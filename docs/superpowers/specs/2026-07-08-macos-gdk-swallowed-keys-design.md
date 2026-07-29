# macOS: forward GDK-swallowed Tab/Space/Enter from the GTK key handler

**Date:** 2026-07-08
**Component:** `crates/viewer/src/main.rs` (`install_keyboard`)
**Status:** Implemented & verified on-device
**Related:** [modifier state fix](2026-07-08-macos-modifier-state-device-independent-design.md) (same debugging session)

## Problem

On macOS, physical keys reach the remote via the raw `NSEvent` local monitor
(`keyboard_macos.rs`). But **Tab, Space, and Return/Enter did nothing on the remote** —
the user's "can't use tab, space, enter" report.

A `RUST_LOG=debug` capture (`scripts/diag-keyboard-macos.sh`) showed the exact mechanism:
for those three keys the monitor logged **only `key up`, never `key down`**, while every
other key (letters, Backspace, punctuation) logged a clean down→up. GDK's macOS backend
routes `keyDown` through `interpretKeyEvents:`, where Cocoa's standard key bindings turn
these into *commands* — Tab → key-view loop (`insertTab:`/`selectNextKeyView:`), Space →
button "click", Return → default-button key equivalent (`insertNewline:`). Consumed as
commands, their `keyDown` never surfaces as a plain key event the monitor recognizes;
`keyUp` isn't interpreted, so it flows through — hence the asymmetry. The remote received
the release but never the press, so the keys were dead.

(Research confirmed local `NSEvent` monitors run *first* in `sendEvent:`, so this is not
AppKit key-equivalent ordering — it is GDK's `interpretKeyEvents:` path upstream.)

## Key finding

A second capture with logging added to GTK's `EventControllerKey` proved these keys **do**
surface in `key_pressed`/`key_released`, with `code` == the Carbon kVK exactly
(Space=0x31, Tab=0x30, Return=0x24) and correct keyvals. GTK's handler already runs on
macOS (for the F11 / Ctrl+Alt+G/P local shortcuts) but otherwise ignores physical keys.

## Design

Forward exactly the GDK-swallowed keys from the GTK handler; leave every other key to the
NSEvent monitor. The two paths are disjoint, so nothing double-sends.

- **`macos_gdk_swallowed_key(code) -> Option<u32>`** — allowlist of the swallowed kVKs
  (`0x24` Return, `0x30` Tab, `0x31` Space, `0x4C` KeypadEnter), returning the evdev
  keycode via the existing `kvk_evdev::translate` (the same table the monitor uses).
- **`key_pressed`** (macOS): if `macos_gdk_swallowed_key(code)` matches, insert into
  `state.pressed`, send `key_code pressed:true`, and `return Propagation::Stop` (so GTK
  doesn't also run local focus traversal / button activation). Dedup on `state.pressed`
  so held-key autorepeat isn't stacked on the remote's own repeat.
- **`key_released`** (macOS): mirror — remove from `state.pressed`, send `pressed:false`.

`state.pressed` is the GTK-side held-set already drained by `release_all_input` on focus
loss / the Ctrl+Alt+P panic, so these keys are cleaned up there too. The monitor's own
held-set is untouched (it never sees these keys), so `keyboard_macos::release_all` and the
GTK cleanup don't overlap.

### Why no double-send

The monitor forwards a key by consuming its `keyDown`; it never sees Tab/Space/Return
`keyDown`, so it never forwards them, and its `keyUp` handler drops them (never in its
held-set). Letters/Backspace/etc. are *not* in the allowlist, so the GTK path skips them.
Modifiers stay on the monitor's `flags` path, so Ctrl+Tab / Shift+Enter compose correctly.

## Verification (on-device, confirmed working)

Capture `/tmp/rmng-kb-20260708-015511.log`:
- Space → `forward code=0x31 evdev=57 pressed=true/false`, no monitor duplicate.
- Tab → `forward code=0x30 evdev=15`, no duplicate.
- Shift+Return → Shift via monitor `flags`, Return via `forward code=0x24 evdev=28`.
- Letters, Backspace, Ctrl+X/C/V → monitor `key down/up` only, GTK path skips them.
- User confirmed Tab/Space/Enter now work on the remote.

`cargo test -p viewer` (24 pass), `cargo build --release -p viewer`, clippy clean on the
new code.

## Follow-up: cursor-navigation keys (arrows, Home/End/PageUp/PageDown)

Arrow keys were reported dead on the remote too. Same mechanism: in Cocoa they are
`moveLeft:`/`moveUp:`/etc. commands consumed by `interpretKeyEvents:` before the monitor
sees their `keyDown`. Confirmed by elimination — the kVK→evdev table maps all four arrows
correctly (`0x7B`→`KEY_LEFT` … `0x7E`→`KEY_UP`), yet they did nothing, so the monitor
never received them. The allowlist was extended to the full cursor-movement family:
arrows `0x7B–0x7E`, Home `0x73`, PageUp `0x74`, End `0x77`, PageDown `0x79`. These are the
same `interpretKeyEvents:` movement class as the arrows; the monitor owns only *text* keys
(letters, Backspace, punctuation), which is why extending the list can't double-send.

## Out of scope / follow-ups

- Escape (`cancelOperation:`) and Forward-Delete (`deleteForward:`) were not reported; if
  they surface as dead, they're one kVK each to add. The `monitor-missed` debug log and a
  duplicate `key down` (would indicate a monitor-owned key wrongly listed) make it easy to
  spot from a capture.
- The verbose per-keystroke GTK diagnostic logging was removed (it logged typed text);
  only the modifier `flags:` log and the 4-key `monitor-missed` log (non-sensitive)
  remain, plus the pre-existing `key down/up` logs.
