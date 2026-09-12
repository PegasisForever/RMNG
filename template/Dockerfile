# syntax=docker/dockerfile:1
# RMNG clone TEMPLATE image (pegasis0/rmng-template) — the pre-provisioned base every
# clone is created FROM. Boots via /sbin/init.
# Clone-daemon + agent-wrapper are NOT baked in — installed pre-boot, so the template
# never drifts from the server.
# BUILD (repo root): docker build -f template/Dockerfile \
#   -t pegasis0/rmng-template:$(date +%Y%m%d) -t pegasis0/rmng-template:latest .
# Immutable dated tags + moving :latest. scripts/publish-template.sh wraps it all.

# 1. gnome-build stage
FROM ubuntu:26.04 AS gnome-build
ENV DEBIAN_FRONTEND=noninteractive
# .deps-done skips the script's own dep install; Apt lists kept (build-shell-deb.sh needs them).
RUN sed -i 's/^Types: deb$/Types: deb deb-src/' /etc/apt/sources.list.d/ubuntu.sources \
 && apt-get update \
 && apt-get build-dep -y gnome-shell \
 && apt-get install -y --no-install-recommends \
      sassc dpkg-dev meson ninja-build patch ca-certificates \
 && mkdir -p /root/rmng-shell-build && touch /root/rmng-shell-build/.deps-done
COPY template/gnome-patch/ /src/gnome-patch/
# build-shell-deb.sh prints `DEB=<path>`; copy it to the fixed path final copies from.
RUN bash /src/gnome-patch/build-shell-deb.sh > /tmp/shell-deb.out \
 && DEB="$(sed -n 's/^DEB=//p' /tmp/shell-deb.out | tail -1)" \
 && test -n "$DEB" && test -f "$DEB" \
 && mkdir -p /out && cp "$DEB" /out/gnome-shell.deb \
 && echo "[gnome-build] gnome-shell.deb: $(du -h /out/gnome-shell.deb | cut -f1)"

# 2. final stage — the provisioned template rootfs
FROM ubuntu:26.04

ARG USERNAME=rmng
ARG CLONE_SOCKET=/srv/rmng-sock/clones.sock

# One RUN per phase, rarest first; each script COPYed right before its RUN (edits
# invalidate only that phase). Env lives INSIDE the scripts, never image ENV.
COPY template/setup/lib.sh /setup/lib.sh

COPY template/setup/10-desktop.sh /setup/10-desktop.sh
RUN /setup/10-desktop.sh

# Patched gnome-shell (hides screen-sharing indicator, enables Shell.Eval).
COPY --from=gnome-build /out/gnome-shell.deb /tmp/gnome-shell.deb
COPY template/setup/15-gnome-patch.sh /setup/15-gnome-patch.sh
RUN /setup/15-gnome-patch.sh

COPY template/setup/20-toolbox.sh /setup/20-toolbox.sh
RUN /setup/20-toolbox.sh

# USERNAME must map to uid 1000 (home/** tar ownership depends on it).
COPY template/setup/30-user.sh /setup/30-user.sh
RUN USERNAME="$USERNAME" CLONE_SOCKET="$CLONE_SOCKET" /setup/30-user.sh

# Clone sshd: pubkey-only, no root/passwords. Host keys NOT baked (per-clone injected).
# systemctl enable is a pure symlink op — needs no running systemd at build time.
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

# Tail: drop caches, blank machine-id + host keys (both regenerate per clone — baked
# copies would unify the fleet), assert uid 1000.
RUN apt-get clean \
 && rm -rf /var/lib/apt/lists/* /tmp/* /var/tmp/* \
 && : > /etc/machine-id \
 && rm -f /etc/ssh/ssh_host_* \
 && [ "$(id -u "$USERNAME")" = 1000 ]

LABEL rmng.image=1 rmng.base=1
ENV container=docker
STOPSIGNAL SIGRTMIN+3
ENTRYPOINT ["/sbin/init"]
CMD []
