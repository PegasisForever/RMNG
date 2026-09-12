#!/usr/bin/env bash
# Phase 20 — dev toolbox, ALL via apt (no flatpak/snap), plus Mission Center / Monaspace /
# adw-gtk3 from upstream releases and GNOME dconf defaults. Any failure fails the build.
set -euo pipefail
. /setup/lib.sh
enable_err_trap

log "dev toolbox: third-party apt repos (docker/chrome/gh/cursor/mozilla/azure/gcloud/stripe)"
. /etc/os-release
CODENAME="${VERSION_CODENAME:-resolute}"
apt-get install -y -qq ca-certificates curl gnupg >/dev/null 2>&1
install -d -m0755 /etc/apt/keyrings

curl -fsSL https://download.docker.com/linux/ubuntu/gpg | gpg --dearmor -o /etc/apt/keyrings/docker.gpg 2>/dev/null && chmod a+r /etc/apt/keyrings/docker.gpg
echo "deb [arch=amd64 signed-by=/etc/apt/keyrings/docker.gpg] https://download.docker.com/linux/ubuntu $CODENAME stable" >/etc/apt/sources.list.d/docker.list

curl -fsSL https://dl.google.com/linux/linux_signing_key.pub | gpg --dearmor -o /etc/apt/keyrings/google-chrome.gpg 2>/dev/null && chmod a+r /etc/apt/keyrings/google-chrome.gpg
echo "deb [arch=amd64 signed-by=/etc/apt/keyrings/google-chrome.gpg] http://dl.google.com/linux/chrome/deb/ stable main" >/etc/apt/sources.list.d/google-chrome.list

curl -fsSL https://cli.github.com/packages/githubcli-archive-keyring.gpg -o /etc/apt/keyrings/githubcli-archive-keyring.gpg 2>/dev/null && chmod a+r /etc/apt/keyrings/githubcli-archive-keyring.gpg
echo "deb [arch=amd64 signed-by=/etc/apt/keyrings/githubcli-archive-keyring.gpg] https://cli.github.com/packages stable main" >/etc/apt/sources.list.d/github-cli.list

# Cursor's repo key is portal-gated — pinned inline; refresh here if apt flags rotation.
base64 -d >/usr/share/keyrings/anysphere.gpg <<'ANYSPHERE_KEY'
mQINBGhv/tgBEAC24VCTfKi5NSVaUAuSaIERf2EC5PCyOQz7WOh/UwyuG/1RB2r8/SYtipV+fD2b+xdu7WGPqrSHrKNNO1A9j6TtqbLVDDweJU2keHOqfIaamxrcyfCw3LMF9elIsmdkbZBukezWM32YBrG5MOwfCmG782sN79jYIPYckGZehh8Q6uIlZAzMTR7Qr6mlRR9cRZOF1gY1hRVCXQc1P3SH+ncX1abo/w3idRjxW3l0tqzjLcovWXD1xQdgt5odrpHlUkXRxxr7ukkPu2yJ2tL0KJydLtRDFf7k6ipYoCQv6hrFziHBHqfAEAMymr4YH96GhlbeP/zTSeUn8Y9Blz18q8sJJ2AKoAwpxWTYDIk7D3GDxHQYkcWIuh3MNJd3nulrptCOXgLBPqAF9/N1PW6UyX2XZmcFf0MQYC++IO0FwgcRw968L4LvgIGJdSCA5umcadDPoCQNcdobTur0WtzrsZ8letGoZ18FAhfeWfMWfljHDPbG0LSImKthaiAwXggi76sQyo374azY/ZfjepxRG3U7iEcesopqeo9p8l/8R7aZEk3zUbVt45yhp7XN8YtDrFvAPZfcIuoQTkeDZEub9Cch+fbeqdNk+LAyUbVzX/cFWBvRWC4ajsI/rD4IgeZInV39uG5ngpiwdb755xmZFiZSD1riGUYFYMfFfI1d80EOtQARAQABtCVBbnlzcGhlcmUgSW5jIDxzZWN1cml0eUBhbnlzcGhlcmUuY28+iQJRBBMBCAA7FiEEOA/0vNw0pL2So1ZTQqF3LmLkktYFAmhv/tgCGwMFCwkIBwICIgIGFQoJCAsCBBYCAwECHgcCF4AACgkQQqF3LmLkktZXUw//fAEm1Vo8uQ1E/4lNToEPM24olQp6If49+HSwFLCB5HhsGFmed6Zx1L+iNDJ8eW8niuepIqSRTX8G/+0z487hP29moLTE85g/YNsgWfkptbps3vgxlStotfgXZIKI71/m7FItBiA/tMS2ZkL1UwCSUQWE1YJgYJ8Gm4IbvoqYNwHv+8i0wJi3/G6lphHMxQp6XuO4HVlIk0dteQPaeszFK7jf74udRVTpxu+ffM0x/NFw08qYPsmBQJ9Of4/dhRfAYI9ZQOAFnIhujykOs7QBnq49JlzF3pYG/ZnvXwpUzRQgga+ro+5bXoQ1DZrNH+zl4EXtiXKpowUoYOZpDSELRVPGUW4vsyi+n34M5jMnxglYQJGB/ZTW95al7c84WZANripx2szeIxKukDcln7y0Qd7jpfGIC7xAjTzwVK8JzsDPisP9KPfua/zifr972QMK/4xlwjRRS6yRyM7Z2QZVdtzpUdPsVgbnXJkb6IBQSJDXKN7LQeB5Wi+4Cg9hddAG6sPu5wIcig67qFN/GEaeu6P4SuQqgBhmtf0x26Y0MDBbJtQ4adHSr90F8Fn8si6/Hb5xjSSOTg9QsPbAbBpmXjblLLRmdhUt0JCbIrBn2+jPKL+bT7aLkXiyI/k6kNC3AbI+YYwYgIDqSpqNHbdu+t9IOHK033IS4qoybKtiKbY=
ANYSPHERE_KEY
chmod a+r /usr/share/keyrings/anysphere.gpg
cat >/etc/apt/sources.list.d/cursor.sources <<'SRC'
Types: deb
URIs: https://downloads.cursor.com/aptrepo
Suites: stable
Components: main
Architectures: amd64,arm64
Signed-By: /usr/share/keyrings/anysphere.gpg
SRC

if ! grep -rqs packages.mozilla.org /etc/apt/sources.list.d/ 2>/dev/null; then
  curl -fsSL https://packages.mozilla.org/apt/repo-signing-key.gpg -o /etc/apt/keyrings/packages.mozilla.org.asc 2>/dev/null && chmod a+r /etc/apt/keyrings/packages.mozilla.org.asc
  echo "deb [signed-by=/etc/apt/keyrings/packages.mozilla.org.asc] https://packages.mozilla.org/apt mozilla main" >/etc/apt/sources.list.d/mozilla.list
  printf 'Package: *\nPin: origin packages.mozilla.org\nPin-Priority: 1000\n' >/etc/apt/preferences.d/mozilla
fi

curl -fsSL https://packages.microsoft.com/keys/microsoft.asc | gpg --dearmor -o /etc/apt/keyrings/microsoft.gpg 2>/dev/null && chmod a+r /etc/apt/keyrings/microsoft.gpg
echo "deb [arch=amd64 signed-by=/etc/apt/keyrings/microsoft.gpg] https://packages.microsoft.com/repos/azure-cli/ noble main" >/etc/apt/sources.list.d/azure-cli.list
# VS Code shares the microsoft.gpg keyring imported just above.
echo "deb [arch=amd64 signed-by=/etc/apt/keyrings/microsoft.gpg] https://packages.microsoft.com/repos/code stable main" >/etc/apt/sources.list.d/vscode.list

curl -fsSL https://packages.cloud.google.com/apt/doc/apt-key.gpg | gpg --dearmor -o /etc/apt/keyrings/cloud.google.gpg 2>/dev/null && chmod a+r /etc/apt/keyrings/cloud.google.gpg
echo "deb [signed-by=/etc/apt/keyrings/cloud.google.gpg] https://packages.cloud.google.com/apt cloud-sdk main" >/etc/apt/sources.list.d/google-cloud-sdk.list

curl -fsSL https://packages.stripe.dev/api/security/keypair/stripe-cli-gpg/public | gpg --dearmor -o /etc/apt/keyrings/stripe.gpg 2>/dev/null && chmod a+r /etc/apt/keyrings/stripe.gpg
echo "deb [signed-by=/etc/apt/keyrings/stripe.gpg] https://packages.stripe.dev/stripe-cli-debian-local stable main" >/etc/apt/sources.list.d/stripe.list

curl -fsSL https://ngrok-agent.s3.amazonaws.com/ngrok.asc | gpg --dearmor -o /etc/apt/keyrings/ngrok.gpg 2>/dev/null && chmod a+r /etc/apt/keyrings/ngrok.gpg
echo "deb [signed-by=/etc/apt/keyrings/ngrok.gpg] https://ngrok-agent.s3.amazonaws.com buster main" >/etc/apt/sources.list.d/ngrok.list

# "squeeze" is ONLYOFFICE's fixed repo suite, not a Debian release.
if curl -fsSL https://download.onlyoffice.com/GPG-KEY-ONLYOFFICE -o /tmp/oo.key 2>/dev/null; then
  if grep -q "BEGIN PGP" /tmp/oo.key; then gpg --dearmor </tmp/oo.key >/etc/apt/keyrings/onlyoffice.gpg; else cp /tmp/oo.key /etc/apt/keyrings/onlyoffice.gpg; fi
  chmod a+r /etc/apt/keyrings/onlyoffice.gpg
  rm -f /tmp/oo.key
  echo "deb [signed-by=/etc/apt/keyrings/onlyoffice.gpg] https://download.onlyoffice.com/repo/debian squeeze main" >/etc/apt/sources.list.d/onlyoffice.list
fi

apt-get update -qq

log "dev toolbox: install (grouped for log readability — every group fails the build)"
apt-get install -y -qq fish ripgrep micro tmux just gh xdotool rsync rclone build-essential clang libclang-dev default-jdk
apt-get install -y -qq docker-ce docker-ce-cli containerd.io docker-buildx-plugin docker-compose-plugin fuse-overlayfs
apt-get install -y -qq azure-cli google-cloud-cli stripe ngrok
apt-get install -y -qq libsodium23 libpq-dev libcairo2 libcairo2-dev
apt-get install -y -qq fonts-noto-cjk fonts-noto-color-emoji papirus-icon-theme
apt-get install -y -qq celluloid ffmpeg firefox google-chrome-stable cursor code
apt-get install -y -qq gnome-shell-extension-manager onlyoffice-desktopeditors
apt-get install -y -qq desktop-file-utils

# Zed — always the latest release (resolved via the GitHub API, never pinned). No
# sha256 to check against (the API reports none); the structure assertions + ldd gate
# below are the safety net — an upstream layout change still fails the build here.
ZED_VERSION="$(curl -fsSL https://api.github.com/repos/zed-industries/zed/releases/latest 2>/dev/null | grep -oE '"tag_name": "v[0-9.]+"' | head -1 | grep -oE 'v[0-9.]+' || true)"
[ -n "$ZED_VERSION" ] || {
  echo "  !! couldn't resolve latest zed version" >&2
  exit 1
}
ZED_URL="https://github.com/zed-industries/zed/releases/download/${ZED_VERSION}/zed-linux-x86_64.tar.gz"
log "dev toolbox: Zed editor ${ZED_VERSION} (latest)"
curl -fsSL "$ZED_URL" -o /tmp/zed.tar.gz
rm -rf /opt/zed.app
tar -xzf /tmp/zed.tar.gz -C /opt
rm -f /tmp/zed.tar.gz
# Structure assertions: an upstream layout change fails the build here, not in a clone.
[ -x /opt/zed.app/bin/zed ]
[ -f /opt/zed.app/libexec/zed-editor ]
ZED_DESKTOP="$(ls /opt/zed.app/share/applications/dev.zed.Zed.desktop /opt/zed.app/share/applications/zed.desktop 2>/dev/null | head -1 || true)"
[ -n "$ZED_DESKTOP" ]
sed -e "s|Icon=zed|Icon=/opt/zed.app/share/icons/hicolor/512x512/apps/zed.png|g" -e "s|Exec=zed|Exec=/opt/zed.app/bin/zed|g" "$ZED_DESKTOP" >/usr/share/applications/dev.zed.Zed.desktop
grep -q '^Exec=/opt/zed.app/bin/zed' /usr/share/applications/dev.zed.Zed.desktop
grep -q '^Icon=/opt/zed.app/share/icons/hicolor/512x512/apps/zed.png' /usr/share/applications/dev.zed.Zed.desktop
printf '#!/bin/sh\nexec /opt/zed.app/bin/zed "$@"\n' >/usr/local/bin/zed
chmod 755 /usr/local/bin/zed
update-desktop-database /usr/share/applications >/dev/null 2>&1
# ldd gate: the loader still needs system X11/Wayland/audio libs — fail here, not at launch.
ZED_MISSING="$(ldd /opt/zed.app/libexec/zed-editor 2>/dev/null | sed -n 's/^[[:space:]]*\(.*\) => not found$/\1/p' || true)"
if [ -n "$ZED_MISSING" ]; then
  echo "  !! Zed missing system libs:$ZED_MISSING" >&2
  exit 1
fi
log "Zed installed: /opt/zed.app/bin/zed + dev.zed.Zed.desktop"

# Mission Center — pinned AppImage (no FUSE). Re-pin version+URL+sha256 together; the
# asserts below are load-bearing (upstream restructured layouts before).
MC_VERSION=1.2.0
MC_URL="https://gitlab.com/mission-center-devs/mission-center/-/jobs/15536631699/artifacts/raw/MissionCenter-${MC_VERSION}-x86_64.AppImage"
MC_SHA256=b3b5c84470a927d189c251039d223464125f7068b3164fda43acb9100384576d
log "dev toolbox: Mission Center ${MC_VERSION} (pinned + sha256-verified)"
curl -fsSL "$MC_URL" -o /tmp/mc.AppImage
echo "${MC_SHA256}  /tmp/mc.AppImage" | sha256sum -c -
chmod +x /tmp/mc.AppImage
# Unique extract dir per run: a fixed squashfs-root collides with stale prior attempts.
mc_work="/tmp/mc-extract-$$"
rm -rf "$mc_work"
mkdir -p "$mc_work"
(cd "$mc_work" && rm -rf squashfs-root AppDir && /tmp/mc.AppImage --appimage-extract >/dev/null 2>&1)
# Resolve a symlinked squashfs-root first, or the move installs a dangling link.
mc_src="$mc_work/squashfs-root"
if [ -L "$mc_src" ]; then mc_src="$(readlink -f "$mc_src")"; fi
if [ ! -d "$mc_src" ] || [ -L "$mc_src" ] || [ ! -x "$mc_src/AppRun" ]; then
  echo "  !! extract produced no real AppRun dir; state dump:" >&2
  ls -la "$mc_work"/ /tmp/ >&2
  exit 1
fi
rm -rf /opt/mission-center
mv "$mc_src" /opt/mission-center
chown -R root:root /opt/mission-center
if [ ! -d /opt/mission-center ] || [ -L /opt/mission-center ] || [ ! -x /opt/mission-center/AppRun ]; then
  echo "  !! /opt/mission-center not a real dir with AppRun; state dump:" >&2
  ls -lad /opt/ /opt/mission-center >&2
  exit 1
fi
rm -rf "$mc_work" /tmp/mc.AppImage
log "Mission Center installed"

# Monaspace full set: static (OTF) + frozen/variable (TTF).
command -v unzip >/dev/null 2>&1 || apt-get install -y -qq unzip >/dev/null 2>&1
mona_install() {
  local json url v
  json="$(curl -fsSL https://api.github.com/repos/githubnext/monaspace/releases/latest 2>/dev/null)"
  [ -n "$json" ] || return 1
  rm -rf /tmp/mona
  mkdir -p /tmp/mona /usr/share/fonts/monaspace
  for v in static frozen variable; do
    url="$(echo "$json" | grep -oE "https://[^\"]+monaspace-$v-[^\"]+\.zip" | head -1 || true)"
    [ -n "$url" ] || return 1
    curl -fsSL "$url" -o "/tmp/mona/$v.zip"
    unzip -oq "/tmp/mona/$v.zip" -d "/tmp/mona/$v"
  done
  find /tmp/mona -type f \( -iname '*.otf' -o -iname '*.ttf' \) -exec cp -f {} /usr/share/fonts/monaspace/ \;
  fc-cache -f >/dev/null 2>&1
  rm -rf /tmp/mona
  [ -n "$(find /usr/share/fonts/monaspace -type f -name '*.ttf' 2>/dev/null | head -1 || true)" ]
}
log "dev toolbox: Monaspace fonts (full: static + frozen + variable)"
mona_install
log "Monaspace installed ($(find /usr/share/fonts/monaspace -type f 2>/dev/null | wc -l) files)"

# adw-gtk3 — pinned release tarball (NOT main). Filter doubles as structure assertion.
ADW_GTK3_VERSION=v6.5
ADW_GTK3_URL="https://github.com/lassekongo83/adw-gtk3/releases/download/${ADW_GTK3_VERSION}/adw-gtk3${ADW_GTK3_VERSION}.tar.xz"
ADW_GTK3_SHA256=a81780fadfc432be0fc3d89c4ebb41aa28e4f032d42c36f9789c57dd10cfa41c
log "adw-gtk3 ${ADW_GTK3_VERSION} (pinned + sha256-verified, load-bearing)"
curl -fsSL "$ADW_GTK3_URL" -o /tmp/adw-gtk3.tar.xz
echo "${ADW_GTK3_SHA256}  /tmp/adw-gtk3.tar.xz" | sha256sum -c -
install -d -m0755 /usr/share/themes
tar -xJf /tmp/adw-gtk3.tar.xz -C /usr/share/themes/ adw-gtk3 adw-gtk3-dark
rm -f /tmp/adw-gtk3.tar.xz
log "adw-gtk3 installed: $(ls -d /usr/share/themes/adw-gtk3 /usr/share/themes/adw-gtk3-dark | wc -l)/2 dirs present"

# GNOME desktop defaults via dconf (session-independent → every clone on first boot).
log "GNOME desktop defaults: adw-gtk3 + Papirus icons + Monaspace Neon Frozen mono + 3 window buttons"
install -d /etc/dconf/profile /etc/dconf/db/local.d
printf 'user-db:user\nsystem-db:local\n' >/etc/dconf/profile/user
cat >/etc/dconf/db/local.d/00-rmng-desktop <<'DCONF'
[org/gnome/desktop/interface]
gtk-theme='adw-gtk3'
icon-theme='Papirus'
monospace-font-name='Monaspace Neon Frozen 11'

[org/gnome/desktop/wm/preferences]
button-layout='appmenu:minimize,maximize,close'
DCONF
dconf update 2>/dev/null

# Apt lists stay (cleaned once in the Dockerfile tail) — same reasoning as phase 10.
