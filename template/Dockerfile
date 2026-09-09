# syntax=docker/dockerfile:1
#
# RMNG clone TEMPLATE image (pegasis0/rmng-template) — the pre-provisioned base every clone
# is created FROM: headless GNOME + Mutter + VA-API + PipeWire, the dev toolbox, and the clone
# user (uid 1000, linger) with its systemd --user units. Boots via /sbin/init (systemd PID 1).
#
# The clone-daemon + agent-wrapper binaries the `--user` units exec are NOT baked in here — the
# control-server installs its own current copies into /opt/rmng/bin before each clone boots
# (provision.rs `CLONE_BINARIES`), so the template never carries a version that could drift from
# the running server. /opt/rmng/bin is pre-created (empty) by template/setup/30-user.sh.
#
# This REPLACES the old in-product bootstrap (the control-server ran provision-clone.sh
# inside a privileged `sleep infinity` build container over `docker exec`, then committed
# it). That recipe now lives in template/setup/*.sh and is RUN by THIS build; the image is
# published to Docker Hub instead of being built per-install.
#
# BUILD (repo-root context — the final stage COPYs template/setup/ + the stage payloads
# from it; run from the repo root, NOT template/):
#
#   docker build -f template/Dockerfile \
#     -t pegasis0/rmng-template:$(date +%Y%m%d) -t pegasis0/rmng-template:latest .
#
# VERSIONING: immutable dated `:YYYYMMDD` tags + a moving `:latest`. A rollback is just
# repointing the template reference at a prior dated tag; nothing is ever overwritten. The
# scripts/publish-template.sh wrapper builds + tags + pushes both.
#
# STAGES:
#   1. gnome-build — the patched gnome-shell .deb (shell-01 + shell-03), copied VERBATIM
#                    from the repo-root Dockerfile so BuildKit's content-addressed cache
#                    reuses the already-built (multi-GB, ~20-min) stage from prior rmng:*
#                    builds on this machine. (A later task removes it from the root file.)
#   2. final       — ubuntu:26.04; the provisioning phase scripts only (the clone binaries
#                    are NOT built here — the control-server installs them at clone-create).

# ---------------------------------------------------------------------------------------
# 1. gnome-build stage — VERBATIM from the repo-root Dockerfile (identical instruction
#    sequence + identical gnome-patch/ input → BuildKit reuses the cached stage).
# ---------------------------------------------------------------------------------------
FROM ubuntu:26.04 AS gnome-build
ENV DEBIAN_FRONTEND=noninteractive
# deb-src + gnome-shell build-deps in their OWN layer: multi-GB, but cached until the base
# image changes — gnome-patch/ edits don't bust it. Apt lists are deliberately kept
# (build-shell-deb.sh runs `apt-get download/source gnome-shell`). The .deps-done marker
# makes the script skip its own (redundant) dep install, exactly like cs-build-ct.sh did.
RUN sed -i 's/^Types: deb$/Types: deb deb-src/' /etc/apt/sources.list.d/ubuntu.sources \
 && apt-get update \
 && apt-get build-dep -y gnome-shell \
 && apt-get install -y --no-install-recommends \
      sassc dpkg-dev meson ninja-build patch ca-certificates \
 && mkdir -p /root/rmng-shell-build && touch /root/rmng-shell-build/.deps-done
COPY gnome-patch/ /src/gnome-patch/
# build-shell-deb.sh prints the produced deb path as `DEB=<path>` on stdout; copy it to the
# fixed path the final stage copies from.
RUN bash /src/gnome-patch/build-shell-deb.sh > /tmp/shell-deb.out \
 && DEB="$(sed -n 's/^DEB=//p' /tmp/shell-deb.out | tail -1)" \
 && test -n "$DEB" && test -f "$DEB" \
 && mkdir -p /out && cp "$DEB" /out/gnome-shell.deb \
 && echo "[gnome-build] gnome-shell.deb: $(du -h /out/gnome-shell.deb | cut -f1)"

# ---------------------------------------------------------------------------------------
# 2. final stage — the provisioned template rootfs (boots as systemd PID 1)
# ---------------------------------------------------------------------------------------
FROM ubuntu:26.04

# Provisioning inputs → the phase-script env. USERNAME must map to uid 1000; MONITORS is the
# CSV monitor layout (empty ⇒ a single 1920x1080 dummy mode); CLONE_SOCKET is the
# control-server media socket the clone-daemon dials.
ARG USERNAME=rmng
ARG MONITORS="2560x1440+2560+0*,2560x1440+0+0"
ARG CLONE_SOCKET=/srv/rmng-sock/clones.sock

# The provisioning runs as ONE `RUN` per phase, ordered by change frequency (rarest first)
# so a user-setup tweak never re-runs the ~20-minute apt/toolbox layers. DEBIAN_FRONTEND +
# SYSTEMD_OFFLINE are exported INSIDE each script (template/setup/lib.sh), never as image
# ENV — SYSTEMD_OFFLINE must not leak into the booted system.
#
# Each phase script is COPYed IMMEDIATELY BEFORE its RUN (not one bulk `COPY setup/`): a
# bulk copy would fold every script into a single layer, so editing 30-user.sh would bust
# phase 10's cache too — defeating the change-frequency ordering. Per-phase copies mean a
# 30-user tweak invalidates only phase 30. lib.sh is shared (every phase
# sources it), so it is copied first and an edit there correctly rebuilds all phases.
COPY template/setup/lib.sh /setup/lib.sh

# Locale/tz, headless GNOME + Mutter + VA-API + PipeWire, Recommends strip + masks.
COPY template/setup/10-desktop.sh /setup/10-desktop.sh
RUN /setup/10-desktop.sh

# Patched gnome-shell (shell-01 hide screen-sharing indicator + shell-03 Shell.Eval).
COPY --from=gnome-build /out/gnome-shell.deb /tmp/gnome-shell.deb
COPY template/setup/15-gnome-patch.sh /setup/15-gnome-patch.sh
RUN /setup/15-gnome-patch.sh

# Third-party apt repos, dev toolbox, Mission Center/Monaspace, dconf defaults.
COPY template/setup/20-toolbox.sh /setup/20-toolbox.sh
RUN /setup/20-toolbox.sh

# Clone user + groups + linger, fish, PATH rc, keyring, CLAUDE.md, claude/uv/rustup/nvm,
# systemd --user units + wants symlinks.
COPY template/setup/30-user.sh /setup/30-user.sh
RUN USERNAME="$USERNAME" MONITORS="$MONITORS" CLONE_SOCKET="$CLONE_SOCKET" /setup/30-user.sh

# Clone sshd: pubkey-only, no root, no passwords. Host keys are NOT baked — the
# control-server injects a unique persisted key per clone at provision (like machine-id).
# `systemctl enable ssh` resolves to ssh.service; Ubuntu 26.04's openssh-server postinst
# already enables ssh.socket (socket activation) by default, so both end up enabled — this
# is the standard dual-enable (verified live: systemd hands ssh.service the socket's
# already-bound fd, no port-22 conflict) and works whichever unit the clone ends up starting
# from. `systemctl enable` here is a pure unit-file symlink op (no running systemd/bus needed
# at build time — same category as the `systemctl mask` calls in 10-desktop.sh), so it
# doesn't require SYSTEMD_OFFLINE.
RUN mkdir -p /etc/ssh/sshd_config.d \
 && printf '%s\n' \
      'PasswordAuthentication no' \
      'PermitRootLogin no' \
      'KbdInteractiveAuthentication no' \
      'PubkeyAuthentication yes' \
      "AllowUsers $USERNAME" \
      'X11Forwarding no' \
      > /etc/ssh/sshd_config.d/10-rmng.conf \
 && systemctl enable ssh

# Tail: drop apt caches, blank /etc/machine-id (a fresh id is regenerated per clone on first
# boot — a baked id would give the whole fleet one identity), strip baked ssh host keys (the
# control-server injects a unique persisted key per clone at provision — a shared baked key
# would give the whole fleet one identity, exactly like machine-id), and assert the clone
# user is uid 1000 (the tar owner mapping for home/<user>/** depends on it).
RUN apt-get clean \
 && rm -rf /var/lib/apt/lists/* /tmp/* /var/tmp/* \
 && : > /etc/machine-id \
 && rm -f /etc/ssh/ssh_host_* \
 && [ "$(id -u "$USERNAME")" = 1000 ]

# Image config (matches DockerCtl::commit's set_boot_config + the rmng.image/rmng.base
# labels): clone-source badges, container marker, clean systemd stop, boot via /sbin/init.
LABEL rmng.image=1 rmng.base=1
ENV container=docker
STOPSIGNAL SIGRTMIN+3
ENTRYPOINT ["/sbin/init"]
CMD []
