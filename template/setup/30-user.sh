#!/usr/bin/env bash
# Phase 30 — the clone user + everything under its home. Cheapest layer → runs last.
# Env (Dockerfile ARGs): USERNAME, CLONE_SOCKET.
set -euo pipefail
. /setup/lib.sh
enable_err_trap

: "${USERNAME:?USERNAME is required}"
: "${CLONE_SOCKET:?CLONE_SOCKET is required}"
# On the base template the password equals the username.
PASSWORD="$USERNAME"

# Single 1080p headless mode, always. (No monitor configuration: the daemon falls back
# to the same 1920x1080 when RMNG_MONITORS is absent, and the server pushes the active
# layout after connect.)
MODE_SPECS="1920x1080"

# Service binaries live in /opt/rmng/bin, NOT baked — installed pre-boot, per clone.
BINDIR=/opt/rmng/bin

log "create user $USERNAME + groups + linger"
# The docker base squats on uid 1000 (`ubuntu`) — evict it, pin ours.
if id ubuntu >/dev/null 2>&1 && [ "$USERNAME" != ubuntu ]; then
  userdel -r ubuntu
fi
id "$USERNAME" >/dev/null 2>&1 || useradd -m -s /bin/bash -u 1000 "$USERNAME"
# sshd StrictModes perms, or it silently ignores the injected key.
install -d -o "$USERNAME" -g "$USERNAME" -m700 "/home/$USERNAME/.ssh"
usermod -aG sudo,render,video "$USERNAME"
# Assert docker group exists — else clones ship unable to docker without sudo.
getent group docker && usermod -aG docker "$USERNAME"
printf '%s:%s\n' "$USERNAME" "$PASSWORD" | chpasswd
printf 'root:%s\n' "$PASSWORD" | chpasswd
printf '%s ALL=(ALL) NOPASSWD:ALL\n' "$USERNAME" >"/etc/sudoers.d/$USERNAME"
chmod 0440 "/etc/sudoers.d/$USERNAME"
# loginctl needs a bus (absent at build) — touch its linger marker directly.
mkdir -p /var/lib/systemd/linger && touch "/var/lib/systemd/linger/$USERNAME"

FISH_SH="$(command -v fish)"
for u in "$USERNAME" root; do usermod -s "$FISH_SH" "$u"; done

# ~/.local/bin on PATH for fish, login shells, non-login bash. Idempotent guards.
log "PATH: add ~/.local/bin for interactive fish + bash"
install -d -m0755 /etc/fish/conf.d
cat >/etc/fish/conf.d/rmng-local-bin.fish <<'FISH'
if test -d "$HOME/.local/bin"; and not contains -- "$HOME/.local/bin" $PATH
    set -gx PATH "$HOME/.local/bin" $PATH
end
FISH
cat >/etc/profile.d/rmng-local-bin.sh <<'SH'
if [ -d "$HOME/.local/bin" ]; then
  case ":$PATH:" in
    *":$HOME/.local/bin:"*) : ;;
    *) PATH="$HOME/.local/bin:$PATH" ;;
  esac
fi
SH
sed -i '/# >>> rmng-local-bin >>>/,/# <<< rmng-local-bin <<</d' /etc/bash.bashrc 2>/dev/null || true
cat >>/etc/bash.bashrc <<'SH'
# >>> rmng-local-bin >>>
if [ -d "$HOME/.local/bin" ]; then
  case ":$PATH:" in
    *":$HOME/.local/bin:"*) : ;;
    *) PATH="$HOME/.local/bin:$PATH" ;;
  esac
fi
# <<< rmng-local-bin <<<
SH

# Server-managed bashrc snippets: whole files, overwrite-idempotent.
grep -q 'bash\.bashrc\.d' /etc/bash.bashrc 2>/dev/null || cat >>/etc/bash.bashrc <<'SH'
# >>> rmng-dropins >>>: server-managed snippets, see /etc/bash.bashrc.d/
for f in /etc/bash.bashrc.d/*.sh; do [ -r "$f" ] && . "$f"; done
# <<< rmng-dropins <<<
SH

# Empty-password keyring so Secret Service apps never prompt. Cleartext is fine here.
log "passwordless gnome-keyring (no Secret Service prompt for Chrome/etc.)"
KRDIR="/home/$USERNAME/.local/share/keyrings"
install -d -o "$USERNAME" -g "$USERNAME" -m700 "$KRDIR"
# install -d leaves parents root-owned — chown them or user writes fail.
chown "$USERNAME:$USERNAME" "/home/$USERNAME/.local" "/home/$USERNAME/.local/share"
cat >"$KRDIR/login.keyring" <<'KEYRING'
[keyring]
display-name=Login
ctime=0
mtime=0
lock-on-idle=false
lock-after=false
KEYRING
printf 'login' >"$KRDIR/default"
chown "$USERNAME:$USERNAME" "$KRDIR/login.keyring" "$KRDIR/default"
chmod 600 "$KRDIR/login.keyring" "$KRDIR/default"

# Autostart never fires headless (no gnome-session) — bake the dirs instead.
log "XDG user dirs (Downloads/Documents/…; autostart never runs headless)"
runuser -u "$USERNAME" -- env HOME="/home/$USERNAME" xdg-user-dirs-update

install -d -m0755 "$BINDIR"

# Inner pipefail: a failed curl must fail the build, not ship a clueless template.
log "install standalone claude CLI (no node)"
runuser -u "$USERNAME" -- bash -lc 'set -o pipefail; command -v claude >/dev/null 2>&1 || curl -fsSL https://claude.ai/install.sh | bash'

log "install standalone codex CLI (no node)"
runuser -u "$USERNAME" -- bash -lc 'set -o pipefail; command -v codex >/dev/null 2>&1 || CODEX_NON_INTERACTIVE=1 curl -fsSL https://chatgpt.com/codex/install.sh | sh'

# Template owns DIRECTORIES (correct owner first — tar invents missing parents
# root-owned); the server writes ALL content at creation. Never the reverse.
log "shared user agent config dirs (codex + pi, dirs only)"
CLAUDE_DIR="/home/$USERNAME/.claude"
install -d -o "$USERNAME" -g "$USERNAME" -m700 "$CLAUDE_DIR"
CODEX_DIR="/home/$USERNAME/.codex"
install -d -o "$USERNAME" -g "$USERNAME" -m700 "$CODEX_DIR"
install -d -o "$USERNAME" -g "$USERNAME" -m700 "/home/$USERNAME/.pi" "/home/$USERNAME/.pi/agent"
install -d -o "$USERNAME" -g "$USERNAME" -m755 "/home/$USERNAME/.config" "/home/$USERNAME/.config/rmng"
install -d -o "$USERNAME" -g "$USERNAME" -m755 "/home/$USERNAME/.claude/skills/rmng-cli" "/home/$USERNAME/.agents/skills/rmng-cli"
install -d -o "$USERNAME" -g "$USERNAME" -m755 "/home/$USERNAME/.cursor" "/home/$USERNAME/.cursor/rules"
install -d -o "$USERNAME" -g "$USERNAME" -m755 "/home/$USERNAME/.rmng"
install -d -m755 /etc/rmng

# `~/.claude.json` is the server's (MCP merge owns it). Not baked.

# A silent installer miss must fail the BUILD, not a clone later.
log "assert user CLI present (claude)"
test -x "/home/$USERNAME/.local/bin/claude"

log "systemd --user units: headless gnome-shell + clone-daemon + agent-wrapper"
UDIR="/home/$USERNAME/.config/systemd/user"
install -d -o "$USERNAME" -g "$USERNAME" "$UDIR"
cat >"$UDIR/gnome-headless.service" <<UNIT
[Unit]
Description=Headless GNOME Shell (no GDM/g-r-d)
# No gnome-session, so pull graphical-session.target in ourselves.
Wants=graphical-session.target
Before=graphical-session.target
[Service]
Type=simple
Environment=XDG_SESSION_TYPE=wayland
Environment=MUTTER_DEBUG_DUMMY_MODE_SPECS=$MODE_SPECS
# No screen reader headless: skips the at-spi bus + registry (~35ms) per shell start.
Environment=NO_AT_BRIDGE=1
ExecStart=/usr/bin/gnome-shell --headless --wayland
Restart=on-failure
[Install]
WantedBy=default.target
UNIT
cat >"$UDIR/rmng-clone-daemon.service" <<UNIT
[Unit]
Description=rmng clone-daemon (capture + input)
# Wants, never BindsTo: daemon restarts must NOT take the session down.
# No After: the daemon starts WITH the shell, not after it — it retries the media
# socket and the holder socket in userspace, so an early start only waits there
# instead of holding the whole boot behind shell + holder setup.
Wants=gnome-headless.service rmng-session-holder.service
[Service]
Type=simple
Environment=WAYLAND_DISPLAY=wayland-0
# Socket path from $CLONE_SOCKET (shared volume, same path both sides).
Environment=RMNG_SOCKET=$CLONE_SOCKET
ExecStart=$BINDIR/rmng-clone-daemon
Restart=on-failure
RestartSec=2
[Install]
WantedBy=default.target
UNIT
cat >"$UDIR/agent-wrapper.service" <<UNIT
[Unit]
Description=rmng agent-wrapper (pi coding agent on :4096)
After=gnome-headless.service
[Service]
Type=simple
# Self-contained Bun binary (pi embedded); pushed Codex token authorizes it.
Environment=PATH=/home/$USERNAME/.local/bin:$BINDIR:/usr/local/bin:/usr/bin:/bin
Environment=AGENT_PORT=4096
ExecStart=$BINDIR/agent-wrapper
Restart=on-failure
RestartSec=2
[Install]
WantedBy=default.target
UNIT

# /etc/environment is the server's (pre-boot tar + reconciler). Not baked.

chown -R "$USERNAME:$USERNAME" "/home/$USERNAME/.config"
# No user bus at build — symlink wants directly; linger starts the manager on boot.
WANTS="$UDIR/default.target.wants"
install -d -o "$USERNAME" -g "$USERNAME" "$WANTS"
for u in gnome-headless rmng-clone-daemon agent-wrapper; do
  ln -sf "../$u.service" "$WANTS/$u.service"
done
chown -h "$USERNAME:$USERNAME" "$WANTS"/*.service

log "phase 30 complete"
