#!/usr/bin/env bash
# Provision the STAGING control-server CT on a Proxmox node. Runs LOCALLY; ships this
# repo to the node, creates an Ubuntu CT with the full build toolchain (Rust + bun +
# GStreamer/VA/GTK *-dev*) + GPU render passthrough, builds the self-contained
# control-server, then runs cs-deploy-ct.sh so the CT comes up as a control-server that
# orchestrates REAL Proxmox clones — identical to the production deploy CT, just with
# the build toolchain. The build CT does NOT run GNOME/capture; real clones do.
#
# Think "staging vs production": same runtime (both run cs-deploy-ct.sh), and the build
# CT additionally carries the toolchain so you can rebuild + restart in place.
#
#   ./provision-build-ct.sh <proxmox-ssh-target> [hostname]
#   e.g. ./provision-build-ct.sh root@10.0.0.100 rmng-build
#
# Output: the CT's id + ip; binary at /usr/local/bin/rmng-control-server, dashboard on
# :9000. The lean production deploy CT is a separate provision-deploy-ct.sh run.
#
# NOTE: real provisioning + a ~10-min in-CT build; operator-supervised on first run.
set -euo pipefail

PROXMOX="${1:?usage: provision-build-ct.sh <proxmox-ssh-target> [hostname]}"
HOSTNAME="${2:-rmng-build}"
STORAGE="${RMNG_STORAGE:-local-lvm}"
BRIDGE="${RMNG_BRIDGE:-vmbr0}"
TEMPLATE="${RMNG_TEMPLATE:-ubuntu-26.04-standard_26.04-1_amd64.tar.zst}"
# Build CT compiles the whole workspace + runs the control-server → roomy.
CORES="${RMNG_CORES:-8}"
MEMORY="${RMNG_MEMORY:-12288}"
ROOTFS_GB="${RMNG_ROOTFS_GB:-40}"
SOCK_HOST_DIR="${RMNG_SOCK_DIR:-/srv/rmng-sock}"   # host dir bind-mounted (clone media socket)
# SSH target the control-server uses to reach the node from inside the CT (for `pct`).
PROXMOX_FROM_CT="${RMNG_PROXMOX_FROM_CT:-root@${PROXMOX#*@}}"

here="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"   # the RMNG project root
say(){ printf '\n\033[1;36m==>\033[0m %s\n' "$*"; }

say "packing source (RMNG incl. vendored agent-wrapper)"
TAR=/tmp/rmng-src-$$.tar.gz
# agent-wrapper is vendored inside the repo (./agent-wrapper) so cs-build-ct.sh can
# bun-compile + embed it. Pack the repo contents (leading ./) → extracts into /root/RMNG.
tar czf "$TAR" -C "$here" \
  --exclude ./target --exclude ./frontend/node_modules \
  --exclude ./frontend/build --exclude ./crates/control-server/embedded-bin \
  --exclude ./agent-wrapper/node_modules --exclude ./.git .
say "copying source to $PROXMOX"
scp -q "$TAR" "$PROXMOX:/tmp/rmng-src.tar.gz"
rm -f "$TAR"
say "copying deploy setup script to $PROXMOX"
scp -q "$here/scripts/cs-deploy-ct.sh" "$PROXMOX:/tmp/cs-deploy-ct.sh"

say "creating build CT + building (this takes ~10 min)…"
ssh "$PROXMOX" \
  HOSTNAME="$HOSTNAME" STORAGE="$STORAGE" BRIDGE="$BRIDGE" TEMPLATE="$TEMPLATE" \
  CORES="$CORES" MEMORY="$MEMORY" ROOTFS_GB="$ROOTFS_GB" \
  SOCK_HOST_DIR="$SOCK_HOST_DIR" PROXMOX_FROM_CT="$PROXMOX_FROM_CT" \
  'bash -s' <<'NODE'
set -euo pipefail
prog(){ printf '\033[1;32mP\033[0m %s\n' "$*" >&2; }

NAME="${TEMPLATE##*/}"
if ! pveam list local 2>/dev/null | grep -q "$NAME"; then
  prog "downloading $NAME"; pveam update >/dev/null 2>&1 || true
  pveam download local "$NAME" >/dev/null 2>&1 || true
fi
TMPL="local:vztmpl/$NAME"

ID="$(pvesh get /cluster/nextid 2>/dev/null || true)"
[ -n "$ID" ] || { for i in $(seq 200 999); do pct status "$i" >/dev/null 2>&1 || { ID=$i; break; }; done; }
[ -n "$ID" ] || { echo "no free CT id" >&2; exit 1; }

# World-writable socket dir so uid-mapped clone CTs can connect to the media socket here.
mkdir -p "$SOCK_HOST_DIR"; chmod 0777 "$SOCK_HOST_DIR"
prog "pct create $ID ($HOSTNAME)"
pct create "$ID" "$TMPL" \
  --hostname "$HOSTNAME" --unprivileged 1 --features nesting=1,keyctl=1,fuse=1 \
  --cores "$CORES" --memory "$MEMORY" --swap 2048 --rootfs "$STORAGE:$ROOTFS_GB" \
  --net0 "name=eth0,bridge=$BRIDGE,ip=dhcp" --onboot 1 >&2
# GPU render node (VA-API encode) + apparmor opt-out + the shared clone-socket dir
# bind-mounted at the SAME path (not under /run — the CT's tmpfs would shadow it).
{
  echo 'dev0: /dev/dri/renderD128,gid=993,mode=0666'
  echo 'lxc.apparmor.profile: unconfined'
  echo "mp0: $SOCK_HOST_DIR,mp=$SOCK_HOST_DIR"
} >> "/etc/pve/lxc/$ID.conf"

prog "starting CT $ID"
pct start "$ID" >&2
prog "waiting for DHCP + DNS"
IP=""
for _ in $(seq 1 60); do
  IP="$(pct exec "$ID" -- hostname -I 2>/dev/null | tr ' ' '\n' | grep -E '^[0-9]' | head -1 || true)"
  [ -n "$IP" ] && pct exec "$ID" -- getent hosts archive.ubuntu.com >/dev/null 2>&1 && break
  sleep 2
done
[ -n "$IP" ] || { echo "no DHCP lease" >&2; exit 1; }
RG="$(pct exec "$ID" -- getent group render 2>/dev/null | cut -d: -f3 || true)"
if [ -n "$RG" ] && [ "$RG" != 993 ]; then
  sed -i "s#renderD128,gid=[0-9]*#renderD128,gid=$RG#" "/etc/pve/lxc/$ID.conf"
  pct stop "$ID" >&2; pct start "$ID" >&2; sleep 5
fi

prog "pushing + extracting source"
pct push "$ID" /tmp/rmng-src.tar.gz /root/rmng-src.tar.gz >&2
pct exec "$ID" -- bash -c 'rm -rf /root/RMNG && mkdir -p /root/RMNG && tar xzf /root/rmng-src.tar.gz -C /root/RMNG' >&2

prog "building (cs-build-ct.sh)"
pct exec "$ID" -- bash /root/RMNG/scripts/cs-build-ct.sh >&2
rm -f /tmp/rmng-src.tar.gz

prog "configuring as staging control-server (cs-deploy-ct.sh)"
pct push "$ID" /tmp/cs-deploy-ct.sh /root/cs-deploy-ct.sh >&2
pct exec "$ID" -- bash /root/cs-deploy-ct.sh "$PROXMOX_FROM_CT" >&2
rm -f /tmp/cs-deploy-ct.sh

prog "authorizing the control-server's orchestration key on the node"
PUB="$(pct exec "$ID" -- cat /root/.ssh/id_ed25519.pub)"
install -d -m700 /root/.ssh; touch /root/.ssh/authorized_keys; chmod 600 /root/.ssh/authorized_keys
grep -qF "$PUB" /root/.ssh/authorized_keys || echo "$PUB" >> /root/.ssh/authorized_keys

echo "RESULT $ID $IP"
NODE

say "staging control-server ready (RESULT <id> <ip> above)."
echo "  Dashboard:    http://<ip>:9000   → Settings for Linear/Claude (optional; import accounts from a clone)."
echo "  Real clones:  POST http://<ip>:9000/api/template/bootstrap  then  POST /api/clone (CoW)."
echo "  Viewer:       RMNG_VIDEO=<ip>:9001 cargo run -p viewer   (once a clone is selected)."
echo "  Rebuild:      rsync source + re-run cs-build-ct.sh in the CT, then"
echo "                systemctl restart rmng-control-server."
echo "  Production deploy CT (lean, no toolchain): ./provision-deploy-ct.sh $PROXMOX"
