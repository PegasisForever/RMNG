#!/usr/bin/env bash
# Phase 30 — the clone user + everything under its home. Creates the uid-1000 user (groups,
# passwordless sudo, linger), sets fish as the shell, drops the interactive PATH rc, the
# passwordless GNOME keyring, the config dirs the control-server fills (Codex/pi guidance
# + MCP), installs the user
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
# the binaries themselves are NOT baked into the template — the control-server installs its own
# current copies before each clone boots (provision.rs CLONE_BINARIES). This script just creates
# the (empty) dir with the intended perms.
BINDIR=/opt/rmng/bin

log "create user $USERNAME + groups + linger"
# The ubuntu:26.04 DOCKER image ships a stock `ubuntu` user squatting on uid 1000 (the LXC
# templates never did). Everything downstream (tar ownership, XDG_RUNTIME_DIR paths, unit
# files) assumes $USERNAME == uid 1000, so evict it and pin the uid explicitly.
if id ubuntu >/dev/null 2>&1 && [ "$USERNAME" != ubuntu ]; then
  userdel -r ubuntu
fi
id "$USERNAME" >/dev/null 2>&1 || useradd -m -s /bin/bash -u 1000 "$USERNAME"
# SSH: the control-server injects authorized_keys here at provision. Pre-create the dir
# with the exact perms/owner sshd StrictModes requires (else it silently ignores the key).
install -d -o "$USERNAME" -g "$USERNAME" -m700 "/home/$USERNAME/.ssh"
usermod -aG sudo,render,video "$USERNAME"
# docker group exists because docker-ce installed in the toolbox phase (which now fails the
# build on error); assert it here so a regression fails loudly instead of shipping clones
# whose user cannot run docker without sudo.
getent group docker && usermod -aG docker "$USERNAME"
printf '%s:%s\n' "$USERNAME" "$PASSWORD" | chpasswd
printf 'root:%s\n' "$PASSWORD" | chpasswd
printf '%s ALL=(ALL) NOPASSWD:ALL\n' "$USERNAME" > "/etc/sudoers.d/$USERNAME"; chmod 0440 "/etc/sudoers.d/$USERNAME"
# Enable linger by touching the marker file directly — `loginctl enable-linger` needs a
# running systemd bus, which isn't up during `docker build`. This is exactly what loginctl
# writes; the user manager auto-starts on first boot of a real clone.
mkdir -p /var/lib/systemd/linger && touch "/var/lib/systemd/linger/$USERNAME"

# Default shell → fish for the clone user + root. Fish installed in the dev toolbox phase,
# which fails the build on error — so it is guaranteed present here, not probed.
FISH_SH="$(command -v fish)"
for u in "$USERNAME" root; do usermod -s "$FISH_SH" "$u"; done

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

# XDG user dirs (Downloads, Documents, Desktop, …). Normally created at first login by the
# /etc/xdg/autostart/xdg-user-dirs.desktop entry — but this template launches
# `gnome-shell --headless` DIRECTLY, with no gnome-session to process autostart, so that
# never fires and Nautilus/Files opens on a bare home with no standard folders. Bake them at
# build time instead: xdg-user-dirs-update reads /etc/xdg/user-dirs.defaults, creates the
# dirs, and writes ~/.config/user-dirs.dirs (the XDG_*_DIR map apps resolve). Run as the user
# so both the dirs and the config file land user-owned; HOME is set explicitly since runuser
# doesn't. Load-bearing under set -e: if xdg-user-dirs (a gnome-session Recommends) ever
# stops shipping, this fails the build here rather than silently regressing to a bare home.
log "XDG user dirs (Downloads/Documents/…; autostart never runs under headless gnome-shell)"
runuser -u "$USERNAME" -- env HOME="/home/$USERNAME" xdg-user-dirs-update

# /opt/rmng/bin: created EMPTY here with the intended 0755 root:root perms. The clone-daemon +
# agent-wrapper binaries are installed by the control-server at clone-create time (pre-boot),
# not baked into the template — see provision.rs CLONE_BINARIES.
install -d -m0755 "$BINDIR"

# Claude Code installs standalone (self-contained binary, no system node) → ~/.local/bin/claude.
# Load-bearing: the agent-wrapper drives this CLI, so a failed install fails the build. The
# inner `set -o pipefail` is essential: without it a failed curl feeds bash EMPTY stdin,
# which exits 0 — and the build would silently publish a template with no claude at all
# (seen live when the build box's egress blipped).
log "install standalone claude CLI (no node)"
runuser -u "$USERNAME" -- bash -lc 'set -o pipefail; command -v claude >/dev/null 2>&1 || curl -fsSL https://claude.ai/install.sh | bash'

# Codex CLI installs standalone (self-contained binary, no node) → ~/.local/bin/codex.
# Strict like everything else in the template: a failed install fails the build.
# This is the SOLE source of the binary — the control-server no longer installs or
# repairs it post-boot (install-if-missing scripts in three places were one truth in
# three copies; the image won). Idempotent (skips if already present).
log "install standalone codex CLI (no node)"
runuser -u "$USERNAME" -- bash -lc 'set -o pipefail; command -v codex >/dev/null 2>&1 || CODEX_NON_INTERACTIVE=1 curl -fsSL https://chatgpt.com/codex/install.sh | sh'

# The clone user's agent config dirs. The control-server writes ALL files under them at
# clone creation (codex parity + MCP merges) and the reconciler keeps them current — the
# template owns the directories (correct owner before anything lands, else tar invents
# them root-owned) and never the content. Every parent the parity tar writes into must
# exist here: Docker's tar extract invents a missing parent as root:root and the agent
# then cannot write beside the placed file (seen live with ~/.pi/agent/AGENTS.md — the
# wrapper could not write its MCP tool cache, so desktop tools never promoted).
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

# `~/.claude.json` is the control-server's: its MCP merge creates the file (`{}` when
# missing) and owns the managed servers, at creation and in the loop. Not baked.

# uv + rustup + nvm (load-bearing user toolchains), then fish-nvm (shell glue, strict like
# everything else in the template),
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
  # Strict: fish is guaranteed by the toolbox phase, so a failure here is real.
  # </dev/null: fisher must not inherit this script's stdin.
  runuser -u "$USERNAME" -- fish -c 'curl -sL https://raw.githubusercontent.com/jorgebucaran/fisher/main/functions/fisher.fish | source && fisher install jorgebucaran/fisher edc/bass FabioAntunes/fish-nvm' </dev/null
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
# The holder owns the Mutter session this daemon captures and injects through, and stays up
# across the daemon's restarts so window positions survive an update. Wants, never BindsTo:
# restarting the daemon must NOT take the holder (and the clone's monitors) with it.
After=gnome-headless.service rmng-session-holder.service
Wants=gnome-headless.service rmng-session-holder.service
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
Description=rmng agent-wrapper (pi coding agent on :4096)
After=gnome-headless.service
[Service]
Type=simple
# Self-contained Bun binary; pi is embedded, and the clone's pushed Codex token authorizes it.
Environment=PATH=/home/$USERNAME/.local/bin:$BINDIR:/usr/local/bin:/usr/bin:/bin
Environment=AGENT_PORT=4096
ExecStart=$BINDIR/agent-wrapper
Restart=on-failure
RestartSec=2
[Install]
WantedBy=default.target
UNIT

# /etc/environment is the control-server's: its pre-boot tar writes the same six
# session keys plus the per-clone control/preset vars, and the reconciler keeps
# them current. Not baked.

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
