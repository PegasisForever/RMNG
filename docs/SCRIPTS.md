# Scripts reference

Two families: **developer build/deploy** scripts (run by hand from your workstation) and
**control-server orchestration** scripts (embedded in the binary via `include_str!` and run
over SSH at runtime). Plus the gnome-patch build.

| Script | Runs where | Invoked by | Purpose |
|---|---|---|---|
| `scripts/provision-build-ct.sh` | workstation → node | operator | Create the **staging** control-server CT: build the binary, then run it (cs-deploy-ct.sh) |
| `scripts/cs-build-ct.sh` | inside build CT | provision-build-ct.sh | Install toolchain, embed binaries+deb, build workspace (no GNOME/capture) |
| `scripts/provision-deploy-ct.sh` | workstation → node | operator | Create the lean runtime CT, copy + run the binary |
| `scripts/cs-deploy-ct.sh` | inside deploy CT | provision-deploy-ct.sh | Runtime deps + config + SSH key + systemd unit |
| `crates/control-server/scripts/bootstrap.sh` | node (SSH) | `orchestrate::bootstrap_template` | Build a fresh template/clone CT from base image |
| `crates/control-server/scripts/provision-clone.sh` | inside new clone CT | bootstrap.sh | Headless GNOME + clone-daemon + agent-wrapper + patched shell |
| `crates/control-server/scripts/clone.sh` | node (SSH) | `orchestrate::clone_ct` | CoW (LVM-thin) snapshot of a template |
| `crates/control-server/scripts/redeploy.sh` | node (SSH) | `orchestrate::redeploy_clone` | Hot-swap a clone's daemon/agent binaries |
| `crates/control-server/scripts/delete.sh` | node (SSH) | `orchestrate::delete_ct` | Destroy a CT + its snapshot |
| `crates/control-server/scripts/apply-monitors.sh` | node (SSH) | `orchestrate::apply_monitors` | Re-apply a monitor layout to a running clone |
| `crates/control-server/scripts/claude-import.sh` | clone via node (`pct exec`) | `claude::{check_clone_auth,import_clone_account,apply_clone_token}` | Read `claude auth status` / the credentials file, clear it, or install a token |
| `gnome-patch/build-shell-deb.sh` | inside build CT | cs-build-ct.sh | Build the patched gnome-shell `.deb` |

The orchestration scripts are baked into the control-server binary at compile time
([orchestrate.rs:14-19](../crates/control-server/src/orchestrate.rs), [claude.rs:36](../crates/control-server/src/claude.rs))
and streamed to the node over `ssh … bash -s --` at runtime — they are **not** pre-installed
on the node. They emit `P <step> <msg>` progress lines and a final `RESULT …` line that
`run_remote` parses.

---

## Developer build/deploy

### `provision-build-ct.sh [flags] <proxmox-ssh> [hostname=rmng-build]`
Runs locally. Provisions the **staging** control-server CT. Packs `RMNG/` (incl. the vendored
`agent-wrapper`), ships it to the node, creates an unprivileged Ubuntu CT (nesting/keyctl/fuse,
render-node passthrough, apparmor unconfined, the `/srv/rmng-sock` clone-socket bind-mount),
runs `cs-build-ct.sh` to build the binary, then runs `cs-deploy-ct.sh` and authorizes the CT's
orchestration key on the node — so the CT comes up as a control-server orchestrating **real
clones**, exactly like the production deploy CT but with the toolchain. The build CT does **not**
run GNOME/capture. Flags (all optional, before the positionals): `--storage` (`local-lvm`),
`--bridge` (`vmbr0`), `--template` (Ubuntu 26.04), `--cores` (8), `--memory` (12288),
`--rootfs-gb` (40), `--sock-dir` (`/srv/rmng-sock`), `--proxmox-from-ct`. `--storage`/`--bridge`/
`--sock-dir` are passed on to `cs-deploy-ct.sh`, which prefills them (and `cloneSocket =
<sock-dir>/clones.sock`) into the CT's `config.json` so the first-run wizard matches the real
infra. Prints `RESULT <ctid> <ip>`; dashboard at `:9000`.

### `cs-build-ct.sh [src-dir=/root/RMNG]`
Runs inside the build CT. **Build only — installs no GNOME/capture session.** Installs the
toolchain (Rust, bun, GStreamer/VA/PipeWire/GTK4 *-dev*, plus the control-server's VA-API
*encode* runtime) and the gnome-shell build-deps (deb-src + `apt build-dep gnome-shell` +
`sassc dpkg-dev`). Then: builds `clone-daemon` (gzip → `embedded-bin/`), `bun build --compile`s
the `agent-wrapper` (gzip → `embedded-bin/`), builds the patched gnome-shell deb via
`gnome-patch/build-shell-deb.sh` (gzip → `embedded-bin/gnome-shell-deb.gz` — the deb is *built*;
gnome-shell is never installed), builds the frontend (`bun run build`), then builds the whole
workspace `--release` — `rust-embed` bakes the frontend + the three gzipped artifacts into
`control-server`. Installs it to `/usr/local/bin/rmng-control-server`. Idempotent.
`provision-build-ct.sh` runs `cs-deploy-ct.sh` afterward to start it as a control-server.

### `provision-deploy-ct.sh [flags] <proxmox-ssh> [hostname=rmng-control] [build-ct=rmng-build]`
Runs locally. Creates a **lean** runtime CT (runtime libs only, render passthrough, the
`/srv/rmng-sock` host dir bind-mounted for the clone socket), copies `control-server` from the
build CT, runs `cs-deploy-ct.sh` inside, and authorizes the CT's orchestration key on the
node. Flags (all optional, before the positionals): same as `provision-build-ct.sh` —
`--storage`/`--bridge`/`--template`/`--cores`/`--memory`/`--rootfs-gb`/`--sock-dir`/
`--proxmox-from-ct` (sizing defaults 4 cores / 4 GB / 12 GB; `--sock-dir` `/srv/rmng-sock`).
`--storage`/`--bridge`/`--sock-dir` are prefilled into the CT's config via `cs-deploy-ct.sh`.
Prints `RESULT <ctid> <ip>`; dashboard at `:9000`.

### `cs-deploy-ct.sh <proxmox-ssh-from-ct> [sock-dir=/srv/rmng-sock] [storage=local-lvm] [bridge=vmbr0]`
Runs inside the deploy CT. Installs runtime deps, writes a minimal `config.json` (the
Proxmox SSH target + the one-time infra settings — `proxmox.storage`, `proxmox.bridge`, and
`cloneSocket = <sock-dir>/clones.sock` — **prefilled** from the args so the first-run wizard
shows values matching the CT that was created, plus `setupComplete: false`), generates the
`~/.ssh/id_ed25519` orchestration key, and installs + starts the `control-server` systemd unit.
The provision scripts pass args 2-4 through; run by hand they default to the values shown.

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

### `claude-import.sh <ctid> <user> status|read|clear|apply [b64]`
On the node, `pct exec` into the clone. `apply` writes `~/.claude/.credentials.json`
(the account's current short-lived access token, refresh emptied — the control-server
refreshes and re-pushes it) — hot-swaps a clone's Claude account live, no restart.

---

## gnome-patch build

### `gnome-patch/build-shell-deb.sh`
Inside the build CT. Repack approach: applies shell-01 + shell-03 to the gnome-shell source,
rebuilds only `libshell-<N>.so` (meson/ninja), swaps it into the stock `.deb`, bumps the
version `+ngshell1`. Prints `DEB=<path>`. Cached (skips if the deb is newer than the patches;
`FORCE=1` rebuilds). See [gnome-patch/README.md](../gnome-patch/README.md).
