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

# A preset PATH needs more than environment.d: interactive shells rewrite PATH on startup
# (login bash re-runs /etc/profile, which hard-resets PATH; fish rebuilds $PATH), so the
# inherited value reaches GUI apps but not a terminal — every OTHER preset var survives.
# Mirror the template's rmng-local-bin blocks: prepend the preset's dirs inside fish
# (conf.d), login sh/bash (profile.d), and non-login interactive bash (/etc/bash.bashrc).
# We always PREPEND (never replace) so the shell keeps its system dirs even if the preset
# set PATH outright, and drop any $PATH token; dirs are reversed so the listed order wins
# (each is prepended in turn). Args: <mnt> <decoded-preset-text> <etc-owner uid:gid>.
write_preset_path_rc(){
  mnt="$1"; env_text="$2"; owner="$3"
  path_val="$(printf '%s\n' "$env_text" | sed -n 's/^PATH=//p' | tail -1)"
  [ -n "$path_val" ] || return 0
  rev=""; OLDIFS="$IFS"; IFS=':'
  for seg in $path_val; do
    case "$seg" in ''|'$PATH'|'${PATH}') continue ;; esac
    rev="\"$seg\" $rev"
  done
  IFS="$OLDIFS"
  [ -n "$rev" ] || return 0
  install -d "$mnt/etc/fish/conf.d"
  printf 'for d in %s\n    if not contains -- "$d" $PATH\n        set -gx PATH "$d" $PATH\n    end\nend\n' "$rev" \
    > "$mnt/etc/fish/conf.d/rmng-preset-path.fish"
  printf '# rmng env preset: prepend the preset PATH dirs for login sh/bash.\nfor d in %s; do\n  case ":$PATH:" in\n    *":$d:"*) : ;;\n    *) PATH="$d:$PATH" ;;\n  esac\ndone\n' "$rev" \
    > "$mnt/etc/profile.d/rmng-preset-path.sh"
  chown "$owner" "$mnt/etc/fish/conf.d/rmng-preset-path.fish" "$mnt/etc/profile.d/rmng-preset-path.sh"
  brc="$mnt/etc/bash.bashrc"
  if [ -f "$brc" ]; then
    sed -i '/# >>> rmng-preset-path >>>/,/# <<< rmng-preset-path <<</d' "$brc" 2>/dev/null || true
    printf '# >>> rmng-preset-path >>>\n# rmng env preset: prepend preset PATH dirs for non-login interactive bash.\nfor d in %s; do\n  case ":$PATH:" in\n    *":$d:"*) : ;;\n    *) PATH="$d:$PATH" ;;\n  esac\ndone\n# <<< rmng-preset-path <<<\n' "$rev" \
      >> "$brc"
  fi
  prog identity "preset PATH → shell rc (fish conf.d + profile.d + bash.bashrc)"
}

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

SRC_RUNNING=0
pct status "$SRC_CTID" 2>/dev/null | grep -q 'status: running' && SRC_RUNNING=1
MNT=""
cleanup(){
  rc=$?; [ $rc -eq 0 ] && exit 0
  [ -n "$MNT" ] && mountpoint -q "$MNT" 2>/dev/null && umount "$MNT" 2>/dev/null || true
  [ -n "$MNT" ] && rmdir "$MNT" 2>/dev/null || true
  rm -f "/etc/pve/lxc/${NEWID}.conf"
  lvremove -fy "${VG}/${NEWLV}" >/dev/null 2>&1 || true
}
trap cleanup EXIT

# Live snapshot: flush the source FS instead of stopping the CT. `sync` pushes dirty
# page-cache to the LV so the CoW snapshot is near-consistent; ext4 journal recovery
# (on the mount below + first clone boot) covers the rest — same path as a power loss.
if [ "$SRC_RUNNING" = 1 ]; then
  prog sync "flushing source CT ${SRC_CTID} filesystem for a live snapshot"
  pct exec "$SRC_CTID" -- sync
fi

prog snapshot "CoW snapshot ${OLDLV} -> ${NEWLV}"
lvcreate -s --setactivationskip n -n "$NEWLV" "${VG}/${OLDLV}" >/dev/null
lvchange -ay "${VG}/${NEWLV}" >/dev/null

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
  write_preset_path_rc "$MNT" "$(printf '%s' "$ENV_B64" | base64 -d)" "$(stat -c '%u:%g' "$MNT/etc")"
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

# Clone resource limits (override whatever we copied from the template config): 32 GiB RAM,
# 8 GiB swap, CPU throttled to 16 cores' worth of time, and NO `cores` cap — the clone sees
# every host core (unlimited parallelism) while cpulimit bounds total CPU usage to 16.
sed -i '/^memory:/d; /^swap:/d; /^cores:/d; /^cpulimit:/d' "/etc/pve/lxc/${NEWID}.conf"
printf 'memory: 32768\nswap: 8192\ncpulimit: 16\n' >> "/etc/pve/lxc/${NEWID}.conf"
prog config "clone limits: 32G mem / 8G swap / cpulimit 16 / cores unlimited"

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
