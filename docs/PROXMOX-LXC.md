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
# Nested containers + keyring (Docker-in-LXC).
features: nesting=1,keyctl=1

# GPU render node passthrough for VA-API (encode on the control-server, capture in clones).
dev0: /dev/dri/renderD128,mode=0666

# Let the guest's Docker/systemd operate without the host AppArmor profile fighting it.
lxc.apparmor.profile: unconfined
```

Set them via `pct set <id> --features nesting=1,keyctl=1` and edit the conf for the `dev0` /
`lxc.apparmor.profile` lines, then restart the CT. Give it enough cores/RAM/disk for the
fleet you intend to run (clones default to 16 cores / 32 GiB each — tune
`docker.cloneCpus` / `docker.cloneMemoryMb` in the wizard).

## 2. Install Docker in the CT

```sh
# Docker CE from the official repo (get.docker.com is the quickest path).
curl -fsSL https://get.docker.com | sh
```

## 3. Verify the daemon before deploying RMNG

```sh
docker info | grep -i 'storage driver'    # must be: Storage Driver: overlay2
ls -l /dev/dri/renderD128                  # the render node must be present in the CT
docker run --rm hello-world                # nested Docker actually runs
```

If the storage driver is `vfs` instead of `overlay2`, nested overlayfs isn't available —
recheck `features: nesting=1` and that the CT was restarted. RMNG's per-clone
`rmng-dind-<id>` volume at `/var/lib/docker` is the overlay-on-overlay fix for the clones'
own inner Docker, but the CT's *outer* Docker still needs overlay2.

## 4. Deploy RMNG

Now the CT is just a Docker host. Continue with [DEPLOY.md](DEPLOY.md): pull/build the image,
`docker compose up -d` (or the `docker run` one-liner), open `http://<ct-ip>:9000`, and run
the setup wizard. The wizard's environment checklist (`GET /api/setup/env`) will confirm the
Docker daemon, the `/srv/rmng-sock` mount, and `/dev/dri/renderD128` from inside the CT.
