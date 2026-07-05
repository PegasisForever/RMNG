# Running RMNG's Docker host on a Proxmox LXC CT

Proxmox is no longer part of RMNG — the control-server drives a **local Docker daemon**, not
`pct`. But an unprivileged Proxmox LXC CT is a perfectly good place to *run that Docker
daemon* (nested Docker on a shared kernel). This is a documentation-only recipe; the code
never assumes it. Once Docker is up and healthy in the CT, follow [DEPLOY.md](DEPLOY.md) as
you would on any host.

## 1. Create an unprivileged CT with nesting + the render node

Use an Ubuntu 26.04 CT template. The RMNG clones need nested Docker and a GPU render node, so
the CT needs these node-side settings (`/etc/pve/lxc/<id>.conf`):

```conf
# Nested containers + keyring (Docker-in-LXC). `fuse=1` is only needed for the OPTIONAL
# lxcfs feature (§2b) — clones seeing their own CPU/RAM limits in /proc; harmless otherwise.
features: nesting=1,keyctl=1,fuse=1

# GPU render node passthrough for VA-API (encode on the control-server, capture in clones).
dev0: /dev/dri/renderD128,mode=0666

# Let the guest's Docker/systemd operate without the host AppArmor profile fighting it.
# The unconfined profile alone is NOT enough for nested Docker: the runtime still probes
# /sys/kernel/security/apparmor and dies with "Could not check if docker-default AppArmor
# profile was loaded: permission denied". The /dev/null bind makes nested runtimes see
# AppArmor as disabled; the relaxed auto-mounts match what the old LXC clones used.
lxc.apparmor.profile: unconfined
lxc.mount.entry: /dev/null sys/module/apparmor/parameters/enabled none bind,optional 0 0
lxc.mount.auto: cgroup:mixed proc:rw sys:mixed
```

Set them via `pct set <id> --features nesting=1,keyctl=1,fuse=1` and edit the conf for the `dev0` /
`lxc.apparmor.profile` lines, then restart the CT. Give it enough cores/RAM/disk for the
fleet you intend to run (clones default to 16 cores / 32 GiB each — tune
`docker.cloneCpus` / `docker.cloneMemoryMb` in the wizard).

## 1b. Raise the kernel keyring quotas on the Proxmox host

In an unprivileged CT, **every** container's root maps to the same host uid, so all their
session keyrings (one per `docker run`, incl. each clone's inner-Docker containers) share
that uid's kernel quota. At the stock `kernel.keys.maxbytes=20000` a fleet dies around its
6th-7th concurrent container with `unable to join session keyring: … disk quota exceeded`
(found live in E2E). On the **Proxmox host**:

```sh
cat >> /etc/sysctl.d/99-rmng-keys.conf <<EOF
kernel.keys.maxkeys = 20000
kernel.keys.maxbytes = 2000000
EOF
sysctl --system
```

## 2. Install Docker in the CT

```sh
# The standard CT template ships without curl — install it first, or the pipe below
# silently feeds `sh` nothing and "succeeds" having installed nothing.
apt-get update && apt-get install -y curl ca-certificates

# Docker CE from the official repo (get.docker.com is the quickest path).
curl -fsSL https://get.docker.com | sh
```

## 2b. (Optional) Install lxcfs so clones see their own CPU/RAM limits

Clones get cgroup limits (16 cpu / 32 GiB by default), but the kernel's `/proc` isn't
namespaced — so inside a clone `free -h`/`nproc`/`htop` otherwise report the whole host's
RAM and cores. Install **lxcfs** in the CT and RMNG binds its cgroup-aware `/proc` files
over each *new* clone's `/proc/{meminfo,cpuinfo,stat,uptime,loadavg,swaps}`:

```sh
apt-get install -y lxcfs
# The lxcfs service starts on install and mounts /var/lib/lxcfs/proc/*; confirm with:
ls /var/lib/lxcfs/proc/            # cpuinfo loadavg meminfo stat swaps uptime
```

**Inside the Docker-host CT specifically**, Ubuntu's `lxcfs.service` ships
`ConditionVirtualization=!container` — so the unit is silently skipped ("unmet condition")
when installed inside an LXC CT rather than on bare metal, and the `ls` above comes up
empty. Drop that condition with an override (found live: `apt-get install` alone is not
enough in this environment):

```sh
mkdir -p /etc/systemd/system/lxcfs.service.d
printf '[Unit]\nConditionVirtualization=\n' > /etc/systemd/system/lxcfs.service.d/in-ct.conf
systemctl daemon-reload && systemctl enable --now lxcfs
```

This needs the CT feature `fuse=1` (set in §1). It's entirely optional: without lxcfs,
clones just see host-wide `/proc` values and everything else works. RMNG auto-detects it —
the setup wizard's environment checklist shows an advisory **LXCFS** row (present / not
installed). Install it, then restart the control-server (or hit Settings → Test) and
re-create clones to pick it up (see the caveat on load average in
[DEPLOY.md](DEPLOY.md#clone-proc-limits-lxcfs)).

## 3. Verify the daemon before deploying RMNG

```sh
docker info | grep -i 'storage driver'    # overlay2 (or overlayfs on Docker ≥29) — NOT vfs
ls -l /dev/dri/renderD128                  # the render node must be present in the CT
docker run --rm hello-world                # nested Docker actually runs
```

If the storage driver is `vfs`, nested overlayfs isn't available —
recheck `features: nesting=1` and that the CT was restarted. RMNG's per-clone
`rmng-dind-<id>` volume at `/var/lib/docker` is the overlay-on-overlay fix for the clones'
own inner Docker, but the CT's *outer* Docker still needs overlay2.

The fleet's Docker Hub pulls are de-duplicated by the shared `rmng-registry` pull-through cache
(the fix for `docker.io` rate limits), and build layers are shared via the `rmng-buildkit`
daemon — **not** by sharing `/var/lib/docker`, which concurrent daemons cannot do (hence the
per-clone `rmng-dind-*` / `rmng-ctd-*` volumes remain fully isolated). See DEPLOY.md → "Shared
build cache & Docker Hub mirror".

## 4. Deploy RMNG

Now the CT is just a Docker host. Continue with [DEPLOY.md](DEPLOY.md): pull/build the image,
`docker compose up -d` (or the `docker run` one-liner), open `http://<ct-ip>:9000`, and run
the setup wizard. The wizard's environment checklist (`GET /api/setup/env`) will confirm the
Docker daemon, the `/srv/rmng-sock` mount, and `/dev/dri/renderD128` from inside the CT, plus
the advisory lxcfs row (§2b).
