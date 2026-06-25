#!/usr/bin/env bash
# Build a patched gnome-shell .deb carrying ONLY the two patches rmng needs:
#   shell-01  hide the screen-sharing ("being watched") indicator — the clone's
#             Mutter RemoteDesktop session would otherwise paint an orange pill
#             into every captured frame the viewer shows.
#   shell-03  allow org.gnome.Shell.Eval without unsafe_mode — clone-daemon's
#             window-management MCP tools (list/move/launch windows) need it.
# (The grd-* / shell-02 patches are obsolete: rmng bypasses gnome-remote-desktop
#  entirely and uses Mutter directly, and applies its own monitor layout.)
#
# Runs INSIDE the build CT. REPACK approach: both patches live in the JS that meson
# compiles into a gresource inside libshell-<N>.so, so we rebuild only that one .so
# and swap it into the stock gnome-shell .deb (everything else identical), bumping
# the version with a +ngshell suffix so it installs cleanly over stock.
#
# Output: progress on stderr; the produced deb path on stdout as `DEB=<path>`.
# Cached: re-running is a no-op if the deb is newer than the patches (FORCE=1 rebuilds).
#
#   ./build-shell-deb.sh        # build (or reuse cached) → prints DEB=<path>
set -euo pipefail
PATCHES="$(cd "$(dirname "${BASH_SOURCE[0]}")/patches" && pwd)"
WORK="${WORK:-/root/rmng-shell-build}"
OUT="${OUT:-$WORK/out}"
SUFFIX="${RMNG_SHELL_SUFFIX:-ngshell1}"
export DEBIAN_FRONTEND=noninteractive
say(){ echo "[shell-deb] $*" >&2; }

mkdir -p "$WORK" "$OUT"

# --- cache: skip everything if a built deb is newer than all inputs ----------------
P01="$PATCHES/shell-01-hide-screen-sharing-indicator.patch"
P03="$PATCHES/shell-03-enable-eval.patch"
EXISTING="$(ls -t "$OUT"/gnome-shell_*+"$SUFFIX"_amd64.deb 2>/dev/null | head -1 || true)"
if [ -n "$EXISTING" ] && [ -z "${FORCE:-}" ] \
   && [ "$EXISTING" -nt "$P01" ] && [ "$EXISTING" -nt "$P03" ] \
   && [ "$EXISTING" -nt "${BASH_SOURCE[0]}" ]; then
  say "up to date: $EXISTING (set FORCE=1 to rebuild)"
  echo "DEB=$EXISTING"; exit 0
fi

# --- build deps (marker-cached: the slow apt build-dep runs once) ------------------
if [ ! -f "$WORK/.deps-done" ] || [ -n "${FORCE:-}" ]; then
  say "enabling deb-src + installing build deps (one-time, slow)"
  if ! grep -rqs '^Types: deb deb-src' /etc/apt/sources.list.d/*.sources; then
    sed -i 's/^Types: deb$/Types: deb deb-src/' /etc/apt/sources.list.d/ubuntu.sources
  fi
  apt-get update -qq
  apt-get build-dep -y -qq gnome-shell >&2
  apt-get install -y -qq sassc dpkg-dev >&2
  touch "$WORK/.deps-done"
fi

cd "$WORK"

# --- stock binary deb (source of every file except libshell) + the source tree ----
say "fetching stock gnome-shell .deb + source"
rm -f gnome-shell_*_amd64.deb
apt-get download gnome-shell >&2
STOCK_DEB="$(ls -t gnome-shell_*_amd64.deb | head -1)"
[ -n "$STOCK_DEB" ] || { echo "no stock gnome-shell .deb downloaded" >&2; exit 1; }
STOCK_VER="$(dpkg-deb -f "$STOCK_DEB" Version)"
say "stock version: $STOCK_VER"

rm -rf gnome-shell-*/
apt-get source gnome-shell >&2
SRCDIR="$(find . -maxdepth 1 -type d -name 'gnome-shell-*' | head -1)"
[ -n "$SRCDIR" ] || { echo "no gnome-shell source directory after apt-get source" >&2; exit 1; }

# --- apply ONLY shell-01 + shell-03 -----------------------------------------------
say "applying shell-01 + shell-03"
( cd "$SRCDIR"
  patch -p1 < "$P01"
  patch -p1 < "$P03"
)

# --- detect the libshell soname from the stock deb, build just that target --------
SONAME="$(dpkg-deb -c "$STOCK_DEB" | grep -oE 'libshell-[0-9]+\.so' | head -1)"
[ -n "$SONAME" ] || { echo "couldn't detect libshell soname in stock deb" >&2; exit 1; }
say "building $SONAME (meson + ninja, single target)"
( cd "$SRCDIR"
  [ -d build ] || meson setup build --prefix=/usr --libdir=lib --buildtype=release \
    -Dtests=false -Dportal_helper=false -Dman=false >&2
  ninja -C build "src/$SONAME" >&2
)
BUILT_SO="$SRCDIR/build/src/$SONAME"
[ -f "$BUILT_SO" ] || { echo "$SONAME not produced by ninja" >&2; exit 1; }

# --- repack the stock deb with the patched .so + a +SUFFIX version -----------------
say "repacking deb"
EXTRACT="$WORK/extract"; rm -rf "$EXTRACT"; mkdir -p "$EXTRACT"
dpkg-deb -R "$STOCK_DEB" "$EXTRACT"
TARGET_SO="$(cd "$EXTRACT" && find . -name "$SONAME" | head -1)"
[ -n "$TARGET_SO" ] || { echo "$SONAME not present inside stock deb" >&2; exit 1; }
install -m644 "$BUILT_SO" "$EXTRACT/$TARGET_SO"

NEW_VER="${STOCK_VER}+${SUFFIX}"
sed -i "s/^Version: .*/Version: ${NEW_VER}/" "$EXTRACT/DEBIAN/control"
# refresh md5sums so the package is self-consistent
( cd "$EXTRACT" && find usr -type f -print0 2>/dev/null | xargs -0 md5sum > DEBIAN/md5sums )

DEB_OUT="$OUT/gnome-shell_${NEW_VER}_amd64.deb"
rm -f "$OUT"/gnome-shell_*+"$SUFFIX"_amd64.deb
dpkg-deb --build "$EXTRACT" "$DEB_OUT" >&2
say "built $DEB_OUT ($(du -h "$DEB_OUT" | cut -f1)); patched $SONAME from $STOCK_VER"
echo "DEB=$DEB_OUT"
