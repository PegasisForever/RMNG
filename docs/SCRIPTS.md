# Scripts reference

Three families: **developer build/deploy** scripts (run by hand from your workstation),
**control-server orchestration** scripts (embedded in the binary via `include_str!` and run
over SSH at runtime), and **local test harnesses**. Plus the gnome-patch build.

| Script | Runs where | Invoked by | Purpose |
|---|---|---|---|
| `scripts/provision-build-ct.sh` | workstation → node | operator | Create the build/dev CT + build the binary |
| `scripts/cs-build-ct.sh` | inside build CT | provision-build-ct.sh | Install toolchain, embed binaries+deb, build workspace |
| `scripts/provision-deploy-ct.sh` | workstation → node | operator | Create the lean runtime CT, copy + run the binary |
| `scripts/cs-deploy-ct.sh` | inside deploy CT | provision-deploy-ct.sh | Runtime deps + config + SSH key + systemd unit |
| `crates/control-server/scripts/bootstrap.sh` | node (SSH) | `orchestrate::bootstrap_template` | Build a fresh template/clone CT from base image |
| `crates/control-server/scripts/provision-clone.sh` | inside new clone CT | bootstrap.sh | Headless GNOME + clone-daemon + agent-wrapper + patched shell |
| `crates/control-server/scripts/clone.sh` | node (SSH) | `orchestrate::clone_ct` | CoW (LVM-thin) snapshot of a template |
| `crates/control-server/scripts/redeploy.sh` | node (SSH) | `orchestrate::redeploy_clone` | Hot-swap a clone's daemon/agent binaries |
| `crates/control-server/scripts/delete.sh` | node (SSH) | `orchestrate::delete_ct` | Destroy a CT + its snapshot |
| `crates/control-server/scripts/apply-monitors.sh` | node (SSH) | `orchestrate::apply_monitors` | Re-apply a monitor layout to a running clone |
| `crates/control-server/scripts/apply-credentials.sh` | inside running clone (SSH) | `claude::apply_clone_token` | Install/hot-swap a Claude token |
| `gnome-patch/build-shell-deb.sh` | inside build CT | cs-build-ct.sh | Build the patched gnome-shell `.deb` |
| `scripts/run-localstack.sh` | build CT | operator | Persistent control-server + clone-daemon for the viewer to hit |

The orchestration scripts are baked into the control-server binary at compile time
([orchestrate.rs:14-19](../crates/control-server/src/orchestrate.rs), [claude.rs:36](../crates/control-server/src/claude.rs))
and streamed to the node over `ssh … bash -s --` at runtime — they are **not** pre-installed
on the node. They emit `P <step> <msg>` progress lines and a final `RESULT …` line that
`run_remote` parses.

---

## Developer build/deploy

### `provision-build-ct.sh <proxmox-ssh> [hostname=rmng-build]`
Runs locally. Packs `RMNG/` + `../agent-wrapper`, ships them to the node, creates an
unprivileged Ubuntu CT (nesting/keyctl/fuse, render-node passthrough, apparmor unconfined),
then runs `cs-build-ct.sh` inside it. Env: `RMNG_STORAGE` (`local-lvm`), `RMNG_BRIDGE` (`vmbr0`),
`RMNG_TEMPLATE` (Ubuntu 26.04), `RMNG_CORES` (8), `RMNG_MEMORY` (12288), `RMNG_ROOTFS_GB` (40).
Prints `RESULT <ctid> <ip>`.

### `cs-build-ct.sh [src-dir=/root/ng/RMNG]`
Runs inside the build CT (also the dev/test box). Installs the full toolchain (Rust, bun,
GStreamer/VA/PipeWire/GTK4 dev + runtime, headless GNOME) and the gnome-shell build-deps
(deb-src + `apt build-dep gnome-shell` + `sassc dpkg-dev`). Then: builds `clone-daemon`
(gzip → `embedded-bin/`), `bun build --compile`s the `agent-wrapper` (gzip → `embedded-bin/`),
builds the patched gnome-shell deb via `gnome-patch/build-shell-deb.sh` (gzip →
`embedded-bin/gnome-shell-deb.gz`), builds the frontend (`bun run build`), then builds the
whole workspace `--release` — `rust-embed` bakes the frontend + the three gzipped artifacts
into `control-server`. Installs it to `/usr/local/bin/rmng-control-server`. Idempotent. Env:
`RMNG_DEV_USER` (`dev`).

### `provision-deploy-ct.sh <proxmox-ssh> [hostname=rmng-control] [build-ct=rmng-build]`
Runs locally. Creates a **lean** runtime CT (runtime libs only, render passthrough, the
`/srv/rmng-sock` host dir bind-mounted for the clone socket), copies `control-server` from the
build CT, runs `cs-deploy-ct.sh` inside, and authorizes the CT's orchestration key on the
node. Env: same `RMNG_*` sizing (defaults 4 cores / 4 GB / 12 GB), `RMNG_SOCK_DIR`
(`/srv/rmng-sock`), `RMNG_PROXMOX_FROM_CT`. Prints `RESULT <ctid> <ip>`; dashboard at `:9000`.

### `cs-deploy-ct.sh <proxmox-ssh-from-ct>`
Runs inside the deploy CT. Installs runtime deps, writes a minimal `config.json` (just the
Proxmox SSH target), generates the `~/.ssh/id_ed25519` orchestration key, and installs +
starts the `control-server` systemd unit.

---

## Control-server orchestration (embedded, run over SSH)

### `bootstrap.sh <hostname> <template> <storage> <bridge> <prov_b64> [cd_bin] [aw_bin] [monitors] [shell_deb]`
On the node. Creates a CT from the base image, configures render/apparmor + the `/srv/rmng-sock`
bind-mount, starts it, waits for DHCP, `pct push`es the staged binaries (clone-daemon,
agent-wrapper, patched gnome-shell deb) + the base64 `provision-clone.sh`, then runs it.
`RESULT <ctid> <ip>`.

### `provision-clone.sh <username> <password> [monitors]`
Inside the new CT. apt upgrade; remove snap + disable apparmor; install headless GNOME +
Mutter + VA-API + PipeWire (no GDM/g-r-d); **install the patched gnome-shell deb** if pushed;
create the user (sudo, render/video, linger); install `clone-daemon` + `agent-wrapper` + the
standalone `claude` CLI; write + enable three `systemd --user` units (`gnome-headless`,
`clone-daemon`, `agent-wrapper`). `RESULT ok`.

### `clone.sh <src-id> <new-hostname> <macprefix>`
On the node. Locate the source CT by hostname, LVM-thin CoW-snapshot its rootfs, reset
machine-id/hostname and regenerate each NIC's MAC (with `<macprefix>` — a snapshot inherits
the template's MAC, which would collide on the shared bridge), start the clone, wait for its
**eth0 (vmbr0)** DHCP lease. `RESULT <ctid> <ip>`. (CoW clones inherit everything baked into
the template, incl. the patched shell. Single-NIC on vmbr0 — no internal subnet.)

### `redeploy.sh <ctid> <username> <cd_bin|-> <aw_bin|->`
On the node. Stop the clone's `clone-daemon` (+`agent-wrapper` unless `-`), `pct push` the new
binaries, restart. The daemon reconnects to the socket.

### `delete.sh <ctid>` · `apply-monitors.sh <ctid> <username> <monitors>`
`delete.sh`: stop + destroy the CT and its thin snapshot. `apply-monitors.sh`: rewrite the
clone's `RMNG_MONITORS` + dummy mode specs and restart its GNOME + daemon (re-creates the
virtual monitors with new positions).

### `apply-credentials.sh` (token via stdin)
Inside a running clone over SSH. Writes `~/.claude/.credentials.json` (long-lived token,
refresh emptied) and nudges the agent-wrapper — hot-swaps a clone's Claude account live.

---

## gnome-patch build

### `gnome-patch/build-shell-deb.sh`
Inside the build CT. Repack approach: applies shell-01 + shell-03 to the gnome-shell source,
rebuilds only `libshell-<N>.so` (meson/ninja), swaps it into the stock `.deb`, bumps the
version `+ngshell1`. Prints `DEB=<path>`. Cached (skips if the deb is newer than the patches;
`FORCE=1` rebuilds). See [gnome-patch/README.md](../gnome-patch/README.md).

---

## Local dev stack (build CT)

### `run-localstack.sh`
Brings up control-server + clone-daemon as transient systemd units
(`rmng-control-server`/`rmng-clone-daemon`) against the CT's headless GNOME, host seeded
`@127.0.0.1`, so a real viewer (e.g. from your laptop) can connect to `<ct-ip>:9001`.
Idempotent (re-running restarts both). Env: `RMNG_USER` (`pega`).
