# syntax=docker/dockerfile:1
#
# RMNG control-server image (BuildKit multi-stage). Canonical build:
# `docker build -t rmng:latest .` Everything heavy (rust toolchain, dev deps, bun)
# lives in build stages; the runtime carries only what the server needs: GStreamer/VA
# ingest + encode, samba (clone homes over SMB), payloads, and a uid-1000 share user.
#
# Nothing is compiled into the server binary: the runtime assembles
# /usr/local/share/rmng/ (clone-daemon + agent-wrapper binaries, frontend static/),
# read at runtime. Payloads stored UNcompressed (registry pushes compress layers anyway).
# The two build stages are independent — parallel builds, rust-only changes skip bun.
#
# Stages: 1. bun-build (frontend + agent-wrapper binary) 2. rust-build (3 binaries)
#   3. runtime (ubuntu:26.04 + runtime libs + payloads + share user, EXPOSE 9000 9001 9005 445).

# ---------------------------------------------------------------------------
# 1. bun stage: frontend build + agent-wrapper bun --compile
# ---------------------------------------------------------------------------
FROM oven/bun:1 AS bun-build
WORKDIR /src

# Manifest+lock first so the install layer caches across source-only edits.
COPY frontend/package.json frontend/bun.lock ./frontend/
RUN cd frontend && bun install --frozen-lockfile
COPY frontend/ ./frontend/
RUN cd frontend && bun run build

# agent-wrapper: single self-contained binary, installed into each clone at create time.
COPY agent-wrapper/package.json agent-wrapper/bun.lock ./agent-wrapper/
RUN cd agent-wrapper && bun install --frozen-lockfile
COPY agent-wrapper/ ./agent-wrapper/
RUN cd agent-wrapper \
 && bun build --compile src/server.ts --outfile /tmp/agent-wrapper

# ---------------------------------------------------------------------------
# 2. rust build stage — binaries only (fully parallel with 1)
# ---------------------------------------------------------------------------
FROM ubuntu:26.04 AS rust-build
ENV DEBIAN_FRONTEND=noninteractive
# *-sys crates need cc/pkg-config; media needs gstreamer/VA/drm/pipewire dev files.
# libgtk-4-dev arrives via the workspace toolchain (viewer); harmless here.
RUN apt-get update \
 && apt-get install -y --no-install-recommends \
      build-essential pkg-config clang git curl ca-certificates \
      libgstreamer1.0-dev libgstreamer-plugins-base1.0-dev \
      libva-dev libdrm-dev libpipewire-0.3-dev libgtk-4-dev \
 && rm -rf /var/lib/apt/lists/*

# apt rustc is too old for edition 2024 — rustup stable instead.
ENV RUSTUP_HOME=/usr/local/rustup CARGO_HOME=/usr/local/cargo PATH=/usr/local/cargo/bin:$PATH
RUN curl -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal --default-toolchain stable

WORKDIR /src
# Whole context (include_str!'d scripts + Cargo.toml/lock; .dockerignore keeps out
# target/, node_modules, frontend/build).
COPY . .

# One cache-mounted RUN so the shared dep graph compiles once. Cache mounts don't persist
# into layers, so outputs are copied OUT in the same RUN.
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/src/target \
    cargo build --release -p clone-daemon -p control-server -p rmng-cli \
 && mkdir -p /out \
 && cp target/release/rmng-clone-daemon /out/clone-daemon \
 && cp target/release/rmng /out/rmng-cli \
 && cp target/release/rmng-control-server /out/rmng-control-server

# ---------------------------------------------------------------------------
# 3. runtime stage
# ---------------------------------------------------------------------------
FROM ubuntu:26.04 AS runtime
ENV DEBIAN_FRONTEND=noninteractive
# samba: serves clone homes over SMB. openssh-server: sshd + ssh-keygen for the :2222
# bastion and per-clone host keys (control plane itself uses unix sockets, no SSH).
# rclone: fast project copies between clone homes (measured 2.1s vs 5.8s cp -a on 23k
# files); cp -a stays the fallback. gstreamer1.0-gl: separate package shipping glupload —
# without it the zero-copy AVC444 encode bridge dies at init and viewers hang on
# "connected, waiting for video".
RUN apt-get update \
 && apt-get install -y --no-install-recommends \
      gstreamer1.0-plugins-base gstreamer1.0-plugins-good gstreamer1.0-plugins-bad \
      gstreamer1.0-gl \
      libva2 libva-drm2 va-driver-all libdrm2 \
      ca-certificates samba openssh-server rclone zfsutils-linux \
 && rm -rf /var/lib/apt/lists/*

# SMB-only `rmng` at uid/gid 1000 (must equal the clone's uid so share-created files land
# owned right). Frees whoever holds uid 1000 first (the base's default `ubuntu` user).
RUN if getent passwd 1000 >/dev/null; then userdel -r "$(getent passwd 1000 | cut -d: -f1)" 2>/dev/null || true; fi \
 && groupadd -g 1000 rmng \
 && useradd -u 1000 -g 1000 -M -s /usr/sbin/nologin rmng

# Version stamp for the self-update UI (publish-server.sh --build-arg; plain builds show
# "dev build"). The only place the server learns its own version.
ARG GIT_SHA=""
ARG BUILD_DATE=""
LABEL org.opencontainers.image.revision="$GIT_SHA" \
      org.opencontainers.image.created="$BUILD_DATE" \
      org.opencontainers.image.version="$GIT_SHA"

COPY --from=rust-build /out/rmng-control-server /usr/local/bin/rmng-control-server

# Payloads + frontend, stored PLAIN. Injected into every clone at CREATE (the sole
# delivery path); the clone template carries none of them.
COPY --from=rust-build  /out/clone-daemon               /usr/local/share/rmng/clone-daemon
COPY --from=rust-build  /out/rmng-cli                   /usr/local/share/rmng/rmng-cli
COPY --from=bun-build   /tmp/agent-wrapper              /usr/local/share/rmng/agent-wrapper
COPY --from=bun-build   /src/frontend/build/client      /usr/local/share/rmng/static

# CWD-relative config.json + data/ land in the /data volume (config.rs uses relative paths).
WORKDIR /data
# 9000 web/API, 9001 video, 9005 forward, 445 SMB (clone homes).
EXPOSE 9000 9001 9005 445
# Logging default only (no config lives in env, per the no-env invariant).
ENV RUST_LOG=info,tower_http=warn,clip=debug
ENTRYPOINT ["/usr/local/bin/rmng-control-server"]
