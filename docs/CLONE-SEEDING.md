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
  (`~/.rmng/hook.py`) + stamp, and the initial contents of the four merge-owned
  files (`~/.claude.json`, `~/.cursor/mcp.json`, `~/.codex/config.toml`,
  `~/.claude/settings.json`, `~/.cursor/hooks.json`) + their four stamps — all
  rendered server-side, all stamped so the loop's first pass is a no-op.

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

## 4. Reconciler loop (`clone_reconcile::run`, every 30 s)

One pass (`reconcile_once`) covers managed, unarchived, id-safe clones whose
container is running. Failures warn once per clone+step (`warned` set) and retry
next pass; keys for vanished clones are dropped. An unresolvable control host skips
the whole pass (warned globally) rather than rewriting the fleet into a degraded URL.

Assumption for the verdicts below: clones never intentionally change these files —
drift comes only from server-side changes (settings, code), crashes, and corruption.
The only event push today is Settings save (`config_put`): SSH keys (`apply_now`,
30 s-bounded, loop retries) and monitor geometry (watched clone only). Everything
else converges here.

1. Archived-state sweep (thaw paused-but-not-archived; stop archived-but-up; skips
   mid-rebase-swap and committing clones). NEEDED — not a file check: enforces the
   stopped/archived invariant across crashes and daemon restarts. Not settings-driven;
   could only move to Docker-events, a different mechanism.
2. SSH ready (dirs + keys + `authorized_keys`, version-stamped). REPAIR ONLY — key
   changes already push on save; pre-boot covers fresh clones. Keep for host-key
   rotation and corruption; cannot drop the loop copy while rotation has no event.
3. `/etc/environment` sync (content-compared; restarts `agent-wrapper` only on a real
   change). NEEDED, MOVABLE TO SAVE — every input (control env excepted) originates
   in settings/presets/keys. Extend `config_put` to compare-and-push like the SSH
   and monitor handling; the loop keeps the repair role. The restart-on-change
   subtlety moves with it.
4. Codex parity files (content-stamped). NEEDED, MOVABLE TO SAVE — inputs are the
   global prompt, playbook, and preset, all settings-side. Same compare-and-push
   shape as 3.
5. `~/.claude.json` MCP merge, stamped. NEEDED, MOVABLE TO SAVE for key/headless
   changes (both visible in old-vs-merged config). Gap, independent of the loop:
   a server-code change to the managed set does NOT re-push — the claude/codex
   stamps are `v1 headless=…`, not content hashes (only cursor's script-hash stamp
   re-pushes). Fix by content-hashing those stamps or bumping `v1`.
6. `~/.cursor/mcp.json` MCP merge, stamped (Linear bearer re-resolved each pass).
   Same verdict as 5, minus the gap (its stamp already tracks the key).
7. Activity probe files + registration, content-stamped. NEEDED, MOVABLE TO A
   POST-RESTART SWEEP — the only input is server code (hash covers it). A one-shot
   push to running clones at server start replaces the 30 s poll for delivery; the
   loop keeps corruption repair.
8. `~/.codex/config.toml` MCP merge, stamped. Same verdict as 5 (including the gap).
9. Payload binaries refresh (hash-compare; restarts daemon + wrapper; mask-aware
   guard on headless). NEEDED, MOVABLE TO A POST-RESTART SWEEP — same reasoning
   as 7: the hash is the delivery trigger for upgrades, polling adds only repair.

Net: with 3+4+5+6+8 fanning out on save and 7+9 sweeping once at server start, the
30 s loop degrades to repair-only (1, 2, corruption) and could run far less often.
Not implemented — proposal only.

NOT in this loop (one-shots elsewhere): home symlinks under `data/hosts`
(synced once at server boot), overlay remounts after a server restart, the
shared-pool dir ensure (server startup), SMB config render.
