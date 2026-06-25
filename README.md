# RMNG

> **Hardware-accelerated, fleet-scale cloud desktops for the agentic era.**

![RMNG — a cloud GNOME desktop streamed to a native multi-monitor viewer](docs/hero.webp)

A self-contained Rust system for running, viewing, and automating a fleet of cloud GNOME
desktops. One **control-server** binary is the control plane, the media hub, and the fleet
gateway: it orchestrates Proxmox **clones**, ingests each clone's GPU frames and
hardware-encodes the selected one to a **native hardware-decode GTK viewer**, and brokers the
desktop-automation **MCP** that per-clone Claude agents drive. Each clone runs a thin
**clone-daemon** that captures dmabufs, injects input, bridges the clipboard, and serves its
desktop-automation MCP.

It replaces an older split where a native RDP client connected directly to each clone's
`gnome-remote-desktop`, and each clone ran a local `computer-use` stdio MCP.

## The shape

The control-server exposes **four ports**; a fifth automation surface lives inside each clone.

| Port | Default | Transport | Purpose |
|---|---|---|---|
| **1 — video** | `9001` | framed H.264 over TCP | the selected clone's monitors to the native GTK viewer, with input + clipboard + cursor back |
| **2 — web API** | `9000` | HTTP + SSE (+ embedded frontend) | the React management UI: host selection, clone/Linear/Claude/chat orchestration, settings |
| **3 — per-clone MCP** | `9002` | HTTP JSON-RPC, by source IP | the in-clone agent reports its verdict (`set_state`); clone resolved from the caller's IP |
| **4 — fleet MCP** | `9003` | HTTP JSON-RPC | every web action + every desktop/window tool (with a `clone` selector); desktop tools proxied to the clone's daemon MCP |
| daemon MCP | `9004` | HTTP JSON-RPC (in each clone) | the full desktop-automation surface; the agent calls it on localhost, the fleet MCP proxies to it |
| clone socket | `/srv/rmng-sock/clones.sock` | unix `SOCK_SEQPACKET` | clone-daemon ⇄ control-server: dmabuf frames (`SCM_RIGHTS`) out, input/clipboard in |

**Design in one breath:** one central encoder, thin clones (no g-r-d / GDM / RDP — just
`gnome-session` + the daemon); one capture feeds both the human viewer and the agent's
screenshots; raw H.264-over-TCP into zero-copy VA-API decode gives RFX-class feel without RDP;
media/input cross a host unix socket, not the network, so only the control-server is externally
reachable. Deploy the control-server to one LXC, give it Proxmox SSH, and it builds the golden
template and provisions clones itself — carrying even the patched gnome-shell `.deb` it installs.

## Documentation

| Doc | Covers |
|---|---|
| [docs/API.md](docs/API.md) | Every HTTP endpoint on the web port (9000), incl. the SSE streams |
| [docs/MCP.md](docs/MCP.md) | All three MCP surfaces (per-clone 9002, fleet 9003, daemon 9004): JSON-RPC envelope + every tool + curl examples |
| [docs/PROTOCOL.md](docs/PROTOCOL.md) | The port-1 video/input/clipboard/cursor wire protocol, the clone socket, the config schema, every env var, the clone-daemon CLI, and the per-crate public API |
| [docs/SCRIPTS.md](docs/SCRIPTS.md) | Every script: what it does, where it runs, its args, and what invokes it |
| [docs/DEPLOY.md](docs/DEPLOY.md) | The build → deploy → provision flow end-to-end, day-2 ops, the patched-shell pipeline, and production cutover |
| [docs/INFRA.md](docs/INFRA.md) | The provisioned CTs on the Proxmox node and how to reach each |

## Workspace map

| Path | Kind | What |
|---|---|---|
| [crates/wire](crates/wire/README.md) | lib | shared types: control state, config, the clone socket + viewer protocols, MCP DTOs; ts-rs export for the frontend |
| [crates/control-server](crates/control-server/README.md) | bin | the 4-port server: media plane, web API/SSE, per-clone + fleet MCP, orchestration, embedded frontend + binaries, template bootstrap |
| [crates/media](crates/media/README.md) | lib | dmabuf ingest → VA-API H.264 per monitor + dmabuf→PNG screenshots + the clone-socket transport |
| [crates/clone-daemon](crates/clone-daemon/README.md) | bin | the thin in-clone pipe: RecordVirtual capture, RemoteDesktop input inject, clipboard bridge, the desktop MCP (:9004), and the needs-human detector |
| [crates/viewer](crates/viewer/README.md) | bin | the native GTK client (GUI + headless test mode): zero-copy VA-API decode, multi-monitor, client-drawn cursor, input + pointer-lock + clipboard |
| [crates/control-client](crates/control-client/README.md) | lib | thin reqwest+SSE client for integration tests |
| [frontend](frontend/README.md) | web app | React Router 7 management UI, ts-rs types from `wire`, served by the control-server |
| [gnome-patch](gnome-patch/README.md) | tooling | builds the patched gnome-shell `.deb` (hide screen-share indicator + enable `Eval` for window-mgmt) embedded in the control-server |

The per-clone **agent-wrapper** (Bun, Claude Agent SDK) is vendored at `agent-wrapper/`; the
control-server embeds + deploys it and proxies chat to it. Its `desktop` MCP points at the
clone-daemon (`http://127.0.0.1:9004`).

<a id="clean-room"></a>
## Clean-room

`RMNG` is its own Cargo workspace (own lockfile, edition 2024). It does **not** import the
old client (`../core`, `../gtk`, `../headless`), the old `../control-server`, or
`../computer-use` — those are reference material for proven techniques, re-expressed fresh. The
one preserved contract is the JSON wire format of `/events` and the web API, so the React
frontend works unchanged.

## Quick start

```sh
# 1. Build/dev CT (full toolchain + headless GNOME + render passthrough) → builds the binary.
./scripts/provision-build-ct.sh root@<proxmox>            # → rmng-build CT

# 2. Lean runtime CT that runs the control-server.
./scripts/provision-deploy-ct.sh root@<proxmox>           # → http://<ip>:9000

# 3. From the dashboard/API, bootstrap the golden template, then CoW-clone from it.
curl -X POST http://<deploy-ip>:9000/api/template/bootstrap -d '{"hostname":"rmng-template"}'
```

Open `http://<ip>:9000` → **Settings** for Linear/Claude credentials. Full flow + the dev loop:
[docs/DEPLOY.md](docs/DEPLOY.md); the CTs that already exist: [docs/INFRA.md](docs/INFRA.md).

## Prerequisites

Rust (edition 2024), `bun`, `clang`/`libclang`; `libpipewire-0.3-dev`, `libva-dev` + AMD VA-API
(radeonsi/Mesa), `libdrm-dev`, GStreamer + **`gstreamer1.0-plugins-bad`** (the `va` elements —
*not* `gstreamer1.0-va`), GTK4; a GPU render node (`/dev/dri/renderD128`) on the control-server
host *and* every clone. Only `wire` builds on a plain laptop — everything else needs the GPU box
(edit locally, build on a GPU CT; see [docs/DEPLOY.md](docs/DEPLOY.md)). **Use the Ubuntu 26.04
CT template.**
