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
#   2. rust-build  — rustup stable; dev deps; cargo build --release clone-daemon
#                    + control-server.
#   3. runtime     — ubuntu:26.04, runtime libs + /usr/local/share/rmng payloads
#                    (2 binaries + static/), WORKDIR /data, EXPOSE 9000-9003.

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

# Release-build both binaries in one cache-mounted RUN (shared dep graph compiles once).
# The `target` cache is a mount, so outputs must be copied OUT in the same RUN (cache
# mounts don't persist into the image layer). clone-daemon is a payload (the server
# pushes it into clones); the control-server binary is the image entrypoint.
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/src/target \
    cargo build --release -p clone-daemon -p control-server \
 && mkdir -p /out \
 && cp target/release/rmng-clone-daemon /out/clone-daemon \
 && cp target/release/rmng-control-server /out/rmng-control-server

# ---------------------------------------------------------------------------------------
# 3. runtime stage
# ---------------------------------------------------------------------------------------
FROM ubuntu:26.04 AS runtime
ENV DEBIAN_FRONTEND=noninteractive
# Runtime deps mined from scripts/cs-deploy-ct.sh, minus openssh-client/sshfs (the Docker
# port dials the local daemon over a unix socket — no SSH). vah264enc/vapostproc live in
# the `va` plugin shipped by gstreamer1.0-plugins-bad; pngenc (screenshots) in -good.
RUN apt-get update \
 && apt-get install -y --no-install-recommends \
      gstreamer1.0-plugins-base gstreamer1.0-plugins-good gstreamer1.0-plugins-bad \
      libva2 libva-drm2 va-driver-all libdrm2 \
      ca-certificates \
 && rm -rf /var/lib/apt/lists/*

COPY --from=rust-build /out/rmng-control-server /usr/local/bin/rmng-control-server

# Payloads + frontend on the image filesystem, stored PLAIN (assets.rs / web.rs read
# these; the binswap engine hot-swaps the same clone-daemon/agent-wrapper bytes into
# already-running clones at /opt/rmng/bin/<bin> — see binswap.rs. Clones are no longer
# provisioned from these payloads: they're created FROM the pre-built, separately
# published clone template (template/Dockerfile), which installs its own copies.
COPY --from=rust-build  /out/clone-daemon               /usr/local/share/rmng/clone-daemon
COPY --from=bun-build   /tmp/agent-wrapper              /usr/local/share/rmng/agent-wrapper
COPY --from=bun-build   /src/frontend/build/client      /usr/local/share/rmng/static

# CWD-relative config.json + data/ land in the /data volume (config.rs uses relative paths).
WORKDIR /data
# 9000 web/API, 9001 video, 9002 per-clone MCP, 9003 global MCP.
EXPOSE 9000-9003
# Logging default only (not a setting — no config lives in env, per the no-env invariant).
ENV RUST_LOG=info,tower_http=warn,clip=debug
ENTRYPOINT ["/usr/local/bin/rmng-control-server"]
