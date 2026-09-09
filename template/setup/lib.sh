#!/usr/bin/env bash
# Shared env + helpers for the template provisioning phase scripts (10 / 15 / 20 / 30).
# SOURCED (not executed) as the first line of every phase script — so these exports land
# at the top of each script, per the "env inside the scripts, never image ENV" rule:
#
#   * DEBIAN_FRONTEND=noninteractive — apt must never block on a prompt during the build.
#   * SYSTEMD_OFFLINE=1 — the `systemctl mask` / `set-default` calls in phase 10 are pure
#     symlink ops: systemd is NOT PID 1 during `docker build`, so systemctl must run in
#     offline mode (no bus to reach). This is deliberately NOT baked as image ENV — it
#     would otherwise leak into the booted system and confuse the real PID-1 systemd.
export DEBIAN_FRONTEND=noninteractive
export SYSTEMD_OFFLINE=1

# Plain build-log helper. The exec-era `[ct]` progress protocol (the control-server parsed
# `    [ct] <msg>` lines out of `docker exec`) is gone — this is a straight `docker build`,
# so a step line is just a build-log line.
log()  { echo "  >> $*"; }

# Strict-build rule: every phase script runs under `set -euo pipefail` and NOTHING swallows
# a failure — no `warn`, no `|| true` on real steps. A failed install fails the template
# build here, not surfaces later as a degraded clone. The only tolerated fallbacks are
# absence checks that branch on them explicitly (e.g. `if id ubuntu`, `command -v` gates
# with a hard failure on the empty path).
