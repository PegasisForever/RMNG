# macOS modifier press/release from device-independent flags

**Date:** 2026-07-08
**Component:** `crates/viewer/src/keyboard_macos.rs`
**Status:** Approved (autonomous fix — user pre-approved plan+spec+implementation)

## Problem

On macOS the viewer captures physical keys via a raw `NSEvent` local monitor. For
modifier keys (`FlagsChanged` events) it must decide whether the transition is a
**press** or a **release** and forward that to the remote.

The previous fix (`bc57e32`) derived press/release by reading the key's
**device-dependent** modifier bit — the low 16 bits of `NSEvent.modifierFlags`
(`NX_DEVICELCTLKEYMASK = 0x0001` … `NX_DEVICERCTLKEYMASK = 0x2000`, from IOKit's
`IOLLEvent.h`):

```rust
let now_down = mf & device_flag_bit(kc) != 0;
```

**Symptom in the field:** after rebuilding the release binary, modifiers still don't
work — Control stays stuck on the remote and Tab/Space/Enter type nothing (they
resolve as Ctrl+Tab / Ctrl+Space / Ctrl+Enter against the stuck Control).

**Root cause (candidate, pending runtime confirmation):** reading the device-dependent
bit *alone* (`mf & bit`) is fragile. If those low-16 bits are ever absent or wrong for a
local monitor's `flagsChanged` event, `now_down` is always `false`, every transition
reads as a release, `flags_transition` drops releases for keys not in the held-set
(returns `None`), and **no modifier press or release is ever forwarded**. Consequences:

1. The remote's Control — left stuck by the earlier buggy session — never receives an
   "up", so it stays stuck.
2. New modifier presses do nothing.

Web research (SDL, Chromium, QEMU) indicates the device-dependent bits *should* be
present on current macOS — yet the device-bit-only build (`bc57e32`) demonstrably fails
in the field. Because only Tab/Space/Enter break while letters still type, the monitor is
live and the fault is specifically in the `FlagsChanged` press/release decision. Rather
than bet on which flag layout the OS delivers, the fix reads the **guaranteed-present**
class flag as the primary signal and treats the device bit as a refinement — correct in
either case. A `RUST_LOG=debug` capture (`scripts/diag-keyboard-macos.sh`) confirms which
branch actually fires and closes out the root cause.

**Key evidence:** the *same file* already reads **device-independent** flags —
`NSEventModifierFlags::Control.0` (`0x40000`) and `::Option.0` (`0x80000`), the high
bits — at lines 151–152 to detect the local Ctrl+Alt+G/P shortcuts, and that path
works. So device-independent flags are proven present at runtime; the device-dependent
low bits are the unverified, failing part.

## Design

Determine `now_down` from the device-**independent** modifier *class* flag (guaranteed
present), identifying the specific physical key by `keyCode` (which already
distinguishes left vs right — the kVK→evdev table maps `kVK_Control`→`KEY_LEFTCTRL`,
`kVK_RightControl`→`KEY_RIGHTCTRL`, etc.). Keep the per-key device-dependent bit only as
a *refinement* used when the event actually carries device-dependent data:

```rust
let class_on = mf & class_flag(kc) != 0;                      // any key of this class down?
let now_down = class_on && (mf & 0xffff == 0 || mf & dev_bit != 0);
```

Decision table:

| class flag | low word (device bits) | `now_down` | rationale |
|------------|------------------------|-----------|-----------|
| clear      | —                      | `false`   | no key of this class is down — definitive release |
| set        | all zero               | `true`    | device bits absent → single-key assumption (the common case) |
| set        | non-zero               | `mf & dev_bit != 0` | device bits present → precise per-key (handles both-L-and-R-of-same-modifier holds) |

`flags_transition(held, keycode, now_down)` is **unchanged**: it still forwards only
state-changing transitions and drops no-ops, which is what fixes the original Cmd+Tab
spurious-release bug (a ⌘ release seen with no tracked press → `class_on` is false →
`now_down` false → dropped, not inverted into a phantom press).

### New/changed functions

- **`modifier_class_flag(kvk: u32) -> Option<u32>`** — maps each modifier kVK to its
  device-independent `NSEventModifierFlags` class bit:
  - `0x3B` Control, `0x3E` RightControl → `Control` (`0x40000`)
  - `0x38` Shift, `0x3C` RightShift → `Shift` (`0x20000`)
  - `0x37` Command, `0x36` RightCommand → `Command` (`0x100000`)
  - `0x3A` Option, `0x3D` RightOption → `Option` (`0x80000`)
- **`device_flag_bit`** — retained, used as the refinement term.
- **`flags_transition`** — unchanged.
- The `FlagsChanged` branch computes `now_down` via the table above, then calls
  `flags_transition`. The `debug!` trace is extended to log `class_on` and the low word
  so a runtime capture can confirm which branch fired.

### Why this is robust

It fixes the reported bug whether or not the OS provides device-dependent bits, because
the primary signal (class flag) is proven present. The only degraded case — dropping the
release of one key while the *other* key of the *same* modifier class is still
physically held, *and* device bits are absent — is rare (requires holding both left and
right of the same modifier) and self-heals on focus loss via `release_all()`.

## Recovery of the already-stuck remote

Independent of the code: the remote currently holds Control down from the old session.
With the fixed binary, a single physical **press + release of Left Control** now
forwards a clean Control-up (`class_on` true on press → send down; `class_on` false on
release → send up), clearing it. Same for any other stuck modifier.

## Testing

Unit tests on the pure logic (no AppKit needed), following the existing `#[cfg(test)]`
pattern:

1. `now_down` helper (extracted as a pure fn `modifier_now_down(mf, kvk)`) —
   - device bits **absent**: class flag set → `true`; class flag clear → `false`.
   - device bits **present**: precise per-key; releasing left-of-pair while right held →
     left `false`, right `true`.
2. Full transition sequences via `flags_transition`:
   - Cmd+Tab spurious release dropped (regression guard, keep existing test).
   - normal press/release forwards both.
   - stuck-Control recovery: press then release forwards down then up.
   - device-bits-absent path forwards a normal cycle (the bug this fixes).
3. `modifier_class_flag` mapping table (all eight kVKs + a non-modifier → `None`).

## Verification plan

- `cargo test -p viewer` — all unit tests pass.
- `cargo build --release -p viewer` — release binary rebuilt.
- Runtime (needs the physical keyboard + remote): the retained `debug!` trace lets a
  single `RUST_LOG=debug` capture confirm the correct branch fires and presses/releases
  are forwarded. This is the one step that cannot be verified from the build host alone.

## Out of scope

- CapsLock handling (unchanged — still emitted as a tap).
- `fn`/Globe (no modifier class, still dropped).
- Non-modifier keys, KeyDown/KeyUp paths (unchanged).
