# RMNG — Architecture & Development

Architecture, the port/protocol map, the workspace layout, and the build prerequisites.
For running the hub, see the [Quick start](../README.md#quick-start); for the full build →
run → wizard → upgrade flow, see [DEPLOY.md](DEPLOY.md).

## The shape

The control-server exposes web/API (with the browser viewer), SSH, and SMB surfaces; the desktop automation MCP lives inside each clone.

| Port | Default | Transport | Purpose |
| --- | --- | --- | --- |
| **web API** | `9000` | HTTP + SSE + WebSocket (+ embedded frontend) | the React web app at `/`: the selected clone's screen (WebSocket `/api/viewer`: the selected clone's monitors as H.264, with input + clipboard + cursor back) and the control panel (clone selection, clone/Linear/Claude/chat orchestration, settings); also the `rmng desktop`/`rmng exec` proxy endpoints |
| SSH bastion | `2222` | SSH (sshd) | jump host into clones (`ssh -J`); also carries port forwarding (`ssh -L`), which the web app's "Port forward" dialog writes out as a command |
| **SMB** | `445` | SMB (smbd) | two shares, fixed cred `rmng`/`rmng`: `clones` browses every running clone's `/home/rmng`, and `shared` is the one pool mounted into every clone at `~/shared` |
| daemon MCP | `9004` | HTTP JSON-RPC (in each clone) | the full desktop-automation surface; the agent calls it on localhost, the `rmng desktop` CLI proxies to it (operator/fleet desktop control) |
| clone socket | `/srv/rmng-sock/clones.sock` | unix `SOCK_SEQPACKET` | clone-daemon ⇄ control-server: dmabuf frames (`SCM_RIGHTS`) out, input/clipboard in |

**Design in one breath:** one central encoder, thin clones (no g-r-d / GDM / RDP — just
`gnome-session` + the daemon); one capture feeds both the human viewer and the agent's
screenshots; raw H.264 over a WebSocket into the browser's WebCodecs decoder, with no RDP and
no WebRTC; media/input cross a host unix socket, not the network, so only the control-server is externally
reachable. `docker run` the control-server container, open the browser, and the first-run
setup wizard pulls the pre-built clone **template** — a separate published image
(`pegasis0/rmng-template`) that already carries the patched gnome-shell — and finishes setup.
Clone binaries (and the `rmng` CLI) are injected into each clone at create time from the
running server's payloads; there's no manual redeploy step.

## Documentation

| Doc | Covers |
| --- | --- |
| [API.md](API.md) | Every HTTP endpoint on the web port (9000), incl. the SSE streams |
| [MCP.md](MCP.md) | Clone-local daemon MCP on 9004: JSON-RPC envelope, tools, and curl examples |
| [CLI.md](CLI.md) | The `rmng` CLI (`/usr/local/bin/rmng` in every clone): every subcommand incl. `desktop`/`exec`, `--json`, exit codes, wait semantics |
| [PROTOCOL.md](PROTOCOL.md) | The browser viewer's WebSocket protocol (video/input/clipboard/cursor), the clone socket, the config schema, every env var, the clone-daemon CLI, and the per-crate public API |
| [SCRIPTS.md](SCRIPTS.md) | Every script: what it does, where it runs, its args, and what invokes it |
| [DEPLOY.md](DEPLOY.md) | The Docker build → run → wizard → clones flow, the image build, upgrades, clone-home browsing, and the dev loop |
| [PROXMOX-LXC.md](PROXMOX-LXC.md) | Running the Docker host on a **privileged** Proxmox LXC CT (one hosting option; privileged is a hard requirement since gen-2, and it is a one-way trip) |
| [GEN2-CLONES.md](GEN2-CLONES.md) | The gen-2 design reference: ZFS home datasets, the home overlay, preset images, and the create/fork/rebase/delete flows. Read this before changing anything about clone storage |
| [CLONE-LAYERS.md](CLONE-LAYERS.md) | Where every byte of a clone comes from: baked into the image, injected before boot, injected after boot, and what converges later |
| [RUNBOOK-GEN1-TO-GEN2.md](archive/RUNBOOK-GEN1-TO-GEN2.md) (archived) | The record of the gen-1 → gen-2 migration of the three production containers, including the still-armed rollback (§9) |
| [RUNBOOK-CT101-TESTING.md](RUNBOOK-CT101-TESTING.md) | Testing a change on CT 101, the disposable test deployment — and the rule that production is never a deploy target |

## Workspace map

| Path | Kind | What |
| --- | --- | --- |
| [crates/wire](../crates/wire/README.md) | lib | shared types: control state, config, the clone socket + browser viewer protocols, MCP DTOs; ts-rs export for the frontend |
| [crates/control-server](../crates/control-server/README.md) | bin | the server: media plane, web API/SSE, passive token accounting, Docker lifecycle, the browser viewer's WebSocket, the SMB shares, and clone payloads |
| [crates/media](../crates/media/README.md) | lib | dmabuf ingest → VA-API H.264 per monitor + dmabuf→JPEG screenshots + the clone-socket transport |
| [crates/clone-daemon](../crates/clone-daemon/README.md) | bin | the thin in-clone pipe: RecordVirtual capture, RemoteDesktop input injection, clipboard bridge, and desktop MCP (:9004) |
| [crates/control-client](../crates/control-client/README.md) | lib | typed reqwest+SSE client for the web API (port 9000) (`/api/state`, `/events`, clone/delete/image/account wrappers); used by the `rmng` CLI and integration tests; also builds `rmng-fps`, the viewer frame-rate test tool |
| [crates/cli](../crates/cli/README.md) | bin | the `rmng` fleet CLI: clones/images/accounts/operations over the web API (port 9000); injected into every clone as `/usr/local/bin/rmng` |
| [frontend](../frontend/README.md) | web app | React Router 7 web app (the clone's screen and the control panel), ts-rs types from `wire`, served by the control-server |
| [template/gnome-patch](../template/gnome-patch/README.md) | tooling | builds the patched gnome-shell `.deb` (hide screen-share indicator + enable `Eval` for window-mgmt); built + installed by `template/Dockerfile`'s `gnome-build` stage into the published clone template — not a control-server payload |

The web UI's per-clone chat panel talks to an outside **assistant** (a pi-web server, set in
Settings → Presets → Assistant), not to a process inside the clone. The assistant drives the clone
remotely with the `rmng` CLI; its default playbook is
[crates/wire/src/agent-playbook.md](../crates/wire/src/agent-playbook.md). See
[API.md](API.md#per-clone-assistant-chat).

<a id="clean-room"></a>

## Clean-room

`RMNG` is its own Cargo workspace (own lockfile, edition 2024). It does **not** import the
old client (`../core`, `../gtk`, `../headless`), the old `../control-server`, or
`../computer-use` — those are reference material for proven techniques, re-expressed fresh. The
one preserved contract is the JSON wire format of `/events` and the web API, so the React
frontend works unchanged.

<a id="prerequisites"></a>

## Prerequisites

Rust (edition 2024), `bun`, `clang`/`libclang`; `libpipewire-0.3-dev`, `libva-dev` + AMD VA-API
(radeonsi/Mesa), `libdrm-dev`, GStreamer + **`gstreamer1.0-plugins-bad`** (the `va` elements —
*not* `gstreamer1.0-va`); a GPU render node (`/dev/dri/renderD128`) on the control-server host
*and* every clone. With those dev libs the **whole workspace compiles on a plain Linux laptop**
(a bare box without them builds only `wire`); the GPU box is only needed to *run* the
capture/encode/server side. The viewer is a page in the frontend and runs in Chrome; it needs
HTTPS (or `localhost`) for WebCodecs and the clipboard. See the
[dev loop](DEPLOY.md#the-dev-loop). **The clone template is built on the `ubuntu:26.04` base OS**
(the patched gnome-shell is compiled against 26.04's GNOME only) — see
[Publishing the template](DEPLOY.md#publishing-the-template).

The capture/encode/server side is Linux-only. On macOS or Windows, build only the `rmng` CLI:
`cargo build -p rmng-cli -p control-client` (`clone-daemon` depends on pipewire, and the
workspace declares no `default-members`). The package is `rmng-cli`, not `cli`, which is only
the directory name.
