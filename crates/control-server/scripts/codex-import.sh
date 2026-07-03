#!/usr/bin/env bash
# Runs INSIDE the target clone container (the control-server streams this over
# `docker exec bash -s`). Executes a Codex credential op as the clone user, printing
# the raw result to stdout. Sibling of claude-import.sh; targets ~/.codex/auth.json.
#
#   codex-import.sh <user> status|read|clear|apply [b64]
#     status — contents of ~/.codex/auth.json, or `{}` if absent (never fails the script;
#              codex has no clean JSON `login status`, so identity is read from the file)
#     read   — contents of ~/.codex/auth.json (fails if absent)
#     clear  — delete that auth file, then print CLEARED
#     apply  — write ~/.codex/auth.json from base64 arg $3 (the full JSON: real access +
#              id token + account_id, refresh_token empty, last_refresh now), print OK.
#              Does NOT restart anything — codex re-reads auth.json per invocation.
set -euo pipefail
USER="${1:-rmng}"; OP="$2"
# Force bash with an explicit PATH rather than the user's login shell (clones default to
# fish, which prints tty/parse noise). Mirrors claude-import.sh exactly.
inct() { runuser -l "$USER" -s /bin/bash -c "export PATH=\$HOME/.local/bin:\$PATH; $1"; }
case "$OP" in
  status) inct 'cat "$HOME/.codex/auth.json" 2>/dev/null || echo "{}"' ;;
  read)   inct 'cat "$HOME/.codex/auth.json"' ;;
  clear)  inct 'rm -f "$HOME/.codex/auth.json"'; echo CLEARED ;;
  apply)  B64="$3"; inct "umask 077; mkdir -p \"\$HOME/.codex\"; echo '$B64' | base64 -d > \"\$HOME/.codex/auth.json\"; chmod 600 \"\$HOME/.codex/auth.json\"; echo OK" ;;
  *)      echo "unknown op: $OP" >&2; exit 2 ;;
esac
