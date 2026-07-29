# macOS parity after the 90-commit merge

Design for the macOS work following merge commit `ebcc1de` (90 upstream Linux commits merged into
the local macOS branch). Audited 2026-07-25 on the target machine (Apple M-series, macOS 26.4,
Homebrew gstreamer 1.28.4 / gtk4 4.22.4 / pkgconf 2.5.1). Every claim below was verified on that
machine, not recalled.

## 1. Starting position

macOS support is **not missing**. The viewer compiles (`cargo check -p viewer` clean, release
build 30.6s cold / 3.7s incremental, 3.4 MB binary), the brew stack needs no environment
variables, per-OS pipeline selection is complete in both the GUI and headless paths, the GPU
unpack shader is numerically exact on the path it covers (`--glunpack-validate 256 144` → max
abs err 0), clipboard round-trips through glib's UTI backend, and the kVK keymap is otherwise
complete and unit-tested.

What broke is **parity**. The merge landed two Linux-developed subsystems — the alacritty
terminal view (`terminal.rs`) and auto pointer-lock (`auto_lock.rs`) — without adapting their
macOS twins, and in doing so made two latent macOS state bugs reachable from ordinary flows.

**Scope decision (owner, 2026-07-25):** the Mac is a **video-only client for now**; headless /
terminal clones are not part of the Mac workflow. Cmd↔Ctrl remap ships **default on**.
Unaccelerated GCMouse deltas, `.app` packaging, CI, and the `~/Library/Application Support`
config move are out.

## 2. The two root causes

### 2.1 Auto pointer-lock can freeze the Mac's cursor process-wide

`main.rs:709-725` reconciles the lock:

```rust
if want {
    if let Some(mw) = windows.borrow().values()
        .find(|w| matches!(w.content, Content::Video(_)) && w.window.is_active())
    { pl.engage(&surface); }          // no active video window → NOTHING HAPPENS
} else if pl.is_engaged() {
    pl.release();
}
```

When `want` is true but no video window is active, the branch falls through and **leaves the lock
engaged**. On Wayland that is harmless: mutter scopes a pointer constraint to surface focus and
deactivates it on blur, which is why the Linux twin needs no lifecycle code at all. On macOS
`CGAssociateMouseAndMouseCursorPosition(false)` (`pointer_lock_macos.rs:158`) is
**process-global and not focus-scoped**.

Before the merge this required the operator to press Ctrl+Alt+G, so it was opt-in. `auto_lock.rs`
now engages unattended for *any* remote app that hides its cursor for ≥180 ms. So: alt-tab out of
a remote game, or have the clone deselected from the web UI, and the Mac's cursor stays frozen in
another app, with the Ctrl+Alt+P escape living on a video window's key controller that is no
longer focused (and, after §2.2, on a keyboard gate that is off).

Two consequences of the same missing lifecycle:

- `reconcile_view` destroys windows (`main.rs:875-880`) and swaps content
  (`teardown_content`, `main.rs:957`) without releasing the lock. `ViewSpec{monitors:[]}` destroys
  every window while the lock is held.
- `engage()` early-returns on `self.engaged` (`pointer_lock_macos.rs:88`) and nothing ever
  reconciles that flag, so once state diverges the lock is silently dead.

### 2.2 The keyboard gate is armed by content that no longer exists

`keyboard_macos.rs` gates key forwarding on `active_windows`, a count of focused video windows.
Its only writer is `connect_is_active_notify` → `note_window_active`, connected inside
`install_keyboard` (`main.rs:1852-1858`), which is called only from `make_video_content`.

Two defects:

- **Never primed.** The counter starts at 0 and only moves on an is-active *transition*. A window
  whose content swaps to video while it is already focused gets no notify, so the counter stays 0
  and the NSEvent monitor passes every key to GTK instead of the remote — the remote keyboard is
  dead until a Cmd-Tab round trip. Reachable on any placeholder→video or terminal→video swap.
- **Handler pile-up.** `teardown_content` (`main.rs:957-967`) removes the key *controller* but
  never disconnects the `is_active_notify` handler. Each `make_video_content` adds another, so
  after N content swaps one focus change runs `release_all_input` N times, and the counter reads N
  instead of 1.

The same defect is what would make terminal typing impossible (keys consumed at
`keyboard_macos.rs:215` and injected at the server as desktop evdev for a clone with no desktop) —
out of scope as a *feature*, but the fix below closes it for free.

## 3. What ships

Six changes, each its own commit, ordered by value.

### 3.1 Scope the pointer lock to focus and window lifetime — `main.rs`

Restructure the reconciler so **absence of an engage target is a release condition**, not a
no-op:

```rust
let target = windows.borrow().values()
    .find(|w| matches!(w.content, Content::Video(_)) && w.window.is_active())
    .and_then(|mw| mw.window.surface());
match (want, target) {
    (true, Some(surface)) => pl.engage(&surface),
    _ if pl.is_engaged()  => pl.release(),
    _ => {}
}
```

Also release before destroying windows and in `teardown_content`. Effect: Cmd-Tab becomes the
recovery gesture instead of the trap, and re-focusing re-engages while `want` holds.

This is behaviour-neutral on Linux — mutter already deactivates on blur, so releasing explicitly
matches what the compositor does — so it is not cfg-gated.

**Escape hatch for testing:** `RMNG_NO_POINTER_LOCK=1` already short-circuits construction
(`pointer_lock_macos.rs:68`). The lock test is run deliberately, second.

### 3.2 Fix the keyboard gate — `main.rs`, `keyboard_macos.rs`

- Prime at install: `keyboard_macos::note_window_active(window.is_active())`.
- Store the `SignalHandlerId` in `VideoContent`; disconnect it in `teardown_content`.
- Log the counter on every transition so `scripts/diag-keyboard-macos.sh` can prove it reaches
  exactly 0 with no video focused and ≥1 with one focused.

### 3.3 Cmd↔Ctrl swap, default on — `keyboard_macos.rs`, `config.rs`

A **true swap**, so Super remains reachable: Cmd → `KEY_LEFTCTRL`/`KEY_RIGHTCTRL`, physical
Control → `KEY_LEFTMETA`/`KEY_RIGHTMETA`. A one-way Cmd→Ctrl map would leave no key producing
Super at all.

Implemented as a remap layer applied **after** `kvk_evdev::translate` and before the held-set
insert, in all three monitor paths (KeyDown / KeyUp / FlagsChanged). `kvk_evdev.rs` stays a pure
physical-identity table with its tests intact. The held-set stores the *remapped* code so presses
and releases pair correctly.

No interaction with local chords: F11 / Ctrl+Alt+G / Ctrl+Alt+P are detected from raw
`NSEvent.modifierFlags` (`keyboard_macos.rs:192-194`) and GDK modifier state, neither of which
this layer touches — they stay on the physical keys.

Control: `cmd_is_ctrl: bool` in `config.rs`, default `true`, with an `RMNG_CMD_IS_CTRL=0`
override. No Settings-dialog checkbox in this pass (the dialog holds only the server address).

### 3.4 Two kVK table corrections — `kvk_evdev.rs`

| kVK | now | correct | why |
| --- | --- | --- | --- |
| `0x72` | `138` `KEY_HELP` | `110` `KEY_INSERT` | On any PC keyboard attached to a Mac this is Insert; Chromium's `dom_code_data.inc` maps kVK `0x72` → `INSERT`. Currently Shift+Insert / Ctrl+Insert / vi-style paste are unreachable. |
| `0x6E` | `U` (unassigned, swallowed) | `127` `KEY_COMPOSE` | Apple's `HIToolbox/Events.h` on this machine defines `kVK_ContextualMenu = 0x6E` — the PC Menu key. Linux delivers that key as `KEY_COMPOSE` (127), not `KEY_MENU`. |

Update the unit tests and the "unassigned" list in the header comment (`kvk_evdev.rs:26`).

### 3.5 Terminal font safety net — `terminal.rs`

Not the terminal feature work (deferred, §4) — a **correctness guard**, because the current code
fails silently and the failure is not macOS-specific in principle.

Verified on this machine: brew installs `gsettings-desktop-schemas` as a GStreamer dependency, so
`interface_settings()` returns `Some` on macOS and yields `'Adwaita Mono 11'` — a font that is not
installed. The `fd.family().is_some()` guard at `terminal.rs:80` cannot catch this, because the
descriptor *has* a family name; it simply does not resolve. Pango falls back to a proportional
face:

| requested | w("iiiiiiiiii") | w("MMMMMMMMMM") | |
| --- | --- | --- | --- |
| `Adwaita Mono 11` — what macOS gets | 50 | 140 | proportional |
| `Menlo 11` | 110 | 110 | monospace |
| `Monospace 11` — the fallback never reached | 110 | 110 | monospace |

Fix: resolve the descriptor through a real pango context and require the *resolved* family to be
monospace; otherwise fall back (`Menlo` on macOS, `Monospace` elsewhere). This also protects Linux
from a `monospace-font-name` naming an uninstalled family.

### 3.6 Documentation and three lying comments

- `docs/DEVELOPMENT.md`: a macOS (Apple Silicon) section — brew deps with the versions verified
  here, `cargo build -p viewer --release` (**not** root `cargo build`, which dies in `libspa-sys`
  because `clone-daemon` depends on pipewire unconditionally and the workspace has no
  `default-members`), and the two-GTK-provider hazard. Documenting `-p viewer` rather than adding
  `default-members` deliberately: that would change what root `cargo build` does for the upstream
  Linux developer.
- `crates/viewer/README.md`: a per-OS backend table. It currently documents VA-API + dmabuf +
  Wayland only, contradicting five shipped macOS modules.
- macOS input notes: Cmd↔Ctrl default, F-row needs `fn` on a MacBook, Cmd-Tab / Cmd-Space cannot
  be forwarded, and `grab_keys()` is a verified no-op on macOS (GTK 4.22.4's
  `inhibit_system_shortcuts` is Wayland-only).
- Comment fixes: the `GSK_RENDERER` pin (`main.rs:103-115`) explains a stale-frame bug and pins
  `gl` on Linux, without noting that **macOS runs the affected renderer with no pin available** —
  the legacy renderer was removed in GTK 4.18 — and that `GSK_RENDERER=cairo` is the manual
  escape; `--glunpack-validate`'s doc comment (`glunpack.rs:648-650`) claims to compile the
  rectangle variant when it only exercises the 2D path, which is the *opposite* of what macOS
  uses in Yuv444.

## 4. Deliberately deferred

Recorded with locations so they are recoverable, not rediscovered.

**Terminal view (video-only scope).** Cell metrics: `GdkMacosDisplay` pins `gtk-xft-dpi` to
72·1024, so an 11 pt font renders ~25% smaller and the `text-scaling-factor` watcher
(`terminal.rs:246`) is inert — a 1280px window claims ~256 columns and resizes the real tmux PTYs
wrongly (`terminal.rs:274-289`). Trackpad scroll treats Surface (pixel) deltas as wheel notches,
`steps = ceil(|dy|)*3`, with no `DISCRETE` flag and no accumulator, which machine-guns the remote
session while vim/less is on the alt screen (`terminal.rs:888`). No Cmd+C/Cmd+V — GDK maps Command
to META, which `encode_key` ignores (`terminal.rs:713`, `1173-1174`). Dark Mode is never detected:
`is_dark()` reads the widget foreground and the only setter is the XDG-portal-only
`follow_system_color_scheme()` (`terminal.rs:326`, `main.rs:501-530`).

**Out of scope by owner decision.** Unaccelerated GCMouse deltas
(`pointer_lock_macos.rs:107` sends OS-accelerated `NSEvent.deltaX/Y` while `docs/PROTOCOL.md`
specifies unaccelerated and the Linux twin sends `dx_unaccel`). `.app` bundling, Info.plist
identity, codesigning, notarisation. `~/Library/Application Support` config move — `~/.config`
round-trips today. GitHub Actions macOS gate.

**Judged not worth the effort yet.** The GSK stale-texture mitigation beyond the comment fix: the
symptom has never actually been observed on a Mac, and both candidate fixes (a `cairo` renderer
default, or a `GtkGLArea` + `appsink max-buffers=1 drop=true` path) trade real performance for a
hypothetical. Extending `--glunpack-validate` to the rectangle path needs a rectangle-texture
source built by hand; only worth it if Yuv444 is ever enabled from this Mac.

**Refuted by the audit** — not real macOS gaps, listed so they are not re-filed: the
`.expect("build decoder")` panic and the log-only GStreamer bus handler (both shared,
byte-identical pre-merge Linux code); missing `--help`/`--version` (platform-agnostic); Escape /
ForwardDelete supposedly missing from the swallowed-key allowlist (they are not swallowed);
CapsLock emitting a tap per FlagsChanged (premise about macOS event delivery is wrong);
`set_tcp_user_timeout` being Linux-gated (correct as-is — Darwin's keepalive teardown is
receive-idle-based); the `rmng` CLI being unusable from macOS (it builds clean and is
HTTP-only).

## 5. Verification

Split by what can actually be proven without a human at the keyboard.

**Self-verifiable (done before handoff).** `cargo build -p viewer --release`;
`cargo test -p viewer` (existing `kvk_evdev` + `auto_lock` suites, plus new unit tests for the
Cmd↔Ctrl remap layer covering press/release pairing through the held-set, and for the
font-resolution guard); `--glunpack-validate 256 144` and `2560 1440` still at max abs err 0;
`cargo check -p viewer` on the untouched Linux paths.

**Requires the operator** — an updated `run-macos-test.sh` with `RUST_LOG=debug` to
`/tmp/rmng_viewer_gui.log` and a per-item checklist:

1. **Keyboard gate.** Type into a remote editor immediately after the viewer connects, with no
   Cmd-Tab first. Log must show the counter at ≥1 and keys forwarding. This is the §2.2 priming
   fix.
2. **Cmd↔Ctrl.** Cmd+C / Cmd+T / Cmd+A behave as Ctrl in the remote session; physical Control
   produces Super (GNOME overview); Ctrl+Alt+G and F11 still work as local chords.
3. **Insert / Menu keys.** Only if a PC keyboard is attached — Shift+Insert pastes in the remote.
4. **Pointer lock, run second and deliberately.** Engage in a cursor-hiding remote app, then
   Cmd-Tab away: the Mac cursor must unfreeze and reappear. Cmd-Tab back: the lock re-engages.
   Then deselect the clone from the web UI while locked — the cursor must be released as the
   windows are destroyed. `RMNG_NO_POINTER_LOCK=1` is the abort switch.
5. **No regressions** in video, resize, trackpad scroll on the desktop, clipboard both ways.

## 6. Invariants — unchanged by this work

- The wire protocol: framing, tags, evdev codes on the wire, AVC444 packing.
- `sync=false` + `appsrc do-timestamp=true` on all pipelines; no
  `glcolorconvert`/`videoconvert` between decoder and `rmngavc444unpack` in Yuv444.
- The `WRITE|GL` output map and VAO bind in `glunpack.rs`.
- Linux behaviour throughout. Only §3.1 touches a shared code path, and it aligns explicit
  release with what mutter already does implicitly.
- No server-side change. Nothing in this plan requires touching the Linux control-server.
