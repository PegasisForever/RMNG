#!/usr/bin/env bash
# Phase 10 — base desktop: headless GNOME + Mutter + VA-API + PipeWire (NO gdm3,
# g-r-d, flatpak), Recommends strip, container masks. Rarest layer → runs first.
set -euo pipefail
. /setup/lib.sh
enable_err_trap

log "apt update + full-upgrade"
apt-get update -qq && apt-get full-upgrade -y -qq

log "pin snapd out"
printf 'Package: snapd\nPin: release *\nPin-Priority: -1\n' >/etc/apt/preferences.d/nosnap.pref

log "locale + timezone + ping range"
apt-get install -y -qq locales
locale-gen en_US.UTF-8
update-locale LANG=en_US.UTF-8
# timedatectl can't set the clock inside a container: symlink + /etc/timezone.
ln -sf /usr/share/zoneinfo/America/Toronto /etc/localtime
echo America/Toronto >/etc/timezone
mkdir -p /etc/sysctl.d && echo 'net.ipv4.ping_group_range = 0 65534' >/etc/sysctl.d/99-ping.conf

# A clone runs privileged with NO user namespace, so its systemd-sysctl writes the HOST's
# sysctls -- inotify limits live in the init userns ucounts, and the container shares it.
# GNOME's localsearch ships /usr/lib/sysctl.d/30-localsearch.conf pinning
#   fs.inotify.max_user_watches = 65536
# which was written to RAISE the old 8192 default and now silently LOWERS a host that has
# deliberately raised it. Every clone start stomped the Proxmox host back to 65536, and a
# host short of inotify watches fails to boot CTs: systemd-networkd exits
# `code: 28 (No space left on device)` and the container comes up with no IP. Chased three
# times in docs/RUNBOOK-GEN1-TO-GEN2.md §2.3 before the cause was found.
# Mask it the systemd way -- /etc/ wins over /usr/lib/ by name, and a /dev/null symlink
# drops the file entirely. Survives a localsearch package upgrade; `rm` would not.
ln -sf /dev/null /etc/sysctl.d/30-localsearch.conf

log "headless GNOME + Mutter + VA-API + PipeWire (NO gdm/g-r-d)"
# vapostproc (screenshot encode) is plugins-bad, pngenc -good, base in -base. sudo +
# openssh-server: the docker image ships neither.
apt-get install -y -qq \
  sudo \
  gnome-session gnome-shell mutter ptyxis nautilus gnome-text-editor loupe \
  dbus-user-session xwayland \
  mesa-va-drivers libva2 va-driver-all vainfo \
  pipewire wireplumber gstreamer1.0-pipewire \
  gstreamer1.0-plugins-base gstreamer1.0-plugins-good gstreamer1.0-plugins-bad \
  fonts-cantarell adwaita-icon-theme jq \
  openssh-server

update-alternatives --set x-terminal-emulator /usr/bin/ptyxis 2>/dev/null

# Symlink, not file: a baked /var/lib/dbus/machine-id would give the whole fleet one id.
ln -sf /etc/machine-id /var/lib/dbus/machine-id

# Mask ModemManager: its D-Bus activation hangs Settings clients ~25s per call.
log "mask ModemManager (D-Bus activation otherwise hangs Settings)"
systemctl mask ModemManager.service >/dev/null 2>&1

# Mask rtkit: wedged in containers it stalls the portal and every GTK launch eats ~25s.
log "mask rtkit-daemon (wedges portal → slow GTK launches)"
systemctl mask rtkit-daemon.service >/dev/null 2>&1

# Mask udev: systemd-udevd in a privileged container sees the HOST's uevents.
log "mask systemd-udevd + udev-trigger (host uevents)"
systemctl mask systemd-udevd.service systemd-udev-trigger.service >/dev/null 2>&1

# Mask tpm-udev: boot /dev churn trips its start limit → permanently "degraded" system.
log "mask tpm-udev (no TPM in a container)"
systemctl mask tpm-udev.path tpm-udev.service >/dev/null 2>&1

# /tmp stays on container disk (agents use it for large scratch), not tmpfs.
log "mask tmp.mount (/tmp on regular disk)"
systemctl mask tmp.mount >/dev/null 2>&1

# gnome-shell Recommends gdm3/g-r-d back in; the desktop chain reinstalls NetworkManager
# (which would DHCP eth0 into oblivion — Docker owns the network). Purge all four.
# Keep iproute2: autoremove would otherwise sweep `ip` out with the chain.
log "strip gdm3 + g-r-d + NetworkManager/ModemManager (Recommends pull-ins); go DM-less"
apt-get purge -y -qq gdm3 gnome-remote-desktop network-manager modemmanager >/dev/null 2>&1
apt-mark manual iproute2 >/dev/null 2>&1
apt-get autoremove --purge -y -qq >/dev/null 2>&1
# No DM → multi-user.target; the headless user unit starts via linger, no DM needed.
systemctl set-default multi-user.target >/dev/null 2>&1

# DM-less ⇒ no login session, so polkit cookie lookups always fail. YES for the sudo
# group skips auth entirely — no privilege sudo doesn't already grant.
log "polkit: authorize the sudo group (DM-less ⇒ cookies unresolvable)"
# mkdir, not install -d: preserves polkitd's packaged 0750 root:polkitd mode.
mkdir -p /etc/polkit-1/rules.d
cat >/etc/polkit-1/rules.d/49-rmng-sudo-nopasswd.rules <<'RULES'
// No display manager ⇒ only a Class=manager session, to which polkit cannot map an auth
// cookie ("No session for cookie"). Return YES to skip the unfixable lookup. Limited to
// `sudo`, which phase 30 already grants NOPASSWD:ALL.
polkit.addRule(function (action, subject) {
    if (subject.isInGroup("sudo")) {
        return polkit.Result.YES;
    }
});
RULES
chmod 0644 /etc/polkit-1/rules.d/49-rmng-sudo-nopasswd.rules

# Apt lists STAY (cleaned once in the Dockerfile tail) — phase 20 survives a flaky update.
