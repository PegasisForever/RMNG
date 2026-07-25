#!/usr/bin/env bash
# Phase 10 — base desktop. Locale/timezone, headless GNOME + Mutter + VA-API + PipeWire
# (NO gdm3, NO gnome-remote-desktop, NO flatpak), the Recommends strip
# (gdm3/g-r-d/NetworkManager/ModemManager purge + iproute2 pin), and the container masks.
# This is the biggest and rarest-changing layer, so it runs first: a tweak to user setup
# (phase 30) never re-runs this ~20-minute apt work.
#
# Codifies the recipe validated on the `rmng-build` box: vanilla headless GNOME brought up
# by a `systemd --user` unit under linger (Mutter's headless backend needs only
# /dev/dri/renderD128), Mesa VA-API decode, no DM. Replaces the old g-r-d/GDM handover.
set -euo pipefail
. /setup/lib.sh

log "apt update + full-upgrade"
apt-get update -qq && apt-get full-upgrade -y -qq

# The ubuntu:26.04 Docker rootfs ships without snapd (the old LXC template had it); only
# the pin is needed, so nothing in the toolbox drags it back in.
log "pin snapd out"
printf 'Package: snapd\nPin: release *\nPin-Priority: -1\n' > /etc/apt/preferences.d/nosnap.pref

log "locale + timezone + ping range"
apt-get install -y -qq locales
locale-gen en_US.UTF-8
update-locale LANG=en_US.UTF-8
# Timezone: symlink + /etc/timezone (timedatectl can't set the clock inside a container).
ln -sf /usr/share/zoneinfo/America/Toronto /etc/localtime; echo America/Toronto > /etc/timezone
mkdir -p /etc/sysctl.d && echo 'net.ipv4.ping_group_range = 0 65534' > /etc/sysctl.d/99-ping.conf

log "headless GNOME + Mutter + VA-API + PipeWire (NO gdm/g-r-d)"
# The desktop-MCP `screenshot` tool encodes a captured dmabuf via
# `appsrc ! vapostproc ! videoconvert ! pngenc`: vapostproc is the `va` plugin in
# gstreamer1.0-plugins-bad, pngenc is in -good, videoconvert/app in -base. Without these
# the tool fails with `no element "vapostproc"` and the agent can't see the screen.
# `sudo` is explicit: the ubuntu:26.04 DOCKER image ships without it (the old LXC template
# had it), and the rmng sudoers drop-in (phase 30) needs /etc/sudoers.d to exist.
# `openssh-server` here (not a new phase): this is the load-bearing base-system apt install,
# the same rarest-changing layer sshd belongs to; its hardening + host-key strip live in the
# Dockerfile tail (host keys must not be baked — the control-server injects one per clone).
apt-get install -y -qq \
  sudo \
  gnome-session gnome-shell mutter ptyxis nautilus gnome-text-editor loupe \
  dbus-user-session xwayland \
  mesa-va-drivers libva2 va-driver-all vainfo \
  pipewire wireplumber gstreamer1.0-pipewire \
  gstreamer1.0-plugins-base gstreamer1.0-plugins-good gstreamer1.0-plugins-bad \
  fonts-cantarell adwaita-icon-theme jq \
  openssh-server

# Default terminal → Ptyxis (installed above in place of gnome-console; gnome-shell doesn't
# Recommend Console, so dropping it from the list is enough — nothing pulls it back).
update-alternatives --set x-terminal-emulator /usr/bin/ptyxis 2>/dev/null || true

# /var/lib/dbus/machine-id must be a SYMLINK to /etc/machine-id (the Debian norm), not the
# regular file dbus's postinst bakes here. With a baked file, systemd "initializes the
# machine ID from the D-Bus machine ID" on every clone → the whole fleet shares one id
# (found live in the E2E). Symlinked, the per-clone empty /etc/machine-id regenerates fresh
# on first boot and dbus follows.
ln -sf /etc/machine-id /var/lib/dbus/machine-id

# Mask ModemManager. The desktop's Recommends chain drags it in (via NetworkManager); both
# get purged in the strip step below, but the mask stays as a backstop for any future
# package that reintroduces it: its unit has ConditionVirtualization=!container so it never
# starts in a container, yet its D-Bus activation file still asks systemd for it — the bus
# name never appears and any client (gnome-control-center/Settings) blocks ~25s per call.
log "mask ModemManager (D-Bus activation otherwise hangs Settings)"
systemctl mask ModemManager.service >/dev/null 2>&1 || true

# Mask RealtimeKit (rtkit-daemon). Same failure class as ModemManager above: in a container it
# can't create its RT-priority threads (RLIMIT_RTPRIO=0) and comes up wedged during the boot
# storm — the daemon keeps running but stops answering D-Bus. xdg-desktop-portal reads a
# RealtimeKit property at startup; against the wedged daemon that call blocks ~25s, so the
# portal blows its 90s start timeout and lands FAILED — after which every GTK4 app eats ~25s of
# portal timeouts at launch (file manager >1 min, found live). Headless clones don't need RT
# audio scheduling, so mask it: the property read fast-fails and the portal starts clean.
log "mask rtkit-daemon (wedges xdg-desktop-portal → slow GTK app launches; RT unused headless)"
systemctl mask rtkit-daemon.service >/dev/null 2>&1 || true

# Mask the udev units. In a privileged container systemd-udevd sees the HOST's uevents (it
# should never manage the host's devices from inside a guest).
log "mask systemd-udevd + udev-trigger (host uevents)"
systemctl mask systemd-udevd.service systemd-udev-trigger.service >/dev/null 2>&1 || true

# Mask tpm-udev too (arrives via systemd's own libtss2 dep chain on resolute). Its .path
# unit watches PathChanged=/dev — which churns so hard during container boot that the unit
# trips systemd's start rate limit and lands FAILED (start-limit-hit), leaving the whole
# system permanently "degraded" even though every triggered run exits 0 (found in the
# template boot smoke). The unit only fixes /dev/tpm* permissions and no rmng container
# ever gets a TPM device, so mask both halves.
log "mask tpm-udev (boot /dev churn trips its start limit; no TPM in a container)"
systemctl mask tpm-udev.path tpm-udev.service >/dev/null 2>&1 || true

# Ubuntu 26.04's basic.target wants tmp.mount, whose packaged unit mounts /tmp as tmpfs
# (size=50%). Clones already have a disk-backed overlay rootfs and agents often use /tmp for
# large build/download scratch space, so keep /tmp on regular container disk. /dev/shm stays
# tmpfs and is sized separately by DockerCtl for Chromium/Electron.
log "mask tmp.mount (/tmp should use regular container disk, not tmpfs)"
systemctl mask tmp.mount >/dev/null 2>&1 || true

# The header's "NO gdm3 / NO gnome-remote-desktop" isn't free: gnome-shell *Recommends*
# gdm3 and the desktop pulls gnome-remote-desktop, so the recommends-on install above drags
# both back in. Same story for NetworkManager (+ its ModemManager recommend): nothing here
# lists it, but the desktop's Recommends chain (gnome-control-center → nm-connection-editor
# → network-manager) reinstalls it — and Docker owns eth0/resolv.conf/hosts, so an NM that
# ever decided to manage eth0 would DHCP the container's IP into oblivion. Purge all four;
# GNOME Settings degrades gracefully to a "NetworkManager not running" panel (verified
# live). Keep iproute2 — it arrived as a dependency of that chain and autoremove would sweep
# `ip` out of the toolbox with it. autoremove then sweeps the rest of the orphaned deps.
# (The explicitly-installed VA/PipeWire packages above are apt-marked manual, so autoremove
# leaves them — Mesa VA-API decode stays intact.)
log "strip gdm3 + g-r-d + NetworkManager/ModemManager (Recommends pull-ins); go DM-less"
apt-get purge -y -qq gdm3 gnome-remote-desktop network-manager modemmanager >/dev/null 2>&1 || true
apt-mark manual iproute2 >/dev/null 2>&1 || true
apt-get autoremove --purge -y -qq >/dev/null 2>&1 || true
# No display manager → default to multi-user.target. The headless GNOME user unit starts via
# linger, independent of graphical.target / any DM.
systemctl set-default multi-user.target >/dev/null 2>&1 || true

# Going DM-less costs polkit its notion of a login session, so grant the sudo group outright.
# With no GDM there is no seat/TTY session: linger opens the clone user's only logind session
# as Class=manager (a bare `systemd --user`), and polkit cannot resolve an auth cookie to a
# session of that class. Every auth_admin action therefore dies at the agent handshake —
# `GDBus.Error:org.freedesktop.PolicyKit1.Error.Failed: No session for cookie` — no matter
# which agent answers, so installing a graphical agent does not fix it. Found via Steam's
# first-run `pkexec steamdeps --update-apt`, which is unrunnable without this; the same wall
# hits any pkexec caller and GNOME Settings' privileged panels.
#
# YES (not AUTH_ADMIN_KEEP) is the point: it authorizes without an authentication step, so
# there is no agent to find and no cookie to resolve. Scoping to the sudo group keeps it to
# the same accounts phase 30 already gives NOPASSWD:ALL in /etc/sudoers.d, so this grants no
# privilege that sudo does not — and a clone is a single-user disposable sandbox anyway.
log "polkit: authorize the sudo group (DM-less ⇒ Class=manager session, cookies unresolvable)"
install -d -m 0755 /etc/polkit-1/rules.d
cat > /etc/polkit-1/rules.d/49-rmng-sudo-nopasswd.rules <<'RULES'
// A clone has no display manager, so its only logind session is Class=manager (linger's
// bare `systemd --user`) with no seat or TTY. polkit cannot map an auth cookie back to such
// a session, so any auth_admin* action fails with "No session for cookie" whatever agent is
// running. Return YES to skip authentication entirely rather than fix an unfixable lookup.
// Limited to `sudo`, which phase 30 already grants NOPASSWD:ALL via /etc/sudoers.d.
polkit.addRule(function (action, subject) {
    if (subject.isInGroup("sudo")) {
        return polkit.Result.YES;
    }
});
RULES
chmod 0644 /etc/polkit-1/rules.d/49-rmng-sudo-nopasswd.rules

# Apt lists deliberately STAY (dropped once, in the Dockerfile's tail cleanup — matching the
# source flow): phase 20's Ubuntu-archive installs keep working off these lists even when its
# own post-repo `apt-get update` hits a transient network failure.
