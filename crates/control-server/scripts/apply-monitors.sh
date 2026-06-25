#!/usr/bin/env bash
# Runs on the Proxmox node (ssh node bash -s --). Apply a new monitor layout to a RUNNING
# clone WITHOUT reprovisioning: rewrite the clone-daemon `RMNG_MONITORS` + the
# gnome-headless dummy mode specs from the config, then restart the headless GNOME session
# + the daemon (which re-creates the virtual monitors and applies the layout at startup).
#   apply-monitors.sh <ctid> <username> <monitors-csv "WxH+X+Y[*],...">
set -euo pipefail
prog(){ echo "P $1 ${*:2}"; }
CTID="$1"; USER="${2:-pega}"; MONS="$3"
# dummy mode specs want just WxH (unique, colon-joined) — strip +X+Y and the * primary mark.
MODE_SPECS="$(printf '%s' "$MONS" | tr ',' '\n' | sed -E 's/\+.*$//; s/\*$//' | awk 'NF && !seen[$0]++' | paste -sd: -)"
[ -n "$MODE_SPECS" ] || MODE_SPECS="1920x1080"
U="$(pct exec "$CTID" -- id -u "$USER")"
GU="/home/$USER/.config/systemd/user/gnome-headless.service"
CU="/home/$USER/.config/systemd/user/rmng-clone-daemon.service"
inct(){ pct exec "$CTID" -- "$@"; }
uctl(){ inct runuser -u "$USER" -- env XDG_RUNTIME_DIR="/run/user/$U" systemctl --user "$@"; }

prog config "monitors=$MONS"
inct sed -i "s#MUTTER_DEBUG_DUMMY_MODE_SPECS=.*#MUTTER_DEBUG_DUMMY_MODE_SPECS=$MODE_SPECS#" "$GU"
inct bash -c "grep -q RMNG_MONITORS '$CU' || sed -i '/RMNG_SOCKET/a Environment=RMNG_MONITORS=x' '$CU'"
inct sed -i "s#RMNG_MONITORS=.*#RMNG_MONITORS=$MONS#" "$CU"
uctl daemon-reload >&2

# Stop the daemon BEFORE bouncing GNOME: otherwise the running daemon loses its Mutter
# session when gnome-shell restarts, crashes, and systemd auto-restarts it — racing our
# explicit restart into the StartLimit and leaving the service failed.
prog restart "restarting headless GNOME"
uctl stop rmng-clone-daemon.service >&2 2>/dev/null || true
uctl restart gnome-headless.service >&2
sleep 6
prog restart "starting clone-daemon"
uctl reset-failed rmng-clone-daemon.service >&2 2>/dev/null || true
uctl start rmng-clone-daemon.service >&2

prog done "applied monitor layout to CT $CTID"
echo "RESULT applied $CTID"
