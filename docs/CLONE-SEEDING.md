# Clone layers: image, pre-boot inject, post-boot inject, reconciler loop

Where every byte of a clone comes from. Sources: `template/` (image),
`crates/control-server/src/provision.rs` (inject), `crates/control-server/src/clone_reconcile.rs`
(loop), `crates/control-server/src/derived.rs` (preset images).

Design rule: list 3 used to be the loop run once, early. It no longer is: everything
file-shaped lands pre-boot in ONE tar (list 2), and the loop (list 4) owns lived-in
convergence. Steps the loop backstops (SSH material) are best-effort at create with
the stamp withheld; the loop re-runs whatever is unstamped. Steps with NO loop
backstop (payload presence, unit masks, bashrc append, resolvable control host)
fail the op instead — booting a half-clone helps nobody.

## 1. Baked into the image

Two image kinds. Both are plain Docker builds; neither carries clone identity or the
clone binaries.

**Template** (`template/Dockerfile`, base `ubuntu:26.04`, published as
`pegasis0/rmng-template`), built in COPY-each-script-before-its-RUN phases:

- Phase 10 (`template/setup/10-desktop.sh`): locale/timezone, headless GNOME + Mutter +
  VA-API + PipeWire (no gdm3, no gnome-remote-desktop, no flatpak), container masks.
- Phase 15 (`template/setup/15-gnome-patch.sh`): patched gnome-shell .deb from the
  `gnome-build` stage (hides the screen-sharing indicator, enables
  `org.gnome.Shell.Eval` for the daemon's window-management tools). Load-bearing:
  a failed install fails the build.
- Phase 20 (`template/setup/20-toolbox.sh`): dev toolbox via apt (CLI tools, Docker,
  cloud CLIs, browsers, Cursor, VS Code, fonts, ONLYOFFICE, …) plus dconf defaults,
  Zed editor (pinned v1.19.2 tarball + sha256 → `/opt/zed.app`, PATH wrapper,
  desktop entry, ldd shared-library gate), Mission Center / Monaspace /
  adw-gtk3 from upstream releases. STRICT: any failure fails the build — nothing
  warns-and-continues.
- Phase 30 (`template/setup/30-user.sh`): the clone user (uid 1000, passwordless sudo,
  linger, fish shell), interactive PATH rc, passwordless GNOME keyring, EMPTY config
  dirs owned by the user (`~/.claude`, `~/.codex`, `~/.pi/agent`, `~/.config[/rmng]`,
  both `rmng-cli` skill dirs, `~/.cursor[/rules]`, `~/.rmng` — every parent the parity
  tar writes into, so the extract never invents a root-owned dir; a test pins this
  against phase 30), user toolchains (claude / uv / rustup / nvm / fish-nvm, plus the
  Codex CLI — the image is its sole source, no post-boot install exists), and the `systemd --user` unit DEFINITIONS (headless
  gnome-shell + clone-daemon + agent-wrapper) with their wants-symlinks. The
  session-holder unit is NOT baked (the server ships it pre-boot on headed clones).
  Pre-creates `/opt/rmng/bin` EMPTY and `~/.ssh` (700, no host keys). Blanks
  machine-id (one baked id would identify the whole fleet). Enables + hardens sshd
  (`AllowUsers $USERNAME`). Symlinks the dbus machine-id.

**Preset images** (`derived.rs`): each preset carries its own FULL Dockerfile, used
verbatim (no FROM rewrite, no digest pinning). Tag = hash of the file text, so any
edit re-tags; empty text falls back to the default base Dockerfile.

Deliberately NOT baked: `rmng-clone-daemon`, `agent-wrapper`, `rmng` CLI (list 2 —
the server installs its own current copies, so a clone can never drift from it) —
and, after the SSOT audit, none of the server-owned content either: no `CLAUDE.md` /
`AGENTS.md` bodies, no MCP files, no `/etc/environment` session keys, no holder unit.
The template owns directories, packages, users, and unit DEFINITIONS; the server owns
all per-clone content.

## 2. Injected before boot (container created, still stopped)

`clone_container_after_create` in `provision.rs`: ONE `upload_tar` (works on stopped
containers) plus one symlink upload (headless masks), then the create-spec bind
mounts — all fixed before start:

Files (a missing payload fails the op — no daemonless boot; the loop hard-errors on
the same absence, so tolerating it here would only delay the failure by one pass):

- Clone binaries (`CLONE_BINARIES`, `provision.rs:1461`): `rmng-clone-daemon` +
  `agent-wrapper` → `/opt/rmng/bin` (0755), `rmng` CLI → `/usr/local/bin` — plus,
  on headed clones, the session-holder unit, plus the payload stamp
  (`opt/rmng/.payload-hash`) so the loop's first hash-compare is a no-op.
- Identity: `etc/machine-id` (fresh random per clone, 0444 — systemd-in-docker
  would otherwise run transient), `etc/environment` (base session + control URLs +
  preset vars; read by PAM and the lingering user manager at boot, which is why it
  cannot wait until after start — and why an unresolvable control host fails the op
  at create/fork/rebase/migrate instead of booting a degraded URL the loop could
  never repair), headless unit MASKS (`gnome-headless` + `clone-daemon` → `/dev/null`
  symlinks over the baked unit files, so the desktop never starts — no reload, no
  pkill, no boot race; fails the op on upload error), and, only when the preset
  sets PATH,
  `etc/fish/conf.d/rmng-preset-path.fish` + `etc/profile.d/rmng-preset-path.sh`.
- Content: playbook (`~/.config/rmng/agent-instructions.md`, skipped when empty),
  Codex parity files + stamp, SSH host key + `authorized_keys` + stamp, probe file
  (`~/.rmng/hook.py`), and six merge-owned files: `~/.claude.json`,
  `~/.cursor/mcp.json`, `~/.codex/config.toml`, `~/.config/mcp/mcp.json`,
  `~/.claude/settings.json`, and `~/.cursor/hooks.json`. Pre-boot and live updates
  share merge rules and five completion stamps; carried user fields survive.
  Malformed JSON fails before upload; rebase also checks before removing its container.

Mounts (create-spec binds, present from first boot — always mounted, no empty skips):

- Home overlay merged view → `/home/rmng` (template home as shared lower layer,
  per-clone dataset as upper).
- Homes parent → `/home/rmng/clones` (every home visible side by side).
- Shared pool `<homes>/.shared` → `/home/rmng/shared` (daemon-visible path —
  anything inside the server's `data/` volume is container-private and Docker
  rejects it as a bind source).
- Clone media socket dir.

## 3. Injected after boot (tmux, wait-ready)

Container started (`docker.start_container`). Everything file-shaped already landed
in the single pre-boot tar (list 2) — including the preset-PATH bashrc drop-in
(`/etc/bash.bashrc.d/rmng-preset-path.sh`, sourced from the baked bashrc). What
remains post-boot needs a live container:

1. Headless: nothing — the desktop units were masked pre-boot (list 2) and could
   never have started.
2. Headless: start the default `main` tmux session, report ready (convenience only —
   `termplane` recreates a missing session on select).
3. Headed: poll the mediaplane for the clone-daemon's `Hello` until
   `WAIT_READY_TIMEOUT`. Alive-but-unregistered reports ready with an explicit
   warning (check it in the UI); an exited container fails with its log tail.

## 4. Convergence triggers (no polling loop)

There is no reconciler loop. `clone_reconcile::run` does one full boot pass and exits.
Everything converges via explicit triggers, all funnelling into the same full-chain
function (`sync_clone_contents` — SSH, env, parity, four MCP merges, probe, payload;
stamped and idempotent throughout):

- Create: the single pre-boot tar (list 2) stamps everything — no trigger needed.
- Server boot: one full pass over running clones (`sync_all_running`, reason `boot`),
  so upgrades (payload, probe, MCP sets) land without waiting on any timer.
- Settings save: one full pass (`sync_all_running`, reason `settings-save`),
  detached — the PUT never waits on Docker. Env, parity, and MCP changes land now.
- Fork / rebase / migrate / unarchive: `spawn_converge_after_start` per clone —
  waits bounded (10 s cadence, 30 min cap) for the container to be running, then
  runs the chain. Covers fresh `/etc` on carried-over homes; quiet no-op when the
  clone never comes up (still archived, deleted mid-wait).

Removed, no replacement:

1. Archived-state sweep — deleted with its commit/swap op filters and tests.
   Crash-frozen clones no longer thaw themselves; archived-but-up clones no longer
   stop themselves. Accepted: manual recovery, and nothing in the supported flows
   produces those states anymore (commit freezes inside its op window; rebase rests
   its own containers).

What used to poll, entry by entry (deleted with the loop):

1. SSH ready: covered above — pre-boot tar, save fan-out (`apply_now` + full pass),
   boot pass, post-op sync. The `ssh.rs` supervisor loop additionally pushes key
   content on its own cadence.

Convergence notes:

- The claude/codex MCP stamps are content hashes of their merge scripts (cursor's
  already was), so a managed-set code change re-pushes at the next trigger —
  previously only a headless flip or key rotation did.
- A trigger failure (wedged daemon mid-save) retries at the next trigger of any
  kind; stamps make every pass cheap and every retry safe.
- Manually `docker start`ing an archived container bypasses all triggers: it keeps
  whatever settings it last converged. Supported starts go through unarchive.
