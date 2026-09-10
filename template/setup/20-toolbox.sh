#!/usr/bin/env bash
# Phase 20 — dev toolbox: the CT-104 dev-template app set, ALL via apt (no flatpak/snap):
# dev/CLI tools, Docker, cloud CLIs, build libs, fonts, themes, browsers, Cursor, VS Code,
# Zed, Celluloid/ffmpeg, Extension Manager, ONLYOFFICE, plus Mission Center /
# Monaspace / adw-gtk3 from their upstream releases, and the system-wide GNOME dconf defaults.
#
# STRICT by design: every step below fails the build on error (via `set -euo pipefail`, no
# warn-and-continue). A transient network/apt failure must fail here — not surface later as
# a template with a silently missing editor, font set, or theme. The load-bearing base
# desktop is already in place from phase 10.
set -euo pipefail
. /setup/lib.sh
enable_err_trap

log "dev toolbox: third-party apt repos (docker/chrome/gh/cursor/mozilla/azure/gcloud/stripe)"
. /etc/os-release; CODENAME="${VERSION_CODENAME:-resolute}"
apt-get install -y -qq ca-certificates curl gnupg >/dev/null 2>&1
install -d -m0755 /etc/apt/keyrings

curl -fsSL https://download.docker.com/linux/ubuntu/gpg | gpg --dearmor -o /etc/apt/keyrings/docker.gpg 2>/dev/null && chmod a+r /etc/apt/keyrings/docker.gpg
echo "deb [arch=amd64 signed-by=/etc/apt/keyrings/docker.gpg] https://download.docker.com/linux/ubuntu $CODENAME stable" > /etc/apt/sources.list.d/docker.list

curl -fsSL https://dl.google.com/linux/linux_signing_key.pub | gpg --dearmor -o /etc/apt/keyrings/google-chrome.gpg 2>/dev/null && chmod a+r /etc/apt/keyrings/google-chrome.gpg
echo "deb [arch=amd64 signed-by=/etc/apt/keyrings/google-chrome.gpg] http://dl.google.com/linux/chrome/deb/ stable main" > /etc/apt/sources.list.d/google-chrome.list

curl -fsSL https://cli.github.com/packages/githubcli-archive-keyring.gpg -o /etc/apt/keyrings/githubcli-archive-keyring.gpg 2>/dev/null && chmod a+r /etc/apt/keyrings/githubcli-archive-keyring.gpg
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
  curl -fsSL https://packages.mozilla.org/apt/repo-signing-key.gpg -o /etc/apt/keyrings/packages.mozilla.org.asc 2>/dev/null && chmod a+r /etc/apt/keyrings/packages.mozilla.org.asc
  echo "deb [signed-by=/etc/apt/keyrings/packages.mozilla.org.asc] https://packages.mozilla.org/apt mozilla main" > /etc/apt/sources.list.d/mozilla.list
  printf 'Package: *\nPin: origin packages.mozilla.org\nPin-Priority: 1000\n' > /etc/apt/preferences.d/mozilla
fi

curl -fsSL https://packages.microsoft.com/keys/microsoft.asc | gpg --dearmor -o /etc/apt/keyrings/microsoft.gpg 2>/dev/null && chmod a+r /etc/apt/keyrings/microsoft.gpg
echo "deb [arch=amd64 signed-by=/etc/apt/keyrings/microsoft.gpg] https://packages.microsoft.com/repos/azure-cli/ noble main" > /etc/apt/sources.list.d/azure-cli.list
# VS Code — same microsoft.gpg keyring imported just above; the `code` repo.
echo "deb [arch=amd64 signed-by=/etc/apt/keyrings/microsoft.gpg] https://packages.microsoft.com/repos/code stable main" > /etc/apt/sources.list.d/vscode.list

curl -fsSL https://packages.cloud.google.com/apt/doc/apt-key.gpg | gpg --dearmor -o /etc/apt/keyrings/cloud.google.gpg 2>/dev/null && chmod a+r /etc/apt/keyrings/cloud.google.gpg
echo "deb [signed-by=/etc/apt/keyrings/cloud.google.gpg] https://packages.cloud.google.com/apt cloud-sdk main" > /etc/apt/sources.list.d/google-cloud-sdk.list

curl -fsSL https://packages.stripe.dev/api/security/keypair/stripe-cli-gpg/public | gpg --dearmor -o /etc/apt/keyrings/stripe.gpg 2>/dev/null && chmod a+r /etc/apt/keyrings/stripe.gpg
echo "deb [signed-by=/etc/apt/keyrings/stripe.gpg] https://packages.stripe.dev/stripe-cli-debian-local stable main" > /etc/apt/sources.list.d/stripe.list

# ngrok — its own keyring (dedicated, matching the per-repo keyring pattern above). No auth
# token is baked here: NGROK_AUTHTOKEN is a per-clone preset env var (the agent reads it
# natively), set through the Presets UI, consistent with the no-env-settings invariant.
curl -fsSL https://ngrok-agent.s3.amazonaws.com/ngrok.asc | gpg --dearmor -o /etc/apt/keyrings/ngrok.gpg 2>/dev/null && chmod a+r /etc/apt/keyrings/ngrok.gpg
echo "deb [signed-by=/etc/apt/keyrings/ngrok.gpg] https://ngrok-agent.s3.amazonaws.com buster main" > /etc/apt/sources.list.d/ngrok.list

# ONLYOFFICE Desktop Editors — official repo (replaces the Flathub build). GPG-KEY file is
# ASCII-armored; handle the binary case too just in case. "squeeze" is ONLYOFFICE's fixed
# repo suite, not a Debian release.
if curl -fsSL https://download.onlyoffice.com/GPG-KEY-ONLYOFFICE -o /tmp/oo.key 2>/dev/null; then
  if grep -q "BEGIN PGP" /tmp/oo.key; then gpg --dearmor < /tmp/oo.key > /etc/apt/keyrings/onlyoffice.gpg; else cp /tmp/oo.key /etc/apt/keyrings/onlyoffice.gpg; fi
  chmod a+r /etc/apt/keyrings/onlyoffice.gpg; rm -f /tmp/oo.key
  echo "deb [signed-by=/etc/apt/keyrings/onlyoffice.gpg] https://download.onlyoffice.com/repo/debian squeeze main" > /etc/apt/sources.list.d/onlyoffice.list
fi

apt-get update -qq

log "dev toolbox: install (grouped for log readability — every group fails the build)"
apt-get install -y -qq fish ripgrep micro tmux just gh xdotool build-essential clang libclang-dev default-jdk
apt-get install -y -qq docker-ce docker-ce-cli containerd.io docker-buildx-plugin docker-compose-plugin fuse-overlayfs
apt-get install -y -qq azure-cli google-cloud-cli stripe ngrok
apt-get install -y -qq libsodium23 libpq-dev libcairo2 libcairo2-dev
apt-get install -y -qq fonts-noto-cjk fonts-noto-color-emoji papirus-icon-theme
apt-get install -y -qq celluloid ffmpeg firefox google-chrome-stable cursor code
# Former Flathub apps now available via apt: Extension Manager (Ubuntu universe) + ONLYOFFICE.
apt-get install -y -qq gnome-shell-extension-manager onlyoffice-desktopeditors
# desktop-file-utils: update-desktop-database for the .desktop entries shipped below
# (Zed) and above (Mission Center) — installed, never assumed.
apt-get install -y -qq desktop-file-utils

# Zed editor (https://zed.dev) — pinned GitHub release tarball + sha256, installed
# system-wide under /opt with a PATH wrapper + desktop entry (mirrors Mission Center).
# Pinned, not `latest`: re-pin deliberately by bumping ZED_VERSION + ZED_SHA256 together.
# The sha256 is the digest GitHub's release API reports for this exact asset file.
ZED_VERSION=v1.19.2
ZED_URL="https://github.com/zed-industries/zed/releases/download/${ZED_VERSION}/zed-linux-x86_64.tar.gz"
ZED_SHA256=c5acff2e52ac3c64890cce85250734cf7279c1de56d5926e4f4e1d4cf676359c
log "dev toolbox: Zed editor ${ZED_VERSION} (pinned + sha256-verified)"
curl -fsSL "$ZED_URL" -o /tmp/zed.tar.gz
echo "${ZED_SHA256}  /tmp/zed.tar.gz" | sha256sum -c -
rm -rf /opt/zed.app
tar -xzf /tmp/zed.tar.gz -C /opt
rm -f /tmp/zed.tar.gz
# Structure assertions: the tarball must unpack to zed.app/ with the launcher, the
# libexec editor binary, and a .desktop file — an upstream layout change fails the build
# here instead of shipping a broken install.
[ -x /opt/zed.app/bin/zed ]
[ -f /opt/zed.app/libexec/zed-editor ]
ZED_DESKTOP="$(ls /opt/zed.app/share/applications/dev.zed.Zed.desktop /opt/zed.app/share/applications/zed.desktop 2>/dev/null | head -1 || true)"
[ -n "$ZED_DESKTOP" ]
# Same Icon=/Exec= rewrite the official install script applies, pointed at /opt.
sed -e "s|Icon=zed|Icon=/opt/zed.app/share/icons/hicolor/512x512/apps/zed.png|g" -e "s|Exec=zed|Exec=/opt/zed.app/bin/zed|g" "$ZED_DESKTOP" > /usr/share/applications/dev.zed.Zed.desktop
grep -q '^Exec=/opt/zed.app/bin/zed' /usr/share/applications/dev.zed.Zed.desktop
grep -q '^Icon=/opt/zed.app/share/icons/hicolor/512x512/apps/zed.png' /usr/share/applications/dev.zed.Zed.desktop
printf '#!/bin/sh\nexec /opt/zed.app/bin/zed "$@"\n' > /usr/local/bin/zed; chmod 755 /usr/local/bin/zed
update-desktop-database /usr/share/applications >/dev/null 2>&1
# Shared-library gate: Zed bundles most of itself under zed.app/lib, but the loader still
# needs system X11/Wayland/audio libs. `ldd` names anything missing and the build fails
# here — not at first launch inside a clone.
ZED_MISSING="$(ldd /opt/zed.app/libexec/zed-editor 2>/dev/null | sed -n 's/^[[:space:]]*\(.*\) => not found$/\1/p' || true)"
if [ -n "$ZED_MISSING" ]; then echo "  !! Zed missing system libs:$ZED_MISSING" >&2; exit 1; fi
log "Zed installed: /opt/zed.app/bin/zed + dev.zed.Zed.desktop"

# Mission Center (system monitor) — no apt/deb upstream, only Flatpak + AppImage. Pull
# the PINNED x86_64 AppImage, --appimage-extract it (no FUSE needed), install the raw
# tree under /opt, and wire up a PATH wrapper + desktop entry (Exec rewritten to the
# wrapper). Pinned, not `latest`: upstream restructured the AppImage between releases
# (icon tree → single top .svg), so floating on latest ships whatever layout breaks the
# assumptions below. Re-pin deliberately: bump version + URL + sha256 together, and
# re-verify the asserted layout (top-level .svg + .desktop + AppRun).
MC_VERSION=1.2.0
MC_URL="https://gitlab.com/mission-center-devs/mission-center/-/jobs/15536631699/artifacts/raw/MissionCenter-${MC_VERSION}-x86_64.AppImage"
MC_SHA256=b3b5c84470a927d189c251039d223464125f7068b3164fda43acb9100384576d
log "dev toolbox: Mission Center ${MC_VERSION} (pinned + sha256-verified)"
curl -fsSL "$MC_URL" -o /tmp/mc.AppImage
echo "${MC_SHA256}  /tmp/mc.AppImage" | sha256sum -c -
chmod +x /tmp/mc.AppImage
( cd /tmp && rm -rf squashfs-root && /tmp/mc.AppImage --appimage-extract >/dev/null 2>&1 )
[ -d /tmp/squashfs-root ]
rm -rf /opt/mission-center; mv /tmp/squashfs-root /opt/mission-center; chown -R root:root /opt/mission-center
printf '#!/bin/sh\nexec /opt/mission-center/AppRun "$@"\n' > /usr/local/bin/mission-center; chmod 755 /usr/local/bin/mission-center
# Icons: upstream ships a single top-level .svg (themed Icon= name, no icon tree).
# Install it into hicolor/scalable where the themed lookup finds it.
mc_icon="$(ls /opt/mission-center/*.svg 2>/dev/null | head -1 || true)"
if [ -z "$mc_icon" ]; then echo "  !! no top-level .svg; state dump:" >&2; id >&2; ls -lad /opt /tmp /tmp/squashfs-root /opt/mission-center >&2; ls -la /opt/ >&2; mount | grep -E " /opt| /tmp" >&2; exit 1; fi
install -d /usr/share/icons/hicolor/scalable/apps
cp "$mc_icon" /usr/share/icons/hicolor/scalable/apps/
d="$(ls /opt/mission-center/usr/share/applications/*.desktop 2>/dev/null | head -1 || true)"; [ -n "$d" ] || d="$(ls /opt/mission-center/*.desktop 2>/dev/null | head -1 || true)"
[ -n "$d" ]
sed -E 's#^Exec=.*#Exec=/usr/local/bin/mission-center#; s#^TryExec=.*#TryExec=/usr/local/bin/mission-center#' "$d" > /usr/share/applications/io.missioncenter.MissionCenter.desktop
update-desktop-database /usr/share/applications >/dev/null 2>&1
gtk-update-icon-cache -f /usr/share/icons/hicolor >/dev/null 2>&1
rm -f /tmp/mc.AppImage
log "Mission Center installed"

# Monaspace fonts (githubnext/monaspace) — full set: static (family "Monaspace Neon"),
# frozen ("Monaspace Neon Frozen", texture-healing baked in — used as the default mono), and
# variable ("Monaspace Neon Var"). frozen+variable are TTF, static is OTF — copy both.
command -v unzip >/dev/null 2>&1 || apt-get install -y -qq unzip >/dev/null 2>&1
mona_install(){
  local json url v
  json="$(curl -fsSL https://api.github.com/repos/githubnext/monaspace/releases/latest 2>/dev/null)"
  [ -n "$json" ] || return 1
  rm -rf /tmp/mona; mkdir -p /tmp/mona /usr/share/fonts/monaspace
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

# adw-gtk3 (https://github.com/lassekongo83/adw-gtk3) — a GTK3 theme matching libadwaita's
# look, so legacy GTK3 apps stop clashing with the GTK4/libadwaita apps that already look
# native. Pinned release tarball (NOT `main`); the sha256 below was independently downloaded
# + hashed (matches the digest GitHub's own release-assets API reports for this file).
# visible product surface. Like every step in this file now, a failed download, checksum
# mismatch, or extract trips `set -e` and FAILS THE BUILD.
ADW_GTK3_VERSION=v6.5
ADW_GTK3_URL="https://github.com/lassekongo83/adw-gtk3/releases/download/${ADW_GTK3_VERSION}/adw-gtk3${ADW_GTK3_VERSION}.tar.xz"
ADW_GTK3_SHA256=a81780fadfc432be0fc3d89c4ebb41aa28e4f032d42c36f9789c57dd10cfa41c
log "adw-gtk3 ${ADW_GTK3_VERSION} (pinned + sha256-verified, load-bearing)"
curl -fsSL "$ADW_GTK3_URL" -o /tmp/adw-gtk3.tar.xz
echo "${ADW_GTK3_SHA256}  /tmp/adw-gtk3.tar.xz" | sha256sum -c -
install -d -m0755 /usr/share/themes
# Filter to the two expected top-level dirs: also doubles as a structure assertion — if the
# release ever stops shipping either, `tar` exits non-zero and (via set -e) fails the build.
tar -xJf /tmp/adw-gtk3.tar.xz -C /usr/share/themes/ adw-gtk3 adw-gtk3-dark
rm -f /tmp/adw-gtk3.tar.xz
log "adw-gtk3 installed: $(ls -d /usr/share/themes/adw-gtk3 /usr/share/themes/adw-gtk3-dark | wc -l)/2 dirs present"

# GNOME desktop defaults, system-wide via dconf (session-independent → every clone gets them
# on first boot): adw-gtk3 as the GTK theme, Papirus icons, Monaspace Neon Frozen 11 as the
# default monospace font, and all three window buttons (minimize/maximize/close). Users can
# still override per-session.
log "GNOME desktop defaults: adw-gtk3 + Papirus icons + Monaspace Neon Frozen mono + 3 window buttons"
install -d /etc/dconf/profile /etc/dconf/db/local.d
printf 'user-db:user\nsystem-db:local\n' > /etc/dconf/profile/user
cat > /etc/dconf/db/local.d/00-rmng-desktop <<'DCONF'
[org/gnome/desktop/interface]
gtk-theme='adw-gtk3'
icon-theme='Papirus'
monospace-font-name='Monaspace Neon Frozen 11'

[org/gnome/desktop/wm/preferences]
button-layout='appmenu:minimize,maximize,close'
DCONF
dconf update 2>/dev/null

# Apt lists deliberately stay (dropped once, in the Dockerfile's tail cleanup) — same
# reasoning as phase 10.
