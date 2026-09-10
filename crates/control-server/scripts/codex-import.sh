#!/usr/bin/env bash
# Runs INSIDE the target clone container (the control-server streams this over
# `docker exec bash -s`). Executes a Codex credential op as the clone user, printing
# the raw result to stdout. Sibling of claude-import.sh, targeting ~/.codex/auth.json.
# Injection only: accounts are signed in to at the server (`crate::oauth`), never read
# back out of a clone.
#
#   codex-import.sh <user> clear|apply [b64] [pi_b64]
#     clear  — delete both auth files, then print CLEARED
#     apply  — write ~/.codex/auth.json from base64 arg $3 (the full JSON: real access +
#              id token + account_id, refresh_token empty, last_refresh now), and MERGE
#              base64 arg $4 (just the openai-codex fragment in pi's provider-keyed shape)
#              into ~/.pi/agent/auth.json, preserving the operator's other providers.
#              Overwriting that file wholesale wiped them (seen live). Then print OK.
#              Does NOT restart anything — codex, pi, and the agent-wrapper all re-read
#              their auth file per invocation.
#
# The second file exists so a `pi` the operator installs starts logged in. pi reads only its
# own auth.json and never looks at ~/.codex. The agent-wrapper does not read it either; it
# bridges ~/.codex/auth.json through its own CredentialStore.
set -euo pipefail
USER="${1:-rmng}"; OP="$2"
# Force bash with an explicit PATH rather than the user's login shell (clones default to
# fish, which prints tty/parse noise). Mirrors claude-import.sh exactly.
inct() { runuser -l "$USER" -s /bin/bash -c "export PATH=\$HOME/.local/bin:\$PATH; $1"; }
case "$OP" in
  clear)  inct 'rm -f "$HOME/.codex/auth.json" "$HOME/.pi/agent/auth.json"'; echo CLEARED ;;
  # See claude-import.sh for why `set -e` has to be repeated inside the inner shell, why the
  # decode goes through a temp file, and why the marker is not just "OK".
  # Both files land before the marker prints, so a caller that sees RMNG_APPLY_OK knows the
  # pair is in sync. pi's dir is created here too: the template only carries it on images
  # built after the pi swap, and the reconciler's prepare step may not have run yet.
  apply)  B64="$3"; PI_B64="${4:-}"; inct "set -e; umask 077; mkdir -p \"\$HOME/.codex\"; printf %s '$B64' | base64 -d > \"\$HOME/.codex/auth.json.tmp\"; chmod 600 \"\$HOME/.codex/auth.json.tmp\"; mv -f \"\$HOME/.codex/auth.json.tmp\" \"\$HOME/.codex/auth.json\"; if [ -n '$PI_B64' ]; then mkdir -p \"\$HOME/.pi/agent\"; printf %s '$PI_B64' | base64 -d > \"\$HOME/.pi/agent/.openai-codex.new\"; chmod 600 \"\$HOME/.pi/agent/.openai-codex.new\"; if jq -e . \"\$HOME/.pi/agent/auth.json\" >/dev/null 2>&1; then jq -s '.[0] * .[1]' \"\$HOME/.pi/agent/auth.json\" \"\$HOME/.pi/agent/.openai-codex.new\" > \"\$HOME/.pi/agent/auth.json.tmp\"; else cat \"\$HOME/.pi/agent/.openai-codex.new\" > \"\$HOME/.pi/agent/auth.json.tmp\"; fi; chmod 600 \"\$HOME/.pi/agent/auth.json.tmp\"; mv -f \"\$HOME/.pi/agent/auth.json.tmp\" \"\$HOME/.pi/agent/auth.json\"; rm -f \"\$HOME/.pi/agent/.openai-codex.new\"; fi; echo RMNG_APPLY_OK" ;;
  *)      echo "unknown op: $OP" >&2; exit 2 ;;
esac
