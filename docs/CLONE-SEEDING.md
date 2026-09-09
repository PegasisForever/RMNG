# Clone layers: image, pre-boot inject, post-boot inject, reconciler loop

Where every byte of a clone comes from. Sources: `template/` (image),
`crates/control-server/src/provision.rs` (inject), `crates/control-server/src/clone_reconcile.rs`
(loop), `crates/control-server/src/derived.rs` (preset images).

Design rule: the post-boot inject (list 3) is the reconciler loop (list 4) run once, early.
Each step is best-effort at create time and stamped; a missing stamp means the loop
re-runs that step on its next pass. Withholding the stamp IS the retry.

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
  linger, fish shell), interactive PATH rc, passwordless GNOME keyring, shared
  CLAUDE.md + linear MCP, user toolchains (claude / uv / rustup / nvm / fish-nvm),
  and the `systemd --user` units (headless gnome-shell + clone-daemon +
  agent-wrapper) with their wants-symlinks. Pre-creates `/opt/rmng/bin` EMPTY and
  `~/.ssh` (700, no host keys). Blanks machine-id (one baked id would identify the
  whole fleet).

**Preset images** (`derived.rs`): each preset carries its own FULL Dockerfile, used
verbatim (no FROM rewrite, no digest pinning). Tag = hash of the file text, so any
edit re-tags; empty text falls back to the default base Dockerfile.

Deliberately NOT baked: `rmng-clone-daemon`, `agent-wrapper`, `rmng` CLI (list 2 —
the server installs its own current copies, so a clone can never drift from it).

Overlap warning: the template ALSO bakes static copies of four files list 3 overwrites
with the live values on every create — `~/.claude/CLAUDE.md`, `~/.codex/AGENTS.md`,
`~/.codex/config.toml` (desktop + linear MCP defaults), and the `~/.claude.json`
baseline (linear + desktop via jq). The baked copies are fallback so the image stands
alone without provision; on a default fleet the inject writes byte-identical guidance
over them and idempotent MCP merges. Drift risk: editing the baked text without the
matching server default (or vice versa) shows one, then flips to the other.

## 2. Injected before boot (container created, still stopped)

`clone_container_after_create` in `provision.rs`, via `upload_tar` (works on stopped
containers) — plus the create-spec bind mounts, which are also fixed before start:

Files:

- Clone binaries (`CLONE_BINARIES`, `provision.rs:1461`): `rmng-clone-daemon` +
  `agent-wrapper` → `/opt/rmng/bin` (0755), `rmng` CLI → `/usr/local/bin` — plus,
  on headed clones, the session-holder unit, plus the payload stamp
  (`opt/rmng/.payload-hash`) so the loop's first hash-compare is a no-op.
- Identity: `etc/machine-id` (fresh random per clone, 0444 — systemd-in-docker
  would otherwise run transient), `etc/environment` (base session + control URLs +
  preset vars; read by PAM and the lingering user manager at boot, which is why it
  cannot wait until after start), and, only when the preset sets PATH,
  `etc/fish/conf.d/rmng-preset-path.fish` + `etc/profile.d/rmng-preset-path.sh`.

Mounts (create-spec binds, present from first boot):

- Home overlay merged view → `/home/rmng` (template home as shared lower layer,
  per-clone dataset as upper).
- Homes parent → `/home/rmng/clones` (every home visible side by side).
- Shared pool `<homes>/.shared` → `/home/rmng/shared` (daemon-visible path —
  anything inside the server's `data/` volume is container-private and Docker
  rejects it as a bind source).
- Clone media socket dir.

## 3. Injected after boot (provision, best-effort + stamps)

Container started (`docker.start_container`), then in order. Every `seed_step`
logs-and-continues on failure; the loop retries whatever is unstamped:

1. Headless only: delete the desktop units (`gnome-headless` + `clone-daemon` unit
   files and wants-symlinks), `daemon-reload`, `pkill` anything the user manager
   already started in the boot race. `agent-wrapper` stays enabled.
2. `~/.codex` dir prep (ownership fix for old templates), then the Codex CLI
   install (network pipe into the user account — must run live, post-start).
3. Second tar: `~/.config/rmng/agent-instructions.md` (global + preset playbook,
   skipped when empty), the Codex parity files + stamp, the clone's stable SSH
   host key + current `authorized_keys` + stamp.
4. `~/.claude.json` MCP servers (jq merge — state-bearing, never a tar entry) +
   stamp. Desktop server removed on headless.
5. `~/.cursor/mcp.json` MCP servers + stamp (`LINEAR_API_KEY` resolved to its
   value here — Cursor does not expand env references).
6. `~/.codex/config.toml` MCP servers (merge — the operator's file) + stamp.
7. Activity probe: hook files tar + registration script + stamp.
8. Preset-PATH append to `/etc/bash.bashrc` (append, not a file — tar cannot do
   it; idempotent delete-then-append; non-fatal; only when the preset sets PATH).
9. Headless: start the default `main` tmux session, report ready. Headed: poll
   the mediaplane for the clone-daemon's `Hello` until `WAIT_READY_TIMEOUT`,
   then report ready.

## 4. Reconciler loop (`clone_reconcile::run`, every 30 s)

One pass (`reconcile_once`) covers managed, unarchived, id-safe clones whose
container is running. Failures warn once per clone+step (`warned` set) and retry
next pass; keys for vanished clones are dropped.

- Archived-state sweep first: thaw paused-but-not-archived clones; stop
  archived-but-up ones. Skips clones mid-rebase-swap and clones being committed
  (thawing/env-syncing those would corrupt the swap/snapshot).
- SSH ready: dirs + host keys + `authorized_keys`, version-stamped (`SSH_STAMP_VERSION`).
- `/etc/environment` sync: control env + per-clone identity key + preset env +
  `ANTHROPIC_MODEL`, content-compared; restarts `agent-wrapper` only on a real
  change (a blind restart would interrupt an in-flight chat turn every 30 s).
- Codex CLI install (idempotent script, runs every pass).
- Codex parity files, content-stamped (prepare script rides the stamp because it
  owns the parent dirs).
- `~/.claude.json` MCP merge, stamped.
- `~/.cursor/mcp.json` MCP merge, stamped (Linear bearer re-resolved each pass).
- Activity probe files + registration, stamped.
- `~/.codex/config.toml` MCP merge, stamped.
- Payload binaries refresh: hash-compare against the server's staged payloads;
  on mismatch re-push and restart `rmng-clone-daemon` + `agent-wrapper`
  (headless: restart guarded by `systemctl cat` — absent units skip cleanly
  instead of wedging the whole step).

NOT in this loop (one-shots elsewhere): home symlinks under `data/hosts`
(synced once at server boot), overlay remounts after a server restart, the
shared-pool dir ensure (server startup), SMB config render.
