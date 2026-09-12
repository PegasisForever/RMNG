#!/usr/bin/env bash
# Phase 15 — install the patched gnome-shell .deb (/tmp/gnome-shell.deb, from gnome-build).
# dpkg -i, NOT apt: apt would re-drag gdm3 via the deb's stock Recommends.
set -euo pipefail
. /setup/lib.sh
enable_err_trap

DEB=/tmp/gnome-shell.deb
test -f "$DEB"

log "install patched gnome-shell (shell-01 + shell-02)"
dpkg -i "$DEB"
log "patched gnome-shell installed: $(dpkg-query -W -f='${Version}' gnome-shell)"

# Pin it: a later Ubuntu SRU would otherwise swap the stock shell back in (un-patching
# Shell.Eval). Moving base = unhold + rebuild.
apt-mark hold gnome-shell
log "held gnome-shell at $(dpkg-query -W -f='${Version}' gnome-shell)"
rm -f "$DEB"
