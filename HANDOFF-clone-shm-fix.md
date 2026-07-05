# Handoff: clone `/dev/shm` too small → Chrome/VSCode crash after the LXC→Docker port

**Branch:** `fix/clone-dev-shm-size` (off `origin/main` @ `87fc024`)
**Status:** root cause confirmed, one-off hotfix proven live on CT 105. Implementation NOT started.
**Audience:** the agent picking up the implementation.

---

## What's broken

Since the LXC→Docker sandbox port ([[docker-port]] / PR #2), Chrome and VSCode (both Chromium/Electron)
crash repeatedly inside clones. Nothing else crashes — that asymmetry is the tell.

**Root cause:** the clone `HostConfig` never sets `shm_size`, so every clone's `/dev/shm` gets Docker's
**64 MB default**. The old LXC clones booted systemd, which mounts `/dev/shm` as tmpfs at ~50% of the CT's
RAM (≈16 GB for a 32 GB clone). Chromium keeps renderer/compositor bitmaps in POSIX shared memory under
`/dev/shm`; when an allocation fails it **deliberately aborts the process** (`ud2` → SIGILL). Every
Chromium process in the clone shares the one 64 MB pool, so under real load (a few windows at 2560×1440,
software-rendered via `--ozone-platform=wayland`) it exhausts fast and processes drop.

### Evidence (gathered 2026-07-05 on CT 105 `pega-rmng`, clone `pega-hyperhost-command-center`)
- `docker exec <clone> df -h /dev/shm` → **64M**.
- Host `dmesg`: repeated `traps: <thread> trap invalid opcode ip:… in code[6b9155f,…]` — Chromium's
  out-of-memory `CHECK` (`ud2`), same binary offset every time, on `Compositor` / `ThreadPoolForeg` threads.
- **Three `code` (VSCode) processes trapped in the same second (22:54:59)**, one of them 9h18m old — independent
  processes hitting the same fault simultaneously ⇒ a *shared* resource ran dry, not a per-process bug or a
  bad CPU instruction.
- VSCode `~/.config/Code/logs/*/main.log`: `renderer process gone (reason: crashed, code: 4)` across sessions;
  matching Crashpad `.dmp` files; Chrome has its own dump pile.
- **Reproduced by the maintainer**: three fullscreen Chrome windows on the template clone crashed it; after the
  live-remount hotfix below, the same repro no longer crashes.

### Ruled out (don't rabbit-hole on these)
seccomp/apparmor (clone is `privileged` + `label=disable` ⇒ both already off), kernel keyring quota (host
sysctls already raised, zero keyring errors in dmesg), inotify limits (65536 / 4M), cgroup OOM kills (none in
dmesg; biggest VSCode scope peaked 5.9 G under a 32 G limit), `/dev/dri` (present via privileged).

### Out of scope — do NOT try to fix here
Separate `gnome-shell`/`libmutter-18` **segfaults** (offsets `15c7b1` / `1df7b8`, preceded by `g_object_ref` /
`g_closure_unref` assertion spam) are a *different* failure, most likely the multi-viewer virtual-monitor
resize churn ([[multi-viewer-merged]], PR #8), not the shm issue. When the shell dies it takes every Wayland
client with it ("Broken pipe"), which can masquerade as "Chrome crashed". Leave it for a separate investigation.

---

## The fix — two parts

### Part 1 — new clones (permanent, trivial)

Add `shm_size` to the clone `HostConfig` in
[`crates/control-server/src/docker.rs`](crates/control-server/src/docker.rs), `create_clone_container`,
the literal at **lines 1163–1174**:

```rust
let mem = (spec.memory_mb as i64) * 1024 * 1024;
let host_config = HostConfig {
    privileged: Some(true),
    nano_cpus: Some((spec.cpus as i64) * 1_000_000_000),
    memory: Some(mem),
    memory_swap: Some(mem + SWAP_BYTES),
    shm_size: Some(mem / 2),          // LXC parity: systemd mounted /dev/shm at ~50% of RAM
    mounts: Some(mounts),
    restart_policy: Some(RestartPolicy {
        name: Some(RestartPolicyNameEnum::UNLESS_STOPPED),
        ..Default::default()
    }),
    ..Default::default()
};
```

- Derive from `mem`, don't add a config knob — there's no reason to tune it independently of memory, and it
  mirrors the existing `SWAP_BYTES` "LXC parity" constant ([docker.rs:84](crates/control-server/src/docker.rs#L84)).
- Why `mem / 2` is safe: tmpfs is lazily allocated (a 16 GB ceiling costs nothing until used), and used pages
  are charged to the clone's **own** memory cgroup — so a runaway shm consumer OOMs that one clone, never the CT.
  Exactly the LXC behavior.
- **Verify the other create paths.** `shm_size` belongs ONLY on desktop-running clones:
  - The **template** create path — confirm whether the template goes through `create_clone_container` (then it's
    covered) or a separate path that also needs the line. It runs the same desktop, so it must get the fix.
  - Do NOT add it to the **self-upgrade helper** ([docker.rs ~1300](crates/control-server/src/docker.rs#L1300),
    unprivileged, sets `apparmor=unconfined`) or the **lxcfs probe** ([docker.rs ~630](crates/control-server/src/docker.rs#L630),
    `network:none`) — neither runs a desktop.

### Part 2 — existing clones (the maintainer's explicit ask: "the new control-server should hot fix any old existing clones")

`ShmSize` **cannot** be changed on an already-created container (`docker update` can't touch it), and recreating
would destroy the clone's writable layer (user's home/desktop state lives there, NOT in the `rmng-dind-*` /
`rmng-ctd-*` volumes — those are the clone's *inner* Docker storage). So the hotfix is a **live in-place remount**,
which is exactly what was proven by hand on CT 105.

**The control-server is perfectly positioned to do this:** its own container (`rmng`) runs
**`privileged: true` + `pid: host`** and ships `/usr/bin/nsenter`. It already resolves clone PIDs via
[`container_pid()`](crates/control-server/src/docker.rs#L1509) and already runs a PID-based reconcile loop over
running managed clones in [`crates/control-server/src/homes.rs`](crates/control-server/src/homes.rs) (15s cadence,
`/proc/<pid>/root/...`, depends on `pid: host`). **Reuse that pattern.**

**Proven mechanism (verified from inside the `rmng` container on 2026-07-05):**
```sh
# pid = clone's CT-namespace PID, from `docker inspect <clone> --format {{.State.Pid}}` (== container_pid()).
# Visible to the control-server because it is pid:host. Enter the clone's mount ns and remount:
nsenter -t <pid> -m mount -o remount,size=<N>,nosuid,nodev,noexec /dev/shm
```

**Recommended design:** a reconcile pass (fold into `homes::reconcile` or add a sibling loop) that, for each
**running** managed clone (`label rmng.managed=1`):
1. Reads the clone's current shm size **without `nsenter`** — the control-server is `pid:host`, so parse the
   `/dev/shm` line's `size=` from `/proc/<pid>/mountinfo` directly. This is the idempotency check.
2. If `size < target` (target = clone's inspected `HostConfig.Memory / 2`), remount via the `nsenter` command above.
3. Idempotent + cheap → safe to run every tick. Log once per clone when applied.

Also apply it right after any control-server-initiated clone start
([`provision.rs:359`](crates/control-server/src/provision.rs#L359) `start_container`) for immediacy, though the
loop will catch it within one tick anyway.

**Durability caveat to encode:** a live remount is lost whenever the container restarts (Docker re-creates
`/dev/shm` at 64 MB from the stored `HostConfig` on every start), including autonomous `unless-stopped` restarts
the control-server didn't initiate. The 15s reconcile loop re-applying is what makes this robust — do NOT assume
one remount sticks. The only *permanent* per-container fix is recreation, which we're intentionally not doing.

---

## Gotchas learned the hard way (save yourself the detour)

- **`docker exec <clone> mount -o remount,size=…G /dev/shm` FAILS** with `mount: /dev/shm: mount point not
  mounted or bad option` (exit 32). The `nsenter -t <pid> -m …` form from the CT / control-server **works**.
  Use `nsenter`, not the Docker exec API, for the remount.
- The naive `nsenter … mount -o remount,size=…G /dev/shm` (no other flags) silently **drops** `nosuid,nodev,noexec`
  — the original Docker mount had them. Always pass them explicitly (shown above) so the hotfix matches Docker's
  own mount and doesn't loosen the mount.
- The template (`pega-template`) currently has a **manual live remount to 16G already applied** so the maintainer
  can keep testing — don't be surprised it already reads 16G. It will revert to 64M on its next restart until this
  code ships.
- Another existing clone (`pega-we-588`) was confirmed still at 64M — the problem is fleet-wide, so Part 2 is not
  optional.

---

## Verification before claiming done

1. **New clone:** create one on staging; `docker exec <clone> df -h /dev/shm` shows ~`mem/2`, and
   `docker inspect <clone> --format '{{.HostConfig.ShmSize}}'` shows the bytes.
2. **Existing clone:** point the new control-server at a clone that's still on 64M; within ~15s its live
   `/dev/shm` should be resized (check via `/proc/<pid>/mountinfo` or `df`). Restart that clone and confirm the
   loop re-applies.
3. **Crash gone:** reproduce the load (VSCode + several fullscreen Chrome windows at 2560×1440); watch
   `df -h /dev/shm` climb well past 64M without dying. Confirm **no new** `traps: … in code` lines in host
   `dmesg`, **no** `renderer process gone (reason: crashed, code: 4)` in VSCode's `main.log`, and no fresh
   Crashpad dumps.
4. Standard `cargo build` / `cargo clippy` / tests for `control-server`.

## Deploy note
Ships in the control-server image ([[dockerhub-pegasis0-rmng]]); roll out per the daemons-first
[[upgrade-with-running-clones]] runbook. Part 1 only affects clones created *after* the upgrade — Part 2 is what
covers the ones already running.

---
*Root-cause investigation + live hotfix by the prior session; see repo memory `docker-clone-shm-64mb`.*
