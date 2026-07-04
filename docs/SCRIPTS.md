# Scripts reference

The Docker port collapsed RMNG's script surface to almost nothing. The old
**developer build/deploy** scripts (`provision-build-ct.sh` / `cs-build-ct.sh` /
`provision-deploy-ct.sh` / `cs-deploy-ct.sh`) are gone — the build is a **Dockerfile**
(`docker build`, see [DEPLOY.md](DEPLOY.md#the-image-build)), and deploy is `docker run` /
`docker compose`. The old **SSH+`pct` orchestration** scripts (`bootstrap.sh` / `clone.sh` /
`redeploy.sh` / `delete.sh`) are gone too — those flows are now pure Rust in
[`provision.rs`](../crates/control-server/src/provision.rs), driving the bollard primitives in
[`docker.rs`](../crates/control-server/src/docker.rs). The `P <step> <msg>` / `RESULT` bash
protocol died with them: Rust emits progress directly through a `FnMut(&str, &str)` callback,
and a guest script's own stdout lines are line-buffered into the Operation log. Clone binaries
also no longer redeploy via a script + endpoint — the control-server hot-swaps them itself
(hash check + `systemctl --user` bounce driven straight from Rust; see
[DEPLOY.md#upgrades](DEPLOY.md#upgrades)).

The in-product clone-source **build** is gone too: what used to be
`crates/control-server/scripts/provision-clone.sh`, run inside a privileged build container
over `docker exec`, is now [`template/setup/`](../template/setup/) — ordered phase scripts
`RUN` directly by [`template/Dockerfile`](../template/Dockerfile) at `docker build` time,
published as an image (`pegasis0/rmng-template`) instead of provisioned per install. See
[DEPLOY.md#publishing-the-template](DEPLOY.md#publishing-the-template).

What survives at **runtime** is **two in-container guest scripts** (`include_str!`'d into the
control-server binary and streamed to a container over `docker exec bash -s` —
`DockerCtl::exec_script`), plus the **template build scripts** and the **gnome-patch build**
(both Dockerfile-stage/`RUN` steps, not `docker exec`).

| Script | Runs where | Invoked by | Purpose |
|---|---|---|---|
| `crates/control-server/scripts/claude-import.sh` | in a clone container (`docker exec`) | `provision::run_clone_op` (`claude.rs`) | Read `claude auth status` / the credentials file, clear it, or install a token |
| `crates/control-server/scripts/codex-import.sh` | in a clone container (`docker exec`) | `clone_ops::run_clone_op` (`codex.rs`) | Read `~/.codex/auth.json` status / the auth file, clear it, or install a token |
| `template/setup/{lib,10-desktop,15-gnome-patch,20-toolbox,30-user}.sh` | in the template build (`RUN`) | `template/Dockerfile` | Provision the clone template rootfs: desktop, patched shell, dev toolbox, the clone user + its units (binaries themselves are `COPY`'d in by the Dockerfile after) |
| `gnome-patch/build-shell-deb.sh` | the `gnome-build` stage of `template/Dockerfile` | `docker build` | Build the patched gnome-shell `.deb` |

The two runtime guest scripts are baked in at compile time
([provision.rs:32-33](../crates/control-server/src/provision.rs)) and fed to
`bash -s -- <args…>` over the exec's stdin at runtime — they are **not** pre-installed in any
container. Each emits its step lines as `    [ct] <message>`, which `provision.rs` strips for
the operation message; other stdout/stderr becomes plain log context. The template build
scripts, by contrast, are `COPY`'d into the build context and `RUN` by the Dockerfile itself —
they never touch the control-server binary or a live container.

---

## In-container guest scripts

### `claude-import.sh <user> status|read|clear|apply [b64]`
Runs inside the target **clone** container as the clone user, printing the raw result to
stdout. `status` — `claude auth status` JSON (stderr merged so a logged-out clone still
parses; never fails). `read` — the clone's `~/.claude/.credentials.json`. `clear` — delete
it, print `CLEARED`. `apply <b64>` — write `~/.claude/.credentials.json` (0600) from the
base64 JSON in `$3` (the current short-lived access token, refresh emptied). Backs
`claude.rs`'s `{check_clone_auth, import_clone_account, apply_clone_token}`; hot-swaps a
running clone's account with no restart (Claude Code re-reads creds per request).

### `codex-import.sh <user> status|read|clear|apply [b64]`
Mirrors `claude-import.sh` for the Codex CLI. Runs inside the target **clone** container
as the clone user. `status` — decode `~/.codex/auth.json` and print identity (email, plan,
account_id) from the `id_token` JWT; exits non-zero if no token or if only an API key is
present. `read` — the clone's `~/.codex/auth.json`. `clear` — delete it, print `CLEARED`.
`apply <b64>` — write `~/.codex/auth.json` (0600) from the base64 JSON in `$3` (the
injected token with `OPENAI_API_KEY: null`, `refresh_token: ""`, `last_refresh: <now>`).
Backs `codex.rs`'s `{check_clone_auth, import_clone_account, apply_clone_token}`; hot-swaps
a running clone's Codex account with no restart (the Codex CLI re-reads auth per request).

> **Provisioning note:** the `codex` CLI is installed into the clone template by
> `template/setup/30-user.sh` (warn-only — the install step does not fail the build if the
> CLI is unavailable). Existing clone images built before this change **do not** have the
> Codex CLI and need a template rebuild (`docker build`) followed by `POST /api/images/pull`
> to pull the updated template. Hot-swapping the binswap (`clone-daemon` / `agent-wrapper`)
> does **not** install CLIs — it only replaces the two RMNG binaries.

---

## Template build scripts

Ordered phase scripts under [`template/setup/`](../template/setup/), each `COPY`'d in
immediately before its own `RUN` in `template/Dockerfile` (not one bulk copy — see the
Dockerfile's comments on why per-phase copies matter for layer caching). Rarest-changing
first, so a `30-user.sh` tweak never re-runs the ~20-minute phase-10 apt layer. Every phase
sources `lib.sh` first (`DEBIAN_FRONTEND=noninteractive` + `SYSTEMD_OFFLINE=1`, exported
inside the script — never baked as image `ENV`, or it would leak into the booted clone).

| Script | Purpose |
|---|---|
| `lib.sh` | Shared env + `log()` helper; sourced (not run) by every phase |
| `10-desktop.sh` | Locale/tz, headless GNOME + Mutter + VA-API + PipeWire (no gdm3/g-r-d/flatpak), the Recommends strip, container masks |
| `15-gnome-patch.sh` | `dpkg -i` the patched gnome-shell `.deb` (from the `gnome-build` stage) over stock |
| `20-toolbox.sh` | Best-effort dev toolbox: CLI tools, Docker, cloud CLIs, browsers, Cursor/VS Code, HMCL/Mission Center/Monaspace, dconf defaults |
| `30-user.sh` | The uid-1000 clone user (groups, linger, fish), preset-PATH rc, keyring, shared `CLAUDE.md` + linear MCP, `claude`/`uv`/`rustup`/`nvm` toolchains, and the three `systemd --user` units (`gnome-headless`, `rmng-clone-daemon`, `agent-wrapper`) + wants symlinks |

`30-user.sh` creates `/opt/rmng/bin` (root:root, 0755); `template/Dockerfile` then `COPY
--from`s the built `clone-daemon` + `agent-wrapper` straight into it as the last two layers —
the same destination [`redeploy_clone`](../crates/control-server/src/provision.rs) hot-swaps
into later (`REDEPLOY_UNITS`). Unlike the retired `provision-clone.sh`, these scripts never run
inside a live container over `docker exec` — they're plain Dockerfile `RUN` steps executed
once, at `template/Dockerfile` build time; see
[DEPLOY.md#publishing-the-template](DEPLOY.md#publishing-the-template).

---

## gnome-patch build

### `gnome-patch/build-shell-deb.sh`
Runs in the **`gnome-build` stage of `template/Dockerfile`** (`docker build`). Repack approach:
applies shell-01 + shell-03 to the gnome-shell source, rebuilds only `libshell-<N>.so`
(meson/ninja), swaps it into the stock `.deb`, and bumps the version `+ngshell1`. Prints
`DEB=<path>` — `template/Dockerfile` copies that to `/tmp/gnome-shell.deb` in the final stage,
where `15-gnome-patch.sh` `dpkg -i`s it directly into the template rootfs (it is **not** a
control-server payload — nothing under `/usr/local/share/rmng/` ships it any more). Cached
(skips if the deb is newer than the patches; `FORCE=1` rebuilds). See
[gnome-patch/README.md](../gnome-patch/README.md).
