#!/usr/bin/env bash
# Make one TCP port look like a slow link, for the viewer test bench (docs/WAN-BENCH.md). Run as
# root on the machine that publishes the web port (CT 101). Other traffic is not shaped.
#
#   scripts/wan-shape.sh proxy start|stop
#   scripts/wan-shape.sh apply lan|home|hotspot [port]
#   scripts/wan-shape.sh clear
#   scripts/wan-shape.sh show
#
# `proxy start` puts a user-space TCP proxy (socat) on port 9001 in front of 127.0.0.1:9000, as
# `tailscale serve` does in production: the viewer's TCP connection ends in the proxy, and the
# proxy opens its own connection to the server. `apply` shapes port 9001 (the proxy) unless
# another port is given; 9000 shapes the direct path.
#
# Down (server → viewer) is shaped on the egress of $DEV (default eth0). Up (viewer → server)
# is shaped on an ifb device: the ingress of $DEV sends the port there. Each direction gets
# half of the round trip. In each direction netem adds the delay and loss, then tbf limits the
# rate; tbf's `latency` is the link's buffer. No jitter: netem jitter reorders packets, which a
# real link seldom does.
#
# The CT shares the Proxmox host's kernel, and `tc` makes the kernel load a missing qdisc,
# filter or action module on the host. So `apply` first checks that every module it needs is
# loaded already, and stops if one is not: loading it is the host owner's decision.
set -euo pipefail

DEV="${DEV:-eth0}"
IFB=ifb9000
PROXY_PORT=9001
PIDFILE=/run/wan-proxy.pid
MODULES=(sch_prio sch_netem sch_tbf sch_ingress cls_u32 act_mirred ifb)

check_modules() {
  local missing=()
  for m in "${MODULES[@]}"; do
    grep -q "^$m " /proc/modules || missing+=("$m")
  done
  if [ ${#missing[@]} -gt 0 ]; then
    echo "not loaded: ${missing[*]}. Nothing was changed. The host owner can run:" >&2
    echo "  modprobe -a ${missing[*]}" >&2
    exit 1
  fi
}

# Remove only what `apply` added: the prio root (handle 1:), an ingress qdisc whose filter
# sends to $IFB, and $IFB itself.
clear_all() {
  if tc qdisc show dev "$DEV" root | grep -q '^qdisc prio 1: root'; then
    tc qdisc del dev "$DEV" root
  fi
  if tc filter show dev "$DEV" parent ffff: 2>/dev/null | grep -qi "redirect to device $IFB"; then
    tc qdisc del dev "$DEV" ingress
  fi
  if ip link show "$IFB" >/dev/null 2>&1; then
    ip link del "$IFB"
  fi
}

# leaf <dev> <"root" or "parent X:Y"> <delay> <loss> <rate> <buffer>
leaf() {
  # shellcheck disable=SC2086 # $2 is two words on purpose
  tc qdisc add dev "$1" $2 handle 10: netem delay "$3" loss "$4"
  tc qdisc add dev "$1" parent 10:1 handle 20: tbf rate "$5" burst 16kb latency "$6"
}

# apply <port> <one-way delay> <loss> <down rate> <up rate> <buffer>
apply() {
  check_modules
  clear_all
  local port=$1
  shift
  # Down: band 4 of a prio qdisc gets only the source port; the default priomap uses 1–3.
  tc qdisc add dev "$DEV" root handle 1: prio bands 4 priomap 1 2 2 2 1 2 0 0 1 1 1 1 1 1 1 1
  tc filter add dev "$DEV" parent 1: protocol ip prio 1 u32 match ip sport "$port" 0xffff flowid 1:4
  leaf "$DEV" "parent 1:4" "$1" "$2" "$3" "$5"
  # Up: the destination port goes through the ifb device.
  ip link add "$IFB" type ifb
  ip link set "$IFB" up
  tc qdisc add dev "$DEV" handle ffff: ingress
  tc filter add dev "$DEV" parent ffff: protocol ip u32 match ip dport "$port" 0xffff \
    action mirred egress redirect dev "$IFB"
  leaf "$IFB" root "$1" "$2" "$4" "$5"
}

proxy_stop() {
  if [ -f "$PIDFILE" ]; then
    kill "$(cat "$PIDFILE")" 2>/dev/null || true
    rm -f "$PIDFILE"
  fi
}

proxy_start() {
  command -v socat >/dev/null || { echo "socat is not installed (apt install socat)" >&2; exit 1; }
  proxy_stop
  # Nagle off on both sides, as tailscaled (Go) has it.
  setsid socat TCP-LISTEN:$PROXY_PORT,fork,reuseaddr,nodelay TCP:127.0.0.1:9000,nodelay \
    </dev/null >/dev/null 2>&1 &
  echo $! >"$PIDFILE"
}

port="${3:-$PROXY_PORT}"
case "${1:-}/${2:-}" in
  proxy/start) proxy_start ;;
  proxy/stop) proxy_stop ;;
  apply/lan | clear/) clear_all ;;
  # About 50 ms round trip, 20 Mbit/s down, 10 up.
  apply/home) apply "$port" 25ms 0% 20mbit 10mbit 200ms ;;
  # About 150 ms round trip, 3 Mbit/s down, 1 up, 1 % loss each way, deep buffer.
  apply/hotspot) apply "$port" 75ms 1% 3mbit 1mbit 500ms ;;
  show/) tc -s qdisc show dev "$DEV"; tc -s qdisc show dev "$IFB" 2>/dev/null || true ;;
  *)
    echo "usage: $0 proxy start|stop | apply lan|home|hotspot [port] | clear | show" >&2
    exit 2
    ;;
esac
