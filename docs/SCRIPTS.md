# Scripts reference

The Docker port collapsed RMNG's script surface to almost nothing. The old
**developer build/deploy** scripts (`provision-build-ct.sh` / `cs-build-ct.sh` /
`provision-deploy-ct.sh` / `cs-deploy-ct.sh`) are gone — the build is a **Dockerfile**
(`docker build`, see [DEPLOY.md](DEPLOY.md#the-image-build)), and deploy is `docker run` /
`docker compose`. The old **SSH+`pct` orchestration** scripts (`bootstrap.sh` / `clone.sh` /
`redeploy.sh` / `delete.sh`) are gone too — those flows are now pure Rust in
[`provision.rs`](../crates/control-server/src/provision.rs), driving the bollard primitives in
[`docker.rs`](../crates/control-server/src/docker.rs). The `P <step> <msg>` / `RESULT` bash
protocol died with them: Rust emits progress directly through a `FnMut(&str, &str)` callback.
Clone binaries also no longer deploy via a script + endpoint — the control-server installs its current
payloads into each clone at create time, before boot (a tar upload straight from Rust; see
[DEPLOY.md#upgrades](DEPLOY.md#upgrades)).

The in-product clone-source **build** is gone too: what used to be
`crates/control-server/scripts/provision-clone.sh`, run inside a privileged build container
over `docker exec`, is now [`template/setup/`](../template/setup/) — ordered phase scripts
`RUN` directly by [`template/Dockerfile`](../template/Dockerfile) at `docker build` time,
published as an image (`pegasis0/rmng-template`) instead of provisioned per install. See
[DEPLOY.md#publishing-the-template](DEPLOY.md#publishing-the-template).

There are **no in-container guest scripts** left. The account-token flows that used to run
`claude-import.sh` / `codex-import.sh` over `docker exec` are plain Rust now (`claude.rs`,
`codex.rs`), and the directory that held them (`crates/control-server/scripts/`) is gone. What
the control-server still embeds with `include_str!` is documentation and a build script it
reads, not a script it runs in a clone: [`docs/CLI.md`](CLI.md) (shipped into every clone as
the `rmng-cli` skill) and `template/setup/30-user.sh` (a test compares it against what the
server injects).

What is left is the **template build scripts**, the **gnome-patch build** (both
Dockerfile-stage/`RUN` steps) and the **developer scripts** in `scripts/`.

| Script | Runs where | Invoked by | Purpose |
| --- | --- | --- | --- |
| `template/setup/{lib,10-desktop,15-gnome-patch,20-toolbox,30-user}.sh` | in the template build (`RUN`) | `template/Dockerfile` | Provision the clone template rootfs: desktop, patched shell, dev toolbox, the clone user + its units (the RMNG binaries are **not** baked in — the control-server injects them at clone-create time) |
| `template/gnome-patch/build-shell-deb.sh` | the `gnome-build` stage of `template/Dockerfile` | `docker build` | Build the patched gnome-shell `.deb` |
| `scripts/build-macos-app.sh` | a developer's Mac | by hand | Build + bundle the native macOS viewer |
| `scripts/publish-server.sh` | a developer's machine | by hand | Build + push the control-server image |
| `scripts/publish-template.sh` | a developer's machine | by hand | Build + push the clone template image |

The template build scripts are `COPY`'d into the build context and `RUN` by the Dockerfile
itself — they never touch the control-server binary or a live container.

---

## Developer scripts

### `scripts/build-macos-app.sh [OUTPUT_DIR]`

Builds the native macOS viewer ([`crates/viewer-macos`](../crates/viewer-macos/README.md)) in
release and wraps it in `OUTPUT_DIR/RMNG Viewer.app` (default `target/macos`), ad-hoc signing it
so arm64 will launch it. The binary links only system frameworks, so the bundle is
self-contained — no Homebrew, nothing to copy in — and runs on a Mac that has never seen this
repo. Prints the framework/Homebrew link counts as a check. Runs on macOS only.

### `scripts/publish-server.sh [SERVER_REPO]`

Builds the **control-server** image from the root [`Dockerfile`](../Dockerfile) with the repo
root as the build context, then pushes it. Stamps `GIT_SHA` + `BUILD_DATE` build args, so the
running server can show its version and detect updates. Tags twice: an immutable dated
`:YYYYMMDD` and a moving `:latest`, and pushes both. The repo defaults to `pegasis0/rmng`;
override it with the `SERVER_REPO` env var or the first argument. Rollback is repointing the
update reference (`docker.serverImage` in the config) at an older dated tag. See
[DEPLOY.md#upgrades](DEPLOY.md#upgrades).

### `scripts/publish-template.sh [TEMPLATE_REPO]`

Builds the **clone template** image from [`template/Dockerfile`](../template/Dockerfile) — also
with the repo root as the build context, because the final stage copies `template/setup/` and
the `gnome-build` stage payloads out of it — then pushes it. Same two tags as
`publish-server.sh`: dated `:YYYYMMDD` plus `:latest`. The repo defaults to
`pegasis0/rmng-template`; override it with the `TEMPLATE_REPO` env var or the first argument.
Rollback is repointing the template reference at an older dated tag. Preset Dockerfiles take
this image through their `FROM` line, so a new `:latest` reaches a preset only when its image
is rebuilt. See [DEPLOY.md#publishing-the-template](DEPLOY.md#publishing-the-template).

---

## Template build scripts

Ordered phase scripts under [`template/setup/`](../template/setup/), each `COPY`'d in
immediately before its own `RUN` in `template/Dockerfile` (not one bulk copy — see the
Dockerfile's comments on why per-phase copies matter for layer caching). Rarest-changing
first, so a `30-user.sh` tweak never re-runs the ~20-minute phase-10 apt layer. Every phase
sources `lib.sh` first (`DEBIAN_FRONTEND=noninteractive` + `SYSTEMD_OFFLINE=1`, exported
inside the script — never baked as image `ENV`, or it would leak into the booted clone).

| Script | Purpose |
| --- | --- |
| `lib.sh` | Shared env + `log()` helper; sourced (not run) by every phase |
| `10-desktop.sh` | Locale/tz, headless GNOME + Mutter + VA-API + PipeWire (no gdm3/g-r-d/flatpak), the Recommends strip, container masks, the polkit sudo-group rule (DM-less ⇒ no resolvable session) |
| `15-gnome-patch.sh` | `dpkg -i` the patched gnome-shell `.deb` (from the `gnome-build` stage) over stock |
| `20-toolbox.sh` | Best-effort dev toolbox: CLI tools, Docker, cloud CLIs, browsers, Cursor/VS Code, HMCL/Mission Center/Monaspace, dconf defaults |
| `30-user.sh` | The uid-1000 clone user (groups, linger, fish), preset-PATH rc, keyring, shared `CLAUDE.md`, Codex `AGENTS.md`/`config.toml`, Claude+Codex Linear MCP defaults, the `claude`/`codex`/`uv`/`rustup` toolchains, and the three `systemd --user` units (`gnome-headless`, `rmng-clone-daemon`, `agent-wrapper`) + wants symlinks. `claude` and `codex` are standalone installs that need no node, and **nvm is not installed at all** — a clone that needs node gets it from its preset Dockerfile |

`30-user.sh` creates `/opt/rmng/bin` (root:root, 0755) **empty** — the template no longer
carries `clone-daemon`/`agent-wrapper`; the control-server installs its own current copies
(plus the `rmng` CLI at `/usr/local/bin/rmng`) into each clone at create time, before boot
([`provision.rs`](../crates/control-server/src/provision.rs) `CLONE_BINARIES`) and refreshes
them on running managed clones through the clone reconciler after server upgrades. Unlike the
retired `provision-clone.sh`, these scripts never run
inside a live container over `docker exec` — they're plain Dockerfile `RUN` steps executed
once, at `template/Dockerfile` build time; see
[DEPLOY.md#publishing-the-template](DEPLOY.md#publishing-the-template).

---

## template/gnome-patch build

### `template/gnome-patch/build-shell-deb.sh`

Runs in the **`gnome-build` stage of `template/Dockerfile`** (`docker build`). Repack approach:
applies shell-01 + shell-02 to the gnome-shell source, rebuilds only `libshell-<N>.so`
(meson/ninja), swaps it into the stock `.deb`, and bumps the version `+ngshell1`. Prints
`DEB=<path>` — `template/Dockerfile` copies that to `/tmp/gnome-shell.deb` in the final stage,
where `15-gnome-patch.sh` `dpkg -i`s it directly into the template rootfs (it is **not** a
control-server payload — nothing under `/usr/local/share/rmng/` ships it any more). Cached
(skips if the deb is newer than the patches; `FORCE=1` rebuilds). See
[template/gnome-patch/README.md](../template/gnome-patch/README.md).
