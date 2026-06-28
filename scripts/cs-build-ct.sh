#!/usr/bin/env bash
# Runs INSIDE the build CT (pushed by provision-build-ct.sh). This CT is BOTH the
# build box AND the dev/test box: it gets the full toolchain (Rust + bun + GStreamer/
# VA/PipeWire/GTK4 *-dev*) AND everything needed to RUN/TEST the whole stack —
# control-server (VA-API encode), clone-daemon (Mutter ScreenCast capture, needs a
# headless GNOME + PipeWire), and the viewer (GTK4 decode). Mirrors the hand-built
# CT 132. The whole workspace is built --release.
#
#   cs-build-ct.sh [src-dir]
set -euo pipefail
SRC="${1:-/root/RMNG}"
cd "$SRC"   # all cargo builds below are workspace-relative (the first embed build runs before any other cd)
export DEBIAN_FRONTEND=noninteractive

echo "[build-ct] installing build toolchain + control-server runtime (no capture/GNOME)" >&2
apt-get update -qq
# Two concerns only: (1) BUILD deps for the workspace — the *-dev libs clone-daemon/
# viewer/media link against, plus sassc/dpkg-dev for the patched gnome-shell deb; and
# (2) the control-server's VA-API *encode* runtime (it ingests clones' dmabufs and
# encodes them). NO capture/session stack: the build CT does NOT run a GNOME desktop —
# real clones do. The patched gnome-shell deb is *built* here (apt build-dep + download/
# source gnome-shell below), but gnome-shell itself is never installed or run.
apt-get install -y -qq \
  build-essential pkg-config clang git curl ca-certificates unzip sudo \
  libgstreamer1.0-dev libgstreamer-plugins-base1.0-dev libva-dev libdrm-dev \
  libpipewire-0.3-dev libgtk-4-dev \
  gstreamer1.0-plugins-base gstreamer1.0-plugins-good gstreamer1.0-plugins-bad \
  gstreamer1.0-tools libva2 libva-drm2 va-driver-all libdrm2 \
  openssh-client sshfs sassc dpkg-dev >&2
# NOTE on the VA plugin: vah264enc / vah264dec / vapostproc come from
# gstreamer1.0-plugins-bad (the `va` plugin). `gstreamer1.0-va` is NOT a package on
# 24.04, and `gstreamer1.0-vaapi` is the unrelated legacy plugin — don't use either.
# sassc + dpkg-dev + the build-dep below are for building the patched gnome-shell deb.

# Build deps for the patched gnome-shell .deb (shell-01 + shell-03). Enable deb-src,
# pull gnome-shell's build-deps once, and mark them done so build-shell-deb.sh (which
# would otherwise redo this) skips straight to the build.
echo "[build-ct] enabling deb-src + gnome-shell build-deps (for the patched shell)" >&2
if ! grep -rqs '^Types: deb deb-src' /etc/apt/sources.list.d/*.sources; then
  sed -i 's/^Types: deb$/Types: deb deb-src/' /etc/apt/sources.list.d/ubuntu.sources
  apt-get update -qq
fi
apt-get build-dep -y -qq gnome-shell >&2
mkdir -p /root/rmng-shell-build && touch /root/rmng-shell-build/.deps-done

if ! command -v cargo >/dev/null 2>&1; then
  echo "[build-ct] installing rust" >&2
  curl -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal >&2
fi
# shellcheck disable=SC1091
. "$HOME/.cargo/env"
export BUN_INSTALL="$HOME/.bun"; export PATH="$BUN_INSTALL/bin:$PATH"
if ! command -v bun >/dev/null 2>&1; then
  echo "[build-ct] installing bun" >&2
  curl -fsSL https://bun.sh/install | bash >&2
fi

# --- embedded binaries (gzipped) — staged BEFORE control-server so rust-embed
#     bakes them into the single self-contained binary ---
AW_SRC="$SRC/agent-wrapper"   # vendored inside the RMNG repo
EMB="$SRC/crates/control-server/embedded-bin"; mkdir -p "$EMB"
echo "[build-ct] clone-daemon (for embed)" >&2
cargo build --release -p clone-daemon >&2
gzip -c "$SRC/target/release/rmng-clone-daemon" > "$EMB/clone-daemon.gz"
if [ -d "$AW_SRC" ]; then
  echo "[build-ct] agent-wrapper bun --compile (for embed)" >&2
  ( cd "$AW_SRC" && bun install >&2 && bun build --compile src/server.ts --outfile /tmp/agent-wrapper >&2 )
  gzip -c /tmp/agent-wrapper > "$EMB/agent-wrapper.gz"; rm -f /tmp/agent-wrapper
  echo "[build-ct] embedded: clone-daemon.gz $(du -h "$EMB/clone-daemon.gz" | cut -f1), agent-wrapper.gz $(du -h "$EMB/agent-wrapper.gz" | cut -f1)" >&2
else
  echo "[build-ct] WARN: agent-wrapper source not at $AW_SRC; not embedding it" >&2
fi

# Patched gnome-shell deb (shell-01 hide-indicator + shell-03 enable-Eval) → embed it
# so the control-server can install it on each clone's template at bootstrap. Non-fatal:
# if it can't build, clones fall back to stock shell (no window-mgmt MCP, share pill shows).
echo "[build-ct] patched gnome-shell deb (for embed)" >&2
if bash "$SRC/gnome-patch/build-shell-deb.sh" > /tmp/shell-deb.out; then
  SHELL_DEB="$(sed -n 's/^DEB=//p' /tmp/shell-deb.out | tail -1)"
  if [ -n "$SHELL_DEB" ] && [ -f "$SHELL_DEB" ]; then
    gzip -c "$SHELL_DEB" > "$EMB/gnome-shell-deb.gz"
    echo "[build-ct] embedded: gnome-shell-deb.gz $(du -h "$EMB/gnome-shell-deb.gz" | cut -f1)" >&2
  fi
else
  echo "[build-ct] WARN: patched gnome-shell deb build failed; clones will use stock shell" >&2
fi
rm -f /tmp/shell-deb.out

echo "[build-ct] building frontend (bun → embedded) + the whole workspace --release" >&2
cd "$SRC/frontend"
bun install >&2
bun run build >&2
cd "$SRC"
# Build everything (control-server + clone-daemon + viewer + media). control-server
# now embeds the frontend + the gzipped clone-daemon/agent-wrapper → one artifact.
cargo build --release >&2
install -m755 target/release/rmng-control-server /usr/local/bin/rmng-control-server

cat >&2 <<DONE
[build-ct] build complete — NO GNOME/capture on the build CT (real clones run those).
  control-server binary: /usr/local/bin/rmng-control-server
    (embeds the frontend + gzipped clone-daemon + agent-wrapper + patched gnome-shell deb)
  provision-build-ct.sh runs cs-deploy-ct.sh next → the build CT becomes a STAGING
    control-server that orchestrates real Proxmox clones, identical to the production
    deploy CT but with the build toolchain still present.
  rebuild after syncing new source: re-run this script (apt/Rust/bun deps are cached),
    then restart the unit: systemctl restart rmng-control-server.
DONE
