# GStreamer init before child spawns — design

Date: 2026-07-24
Status: approved design, ready for implementation planning

## Summary

Fix a control-server boot hang where `gstreamer::init()` never finishes and
ports 9000 / 9001 / 9005 never bind. Root cause: background supervisors
(`cliproxy`, and similarly `smb` / `ssh`) spawn child processes while
`mediaplane::spawn` is still inside `media::init()`. Those children inherit
GStreamer's private plugin-scanner pipes; an inherited write end prevents EOF,
so the scanner (and therefore `media::init`) blocks forever.

The fix is ordering: complete GStreamer initialization **before** any
background task that can spawn a child process.

### Out of scope

- Publishing / redeploying CT 105 (source + tests only).
- Disabling GStreamer's external `gst-plugin-scanner`.
- Closing FDs in every child spawn site (`CLOEXEC` / `pre_exec`) as the primary
  fix — ordering is sufficient and cheaper.
- Changing cliproxy / smb / ssh supervision behavior beyond boot order.

## Incident evidence (production CT 105)

1. Self-update succeeded: container recreated, handoff reconciled
   (`update complete`).
2. Process stayed up, but `curl :9000` got connection reset / refused — no web
   listener.
3. `gst-plugin-scanner` was a live child of `rmng-control-server`, stuck in
   `ppoll` waiting on its control pipes.
4. A `cliproxy-sidecar` held the write end of the parent's scanner result pipe.
5. `SIGTERM` of that sidecar closed the leaked FD; GStreamer finished; video /
   forward / web listeners bound immediately; clones reconnected.

## Current boot order (broken)

In `crates/control-server/src/main.rs`:

1. Config / state / Docker self-setup / reconcile.
2. `tokio::spawn` of monitor, clone_reconcile, homes, shm, buildinfra, **smb**,
   **ssh**, **cliproxy**, usage poller, token tasks.
3. `mediaplane::spawn(app)` — which **synchronously** calls `media::init()`
   (`gstreamer::init()`), then binds listeners.
4. `web::serve(app).await`.

Steps 2 and 3 race. Cliproxy / smbd / sshd can spawn (and inherit FDs) while
step 3 still holds open scanner pipes.

## Design

### 1. Split media init from listener startup

In `crates/control-server/src/mediaplane.rs`:

- Add `pub fn init() -> MediaInit` that calls `media::init()` once and returns a
  token proving it ran. On failure: log the existing error
  (`media init failed; port 1 disabled`) and return a token that marks media
  **disabled** (same non-fatal posture as today).
- Change `pub fn spawn(app: App, init: MediaInit)` so it **does not** call
  `media::init()` again. If the token says disabled, return without binding
  video / forward / clone-socket listeners. Otherwise start the existing
  listener threads / loops.

`MediaInit` is a small opaque (or `pub(crate)`) token — not a boolean callers
invent — so `spawn` cannot be called without going through `init`.

### 2. Call `mediaplane::init()` before child-spawning tasks

In `main.rs`, after legacy token migration and **before** the block of
`tokio::spawn(...)` that starts smb / ssh / cliproxy (and the other background
loops), call:

```rust
let media_init = mediaplane::init();
```

Keep `mediaplane::spawn(app.clone(), media_init)` where `spawn` sits today
(just before `web::serve`), so listeners still start after supervisors are
scheduled — only the GStreamer work moves earlier.

### 3. Regression test

Add a unit / integration-style test in the control-server crate that encodes
the ordering invariant without requiring a GPU:

- Prefer a structural / compile-time-friendly check if practical (e.g. a small
  `boot` helper that takes hooks and asserts `media_init` runs before any
  `spawn_child` hook).
- Otherwise: extract the late-boot sequence into a testable function with
  injectable “init media” / “spawn background” / “spawn mediaplane listeners”
  steps and assert call order.

The test must fail if someone reorders `tokio::spawn(cliproxy/smb/ssh/…)`
before `mediaplane::init()`.

### 4. Error handling

Unchanged semantics:

- `media::init()` failure → log, disable port 1 / media plane, continue boot so
  the web API / setup wizard remain reachable.
- Success → token enables full `spawn` behavior.

## Testing plan

- Unit test for boot-order invariant (must fail if order is wrong).
- Existing control-server / media tests still pass.
- Manual verification optional (out of scope for deploy): after a restart,
  logs show video + web listeners within seconds, no stuck
  `gst-plugin-scanner`.

## Success criteria

- No child-spawning supervisor starts until GStreamer init has returned.
- Web / video / forward bind even when cliproxy / smb / ssh start on the same
  boot.
- Media init failure remains non-fatal.
