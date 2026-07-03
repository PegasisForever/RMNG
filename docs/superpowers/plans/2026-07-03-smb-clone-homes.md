# SMB Share for Clone Homes — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Serve every clone's `/home/rmng` over a single read-write SMB share (`smb://<host>/clones`) from inside the control-server container, replacing the sshfs/sftp browsing method.

**Architecture:** A new `smb.rs` module writes a static `smb.conf`, provisions a fixed `rmng`/`rmng` SMB credential, and supervises a child `smbd` (foreground, restart-on-exit) — the control-server binary stays the container's sole ENTRYPOINT. The share root is the existing `data/hosts` symlink directory. To let `smbd` (acting as uid 1000 via `force user`) follow the `/proc/<pid>/root` symlinks, the `homes.rs` reconciler is changed to target a **uid-1000** process's proc-root instead of the clone's root-owned init.

**Tech Stack:** Rust (edition 2024, tokio "full", anyhow), Samba (`smbd`), Docker / docker compose, Proxmox LXC (for the E2E).

**Reference spec:** `docs/superpowers/specs/2026-07-03-smb-clone-homes-design.md`.

## Global Constraints

- **Rust edition 2024, rust-version 1.85** — matches the workspace `Cargo.toml`.
- **No-env-settings invariant** — config lives in `config.json`; never add `-e`/env-var settings. The SMB credential is a compile-time constant (a deliberate "fixed built-in credential" per the spec), not env config. The one reserved env var is `RUST_LOG`.
- **`bollard` is the only Docker client** from the server — never shell out to the `docker` CLI from the running server. (The `smbd`/`smbpasswd`/`useradd` child processes in this feature are local system tools, not Docker calls — allowed.)
- **Fixed credential:** SMB user `rmng`, password `rmng`. Same on every deployment.
- **Port:** SMB on **445** (`-p 445:445`); host 445 must be free.
- **Clone uid:** the clone user `rmng` is **uid 1000** (`docker::CLONE_USER`); clones are not userns-remapped, so uid 1000 is consistent across clone/host/control-server.
- **Share path is derived from config,** not hardcoded: `<cwd>/<config.data_dir>/hosts`. With WORKDIR `/data` and `data_dir` default `"data"`, that resolves to `/data/data/hosts` (matches `homes.rs` and the host volume path `…/rmng-data/_data/data/hosts`).
- **Commit trailer** — end every commit message with:
  ```
  Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>
  ```
- **Verify loop** (Rust-only feature): `cargo test -p control-server && cargo build -p control-server`.
- **Branch:** commit on `port-forwarding` (the current branch), local only — do not push (per the brainstorm decision).

---

## File Structure

**Created:**
- `crates/control-server/src/smb.rs` — `render_smb_conf` (pure), account provisioning, `smbd` supervisor, `run`.

**Modified:**
- `crates/control-server/src/main.rs` — `mod smb;` + `tokio::spawn(smb::run(app.clone()))`.
- `crates/control-server/src/homes.rs` — retarget the browse symlink at a uid-1000 pid (`pick_home_pid` pure + `home_pid`/`proc_uid`/`mnt_ns_ino` wrappers); update module doc.
- `Dockerfile` — install `samba`; create `rmng` uid 1000; `EXPOSE … 445`; update the "no SSH/sshfs" comments.
- `compose.yaml` — publish `445:445`; update the one-liner comment.
- `docs/DEPLOY.md` — rewrite the "Browsing clone homes" section: drop sshfs, add SMB.

---

## Task 1: SMB config renderer

**Files:**
- Create: `crates/control-server/src/smb.rs`
- Modify: `crates/control-server/src/main.rs` (add `mod smb;`)
- Test: inline `#[cfg(test)]` in `smb.rs`

**Interfaces:**
- Produces: `pub fn render_smb_conf(hosts_path: &str) -> String`; module constants `SMB_USER`, `SMB_PASS`, `SMB_CONF_PATH`.

- [ ] **Step 1: Declare the module** — add `mod smb;` to `crates/control-server/src/main.rs` in the alphabetical `mod` block (between `mod provision;` and `mod state;`):

```rust
mod provision;
mod smb;
mod state;
```

- [ ] **Step 2: Write the failing test** — create `crates/control-server/src/smb.rs` with only the renderer signature + test:

```rust
//! Port-445 SMB share of the clone homes (`data/hosts`). smbd runs as a child of the
//! control-server (the single ENTRYPOINT), serving one read-write share whose root is the
//! list of clone ids. Fixed built-in credential (rmng/rmng). See the design spec.

/// The SMB (and matching unix) account the share authenticates + acts as. uid 1000 so
/// created files match the clone's `rmng` user.
const SMB_USER: &str = "rmng";
/// Fixed built-in password (per spec — same on every deployment).
const SMB_PASS: &str = "rmng";
/// Where the generated config is written; `smbd --configfile` points here.
const SMB_CONF_PATH: &str = "/etc/samba/smb.conf";

/// Render the static Samba config. `hosts_path` is the absolute share root (the
/// `data/hosts` symlink dir). Pure, so it's unit-testable.
#[allow(dead_code)] // used by `run` (Task 3); until then only the test consumes it
pub fn render_smb_conf(hosts_path: &str) -> String {
    format!(
"[global]
   workgroup = WORKGROUP
   server min protocol = SMB2
   unix extensions = no
   allow insecure wide links = yes
   security = user
   smb ports = 445
   load printers = no
   printing = bsd
   printcap name = /dev/null
   disable spoolss = yes
   vfs objects = catia fruit
   logging = stdout
   log level = 1
   passdb backend = tdbsam

[clones]
   comment = RMNG clone homes
   path = {hosts_path}
   browseable = yes
   read only = no
   wide links = yes
   follow symlinks = yes
   force user = {user}
   force group = {user}
   valid users = {user}
   create mask = 0644
   directory mask = 0755
"
    , hosts_path = hosts_path, user = SMB_USER)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn render_includes_load_bearing_lines() {
        let c = render_smb_conf("/data/data/hosts");
        for needle in [
            "path = /data/data/hosts",
            "wide links = yes",
            "unix extensions = no",
            "allow insecure wide links = yes",
            "force user = rmng",
            "server min protocol = SMB2",
        ] {
            assert!(c.contains(needle), "smb.conf missing `{needle}`\n---\n{c}");
        }
    }
}
```

- [ ] **Step 3: Run the test to verify it passes** (renderer is complete):

Run: `cargo test -p control-server smb::tests::render_includes_load_bearing_lines`
Expected: PASS (1 test). A transient `dead_code` warning on `SMB_PASS`/`SMB_CONF_PATH` is expected — Task 3 consumes them.

- [ ] **Step 4: Commit**

```bash
git add crates/control-server/src/smb.rs crates/control-server/src/main.rs
git commit -m "feat(control-server): smb.conf renderer for clone-homes share

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

## Task 2: Retarget the browse symlink at a uid-1000 pid

**Why:** `smbd` serves as uid 1000 (`force user = rmng`). The reconciler currently links `data/hosts/<id>` → the clone's **root-owned main pid** proc-root, which uid 1000 can't follow (ptrace perms). Point it at a uid-1000 process in the same clone instead — same rootfs, but followable by uid 1000. Transparent to the host-side / `docker exec` consumers (same filesystem).

**Files:**
- Modify: `crates/control-server/src/homes.rs`
- Test: inline `#[cfg(test)]` in `homes.rs`

**Interfaces:**
- Produces: `fn pick_home_pid(target_mnt_ino: u64, candidates: &[(i64, u32, u64)]) -> Option<i64>` (pure); `fn home_pid(main_pid: i64) -> Option<i64>`.
- Consumes: existing `clone_home(pid)`, `ensure_symlink`, `reconcile`.

- [ ] **Step 1: Write the failing test** — add to the `#[cfg(test)] mod tests` block in `crates/control-server/src/homes.rs`:

```rust
    #[test]
    fn pick_home_pid_wants_uid1000_in_target_ns() {
        let target = 42u64; // the clone's mount-namespace inode
        // (pid, uid, mnt_ns_ino): root init in target ns, a uid-1000 session in target ns,
        // and a uid-1000 process in a DIFFERENT ns (another clone) that must be ignored.
        let cands = [(1i64, 0u32, 42u64), (37, 1000, 42), (99, 1000, 7)];
        assert_eq!(pick_home_pid(target, &cands), Some(37));
        // Clone still booting — no uid-1000 process in its ns yet → None.
        assert_eq!(pick_home_pid(target, &[(1i64, 0u32, 42u64)]), None);
    }
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p control-server homes::tests::pick_home_pid_wants_uid1000_in_target_ns`
Expected: FAIL — `cannot find function pick_home_pid`.

- [ ] **Step 3: Add the selection logic** — insert near the top of `homes.rs` (after the `RECONCILE_INTERVAL` const):

```rust
/// The clone user's uid (see `docker::CLONE_USER`). The SMB share acts as this uid, so the
/// browse link must point at a uid-1000 process's proc-root.
const CLONE_UID: u32 = 1000;

/// From `(pid, uid, mnt_ns_ino)` triples, pick a pid in the clone's mount namespace
/// (`target_mnt_ino`) that runs as the clone user. Pure, so it's unit-testable.
fn pick_home_pid(target_mnt_ino: u64, candidates: &[(i64, u32, u64)]) -> Option<i64> {
    candidates
        .iter()
        .find(|(_, uid, ino)| *uid == CLONE_UID && *ino == target_mnt_ino)
        .map(|(pid, _, _)| *pid)
}

/// Real uid from `/proc/<pid>/status` (first field of the `Uid:` line).
fn proc_uid(pid: i64) -> Option<u32> {
    let status = std::fs::read_to_string(format!("/proc/{pid}/status")).ok()?;
    let line = status.lines().find(|l| l.starts_with("Uid:"))?;
    line.split_whitespace().nth(1)?.parse().ok()
}

/// Inode of `/proc/<pid>/ns/mnt` — identical for every process in one mount namespace
/// (i.e. one clone container). `None` if unreadable.
fn mnt_ns_ino(pid: i64) -> Option<u64> {
    use std::os::unix::fs::MetadataExt;
    std::fs::metadata(format!("/proc/{pid}/ns/mnt")).ok().map(|m| m.ino())
}

/// A uid-1000 pid in the same mount namespace as the clone's root-owned main `pid`. Scans
/// /proc once. `None` while the clone has no uid-1000 session yet (still booting).
fn home_pid(main_pid: i64) -> Option<i64> {
    let target = mnt_ns_ino(main_pid)?;
    let mut candidates: Vec<(i64, u32, u64)> = Vec::new();
    for entry in std::fs::read_dir("/proc").ok()?.flatten() {
        let Ok(pid) = entry.file_name().to_string_lossy().parse::<i64>() else { continue };
        if let (Some(uid), Some(ino)) = (proc_uid(pid), mnt_ns_ino(pid)) {
            candidates.push((pid, uid, ino));
        }
    }
    pick_home_pid(target, &candidates)
}
```

- [ ] **Step 4: Run the test to verify it passes**

Run: `cargo test -p control-server homes::tests::pick_home_pid_wants_uid1000_in_target_ns`
Expected: PASS.

- [ ] **Step 5: Wire it into `reconcile`** — in `homes.rs`, replace the final two lines of the `for h in &hosts` loop (currently `ensure_symlink(&root.join(&h.id), &clone_home(pid), &h.id); desired.insert(h.id.clone());`) with:

```rust
        // Link a uid-1000 process's proc-root (not the root-owned main pid), so the SMB
        // share (smbd → force user=rmng) can follow it. No uid-1000 session yet (clone
        // still booting) → keep any existing link and retry next tick.
        let Some(home) = home_pid(pid) else {
            if root.join(&h.id).exists() {
                desired.insert(h.id.clone());
            }
            continue;
        };
        ensure_symlink(&root.join(&h.id), &clone_home(home), &h.id);
        desired.insert(h.id.clone());
```

- [ ] **Step 6: Update the module doc** — in the `homes.rs` header comment, change the symlink line and add an SMB note. Replace the line:

```
//! `<data_dir>/hosts/<id>` → `/proc/<clone-pid>/root/home/rmng` for every RUNNING managed
```

with:

```
//! `<data_dir>/hosts/<id>` → `/proc/<uid-1000-pid>/root/home/rmng` for every RUNNING managed
//! clone. The target is a uid-1000 process's proc-root (not the clone's root-owned init) so
//! the SMB share (smb.rs, smbd acting as uid 1000) can follow the link; every process in the
//! clone shares one rootfs, so host-side / `docker exec` browsing is unaffected.
```

(Delete the now-duplicated `clone.` continuation on the following line so the paragraph still reads cleanly.)

- [ ] **Step 7: Verify the whole crate still builds + tests pass**

Run: `cargo test -p control-server && cargo build -p control-server`
Expected: PASS (all `homes::tests` + `smb::tests` green; build clean apart from the known Task-1 dead-code warnings).

- [ ] **Step 8: Commit**

```bash
git add crates/control-server/src/homes.rs
git commit -m "feat(control-server): reconciler links uid-1000 proc-root for SMB traversal

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

## Task 3: SMB account provisioning + smbd supervisor + wiring

**Files:**
- Modify: `crates/control-server/src/smb.rs` (add provisioning, supervisor, `run`; drop the `#[allow(dead_code)]`)
- Modify: `crates/control-server/src/main.rs` (spawn `smb::run`)

**Interfaces:**
- Consumes: `render_smb_conf` (Task 1); `crate::app::App`; `App::config().data_dir`.
- Produces: `pub async fn run(app: App)`.

- [ ] **Step 1: Add imports + provisioning + supervisor to `smb.rs`** — add at the top (below the existing `//!` doc, above the constants):

```rust
use std::path::PathBuf;
use std::time::Duration;

use tokio::process::Command;

use crate::app::App;
```

Remove the `#[allow(dead_code)]` line above `render_smb_conf` (it's now used by `run`). Then append, after `render_smb_conf`:

```rust
/// Absolute share root: `<cwd>/<data_dir>/hosts` (WORKDIR /data + data_dir "data" →
/// /data/data/hosts; matches homes.rs and the host volume path).
fn share_path(app: &App) -> PathBuf {
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/data"));
    cwd.join(app.config().data_dir.clone()).join("hosts")
}

/// Ensure the local `rmng` unix user (uid 1000) exists and set its SMB password. The
/// Dockerfile creates the user at build; `useradd` here is an idempotent safety net, and
/// `smbpasswd` populates the (empty-in-a-fresh-image) tdbsam. Best-effort — logged, not fatal.
async fn provision_account() {
    // No-op if the user already exists (non-zero exit ignored).
    let _ = Command::new("useradd")
        .args(["-u", "1000", "-M", "-s", "/usr/sbin/nologin", SMB_USER])
        .status()
        .await;
    match set_smb_password().await {
        Ok(true) => tracing::info!(target: "smb", "smb account '{SMB_USER}' provisioned"),
        Ok(false) => tracing::warn!(target: "smb", "smbpasswd exited non-zero"),
        Err(e) => tracing::warn!(target: "smb", "smbpasswd failed: {e:#}"),
    }
}

/// `smbpasswd -a -s rmng`, feeding the password twice on stdin.
async fn set_smb_password() -> anyhow::Result<bool> {
    use tokio::io::AsyncWriteExt;
    let mut child = Command::new("smbpasswd")
        .args(["-a", "-s", SMB_USER])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .spawn()?;
    if let Some(mut stdin) = child.stdin.take() {
        stdin.write_all(format!("{SMB_PASS}\n{SMB_PASS}\n").as_bytes()).await?;
    }
    Ok(child.wait().await?.success())
}

/// Write the config, provision the account once, then supervise `smbd` forever — restart on
/// exit with capped exponential backoff. Always spawned; harmless without `pid: host` (the
/// share is just empty, same as the homes reconciler).
pub async fn run(app: App) {
    let hosts = share_path(&app).to_string_lossy().into_owned();
    if let Err(e) = std::fs::write(SMB_CONF_PATH, render_smb_conf(&hosts)) {
        tracing::error!(target: "smb", "writing {SMB_CONF_PATH}: {e:#} — smb share disabled");
        return;
    }
    provision_account().await;
    tracing::info!(target: "smb", "smb share serving {hosts} on :445 (user '{SMB_USER}')");

    let mut failures: u32 = 0;
    loop {
        match Command::new("smbd")
            .args(["--foreground", "--no-process-group", "--configfile", SMB_CONF_PATH])
            .status()
            .await
        {
            Ok(st) => tracing::warn!(target: "smb", "smbd exited ({st}); restarting"),
            Err(e) => tracing::error!(target: "smb", "spawning smbd failed: {e:#}; retrying"),
        }
        failures = failures.saturating_add(1);
        // 30s·2^n, capped at 5 min. smbd shouldn't crash-loop; a short cap is fine.
        let backoff = std::cmp::min(
            Duration::from_secs(300),
            Duration::from_secs(30) * 2u32.saturating_pow(failures.min(4)),
        );
        tokio::time::sleep(backoff).await;
    }
}
```

- [ ] **Step 2: Spawn it from `main.rs`** — next to the other background loops (after `tokio::spawn(homes::run(app.clone()));`), add:

```rust
    tokio::spawn(smb::run(app.clone()));
```

- [ ] **Step 3: Verify it builds cleanly** (no more dead-code warnings from Task 1):

Run: `cargo build -p control-server && cargo test -p control-server`
Expected: PASS, and the earlier `SMB_PASS`/`SMB_CONF_PATH` dead-code warnings are gone (now consumed by `run`).

- [ ] **Step 4: Commit**

```bash
git add crates/control-server/src/smb.rs crates/control-server/src/main.rs
git commit -m "feat(control-server): supervise smbd + provision rmng credential

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

## Task 4: Image — install Samba, create rmng uid 1000, expose 445

**Files:**
- Modify: `Dockerfile` (runtime stage)
- Modify: `compose.yaml`

- [ ] **Step 1: Add `samba` to the runtime apt install** — in `Dockerfile`, in the runtime stage's `apt-get install` list, add `samba` and update the preceding comment. Replace:

```dockerfile
# Runtime deps mined from scripts/cs-deploy-ct.sh, minus openssh-client/sshfs (the Docker
# port dials the local daemon over a unix socket — no SSH). vah264enc/vapostproc live in
# the `va` plugin shipped by gstreamer1.0-plugins-bad; pngenc (screenshots) in -good.
RUN apt-get update \
 && apt-get install -y --no-install-recommends \
      gstreamer1.0-plugins-base gstreamer1.0-plugins-good gstreamer1.0-plugins-bad \
      libva2 libva-drm2 va-driver-all libdrm2 \
      ca-certificates \
 && rm -rf /var/lib/apt/lists/*
```

with:

```dockerfile
# Runtime deps mined from scripts/cs-deploy-ct.sh. Still no openssh-client/sshfs (the Docker
# port dials the daemon over a unix socket — no SSH); `samba` provides smbd, which the
# control-server supervises to serve clone homes over SMB (smb.rs). vah264enc/vapostproc live
# in the `va` plugin shipped by gstreamer1.0-plugins-bad; pngenc (screenshots) in -good.
RUN apt-get update \
 && apt-get install -y --no-install-recommends \
      gstreamer1.0-plugins-base gstreamer1.0-plugins-good gstreamer1.0-plugins-bad \
      libva2 libva-drm2 va-driver-all libdrm2 \
      ca-certificates samba \
 && rm -rf /var/lib/apt/lists/*
```

- [ ] **Step 2: Create the `rmng` uid-1000 user** — in `Dockerfile`, immediately after that `RUN … samba …` block, add:

```dockerfile
# The SMB share acts as uid 1000 (force user=rmng) so files it creates match the clone's
# rmng user. smbpasswd (run at server start) needs this unix user to exist.
RUN useradd -u 1000 -M -s /usr/sbin/nologin rmng
```

- [ ] **Step 3: Expose 445** — in `Dockerfile`, update the EXPOSE line + its comment. Replace:

```dockerfile
# 9000 web/API, 9001 video, 9002 per-clone MCP, 9003 global MCP, 9005 forward data plane.
EXPOSE 9000-9003 9005
```

with:

```dockerfile
# 9000 web/API, 9001 video, 9002 per-clone MCP, 9003 global MCP, 9005 forward, 445 SMB.
EXPOSE 9000-9003 9005 445
```

- [ ] **Step 4: Publish 445 in compose** — in `compose.yaml`, update the `ports:` block + the header one-liner. In `ports:` replace:

```yaml
    ports:
      # 9000 web/API, 9001 video, 9002 per-clone MCP, 9003 global MCP, 9005 forward.
      - "9000-9003:9000-9003"
      - "9005:9005"
```

with:

```yaml
    ports:
      # 9000 web/API, 9001 video, 9002 per-clone MCP, 9003 global MCP, 9005 forward, 445 SMB.
      - "9000-9003:9000-9003"
      - "9005:9005"
      - "445:445"
```

And in the header comment's one-liner, change `-p 9000-9003:9000-9003 rmng:latest` to `-p 9000-9003:9000-9003 -p 445:445 rmng:latest`.

- [ ] **Step 5: Verify the image builds and contains smbd**

Run:
```bash
docker build -t rmng:smb-test .
docker run --rm --entrypoint sh rmng:smb-test -c 'command -v smbd && id rmng'
```
Expected: prints a path like `/usr/sbin/smbd` and `uid=1000(rmng) …`.

- [ ] **Step 6: Commit**

```bash
git add Dockerfile compose.yaml
git commit -m "build(control-server): install samba, create rmng uid1000, expose 445

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

## Task 5: Docs — rewrite "Browsing clone homes" for SMB

**Files:**
- Modify: `docs/DEPLOY.md`

- [ ] **Step 1: Replace the sshfs recipe with SMB** — in `docs/DEPLOY.md`, replace the three access-method bullets in the "Browsing clone homes" section (the block starting `- **From the Docker host**` and ending with the `docker exec` bullet, including the whole `- **Over sshfs** …` bullet and its fenced `sshfs …` command) with:

```markdown
- **Over SMB (the easy macOS/Windows path).** The control-server runs a Samba server on
  **port 445** exposing one share, `clones`, whose root is the list of clone ids. From a Mac:
  Finder → ⌘K → `smb://<control-server-host>/clones`, and log in as **`rmng`** / **`rmng`**.
  macOS mounts SMB natively — no macFUSE / kernel extension, no `follow_symlinks` flag. Files
  you create are owned by the clone's `rmng` user. Prerequisites: host port 445 must be free
  (published as `-p 445:445`), and the share is empty without `--pid host`.
- **From the Docker host** (the same symlink path resolves there, since `/proc/<pid>/root` is
  the clone's rootfs): `/var/lib/docker/volumes/rmng-data/_data/data/hosts/<id>`.
- **`docker exec`** into the control-server container and browse `data/hosts/`.
```

- [ ] **Step 2: Fix the symlink-target line in the same section** — replace `<data_dir>/hosts/<id> → /proc/<clone-pid-1>/root/home/rmng` with `<data_dir>/hosts/<id> → /proc/<uid-1000-pid>/root/home/rmng` and, if the surrounding prose still says "sshfs", reword to "the SMB share and the two direct paths below".

- [ ] **Step 3: Verify no stale sshfs references remain**

Run: `grep -rin "sshfs" docs/DEPLOY.md`
Expected: no output (zero matches).

- [ ] **Step 4: Commit**

```bash
git add docs/DEPLOY.md
git commit -m "docs: SMB replaces sshfs for browsing clone homes

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

## Task 6: End-to-end validation on a fresh LXC CT (executed by Claude)

> **This task is executed by Claude directly** (has Proxmox/LXC access) — it is the real pass/fail gate for the ownership/traversal spike, and it cannot be faked in a unit test. It is not a hand-off to an engineer. Steps are commands + expected output + a decision gate, not TDD red/green.

**Goal:** On a brand-new CT, deploy the current (pre-SMB) image, then **upgrade** to the SMB build via the documented `docker compose up -d` recreate path (preserving the `rmng-data` volume), and prove the share lists clones, reads, writes, and writes as uid 1000.

**Reference:** `docs/PROXMOX-LXC.md` §1 (CT features `nesting=1,keyctl=1,fuse=1`) + §1b (host keyring sysctls, already raised per prior work); `docs/DEPLOY.md` §Upgrades. GPU note: the fresh CT does **not** need W6800 passthrough for this — the media plane may log errors, but `homes.rs` + `smb.rs` are GPU-independent. A running clone container (for its uid-1000 session + `/home/rmng`) is what's required.

- [ ] **Step 1: Create + prep the CT.** Pick an unused CTID (`pct list`), create an Ubuntu 24.04 CT with `--features nesting=1,keyctl=1,fuse=1`, start it, and install Docker inside (`apt-get install -y docker.io docker-compose-v2`). Confirm `docker run --rm hello-world` works (validates nesting/keyctl).

- [ ] **Step 2: Get both images onto the CT.** Build the SMB image (`docker build -t pegasis0/rmng:smb-test .`) and push to Docker Hub (local docker is logged in); on the CT, `docker pull pegasis0/rmng:latest` (the OLD pre-SMB image) and `docker pull pegasis0/rmng:smb-test`. Copy `compose.yaml` to the CT.

- [ ] **Step 3: Deploy the OLD image + create a clone.** Point compose `image:` at `pegasis0/rmng:latest`, `docker compose up -d`. Open the wizard (`http://<ct-ip>:9000`), finish setup, pull the template, and create one clone. Confirm the clone is running and port 445 is **closed** (`nc -z <ct-ip> 445` fails) — baseline: no SMB before the upgrade.

- [ ] **Step 4: Upgrade to the SMB image.** Point compose `image:` at `pegasis0/rmng:smb-test`, `docker compose up -d` (recreates the container onto the new image; the `rmng-data` + `rmng-sock` named volumes persist). Confirm: the previously-created clone is still listed (data survived the upgrade), the server log shows `smb share serving …/data/hosts on :445`, and `nc -z <ct-ip> 445` now succeeds.

- [ ] **Step 5: Validate the share end-to-end** (from the CT or Claude's host, using `smbclient` — this exercises the full server path; the Finder mount is the user's final manual check):

```bash
smbclient -L //<ct-ip> -U rmng%rmng                 # lists the `clones` share
smbclient //<ct-ip>/clones -U rmng%rmng -c 'ls'      # lists <clone-id> dirs
smbclient //<ct-ip>/clones -U rmng%rmng -c 'cd <clone-id>; ls'   # reads INTO a clone home
# write test:
echo hi > /tmp/smbtest.txt
smbclient //<ct-ip>/clones -U rmng%rmng -c 'cd <clone-id>; put /tmp/smbtest.txt smbtest.txt'
# ownership check (inside the clone container):
docker exec <clone-container> ls -n /home/rmng/smbtest.txt   # expect uid 1000
```
Expected (R2 confirmed): the share lists, `cd <clone-id>; ls` **reads** the clone's home, the put **succeeds**, and `ls -n` shows **uid 1000**.

- [ ] **Step 6: Decision gate — R2 pass, or fall back to R3.**
  - **If Step 5 fully passes** → R2 is confirmed. Done; go to Step 7.
  - **If `cd <clone-id>; ls` fails with `NT_STATUS_ACCESS_DENIED` / the read can't traverse** → R2 is flaky (no followable uid-1000 proc-root). Apply the **R3 fallback** and re-run Step 5:
    - In `smb.rs` `render_smb_conf`, in `[clones]` replace `force user = rmng` / `force group = rmng` / `valid users = rmng` with:
      ```ini
   force user = root
   valid users = rmng
      ```
      (smbd already runs as root, so root-forced file ops traverse any clone's proc-root; created files will be **root-owned** — a documented caveat.)
    - Revert Task 2's `reconcile` change back to `ensure_symlink(&root.join(&h.id), &clone_home(pid), &h.id);` (main-pid link is fine when smbd acts as root), and keep the `pick_home_pid`/`home_pid` helpers only if still used (else delete them and their test to avoid dead code).
    - Note the ownership caveat in `docs/DEPLOY.md` (files created over SMB are root-owned under R3).
    - Commit the fallback with a message explaining R2 was rejected on hardware.

- [ ] **Step 7: Record the result + tear down.** Note in the final report which path (R2/R3) shipped and the exact `smbclient` output that proved it. Tear down the test CT (`pct stop <id> && pct destroy <id>`) unless keeping it as a testbed. Leave a one-line summary for the user to do the final Finder mount confirmation from their Mac.

---

## Self-Review

**Spec coverage:**
- SMB in-container, share root = clone list → Tasks 1, 3, 4. ✓
- Fixed rmng/rmng credential → Tasks 1 (consts), 3 (smbpasswd), 4 (unix user). ✓
- Read-write, uid-1000 ownership → smb.conf `force user` (Task 1) + reconciler retarget (Task 2), proven in Task 6. ✓
- Always-on, gated by pid:host → unconditional `smb::run` spawn (Task 3); empty share without pid:host (documented). ✓
- Port 445, `-p 445:445` → Task 4. ✓
- Supervised by the Rust binary (single ENTRYPOINT) → Task 3. ✓
- Ownership/traversal spike (R2 primary, R3 fallback) → Task 6 decision gate with fallback code. ✓
- Docs: drop sshfs, add SMB; fix homes.rs module doc; Dockerfile comments → Tasks 5, 2 (step 6), 4 (steps 1/3). ✓
- Tests: `render_smb_conf` unit + `pick_home_pid` unit + on-hardware integration → Tasks 1, 2, 6. ✓

**Placeholder scan:** none — every code step shows complete code; the R3 fallback ships real diffs.

**Type consistency:** `render_smb_conf(&str) -> String`, `pick_home_pid(u64, &[(i64,u32,u64)]) -> Option<i64>`, `home_pid(i64) -> Option<i64>`, `run(App)` used consistently across tasks. `SMB_USER`/`SMB_PASS`/`SMB_CONF_PATH` defined in Task 1, consumed in Task 3. `CLONE_UID` defined + used in Task 2.
