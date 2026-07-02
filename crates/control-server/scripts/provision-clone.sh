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
USERNAME="${1:-rmng}"; PASSWORD="${2:-rmng}"
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

say "locale + timezone + ping range"
apt-get install -y -qq locales >/dev/null 2>&1 || true
locale-gen en_US.UTF-8 >/dev/null 2>&1 || true; update-locale LANG=en_US.UTF-8
# Timezone: symlink + /etc/timezone (timedatectl can't set the clock in an unprivileged LXC).
ln -sf /usr/share/zoneinfo/America/Toronto /etc/localtime; echo America/Toronto > /etc/timezone
mkdir -p /etc/sysctl.d && echo 'net.ipv4.ping_group_range = 0 65534' > /etc/sysctl.d/99-ping.conf

say "headless GNOME + Mutter + VA-API + PipeWire (NO gdm/g-r-d)"
# The desktop-MCP `screenshot` tool encodes a captured dmabuf via
# `appsrc ! vapostproc ! videoconvert ! pngenc`: vapostproc is the `va` plugin in
# gstreamer1.0-plugins-bad, pngenc is in -good, videoconvert/app in -base. Without
# these the tool fails with `no element "vapostproc"` and the agent can't see the screen.
apt-get install -y -qq \
  gnome-session gnome-shell mutter ptyxis nautilus gnome-text-editor loupe \
  dbus-user-session xwayland \
  mesa-va-drivers libva2 va-driver-all vainfo \
  pipewire wireplumber gstreamer1.0-pipewire \
  gstreamer1.0-plugins-base gstreamer1.0-plugins-good gstreamer1.0-plugins-bad \
  fonts-cantarell adwaita-icon-theme network-manager jq >/dev/null

# Default terminal → Ptyxis (installed above in place of gnome-console; gnome-shell doesn't
# Recommend Console, so dropping it from the list is enough — nothing pulls it back).
update-alternatives --set x-terminal-emulator /usr/bin/ptyxis 2>/dev/null || true

# Mask ModemManager. It's pulled in by network-manager but its unit has
# ConditionVirtualization=!container, so it never starts in an LXC — yet its D-Bus
# activation file still asks systemd to start it, so the bus name never appears and
# any client (gnome-control-center / Settings) blocks ~25s per call (a ~1-min freeze).
# Masking makes the activation fail instantly instead of timing out.
say "mask ModemManager (won't run in a container; its D-Bus activation otherwise hangs Settings)"
systemctl mask ModemManager.service >/dev/null 2>&1 || true

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

# The header's "NO gdm3 / NO gnome-remote-desktop" isn't free: gnome-shell *Recommends*
# gdm3 and the desktop pulls gnome-remote-desktop, so the recommends-on install above
# drags both back in. We run gnome-shell headless under linger (below) with no display
# manager and bypass g-r-d entirely — so strip them. gdm otherwise sits idle as the
# registered display-manager.service (graphical.target pulls it in); g-r-d only adds an
# unused VA-API encoder + a FreeRDP/TSS2 stack. autoremove then sweeps their orphaned
# deps. (The explicitly-installed VA/PipeWire packages above are apt-marked manual, so
# autoremove leaves them — Mesa VA-API decode stays intact.)
say "strip unused gdm3 + gnome-remote-desktop (pulled in as Recommends); go DM-less"
apt-get purge -y -qq gdm3 gnome-remote-desktop >/dev/null 2>&1 || true
apt-get autoremove --purge -y -qq >/dev/null 2>&1 || true
# No display manager → default to multi-user.target. The headless GNOME user unit starts
# via linger, independent of graphical.target / any DM.
systemctl set-default multi-user.target >/dev/null 2>&1 || true

# ─────────────────────────────────────────────────────────────────────────────
# Dev toolbox — the CT-104 dev-template app set, ALL via apt (no flatpak/snap):
# dev/CLI tools, Docker, cloud CLIs, build libs, fonts, themes, browsers, Cursor,
# Celluloid/ffmpeg, Extension Manager, ONLYOFFICE, plus HMCL from its GitHub release.
# Best-effort: a transient network/apt failure WARNs rather than aborting the build.
# ─────────────────────────────────────────────────────────────────────────────
apti(){ apt-get install -y -qq "$@" || say "WARN: install failed: $*"; }
say "dev toolbox: third-party apt repos (docker/chrome/gh/cursor/mozilla/azure/gcloud/stripe)"
. /etc/os-release; CODENAME="${VERSION_CODENAME:-resolute}"
apt-get install -y -qq ca-certificates curl gnupg >/dev/null 2>&1 || say "WARN: repo prereqs"
install -d -m0755 /etc/apt/keyrings

curl -fsSL https://download.docker.com/linux/ubuntu/gpg | gpg --dearmor -o /etc/apt/keyrings/docker.gpg 2>/dev/null && chmod a+r /etc/apt/keyrings/docker.gpg || say "WARN: docker key"
echo "deb [arch=amd64 signed-by=/etc/apt/keyrings/docker.gpg] https://download.docker.com/linux/ubuntu $CODENAME stable" > /etc/apt/sources.list.d/docker.list

curl -fsSL https://dl.google.com/linux/linux_signing_key.pub | gpg --dearmor -o /etc/apt/keyrings/google-chrome.gpg 2>/dev/null && chmod a+r /etc/apt/keyrings/google-chrome.gpg || say "WARN: chrome key"
echo "deb [arch=amd64 signed-by=/etc/apt/keyrings/google-chrome.gpg] http://dl.google.com/linux/chrome/deb/ stable main" > /etc/apt/sources.list.d/google-chrome.list

curl -fsSL https://cli.github.com/packages/githubcli-archive-keyring.gpg -o /etc/apt/keyrings/githubcli-archive-keyring.gpg 2>/dev/null && chmod a+r /etc/apt/keyrings/githubcli-archive-keyring.gpg || say "WARN: gh key"
echo "deb [arch=amd64 signed-by=/etc/apt/keyrings/githubcli-archive-keyring.gpg] https://cli.github.com/packages stable main" > /etc/apt/sources.list.d/github-cli.list

# Cursor: its repo key is gated behind the download portal, so pin Anysphere's public
# repo-signing key inline (fpr 380F F4BC DC34 A4BD 92A3 5653 42A1 772E 62E4 92D6). If
# Anysphere ever rotates it, `apt update` will flag the cursor repo — refresh here.
base64 -d > /usr/share/keyrings/anysphere.gpg <<'ANYSPHERE_KEY'
mQINBGhv/tgBEAC24VCTfKi5NSVaUAuSaIERf2EC5PCyOQz7WOh/UwyuG/1RB2r8/SYtipV+fD2b+xdu7WGPqrSHrKNNO1A9j6TtqbLVDDweJU2keHOqfIaamxrcyfCw3LMF9elIsmdkbZBukezWM32YBrG5MOwfCmG782sN79jYIPYckGZehh8Q6uIlZAzMTR7Qr6mlRR9cRZOF1gY1hRVCXQc1P3SH+ncX1abo/w3idRjxW3l0tqzjLcovWXD1xQdgt5odrpHlUkXRxxr7ukkPu2yJ2tL0KJydLtRDFf7k6ipYoCQv6hrFziHBHqfAEAMymr4YH96GhlbeP/zTSeUn8Y9Blz18q8sJJ2AKoAwpxWTYDIk7D3GDxHQYkcWIuh3MNJd3nulrptCOXgLBPqAF9/N1PW6UyX2XZmcFf0MQYC++IO0FwgcRw968L4LvgIGJdSCA5umcadDPoCQNcdobTur0WtzrsZ8letGoZ18FAhfeWfMWfljHDPbG0LSImKthaiAwXggi76sQyo374azY/ZfjepxRG3U7iEcesopqeo9p8l/8R7aZEk3zUbVt45yhp7XN8YtDrFvAPZfcIuoQTkeDZEub9Cch+fbeqdNk+LAyUbVzX/cFWBvRWC4ajsI/rD4IgeZInV39uG5ngpiwdb755xmZFiZSD1riGUYFYMfFfI1d80EOtQARAQABtCVBbnlzcGhlcmUgSW5jIDxzZWN1cml0eUBhbnlzcGhlcmUuY28+iQJRBBMBCAA7FiEEOA/0vNw0pL2So1ZTQqF3LmLkktYFAmhv/tgCGwMFCwkIBwICIgIGFQoJCAsCBBYCAwECHgcCF4AACgkQQqF3LmLkktZXUw//fAEm1Vo8uQ1E/4lNToEPM24olQp6If49+HSwFLCB5HhsGFmed6Zx1L+iNDJ8eW8niuepIqSRTX8G/+0z487hP29moLTE85g/YNsgWfkptbps3vgxlStotfgXZIKI71/m7FItBiA/tMS2ZkL1UwCSUQWE1YJgYJ8Gm4IbvoqYNwHv+8i0wJi3/G6lphHMxQp6XuO4HVlIk0dteQPaeszFK7jf74udRVTpxu+ffM0x/NFw08qYPsmBQJ9Of4/dhRfAYI9ZQOAFnIhujykOs7QBnq49JlzF3pYG/ZnvXwpUzRQgga+ro+5bXoQ1DZrNH+zl4EXtiXKpowUoYOZpDSELRVPGUW4vsyi+n34M5jMnxglYQJGB/ZTW95al7c84WZANripx2szeIxKukDcln7y0Qd7jpfGIC7xAjTzwVK8JzsDPisP9KPfua/zifr972QMK/4xlwjRRS6yRyM7Z2QZVdtzpUdPsVgbnXJkb6IBQSJDXKN7LQeB5Wi+4Cg9hddAG6sPu5wIcig67qFN/GEaeu6P4SuQqgBhmtf0x26Y0MDBbJtQ4adHSr90F8Fn8si6/Hb5xjSSOTg9QsPbAbBpmXjblLLRmdhUt0JCbIrBn2+jPKL+bT7aLkXiyI/k6kNC3AbI+YYwYgIDqSpqNHbdu+t9IOHK033IS4qoybKtiKbY=
ANYSPHERE_KEY
chmod a+r /usr/share/keyrings/anysphere.gpg
cat > /etc/apt/sources.list.d/cursor.sources <<'SRC'
Types: deb
URIs: https://downloads.cursor.com/aptrepo
Suites: stable
Components: main
Architectures: amd64,arm64
Signed-By: /usr/share/keyrings/anysphere.gpg
SRC

# Firefox from Mozilla's apt repo (pinned over the snap-transitional). Skip if the base
# image already ships a packages.mozilla.org source — else apt warns "configured twice".
if ! grep -rqs packages.mozilla.org /etc/apt/sources.list.d/ 2>/dev/null; then
  curl -fsSL https://packages.mozilla.org/apt/repo-signing-key.gpg -o /etc/apt/keyrings/packages.mozilla.org.asc 2>/dev/null && chmod a+r /etc/apt/keyrings/packages.mozilla.org.asc || say "WARN: mozilla key"
  echo "deb [signed-by=/etc/apt/keyrings/packages.mozilla.org.asc] https://packages.mozilla.org/apt mozilla main" > /etc/apt/sources.list.d/mozilla.list
  printf 'Package: *\nPin: origin packages.mozilla.org\nPin-Priority: 1000\n' > /etc/apt/preferences.d/mozilla
fi

curl -fsSL https://packages.microsoft.com/keys/microsoft.asc | gpg --dearmor -o /etc/apt/keyrings/microsoft.gpg 2>/dev/null && chmod a+r /etc/apt/keyrings/microsoft.gpg || say "WARN: msft key"
echo "deb [arch=amd64 signed-by=/etc/apt/keyrings/microsoft.gpg] https://packages.microsoft.com/repos/azure-cli/ noble main" > /etc/apt/sources.list.d/azure-cli.list

curl -fsSL https://packages.cloud.google.com/apt/doc/apt-key.gpg | gpg --dearmor -o /etc/apt/keyrings/cloud.google.gpg 2>/dev/null && chmod a+r /etc/apt/keyrings/cloud.google.gpg || say "WARN: gcloud key"
echo "deb [signed-by=/etc/apt/keyrings/cloud.google.gpg] https://packages.cloud.google.com/apt cloud-sdk main" > /etc/apt/sources.list.d/google-cloud-sdk.list

curl -fsSL https://packages.stripe.dev/api/security/keypair/stripe-cli-gpg/public | gpg --dearmor -o /etc/apt/keyrings/stripe.gpg 2>/dev/null && chmod a+r /etc/apt/keyrings/stripe.gpg || say "WARN: stripe key"
echo "deb [signed-by=/etc/apt/keyrings/stripe.gpg] https://packages.stripe.dev/stripe-cli-debian-local stable main" > /etc/apt/sources.list.d/stripe.list

# ONLYOFFICE Desktop Editors — official repo (replaces the Flathub build). GPG-KEY file is
# ASCII-armored; handle the binary case too just in case. "squeeze" is ONLYOFFICE's fixed
# repo suite, not a Debian release.
if curl -fsSL https://download.onlyoffice.com/GPG-KEY-ONLYOFFICE -o /tmp/oo.key 2>/dev/null; then
  if grep -q "BEGIN PGP" /tmp/oo.key; then gpg --dearmor < /tmp/oo.key > /etc/apt/keyrings/onlyoffice.gpg || say "WARN: onlyoffice key"; else cp /tmp/oo.key /etc/apt/keyrings/onlyoffice.gpg; fi
  chmod a+r /etc/apt/keyrings/onlyoffice.gpg; rm -f /tmp/oo.key
  echo "deb [signed-by=/etc/apt/keyrings/onlyoffice.gpg] https://download.onlyoffice.com/repo/debian squeeze main" > /etc/apt/sources.list.d/onlyoffice.list
else
  say "WARN: onlyoffice key fetch failed"
fi

apt-get update -qq || say "WARN: apt update after adding toolbox repos"

say "dev toolbox: install (grouped so one bad package doesn't sink the rest)"
apti fish ripgrep micro tmux just gh xdotool build-essential clang libclang-dev default-jdk
apti docker-ce docker-ce-cli containerd.io docker-buildx-plugin docker-compose-plugin fuse-overlayfs
apti azure-cli google-cloud-cli stripe
apti libsodium23 libpq-dev libcairo2 libcairo2-dev
apti fonts-noto-cjk fonts-noto-color-emoji papirus-icon-theme
apti celluloid ffmpeg firefox google-chrome-stable cursor
# Former Flathub apps now available via apt: Extension Manager (Ubuntu universe) + ONLYOFFICE.
apti gnome-shell-extension-manager onlyoffice-desktopeditors

say "dev toolbox: HMCL (latest .deb from its GitHub release)"
HMCL_URL="$(curl -fsSL https://api.github.com/repos/HMCL-dev/HMCL/releases/latest 2>/dev/null | grep -oE 'https://[^"]+/HMCL-[0-9.]+\.deb' | head -1)"
if [ -n "${HMCL_URL:-}" ] && curl -fsSL "$HMCL_URL" -o /tmp/hmcl.deb 2>/dev/null; then
  apti /tmp/hmcl.deb && say "HMCL installed ($(basename "$HMCL_URL"))"; rm -f /tmp/hmcl.deb
else
  say "WARN: could not fetch HMCL release"
fi

# Mission Center (system monitor) — no apt/deb upstream, only Flatpak + AppImage. Pull the
# latest x86_64 AppImage, --appimage-extract it (no FUSE needed), install the raw tree under
# /opt, and wire up a PATH wrapper + desktop entry (Exec rewritten to the wrapper). Defined
# as a function so `set -e` is suppressed across the block (run as `mc_install && … || …`),
# keeping this optional app from ever aborting the build.
mc_install(){
  local url d
  url="$(curl -fsSL 'https://gitlab.com/api/v4/projects/mission-center-devs%2Fmission-center/releases' 2>/dev/null | grep -oE 'https://[^"]+x86_64\.AppImage' | head -1)"
  [ -n "$url" ] || return 1
  curl -fsSL "$url" -o /tmp/mc.AppImage || return 1
  chmod +x /tmp/mc.AppImage
  ( cd /tmp && rm -rf squashfs-root && /tmp/mc.AppImage --appimage-extract >/dev/null 2>&1 ) || true
  [ -d /tmp/squashfs-root ] || return 1
  rm -rf /opt/mission-center; mv /tmp/squashfs-root /opt/mission-center; chown -R root:root /opt/mission-center
  printf '#!/bin/sh\nexec /opt/mission-center/AppRun "$@"\n' > /usr/local/bin/mission-center; chmod 755 /usr/local/bin/mission-center
  [ -d /opt/mission-center/usr/share/icons ] && cp -rn /opt/mission-center/usr/share/icons/* /usr/share/icons/ 2>/dev/null || true
  d="$(ls /opt/mission-center/usr/share/applications/*.desktop 2>/dev/null | head -1)"; [ -n "$d" ] || d="$(ls /opt/mission-center/*.desktop 2>/dev/null | head -1)"
  [ -n "$d" ] && sed -E 's#^Exec=.*#Exec=/usr/local/bin/mission-center#; s#^TryExec=.*#TryExec=/usr/local/bin/mission-center#' "$d" > /usr/share/applications/io.missioncenter.MissionCenter.desktop
  update-desktop-database /usr/share/applications >/dev/null 2>&1 || true
  gtk-update-icon-cache -f /usr/share/icons/hicolor >/dev/null 2>&1 || true
  rm -f /tmp/mc.AppImage
}
say "dev toolbox: Mission Center (AppImage → extract → /opt + desktop entry)"
mc_install && say "Mission Center installed" || say "WARN: Mission Center install skipped"

# Monaspace fonts (githubnext/monaspace) — full set: static (family "Monaspace Neon"),
# frozen ("Monaspace Neon Frozen", texture-healing baked in — used as the default mono),
# and variable ("Monaspace Neon Var"). frozen+variable are TTF, static is OTF — copy both.
command -v unzip >/dev/null 2>&1 || apt-get install -y -qq unzip >/dev/null 2>&1
mona_install(){
  local json url v
  json="$(curl -fsSL https://api.github.com/repos/githubnext/monaspace/releases/latest 2>/dev/null)"
  [ -n "$json" ] || return 1
  rm -rf /tmp/mona; mkdir -p /tmp/mona /usr/share/fonts/monaspace
  for v in static frozen variable; do
    url="$(echo "$json" | grep -oE "https://[^\"]+monaspace-$v-[^\"]+\.zip" | head -1)"
    [ -n "$url" ] || continue
    curl -fsSL "$url" -o "/tmp/mona/$v.zip" && unzip -oq "/tmp/mona/$v.zip" -d "/tmp/mona/$v" || true
  done
  find /tmp/mona -type f \( -iname '*.otf' -o -iname '*.ttf' \) -exec cp -f {} /usr/share/fonts/monaspace/ \;
  fc-cache -f >/dev/null 2>&1 || true
  rm -rf /tmp/mona
  [ -n "$(find /usr/share/fonts/monaspace -type f -name '*.ttf' 2>/dev/null | head -1)" ]
}
say "dev toolbox: Monaspace fonts (full: static + frozen + variable)"
mona_install && say "Monaspace installed ($(find /usr/share/fonts/monaspace -type f 2>/dev/null | wc -l) files)" || say "WARN: Monaspace install skipped"

# GNOME desktop defaults, system-wide via dconf (session-independent → every clone gets them
# on first boot): Papirus icons, Monaspace Neon Frozen 11 as the default monospace font, and
# all three window buttons (minimize/maximize/close). Users can still override per-session.
say "GNOME desktop defaults: Papirus icons + Monaspace Neon Frozen mono + 3 window buttons"
install -d /etc/dconf/profile /etc/dconf/db/local.d
printf 'user-db:user\nsystem-db:local\n' > /etc/dconf/profile/user
cat > /etc/dconf/db/local.d/00-rmng-desktop <<'DCONF'
[org/gnome/desktop/interface]
icon-theme='Papirus'
monospace-font-name='Monaspace Neon Frozen 11'

[org/gnome/desktop/wm/preferences]
button-layout='appmenu:minimize,maximize,close'
DCONF
dconf update 2>/dev/null || say "WARN: dconf update failed"

say "create user $USERNAME + groups + linger"
id "$USERNAME" >/dev/null 2>&1 || useradd -m -s /bin/bash "$USERNAME"
usermod -aG sudo,render,video "$USERNAME"
# docker group exists once docker-ce installed in the toolbox above; add the user so they
# can run docker without sudo. Non-fatal if the group is absent (docker install failed).
getent group docker >/dev/null 2>&1 && usermod -aG docker "$USERNAME" || say "WARN: docker group absent; not added"
printf '%s:%s\n' "$USERNAME" "$PASSWORD" | chpasswd
printf 'root:%s\n' "$PASSWORD" | chpasswd
printf '%s ALL=(ALL) NOPASSWD:ALL\n' "$USERNAME" > "/etc/sudoers.d/$USERNAME"; chmod 0440 "/etc/sudoers.d/$USERNAME"
loginctl enable-linger "$USERNAME"

# Default shell → fish for the clone user + root (fish installed in the dev toolbox above;
# it registers itself in /etc/shells so chsh accepts it). Non-fatal if fish is missing.
FISH_SH="$(command -v fish || true)"
if [ -n "$FISH_SH" ]; then
  for u in "$USERNAME" root; do chsh -s "$FISH_SH" "$u" 2>/dev/null || usermod -s "$FISH_SH" "$u" || say "WARN: set fish shell for $u"; done
fi

# ~/.local/bin + ~/.cargo/bin on PATH for interactive shells. User-local tools install
# there — Claude Code / uv → ~/.local/bin, rustup/cargo → ~/.cargo/bin — but neither fish
# (the clones' default shell, set above) nor a non-login bash puts them on PATH, so the
# tools aren't found in a terminal even though the agent-wrapper unit hardcodes ~/.local/bin.
# Cover every fish shell (conf.d), login sh/bash (profile.d), and non-login interactive
# bash (/etc/bash.bashrc). Guards keep it idempotent and skip dirs until they're created.
say "PATH: add ~/.local/bin + ~/.cargo/bin for interactive fish + bash"
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
# Non-login interactive bash sources /etc/bash.bashrc (not profile.d). Delete any prior
# rmng block (marker-delimited) then re-append, so re-provisioning stays idempotent.
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

# Passwordless GNOME keyring. The headless session has no login password to unlock a
# keyring, so the first Secret Service client (Chrome, VS Code, etc.) pops a "Choose
# password for new keyring" dialog. Pre-create an empty-password login keyring — the
# unencrypted, never-locked [keyring] text format — and alias it as the default
# collection, so every Secret Service app works silently. Secrets land in cleartext on
# disk, which is fine for an ephemeral remote-desktop clone. (Verified: a daemon-created
# empty-password keyring is byte-for-byte this text file; dropping it is enough — no
# gnome-keyring --login dance, and it auto-serves on a cold boot via D-Bus activation.)
say "passwordless gnome-keyring (no Secret Service prompt for Chrome/etc.)"
KRDIR="/home/$USERNAME/.local/share/keyrings"
install -d -o "$USERNAME" -g "$USERNAME" -m700 "$KRDIR"
# install -d does NOT chown the intermediate parents it creates, so ~/.local and
# ~/.local/share would be left root-owned — which blocks the user's own writes there
# (notably the claude installer's `mkdir ~/.local/share/claude` → EACCES). Chown them.
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

# Service binaries live in /opt/rmng/bin (root-owned, 755) — NOT the user's home, so they
# don't clutter the Files/Nautilus Home view. The systemd --user units exec them from here.
BINDIR=/opt/rmng/bin; install -d -m0755 "$BINDIR"

say "install clone-daemon"
install -m755 /root/rmng-clone-daemon "$BINDIR/rmng-clone-daemon" 2>/dev/null || \
  say "WARN: /root/rmng-clone-daemon not pushed; install it later"

say "install agent-wrapper (Bun single-exec) + standalone claude CLI (no node)"
install -m755 /root/agent-wrapper "$BINDIR/agent-wrapper" 2>/dev/null || \
  say "WARN: /root/agent-wrapper not pushed; install it later"
# Claude Code installs standalone (self-contained binary, no system node) → ~/.local/bin/claude.
runuser -u "$USERNAME" -- bash -lc 'command -v claude >/dev/null 2>&1 || curl -fsSL https://claude.ai/install.sh | bash' 2>/dev/null || \
  say "WARN: standalone claude CLI install failed; install it later"

# Shared user CLAUDE.md — operating memory read by EVERY `claude` on this clone: the
# agent-wrapper's SDK agent (settingSources: ["user"]), the Claude Code it drives inside
# Cursor to implement tickets, and any interactive `claude` a human opens. General
# engineering guidance ONLY — deliberately NOT the desktop operating notes or the ticket
# procedure (those are baked into the agent-wrapper and injected as its system prompt; the
# ticket procedure must never reach the inner Cursor agent or it would recursively try to
# open Cursor). install -d is idempotent — the claude installer already made ~/.claude.
say "shared user CLAUDE.md (agent operating memory)"
CLAUDE_DIR="/home/$USERNAME/.claude"
install -d -o "$USERNAME" -g "$USERNAME" -m700 "$CLAUDE_DIR"
cat > "$CLAUDE_DIR/CLAUDE.md" <<'CLAUDEMD'
# Working in this clone

This machine is a **disposable, single-purpose dev sandbox** that belongs to you,
with **passwordless `sudo`**. Install packages, toolchains, and global CLIs freely
and reconfigure the system as needed — the machine itself is throwaway and there is
no other user to disturb. Optimize for getting the task done.

## Engineering standards

- **Verify before claiming done.** Build, typecheck, and run the relevant tests
  after a change; don't leave the tree broken. If you can't verify something, say
  so plainly instead of asserting it works.
- **Match the surrounding code.** Follow the existing style, naming, and patterns
  of the file and repo you're editing. Keep the diff minimal and scoped to the
  task — no drive-by reformatting or unrelated refactors.
- **Understand before you change.** Read the relevant code and how it's used first;
  prefer the smallest change that correctly solves the problem.

## Git

- The checked-out branch is real work — write clear, present-tense commit messages
  that explain *why*, not just *what*.
- **Commit and push only when the task calls for it.** Never force-push a shared
  branch or rewrite already-published history.

## When you're blocked

If you're genuinely stuck — missing access or credentials, an ambiguous
requirement, or a call that's the human's to make — **stop and ask** rather than
guessing or thrashing. A precise question beats a confident wrong turn.
CLAUDEMD
chown "$USERNAME:$USERNAME" "$CLAUDE_DIR/CLAUDE.md"
chmod 644 "$CLAUDE_DIR/CLAUDE.md"

# User-scope `linear` MCP for every `claude` on the clone (interactive shell, inner
# Cursor agent; the agent-wrapper registers the same server programmatically).
# mcpServers lives in ~/.claude.json — a top-level key; settings.json does NOT
# support it. ${LINEAR_API_KEY} stays literal here (single-quoted jq arg): claude
# expands it at runtime from the session env, where clone.sh's 30-rmng-preset.conf
# put the chosen preset's key. No key in the env (e.g. on the template itself) ⇒
# claude skips the server with a "missing environment variables" warning.
say "user-scope linear MCP → ~/.claude.json"
CLAUDE_JSON="/home/$USERNAME/.claude.json"
[ -s "$CLAUDE_JSON" ] || echo '{}' > "$CLAUDE_JSON"
jq --arg auth 'Bearer ${LINEAR_API_KEY}' \
  '.mcpServers.linear = {"type":"http","url":"https://mcp.linear.app/mcp","headers":{"Authorization":$auth}}' \
  "$CLAUDE_JSON" > "$CLAUDE_JSON.tmp" && mv "$CLAUDE_JSON.tmp" "$CLAUDE_JSON"
chown "$USERNAME:$USERNAME" "$CLAUDE_JSON"
chmod 600 "$CLAUDE_JSON"

# uv + nvm + fish-nvm, all installed as the clone user (per-user, like claude above).
# Subshell cd's to the user's home so fisher can getcwd (root's cwd isn't readable by
# the user → fisher would otherwise spew "Unable to open the current working directory").
say "user tools: uv (Astral) + nvm + fish-nvm"
( cd "/home/$USERNAME" 2>/dev/null || cd /
  # uv — Astral's Python package/venv manager → ~/.local/bin/uv (already on PATH via the
  # shell files written above). UV_NO_MODIFY_PATH: PATH is ours, don't let it touch profiles.
  runuser -u "$USERNAME" -- bash -lc 'command -v uv >/dev/null 2>&1 || curl -LsSf https://astral.sh/uv/install.sh | env UV_NO_MODIFY_PATH=1 sh' \
    || say "WARN: uv install failed; install it later"
  # rustup + latest stable Rust → ~/.cargo + ~/.rustup. --no-modify-path: we own PATH (the
  # rmng-local-bin files above put ~/.cargo/bin on it). Default profile (rustc/cargo/clippy/
  # rustfmt/std). </dev/null so the installer never blocks on this script's stdin.
  runuser -u "$USERNAME" -- bash -lc 'command -v rustup >/dev/null 2>&1 || curl --proto "=https" --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --no-modify-path --default-toolchain stable --profile default' </dev/null \
    || say "WARN: rustup install failed; install it later"
  # nvm → ~/.nvm. Force PROFILE=~/.bashrc so its loader lands in bash: the user's default
  # shell is fish, which nvm's installer can't detect and would otherwise skip the profile.
  # (|| true keeps the substitution from tripping set -e/pipefail when the tag lookup fails.)
  NVM_TAG="$(curl -fsSL https://api.github.com/repos/nvm-sh/nvm/releases/latest 2>/dev/null | grep -oE 'v[0-9]+\.[0-9]+\.[0-9]+' | head -1 || true)"
  : "${NVM_TAG:=v0.40.1}"
  runuser -u "$USERNAME" -- bash -lc "[ -s \"\$HOME/.nvm/nvm.sh\" ] || { export PROFILE=\"\$HOME/.bashrc\"; curl -o- 'https://raw.githubusercontent.com/nvm-sh/nvm/$NVM_TAG/install.sh' | bash; }" \
    || say "WARN: nvm install failed; install it later"
  # fish-nvm — makes nvm/node/npm/npx/yarn work in fish (the default shell) by lazily
  # sourcing nvm via bass. Bootstrap fisher, then install it + its bass + fish-nvm deps.
  # </dev/null: fisher must not inherit this script's stdin (it would consume later lines).
  runuser -u "$USERNAME" -- fish -c 'curl -sL https://raw.githubusercontent.com/jorgebucaran/fisher/main/functions/fisher.fish | source && fisher install jorgebucaran/fisher edc/bass FabioAntunes/fish-nvm' </dev/null \
    || say "WARN: fish-nvm install failed; install it later"
)

say "systemd --user units: headless gnome-shell + clone-daemon"
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
# Ship to the control-server's media socket — the shared host dir /srv/rmng-sock is
# bind-mounted at the same path (see bootstrap.sh / the deploy CT mp0). Without this
# the daemon falls back to standalone capture self-test (no connection to the server).
Environment=RMNG_SOCKET=/srv/rmng-sock/clones.sock
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
# `gnome-shell --headless` directly — no GDM / gnome-session / pam_systemd — so nothing
# else sets these. systemd --user reads environment.d → the session + all user units.
# Per-clone env presets live in 30-rmng-preset.conf (written by clone.sh); higher number wins.
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
$SC daemon-reexec 2>/dev/null || true  # re-read environment.d into the manager env before starting units
# Start now too — the user manager came up at enable-linger time (before the units
# were linked), so the wants symlinks alone won't start them until the next boot.
$SC start gnome-headless.service 2>/dev/null || true
sleep 2
$SC start rmng-clone-daemon.service agent-wrapper.service 2>/dev/null || true

say "provision complete; render node:"; ls -l /dev/dri || true
echo "RESULT ok"
