# RMNG

![RMNG](docs/hero.webp)

> **Hardware-accelerated, fleet-scale cloud desktops for the agentic era.**

A self-contained Rust system for running, viewing, and automating a fleet of cloud GNOME desktops. A single **control-server** container orchestrates **clone containers** on a local Docker daemon, hardware-encodes the selected clone's GPU frames to a **native hardware-decode GTK viewer**, and brokers the desktop-automation **MCP** that per-clone Claude agents drive. Each clone runs a thin **clone-daemon** that captures frames, injects input, and bridges the clipboard.

## Architecture

One central encoder feeds both the viewer and the agents' screenshots; raw H.264 over TCP into zero-copy VA-API decode gives RFX-class feel without RDP, and media/input cross a host unix socket so only the control-server is exposed. Full port map, protocols, and workspace layout: **[docs/DEVELOPMENT.md](docs/DEVELOPMENT.md)**.

**Built from scratch** — encoder, decoder, viewer, and wire protocol are all original Rust; no code from VNC, RDP, or gnome-remote-desktop.

## Features

**Remote Desktop**

- Zero-copy full-chroma 4:4:4 hardware H.264 video pipeline end to end (even on hardware that only support 4:2:0!)
- Native hardware accelerated viewer on Linux and macOS
- Full 60fps in local network
- Multi-monitor
- Instant swap between clones
- Absolute + relative pointer
- Rich clipboard bridge, remote↔remote and local↔remote
- Port forwarding

**Fleet Management**

- One control-server Docker container manages everything, and is the access point to everything
- Web based control dashboard, note for each clone
- Docker + lxcfs for clone sandboxing
- Full gnome in each clone
- every clone's home dir accessable in one central SMB share

**Automation (CLI + MCP + agents)**

- `rmng` fleet management CLI in every clone (hosts, clones, images, accounts — over the control-server web API)
- Computer use: fleet-wide via the `rmng desktop` CLI (per-`clone`), backed by each clone's daemon MCP on localhost:9004
- Chat with per-clone agent over web ui
- Needs human detector

**Accounts & integrations**

- Both Claude and Codex/ChatGPT accounts, full parity
- Import/harvest from a signed-in clone (server owns the token lifecycle after)
- 5h + 7d usage bars, spend vs. limit, stale/rate-limited flags
- Live hot-swap of a running clone's account, no restart
- Named account groups with sticky rotation
- Auto-assign at clone creation; "auto" rotates a clone across all imported accounts as they exhaust
- Pin, or bind a clone to auto / a group / none



## Quick start

> **Hardware support:** the **encode** path (control-server, VA-API H.264) has only been tested on an **AMD Radeon Pro W6800**; the **decode** path (viewer) has only been tested on **Intel integrated graphics** (Linux) and **Apple M-series** (macOS). Other GPUs may work but are untested.

Needs a Linux host with Docker and a GPU render node (`/dev/dri/renderD128`). Pull the published image (or `docker build -t rmng:latest .` from a checkout), then run the hub:

```sh
docker run -d --name rmng --privileged --init --pid host --restart unless-stopped \
  -v /var/run/docker.sock:/var/run/docker.sock \
  -v rmng-data:/data -v rmng-sock:/srv/rmng-sock \
  -p 9000-9002:9000-9002 -p 9005:9005 -p 445:445 -p 2222:2222 pegasis0/rmng
```

Ports: `9000` web UI/API · `9001` video · `9002` per-clone MCP · `9005` port-forward data plane · `445` SMB clone-home share (host `445` must be free) · `2222` SSH bastion (jump into clones).

Open `http://<host>:9000` → the **first-run setup wizard** (environment checklist → server settings → download the clone template → finish) does the rest; then **Settings** for Linear/Claude credentials. There are zero `-e` config flags — everything is set in the UI. Full flow, the image build, publishing the template, upgrades, and the dev loop: [docs/DEPLOY.md](docs/DEPLOY.md). Running the Docker host on a Proxmox LXC CT: [docs/PROXMOX-LXC.md](docs/PROXMOX-LXC.md).

## Documentation

Architecture, the full port/protocol map, the workspace layout, build prerequisites, and the clean-room policy live in **[docs/DEVELOPMENT.md](docs/DEVELOPMENT.md)**, which also links every deep-reference doc ([API](docs/API.md) · [CLI](docs/CLI.md) · [MCP](docs/MCP.md) · [PROTOCOL](docs/PROTOCOL.md) · [SCRIPTS](docs/SCRIPTS.md) · [DEPLOY](docs/DEPLOY.md) · [PROXMOX-LXC](docs/PROXMOX-LXC.md)).