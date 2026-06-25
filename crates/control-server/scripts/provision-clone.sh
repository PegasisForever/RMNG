#!/usr/bin/env bash
# In-CT provisioning for a rmng clone/template (runs INSIDE the CT as root).
# Codifies the recipe validated on CT 132 `rmng-build`: vanilla headless GNOME (NO
# gdm3, NO gnome-remote-desktop, NO flatpak) + Mesa VA-API + the `clone-daemon`,
# brought up by a `systemd --user` unit under linger (Mutter's headless backend
# needs only /dev/dri/renderD128 — already passed in the LXC config). Replaces the
# old g-r-d/GDM handover + the computer-use virtual-monitor service.
#
#   provision-clone.sh <username> <password>   (clone-daemon pushed to /root/rmng-clone-daemon)
set -euo pipefail
say(){ echo "    [ct] $*"; }
USERNAME="${1:-pega}"; PASSWORD="${2:-pega}"
# Monitor layout (CSV "WxH,WxH" from config.monitors via bootstrap.sh) → the clone-daemon
# RMNG_MONITORS env (one virtual monitor per entry). The headless dummy backend's mode
# specs must offer each requested size, so derive them (unique sizes, colon-joined).
MONITORS="${3:-}"
if [ -n "$MONITORS" ]; then
  # Each entry is WxH+X+Y[*]; the dummy mode specs want just WxH (unique, colon-joined).
  MODE_SPECS="$(printf '%s' "$MONITORS" | tr ',' '\n' | sed -E 's/\+.*$//; s/\*$//' | awk 'NF && !seen[$0]++' | paste -sd: -)"
else
  MODE_SPECS="1920x1080"
fi
export DEBIAN_FRONTEND=noninteractive

say "apt update + upgrade"
apt-get update -qq && apt-get full-upgrade -y -qq

say "remove snap + disable guest AppArmor"
systemctl disable --now snapd.service snapd.socket 2>/dev/null || true
apt-get purge -y -qq snapd 2>/dev/null || true
rm -rf /var/snap /var/lib/snapd /var/cache/snapd
printf 'Package: snapd\nPin: release *\nPin-Priority: -1\n' > /etc/apt/preferences.d/nosnap.pref
aa-teardown 2>/dev/null || true; systemctl disable --now apparmor 2>/dev/null || true

say "locale + ping range"
apt-get install -y -qq locales >/dev/null 2>&1 || true
locale-gen en_US.UTF-8 >/dev/null 2>&1 || true; update-locale LANG=en_US.UTF-8
mkdir -p /etc/sysctl.d && echo 'net.ipv4.ping_group_range = 0 65534' > /etc/sysctl.d/99-ping.conf

say "headless GNOME + Mutter + VA-API + PipeWire (NO gdm/g-r-d)"
apt-get install -y -qq \
  gnome-session gnome-shell mutter gnome-console nautilus gnome-text-editor \
  dbus-user-session xwayland \
  mesa-va-drivers libva2 va-driver-all vainfo \
  pipewire wireplumber gstreamer1.0-pipewire \
  fonts-cantarell adwaita-icon-theme network-manager >/dev/null

# Patched gnome-shell (shell-01 hide screen-sharing indicator + shell-03 enable
# org.gnome.Shell.Eval for the clone-daemon window-management MCP tools), if the
# control-server pushed it. Installs over the stock shell just apt-installed above;
# the +ngshell version is strictly newer so apt takes it. Non-fatal: without it the
# clone uses stock shell (window-mgmt tools error, share pill shows in capture).
if [ -f /root/gnome-shell-patched.deb ]; then
  say "install patched gnome-shell (shell-01 + shell-03)"
  apt-get install -y -qq --allow-downgrades /root/gnome-shell-patched.deb >/dev/null 2>&1 \
    && say "patched gnome-shell installed: $(dpkg-query -W -f='${Version}' gnome-shell)" \
    || say "WARN: patched gnome-shell install failed; using stock (no window-mgmt MCP)"
  rm -f /root/gnome-shell-patched.deb
else
  say "no patched gnome-shell deb pushed; using stock (window-mgmt MCP unavailable)"
fi

say "create user $USERNAME + groups + linger"
id "$USERNAME" >/dev/null 2>&1 || useradd -m -s /bin/bash "$USERNAME"
usermod -aG sudo,render,video "$USERNAME"
printf '%s:%s\n' "$USERNAME" "$PASSWORD" | chpasswd
printf 'root:%s\n' "$PASSWORD" | chpasswd
printf '%s ALL=(ALL) NOPASSWD:ALL\n' "$USERNAME" > "/etc/sudoers.d/$USERNAME"; chmod 0440 "/etc/sudoers.d/$USERNAME"
loginctl enable-linger "$USERNAME"

say "install clone-daemon"
install -o "$USERNAME" -g "$USERNAME" -m755 /root/rmng-clone-daemon "/home/$USERNAME/rmng-clone-daemon" 2>/dev/null || \
  say "WARN: /root/rmng-clone-daemon not pushed; install it later"

say "install agent-wrapper (Bun single-exec) + standalone claude CLI (no node)"
install -o "$USERNAME" -g "$USERNAME" -m755 /root/agent-wrapper "/home/$USERNAME/agent-wrapper" 2>/dev/null || \
  say "WARN: /root/agent-wrapper not pushed; install it later"
# Claude Code installs standalone (self-contained binary, no system node) → ~/.local/bin/claude.
runuser -u "$USERNAME" -- bash -lc 'command -v claude >/dev/null 2>&1 || curl -fsSL https://claude.ai/install.sh | bash' 2>/dev/null || \
  say "WARN: standalone claude CLI install failed; install it later"

say "systemd --user units: headless gnome-shell + clone-daemon"
UDIR="/home/$USERNAME/.config/systemd/user"
install -d -o "$USERNAME" -g "$USERNAME" "$UDIR"
cat > "$UDIR/gnome-headless.service" <<UNIT
[Unit]
Description=Headless GNOME Shell (no GDM/g-r-d)
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
# Ship to the control-server's media socket — the shared host dir /srv/rmng-sock is
# bind-mounted at the same path (see bootstrap.sh / the deploy CT mp0). Without this
# the daemon falls back to standalone capture self-test (no connection to the server).
Environment=RMNG_SOCKET=/srv/rmng-sock/clones.sock
${MONITORS:+Environment=RMNG_MONITORS=$MONITORS}
ExecStart=/home/$USERNAME/rmng-clone-daemon
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
ExecStart=/home/$USERNAME/agent-wrapper
Restart=on-failure
RestartSec=2
[Install]
WantedBy=default.target
UNIT
chown -R "$USERNAME:$USERNAME" "/home/$USERNAME/.config"
uid="$(id -u "$USERNAME")"
# Enable for auto-start by creating the wants symlinks directly — `systemctl --user
# enable` is unreliable during provisioning (the user manager may not be up yet).
WANTS="$UDIR/default.target.wants"; install -d -o "$USERNAME" -g "$USERNAME" "$WANTS"
for u in gnome-headless rmng-clone-daemon agent-wrapper; do
  ln -sf "../$u.service" "$WANTS/$u.service"
done
chown -h "$USERNAME:$USERNAME" "$WANTS"/*.service
SC="runuser -u $USERNAME -- env XDG_RUNTIME_DIR=/run/user/$uid systemctl --user"
$SC daemon-reload 2>/dev/null || true
# Start now too — the user manager came up at enable-linger time (before the units
# were linked), so the wants symlinks alone won't start them until the next boot.
$SC start gnome-headless.service 2>/dev/null || true
sleep 2
$SC start rmng-clone-daemon.service agent-wrapper.service 2>/dev/null || true

say "provision complete; render node:"; ls -l /dev/dri || true
echo "RESULT ok"
