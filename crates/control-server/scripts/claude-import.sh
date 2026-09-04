#!/usr/bin/env bash
# Runs INSIDE the target clone container (the control-server streams this over
# `docker exec bash -s`). Executes a Claude credential op as the clone user, printing
# the raw result to stdout. This is the injection path: the server owns every account's
# OAuth pair and writes a short-lived access token into each clone. Reading credentials
# back OUT of a clone is gone; accounts are signed in to at the server (`crate::oauth`).
#
#   claude-import.sh <user> clear|apply [creds_b64] [identity_b64]
#     clear  — delete that credentials file, then print CLEARED
#     apply  — write ~/.claude/.credentials.json from base64 arg $3 (the full JSON,
#              short-lived access token as accessToken, refreshToken empty), print OK.
#              Does NOT restart agent-wrapper — Claude Code re-reads creds at request time.
#              Arg $4 is the account identity, or "-" when the server does not know the
#              account uuid yet. It names the account this clone now runs, and Claude Code
#              sends that name to Anthropic with every request.
set -euo pipefail
USER="${1:-rmng}"; OP="$2"
# Force bash with an explicit PATH rather than the user's login shell: clones default
# to fish, which isn't where `claude` (in ~/.local/bin) is on PATH and which prints
# tty/parse noise. `-l` still gives a login env (HOME=/home/$USER); `-s /bin/bash`
# overrides only which shell interprets the command.
inct() { runuser -l "$USER" -s /bin/bash -c "export PATH=\$HOME/.local/bin:\$PATH; $1"; }

# Merge the account identity into ~/.claude.json.
#
# That file belongs to Claude Code and holds about fifty other keys, so this replaces three
# of them and leaves the rest alone. A file that does not parse is left untouched: losing a
# clone's project history to repair its identity is the worse trade.
#
# Python rather than jq, because the read, the compare and the atomic rename are one program
# here and three pipelines there.
write_identity() {
  local prog
  prog="$(mktemp /tmp/rmng-identity-XXXXXX.py)"
  cat >"$prog" <<'PYEOF'
import base64, json, os, sys

patch = json.loads(base64.b64decode(sys.argv[1]))
path = os.path.join(os.path.expanduser('~'), '.claude.json')
cur = {}
mode = 0o600
if os.path.exists(path):
    mode = os.stat(path).st_mode & 0o777
    with open(path) as f:
        raw = f.read()
    if raw.strip():
        try:
            cur = json.loads(raw)
        except ValueError:
            print('RMNG_IDENTITY_FAILED: ~/.claude.json is not readable JSON')
            sys.exit(0)
if not isinstance(cur, dict):
    print('RMNG_IDENTITY_FAILED: ~/.claude.json is not a JSON object')
    sys.exit(0)

# Snapshot before anything is touched, and build the new block as a copy. Editing the
# existing block in place would make it identical to its own snapshot, and the write that
# repairs the file would be skipped as unnecessary.
before = json.dumps(cur, sort_keys=True)

want = patch['oauthAccount']
have = cur.get('oauthAccount')
if isinstance(have, dict) and have.get('accountUuid') == want['accountUuid']:
    # The same account, so its billing, seat and rate-limit fields still describe it.
    block = dict(have)
    block.update(want)
    if block != have:
        # Something we own moved, so the rest of the block is a profile of the old state.
        # Claude Code refills it the next time it looks the account up.
        block.pop('profileFetchedAt', None)
else:
    # A different account. Nothing the old block said carries over.
    block = dict(want)
have = block

cur['userID'] = patch['userID']
cur['machineID'] = patch['machineID']
cur['oauthAccount'] = have
if json.dumps(cur, sort_keys=True) == before:
    print('RMNG_IDENTITY_CURRENT')
    sys.exit(0)

# Rename, so a Claude Code that reads the file mid-write sees one version or the other.
tmp = path + '.rmng.tmp'
with open(tmp, 'w') as f:
    json.dump(cur, f)
os.chmod(tmp, mode)
os.replace(tmp, path)
print('RMNG_IDENTITY_WRITTEN')
PYEOF
  chmod 644 "$prog"
  inct "python3 '$prog' '$1'" || echo "RMNG_IDENTITY_FAILED: the merge did not run" >&2
  rm -f "$prog"
}

case "$OP" in
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
  apply)
    B64="$3"; IDB64="${4:--}"
    inct "set -e; umask 077; mkdir -p \"\$HOME/.claude\"; printf %s '$B64' | base64 -d > \"\$HOME/.claude/.credentials.json.tmp\"; chmod 600 \"\$HOME/.claude/.credentials.json.tmp\"; mv -f \"\$HOME/.claude/.credentials.json.tmp\" \"\$HOME/.claude/.credentials.json\"; echo RMNG_APPLY_OK"
    # The identity is delivered after the token and reported separately. A clone that took
    # the token and refused the identity still runs, so this never fails the push.
    if [ "$IDB64" != "-" ]; then write_identity "$IDB64"; fi
    ;;
  *)      echo "unknown op: $OP" >&2; exit 2 ;;
esac
