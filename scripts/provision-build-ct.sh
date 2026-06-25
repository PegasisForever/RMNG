#!/usr/bin/env bash
# Provision a BUILD/development CT on a Proxmox node and build the control-server in
# it. Runs LOCALLY; ships this repo's rmng/ to the node, creates an Ubuntu CT
# with the full dev toolchain (Rust + bun + GStreamer/VA *-dev*) and the GPU render
# node passed through (so you can also run/test there), then builds the binary.
#
#   ./provision-build-ct.sh <proxmox-ssh-target> [hostname]
#   e.g. ./provision-build-ct.sh root@10.0.0.100 rmng-build
#
# Output: the build CT's id + ip; the binary lands at /usr/local/bin/rmng-control-server
# inside it. provision-deploy-ct.sh copies that binary into a lean runtime CT.
#
# NOTE: real provisioning + a ~10-min in-CT build; operator-supervised on first run.
set -euo pipefail

PROXMOX="${1:?usage: provision-build-ct.sh <proxmox-ssh-target> [hostname]}"
HOSTNAME="${2:-rmng-build}"
STORAGE="${RMNG_STORAGE:-local-lvm}"
BRIDGE="${RMNG_BRIDGE:-vmbr0}"
TEMPLATE="${RMNG_TEMPLATE:-ubuntu-26.04-standard_26.04-1_amd64.tar.zst}"
# Build CT is also the dev/test box (full stack incl. headless GNOME) → roomy.
CORES="${RMNG_CORES:-8}"
MEMORY="${RMNG_MEMORY:-12288}"
ROOTFS_GB="${RMNG_ROOTFS_GB:-40}"

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

say "creating build CT + building (this takes ~10 min)…"
ssh "$PROXMOX" \
  HOSTNAME="$HOSTNAME" STORAGE="$STORAGE" BRIDGE="$BRIDGE" TEMPLATE="$TEMPLATE" \
  CORES="$CORES" MEMORY="$MEMORY" ROOTFS_GB="$ROOTFS_GB" \
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

prog "pct create $ID ($HOSTNAME)"
pct create "$ID" "$TMPL" \
  --hostname "$HOSTNAME" --unprivileged 1 --features nesting=1,keyctl=1,fuse=1 \
  --cores "$CORES" --memory "$MEMORY" --swap 2048 --rootfs "$STORAGE:$ROOTFS_GB" \
  --net0 "name=eth0,bridge=$BRIDGE,ip=dhcp" --onboot 1 >&2
# GPU render node passthrough (so the build CT can also run/test) + apparmor opt-out.
{ echo 'dev0: /dev/dri/renderD128,gid=993,mode=0666'; echo 'lxc.apparmor.profile: unconfined'; } \
  >> "/etc/pve/lxc/$ID.conf"

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
echo "RESULT $ID $IP"
NODE

say "build CT ready (RESULT <id> <ip> above). Next:"
echo "  ./provision-deploy-ct.sh $PROXMOX        # creates the runtime CT + copies the binary"
echo "  (to rebuild later: re-sync source + re-run cs-build-ct.sh in the CT)"
