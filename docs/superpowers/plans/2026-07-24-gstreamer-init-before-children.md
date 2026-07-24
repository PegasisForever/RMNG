# GStreamer Init Before Children Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Ensure GStreamer (`media::init`) finishes before any background supervisor that can spawn a child process, so control-server boot cannot hang with ports 9000/9001/9005 unbound.

**Architecture:** Split `mediaplane::spawn` into `mediaplane::init() -> MediaInit` (runs `media::init` once) and `mediaplane::spawn(app, init)` (binds listeners only if enabled). Drive the late-boot sequence through a tiny `boot::run_late_boot` helper that always runs init → background spawns → media listeners, so a unit test can assert order without a GPU.

**Tech Stack:** Rust (edition 2024), existing `media` crate / GStreamer, tokio, control-server unit tests.

**Reference spec:** `docs/superpowers/specs/2026-07-24-gstreamer-init-before-children-design.md`.

## Global Constraints

- **Source + tests only** — do not publish images or redeploy CT 105.
- **Non-fatal media init** — failure logs `media init failed; port 1 disabled: …` and continues boot (web API stays up); same message string as today.
- **No CLOEXEC / pre_exec changes** on cliproxy/smb/ssh — ordering is the fix.
- **Do not call `media::init` twice** — only inside `mediaplane::init`.
- **Commit only when the user asks** — plan steps may stage commits; skip `git commit` unless explicitly requested in the session.

---

## File Structure

**Created:**
- `crates/control-server/src/boot.rs` — `run_late_boot` ordering helper + unit test for init-before-background.

**Modified:**
- `crates/control-server/src/mediaplane.rs` — add `MediaInit`, `pub fn init() -> MediaInit`; change `spawn(app, init)`.
- `crates/control-server/src/main.rs` — `mod boot`; call `run_late_boot` for the late-boot section (after token migration, through `mediaplane::spawn`).

---

### Task 1: Boot-order helper + failing regression test

**Files:**
- Create: `crates/control-server/src/boot.rs`
- Modify: `crates/control-server/src/main.rs` (add `mod boot;` only in this task — wiring comes in Task 3)

**Interfaces:**
- Produces:
  ```rust
  pub fn run_late_boot<T, FInit, FBg, FMedia>(
      init_media: FInit,
      spawn_background: FBg,
      spawn_media_listeners: FMedia,
  ) where
      FInit: FnOnce() -> T,
      FBg: FnOnce(),
      FMedia: FnOnce(T);
  ```

- [ ] **Step 1: Create `boot.rs` with the helper and a failing-order-sensitive test**

Create `crates/control-server/src/boot.rs`:

```rust
//! Late-boot sequencing for the control-server.
//!
//! GStreamer plugin scanning opens private pipes. Any child spawned while that
//! scan is in flight (cliproxy / smbd / sshd) can inherit a write end and prevent
//! EOF, hanging `media::init` forever and leaving ports 9000/9001/9005 unbound.
//! This helper encodes the required order: finish media init, then spawn
//! background supervisors, then start media listeners.

/// Run the post-setup late-boot sequence in the only safe order.
///
/// `init_media` must complete (including any GStreamer registry scan) before
/// `spawn_background` runs, because background tasks may fork children that
/// inherit open FDs. `spawn_media_listeners` receives the token from
/// `init_media` and starts video / forward / clone-socket listeners.
pub fn run_late_boot<T, FInit, FBg, FMedia>(
    init_media: FInit,
    spawn_background: FBg,
    spawn_media_listeners: FMedia,
) where
    FInit: FnOnce() -> T,
    FBg: FnOnce(),
    FMedia: FnOnce(T),
{
    let token = init_media();
    spawn_background();
    spawn_media_listeners(token);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    #[test]
    fn media_init_runs_before_background_and_listeners() {
        let order = Arc::new(Mutex::new(Vec::new()));
        let o1 = order.clone();
        let o2 = order.clone();
        let o3 = order.clone();

        run_late_boot(
            || {
                o1.lock().unwrap().push("init");
                "token"
            },
            || o2.lock().unwrap().push("background"),
            |token| {
                assert_eq!(token, "token");
                o3.lock().unwrap().push("listeners");
            },
        );

        assert_eq!(
            *order.lock().unwrap(),
            vec!["init", "background", "listeners"]
        );
    }
}
```

- [ ] **Step 2: Register the module in `main.rs`**

Near the other `mod` lines at the top of `crates/control-server/src/main.rs`, add:

```rust
mod boot;
```

Do **not** call `boot::run_late_boot` yet — that is Task 3.

- [ ] **Step 3: Run the new test**

Run: `cargo test -p control-server boot::tests::media_init_runs_before_background_and_listeners -- --nocapture`

Expected: PASS (the helper itself is correct; Task 3 wires `main` to use it so production cannot regress).

Note: This test alone does not yet force `main.rs` to use the helper. Task 3 makes `main` the sole caller of this ordering; any future reorder must edit `run_late_boot` or stop using it (reviewable). If you want a stronger compile-time gate later, that is out of scope.

- [ ] **Step 4: Commit (only if the user asked to commit)**

```bash
git add crates/control-server/src/boot.rs crates/control-server/src/main.rs
git commit -m "$(cat <<'EOF'
test(control-server): encode late-boot media-init-before-children order

EOF
)"
```

---

### Task 2: Split `mediaplane::init` from `spawn`

**Files:**
- Modify: `crates/control-server/src/mediaplane.rs` (around `pub fn spawn`, currently ~208–216)

**Interfaces:**
- Produces:
  ```rust
  /// Proof that [`init`] ran. Opaque so callers cannot invent a fake “enabled” flag.
  pub struct MediaInit { enabled: bool }

  pub fn init() -> MediaInit;

  pub fn spawn(app: App, init: MediaInit);
  ```
- Consumes: existing `media::init()` from the `media` crate.

- [ ] **Step 1: Add a small token-state unit test**

In `crates/control-server/src/mediaplane.rs`, inside the existing `#[cfg(test)] mod tests` block, add (after `MediaInit` exists — if writing test-first, temporarily stub `MediaInit` with `enabled: bool` and `enabled_for_test`):

```rust
#[test]
fn media_init_disabled_token_is_distinguishable() {
    let disabled = MediaInit { enabled: false };
    assert!(!disabled.enabled_for_test());
}
```

- [ ] **Step 2: Implement `MediaInit` + `init` + change `spawn`**

Replace the start of `pub fn spawn` in `mediaplane.rs` so GStreamer init is no longer inside `spawn`.

Add **above** `pub fn spawn`:

```rust
/// Proof that [`init`] completed. Opaque so callers must go through [`init`]
/// rather than inventing an “enabled” boolean.
pub struct MediaInit {
    enabled: bool,
}

impl MediaInit {
    #[cfg(test)]
    pub(crate) fn enabled_for_test(&self) -> bool {
        self.enabled
    }
}

/// Initialize GStreamer (and related media env) once. Must run **before** any
/// background task that can spawn a child process — inherited scanner pipes
/// otherwise hang init forever (ports never bind).
///
/// Failure is non-fatal: returns a disabled token so the web API still boots.
pub fn init() -> MediaInit {
    match media::init() {
        Ok(()) => MediaInit { enabled: true },
        Err(e) => {
            tracing::error!("media init failed; port 1 disabled: {e}");
            MediaInit { enabled: false }
        }
    }
}
```

Change the signature and body start of `spawn` from:

```rust
pub fn spawn(app: App) {
    let rt_handle = tokio::runtime::Handle::current();
    if let Err(e) = media::init() {
        tracing::error!("media init failed; port 1 disabled: {e}");
        return;
    }
    let cfg = app.config();
    // ...
```

to:

```rust
pub fn spawn(app: App, init: MediaInit) {
    if !init.enabled {
        return;
    }
    // `spawn` is called from the async `main`, so a tokio runtime exists here; capture its
    // handle so the std-thread media plane can `block_on` async control-plane calls
    // (`App::dial_host`) from the forward-data serve threads.
    let rt_handle = tokio::runtime::Handle::current();
    let cfg = app.config();
    // ... rest unchanged ...
```

Leave the rest of `spawn` (listener threads, clone socket, etc.) unchanged.

- [ ] **Step 3: Confirm the crate still compiles except for `main` call sites**

Run: `cargo build -p control-server 2>&1 | head -40`

Expected: FAIL with something like `this function takes 2 arguments but 1 argument was supplied` at `mediaplane::spawn(app.clone())` in `main.rs`. That proves the signature change landed.

- [ ] **Step 4: Commit (only if the user asked to commit)**

```bash
git add crates/control-server/src/mediaplane.rs
git commit -m "$(cat <<'EOF'
refactor(mediaplane): split GStreamer init from listener spawn

EOF
)"
```

---

### Task 3: Wire `main` through `run_late_boot`

**Files:**
- Modify: `crates/control-server/src/main.rs` (late-boot section after token migration, ~205–232)

**Interfaces:**
- Consumes: `boot::run_late_boot`, `mediaplane::init`, `mediaplane::spawn(app, MediaInit)`
- Produces: production boot order init → background → listeners → `web::serve`

- [ ] **Step 1: Replace the late-boot block with `run_late_boot`**

In `main.rs`, after the legacy token migration `catch_unwind` block, replace the sequence from the “Background loops” comment through `mediaplane::spawn(app.clone());` with:

```rust
    // GStreamer init MUST finish before cliproxy/smb/ssh (and any other child
    // spawners). Those supervisors otherwise inherit gst-plugin-scanner pipes and
    // hang media init forever — web/video/forward never bind. See
    // docs/superpowers/specs/2026-07-24-gstreamer-init-before-children-design.md.
    let app_for_bg = app.clone();
    let app_for_media = app.clone();
    boot::run_late_boot(
        mediaplane::init,
        move || {
            // Background loops: the per-host agent-state monitor poller, the clone-home reconciler
            // (the Docker-port successor to the Proxmox-era sshfs mount loop — it symlinks
            // data/hosts/<id> → /proc/<uid-1000-pid>/root/home/rmng so every clone's home is browsable
            // in one place; needs the container's `pid: "host"`), the smbd supervisor that serves that
            // same directory as the `clones` SMB share (port 445), so the homes are browsable over
            // `smb://<host>/clones` too, and the /dev/shm reconciler that keeps each running clone's
            // shared memory at LXC parity (~50% of RAM) so Chromium/Electron apps don't exhaust
            // Docker's 64 MB default (also needs `pid: "host"`). Claude/Codex account usage is polled
            // by-group via `cliproxy::run_usage_poller` (below), which owns all account display now.
            tokio::spawn(monitor::run(app_for_bg.clone()));
            tokio::spawn(clone_reconcile::run(app_for_bg.clone()));
            tokio::spawn(homes::run(app_for_bg.clone()));
            tokio::spawn(shm::run(app_for_bg.clone()));
            tokio::spawn(buildinfra::run(app_for_bg.clone()));
            tokio::spawn(smb::run(app_for_bg.clone()));
            tokio::spawn(ssh::run(app_for_bg.clone()));
            // Group-proxy supervisor: one CLIProxyAPI instance per account group.
            tokio::spawn(cliproxy::run(app_for_bg.clone()));
            // By-group usage poller: reads each instance's auth-dir tokens and publishes
            // `ControlState.usage_groups` (the old flat claude_accounts pollers stay running).
            tokio::spawn(cliproxy::run_usage_poller(app_for_bg.clone()));
            tokio::spawn(app_for_bg.tokens.clone().run_persister());
            tokio::spawn(app_for_bg.tokens.clone().run_fable_ticker());
        },
        move |media_init| {
            // Port 1 (video) — ingest clone dmabufs, VA-API encode, serve the viewer.
            mediaplane::spawn(app_for_media, media_init);
        },
    );

    web::serve(app).await
```

Keep `web::serve(app).await` outside `run_late_boot` (it must remain the last awaited call in `main`).

- [ ] **Step 2: Build and run focused tests**

Run:

```bash
cargo test -p control-server boot::tests::media_init_runs_before_background_and_listeners -- --nocapture
cargo build -p control-server
```

Expected: both succeed (build may take a while; no missing-arg errors on `spawn`).

- [ ] **Step 3: Run a broader control-server test slice (no GPU required)**

Run:

```bash
cargo test -p control-server 2>&1 | tail -60
```

Expected: all tests PASS (or pre-existing ignored tests only).

- [ ] **Step 4: Commit (only if the user asked to commit)**

```bash
git add crates/control-server/src/main.rs crates/control-server/src/boot.rs crates/control-server/src/mediaplane.rs
git commit -m "$(cat <<'EOF'
fix(control-server): init GStreamer before child-spawning supervisors

EOF
)"
```

---

## Spec coverage checklist

| Spec requirement | Task |
|---|---|
| Split `init` / `spawn` with opaque `MediaInit` | Task 2 |
| Call init before child-spawning `tokio::spawn`s | Task 3 |
| Keep listeners after supervisors are scheduled | Task 3 (`spawn` still last before `web::serve`) |
| Non-fatal init failure, same log message | Task 2 |
| Regression test for ordering | Task 1 |
| No publish / redeploy | Global constraint |
| No CLOEXEC as primary fix | Global constraint |

## Self-review notes

- No TBD/placeholder steps.
- `MediaInit` / `run_late_boot` / `spawn(app, init)` names consistent across tasks.
- Commit steps are optional per session policy.
