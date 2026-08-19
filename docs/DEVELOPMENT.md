# RMNG — Architecture & Development

Architecture, the port/protocol map, the workspace layout, and the build prerequisites.
For running the hub, see the [Quick start](../README.md#quick-start); for the full build →
run → wizard → upgrade flow, see [DEPLOY.md](DEPLOY.md).

## The shape

The control-server exposes video, web/API, forward, SSH, and SMB surfaces; the desktop automation MCP lives inside each clone.

| Port | Default | Transport | Purpose |
|---|---|---|---|
| **1 — video** | `9001` | framed H.264 over TCP | the selected clone's monitors to the native GTK viewer, with input + clipboard + cursor back |
| **2 — web API** | `9000` | HTTP + SSE (+ embedded frontend) | the React management UI: clone selection, clone/Linear/Claude/chat orchestration, settings; also the `rmng desktop`/`rmng exec` proxy endpoints |
| **4 — forward** | `9005` | framed TCP over TCP | the viewer's port-forwarding data plane: one TCP connection per accepted local socket, spliced to the clone |
| **SMB** | `445` | SMB (smbd) | two shares, fixed cred `rmng`/`rmng`: `clones` browses every running clone's `/home/rmng`, and `shared` is the one pool mounted into every clone at `/home/rmng/shared` |
| daemon MCP | `9004` | HTTP JSON-RPC (in each clone) | the full desktop-automation surface; the agent calls it on localhost, the `rmng desktop` CLI proxies to it (operator/fleet desktop control) |
| clone socket | `/srv/rmng-sock/clones.sock` | unix `SOCK_SEQPACKET` | clone-daemon ⇄ control-server: dmabuf frames (`SCM_RIGHTS`) out, input/clipboard in |

**Design in one breath:** one central encoder, thin clones (no g-r-d / GDM / RDP — just
`gnome-session` + the daemon); one capture feeds both the human viewer and the agent's
screenshots; raw H.264-over-TCP into zero-copy VA-API decode gives RFX-class feel without RDP;
media/input cross a host unix socket, not the network, so only the control-server is externally
reachable. `docker run` the control-server container, open the browser, and the first-run
setup wizard pulls the pre-built clone **template** — a separate published image
(`pegasis0/rmng-template`) that already carries the patched gnome-shell — and finishes setup.
Clone binaries (and the `rmng` CLI) are injected into each clone at create time from the
running server's payloads; there's no manual redeploy step.

## Documentation

| Doc | Covers |
|---|---|
| [API.md](API.md) | Every HTTP endpoint on the web port (9000), incl. the SSE streams |
| [MCP.md](MCP.md) | Clone-local daemon MCP on 9004: JSON-RPC envelope, tools, and curl examples |
| [CLI.md](CLI.md) | The `rmng` CLI (`/usr/local/bin/rmng` in every clone): every subcommand incl. `desktop`/`exec`, `--json`, exit codes, wait semantics |
| [PROTOCOL.md](PROTOCOL.md) | The port-1 video/input/clipboard/cursor wire protocol, the clone socket, the config schema, every env var, the clone-daemon CLI, and the per-crate public API |
| [SCRIPTS.md](SCRIPTS.md) | Every script: what it does, where it runs, its args, and what invokes it |
| [DEPLOY.md](DEPLOY.md) | The Docker build → run → wizard → images/clones flow, the image build, upgrades, clone-home browsing, and the dev loop |
| [PROXMOX-LXC.md](PROXMOX-LXC.md) | Running the Docker host on an unprivileged Proxmox LXC CT (one hosting option) |

## Workspace map

| Path | Kind | What |
|---|---|---|
| [crates/wire](../crates/wire/README.md) | lib | shared types: control state, config, the clone socket + viewer protocols, MCP DTOs; ts-rs export for the frontend |
| [crates/control-server](../crates/control-server/README.md) | bin | the server: media plane, web API/SSE, passive token accounting, Docker lifecycle, port-forward + SMB planes, and clone payloads |
| [crates/media](../crates/media/README.md) | lib | dmabuf ingest → VA-API H.264 per monitor + dmabuf→JPEG screenshots + the clone-socket transport |
| [crates/clone-daemon](../crates/clone-daemon/README.md) | bin | the thin in-clone pipe: RecordVirtual capture, RemoteDesktop input injection, clipboard bridge, and desktop MCP (:9004) |
| [crates/viewer](../crates/viewer/README.md) | bin | the native GTK client (GUI + headless test mode): zero-copy VA-API decode, multi-monitor, client-drawn cursor, input + pointer-lock + clipboard |
| [crates/control-client](../crates/control-client/README.md) | lib | typed reqwest+SSE client for the port-2 web API (`/api/state`, `/events`, clone/delete/image/account wrappers); used by the `rmng` CLI and integration tests |
| [crates/cli](../crates/cli/README.md) | bin | the `rmng` fleet CLI: clones/images/accounts/operations over the port-2 web API; injected into every clone as `/usr/local/bin/rmng` |
| [frontend](../frontend/README.md) | web app | React Router 7 management UI, ts-rs types from `wire`, served by the control-server |
| [gnome-patch](../gnome-patch/README.md) | tooling | builds the patched gnome-shell `.deb` (hide screen-share indicator + enable `Eval` for window-mgmt); built + installed by `template/Dockerfile`'s `gnome-build` stage into the published clone template — not a control-server payload |

The per-clone **agent-wrapper** (Bun, Claude Agent SDK) is vendored at `agent-wrapper/`; the
control-server installs its current build into each clone at create time (the template
doesn't carry it) and proxies chat to it. Its `desktop` MCP points at the clone-daemon
(`http://127.0.0.1:9004`).

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
*not* `gstreamer1.0-va`) + **`gstreamer1.0-gtk4`** (the `gtk4paintablesink` the viewer renders
into — without it the viewer aborts with `no element "gtk4paintablesink"` on the first video
frame), GTK4; a GPU render node (`/dev/dri/renderD128`) on the control-server host *and* every
clone. With those dev libs the **whole workspace compiles on a plain laptop**
(a bare box without them builds only `wire`); the GPU box is only needed to *run* the
capture/encode/server side — the **`viewer` builds *and* runs locally** (client-side decode).
See the [dev loop](DEPLOY.md#the-dev-loop). **The clone template is built on the
`ubuntu:26.04` base OS** (the patched gnome-shell is compiled against 26.04's GNOME only) —
see [Publishing the template](DEPLOY.md#publishing-the-template).

<a id="macos"></a>
### macOS (Apple Silicon) — viewer only

Only the **viewer** builds and runs on macOS; the capture/encode/server side is Linux-only by
design. Verified on macOS 26.4 / Apple M-series with Homebrew:

```sh
brew install gtk4 gstreamer pkgconf     # verified: gtk4 4.22.4, gstreamer 1.28.4, pkgconf 2.5.1
cargo build -p viewer --release         # → target/release/rmng-viewer
```

**Build the viewer package, not the workspace root.** `cargo build` / `check` / `test` / `clippy`
at the root fail on macOS inside `libspa-sys`: `clone-daemon` depends on pipewire
unconditionally, and the workspace declares no `default-members`. Everything you need on a Mac is
`-p viewer` (add `-p rmng-cli -p control-client` if you want the `rmng` CLI, which builds clean
and is HTTP-only — note the package is `rmng-cli`, not `cli`, which is only the directory name).

**Never mix GTK providers.** The official `GStreamer.framework` `.pkg` bundles *its own* GTK4 for
the gtk4 plugin. Use Homebrew for **both** GStreamer and GTK4 (one GTK in the process) or the
framework for both — mixing them produces link-time and runtime chaos. No environment variables
are needed with an all-Homebrew stack at the default `/opt/homebrew` prefix.

Sanity-check the stack before debugging any video problem:

```sh
gst-inspect-1.0 vtdec_hw            # VideoToolbox HW decoder (applemedia)
gst-inspect-1.0 gtk4paintablesink   # GL zero-copy sink
cargo run -p viewer --release -- --glunpack-validate 256 144   # expect max abs err 0
```

**macOS input notes.** **Cmd and Control are swapped on the wire by default**, so Mac chords
(Cmd+C, Cmd+T) reach the remote GNOME session as Ctrl and physical Control produces Super —
disable with `RMNG_CMD_IS_CTRL=0` or `"cmd_is_ctrl": false` in
`~/.config/rmng-viewer/config.json`. The F-row needs `fn` on a default MacBook (or turn on "Use
F1, F2 etc. as standard function keys"). **Cmd-Tab and Cmd-Space cannot be forwarded** — the
Wayland `inhibit_system_shortcuts` protocol that `grab_keys()` uses does not exist on macOS, so
that call is a silent no-op there; capturing those would need a permission-gated `CGEventTap`,
which the viewer deliberately avoids (an `NSEvent` local monitor needs no Input Monitoring grant).
`RMNG_NO_POINTER_LOCK=1` disables pointer lock entirely.

<a id="windows"></a>
### Windows — viewer only

As on macOS, only the **viewer** builds and runs; the capture/encode/server side is Linux-only by
design. The toolchain is **MSYS2 MINGW64 + the `x86_64-pc-windows-gnu` Rust target**, because
that is the only combination with prebuilt GTK4 *and* GStreamer packages. (The MSVC route works
too, but GTK4 has no MSVC binary distribution — `gvsbuild` compiles the whole stack from source,
which takes hours and buys nothing here.)

**Use MINGW64, not UCRT64.** Rust's `x86_64-pc-windows-gnu` target links against msvcrt, which
is what the `mingw-w64-x86_64-*` packages are built for; the UCRT64 environment pairs a different
C runtime into the same process.

```sh
# 1. MSYS2 from https://www.msys2.org, then in the MSYS2 shell:
pacman -Syu                             # run twice; the first pass updates the core runtime
pacman -S --needed \
  mingw-w64-x86_64-toolchain mingw-w64-x86_64-pkgconf \
  mingw-w64-x86_64-gtk4 \
  mingw-w64-x86_64-gstreamer mingw-w64-x86_64-gst-plugins-base \
  mingw-w64-x86_64-gst-plugins-good mingw-w64-x86_64-gst-plugins-bad \
  mingw-w64-x86_64-gst-plugins-rs mingw-w64-x86_64-gst-libav

# 2. Rust, targeting the GNU ABI so it matches the mingw-w64 libraries above:
rustup-init.exe -y --default-host x86_64-pc-windows-gnu

# 3. Build. /mingw64/bin must be on PATH for pkg-config to find the .pc files
#    AND for the linker to find the import libraries.
export PATH="/mingw64/bin:$PATH"
cargo build -p viewer --release         # → target/release/rmng-viewer.exe
```

**Build the viewer package, not the workspace root** — same reason as macOS: `clone-daemon`
depends on pipewire unconditionally and the workspace declares no `default-members`. Add
`-p rmng-cli -p control-client` for the `rmng` CLI, which is HTTP-only and builds clean.

**`rmng-viewer.exe` needs `mingw64\bin` on `PATH` at run time**, not just at build time — GTK4,
GStreamer and their plugins are DLLs resolved by the loader. Launching from the MSYS2 MINGW64
shell handles this; a shortcut or a copy dropped elsewhere will fail to start until `PATH`
includes `C:\msys64\mingw64\bin` (adjust for your install root).

Sanity-check the stack before debugging any video problem:

```sh
gst-inspect-1.0 d3d11h264dec        # D3D11VA HW decoder — absent on VMs and some remote sessions
gst-inspect-1.0 gtk4paintablesink   # GL zero-copy sink (gst-plugins-rs)
cargo run -p viewer --release -- --glunpack-validate 256 144   # expect max abs err 0
```

`d3d11h264dec` being absent is **not** fatal: the viewer picks the first registered decoder from
`d3d11h264dec`, `avdec_h264`, `openh264dec` and logs which it got at startup
(`windows H.264 decoder: …`). Only software decode is then in play, so expect higher CPU and
lower frame rates on large monitors.

**Windows input notes.** The server address lives in
`%APPDATA%\rmng-viewer\config.json` (not `~/.config`, which on Windows would resolve relative to
the working directory). Physical keys are recovered from the Win32 virtual key by inverting it to
a set-1 scancode, so **non-US layouts send the correct physical position**; the one exception is
the keypad `Enter`, which arrives as `KEY_ENTER`. **`Super` and `Alt+Tab` cannot be forwarded** —
`grab_keys()` uses the Wayland `inhibit_system_shortcuts` protocol, which GDK does not implement
on Windows, so the local Start menu and task switcher win; capturing them would need a
`WH_KEYBOARD_LL` hook. Pointer lock uses `ClipCursor` plus Raw Input and gives **unaccelerated**
deltas (better than macOS, which can only offer accelerated ones); `RMNG_NO_POINTER_LOCK=1`
disables it. If the local cursor ever stays pinned after a crash, any `ClipCursor` owner exiting
releases it — logging out is never required.
