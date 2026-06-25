#!/usr/bin/env bash
# Bring up a PERSISTENT control-server + clone-daemon on the build/dev CT (which has
# the headless GNOME session + GPU), as transient systemd units so they survive ssh
# disconnect and can be restarted/stopped. Run as root. Idempotent: re-running
# restarts both. Point the viewer at <this-host>:9001.
set -uo pipefail
USER_NAME="${RMNG_USER:-pega}"; UID_NUM="$(id -u "$USER_NAME")"; RUNDIR="/run/user/${UID_NUM}"
SRC=/root/rmng; T=/tmp/rmng-run; BIN="$T/bin"

echo "== stop any existing units =="
systemctl stop rmng-control-server rmng-clone-daemon 2>/dev/null
systemctl reset-failed rmng-control-server rmng-clone-daemon 2>/dev/null
pkill -f "$BIN/rmng-clone-daemon" 2>/dev/null; pkill -f "$BIN/rmng-control-server" 2>/dev/null
sleep 1

echo "== stage binaries + config/state (host @127.0.0.1, selected) =="
rm -rf "$T"; mkdir -p "$T/data" "$BIN"
cp "$SRC/target/debug/rmng-control-server" "$SRC/target/debug/rmng-clone-daemon" "$BIN/"
echo '{ "dataDir": "/tmp/rmng-run/data" }' > "$T/config.json"
echo '{ "selected": "testclone", "hosts": [ { "id": "testclone", "host": "127.0.0.1" } ] }' > "$T/data/state.json"
chmod -R 777 "$T"

ENVS="--setenv=XDG_RUNTIME_DIR=$RUNDIR --setenv=DBUS_SESSION_BUS_ADDRESS=unix:path=$RUNDIR/bus"

echo "== start control-server (ports 9000-9003 + clone socket) =="
systemd-run --unit=rmng-control-server --uid="$USER_NAME" $ENVS \
  --setenv=RMNG_CONFIG="$T/config.json" --setenv=RMNG_CLONE_SOCKET="$T/clones.sock" \
  --setenv=RUST_LOG=info,tower_http=warn,control_server::mediaplane=debug --property=Restart=on-failure \
  "$BIN/rmng-control-server" >/dev/null

echo "== start clone-daemon (Mutter capture + MCP :9004) =="
systemd-run --unit=rmng-clone-daemon --uid="$USER_NAME" $ENVS \
  --setenv=RMNG_SOCKET="$T/clones.sock" --setenv=RMNG_CLONE_ID=testclone \
  --setenv=RMNG_MONITORS=1920x1080 --setenv=RMNG_DAEMON_MCP_PORT=9004 --setenv=RUST_LOG=info,clone_daemon::clipboard=debug \
  --property=Restart=always \
  "$BIN/rmng-clone-daemon" >/dev/null

echo "== wait for video :9001 + MCP :9004 (up to 40s) =="
for i in $(seq 1 40); do
  vid=$(ss -ltn 2>/dev/null | grep -cE ":9001\b")
  mcp=$(curl -fsS -m 2 -X POST http://127.0.0.1:9004/ -H 'content-type: application/json' -d '{"jsonrpc":"2.0","id":1,"method":"ping"}' >/dev/null 2>&1 && echo 1 || echo 0)
  if [ "$vid" -ge 1 ] && [ "$mcp" = 1 ]; then echo "UP"; break; fi
  sleep 1
done

echo "== status =="
systemctl --no-pager --no-legend status rmng-control-server rmng-clone-daemon 2>/dev/null | grep -E "●|Active:|Main PID:" | head -8
echo "-- listening --"; ss -ltn 2>/dev/null | grep -oE ":(9000|9001|9002|9003|9004)\b" | sort -u | tr '\n' ' '; echo
echo "-- clone connected? --"; journalctl -u rmng-control-server --no-pager -n 30 2>/dev/null | grep -iE "clone-daemon '.*' connected|video.*listening|MCP" | tail -3
echo
echo "logs:  journalctl -u rmng-control-server -f   |   journalctl -u rmng-clone-daemon -f"
echo "stop:  systemctl stop rmng-control-server rmng-clone-daemon"
