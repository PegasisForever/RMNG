# Build & deploy

RMNG runs its whole fleet as containers on **one local Docker daemon**. The control-server
is itself a container; it drives sibling clone containers through the Docker socket (bollard,
unix socket only — no SSH, no Proxmox). Deployment is: run the control-server container →
open the browser → the first-run **setup wizard** builds the base image and finishes setup.
Everything after that (base images, clones, redeploys, monitor layouts) is driven from the
running server's dashboard/API.

> **The base OS is `ubuntu:26.04`.** 24.04's older mesa negotiates a different DRM modifier
> than the capture path expects → `no more input formats`. The base image is fixed in code
> (`BASE_DOCKER_IMAGE`) — the patched gnome-shell is compiled against 26.04's GNOME only.

## Requirements

- A Linux host with **Docker** (bare metal, a VM, or an LXC CT — see
  [PROXMOX-LXC.md](PROXMOX-LXC.md)), overlay2 storage driver.
- A **GPU render node** `/dev/dri/renderD128` on the host (AMD radeonsi/Mesa VA-API). The
  control-server VA-API-**encodes** every clone's frames and each clone **captures** its own
  desktop — both need the render node. Validated on the AMD Radeon Pro **W6800**.
- Ports **9000–9003** free (web/API, video, per-clone MCP, fleet MCP).

## 1. Get the image

Pull the published image, or build it from source (the canonical alternative — the build is
fully hermetic):

```sh
# Published image (Docker Hub):
docker pull pegasis0/rmng

# …or build locally (see "The image build" below). Produces rmng:latest.
docker build -t rmng:latest .
```

Air-gapped host with no registry access? Ship the image over SSH:

```sh
docker save pegasis0/rmng | ssh <host> docker load
```

## 2. Run the control-server

The reference deployment is [`compose.yaml`](../compose.yaml) at the repo root. It builds
`rmng:latest` from source and brings the hub up:

```sh
docker compose up -d --build          # builds rmng:latest, then starts it
```

To pull the published image instead of building, point compose's `image:` at `pegasis0/rmng`
and run `docker compose up -d` (no `--build`). The equivalent one-liner off the registry:

```sh
docker run -d --name rmng --privileged --init --pid host --restart unless-stopped \
  -v /var/run/docker.sock:/var/run/docker.sock \
  -v rmng-data:/data -v rmng-sock:/srv/rmng-sock \
  -p 9000-9003:9000-9003 pegasis0/rmng
```

What each piece is for:

| Flag / mount | Why |
|---|---|
| `--privileged` | the control-server orchestrates **privileged** clone containers (nested Docker) on the same daemon |
| `--init` | PID-1 reaper for the short-lived exec/tar helpers the server spawns |
| `--pid host` | share the host PID namespace so clone PIDs are visible → the clone-home browse view (below). Omitting it disables **only** that feature (the server warns once) |
| `-v /var/run/docker.sock:…` | the daemon the server drives via bollard |
| `-v rmng-data:/data` | `config.json` + `data/` (WORKDIR is `/data`) — persists setup + state across restarts |
| `-v rmng-sock:/srv/rmng-sock` | the shared clone **media socket** dir. Load-bearing: this exact **named** volume is mounted into every clone at `/srv/rmng-sock` so clone-daemons reach the media plane. Must be a named volume (not a bind) so clones can share it |
| `-p 9000-9003:9000-9003` | the four listen ports |

**There are zero `-e` configuration flags, by design.** `config.json` (edited via the
wizard / Settings, `PUT /api/config`) is the single source of truth — subnet, hostname
prefix, monitors, ports, clone limits, presets are all set in the UI (the no-env-settings
invariant). The only `ENV` in the image is `RUST_LOG=info,tower_http=warn,clip=debug`, a
logging default, not a setting.

The server **boots even when Docker is absent or the socket isn't mounted** — a missing /
broken `docker.sock` is surfaced as a failing row in the wizard's environment checklist
(`GET /api/setup/env`), not a crash, so the operator fixes it there.

## 3. First-run setup wizard

Open `http://<host>:9000`. A fresh deploy ships `config.json` with `"setupComplete": false`,
so the web UI opens the **first-run setup wizard** instead of the dashboard. There is no
grandfather rule: an old `config.json` re-runs the wizard (new machine, no `rmng` network /
base image); a stale `proxmox` block is scrubbed on load and its `hostnamePrefix` is carried
into `docker.hostnamePrefix`.

The wizard walks four things:

1. **Environment checklist** (`GET /api/setup/env`) — pass/fail rows: **Docker daemon**
   reachable, **control-server container** detected (info; absence = dev mode), **clone media
   socket mount** present (`/srv/rmng-sock`), **GPU render node** `/dev/dri/renderD128`
   present. Required rows must pass to proceed.
2. **Server settings** — the one-time `docker.subnet` (IPv4 CIDR, validated `/16`–`/24`,
   default `10.99.0.0/24`), `docker.hostnamePrefix` (e.g. `pega-`), monitor layout, listen
   ports, and per-clone limits (`docker.cloneCpus`, `docker.cloneMemoryMb`).
3. **Build the base image** (`POST /api/images/bootstrap {name}`) — from-zero build of the
   first clone-source image (headless GNOME + clone-daemon + agent-wrapper + patched
   gnome-shell). `name` is a bare DNS label; the server prepends the repo →
   `rmng/template:<name>`.
4. **Finish** — latches `setupComplete: true`, which is where the lazy `rmng` bridge network
   is first materialized (`.1` gateway, `.2` control-server, `.10+` clone pool).

Afterward, use **Settings** to create presets (Linear key + labels + env vars), Claude
settings, monitor defaults, and the ports. Claude accounts are imported from a signed-in
clone, not entered here. Secrets are write-only and redacted on read. The one-time fields
(`dataDir`, `cloneSocket`, `docker.subnet`) lock once the wizard latches. See
[SCRIPTS.md](SCRIPTS.md) for the in-container provisioning scripts and [API.md](API.md) for
every endpoint.

## Images & clones

RMNG uses **image-only templates** — there is no golden-CT / CoW model. A clone-source image
is any image labeled `rmng.image=1`, repo `rmng/template:<name>`:

- **Build a base image**: `POST /api/images/bootstrap {name}` (the wizard's step 3; also the
  "New image" affordance later). Labeled `rmng.base=1`.
- **List images**: `GET /api/images` — each with the ids of live clones running on it.
- **Clone from an image**: `POST /api/clone` takes `image` (a `rmng/template:<name>`
  reference from the image list) plus a task mode (Linear ticket / new ticket / plain). The
  clone gets a static IP (`.10+`), fixed `rmng`/`rmng` credentials, its preset env, and a
  Claude account.
- **Commit a clone to a new image**: `POST /api/images/commit {host, name}` — freezes the
  running clone and commits it to `rmng/template:<name>` (`rmng.created-from` records
  lineage). On-disk credentials in the clone's home are baked into the image (logged as a
  warning).
- **Delete an image**: `POST /api/images/delete {reference}` — 409 if any clone still runs on
  it or a build/commit is in flight.

**DinD × images are decoupled** (a semantic change from the old LVM-snapshot behavior):
`docker commit` **excludes volume mounts**, and each clone's inner Docker (`/var/lib/docker`)
lives on its per-clone `rmng-dind-<id>` volume. So a clone's inner-Docker state (pulled
images, build cache, running inner containers) **never travels into a committed image** —
every clone always starts with an **empty inner Docker**. Daemon config / compose files in
the clone user's `$HOME` **do** travel (they're on the image filesystem, not the volume). If
you ever need seeded inner state, bake it at bootstrap; commit-from-clone can't carry it.

## Upgrades

The image is stateless; state lives in the `rmng-data` volume and in the sibling clone
containers, both of which survive a control-server replacement:

```sh
docker pull pegasis0/rmng          # or: docker build -t rmng:latest .
docker rm -f rmng
docker run -d --name rmng …         # the same run/compose invocation as above
# or: docker compose up -d
```

The `rmng-data` / `rmng-sock` volumes and every running clone container persist across the
swap. The control-server keeps its static `.2` address, so URLs baked into clones still
resolve. After upgrading, push the new image's clone binaries into existing clones without
reprovisioning: `POST /api/clone/redeploy {id, daemonOnly?}` copies fresh `clone-daemon`
(+ `agent-wrapper` unless `daemonOnly`) payloads from the new image into the clone (~10 s);
`daemonOnly` keeps the Claude session alive.

## Browsing clone homes (`data/hosts/<id>`)

With `--pid host`, the control-server shares the host PID namespace, so a 15 s reconciler
maintains a symlink per running managed clone:

```
<data_dir>/hosts/<id> → /proc/<clone-pid-1>/root/home/rmng
```

That surfaces every clone's home (`/home/rmng`) in one directory. It repoints links across
clone restarts (the PID changes) and prunes stopped/deleted clones. Reach it three ways:

- **From the Docker host** (the same symlink path resolves there, since `/proc/<pid>/root` is
  the clone's rootfs): `/var/lib/docker/volumes/rmng-data/_data/data/hosts/<id>`.
- **Over sshfs** to the host: mount with `-o follow_symlinks` so the `/proc/*` targets
  resolve.
- **`docker exec`** into the control-server container and browse `data/hosts/`.

Omit `--pid host` and this feature is simply off (the server logs a one-time hint per clone);
nothing else is affected.

## The image build

`docker build -t rmng:latest .` is **fully hermetic** — one command produces everything,
including the patched gnome-shell. There is no `artifacts/` dir, no `build-shell-artifact.sh`,
no build CT. The Dockerfile (BuildKit multi-stage) has three independent build stages that
BuildKit runs **in parallel**, feeding one runtime stage:

| Stage | Produces |
|---|---|
| `bun-build` | the frontend (`frontend/build/client`) + `agent-wrapper` (`bun build --compile`) |
| `gnome-build` | the patched gnome-shell `.deb` (shell-01 hide screen-share indicator + shell-03 enable `Shell.Eval`) via `gnome-patch/build-shell-deb.sh` |
| `rust-build` | `clone-daemon` + `control-server` (`cargo build --release`) |
| `runtime` | `ubuntu:26.04` + GStreamer/VA runtime + the payloads below |

- **First build is long** — the `gnome-build` stage's `apt build-dep gnome-shell` layer is
  multi-GB. It is build-stage-only and cached until the base image changes; a `gnome-patch/`
  edit re-runs only the compile layer.
- **Rebuilds are cached and stage-independent** — the stages share no dependencies, so a
  **source-only Rust change rebuilds only the rust layers**; the gnome deps layer and the bun
  install layer stay cached.

**Nothing is compiled into the binary** (rust-embed is gone). The runtime image carries plain
payloads under `/usr/local/share/rmng/`:

```
/usr/local/share/rmng/clone-daemon      # pushed into clones by provision.rs
/usr/local/share/rmng/agent-wrapper     # pushed into clones
/usr/local/share/rmng/gnome-shell.deb   # installed over stock at bootstrap
/usr/local/share/rmng/static/           # the frontend, served on port 2
```

`assets.rs` reads these at runtime with a two-entry search path: the image install dir first,
then a repo-relative **dev fallback** — `crates/control-server/embedded-bin/` for the three
payloads and `frontend/build/client` for the frontend. That is what makes `cargo run -p
control-server` from a checkout work without any config (see the dev loop). A missing payload
is tolerated (WARN + fall back — e.g. no `gnome-shell.deb` → clones run the stock shell).

## The dev loop

The whole workspace compiles on any Linux dev box with the desktop media/GUI dev libs (the
[Prerequisites](../README.md#prerequisites): GStreamer + GTK4 + PipeWire + libdrm + `clang`);
a bare box without them builds only `wire`. What needs a **GPU** is *running* the pipeline —
the control-server's VA-API **encode** and each clone's **capture** — so exercising real
clones requires the W6800 host with Docker. The **`viewer` is the exception: it builds *and*
runs locally** (client-side VA-API **decode** only; Intel iGPU decode is validated against
AMD-encoded streams).

### Local (on your laptop)

| You changed | Build & run locally | See the result |
|---|---|---|
| **`viewer`** | `RMNG_VIDEO=<host>:9001 cargo run -p viewer` | GUI window streaming the server's *selected clone* |
| **`viewer`**, no display | `RMNG_VIDEO=<host>:9001 RMNG_DUMP=frame.png cargo run -p viewer -- --headless` | per-monitor fps in the logs; `frame.png` = one decoded frame |
| **frontend** | `cd frontend && bun run dev` | Vite dev server + HMR; proxies `/api` + `/events` to a running backend |
| **`wire`** types / DTOs | `cargo test -p wire` | compiles + regenerates the frontend's ts-rs types |
| pure logic in **any** crate | `cargo build -p <crate>` · `cargo test -p <crate>` | the whole workspace compiles locally, so the compiler + unit tests are a local loop |

### On the GPU host (real clones)

Two options for exercising the full clone/capture/encode path against a local Docker daemon:

- **Image loop**: `docker build -t rmng:latest .` then `docker compose up -d` on the GPU
  host. Fresh clone binaries land in existing clones via `POST /api/clone/redeploy`.
- **`cargo run` loop** (fast rebuilds, no image): run `cargo run -p control-server` from the
  checkout on the GPU host. It runs in **dev mode** — no self-container, so it uses the `rmng`
  bridge **gateway `.1`** as its control IP and talks to the local daemon at
  `/var/run/docker.sock`. For provisioning to work, stage the three payloads into
  `crates/control-server/embedded-bin/` (gitignored) and either `bun run build` the frontend
  (so `frontend/build/client` resolves) or run `bun run dev`. `config.json` + `data/` are
  CWD-relative.

Then, from the dashboard: build a base image (`POST /api/images/bootstrap`), clone from it
(`POST /api/clone`), select the clone, and point the viewer at the host. Redeploy a clone's
binaries after a `clone-daemon` / `agent-wrapper` change with `POST /api/clone/redeploy {id,
daemonOnly?}` (or the fleet MCP `redeploy` tool).

## Networking & the media socket

- **`rmng` bridge**: a user-defined bridge with static IPAM from `docker.subnet` — `.1`
  gateway, `.2` control-server (static, so recreating it never strands baked
  `RMNG_CONTROL_URL`s), `.10+` clone pool (lowest-free allocator). Created lazily at wizard
  finish and before each clone. If an `rmng` network already exists with a **different**
  subnet, `ensure_network` errors — delete it with `docker network rm rmng` and re-run setup.
- **Clone media socket**: clone-daemon ships dmabuf frames to the control-server over a
  `SOCK_SEQPACKET` unix socket (fds via `SCM_RIGHTS`), *not* the network. The shared
  `rmng-sock` named volume is mounted into the control-server and every clone at the same path
  `/srv/rmng-sock`; the server `chmod 0777`s the socket so a different-uid clone-daemon can
  connect. See [PROTOCOL.md](PROTOCOL.md).

## Patched gnome-shell

The clone-daemon needs two gnome-shell patches: **shell-01** (hide the screen-sharing pill
that would otherwise composite into captured frames) and **shell-03** (enable
`org.gnome.Shell.Eval` for the window-management MCP tools). The `gnome-build` Dockerfile
stage builds the patched `gnome-shell_*+ngshell1` `.deb` (rebuilding only `libshell-<N>.so`
and repacking the stock deb) → `/usr/local/share/rmng/gnome-shell.deb`. `provision-clone.sh`
installs it over stock during a base-image build; every clone off that image inherits it.
Details + verification: [gnome-patch/README.md](../gnome-patch/README.md).

## Day-2 operations (from the dashboard / API / fleet MCP)

- **Clone**: `POST /api/clone` — Linear ticket / new ticket / plain, from a chosen image.
- **Commit a clone → image**: `POST /api/images/commit {host, name}`.
- **Redeploy binaries** (no reprovision, ~10 s): `POST /api/clone/redeploy {id, daemonOnly?}`
  or the fleet MCP `redeploy` tool. `daemonOnly` keeps the Claude session alive.
- **Apply a monitor layout** to running clones: `POST /api/monitors/apply` (rewrites each
  clone's `RMNG_MONITORS` + restarts its GNOME/daemon).
- **Hot-swap a Claude account**: `POST /api/claude/swap {host, account}` — writes the clone's
  `~/.claude/.credentials.json` live via `docker exec`.
- **Delete**: `POST /api/delete {id}` (stops + removes the container and its
  `rmng-dind-<id>` volume; an unmanaged row is just unregistered).

## Gotchas

These are baked into the code/scripts now; listed so they aren't re-discovered.

1. **`gstreamer1.0-va` is not a package** on 24.04/26.04 — the `va` elements
   (`vah264enc`/`vapostproc`) live in **`gstreamer1.0-plugins-bad`**; `gstreamer1.0-vaapi` is
   the unrelated legacy plugin. The runtime image installs `-bad` (+ `-good` for `pngenc`).
2. **The DRM modifier is pinned** to the W6800 tiled modifier validated on 26.04's mesa → use
   the 26.04 base (proper PipeWire modifier negotiation is a tracked follow-up). On 24.04 the
   capture path fails with `no more input formats`.
3. **The clone socket must be a named volume, not a bind** — every clone mounts the same
   `rmng-sock` volume at `/srv/rmng-sock`; a host bind wouldn't be shareable into siblings.
4. **Clones need `StopSignal=SIGRTMIN+3`** (baked into every image by `commit` with
   `set_boot_config`) or every stop is a 20 s hang + SIGKILL.
5. **A per-clone `rmng-dind-<id>` volume** mounts at `/var/lib/docker` (the overlay-on-
   overlay fix). It is never committed into images and is removed on clone delete.
6. **`ubuntu:26.04` pull rate limits** on Docker Hub surface verbatim in the wizard's
   base-image build log.
