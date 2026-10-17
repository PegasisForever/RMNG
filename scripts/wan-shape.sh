#!/usr/bin/env bash
# Make TCP port 9000 (the web port: dashboard and browser viewer) look like a slow link, for
# the viewer test bench (docs/WAN-BENCH.md). Run as root on the machine that publishes the
# port (CT 101). Other traffic is not shaped.
#
#   scripts/wan-shape.sh apply lan|home|hotspot
#   scripts/wan-shape.sh clear
#   scripts/wan-shape.sh show
#
# Down (server → viewer) is shaped on the egress of $DEV (default eth0). Up (viewer → server)
# is shaped on an ifb device: the ingress of $DEV sends port 9000 there. Each direction gets
# half of the round trip. In each direction netem adds the delay, jitter and loss, then tbf
# limits the rate; tbf's `latency` is the link's buffer.
set -euo pipefail

DEV="${DEV:-eth0}"
IFB=ifb9000
PORT=9000

clear_all() {
  tc qdisc del dev "$DEV" root 2>/dev/null || true
  tc qdisc del dev "$DEV" ingress 2>/dev/null || true
  ip link del "$IFB" 2>/dev/null || true
}

# leaf <dev> <"root" or "parent X:Y"> <delay> <jitter> <loss> <rate> <buffer>
leaf() {
  # shellcheck disable=SC2086 # $2 is two words on purpose
  tc qdisc add dev "$1" $2 handle 10: netem delay "$3" "$4" loss "$5"
  tc qdisc add dev "$1" parent 10:1 handle 20: tbf rate "$6" burst 16kb latency "$7"
}

# apply <one-way delay> <jitter> <loss> <down rate> <up rate> <buffer>
apply() {
  clear_all
  # Down: band 4 of a prio qdisc gets only source port 9000; the default priomap uses 1–3.
  tc qdisc add dev "$DEV" root handle 1: prio bands 4 priomap 1 2 2 2 1 2 0 0 1 1 1 1 1 1 1 1
  tc filter add dev "$DEV" parent 1: protocol ip prio 1 u32 match ip sport $PORT 0xffff flowid 1:4
  leaf "$DEV" "parent 1:4" "$1" "$2" "$3" "$4" "$6"
  # Up: destination port 9000 goes through the ifb device.
  ip link add "$IFB" type ifb
  ip link set "$IFB" up
  tc qdisc add dev "$DEV" handle ffff: ingress
  tc filter add dev "$DEV" parent ffff: protocol ip u32 match ip dport $PORT 0xffff \
    action mirred egress redirect dev "$IFB"
  leaf "$IFB" root "$1" "$2" "$3" "$5" "$6"
}

case "${1:-}/${2:-}" in
  apply/lan | clear/) clear_all ;;
  # About 50 ms round trip, 20 Mbit/s down, 10 up.
  apply/home) apply 25ms 2ms 0% 20mbit 10mbit 200ms ;;
  # About 150 ms round trip, 3 Mbit/s down, 1 up, 1 % loss each way, deep buffer.
  apply/hotspot) apply 75ms 10ms 1% 3mbit 1mbit 500ms ;;
  show/) tc -s qdisc show dev "$DEV"; tc -s qdisc show dev "$IFB" 2>/dev/null || true ;;
  *) echo "usage: $0 apply lan|home|hotspot | clear | show" >&2; exit 2 ;;
esac
