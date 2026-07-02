# Build & deploy

The whole stack ships as **one self-contained `control-server` binary** that provisions
clones itself. The deploy flow is three commands; everything else (templates, clones,
redeploys, monitor layouts) is driven from the running server's dashboard/API.

> **Use the Ubuntu 26.04 CT template.** 24.04's older mesa offers a different DRM modifier
> than the capture path expects → `no more input formats`. The default base image is
> `ubuntu-26.04-standard_26.04-1`.

## The three commands

```sh
# 1. Build/dev CT — full toolchain + headless GNOME + render passthrough; builds the
#    self-contained control-server (frontend + clone-daemon + agent-wrapper + patched
#    gnome-shell deb all embedded).
./scripts/provision-build-ct.sh   root@<proxmox>            # → rmng-build

# 2. Deploy CT — runtime libs only; copies the ONE binary, writes minimal config (just the
#    Proxmox SSH target), generates + authorizes the orchestration SSH key, starts the unit.
./scripts/provision-deploy-ct.sh  root@<proxmox>            # → rmng-control, http://<ip>:9000

# 3. Clones are provisioned BY the running control-server. Bootstrap the golden template
#    once, then CoW-clone from it (web UI or API).
curl -X POST http://<deploy-ip>:9000/api/template/bootstrap -d '{"hostname":"rmng-template"}'
```

Then open `http://<deploy-ip>:9000` → **Settings** to enter Linear keys, Claude accounts,
clone-account tokens, template build params, monitor defaults, and the listen ports — the
provision seeds only the Proxmox SSH target + the orchestration key. Secrets are write-only
and redacted on read. See [SCRIPTS.md](SCRIPTS.md) for each script's args/env.

## The dev loop

**Staging vs. production, and build vs. run.** `provision-build-ct.sh` makes the build CT a
**staging control-server**: the *same* runtime as the production deploy CT (it runs
`cs-deploy-ct.sh`, orchestrating **real Proxmox clones**), plus the build toolchain so you can
rebuild + restart in place. The control-server CT does **not** run GNOME/capture itself — the
**clones** do. As for crates: the *entire* workspace compiles on any Linux dev box with the
desktop media/GUI dev libs (the [Prerequisites](../README.md#prerequisites): GStreamer + GTK4 +
PipeWire + libdrm + `clang`); a bare laptop without them builds only `wire`. What needs a GPU is
*running* the pipeline — the control-server's VA-API **encode** (staging/deploy CT) and the
**capture** side (PipeWire `RecordVirtual`, the pinned W6800 DRM modifier, headless GNOME) which
runs **on each clone**. The **`viewer` is the exception: it builds *and* runs locally**
(client-side VA-API **decode** only — Intel iGPU decode is validated against AMD-encoded streams).

### Local (on your laptop)

Everything below runs entirely on your machine — no CT, no rsync (`<staging-ip>` = the build CT):

| You changed | Build & run locally | See the result |
|---|---|---|
| **`viewer`** (decode / render / input) | `RMNG_VIDEO=<staging-ip>:9001 cargo run -p viewer` | GUI window streaming the staging CT's *selected clone* |
| **`viewer`**, no display | `RMNG_VIDEO=<staging-ip>:9001 RMNG_DUMP=frame.png cargo run -p viewer -- --headless` | per-monitor fps in the logs; `frame.png` = one decoded frame |
| **frontend** (React UI) | `cd frontend && bun run dev` | Vite dev server + HMR; proxies `/api` + `/events` to a running backend |
| **`wire`** types / DTOs | `cargo test -p wire` | compiles + regenerates the frontend's ts-rs types |
| pure logic in **any** crate | `cargo build -p <crate>` · `cargo test -p <crate>` | the whole workspace *compiles* locally, so the compiler + unit tests are a local loop |
| **`control-server` / `media` / `clone-daemon`** runtime behavior | builds locally, but must **run** on the staging CT ↓ | — |

### On the staging control-server CT

The loop is *edit locally → rebuild on the staging CT → restart the unit → drive real clones*:

```sh
rsync -az --exclude target --exclude frontend/node_modules ./ root@<staging-ip>:/root/RMNG/
# Re-runs the build (re-embeds clone-daemon/agent-wrapper/frontend) + installs the binary:
ssh root@<staging-ip> 'bash /root/RMNG/scripts/cs-build-ct.sh && systemctl restart rmng-control-server'
cargo test -p wire -p control-server                               # ~42 tests, on the CT
```

For a control-server-only change, skip the re-embed:
```sh
ssh root@<staging-ip> 'cd /root/RMNG && cargo build --release -p control-server \
  && install -m755 target/release/rmng-control-server /usr/local/bin/ && systemctl restart rmng-control-server'
```

- **Clones** are real CTs the control-server provisions (`POST /api/template/bootstrap`, then
  `POST /api/clone`); each runs headless GNOME + `clone-daemon` + `agent-wrapper` and connects
  back over the `/srv/rmng-sock` media socket. Select one in the dashboard; the viewer streams it.
- **Redeploy a clone's binaries** after a `clone-daemon`/`agent-wrapper` change without
  reprovisioning (~10 s): `POST /api/clone/redeploy {id, daemonOnly?}`.
- **Decode driver:** `RMNG_VIDEO=<staging-ip>:9001 rmng-viewer --headless` logs per-monitor fps;
  add `RMNG_DUMP=frame.png` to dump one decoded frame.

## The self-contained binary (embed)

`control-server` (~50 MB) carries, via `rust-embed` + `flate2`:
- the **frontend** (`frontend/build/client`),
- **`clone-daemon`** gzipped,
- **`agent-wrapper`** gzipped (`bun build --compile` single-exec of the Claude Agent SDK service),
- the patched **gnome-shell `.deb`** gzipped (`gnome-shell-deb.gz`).

`cs-build-ct.sh` stages all four into `crates/control-server/embedded-bin/` **before**
building control-server. At provision time `orchestrate.rs` decompresses each → temp file →
`scp` to the node → `bootstrap.sh` `pct push`es them into the new CT → `provision-clone.sh`
installs them. A plain `cargo build` with an empty `embedded-bin/` still works — it just
carries nothing (orchestration falls back to `RMNG_*_BIN` on-disk paths). A clone needs
only the standalone `claude` CLI at runtime.

## Patched gnome-shell

The clone-daemon needs two gnome-shell patches: **shell-01** (hide the screen-sharing pill
that would otherwise show in captured frames) and **shell-03** (enable `org.gnome.Shell.Eval`
for the window-management MCP tools). The build CT builds a patched `gnome-shell_*+ngshell1`
`.deb` (rebuilding only `libshell-<N>.so` and repacking the stock deb), it's embedded in the
control-server, pushed at template bootstrap, and `provision-clone.sh` installs it over stock.
CoW clones inherit it from the template. Details + verification:
[gnome-patch/README.md](../gnome-patch/README.md).

## Shared media socket (cross-CT dmabuf)

clone-daemon ships dmabuf frames to the control-server over a `SOCK_SEQPACKET` unix socket
(fds via `SCM_RIGHTS`). It's a host dir **bind-mounted into every CT** — the deploy CT + every
clone — at the **same path `/srv/rmng-sock`** (NOT under `/run`: the CT's tmpfs shadows a `/run`
mount). The control-server `chmod 0777`s the socket so a different-uid clone-daemon can
connect.

## Day-2 operations (from the dashboard/API/fleet MCP)

- **Clone:** `POST /api/clone` (CoW from the template) — Linear ticket / new ticket / plain.
- **Redeploy binaries** (no reprovision, ~10 s): `POST /api/clone/redeploy {id, daemonOnly?}`
  or the fleet MCP `redeploy` tool. `daemonOnly` keeps the Claude session alive.
- **Apply monitor layout** to running clones: `POST /api/monitors/apply` (rewrites each
  clone's `RMNG_MONITORS` + restarts its GNOME/daemon).
- **Hot-swap a Claude account:** `POST /api/claude/swap {host, account}` — writes the clone's
  `~/.claude/.credentials.json` live.
- **Delete:** `POST /api/delete {id}`.

## Production cutover

Deploy the 4-port Rust control-server and retire the old g-r-d/Bun stack (CT 101 +
`pega-*` clones, see [INFRA.md](INFRA.md)):

1. **Deploy** the control-server (the three commands above), enter Linear/Claude settings,
   and import Claude accounts from a signed-in clone.
2. Clone provisioning is already the RMNG fork (`provision-clone.sh`); the agent-wrapper's
   `desktop` MCP already points at the in-clone daemon MCP (`http://127.0.0.1:9004`).
3. **Retire** the old native RDP client (`../core`/`../gtk`/`../headless`), the `../computer-use`
   stdio MCP + binary, `control-server-ctl`, and the Bun `../control-server`.
4. **Soak + rollback:** run both stacks briefly; keep the Bun backend reversible until the Rust
   stack is confirmed. Then delete the `../computer-use` crate (its capabilities now live in
   `clone-daemon`).

First runs create real CTs + a ~10-min build — treat as operator-supervised. A few acceptance
checks need a physical display or an on-subnet clone: the native viewer GUI render + game-input
feel (pointer-lock/keycode/F11), and window-mgmt + the needs-human detector against a patched,
on-subnet clone (the inference CT `10.60.0.10:8080` is unreachable from the build CT).

## Gotchas (hard-won during the first full E2E, 2026-06-24)

These are baked into the scripts now; listed so they aren't re-discovered:

1. **`gstreamer1.0-va` is not a package** on 24.04/26.04 — the `va` elements
   (`vah264enc`/`vapostproc`) live in **`gstreamer1.0-plugins-bad`**; `gstreamer1.0-vaapi` is
   the unrelated legacy plugin.
2. **`/run/*` bind-mounts are shadowed** by the CT's `/run` tmpfs → the media socket lives at
   `/srv/rmng-sock`, mounted at the same path in every CT, `chmod 0777`.
3. **dmabuf modifier is pinned** to the W6800 tiled modifier validated on 26.04's mesa → use
   the 26.04 template (proper PipeWire modifier negotiation is a tracked follow-up).
4. **clone units auto-start via direct `default.target.wants` symlinks** + an explicit
   `systemctl --user start` — `systemctl --user enable` is unreliable mid-provision (the user
   manager comes up at `enable-linger`, before the units exist).
5. The clone-daemon needs **`RMNG_SOCKET`** in its unit or it silently runs the capture
   self-test and never connects.

## Known follow-ups

- Replace the hardcoded DRM modifier with PipeWire modifier negotiation (un-pin from 26.04).
- agent-wrapper `bun --compile` warns `could not read ticket-procedure.md` — non-fatal (system
  prompt is injected from code; the file is bundled in the agent-wrapper dir).
- Retire the `../computer-use` crate once the detector port is live-verified on a real clone
  (its capabilities are now in `clone-daemon`).
