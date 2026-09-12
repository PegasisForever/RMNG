#!/usr/bin/env bash
# Shared by all template phase scripts. SOURCED, not executed. Env stays here, never
# image ENV (SYSTEMD_OFFLINE=1 would confuse the booted PID-1 systemd).
export DEBIAN_FRONTEND=noninteractive
export SYSTEMD_OFFLINE=1

# set -E so the ERR trap fires inside functions too — else a strict build dies unnamed.
set -E

log()  { echo "  >> $*"; }

# Names the exact failing command + line under set -e. Phases enable it after sourcing.
report_err() { echo "  !! FAILED at line $1: $2" >&2; }
enable_err_trap() { trap 'report_err "$LINENO" "$BASH_COMMAND"' ERR; }
