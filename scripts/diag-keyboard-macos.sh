#!/usr/bin/env bash
# Keyboard modifier diagnostic capture for rmng-viewer (macOS).
#
# Launches the release viewer with debug logging so the FlagsChanged handler prints
# exactly what macOS delivers for each modifier key. Walks you through a fixed keystroke
# sequence, then saves a log Claude can read directly to confirm the modifier fix.
#
# Usage (from the repo root):  ./scripts/diag-keyboard-macos.sh [viewer args...]
set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "$SCRIPT_DIR/.."

BIN="./target/release/rmng-viewer"
LOG="/tmp/rmng-kb-$(date +%Y%m%d-%H%M%S).log"

if [[ ! -x "$BIN" ]]; then
  echo "error: $BIN not found — build it first:  cargo build --release -p viewer" >&2
  exit 1
fi

echo "Using binary: $BIN  ($(date -r "$BIN" '+%Y-%m-%d %H:%M'))"
cat <<'INSTRUCTIONS'
============================================================
  rmng-viewer keyboard diagnostic
============================================================
After you press ENTER the viewer launches with debug logging.

  1. Connect to your remote as usual.
  2. CLICK INTO the remote video window so it has focus — the
     key handler only runs while that window is focused.
  3. Then do the following SLOWLY, ~1 second between each:

       a. Tap Left Control       (press, hold 1s, release)
       b. Tap Left Command  (⌘)  (press, hold 1s, release)
       c. Tap Left Shift         (press, hold 1s, release)
       d. Tap Left Option   (⌥)  (press, hold 1s, release)
       e. Type once each:  Tab , Space , Enter
       f. Bug repro: hold ⌘, press Tab to switch AWAY from the
          viewer, Cmd+Tab BACK to it, then release ⌘.
       g. Tap Left Control once more, then try Tab / Space /
          Enter again — do they type now?

  4. QUIT the viewer (Cmd+Q or close the window) to finish.
============================================================
INSTRUCTIONS

read -r -p "Press ENTER to launch and start capturing... " _

echo "Capturing to $LOG ..."
RUST_LOG="info,rmng_viewer=debug,viewer=debug" "$BIN" "$@" >"$LOG" 2>&1 || true

echo
echo "Viewer exited. Modifier events captured:"
echo "------------------------------------------------------------"
grep -nE "flags:|key down:|key up:|monitor-missed" "$LOG" \
  || echo "(none — the remote window may not have been focused, or the monitor did not install)"
echo "------------------------------------------------------------"
echo "Full log: $LOG"
echo "Tell Claude the path above — it will read and analyze it."
