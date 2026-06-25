#!/usr/bin/env bash
# Install a long-lived Claude token into ~/.claude/.credentials.json so Claude Code
# runs under it. The refreshToken is EMPTY on purpose — the long-lived token must
# never be rotated/replaced by the SDK. Because Claude Code reads this file at
# request time, writing it hot-swaps a *running* clone (no shell re-login).
#
# Arg: $1 = long-lived sk-ant-oat01-… token. Emits "OK" on success.
set -euo pipefail
TOK="$1"
mkdir -p "$HOME/.claude"
umask 077
# printf %s is injection-safe (no shell/heredoc expansion of the token).
printf '{"claudeAiOauth":{"accessToken":"%s","refreshToken":"","expiresAt":4102444800000,"scopes":["user:inference","user:profile"],"subscriptionType":"max"}}\n' \
  "$TOK" > "$HOME/.claude/.credentials.json"
chmod 600 "$HOME/.claude/.credentials.json"
# Best-effort: nudge the agent-wrapper to re-read credentials.
systemctl --user restart agent-wrapper 2>/dev/null \
  || pkill -HUP -f agent-wrapper 2>/dev/null \
  || true
echo OK
