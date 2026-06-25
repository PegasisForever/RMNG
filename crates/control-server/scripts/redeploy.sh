#!/usr/bin/env bash
# Runs on the Proxmox node (shipped via `ssh node bash -s --`). Hot-swaps a clone's
# clone-daemon and/or agent-wrapper binaries WITHOUT reprovisioning: stop the user
# unit → `pct push` the new binary → chown/chmod → restart. Emits "P <step> <msg>".
#
#   redeploy.sh <ctid> <username> <clone-daemon-bin|-> <agent-wrapper-bin|->
# A binary path of "-" skips that component. The bins are node-side paths scp'd here
# by the control-server (from its embedded, gzipped copies).
set -euo pipefail
prog(){ echo "P $1 ${*:2}"; }
CTID="$1"; USER="${2:-pega}"; CD="${3:--}"; AW="${4:--}"
UID_="$(pct exec "$CTID" -- id -u "$USER")"
uctl(){ pct exec "$CTID" -- runuser -u "$USER" -- env XDG_RUNTIME_DIR="/run/user/$UID_" systemctl --user "$@"; }

swap(){ # <unit> <node-bin> <dest-in-ct>
  local unit="$1" bin="$2" dest="$3"
  [ "$bin" = "-" ] && return 0
  [ -f "$bin" ] || { echo "redeploy: missing staged binary $bin" >&2; return 1; }
  prog "$unit" "stopping"
  uctl stop "$unit" >&2 2>/dev/null || true
  prog "$unit" "pushing $(du -h "$bin" | cut -f1)"
  pct push "$CTID" "$bin" "$dest" >&2
  pct exec "$CTID" -- chown "$USER:$USER" "$dest" >&2
  pct exec "$CTID" -- chmod 755 "$dest" >&2
  rm -f "$bin"
  prog "$unit" "starting"
  uctl start "$unit" >&2
}

swap rmng-clone-daemon "$CD" "/home/$USER/rmng-clone-daemon"
swap agent-wrapper "$AW" "/home/$USER/agent-wrapper"
prog done "redeployed CT $CTID"
echo "RESULT redeployed $CTID"
