#!/usr/bin/env bash
# Runs INSIDE the target clone container (the control-server streams this over
# `docker exec bash -s`). Executes a Claude credential op as the clone user, printing
# the raw result to stdout. Used by the control-server's token-import flow.
#
#   claude-import.sh <user> status|read|clear|apply [b64]
#     status — `claude auth status` JSON (stderr merged so a missing/logged-out
#              clone still produces a parseable message; never fails the script)
#     read   — contents of the clone's ~/.claude/.credentials.json (fails if absent)
#     clear  — delete that credentials file, then print CLEARED
#     apply  — write ~/.claude/.credentials.json from base64 arg $3 (the full JSON,
#              short-lived access token as accessToken, refreshToken empty), print OK.
#              Does NOT restart agent-wrapper — Claude Code re-reads creds at request time.
set -euo pipefail
USER="${1:-rmng}"; OP="$2"
# Force bash with an explicit PATH rather than the user's login shell: clones default
# to fish, which isn't where `claude` (in ~/.local/bin) is on PATH and which prints
# tty/parse noise. `-l` still gives a login env (HOME=/home/$USER); `-s /bin/bash`
# overrides only which shell interprets the command.
inct() { runuser -l "$USER" -s /bin/bash -c "export PATH=\$HOME/.local/bin:\$PATH; $1"; }
case "$OP" in
  status) inct 'claude auth status' 2>&1 || true ;;
  read)   inct 'cat "$HOME/.claude/.credentials.json"' ;;
  clear)  inct 'rm -f "$HOME/.claude/.credentials.json"'; echo CLEARED ;;
  # `set -e` INSIDE the inner shell: `runuser -c` starts a fresh bash that does not inherit
  # this script's `set -euo pipefail`, so without it the command's exit status is `echo`'s
  # and a failed decode or a failed write still reported success. The server would then
  # record the push as delivered and never revisit the clone.
  #
  # Decode to a temp file and rename, so a partial write cannot leave the clone with a
  # truncated credentials file — the redirect truncates before `base64` has produced a byte.
  #
  # `RMNG_APPLY_OK`, not `OK`: the caller matches a substring against stdout and stderr
  # merged, and plenty of ordinary output contains "OK".
  apply)  B64="$3"; inct "set -e; umask 077; mkdir -p \"\$HOME/.claude\"; printf %s '$B64' | base64 -d > \"\$HOME/.claude/.credentials.json.tmp\"; chmod 600 \"\$HOME/.claude/.credentials.json.tmp\"; mv -f \"\$HOME/.claude/.credentials.json.tmp\" \"\$HOME/.claude/.credentials.json\"; echo RMNG_APPLY_OK" ;;
  *)      echo "unknown op: $OP" >&2; exit 2 ;;
esac
