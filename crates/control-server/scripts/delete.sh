# Remote delete script — runs on the Proxmox node via `ssh … bash -s --`.
# Gracefully stops the container (if running) then destroys it; for an
# LVM-thin CoW clone this removes the config + the thin-snapshot volume, leaving
# the origin untouched.
#
# Args: CTID
# Emits: "P <step> <message>" progress lines, "RESULT ok" on success.
set -euo pipefail
CTID="$1"
prog(){ echo "P $1 ${*:2}"; }
die(){ echo "$*" >&2; exit 1; }

prog check "checking container ${CTID}"
pct status "$CTID" >/dev/null 2>&1 || die "container ${CTID} does not exist"
if pct status "$CTID" 2>/dev/null | grep -q running; then
  prog stop "shutting down CT ${CTID}"
  pct shutdown "$CTID" --timeout 60 --forceStop 1
fi
prog destroy "destroying CT ${CTID} (config + thin-snapshot volume)"
pct destroy "$CTID"
prog done "CT ${CTID} destroyed"
echo "RESULT ok"
