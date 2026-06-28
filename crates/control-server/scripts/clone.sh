# Remote clone script — runs on the Proxmox node via `ssh … bash -s --`.
# Ported from tests/cow-clone.sh (node portion), minus the control-hosts append:
# the web app owns state.json and registers the host itself.
#
# Args: SRC_ID NEWHOST MACPREFIX
# Emits: "P <step> <message>" progress lines, "RESULT <newid> <ip>" on success.
set -euo pipefail
SRC_ID="$1"; NEWHOST="$2"; MACPREFIX="$3"; USER="${4:-rmng}"; ENV_B64="${5:-}"
prog(){ echo "P $1 ${*:2}"; }
die(){ echo "$*" >&2; exit 1; }

prog locate "looking up source container ${SRC_ID}"
SRC_CONF="$(grep -ls "^hostname: ${SRC_ID}\$" /etc/pve/lxc/*.conf 2>/dev/null | head -1 || true)"
[ -n "$SRC_CONF" ] || die "no container has hostname '${SRC_ID}'"
SRC_CTID="$(basename "$SRC_CONF" .conf)"
prog locate "source is CT ${SRC_CTID}"

prog storage "checking rootfs storage"
ROOTFS_VAL="$(sed -n 's/^rootfs: //p' "$SRC_CONF" | head -1)"
[ -n "$ROOTFS_VAL" ] || die "CT ${SRC_CTID} has no rootfs"
STORAGE="${ROOTFS_VAL%%:*}"; REST="${ROOTFS_VAL#*:}"; OLDLV="${REST%%,*}"
SIZEOPT=""; [ "$REST" != "$OLDLV" ] && SIZEOPT=",${REST#*,}"
grep -qE "^lvmthin: ${STORAGE}\$" /etc/pve/storage.cfg || die "storage '${STORAGE}' is not LVM-thin"
VG="$(sed -n "/^lvmthin: ${STORAGE}\$/,/^[^[:space:]]/p" /etc/pve/storage.cfg | awk '/vgname/{print $2; exit}')"
[ -n "$VG" ] || die "no vgname for storage '${STORAGE}'"

prog allocate "allocating new container id"
NEWID="$(pvesh get /cluster/nextid)"; [ -n "$NEWID" ] || die "no free CTID"
NEWLV="vm-${NEWID}-disk-0"
[ -e "/etc/pve/lxc/${NEWID}.conf" ] && die "config for ${NEWID} already exists"
lvs "${VG}/${NEWLV}" >/dev/null 2>&1 && die "LV ${VG}/${NEWLV} already exists"

SRC_WAS_RUNNING=0
pct status "$SRC_CTID" 2>/dev/null | grep -q 'status: running' && SRC_WAS_RUNNING=1
MNT=""
cleanup(){
  rc=$?; [ $rc -eq 0 ] && exit 0
  if [ "$SRC_WAS_RUNNING" = 1 ]; then pct status "$SRC_CTID" 2>/dev/null | grep -q running || pct start "$SRC_CTID" 2>/dev/null || true; fi
  [ -n "$MNT" ] && mountpoint -q "$MNT" 2>/dev/null && umount "$MNT" 2>/dev/null || true
  [ -n "$MNT" ] && rmdir "$MNT" 2>/dev/null || true
  rm -f "/etc/pve/lxc/${NEWID}.conf"
  lvremove -fy "${VG}/${NEWLV}" >/dev/null 2>&1 || true
}
trap cleanup EXIT

if [ "$SRC_WAS_RUNNING" = 1 ]; then
  prog shutdown-source "shutting down source CT ${SRC_CTID} for a clean snapshot"
  pct shutdown "$SRC_CTID" --timeout 120 --forceStop 1
fi

prog snapshot "CoW snapshot ${OLDLV} -> ${NEWLV}"
lvcreate -s --setactivationskip n -n "$NEWLV" "${VG}/${OLDLV}" >/dev/null
lvchange -ay "${VG}/${NEWLV}" >/dev/null

if [ "$SRC_WAS_RUNNING" = 1 ]; then
  prog start-source "starting source CT ${SRC_CTID} back up"
  pct start "$SRC_CTID"
fi

prog identity "resetting machine-id + hostname on the clone"
MNT="$(mktemp -d)"
mount "/dev/${VG}/${NEWLV}" "$MNT"
: > "$MNT/etc/machine-id" 2>/dev/null || true
if [ -f "$MNT/var/lib/dbus/machine-id" ] && [ ! -L "$MNT/var/lib/dbus/machine-id" ]; then
  : > "$MNT/var/lib/dbus/machine-id" 2>/dev/null || true
fi
[ -f "$MNT/etc/hostname" ] && echo "$NEWHOST" > "$MNT/etc/hostname"
# Chosen env preset → the clone's session env BEFORE first boot (no session restart).
# Owner is read from /home/$USER so the file gets the container-mapped uid (idmap-safe
# for unprivileged CTs). systemd --user reads environment.d → all user units + the session.
if [ -n "$ENV_B64" ] && [ -d "$MNT/home/$USER" ]; then
  OWNER="$(stat -c '%u:%g' "$MNT/home/$USER")"
  install -d -o "${OWNER%:*}" -g "${OWNER#*:}" "$MNT/home/$USER/.config/environment.d"
  printf '%s' "$ENV_B64" | base64 -d > "$MNT/home/$USER/.config/environment.d/30-rmng-preset.conf"
  chown "$OWNER" "$MNT/home/$USER/.config/environment.d/30-rmng-preset.conf"
  prog identity "wrote env preset → 30-rmng-preset.conf"
fi
umount "$MNT"; rmdir "$MNT"; MNT=""

prog config "writing container config for CT ${NEWID}"
genmac(){ printf '%s:%02X:%02X:%02X' "$MACPREFIX" $((RANDOM%256)) $((RANDOM%256)) $((RANDOM%256)); }
sed -e '/^\[/,$d' -e '/^lock:/d' "$SRC_CONF" > "/etc/pve/lxc/${NEWID}.conf"
sed -i "s#^rootfs: .*#rootfs: ${STORAGE}:${NEWLV}${SIZEOPT}#" "/etc/pve/lxc/${NEWID}.conf"
sed -i "s#^hostname: .*#hostname: ${NEWHOST}#"                "/etc/pve/lxc/${NEWID}.conf"
for nid in $(grep -oE '^net[0-9]+' "/etc/pve/lxc/${NEWID}.conf" || true); do
  sed -i "s#\(^${nid}:.*hwaddr=\)[0-9A-Fa-f:]\{17\}#\1$(genmac)#" "/etc/pve/lxc/${NEWID}.conf"
done

prog start-clone "starting clone CT ${NEWID}"
pct start "$NEWID"
trap - EXIT

prog wait-lease "waiting for an eth0 (vmbr0) DHCP lease"
IP=""
for _ in $(seq 1 60); do
  IP="$(pct exec "$NEWID" -- ip -4 -br addr show eth0 2>/dev/null | grep -oE '[0-9]+(\.[0-9]+){3}' | head -1 || true)"
  [ -n "$IP" ] && break
  sleep 2
done
[ -n "$IP" ] || die "clone CT ${NEWID} booted but got no eth0 DHCP lease"

prog done "clone CT ${NEWID} up at ${IP}"
echo "RESULT ${NEWID} ${IP}"
