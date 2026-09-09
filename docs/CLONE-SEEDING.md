# Clone layers: image, pre-boot inject, post-boot inject, reconciler loop

Where every byte of a clone comes from. Sources: `template/` (image),
`crates/control-server/src/provision.rs` (inject), `crates/control-server/src/clone_reconcile.rs`
(loop), `crates/control-server/src/derived.rs` (preset images).

Design rule: the post-boot inject (list 3) is the reconciler loop (list 4) run once, early.
Steps the loop backstops are best-effort at create time and stamped; a missing stamp
means the loop re-runs that step on its next pass. Withholding the stamp IS the retry.
Steps with NO loop backstop (payload presence, headless unit delete, bashrc append,
resolvable control host) fail the op instead — booting a half-clone helps nobody.

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
  dirs owned by the user (`~/.claude`, `~/.codex`, `~/.pi/agent` — the server fills
  them; pre-creating avoids root-owned parents), user toolchains (claude / uv /
  rustup / nvm / fish-nvm), and the `systemd --user` unit DEFINITIONS (headless
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

`clone_container_after_create` in `provision.rs`, via `upload_tar` (works on stopped
containers) — plus the create-spec bind mounts, which are also fixed before start:

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
  never repair), and, only when the preset sets PATH,
  `etc/fish/conf.d/rmng-preset-path.fish` + `etc/profile.d/rmng-preset-path.sh`.

Mounts (create-spec binds, present from first boot — always mounted, no empty skips):

- Home overlay merged view → `/home/rmng` (template home as shared lower layer,
  per-clone dataset as upper).
- Homes parent → `/home/rmng/clones` (every home visible side by side).
- Shared pool `<homes>/.shared` → `/home/rmng/shared` (daemon-visible path —
  anything inside the server's `data/` volume is container-private and Docker
  rejects it as a bind source).
- Clone media socket dir.

## 3. Injected after boot (provision: stamped steps + fail-loud steps)

Container started (`docker.start_container`), then in order. Steps the loop backstops
are best-effort (`seed_step` logs-and-continues, stamp withheld); steps with no
backstop fail the op:

1. Headless only: delete the desktop units (`gnome-headless` + `clone-daemon` unit
   files and wants-symlinks), `daemon-reload`, `pkill` anything the user manager
   already started in the boot race. `agent-wrapper` stays enabled. FAILS THE OP
   on error — no loop step reaps a surviving desktop.
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
7. Activity probe: hook files tar + registration script + stamp. Tar vs register
   failures are logged separately; either way the stamp is withheld and the loop
   retries.
8. Preset-PATH append to `/etc/bash.bashrc` (append, not a file — tar cannot do
   it; idempotent delete-then-append; only when the preset sets PATH). FAILS THE
   OP on error — no loop step re-appends it.
9. Headless: start the default `main` tmux session, report ready (convenience only —
   `termplane` recreates a missing session on select). Headed: poll the mediaplane
   for the clone-daemon's `Hello` until `WAIT_READY_TIMEOUT`. Alive-but-unregistered
   reports ready with an explicit warning (check it in the UI); an exited container
   fails with its log tail.

## 4. Reconciler loop (`clone_reconcile::run`, every 30 s)

One pass (`reconcile_once`) covers managed, unarchived, id-safe clones whose
container is running. Failures warn once per clone+step (`warned` set) and retry
next pass; keys for vanished clones are dropped. An unresolvable control host skips
the whole pass (warned globally) rather than rewriting the fleet into a degraded URL.

- Archived-state sweep first: thaw paused-but-not-archived clones (an unreadable
  pause state logs at debug and skips); stop archived-but-up ones. Skips clones
  mid-rebase-swap and clones being committed
  (thawing/env-syncing those would corrupt the swap/snapshot).
- SSH ready: dirs + host keys + `authorized_keys`, version-stamped (`SSH_STAMP_VERSION`).
- `/etc/environment` sync: control env + per-clone identity key + preset env +
  `ANTHROPIC_MODEL`, content-compared; restarts `agent-wrapper` only on a real
  change (a blind restart would interrupt an in-flight chat turn every 30 s).
- Codex CLI install (idempotent script, runs every pass).
- Codex parity files, content-stamped over entries PLUS the prepare script (one
  value source shared with the create path — a stamp mismatch used to force one
  redundant re-push on every fresh clone).
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
