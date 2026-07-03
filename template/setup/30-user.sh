#!/usr/bin/env bash
# Phase 30 — the clone user + everything under its home. Creates the uid-1000 user (groups,
# passwordless sudo, linger), sets fish as the shell, drops the interactive PATH rc, the
# passwordless GNOME keyring, the shared CLAUDE.md + the linear MCP, installs the user
# toolchains (claude / uv / rustup / nvm / fish-nvm), and writes the systemd --user units
# (headless gnome-shell + clone-daemon + agent-wrapper) with their wants symlinks. Cheapest,
# most-frequently-tweaked layer → runs last, so a change here never re-runs phases 10/20.
#
# Env (from the Dockerfile ARGs on the RUN line): USERNAME, MONITORS, CLONE_SOCKET.
set -euo pipefail
. /setup/lib.sh

: "${USERNAME:?USERNAME is required}"
: "${CLONE_SOCKET:?CLONE_SOCKET is required}"
# MONITORS is optional (CSV "WxH+X+Y[*]"): empty ⇒ a single 1920x1080 dummy mode + the unit
# omits RMNG_MONITORS. On the base template it is set by the ARG default.
MONITORS="${MONITORS:-}"
# On the base template the password equals the username (the old exec path passed
# `<username> <username>`; there is no separate password on a template image).
PASSWORD="$USERNAME"

# Monitor layout → the headless dummy backend's mode specs. Each entry is WxH+X+Y[*]; the
# specs want just WxH (unique, colon-joined).
if [ -n "$MONITORS" ]; then
  MODE_SPECS="$(printf '%s' "$MONITORS" | tr ',' '\n' | sed -E 's/\+.*$//; s/\*$//' | awk 'NF && !seen[$0]++' | paste -sd: -)"
else
  MODE_SPECS="1920x1080"
fi

# Service binaries live in /opt/rmng/bin (root-owned, 755) — NOT the user's home, so they
# don't clutter the Files/Nautilus Home view. The systemd --user units exec them from here;
# the binaries themselves land via `COPY --from` after this script (a daemon rebuild busts
# only those cheap layers).
BINDIR=/opt/rmng/bin

log "create user $USERNAME + groups + linger"
# The ubuntu:26.04 DOCKER image ships a stock `ubuntu` user squatting on uid 1000 (the LXC
# templates never did). Everything downstream (tar ownership, XDG_RUNTIME_DIR paths, unit
# files) assumes $USERNAME == uid 1000, so evict it and pin the uid explicitly.
if id ubuntu >/dev/null 2>&1 && [ "$USERNAME" != ubuntu ]; then
  userdel -r ubuntu 2>/dev/null || userdel ubuntu 2>/dev/null || warn "could not remove stock ubuntu user"
fi
id "$USERNAME" >/dev/null 2>&1 || useradd -m -s /bin/bash -u 1000 "$USERNAME"
usermod -aG sudo,render,video "$USERNAME"
# docker group exists once docker-ce installed in the toolbox above; add the user so they can
# run docker without sudo. Non-fatal if the group is absent (docker install failed).
getent group docker >/dev/null 2>&1 && usermod -aG docker "$USERNAME" || warn "docker group absent; not added"
printf '%s:%s\n' "$USERNAME" "$PASSWORD" | chpasswd
printf 'root:%s\n' "$PASSWORD" | chpasswd
printf '%s ALL=(ALL) NOPASSWD:ALL\n' "$USERNAME" > "/etc/sudoers.d/$USERNAME"; chmod 0440 "/etc/sudoers.d/$USERNAME"
# Enable linger by touching the marker file directly — `loginctl enable-linger` needs a
# running systemd bus, which isn't up during `docker build`. This is exactly what loginctl
# writes; the user manager auto-starts on first boot of a real clone.
mkdir -p /var/lib/systemd/linger && touch "/var/lib/systemd/linger/$USERNAME"

# Default shell → fish for the clone user + root (fish installed in the dev toolbox above; it
# registers itself in /etc/shells so chsh accepts it). Non-fatal if fish is missing (toolbox
# is best-effort) — the shell then stays bash.
FISH_SH="$(command -v fish || true)"
if [ -n "$FISH_SH" ]; then
  for u in "$USERNAME" root; do chsh -s "$FISH_SH" "$u" 2>/dev/null || usermod -s "$FISH_SH" "$u" || warn "set fish shell for $u"; done
fi

# ~/.local/bin + ~/.cargo/bin on PATH for interactive shells. User-local tools install there
# — Claude Code / uv → ~/.local/bin, rustup/cargo → ~/.cargo/bin — but neither fish (the
# clones' default shell, set above) nor a non-login bash puts them on PATH, so the tools
# aren't found in a terminal even though the agent-wrapper unit hardcodes ~/.local/bin.
# Cover every fish shell (conf.d), login sh/bash (profile.d), and non-login interactive bash
# (/etc/bash.bashrc). Guards keep it idempotent and skip dirs until they're created.
log "PATH: add ~/.local/bin + ~/.cargo/bin for interactive fish + bash"
install -d -m0755 /etc/fish/conf.d
cat > /etc/fish/conf.d/rmng-local-bin.fish <<'FISH'
for d in "$HOME/.local/bin" "$HOME/.cargo/bin"
    if test -d "$d"; and not contains -- "$d" $PATH
        set -gx PATH "$d" $PATH
    end
end
FISH
cat > /etc/profile.d/rmng-local-bin.sh <<'SH'
# User-local tools: Claude Code / uv → ~/.local/bin, rustup/cargo → ~/.cargo/bin.
for d in "$HOME/.local/bin" "$HOME/.cargo/bin"; do
  [ -d "$d" ] || continue
  case ":$PATH:" in
    *":$d:"*) : ;;
    *) PATH="$d:$PATH" ;;
  esac
done
SH
# Non-login interactive bash sources /etc/bash.bashrc (not profile.d). Delete any prior rmng
# block (marker-delimited) then re-append, so re-provisioning stays idempotent.
sed -i '/# >>> rmng-local-bin >>>/,/# <<< rmng-local-bin <<</d' /etc/bash.bashrc 2>/dev/null || true
cat >> /etc/bash.bashrc <<'SH'
# >>> rmng-local-bin >>>
# user-local tools (Claude Code / uv → ~/.local/bin, rustup/cargo → ~/.cargo/bin); add for
# non-login interactive bash (login shells get these via /etc/profile.d/rmng-local-bin.sh).
for d in "$HOME/.local/bin" "$HOME/.cargo/bin"; do
  [ -d "$d" ] || continue
  case ":$PATH:" in
    *":$d:"*) : ;;
    *) PATH="$d:$PATH" ;;
  esac
done
# <<< rmng-local-bin <<<
SH

# Passwordless GNOME keyring. The headless session has no login password to unlock a keyring,
# so the first Secret Service client (Chrome, VS Code, etc.) pops a "Choose password for new
# keyring" dialog. Pre-create an empty-password login keyring — the unencrypted, never-locked
# [keyring] text format — and alias it as the default collection, so every Secret Service app
# works silently. Secrets land in cleartext on disk, which is fine for an ephemeral
# remote-desktop clone.
log "passwordless gnome-keyring (no Secret Service prompt for Chrome/etc.)"
KRDIR="/home/$USERNAME/.local/share/keyrings"
install -d -o "$USERNAME" -g "$USERNAME" -m700 "$KRDIR"
# install -d does NOT chown the intermediate parents it creates, so ~/.local and
# ~/.local/share would be left root-owned — which blocks the user's own writes there (notably
# the claude installer's `mkdir ~/.local/share/claude` → EACCES). Chown them.
chown "$USERNAME:$USERNAME" "/home/$USERNAME/.local" "/home/$USERNAME/.local/share"
cat > "$KRDIR/login.keyring" <<'KEYRING'
[keyring]
display-name=Login
ctime=0
mtime=0
lock-on-idle=false
lock-after=false
KEYRING
printf 'login' > "$KRDIR/default"
chown "$USERNAME:$USERNAME" "$KRDIR/login.keyring" "$KRDIR/default"
chmod 600 "$KRDIR/login.keyring" "$KRDIR/default"

# /opt/rmng/bin (the COPY --from lands the binaries here after this script; create it now so
# the perms are the intended 0755 root:root).
install -d -m0755 "$BINDIR"

# Claude Code installs standalone (self-contained binary, no system node) → ~/.local/bin/claude.
# Load-bearing: the agent-wrapper drives this CLI, so a failed install fails the build. The
# inner `set -o pipefail` is essential: without it a failed curl feeds bash EMPTY stdin,
# which exits 0 — and the build would silently publish a template with no claude at all
# (seen live when the build box's egress blipped).
log "install standalone claude CLI (no node)"
runuser -u "$USERNAME" -- bash -lc 'set -o pipefail; command -v claude >/dev/null 2>&1 || curl -fsSL https://claude.ai/install.sh | bash'

# Codex CLI installs standalone (self-contained binary, no node) → ~/.local/bin/codex.
# Warn-only: unlike claude, the agent-wrapper does not require codex, so a failed install
# must not fail the template build. Idempotent (skips if already present).
log "install standalone codex CLI (no node)"
runuser -u "$USERNAME" -- bash -lc 'set -o pipefail; command -v codex >/dev/null 2>&1 || CODEX_NON_INTERACTIVE=1 curl -fsSL https://chatgpt.com/codex/install.sh | sh' \
  || warn "codex install failed; codex accounts will be unavailable on clones from this template"

# Shared user CLAUDE.md — operating memory read by EVERY `claude` on this clone: the
# agent-wrapper's SDK agent (settingSources: ["user"]), the Claude Code it drives inside
# Cursor to implement tickets, and any interactive `claude` a human opens. General
# engineering guidance ONLY — deliberately NOT the desktop operating notes or the ticket
# procedure (those are baked into the agent-wrapper and injected as its system prompt; the
# ticket procedure must never reach the inner Cursor agent or it would recursively try to
# open Cursor). install -d is idempotent — the claude installer already made ~/.claude.
log "shared user CLAUDE.md (agent operating memory)"
CLAUDE_DIR="/home/$USERNAME/.claude"
install -d -o "$USERNAME" -g "$USERNAME" -m700 "$CLAUDE_DIR"
cat > "$CLAUDE_DIR/CLAUDE.md" <<'CLAUDEMD'
# Working in this clone

This machine is a **disposable, single-purpose dev sandbox** that belongs to you,
with **passwordless `sudo`**. Install packages, toolchains, and global CLIs freely
and reconfigure the system as needed — the machine itself is throwaway and there is
no other user to disturb. Optimize for getting the task done.

## When you're blocked

If you're genuinely stuck — missing access or credentials, an ambiguous
requirement, or a call that's the human's to make — **stop and ask** rather than
guessing or thrashing. A precise question beats a confident wrong turn.
CLAUDEMD
chown "$USERNAME:$USERNAME" "$CLAUDE_DIR/CLAUDE.md"
chmod 644 "$CLAUDE_DIR/CLAUDE.md"

# User-scope `linear` MCP for every `claude` on the clone (interactive shell, inner Cursor
# agent; the agent-wrapper registers the same server programmatically). mcpServers lives in
# ~/.claude.json — a top-level key; settings.json does NOT support it. ${LINEAR_API_KEY}
# stays literal here (single-quoted jq arg): claude expands it at runtime from the session
# env, where the per-clone 30-rmng-preset.conf (written by the control-server) put the chosen
# preset's key. No key in the env (e.g. on the base image) ⇒ claude skips the server with a
# "missing environment variables" warning.
log "user-scope linear MCP → ~/.claude.json"
CLAUDE_JSON="/home/$USERNAME/.claude.json"
[ -s "$CLAUDE_JSON" ] || echo '{}' > "$CLAUDE_JSON"
jq --arg auth 'Bearer ${LINEAR_API_KEY}' \
  '.mcpServers.linear = {"type":"http","url":"https://mcp.linear.app/mcp","headers":{"Authorization":$auth}}' \
  "$CLAUDE_JSON" > "$CLAUDE_JSON.tmp" && mv "$CLAUDE_JSON.tmp" "$CLAUDE_JSON"
chown "$USERNAME:$USERNAME" "$CLAUDE_JSON"
chmod 600 "$CLAUDE_JSON"

# uv + rustup + nvm (load-bearing user toolchains), then fish-nvm (best-effort shell glue),
# all installed as the clone user. Subshell cd's to the user's home so fisher can getcwd
# (root's cwd isn't readable by the user → fisher would otherwise spew "Unable to open the
# current working directory"). Each load-bearing installer runs with an inner
# `set -o pipefail` (same reasoning as the claude install above: a failed curl must not
# silently no-op the `| sh`/`| bash` stage).
log "user tools: uv (Astral) + rustup + nvm + fish-nvm"
( cd "/home/$USERNAME" 2>/dev/null || cd /
  # uv — Astral's Python package/venv manager → ~/.local/bin/uv (already on PATH via the
  # shell files written above). UV_NO_MODIFY_PATH: PATH is ours, don't let it touch profiles.
  runuser -u "$USERNAME" -- bash -lc 'set -o pipefail; command -v uv >/dev/null 2>&1 || curl -LsSf https://astral.sh/uv/install.sh | env UV_NO_MODIFY_PATH=1 sh'
  # rustup + latest stable Rust → ~/.cargo + ~/.rustup. --no-modify-path: we own PATH (the
  # rmng-local-bin files above put ~/.cargo/bin on it). Default profile (rustc/cargo/clippy/
  # rustfmt/std). </dev/null so the installer never blocks on this script's stdin.
  runuser -u "$USERNAME" -- bash -lc 'set -o pipefail; command -v rustup >/dev/null 2>&1 || curl --proto "=https" --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --no-modify-path --default-toolchain stable --profile default' </dev/null
  # nvm → ~/.nvm. Force PROFILE=~/.bashrc so its loader lands in bash: the user's default
  # shell is fish, which nvm's installer can't detect and would otherwise skip the profile.
  # (|| true keeps the substitution from tripping set -e/pipefail when the tag lookup fails.)
  NVM_TAG="$(curl -fsSL https://api.github.com/repos/nvm-sh/nvm/releases/latest 2>/dev/null | grep -oE 'v[0-9]+\.[0-9]+\.[0-9]+' | head -1 || true)"
  : "${NVM_TAG:=v0.40.1}"
  runuser -u "$USERNAME" -- bash -lc "set -o pipefail; [ -s \"\$HOME/.nvm/nvm.sh\" ] || { export PROFILE=\"\$HOME/.bashrc\"; curl -o- 'https://raw.githubusercontent.com/nvm-sh/nvm/$NVM_TAG/install.sh' | bash; }"
  # fish-nvm — makes nvm/node/npm/npx/yarn work in fish (the default shell) by lazily
  # sourcing nvm via bass. Bootstrap fisher, then install it + its bass + fish-nvm deps.
  # Best-effort: it depends on fish (a best-effort toolbox app) and node still works in bash
  # via nvm without it. </dev/null: fisher must not inherit this script's stdin.
  runuser -u "$USERNAME" -- fish -c 'curl -sL https://raw.githubusercontent.com/jorgebucaran/fisher/main/functions/fisher.fish | source && fisher install jorgebucaran/fisher edc/bass FabioAntunes/fish-nvm' </dev/null \
    || warn "fish-nvm install failed; node still works in bash via nvm"
)

# Belt-and-braces: assert every load-bearing user toolchain actually landed. An installer
# that exits 0 without producing its artifact (or a future regression of the pipefail
# guards) must fail the BUILD here, not surface later as a template whose agent-wrapper has
# no claude to drive.
log "assert user toolchains present (claude / uv / cargo / nvm)"
test -x "/home/$USERNAME/.local/bin/claude"
test -x "/home/$USERNAME/.local/bin/uv"
test -x "/home/$USERNAME/.cargo/bin/cargo"
test -s "/home/$USERNAME/.nvm/nvm.sh"

log "systemd --user units: headless gnome-shell + clone-daemon + agent-wrapper"
UDIR="/home/$USERNAME/.config/systemd/user"
install -d -o "$USERNAME" -g "$USERNAME" "$UDIR"
cat > "$UDIR/gnome-headless.service" <<UNIT
[Unit]
Description=Headless GNOME Shell (no GDM/g-r-d)
# gnome-session normally reaches graphical-session.target; we run gnome-shell directly, so
# pull it in ourselves — session-bound services (xdg-desktop-portal-gnome, etc.) require it,
# else they fail with a dependency error and portal calls hang on the gtk fallback.
Wants=graphical-session.target
Before=graphical-session.target
[Service]
Type=simple
Environment=XDG_SESSION_TYPE=wayland
Environment=MUTTER_DEBUG_DUMMY_MODE_SPECS=$MODE_SPECS
ExecStart=/usr/bin/gnome-shell --headless --wayland
Restart=on-failure
[Install]
WantedBy=default.target
UNIT
cat > "$UDIR/rmng-clone-daemon.service" <<UNIT
[Unit]
Description=rmng clone-daemon (capture + input)
After=gnome-headless.service
Wants=gnome-headless.service
[Service]
Type=simple
Environment=WAYLAND_DISPLAY=wayland-0
# Ship to the control-server's media socket (path from config cloneSocket, passed in as
# \$CLONE_SOCKET) — its host dir is the shared sock volume mounted at the same path
# (/srv/rmng-sock). Without this the daemon falls back to standalone capture self-test
# (no connection to the server).
Environment=RMNG_SOCKET=$CLONE_SOCKET
${MONITORS:+Environment=RMNG_MONITORS=$MONITORS}
ExecStart=$BINDIR/rmng-clone-daemon
Restart=on-failure
RestartSec=2
[Install]
WantedBy=default.target
UNIT
cat > "$UDIR/agent-wrapper.service" <<UNIT
[Unit]
Description=rmng agent-wrapper (Claude Agent SDK on :4096)
After=gnome-headless.service
[Service]
Type=simple
# Self-contained Bun binary; the SDK drives the standalone claude CLI (~/.local/bin).
Environment=PATH=/home/$USERNAME/.local/bin:/usr/local/bin:/usr/bin:/bin
Environment=AGENT_PORT=4096
ExecStart=$BINDIR/agent-wrapper
Restart=on-failure
RestartSec=2
[Install]
WantedBy=default.target
UNIT

# Base session env every clone gets (NOT a preset): identifies the desktop so apps,
# xdg-desktop-portal (it picks the GNOME backend from XDG_CURRENT_DESKTOP), dark-mode/
# settings portal, and theming behave like a real GNOME session. We launch
# `gnome-shell --headless` directly — no GDM / gnome-session / pam_systemd — so nothing else
# sets these. systemd --user reads environment.d → the session + all user units. Per-clone
# env presets live in 30-rmng-preset.conf (written by the control-server at clone create);
# higher number wins.
ENVDIR="/home/$USERNAME/.config/environment.d"; install -d "$ENVDIR"
cat > "$ENVDIR/10-rmng-session.conf" <<'ENVD'
XDG_CURRENT_DESKTOP=GNOME
XDG_SESSION_DESKTOP=gnome
DESKTOP_SESSION=gnome
XDG_SESSION_CLASS=user
XDG_MENU_PREFIX=gnome-
XDG_SESSION_TYPE=wayland
ENVD

chown -R "$USERNAME:$USERNAME" "/home/$USERNAME/.config"
# Enable for auto-start by creating the wants symlinks directly (a plain `ln`, no bus or
# `systemctl --user enable` needed). During `docker build` there is no user systemd manager,
# so the symlinks are what carry over into the image; they take effect on the first boot of a
# real clone (linger, marked above, starts the user manager then).
WANTS="$UDIR/default.target.wants"; install -d -o "$USERNAME" -g "$USERNAME" "$WANTS"
for u in gnome-headless rmng-clone-daemon agent-wrapper; do
  ln -sf "../$u.service" "$WANTS/$u.service"
done
chown -h "$USERNAME:$USERNAME" "$WANTS"/*.service

log "phase 30 complete"
