#!/usr/bin/env bash
# Runs INSIDE the deploy CT (pushed by provision-deploy-ct.sh). Installs only the
# RUNTIME deps (no compilers, no Rust, no bun), writes a minimal config + systemd
# unit, and generates the orchestration SSH key. The control-server binary is
# already at /usr/local/bin/rmng-control-server (copied from the build CT beforehand).
#
#   cs-deploy-ct.sh <proxmox-ssh-target-from-ct>
set -euo pipefail
PROXMOX_FROM_CT="${1:?usage: cs-deploy-ct.sh <proxmox-ssh-target>}"
export DEBIAN_FRONTEND=noninteractive

echo "[deploy-ct] installing runtime deps (no dev toolchain)" >&2
apt-get update -qq
# Encodes received dmabufs via VA-API; does not capture → no PipeWire/GNOME.
# vah264enc/vapostproc come from gstreamer1.0-plugins-bad (the `va` plugin);
# pngenc (screenshot) from -good. `gstreamer1.0-va` is NOT a package on 24.04.
apt-get install -y -qq \
  gstreamer1.0-plugins-base gstreamer1.0-plugins-good gstreamer1.0-plugins-bad \
  libva2 libva-drm2 va-driver-all libdrm2 \
  openssh-client sshfs ca-certificates >&2

echo "[deploy-ct] config + ssh key + unit" >&2
mkdir -p /var/lib/rmng /srv/rmng-sock
# Minimal config: just the Proxmox SSH target so orchestration works out of the box.
# Linear/Claude/cloneAccounts/subnet are entered later in the web UI (/api/config).
cat > /var/lib/rmng/config.json <<CFG
{ "proxmox": { "ssh": "$PROXMOX_FROM_CT" } }
CFG
chmod 600 /var/lib/rmng/config.json

# Orchestration key (this CT → the Proxmox node, for `pct`). accept-new lets the
# server's BatchMode ssh connect on first contact without a prompt.
install -d -m700 /root/.ssh
[ -f /root/.ssh/id_ed25519 ] || ssh-keygen -t ed25519 -N '' -C rmng-control -f /root/.ssh/id_ed25519 >&2
grep -q StrictHostKeyChecking /root/.ssh/config 2>/dev/null \
  || printf 'Host *\n    StrictHostKeyChecking accept-new\n' >> /root/.ssh/config

cat > /etc/systemd/system/rmng-control-server.service <<'UNIT'
[Unit]
Description=rmng control-server
After=network-online.target
Wants=network-online.target

[Service]
ExecStart=/usr/local/bin/rmng-control-server
WorkingDirectory=/var/lib/rmng
Environment=RMNG_CONFIG=/var/lib/rmng/config.json
Environment=RMNG_CLONE_SOCKET=/srv/rmng-sock/clones.sock
Environment=RUST_LOG=info,tower_http=warn
Restart=on-failure
RestartSec=3

[Install]
WantedBy=multi-user.target
UNIT

systemctl daemon-reload
systemctl enable --now control-server >&2
echo "[deploy-ct] enabled" >&2
