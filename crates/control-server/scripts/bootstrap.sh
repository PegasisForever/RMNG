#!/usr/bin/env bash
# Runs on the Proxmox node (shipped via `ssh node bash -s --`). Builds a rmng
# template/clone CT from a base image, end-to-end (the "from-zero" path, proven on
# CT 132 rmng-build). Emits "P <step> <msg>" progress + "RESULT <ctid> <ip>".
#
#   bootstrap.sh <hostname> <template-vztmpl> <storage> <bridge> <prov_b64>
# where prov_b64 = base64(provision-clone.sh), run inside the new CT.
set -euo pipefail
prog(){ echo "P $1 ${*:2}"; }
HOSTNAME="$1"; TEMPLATE="$2"; STORAGE="${3:-local-lvm}"; BRIDGE="${4:-vmbr0}"; PROV_B64="$5"
CD_BIN="${6:-}"  # node-side path to the clone-daemon binary (scp'd here by the control-server)
AW_BIN="${7:-}"  # node-side path to the agent-wrapper binary (scp'd here by the control-server)
MONITORS="${8:-}"  # monitor layout CSV "WxH,WxH" (config.monitors) → clone-daemon RMNG_MONITORS
SHELL_DEB="${9:-}"  # node-side path to the patched gnome-shell .deb (shell-01 + shell-03)

prog locate "ensuring base image $TEMPLATE"
case "$TEMPLATE" in
  *vztmpl/*) NAME="${TEMPLATE#*vztmpl/}";;
  *) NAME="$TEMPLATE";;
esac
if ! pveam list local 2>/dev/null | grep -q "$NAME"; then
  prog locate "downloading $NAME"
  pveam update >/dev/null 2>&1 || true
  pveam download local "$NAME" >/dev/null 2>&1 || true
fi

prog allocate "picking a free CT id"
NEWID=$(pvesh get /cluster/nextid 2>/dev/null || echo "")
[ -n "$NEWID" ] || { for i in $(seq 200 999); do pct status "$i" >/dev/null 2>&1 || { NEWID=$i; break; }; done; }
[ -n "$NEWID" ] || { echo "no free CT id" >&2; exit 1; }

prog config "pct create $NEWID ($HOSTNAME)"
pct create "$NEWID" "$TEMPLATE" \
  --hostname "$HOSTNAME" --unprivileged 1 --features nesting=1,keyctl=1,fuse=1 \
  --cores 8 --memory 16384 --swap 4096 --rootfs "$STORAGE:48" \
  --net0 "name=eth0,bridge=$BRIDGE,ip=dhcp" --onboot 0 >&2
# render node passthrough (mode 0666) + apparmor opt-out (headless Mutter path) +
# the shared control-server media socket dir bind-mounted at the SAME path (NOT under
# /run — the CT's tmpfs would shadow it). clone-daemon ships to <dir>/clones.sock.
# Clones inherit this via clone.sh's config copy.
SOCK_HOST_DIR="${RMNG_SOCK_DIR:-/srv/rmng-sock}"
mkdir -p "$SOCK_HOST_DIR"; chmod 0777 "$SOCK_HOST_DIR"
printf '%s\n' \
  'dev0: /dev/dri/renderD128,gid=993,mode=0666' \
  'lxc.apparmor.profile: unconfined' \
  "mp0: $SOCK_HOST_DIR,mp=$SOCK_HOST_DIR" \
  >> "/etc/pve/lxc/$NEWID.conf"

prog start-clone "starting CT $NEWID"
pct start "$NEWID" >&2

prog wait-lease "waiting for DHCP + DNS"
IP=""
for _ in $(seq 1 60); do
  IP="$(pct exec "$NEWID" -- hostname -I 2>/dev/null | tr ' ' '\n' | grep -E '^[0-9]' | head -1 || true)"
  [ -n "$IP" ] && pct exec "$NEWID" -- getent hosts archive.ubuntu.com >/dev/null 2>&1 && break
  sleep 2
done
[ -n "$IP" ] || { echo "no DHCP lease" >&2; exit 1; }
RG="$(pct exec "$NEWID" -- getent group render 2>/dev/null | cut -d: -f3 || true)"
[ -n "$RG" ] && sed -i "s#renderD128,gid=[0-9]*#renderD128,gid=$RG#" "/etc/pve/lxc/$NEWID.conf"

prog identity "provisioning (headless GNOME + clone-daemon; ~5-10 min)"
# Push the embedded binaries so provision-clone.sh can install them (else it warns).
if [ -n "$CD_BIN" ] && [ -f "$CD_BIN" ]; then
  pct push "$NEWID" "$CD_BIN" /root/rmng-clone-daemon >&2; rm -f "$CD_BIN"
fi
if [ -n "$AW_BIN" ] && [ -f "$AW_BIN" ]; then
  pct push "$NEWID" "$AW_BIN" /root/agent-wrapper >&2; rm -f "$AW_BIN"
fi
# Patched gnome-shell deb → /root/gnome-shell-patched.deb; provision-clone.sh installs it.
if [ -n "$SHELL_DEB" ] && [ -f "$SHELL_DEB" ]; then
  pct push "$NEWID" "$SHELL_DEB" /root/gnome-shell-patched.deb >&2; rm -f "$SHELL_DEB"
fi
printf '%s' "$PROV_B64" | base64 -d > "/tmp/prov-$NEWID.sh"
pct push "$NEWID" "/tmp/prov-$NEWID.sh" /root/provision-clone.sh >&2
pct exec "$NEWID" -- bash /root/provision-clone.sh pega pega "$MONITORS" >&2
rm -f "/tmp/prov-$NEWID.sh"

prog done "CT $NEWID ready at $IP"
echo "RESULT $NEWID $IP"
