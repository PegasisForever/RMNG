# Build & deploy

RMNG runs its whole fleet as containers on **one local Docker daemon**. The control-server
is itself a container; it drives sibling clone containers through the Docker socket (bollard,
unix socket only — no SSH, no Proxmox). Deployment is: run the control-server container →
open the browser → the first-run **setup wizard** pulls the clone template and finishes setup.
Everything after that (images, clones, monitor layouts) is driven from the running server's
dashboard/API/`rmng` CLI — clone binaries are injected at clone-create time by the running
server, with no manual redeploy step (see [Upgrades](#upgrades)).

> **The clone template's base OS is `ubuntu:26.04`.** 24.04's older mesa negotiates a different
> DRM modifier than the capture path expects → `no more input formats`. The base OS is fixed in
> [`template/Dockerfile`](../template/Dockerfile)'s final stage (`FROM ubuntu:26.04`) — the
> patched gnome-shell is compiled against 26.04's GNOME only, so it isn't a pull-time choice.

## Requirements

- A Linux host with **Docker** (bare metal, a VM, or an LXC CT — see
  [PROXMOX-LXC.md](PROXMOX-LXC.md)), overlay2 storage driver.
- A **GPU render node** `/dev/dri/renderD128` on the host (AMD radeonsi/Mesa VA-API). The
  control-server VA-API-**encodes** every clone's frames and each clone **captures** its own
  desktop — both need the render node. Validated on the AMD Radeon Pro **W6800**.
- Ports **9000–9002**, **9005** (port-forward), **445** (SMB), and **2222** (SSH bastion) free
  (web/API, video, per-clone MCP, port-forward data plane, clone-home share, jump
  into clones).

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
  -p 9000-9002:9000-9002 -p 9005:9005 -p 445:445 -p 2222:2222 pegasis0/rmng
```

What each piece is for:

| Flag / mount | Why |
|---|---|
| `--privileged` | the control-server orchestrates **privileged** clone containers (nested Docker) on the same daemon |
| `--init` | PID-1 reaper for the short-lived exec/tar helpers the server spawns |
| `--pid host` | share the host PID namespace so clone PIDs are visible — that's what lets the reconciler find each clone's **uid-1000** process (matched by mount namespace via `/proc/<pid>/ns/mnt`) and link the browse target, served both as the `clones` SMB share and at the host path. Omitting it disables **only** that feature (the server warns once) |
| `-v /var/run/docker.sock:…` | the daemon the server drives via bollard |
| `-v rmng-data:/data` | `config.json` + `data/` (WORKDIR is `/data`) — persists setup + state across restarts |
| `-v rmng-sock:/srv/rmng-sock` | the shared clone **media socket** dir. Load-bearing: this exact **named** volume is mounted into every clone at `/srv/rmng-sock` so clone-daemons reach the media plane. Must be a named volume (not a bind) so clones can share it |
| `-p 9000-9002:9000-9002` | the web API, video, and per-clone MCP ports |
| `-p 9005:9005` | the port-forward data plane (viewer↔clone TCP splice) |
| `-p 445:445` | the SMB shares — `clones` (browse every running clone's `/home/rmng`) and `feedback` (the detector-feedback records) — from `smb://<host>/clones` and `smb://<host>/feedback` (below) |
| `-p 2222:2222` | the SSH bastion — jump host for `ssh`/`scp`/`rsync`/VSCode Remote-SSH into any clone's own `sshd` (see [SSH into clones](#ssh-into-clones)) |

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
template pulled yet); a stale `proxmox` block is scrubbed on load and its `hostnamePrefix` is
carried into `docker.hostnamePrefix`.

The wizard walks four things:

1. **Environment checklist** (`GET /api/setup/env`) — pass/fail rows: **Docker daemon**
   reachable, **control-server container** detected (info; absence = dev mode), **clone media
   socket mount** present (`/srv/rmng-sock`), **GPU render node** `/dev/dri/renderD128`
   present. Required rows must pass to proceed.
2. **Server settings** — the one-time `docker.subnet` (IPv4 CIDR, validated `/16`–`/24`,
   default `10.99.0.0/24`), `docker.hostnamePrefix` (e.g. `pega-`), monitor layout, listen
   ports, and per-clone limits (`docker.cloneCpus`, `docker.cloneMemoryMb`).
3. **Download template** (`POST /api/images/pull {reference?}`) — pulls the pre-built
   clone template (headless GNOME + clone-daemon + agent-wrapper + patched gnome-shell, built
   on `ubuntu:26.04`) from a registry. The pulled image keeps its own `repo:tag` as the
   clone-source reference (no local retag); `reference` defaults to the configured
   `docker.templateReference` (`pegasis0/rmng-template:latest`) and is editable in the wizard.
   Aggregate byte progress streams over the driving `Operation` (kind `pull`). This step is
   **skippable** ("Skip for now") — pull a template later from the Images panel. See
   [Publishing the template](#publishing-the-template) for how that image is built.
4. **Finish** — latches `setupComplete: true`, which is where the lazy `rmng` bridge network
   is first materialized (`.1` gateway, `.2` control-server, `.10+` clone pool).

Afterward, use **Settings** to create presets (Linear key + labels + env vars), Claude
settings, monitor defaults, and the ports. Claude accounts are imported from a signed-in
clone, not entered here. Secrets are write-only and redacted on read. The one-time fields
(`dataDir`, `cloneSocket`, `docker.subnet`) lock once the wizard latches. See
[SCRIPTS.md](SCRIPTS.md) for the in-container guest scripts and [API.md](API.md) for every
endpoint.

## Shared build cache & Docker Hub mirror

The control-server automatically runs two shared infra containers on the `rmng` bridge
(labeled `rmng.infra=1`, started at boot, `restart: unless-stopped`):

- **`rmng-registry`** — a pull-through cache for Docker Hub. Every clone's base-image pulls
  are served from here after the first fleet-wide fetch, so `docker.io` rate limits stop
  biting. Cache lives in the `rmng-registry-data` volume.
- **`rmng-buildkit`** — a shared BuildKit daemon. Each clone's `docker build` transparently
  routes to it (via a `--driver remote` buildx builder named `rmng`), so identical layers are
  built once and reused across the whole fleet. Cache lives in `rmng-buildkit-cache`, capped
  at `docker.buildkitCacheGb` GiB (default 40).

No setup is needed — a fresh control-server ensures both containers, and a background
reconciler migrates the mirror config + builder onto every running clone within ~30 s (live,
no clone restart). Existing clones created before the upgrade are migrated the same way.

**Operator notes**
- Builds run on the shared `rmng-buildkit` daemon, so a build is **not** bounded by a single
  clone's CPU/memory limit and competes fleet-wide (fine for a small trusted fleet).
- If `rmng-buildkit` is down, in-clone `docker build` fails until it is back; the clone's local
  `default` builder remains as a manual fallback: `docker buildx use default`.
- Turn the whole feature off with `docker.buildInfraEnabled = false` in config.json.
  This is a pure "stop managing": the reconciler stops touching clones and no infra
  is ensured; already-created infra containers and already-migrated clone config are left in
  place (remove them manually if you want them gone).

## Images & clones

RMNG uses **image-only templates** — there is no golden-CT / CoW model. A clone-source image
is any image labeled `rmng.image=1`, identified by its own `repo:tag` (e.g.
`pegasis0/rmng-template:latest`):

- **Pull a template**: `POST /api/images/pull {reference?}` (the wizard's step 3,
  "Download template"; also the pull affordance in the Images panel later). The pulled image
  keeps its own `repo:tag` as the clone-source reference (no retag); `reference` defaults to
  `config.docker.templateReference`. The stock published template already carries `rmng.base=1`
  (baked in by `template/Dockerfile`). See
  [Publishing the template](#publishing-the-template) for how it's built.
- **List images**: `GET /api/images` — each with the ids of live clones running on it.
- **Clone from an image**: `POST /api/clone` takes `image` (a `repo:tag` reference such as
  `pegasis0/rmng-template:latest` from the image list) plus a task mode (Linear ticket / new
  ticket / plain). The clone joins the `rmng` bridge (addressed by container name — Docker DNS;
  its IP is plain Docker IPAM) with fixed `rmng`/`rmng` credentials, its preset env, and a
  Claude account.
- **Commit a clone to a new image**: `POST /api/images/commit {host, name}` — freezes the
  running clone and commits it to `<name>:latest` (the name is the full repo; `rmng.created-from`
  records lineage). On-disk credentials in the clone's home are baked into the image (logged as
  a warning).
- **Delete an image**: `POST /api/images/delete {reference}` — 409 if any clone still runs on
  it or a running pull/commit is in flight.

## Publishing the template

The clone template (`pegasis0/rmng-template` by default) is a **separate image** from the
control-server image — it's what `POST /api/images/pull` downloads and what every clone is
created FROM. It replaces the old in-product bootstrap (the control-server used to run
`provision-clone.sh` inside a privileged build container over `docker exec`, then commit the
result); that recipe now lives in [`template/setup/`](../template/setup/) as ordered phase
scripts (`lib.sh` shared helpers, then `10-desktop.sh`, `15-gnome-patch.sh`, `20-toolbox.sh`,
`30-user.sh`) **run by [`template/Dockerfile`](../template/Dockerfile)** itself — the template
is built once and published, not built per install.

Build + tag + push with the wrapper script (repo-root context — the final stage `COPY`s
`template/setup/` + the stage payloads):

```sh
docker login                        # once, to the target registry (Docker Hub pegasis0 org by default)
scripts/publish-template.sh          # builds + tags + pushes pegasis0/rmng-template
# …or a different repo:
scripts/publish-template.sh myorg/rmng-template
TEMPLATE_REPO=myorg/rmng-template scripts/publish-template.sh
```

Equivalent to what the script runs:

```sh
docker build -f template/Dockerfile \
  -t pegasis0/rmng-template:$(date +%Y%m%d) -t pegasis0/rmng-template:latest .
docker push pegasis0/rmng-template:$(date +%Y%m%d)
docker push pegasis0/rmng-template:latest
```

**Versioning**: every publish tags an immutable dated `:YYYYMMDD` **and** repoints the moving
`:latest`. Nothing is ever overwritten — a rollback is just pointing `docker.templateReference`
(Settings, or the pull body's `reference`) at a prior dated tag and pulling again.

**A multi-tagged image only untags on the first delete.** `pull_template` does **not** retag —
the pulled image keeps its own `repo:tag` (e.g. `pegasis0/rmng-template:latest`) as the
clone-source reference. But if the same image carries more than one tag (say you pulled both
`:latest` and a dated `:20260703`), `POST /api/images/delete {reference: "…:latest"}` only
removes that one tag; the underlying layers aren't freed while another tag still references
them, and `GET /api/images` re-lists the same row under whichever tag remains. Delete it again
— using the reference the row now shows — to actually free the layers.

**DinD × images are decoupled** (a semantic change from the old LVM-snapshot behavior):
`docker commit` **excludes volume mounts**, and each clone's inner Docker (`/var/lib/docker`)
lives on its per-clone `rmng-dind-<id>` volume. So a clone's inner-Docker state (pulled
images, build cache, running inner containers) **never travels into a committed image** —
every clone always starts with an **empty inner Docker**. Daemon config / compose files in
the clone user's `$HOME` **do** travel (they're on the image filesystem, not the volume). If
you ever need seeded inner state, bake it into the template build; commit-from-clone can't
carry it.

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
resolve.

**Clone binaries are installed at create time — there is no redeploy button, endpoint, or
hot-swap engine.** The control-server copies its own current payloads (`clone-daemon` +
`agent-wrapper` → `/opt/rmng/bin`, the `rmng` fleet CLI → `/usr/local/bin/rmng` — see
[`provision.rs`](../crates/control-server/src/provision.rs) `CLONE_BINARIES`) into every
clone **before it boots**. That is the **sole delivery path** (the binswap hot-swap engine is
retired): the template carries none of them, and a fresh clone always runs binaries matching
the server that created it. **Existing clones keep the binaries they were created with**
across a control-server upgrade — recreate a clone to move it onto the new binaries (which
also means clones created before a server that ships the `rmng` CLI don't have it).

**Dev caveat**: in a `cargo run` dev checkout the payloads come from
`crates/control-server/embedded-bin/` — with nothing staged there, a clone boots without
clone-daemon/agent-wrapper (a WARN says so at create time).

### In-product restart & update (Docker deployment)

Once the control-server is running a build that includes the self-update feature, its
Settings page has **Restart control-server** and **Update** buttons:

- **Restart** does an in-place `docker restart` of the control-server container (applies
  changed port/socket/static-dir/chroma settings, re-read from config.json on boot). It does
  NOT change the container's host-published port mapping — a `listen` port moved outside the
  published `9000-9002` range still needs a host-level recreate.
- **Update** pulls `docker.serverImage` (default `pegasis0/rmng:latest`) and swaps the
  running container onto it via a detached helper. Running clones and the data volumes
  survive.

**First update is manual.** A server that predates this feature has no update code path, so
the first hop onto a feature-bearing image is still the manual `docker pull … && docker rm -f
… && docker run …` above. Every update after that is in-product.

Publish a new control-server image with `scripts/publish-server.sh` (tags `:YYYYMMDD` +
`:latest`, stamps the version labels the UI reads).

## Browsing clone homes (`data/hosts/<id>`)

With `--pid host`, the control-server shares the host PID namespace, so a 15 s reconciler
maintains a symlink per running managed clone:

```
<data_dir>/hosts/<id> → /proc/<uid-1000-pid>/root/home/rmng
```

That surfaces every clone's home (`/home/rmng`) in one directory. It repoints links across
clone restarts (the PID changes) and prunes stopped/deleted clones. Reach it three ways:

- **Over SMB** (the primary client path) — the control-server serves that directory as the
  `clones` share on port **445**, so `smb://<docker-host>/clones` browses every clone's home
  from any machine. Linux: `smbclient //<host>/clones -U rmng`, or mount it with
  `mount -t cifs //<host>/clones /mnt -o user=rmng`; macOS: Finder → ⌘K → `smb://<host>/clones`.
  Fixed credential → user `rmng`, password `rmng`. **Prerequisite: host port 445 must be free**
  (the `-p 445:445` publish fails clearly if something already holds it). Files you create over
  SMB land owned by the clone's own `rmng` user (uid **1000**).
- **The `feedback` share** — the same `smbd` also serves the control-server's detector-feedback
  records (`data/detector-feedback`) as `smb://<host>/feedback`, read-write, same `rmng`/`rmng`
  credential. Browse or prune the JSON records + screenshots while tuning the detector. Scoped to
  that folder only (not the whole `data_dir`, which holds the `claude-accounts.json` secret store).
  Unlike `clones`, it does not need `--pid host`.
- **From the Docker host** (the same symlink path resolves there, since `/proc/<pid>/root` is
  the clone's rootfs): `/var/lib/docker/volumes/rmng-data/_data/data/hosts/<id>`.
- **`docker exec`** into the control-server container and browse `data/hosts/`.

Omit `--pid host` and this feature is simply off (the server logs a one-time hint per clone);
nothing else is affected.

## SSH into clones

Every clone runs a real `sshd` on `:22` (internal `rmng` network only — never host-published).
The control-server runs a locked-down, jump-only bastion `sshd` on host port **2222**
(`listen.bastion`, see [PROTOCOL.md](PROTOCOL.md)), reusing the existing `rmng` account, that
forwards only to live clones' `:22`. There is no separate SSH login/account model to manage —
one public key list, installed on both the bastion and every clone.

**Setup (one time per key):**

1. **Settings → SSH Access** — paste a public key (or several, one per line) and save.
2. The key is installed on the bastion's `authorized_keys` and pushed to every managed clone.
   This propagates to already-**running** clones within **~10 s** (the periodic reconciler), or
   **immediately** on save (an explicit apply is triggered by the config write) — no clone
   restart needed.

**Connecting:**

- **From a clone's row in the UI**: click **Copy SSH command**, then paste into a terminal and
  hit enter.
- **From the CLI**: `rmng ssh <clone-id>` prints the same ready-to-paste command (see
  [CLI.md](CLI.md)).
- Either way the command is:

  ```sh
  ssh -J rmng@<control-host>:2222 -o StrictHostKeyChecking=accept-new rmng@<clone-id>
  ```

  `<control-host>` is the control-server's inferred public address (`ssh.publicHost`, override
  in Settings if the auto-detected host/IP is wrong); `<clone-id>` is the clone's host id (its
  Docker DNS name on the `rmng` network). `-J` jumps through the bastion on `:2222` and
  terminates at the clone's own `sshd` on `:22` — a real end-to-end SSH session, not a shell
  inside the bastion.

**It's just SSH**, so anything that takes an `ssh` command string works unchanged:

- `scp -J rmng@<control-host>:2222 <file> rmng@<clone-id>:~/` / `rsync -e 'ssh -J
  rmng@<control-host>:2222' <src> rmng@<clone-id>:~/dst`
- `GIT_SSH_COMMAND='ssh -J rmng@<control-host>:2222' git clone rmng@<clone-id>:~/repo.git`
- VS Code **Remote-SSH: Add New SSH Host…** → paste the same one-liner → connect.

**Host keys are stable** (control-server-generated and persisted, both the bastion's and each
clone's), so there is no risk of silently trusting a rotated key — neither the bastion's nor a
clone's changes under normal operation. `-o StrictHostKeyChecking=accept-new` silences the
prompt for the **clone's** key (the final hop, which the command-line option governs). OpenSSH's
`-J` does **not** apply that option to the **jump host** (the bastion), so on the very first
connection your ssh client asks you to confirm the bastion's key once (`Are you sure you want to
continue connecting?` → `yes`); every connection after that is silent. To skip even that one
prompt, pre-seed it with `ssh-keyscan -p 2222 <control-host> >> ~/.ssh/known_hosts`.

**Gotcha: clones created before the template rebuilt with `sshd` have no `sshd` to jump to.**
`ssh -J` will connect to the bastion fine but the clone hop will fail (connection refused on
`:22`) — recreate the clone from the current template to pick up `sshd`.

### Verify (manual)

This flow's real end-to-end behavior — a laptop actually reaching a clone's shell through the
bastion — needs a laptop with a matching private key and the W6800 box (CT 106) running the
rebuilt template, so it isn't exercised in CI or in this dev environment. To confirm it after a
deploy:

```bash
# 1. paste a pubkey in Settings → SSH Access, save.
# 2. create/recreate a clone from the rebuilt template.
# 3. from a laptop whose private key matches:
ssh -J rmng@<control-host>:2222 -o StrictHostKeyChecking=accept-new rmng@<clone-id> 'hostname; id'
scp -J rmng@<control-host>:2222 /etc/hostname rmng@<clone-id>:~/from-laptop
# 4. VSCode: Remote-SSH: Add New SSH Host… → paste the ssh command → connect.
# 5. add a second key in the UI; confirm `ssh` works within ~10s WITHOUT recreating the clone.
```

Expected: command 3 runs with no host-key prompt; the `scp` lands the file; VSCode connects; the
key added in step 5 works against the already-running clone without recreating it.

## Clone `/proc` limits (lxcfs)

Clones get cgroup limits (16 cpu / 32 GiB by default) but the kernel's `/proc` isn't
namespaced, so by default `free -h`/`nproc`/`htop` inside a clone report the whole host's
RAM and cores. Install **lxcfs** on the Docker host and RMNG binds its cgroup-aware `/proc`
files (`meminfo`, `cpuinfo`, `stat`, `uptime`, `loadavg`, `swaps`) over each *new* clone's,
so those tools reflect the clone's own 16-cpu / 32-GiB limits.

- **Optional, auto-detected.** RMNG probes for lxcfs at boot / on Settings → Test / at wizard
  finish and shows the result as an advisory row in the setup checklist ("LXCFS"). Without
  lxcfs, clones just keep host-wide `/proc` — everything else works.
- **Install** it on the host (on a Proxmox LXC CT the CT also needs the `fuse=1` feature — see
  [PROXMOX-LXC.md](PROXMOX-LXC.md) §1/§2b): `apt install lxcfs` (its service mounts
  `/var/lib/lxcfs/proc/*`).
- **Pick it up:** after installing, **restart the control-server (or hit Settings → Test) and
  re-create clones**. The binds are applied at clone-create time, so only clones created after
  the probe saw lxcfs get them; existing clones keep their old view until re-created. The
  binds are container config only — never baked into a committed image.
- **Load average is the one exception.** Even with lxcfs installed and its `loadavg` mask in
  place, the reported load average stays host-wide — lxcfs only virtualizes it per-cgroup with
  its non-default `-l` startup flag, which RMNG's mount doesn't pass — while `free`, `nproc`,
  and the rest of `uptime`'s output are masked and do reflect the clone's own limits.

## The image build

`docker build -t rmng:latest .` produces the **control-server image only** — it no longer
builds the clone template (no patched gnome-shell, nothing under `template/`). The Dockerfile
(BuildKit multi-stage) has two independent build stages that BuildKit runs **in parallel**,
feeding one runtime stage:

| Stage | Produces |
|---|---|
| `bun-build` | the frontend (`frontend/build/client`) + `agent-wrapper` (`bun build --compile`) |
| `rust-build` | `clone-daemon` + `control-server` (`cargo build --release`) |
| `runtime` | `ubuntu:26.04` + GStreamer/VA runtime + the payloads below |

- **Rebuilds are cached and stage-independent** — the stages share no dependencies, so a
  **source-only Rust change rebuilds only the rust layers**; the bun install layer stays
  cached.
- Building the clone **template** (patched gnome-shell + the rest of the desktop stack) is a
  separate, much longer build — `template/Dockerfile`, published via
  `scripts/publish-template.sh`; see [Publishing the template](#publishing-the-template).

**Nothing is compiled into the binary** (rust-embed is gone). The runtime image carries plain
payloads under `/usr/local/share/rmng/`:

```
/usr/local/share/rmng/clone-daemon      # hot-swapped into running clones (see Upgrades)
/usr/local/share/rmng/agent-wrapper     # hot-swapped into running clones
/usr/local/share/rmng/static/           # the frontend, served on port 2
```

The patched gnome-shell `.deb` is **not** shipped here any more — its only consumer was the
retired in-product bootstrap. The clone template still needs it: `template/Dockerfile` builds
it in its own `gnome-build` stage and installs it directly into the template's rootfs, without
ever landing under `/usr/local/share/rmng/`.

`assets.rs` reads the two payloads above at runtime with a two-entry search path: the image
install dir first, then a repo-relative **dev fallback** — `crates/control-server/embedded-bin/`
for the binaries and `frontend/build/client` for the frontend. That is what makes `cargo run -p
control-server` from a checkout work without any config (see the dev loop). A missing payload
is tolerated (WARN + fall back — e.g. no payload staged leaves the hot-swap engine idle for
that unit).

## The dev loop

The whole workspace compiles on any Linux dev box with the desktop media/GUI dev libs (the
[Prerequisites](DEVELOPMENT.md#prerequisites): GStreamer + GTK4 + PipeWire + libdrm + `clang`);
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
  host. The new image's `clone-daemon`/`agent-wrapper`/`rmng` CLI reach clones created
  after the swap (binaries are injected at create time); see [Upgrades](#upgrades).
- **`cargo run` loop** (fast rebuilds, no image): run `cargo run -p control-server` from the
  checkout on the GPU host. It runs in **dev mode** — no self-container, so it uses the `rmng`
  bridge **gateway `.1`** as its control IP and talks to the local daemon at
  `/var/run/docker.sock`. For clone provisioning to work, stage the payloads into
  `crates/control-server/embedded-bin/` (gitignored) and either `bun run build` the frontend
  (so `frontend/build/client` resolves) or run `bun run dev`. `config.json` + `data/` are
  CWD-relative. The expected hashes are pinned once at server start (see the dev caveat in
  [Upgrades](#upgrades)) — restart the dev server after restaging `embedded-bin/`.

Then, from the dashboard: pull a template (`POST /api/images/pull`), clone from it
(`POST /api/clone`), select the clone, and point the viewer at the host. After a
`clone-daemon` / `agent-wrapper` change, restage `embedded-bin/` and restart the dev server —
the hot-swap engine picks up every existing clone on its next sweep/`Hello`, no manual step.

## Networking & the media socket

- **`rmng` bridge**: a user-defined bridge with the subnet from `docker.subnet`, created
  lazily at wizard finish and before each clone. Addressing is Docker's embedded DNS, not
  static IPs: every clone resolves by its container name (== host id), and the
  control-server attaches itself under the `rmng-control` alias (so recreating its container
  never strands the baked `RMNG_CONTROL_URL`s). Clone IPs are plain Docker IPAM — nothing
  allocates or stores them. If an `rmng` network already exists with a **different** subnet,
  `ensure_network` errors — delete it with `docker network rm rmng` and re-run setup.
- **Clone media socket**: clone-daemon ships dmabuf frames to the control-server over a
  `SOCK_SEQPACKET` unix socket (fds via `SCM_RIGHTS`), *not* the network. The shared
  `rmng-sock` named volume is mounted into the control-server and every clone at the same path
  `/srv/rmng-sock`; the server `chmod 0777`s the socket so a different-uid clone-daemon can
  connect. See [PROTOCOL.md](PROTOCOL.md).

## Patched gnome-shell

The clone-daemon needs two gnome-shell patches: **shell-01** (hide the screen-sharing pill
that would otherwise composite into captured frames) and **shell-03** (enable
`org.gnome.Shell.Eval` for the window-management MCP tools). `template/Dockerfile`'s
`gnome-build` stage builds the patched `gnome-shell_*+ngshell1` `.deb` (rebuilding only
`libshell-<N>.so` and repacking the stock deb); `template/setup/15-gnome-patch.sh` `dpkg -i`s
it over stock **during the template build** — every clone created from the published template
inherits it (there's no per-install control-server payload any more; see
[Publishing the template](#publishing-the-template)). Details + verification:
[gnome-patch/README.md](../gnome-patch/README.md).

## Day-2 operations (from the dashboard / API / `rmng` CLI)

- **Clone**: `POST /api/clone` — Linear ticket / new ticket / plain, from a chosen image. The
  new clone is always brought to `config.effective_monitors()` (the active layout preset, or
  the built-in default when no presets exist) as soon as its clone-daemon connects — the
  control-server pushes the active layout via `SetMonitors` on the daemon's first `Hello`, so
  the template's baked-in `RMNG_MONITORS` boot value is corrected immediately and never
  actually persists.
- **Pull a template**: `POST /api/images/pull {reference?}` — from the Images panel, any
  time (not just first-run setup).
- **Commit a clone → image**: `POST /api/images/commit {host, name}`.
- **Activate a layout preset** on already-running clones: `POST /api/layout/activate {name}`
  — pushes `ServerMsg::SetMonitors` to every connected clone-daemon, which live-swaps to a
  fresh Mutter session with the new monitors (make-before-break — no GNOME restart, no app
  loss).
- **Hot-swap a Claude account**: `POST /api/claude/swap {host, account}` — writes the clone's
  `~/.claude/.credentials.json` live via `docker exec`.
- **Delete**: `POST /api/delete {id}` (stops + removes the container and its
  `rmng-dind-<id>` volume; an unmanaged row is just unregistered).

Most of these are also scriptable from any clone via the `rmng` CLI ([CLI.md](CLI.md)).
Clone binaries (`clone-daemon`/`agent-wrapper`/the `rmng` CLI) are **not** a day-2 op — the
control-server installs them at clone-create time; see [Upgrades](#upgrades).

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
6. **Docker Hub pull rate limits** surface verbatim: in the wizard's/Images panel's
   template-pull log (`POST /api/images/pull`) for `pegasis0/rmng-template:*`, or in a manual
   `docker pull ubuntu:26.04` while *building* a template (`scripts/publish-template.sh`).
