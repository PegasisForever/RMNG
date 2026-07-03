# RMNG

> **Hardware-accelerated, fleet-scale cloud desktops for the agentic era.**

![RMNG — a cloud GNOME desktop streamed to a native multi-monitor viewer](docs/hero.webp)

A self-contained Rust system for running, viewing, and automating a fleet of cloud GNOME
desktops. One **control-server** container is the control plane, the media hub, and the fleet
gateway: it orchestrates **clone containers** on a local Docker daemon, ingests each clone's
GPU frames and hardware-encodes the selected one to a **native hardware-decode GTK viewer**,
and brokers the desktop-automation **MCP** that per-clone Claude agents drive. Each clone runs
a thin **clone-daemon** that captures dmabufs, injects input, bridges the clipboard, and
serves its desktop-automation MCP.

It replaces an older split where a native RDP client connected directly to each clone's
`gnome-remote-desktop`, and each clone ran a local `computer-use` stdio MCP.

## The shape

The control-server exposes **four ports**; a fifth automation surface lives inside each clone.

| Port | Default | Transport | Purpose |
|---|---|---|---|
| **1 — video** | `9001` | framed H.264 over TCP | the selected clone's monitors to the native GTK viewer, with input + clipboard + cursor back |
| **2 — web API** | `9000` | HTTP + SSE (+ embedded frontend) | the React management UI: host selection, clone/Linear/Claude/chat orchestration, settings |
| **3 — per-clone MCP** | `9002` | HTTP JSON-RPC, header-routed | the in-clone agent reports its verdict (`set_state`); caller self-identifies via the `x-rmng-clone` header |
| **4 — fleet MCP** | `9003` | HTTP JSON-RPC | every web action + every desktop/window tool (with a `clone` selector); desktop tools proxied to the clone's daemon MCP |
| daemon MCP | `9004` | HTTP JSON-RPC (in each clone) | the full desktop-automation surface; the agent calls it on localhost, the fleet MCP proxies to it |
| clone socket | `/srv/rmng-sock/clones.sock` | unix `SOCK_SEQPACKET` | clone-daemon ⇄ control-server: dmabuf frames (`SCM_RIGHTS`) out, input/clipboard in |

**Design in one breath:** one central encoder, thin clones (no g-r-d / GDM / RDP — just
`gnome-session` + the daemon); one capture feeds both the human viewer and the agent's
screenshots; raw H.264-over-TCP into zero-copy VA-API decode gives RFX-class feel without RDP;
media/input cross a host unix socket, not the network, so only the control-server is externally
reachable. `docker run` the control-server container, open the browser, and the first-run
setup wizard pulls the pre-built clone **template** — a separate published image
(`pegasis0/rmng-template`) that already carries the patched gnome-shell + clone binaries — and
finishes setup. Clone binaries then hot-swap themselves on every control-server upgrade; there's
no manual redeploy step.

## Documentation

| Doc | Covers |
|---|---|
| [docs/API.md](docs/API.md) | Every HTTP endpoint on the web port (9000), incl. the SSE streams |
| [docs/MCP.md](docs/MCP.md) | All three MCP surfaces (per-clone 9002, fleet 9003, daemon 9004): JSON-RPC envelope + every tool + curl examples |
| [docs/PROTOCOL.md](docs/PROTOCOL.md) | The port-1 video/input/clipboard/cursor wire protocol, the clone socket, the config schema, every env var, the clone-daemon CLI, and the per-crate public API |
| [docs/SCRIPTS.md](docs/SCRIPTS.md) | Every script: what it does, where it runs, its args, and what invokes it |
| [docs/DEPLOY.md](docs/DEPLOY.md) | The Docker build → run → wizard → images/clones flow, the image build, upgrades, clone-home browsing, and the dev loop |
| [docs/PROXMOX-LXC.md](docs/PROXMOX-LXC.md) | Running the Docker host on an unprivileged Proxmox LXC CT (one hosting option) |

## Workspace map

| Path | Kind | What |
|---|---|---|
| [crates/wire](crates/wire/README.md) | lib | shared types: control state, config, the clone socket + viewer protocols, MCP DTOs; ts-rs export for the frontend |
| [crates/control-server](crates/control-server/README.md) | bin | the 4-port server: media plane, web API/SSE, per-clone + fleet MCP, Docker orchestration (bollard), on-disk frontend + clone payloads, clone-template pull + automatic binary hot-swap |
| [crates/media](crates/media/README.md) | lib | dmabuf ingest → VA-API H.264 per monitor + dmabuf→PNG screenshots + the clone-socket transport |
| [crates/clone-daemon](crates/clone-daemon/README.md) | bin | the thin in-clone pipe: RecordVirtual capture, RemoteDesktop input inject, clipboard bridge, the desktop MCP (:9004), and the needs-human detector |
| [crates/viewer](crates/viewer/README.md) | bin | the native GTK client (GUI + headless test mode): zero-copy VA-API decode, multi-monitor, client-drawn cursor, input + pointer-lock + clipboard |
| [crates/control-client](crates/control-client/README.md) | lib | thin reqwest+SSE client for integration tests |
| [frontend](frontend/README.md) | web app | React Router 7 management UI, ts-rs types from `wire`, served by the control-server |
| [gnome-patch](gnome-patch/README.md) | tooling | builds the patched gnome-shell `.deb` (hide screen-share indicator + enable `Eval` for window-mgmt); built + installed by `template/Dockerfile`'s `gnome-build` stage into the published clone template — not a control-server payload |

The per-clone **agent-wrapper** (Bun, Claude Agent SDK) is vendored at `agent-wrapper/`; the
clone template ships it pre-installed, the control-server hot-swaps it into running clones on
upgrade, and proxies chat to it. Its `desktop` MCP points at the clone-daemon
(`http://127.0.0.1:9004`).

<a id="clean-room"></a>
## Clean-room

`RMNG` is its own Cargo workspace (own lockfile, edition 2024). It does **not** import the
old client (`../core`, `../gtk`, `../headless`), the old `../control-server`, or
`../computer-use` — those are reference material for proven techniques, re-expressed fresh. The
one preserved contract is the JSON wire format of `/events` and the web API, so the React
frontend works unchanged.

## Quick start

Needs a Linux host with Docker and a GPU render node (`/dev/dri/renderD128`). Pull the
published image (or `docker build -t rmng:latest .` from a checkout), then run the hub:

```sh
docker run -d --name rmng --privileged --init --pid host --restart unless-stopped \
  -v /var/run/docker.sock:/var/run/docker.sock \
  -v rmng-data:/data -v rmng-sock:/srv/rmng-sock \
  -p 9000-9003:9000-9003 pegasis0/rmng
# …or, from a checkout: docker compose up -d --build
```

Open `http://<host>:9000` → the **first-run setup wizard** (environment checklist → server
settings → download the clone template → finish) does the rest; then **Settings** for
Linear/Claude credentials. There are zero `-e` config flags — everything is set in the UI. Full
flow, the image build, publishing the template, upgrades, and the dev loop:
[docs/DEPLOY.md](docs/DEPLOY.md). Running the Docker host on a Proxmox LXC CT:
[docs/PROXMOX-LXC.md](docs/PROXMOX-LXC.md).

## Prerequisites

Rust (edition 2024), `bun`, `clang`/`libclang`; `libpipewire-0.3-dev`, `libva-dev` + AMD VA-API
(radeonsi/Mesa), `libdrm-dev`, GStreamer + **`gstreamer1.0-plugins-bad`** (the `va` elements —
*not* `gstreamer1.0-va`), GTK4; a GPU render node (`/dev/dri/renderD128`) on the control-server
host *and* every clone. With those dev libs the **whole workspace compiles on a plain laptop**
(a bare box without them builds only `wire`); the GPU box is only needed to *run* the
capture/encode/server side — the **`viewer` builds *and* runs locally** (client-side decode).
See the [dev loop](docs/DEPLOY.md#the-dev-loop). **The clone template is built on the
`ubuntu:26.04` base OS** (the patched gnome-shell is compiled against 26.04's GNOME only) —
see [Publishing the template](docs/DEPLOY.md#publishing-the-template).
