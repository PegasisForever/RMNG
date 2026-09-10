# Running RMNG's Docker host on a Proxmox LXC CT

RMNG drives a **local Docker daemon**, not `pct`. A **privileged** Proxmox LXC CT is where
that Docker daemon runs (nested Docker on a shared kernel). Privileged is a hard requirement
since gen-2: every clone home is its own ZFS dataset, and only a privileged CT can hold
`/dev/zfs` plus the homes mount (§1c). This is a one-way trip — an existing unprivileged CT
is converted via dump → restore-as-privileged (see [GEN2-CLONES.md](GEN2-CLONES.md) §4).
The CT-wide live resource summary intentionally supports the documented production layout
only: CT 105 with cgroup v2, an enforced 16-CPU capacity, and a ZFS-backed rootfs. Once
Docker is up and healthy in the CT, follow [DEPLOY.md](DEPLOY.md) as you would on any host.

## 1. Create a privileged CT with nesting + the render node

Use an Ubuntu 26.04 CT template (unprivileged OFF). The CT needs nested Docker and a GPU
render node, so set these node-side settings (`/etc/pve/lxc/<id>.conf`):

```conf
# Nested containers + keyring (Docker-in-LXC). `fuse=1` is only needed for the OPTIONAL
# lxcfs feature (§2b) — clones seeing their own CPU/RAM limits in /proc; harmless otherwise.
features: nesting=1,keyctl=1,fuse=1

# GPU render node passthrough for VA-API (encode on the control-server, capture in clones).
# Still required: the setup wizard's environment check gates finishing setup on it, and the
# video plane needs it at runtime. There is deliberately NO kfd/compute node anymore:
# clone-side GPU support was removed, and clones capture fine without it (verified Sep 2026:
# first clone on a fresh CT screenshots with renderD128 alone).
dev0: /dev/dri/renderD128,mode=0666

# Let the guest's Docker/systemd operate without the host AppArmor profile fighting it.
# Still required on privileged CTs too (verified Sep 2026: without these lines `docker run`
# dies with "AppArmor enabled on system but the docker-default profile could not be
# loaded: permission denied").
# The unconfined profile alone is NOT enough for nested Docker: the runtime still probes
# /sys/kernel/security/apparmor and dies with "Could not check if docker-default AppArmor
# profile was loaded: permission denied". The /dev/null bind makes nested runtimes see
# AppArmor as disabled; the relaxed auto-mounts match what the old LXC clones used.
lxc.apparmor.profile: unconfined
lxc.mount.entry: /dev/null sys/module/apparmor/parameters/enabled none bind,optional 0 0
lxc.mount.auto: cgroup:mixed proc:rw sys:mixed
```

Set them via `pct set <id> --features nesting=1,keyctl=1,fuse=1` and edit the conf for the `dev0` /
`dev1` / `lxc.apparmor.profile` lines, then restart the CT. Give it enough cores/RAM/disk for the
fleet you intend to run (clones default to 16 cores / 32 GiB each — tune
`docker.cloneCpus` / `docker.cloneMemoryMb` in the wizard).

RMNG's live memory accounting reads cgroup-v2 counters through the control-server's shared PID
namespace, so retain the deployment's `privileged: true` and `pid: "host"` settings. Without
those settings, swap-aware clone memory samples are unavailable.

### CT 105-wide sidebar metrics

The sidebar’s `LXC` header is measured from CT 105 itself rather than by adding clone values. It
reads the CT-root cgroup through `/proc/1/root`: CPU comes from `cpu.stat` against its enforced
16-CPU capacity, and memory includes every CT process with the same swap-aware/cache-excluding policy used for
clone rows. Disk is `statvfs` usage of the CT root filesystem, so this ZFS rootfs figure is
physical and compression-aware. There is deliberately no logical/pre-compression disk metric:
RMNG does not query the Proxmox host for ZFS `logicalused`.

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

## 1c. ZFS in the CT + the homes dataset (gen-2, required)

Every clone home is a ZFS dataset under one parent, e.g. `rpool/rmng-homes` (the pool name
differs per host). The control-server creates per-clone datasets from inside its own mount
namespace, so three things must be true before it ever runs:

On the **Proxmox host**:

```sh
# Find the ZFS device number (usually 10:249) and allow the CT to hold the node.
ls -l /dev/zfs
# ... then in /etc/pve/lxc/<id>.conf (10:249 here — use what you measured):
# lxc.cgroup2.devices.allow: c 10:249 rwm

# The parent dataset, mounted into the CT at the fixed /srv/rmng-homes path:
zfs create -o mountpoint=/srv/rmng-homes-fresh rpool/rmng-homes-fresh
# ... then in /etc/pve/lxc/<id>.conf:
# mp0: /srv/rmng-homes-fresh,mp=/srv/rmng-homes
```

Do NOT bind-mount the host `/dev/zfs` (`lxc.mount.entry` for it breaks nested container
mount joins: every `docker exec` silently lands on CT files). Restart the CT after editing
its conf.

Inside the **CT**:

```sh
apt-get install -y zfsutils-linux
mknod /dev/zfs c 10 249   # same major:minor as the host node; lives on the CT /dev
                          # tmpfs, so re-create it after every CT restart (the
                          # control-server re-creates its own at boot, but your shell
                          # needs this one for manual zfs)
zfs list                  # must work
```

Smoke-test snapshot/clone/destroy timing on a scratch dataset before going further
([GEN2-CLONES.md](GEN2-CLONES.md) §5.2). Then tell the server which parent is yours BEFORE
the first create — Settings → Docker → homes parent, or
`PUT /api/config {"docker":{"homesParent":"rpool/rmng-homes-fresh"}}`. The default
(`tank/rmng/homes`) fits almost nobody; a first create with the wrong parent fails with
`no such pool`, cleanly but confusingly. The setup wizard does not ask for this yet.

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

## 2c. (Optional) Let clones resolve Tailscale (`*.ts.net`) names

If the CT is on a tailnet and you want clones to reach tailnet services **by name** — opening
a self-hosted app's link in a clone's browser, say — point the Docker daemon's DNS at the
Tailscale resolver. Clones inherit the daemon's `dns` setting; RMNG never sets container DNS
itself (it sets only privileged/cpu/memory/shm/mounts/restart-policy on a clone's
`HostConfig`), so this is purely host-side and needs no RMNG change.

Routing to the tailnet already works without any of this: the clone's default route is the
`rmng` bridge gateway, and `-s <docker.subnet> -j MASQUERADE` NATs it onto `tailscale0`. Only
**name resolution** is missing.

```sh
# 1. Let the CT use the tailnet resolver (persisted in tailscaled.state; survives reboot).
tailscale set --accept-dns=true

# 2. Hand that resolver to every NEW container.
mkdir -p /etc/docker
cat > /etc/docker/daemon.json <<'EOF'
{
  "dns": ["100.100.100.100", "64.71.255.204"]
}
EOF

# 3. SIGHUP — `dns` is a reloadable option, so dockerd is NOT restarted and running
#    clones are untouched. Confirm the PID is unchanged.
systemctl show -p MainPID --value docker
systemctl reload docker
systemctl show -p MainPID --value docker
```

The second entry is the CT's pre-existing `nameserver` (from `/etc/resolv.conf` before step 1),
kept as a plain resolver fallback. Note what it does **not** buy you: with `tailscaled` stopped,
`*.ts.net` fails either way — quad100 is a netfilter interception, so when tailscaled is down
the address is simply unreachable and no fallback can answer for the tailnet. Public names keep
resolving in both cases (measured: with and without the second entry). Listing it is still
worth doing — it is what answers if quad100 is reachable but the tailnet resolver misbehaves,
and it makes the intended upstream explicit rather than implicit.

Verify from a throwaway container on the `rmng` network — tailnet **and** public names must
both resolve:

```sh
docker run --rm --network rmng alpine:latest sh -c '
  for h in <host>.<tailnet>.ts.net registry-1.docker.io github.com; do
    printf "%-34s " "$h"; getent hosts "$h" >/dev/null 2>&1 && echo OK || echo FAIL
  done'
```

`dns` applies to **newly created** containers, so existing clones keep the old resolver until
they are re-created — the same rule as the lxcfs binds in §2b.

Three counter-intuitive details, each of which costs an afternoon if you rediscover it:

- **`*.ts.net` resolves even with `accept-dns=false`**, via the tailnet's split-DNS route
  (`ts.net. → <resolver>`, visible in `tailscale dns status`). What the flag actually buys is
  **public** resolution: without it `100.100.100.100` has no upstream to forward to, so
  `registry-1.docker.io` and `github.com` fail while tailnet names still work. The failure
  mode is the opposite of what you would guess.
- **Nothing listens on `100.100.100.100:53`.** `tailscaled` intercepts it with netfilter
  (`NetfilterMode: 2`), so `ss -lunp` shows only `:41641`. An empty `ss` output is *not*
  evidence that quad100 is broken — query it instead, and use a real client (`getent`,
  `nslookup`); a hand-rolled DNS packet with a malformed 13-byte header gets `SERVFAIL`/
  `FORMERR` from *every* resolver and looks exactly like "quad100 is dead".
- **Reaching a `tailscale serve` endpoint requires correct SNI.** `curl https://<tailscale-ip>/`
  fails the TLS handshake (curl exit 35) even when connectivity is perfect; test by name, or
  with `--resolve <name>:443:<ip>`. Likewise curl exit 6 ("could not resolve host") means DNS
  never resolved — no packet was sent, so it says nothing about reachability.

Step 1 hands `/etc/resolv.conf` to Tailscale (it rewrites the file and backs up the original).
On a Proxmox CT that file is also written by PVE at CT start, and `systemd-resolved` may be
active alongside in `resolv.conf mode: foreign` — check `resolvectl status` if resolution
behaves oddly after a CT restart. To undo everything:

```sh
tailscale set --accept-dns=false
rm /etc/docker/daemon.json          # or drop just the "dns" key if the file has other settings
systemctl reload docker
```

## 3. Verify the daemon before deploying RMNG

```sh
docker info | grep -i 'storage driver'    # overlay2 (or overlayfs on Docker ≥29) — NOT vfs
ls -l /dev/dri/renderD128                  # the render node must be present in the CT
ls -l /dev/zfs && zfs list | head -n 2     # ZFS usable (§1c)
ls -d /srv/rmng-homes                      # the homes mount (§1c)
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

Now the CT is just a Docker host. Do NOT use `docker compose` (it prefixes volume names and
loses the completed setup); run the control-server directly, with the homes dir bound
**shared** — datasets are created from inside this container, and only a shared bind
propagates their mounts into dockerd's namespace (without it `docker create` fails with
`bind source path does not exist`):

```sh
docker pull pegasis0/rmng:latest
docker run -d --name rmng --privileged --pid=host --restart unless-stopped \
  -p 445:445 -p 2222:2222 -p 9000:9000 -p 9001:9001 -p 9005:9005 \
  -v /var/run/docker.sock:/var/run/docker.sock -v rmng-data:/data -v rmng-sock:/srv/rmng-sock \
  -v /srv/rmng-homes:/srv/rmng-homes:shared \
  -e RUST_LOG=info,tower_http=warn pegasis0/rmng:latest
```

Then open `http://<ct-ip>:9000` and run the setup wizard. The wizard's environment checklist
(`GET /api/setup/env`) confirms the Docker daemon, the `/srv/rmng-sock` mount, and
`/dev/dri/renderD128` from inside the CT, plus the advisory lxcfs row (§2b). Before the
first create, set the homes parent (§1c) and size the clones to the CT (Settings → Docker →
clone CPUs/memory — the defaults assume a 32-core fleet host and fail small CTs with a
CPU-range error). See [DEPLOY.md](DEPLOY.md) for ports, volumes, and upgrade notes.
