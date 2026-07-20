# syntax=docker/dockerfile:1
#
# RMNG control-server image (BuildKit multi-stage). `docker build -t rmng:latest .` is
# the canonical build — it replaces the retired build CT. Everything (rust toolchain,
# gstreamer/libva/libdrm/pipewire dev deps, bun) lives in build stages; the runtime stage
# carries only the GStreamer/VA runtime the server needs to ingest clone dmabufs and
# VA-API encode. The W6800 GPU is a RUNTIME requirement, not a build one.
#
# Nothing is compiled into the server binary (rust-embed is gone): the runtime stage
# assembles /usr/local/share/rmng/ — the plain clone-daemon + agent-wrapper binaries and
# static/ (the frontend) — which assets.rs/web.rs read at runtime. (The patched
# gnome-shell .deb is no longer a control-server payload: the retired in-product bootstrap
# was its only consumer; the clone template — which needs it — is now built by
# template/Dockerfile.) Payloads are stored UNcompressed (no gzip: it only ever existed to
# keep the rust-embed blob small; registry pushes compress layers anyway). The two build
# stages are fully independent, so BuildKit runs them in parallel and a source-only rust
# change rebuilds only the rust layers.
#
# Stages:
#   1. bun-build   — frontend (react-router → frontend/build/client) + agent-wrapper
#                    (bun build --compile).
#   2. rust-build  — rustup stable; dev deps; cargo build --release clone-daemon + rmng-cli
#                    + control-server.
#   3. go-build    — cliproxy-sidecar: one CLIProxyAPI (v7) instance per account group; the
#                    control-server spawns/supervises these (see cliproxy.rs). Independent of
#                    1/2, so BuildKit runs all three in parallel.
#   4. runtime     — ubuntu:26.04, runtime libs + samba (smbd serves clone homes over SMB)
#                    + /usr/local/bin/cliproxy-sidecar + /usr/local/share/rmng payloads
#                    (2 binaries + static/), a local rmng uid-1000 user for the share,
#                    WORKDIR /data, EXPOSE 9000-9003 9005 445.

# ---------------------------------------------------------------------------------------
# 1. bun stage: frontend build + agent-wrapper bun --compile
# ---------------------------------------------------------------------------------------
FROM oven/bun:1 AS bun-build
WORKDIR /src

# Frontend (react-router build → frontend/build/client). Copy manifest+lock first so the
# install layer caches across source-only edits.
COPY frontend/package.json frontend/bun.lock ./frontend/
RUN cd frontend && bun install --frozen-lockfile
COPY frontend/ ./frontend/
RUN cd frontend && bun run build

# agent-wrapper: bun build --compile a single self-contained binary (the control-server
# installs it into each clone during provisioning).
COPY agent-wrapper/package.json agent-wrapper/bun.lock ./agent-wrapper/
RUN cd agent-wrapper && bun install --frozen-lockfile
COPY agent-wrapper/ ./agent-wrapper/
RUN cd agent-wrapper \
 && bun build --compile src/server.ts --outfile /tmp/agent-wrapper

# ---------------------------------------------------------------------------------------
# 2. rust build stage — binaries only (no asset staging; fully parallel with 1)
# ---------------------------------------------------------------------------------------
FROM ubuntu:26.04 AS rust-build
ENV DEBIAN_FRONTEND=noninteractive
# Dev deps mined from scripts/cs-build-ct.sh (build toolchain) minus the SSH bits (no
# sshfs/openssh; the Docker port dials the local daemon over a unix socket). Package
# rationale:
#   build-essential pkg-config clang  — cc/linker + pkg-config for the *-sys crates
#   curl ca-certificates              — rustup installer + TLS
#   libgstreamer1.0-dev
#   libgstreamer-plugins-base1.0-dev  — ships gstreamer-gl-1.0.pc (media crate's
#                                       gstreamer-gl) + app/video/allocators .pc files
#   libva-dev libdrm-dev              — VA-API encode + dmabuf (media)
#   libpipewire-0.3-dev               — clone-daemon raw PipeWire capture
#   libgtk-4-dev                      — pulled in transitively by the workspace toolchain
#                                       (viewer); harmless here, matches the old build CT
RUN apt-get update \
 && apt-get install -y --no-install-recommends \
      build-essential pkg-config clang git curl ca-certificates \
      libgstreamer1.0-dev libgstreamer-plugins-base1.0-dev \
      libva-dev libdrm-dev libpipewire-0.3-dev libgtk-4-dev \
 && rm -rf /var/lib/apt/lists/*

# rustup stable (workspace is edition 2024 / rust-version 1.85 — apt rustc is too old).
ENV RUSTUP_HOME=/usr/local/rustup CARGO_HOME=/usr/local/cargo PATH=/usr/local/cargo/bin:$PATH
RUN curl -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal --default-toolchain stable

WORKDIR /src
# Copy the whole build context (crate sources incl. crates/control-server/scripts/ which
# are include_str!'d, plus Cargo.toml/Cargo.lock). .dockerignore keeps target/, node_modules,
# frontend/build and the retired root /scripts/ out.
COPY . .

# Release-build the binaries in one cache-mounted RUN (shared dep graph compiles once).
# The `target` cache is a mount, so outputs must be copied OUT in the same RUN (cache
# mounts don't persist into the image layer). clone-daemon + the rmng CLI are payloads
# (the server pushes them into clones); the control-server binary is the image entrypoint.
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/src/target \
    cargo build --release -p clone-daemon -p control-server -p rmng-cli \
 && mkdir -p /out \
 && cp target/release/rmng-clone-daemon /out/clone-daemon \
 && cp target/release/rmng /out/rmng-cli \
 && cp target/release/rmng-control-server /out/rmng-control-server

# ---------------------------------------------------------------------------------------
# 3. go build stage — cliproxy-sidecar (fully parallel with 1/2)
# ---------------------------------------------------------------------------------------
# One CLIProxyAPI service instance per account group. The control-server writes each
# instance's config.yaml + auth-dir and spawns this binary per group (crates/control-server/
# src/cliproxy.rs), routing clone traffic to it. Static (CGO off) so it drops straight into
# the runtime stage; upstream TLS uses the runtime's ca-certificates.
FROM golang:1.26 AS go-build
WORKDIR /src/cliproxy-sidecar
# Manifest first so the module-download layer caches across source-only edits.
COPY cliproxy-sidecar/go.mod cliproxy-sidecar/go.sum ./
RUN go mod download
COPY cliproxy-sidecar/ ./
RUN CGO_ENABLED=0 go build -trimpath -ldflags="-s -w" -o /out/cliproxy-sidecar .

# ---------------------------------------------------------------------------------------
# 4. runtime stage
# ---------------------------------------------------------------------------------------
FROM ubuntu:26.04 AS runtime
ENV DEBIAN_FRONTEND=noninteractive
# Runtime deps mined from scripts/cs-deploy-ct.sh. Still no openssh-client/sshfs: the Docker
# port dials the local daemon over a unix socket — no SSH anywhere for control-plane calls.
# Clone homes are instead served over SMB by samba's smbd (see smb.rs), so `samba` is added
# here — it provides smbd + smbpasswd and the vfs_fruit/catia modules smb.conf loads.
# `openssh-server` IS added, though: it provides `/usr/sbin/sshd` + `ssh-keygen`, which the
# bastion supervisor (see ssh.rs) needs to run its own jump-only `sshd` on :2222 and to mint
# host keys for itself and every clone. vah264enc/vapostproc live in the
# `va` plugin shipped by gstreamer1.0-plugins-bad; pngenc (screenshots) in -good. The
# zero-copy GL→VA AVC444 encode bridge needs `glupload` (+ the rest of the GL elements) from
# libgstopengl.so, which ships in the SEPARATE gstreamer1.0-gl package — omit it and the
# encoder dies at init with `no element "glupload"` and every viewer hangs on "connected,
# waiting for video". (The mesa EGL/GBM/gallium userspace the GL context links is already
# pulled in by va-driver-all, so the plugin package is the only missing piece.)
RUN apt-get update \
 && apt-get install -y --no-install-recommends \
      gstreamer1.0-plugins-base gstreamer1.0-plugins-good gstreamer1.0-plugins-bad \
      gstreamer1.0-gl \
      libva2 libva-drm2 va-driver-all libdrm2 \
      ca-certificates samba openssh-server \
 && rm -rf /var/lib/apt/lists/*

# Local `rmng` account at uid/gid 1000 for the SMB share. `force user = rmng` in smb.conf
# (see smb.rs) maps every SMB session to this uid, and it must equal the clone's rmng uid
# (1000) so files created through the share land with the right owner. The ubuntu:26.04 base
# ships a default `ubuntu` user at uid 1000, so free that uid first — delete whoever holds it
# (the getent guard makes this a no-op if a future base drops the default user, so the build
# never fails on its absence; deleting the user also releases its private group at gid 1000).
# `-M` (no home) + `/usr/sbin/nologin`: an SMB-only account, never an interactive login.
# smb.rs re-runs the same groupadd/useradd idempotently at boot; baking it here pins the uid
# so that boot-time provisioning is a harmless no-op.
RUN if getent passwd 1000 >/dev/null; then userdel -r "$(getent passwd 1000 | cut -d: -f1)" 2>/dev/null || true; fi \
 && groupadd -g 1000 rmng \
 && useradd -u 1000 -g 1000 -M -s /usr/sbin/nologin rmng

# Version stamp for the in-product self-update UI. Passed by scripts/publish-server.sh
# (--build-arg); a plain `docker build` with no args leaves them empty → the UI shows a
# "dev build". These are the only place the running server learns its own version.
ARG GIT_SHA=""
ARG BUILD_DATE=""
LABEL org.opencontainers.image.revision="$GIT_SHA" \
      org.opencontainers.image.created="$BUILD_DATE" \
      org.opencontainers.image.version="$GIT_SHA"

COPY --from=rust-build /out/rmng-control-server /usr/local/bin/rmng-control-server

# Per-group inference proxy (one process spawned per account group by cliproxy.rs).
COPY --from=go-build   /out/cliproxy-sidecar    /usr/local/bin/cliproxy-sidecar

# Payloads + frontend on the image filesystem, stored PLAIN (assets.rs / web.rs read
# these). The control-server injects clone-daemon / agent-wrapper (→ /opt/rmng/bin) and
# the rmng CLI (→ /usr/local/bin) into every clone at CREATE time — the sole delivery
# path since the binswap hot-swap engine was retired; the clone template itself
# (template/Dockerfile) carries none of them.
COPY --from=rust-build  /out/clone-daemon               /usr/local/share/rmng/clone-daemon
COPY --from=rust-build  /out/rmng-cli                   /usr/local/share/rmng/rmng-cli
COPY --from=bun-build   /tmp/agent-wrapper              /usr/local/share/rmng/agent-wrapper
COPY --from=bun-build   /src/frontend/build/client      /usr/local/share/rmng/static

# CWD-relative config.json + data/ land in the /data volume (config.rs uses relative paths).
WORKDIR /data
# 9000 web/API, 9001 video, 9002 per-clone MCP, 9003 global MCP, 9005 forward, 445 SMB (clone homes).
EXPOSE 9000-9003 9005 445
# Logging default only (not a setting — no config lives in env, per the no-env invariant).
ENV RUST_LOG=info,tower_http=warn,clip=debug
ENTRYPOINT ["/usr/local/bin/rmng-control-server"]
