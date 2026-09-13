# Runbook — migrating CT 104, CT 105 and CT 106 from gen-1 to gen-2

**Scope.** These three containers only, all on the Proxmox host `10.0.0.100`. Not a general
recipe.

**What happens.** A gen-1 clone keeps its home inside the container's writable layer; a gen-2
clone keeps it on a ZFS dataset outside the container. The new control-server cannot run
gen-1 clones, so each CT gets one window: build a new privileged CT, move the Docker state
into it, start the new server, and let it rewrite every clone.

**Status.** CT 104 is done — it is now CT 204 (`ivan-rmng`, 10.0.0.125). CT 105 and CT 106
are outstanding. One item is still open on CT 204: its `tailscale serve` config was not
restored (§6.3).

**Order: CT 104, then CT 105, then CT 106.** Smallest first; CT 106 last because it is the
largest, has the most rows, and is the only one with rows that cannot migrate (§3.4).

## The three boxes

| | CT 104 → 204 | CT 105 → 205 | CT 106 → 206 |
| --- | --- | --- | --- |
| IP | 10.0.0.206 | 10.0.0.15 | 10.0.0.180 |
| rootfs used | 101 GB | 612 GB | 917 GB |
| Docker | 29.7.2 / containerd.io 2.3.3 | 29.6.1 / containerd.io 2.2.5 | 29.6.1 / containerd.io 2.2.5 |
| clone rows | 8 | 30 | 104 |
| hostname prefix | `ivan-` | `pega-` | `haoran-` |
| presets | 1 (`Medi`) | 3 (`medi`, `hyperhost`, `wealthstack`) | 1 (`talktomedi`) |
| published ports | 445 2222 9000 9001 9005 | 445 2222 9000 9001 9005 | 445 2222 9000 9001 9005 |
| `/etc/docker/daemon.json` | none | `{"dns": […]}` | none |
| GPU devices to pass | render node + `/dev/kfd` | render node + `/dev/kfd` | render node only |
| tailnet | `tail8d43a5.ts.net` | `tail0e863.ts.net` | `tailc2613e.ts.net` |
| `tailscale serve` | — (restore it, §6.3) | yes → `:9000` | none |

CT 105 also publishes 9002 and 9003 with nothing behind them. Drop them from the new run
command.

Budget: copy ≈ 30 min (104), 3 h (105), 4 h 30 m (106), plus the per-clone migration in §5.6.

---

## 1. Decide two things before the first window

Neither has a safe default, and both are irreversible once the window starts.

### 1.1 The base image

A migrated clone does **not** keep its own base image. The server rebuilds it from the row
preset's Dockerfile, so every clone lands on `FROM pegasis0/rmng-template:latest`. The home
survives; everything the old image held outside `/home/rmng` does not.

Measure the gap per CT first — it is usually small. Put this in a file on the Proxmox host
and run it there; the quoting does not survive being typed through three shells:

```sh
#!/bin/bash
# usage: basediff.sh <ct-id> <a-live-clone-name>
CT=$1; CLONE=$2
pct exec "$CT" -- docker exec "$CLONE" bash -lc 'dpkg-query -W -f="${Package}\n"' \
  | LC_ALL=C sort > /tmp/clone.pkgs
pct exec "$CT" -- docker run --rm --entrypoint bash pegasis0/rmng-template:latest \
  -lc 'dpkg-query -W -f="${Package}\n"' | LC_ALL=C sort > /tmp/tmpl.pkgs
echo "lost:   $(LC_ALL=C comm -23 /tmp/clone.pkgs /tmp/tmpl.pkgs | tr '\n' ' ')"
echo "gained: $(LC_ALL=C comm -13 /tmp/clone.pkgs /tmp/tmpl.pkgs | tr '\n' ' ')"
for d in /opt /usr/local/bin; do
  echo "--- $d"
  echo "  clone:    $(pct exec "$CT" -- docker exec "$CLONE" ls "$d" | tr '\n' ' ')"
  echo "  template: $(pct exec "$CT" -- docker run --rm --entrypoint ls \
                        pegasis0/rmng-template:latest "$d" | tr '\n' ' ')"
done
```

Both `sort` calls need `LC_ALL=C`, or `comm` prints nonsense.

Then choose, per CT:

- **Accept the loss** and transcribe what matters into the preset Dockerfile as `RUN` lines
  on top of `FROM pegasis0/rmng-template:latest`. This is the intended path.
- **Publish the base**: `docker tag pega-template13:latest pegasis0/pega-template13:latest`,
  push it, and set the preset Dockerfile to `FROM pegasis0/pega-template13:latest`. Costs one
  upload of ~28 GB of commit layers.

A local-only tag cannot work: preset builds force `--pull`, and the CT's `rmng-registry` is a
pull-through proxy that refuses pushes.

### 1.2 The preset vars

Gen-2 has no preset `vars` field, and it writes only dynamic keys into `/etc/environment`.
Two carriers are needed, because they reach different processes:

- **Dockerfile `ENV` lines**, for every var including `PATH`. Reaches `docker exec` shells.
- **`~/.config/environment.d/10-rmng-preset.conf` in each clone's home**, for every var
  **except `PATH`**. This is the one the agent sees: it runs under `systemd --user`, which
  takes its environment from the user manager, not from the container's `Config.Env`.

Exclude `PATH` from the home file deliberately. Gen-1 did not apply the preset `PATH` to the
agent unit either, and setting it here drops `/opt/rmng/bin` from the agent's PATH.

§5.4 captures the values and §5.8 writes the file.

---

## 2. Before the window

### 2.1 Measure

```sh
PVE="ssh root@10.0.0.100"
CT=105                                   # or 104 / 106

# The running server revision. Never read this off `docker inspect` — a self-update copies
# the old container's labels onto the new one, so the label records the last hand create.
$PVE "pct exec $CT -- curl -s http://127.0.0.1:9000/api/server/version"

# The Docker package versions to pin on the new CT.
$PVE "pct exec $CT -- dpkg -l | grep -E 'docker-ce |docker-ce-cli|containerd.io|buildx|compose'"

# How much data moves. Use `du -shx`: without -x, du descends into every running container's
# overlay mount and double counts (CT 104 read 126G against a real 30G).
$PVE "pct exec $CT -- du -shx /var/lib/docker /var/lib/containerd"

# The exact run command of the current server, so the new one matches it.
$PVE "pct exec $CT -- docker inspect rmng \
  --format '{{json .HostConfig.PortBindings}} {{json .HostConfig.Binds}} {{.HostConfig.Init}} {{json .Config.Env}}'"
```

### 2.2 Check pool space

The new CT holds a second copy of the whole Docker state until the old CT is destroyed.

```sh
ssh root@10.0.0.100 'zpool list -o name,size,alloc,free,cap,dedup rpool'
```

Dedup makes the copy cheap in practice — CT 104's 98 GB grew the pool by 24 GiB — but only
when dedup is on **before** the write. §3.1 and §5.2 set it at the two moments that matter.

### 2.3 Raise the host inotify limit

A CT that exhausts `fs.inotify.max_user_watches` will not finish booting.

```sh
ssh root@10.0.0.100 'sysctl -w fs.inotify.max_user_watches=2000000
  echo fs.inotify.max_user_watches=2000000 > /etc/sysctl.d/99-rmng-inotify.conf'
```

### 2.4 Delete the container-less rows (CT 106 only) — not optional

`haoran-dev-621`, `haoran-dev-635` and `ng-52378be7` have rows but no containers. The
migration reads the home out of the container, so they fail on every pass and stay gen-1 —
and **a leftover gen-1 row makes every later control-server restart re-run the migration,
stopping the whole fleet each time.**

Delete them in the UI before the window, or on the new server afterwards (the gen-2 delete
tolerates a missing container):

```sh
curl -s -XPOST http://<new-ct-ip>:9000/api/delete \
  -H 'Content-Type: application/json' -d '{"id":"haoran-dev-621"}'
```

### 2.5 Write the preset Dockerfiles

Decide the content now (§1.1, §1.2). Gen-1 presets have no Dockerfile field, so the text can
only be set after the new server boots — but every clone builds from it during the window.

---

## 3. Build the new CT

Fresh and privileged. **Never restore a dump as privileged** — it corrupts the LXC namespace
state so every `pct exec` and `docker exec` silently runs against CT files instead of
container files, and it cannot be repaired in place.

### 3.1 The homes dataset

`dedup=blake3` must be set here, at create time. The control-server sets only `mountpoint` on
the datasets it creates; every other property is inherited from this parent.

```sh
ssh root@10.0.0.100 '
  ls -l /dev/zfs                     # note major:minor, 10:249 on this host
  zfs create -o mountpoint=/srv/rmng-homes-105 -o dedup=blake3 rpool/rmng-homes-105
  zfs get -o property,value,source dedup,compression,recordsize rpool/rmng-homes-105
'
```

Expect `dedup blake3 local`, `compression on inherited`, `recordsize 128K default`. Do **not**
set `compression` or `recordsize` to anything else — dedup matches blocks as written, so a
different compression algorithm matches nothing and costs the whole saving.

### 3.2 Create the CT

```sh
ssh root@10.0.0.100 '
pct create 205 local:vztmpl/ubuntu-26.04-standard_26.04-1_amd64.tar.zst \
  --hostname pega-rmng --cores 32 --cpulimit 24 --memory 262144 --swap 10000 \
  --rootfs local-zfs:1024 --net0 name=eth0,bridge=vmbr0,ip=dhcp,type=veth \
  --features nesting=1,keyctl=1,fuse=1 --unprivileged 0 --ostype ubuntu --onboot 1 \
  --mp0 /srv/rmng-homes-105,mp=/srv/rmng-homes
'
```

Match `cores`, `cpulimit`, `memory`, `swap` and rootfs size to the CT being replaced.
`cores` must be at least `docker.cloneCpus` (16 on all three) or the daemon refuses to
create a clone.

`pct set` rejects raw `lxc.*` keys, so append the rest to `/etc/pve/lxc/205.conf` by hand:

```conf
dev0: /dev/dri/renderD128,gid=991,mode=0666
dev1: /dev/kfd,gid=991,mode=0666
lxc.apparmor.profile: unconfined
lxc.mount.entry: /dev/null sys/module/apparmor/parameters/enabled none bind,optional 0 0
lxc.mount.auto: cgroup:mixed proc:rw sys:mixed
lxc.cgroup2.devices.allow: c 10:200 rwm
lxc.cgroup2.devices.allow: c 10:249 rwm
lxc.mount.entry: /dev/net/tun dev/net/tun none bind,create=file
```

Copy `dev0`/`dev1` from the CT being replaced (CT 106 passes only the render node). The
render node is not optional — the setup wizard requires it and the video plane needs it.

**The three AppArmor lines are required on Ubuntu 26.04, privileged or not.** Without them
`docker run` fails with `the docker-default profile could not be loaded`.

`10:249` is the ZFS device node; use whatever `ls -l /dev/zfs` printed. Do **not** bind-mount
the host's `/dev/zfs` with `lxc.mount.entry` — that breaks nested container mount joins.

### 3.3 First boot

```sh
ssh root@10.0.0.100 'pct start 205 && sleep 15 && pct exec 205 -- bash -lc "
  ps -e >/dev/null && echo PROC-OK          # an unmounted /proc poisons every install
  hostname -I
  ls -ld /srv/rmng-homes
  mknod /dev/zfs c 10 249                   # /dev is tmpfs: redo after every CT restart
  ls -l /dev/zfs
"'
```

If `ps -e` fails, restart the CT before installing anything.

### 3.4 Docker, pinned to the source CT's versions

```sh
ssh root@10.0.0.100 'pct exec 205 -- bash -lc "
export DEBIAN_FRONTEND=noninteractive
apt-get update -qq && apt-get install -y -qq curl ca-certificates
install -m 0755 -d /etc/apt/keyrings
curl -fsSL https://download.docker.com/linux/ubuntu/gpg -o /etc/apt/keyrings/docker.asc
chmod a+r /etc/apt/keyrings/docker.asc
. /etc/os-release
echo \"deb [arch=amd64 signed-by=/etc/apt/keyrings/docker.asc] https://download.docker.com/linux/ubuntu \$VERSION_CODENAME stable\" \
  > /etc/apt/sources.list.d/docker.list
apt-get update -qq
V=5:29.6.1-1~ubuntu.26.04~resolute          # CT 104 is 5:29.7.2-… / containerd.io 2.3.3
apt-get install -y -qq docker-ce=\$V docker-ce-cli=\$V \
  containerd.io=2.2.5-1~ubuntu.26.04~resolute \
  docker-buildx-plugin=0.35.0-1~ubuntu.26.04~resolute \
  docker-compose-plugin=5.3.0-1~ubuntu.26.04~resolute
apt-mark hold docker-ce docker-ce-cli containerd.io docker-buildx-plugin docker-compose-plugin
systemctl enable --now docker
docker version --format \"{{.Server.Version}}\"
docker info | grep -A1 \"Storage Driver\"
docker run --rm hello-world | head -2
"'
```

`Storage Driver: overlayfs` with `driver-type: io.containerd.snapshotter.v1` is the right
answer. All three CTs use the containerd image store, so write **no** `daemon.json` storage
keys — no `storage-driver`, and do not disable the snapshotter. Either would give you a
daemon that cannot see a single moved image.

`export DEBIAN_FRONTEND=noninteractive` on every apt step. A run killed mid-dialog leaves an
orphaned apt holding the dpkg lock — run `dpkg --configure -a` before retrying.

Do **not** install `zfsutils-linux` in the CT; its `zfs-dkms` post-install fails under a
shared-kernel LXC. The server image bundles its own `zfs`. Run `zfs` from the Proxmox host.

### 3.5 lxcfs — required

All three CTs run lxcfs, so the new CT must too. Every clone container carries bind mounts
from `/var/lib/lxcfs/proc/*`; those binds travel with the container and resolve on the new
host. Without lxcfs there, `docker cp` cannot read any path out of a clone and the migration
fails with a misleading `404: Could not find the file /home/rmng`.

Ubuntu's unit carries `ConditionVirtualization=!container`, so inside a CT it is silently
skipped until that condition is dropped:

```sh
ssh root@10.0.0.100 'pct exec 205 -- bash -lc "
export DEBIAN_FRONTEND=noninteractive
apt-get install -y -qq lxcfs
mkdir -p /etc/systemd/system/lxcfs.service.d
printf \"[Unit]\nConditionVirtualization=\n\" > /etc/systemd/system/lxcfs.service.d/in-ct.conf
systemctl daemon-reload && systemctl enable --now lxcfs
systemctl is-active lxcfs
ls /var/lib/lxcfs/proc/          # cpuinfo diskstats loadavg meminfo slabinfo stat swaps uptime
"'
```

If you hit the failure after the fact: install lxcfs, then `docker restart rmng` to re-file
the migration. The failing step is the home read, which happens before the old container is
removed, so a failed pass destroys nothing.

### 3.6 CT 105 only — the DNS daemon.json

CT 105 has `/etc/docker/daemon.json` with a `dns` key so clones resolve `*.ts.net`. Copy that
file across (see `PROXMOX-LXC.md` §2c) and `systemctl reload docker`.

---

## 4. Tailscale — release the name

Each CT is its own node on its own tailnet, and the new CT must take the old one's name.
Tailscale appends `-1` to a name already claimed by a live node, so the old node has to go
first. Do this at the same time as §5.1.

```sh
ssh root@10.0.0.100 'pct exec 105 -- bash -lc "
  tailscale serve status          # record it — §6.3 puts it back
  tailscale down
  systemctl disable --now tailscaled
"'
```

Leave the node in the tailnet admin console for now; removing it there is part of §7.

---

## 5. The window

### 5.1 Stop everything on the old CT

```sh
ssh root@10.0.0.100 'pct exec 105 -- bash -lc "
  docker stop \$(docker ps -q)
  systemctl stop docker.socket docker containerd
  systemctl is-active docker docker.socket containerd
  ps -eo comm | grep -E \"dockerd|containerd\" || echo NO-DAEMONS
"'
```

**`containerd` must be in the stop list.** With the containerd image store that daemon owns
`/var/lib/containerd` — the directory about to be copied, including its bolt metadata DB.
Stop `docker.socket` first, or socket activation restarts the daemon mid-copy.

Use `bash -lc`. A bare `sh -c` has no `docker` on `PATH`, so the fleet never stops and the
copy runs against live containers.

Then snapshot, with the daemons down so it is clean:

```sh
ssh root@10.0.0.100 'pct snapshot 105 pre-gen2 --description "before the gen-2 migration"
                     pct listsnapshot 105'
```

This is the whole backup. There are no dumps on this host, and the rollback is the old CT
itself — so **do not destroy an old CT until its replacement has run a working day.**

### 5.2 Clear the new CT's own Docker state

The `hello-world` test wrote an engine id and containerd metadata. Extracting on top of it
mixes two stores.

```sh
ssh root@10.0.0.100 '
  pct exec 205 -- bash -lc "systemctl stop docker.socket docker containerd
                            rm -rf /var/lib/docker /var/lib/containerd"
  # Required, and required NOW — dedup covers only blocks written after it is on.
  zfs set dedup=blake3 rpool/data/subvol-205-disk-0
  zfs get -o property,value,source dedup,compression rpool/data/subvol-205-disk-0
  zpool list -Hp -o alloc rpool          # note this number, compare after §5.3
'
```

The homes parent got its own `dedup=blake3` in §3.1. These are two separate datasets.

### 5.3 Move the Docker state

One host-side pipe, no intermediate tarball.

```sh
ssh root@10.0.0.100 '
  time (pct exec 105 -- tar cf - --numeric-owner -C /var/lib docker containerd \
      | pct exec 205 -- tar xf - --numeric-owner -C /var/lib)
'
```

Reading through the old CT's namespace and writing through the new one is what makes the uid
shift correct: the archive records the inside uids (0, 1000) and the privileged CT extracts
them as the same numbers. No ownership fix step.

`tar` prints `socket ignored` warnings for X11, samba and buildkit sockets. Expected.

### 5.4 Point the config at the homes dataset, and capture the preset vars

The old API drops unknown keys, so this cannot be a `PUT`. Edit the moved file with the
server still stopped — and capture the `vars` in the same pass, because the gen-2 server
drops that retired field the first time it rewrites `config.json`.

```sh
ssh root@10.0.0.100 'pct exec 205 -- python3 - <<PY
import json
path = "/var/lib/docker/volumes/rmng-data/_data/config.json"
c = json.load(open(path))
c["docker"]["homesParent"] = "rpool/rmng-homes-105"
json.dump({p["name"]: p.get("vars", []) for p in c.get("presets", [])},
          open("/root/preset-vars.json", "w"), indent=2)
json.dump(c, open(path, "w"), indent=2)
print("homesParent =", c["docker"]["homesParent"])
print(open("/root/preset-vars.json").read())
PY'
```

The default `homesParent` is `tank/rmng/homes`, which fits nobody here; a first create with
the wrong parent fails with `no such pool`.

### 5.5 Start Docker and check the state arrived

```sh
ssh root@10.0.0.100 'pct exec 205 -- bash -lc "
  systemctl start containerd docker
  docker images  --format \"{{.Repository}}:{{.Tag}}\" | sort
  docker ps -a   --format \"{{.Names}}|{{.State}}\"    | sort
  docker volume ls -q | sort
  ls -n /var/lib/docker/volumes/rmng-data/_data/       # owners must be 0, not 100000
"'
```

Every image, container and volume from §5.1 must be listed.

### 5.6 Boot the gen-2 server

The old run flags plus the homes bind. `:shared` propagation is load-bearing — the datasets
are created from inside this container, and only a shared bind propagates their mounts into
dockerd's namespace.

```sh
ssh root@10.0.0.100 'pct exec 205 -- bash -lc "
docker pull pegasis0/rmng:latest
docker run -d --name rmng --privileged --init --pid=host --restart unless-stopped \
  -p 445:445 -p 2222:2222 -p 9000:9000 -p 9001:9001 -p 9005:9005 \
  -v /var/run/docker.sock:/var/run/docker.sock \
  -v rmng-data:/data -v rmng-sock:/srv/rmng-sock \
  -v /srv/rmng-homes:/srv/rmng-homes:shared \
  -e RUST_LOG=info,tower_http=warn,clip=debug pegasis0/rmng:latest
docker logs -f rmng
"'
```

On boot the server folds the retired config fields, recreates `/dev/zfs` if missing, creates
the shared pool, stops every non-archived clone, and files one `Migrate` op per gen-1 row,
four at a time. After the last one it starts every non-archived clone and re-pushes each
one's stored Claude and Codex tokens. Archived clones stay stopped. Failures log and
continue, and one retry pass runs at the end.

**Follow `docker logs -f rmng`, not the jobs UI.** An operation is pruned from the state 60 s
after it fails and 8 s after it succeeds, so on a fleet-sized window most ops will have
vanished from the UI. The run ends with one summary line:

```
gen-2 migration: 6 passed, 2 failed (haoran-rep-norow-arch, haoran-rep-norow-live), 3 started
```

**The first clone takes several minutes longer than the rest** while the daemon pulls the
current published template. Every later clone reuses the tag — all gen-1 presets resolve to
the same default Dockerfile text, so the whole fleet shares one build. Do not restart
anything while it looks stuck.

If the server is killed mid-migration, the half-built dataset is orphaned and the next pass
fails with `cannot create '<parent>/<id>': dataset already exists`. That failure cleans up
after itself and the retry succeeds; `zfs destroy <parent>/<id>` clears it directly.

### 5.7 Restart the control-server once, after the last op

**Required, not hygiene.** The `data/hosts/<id>` symlinks — read by the `clones` SMB share,
the ledger, the token scanner and the stuck detector — are written by a one-shot boot pass
that runs before any clone has a dataset. Without a restart the directory stays empty for the
rest of the server's life.

Wait for `gen-2 migration: N passed, 0 failed`, then:

```sh
ssh root@10.0.0.100 'pct exec 205 -- bash -lc "
  docker restart rmng
  sleep 20
  ls -l /var/lib/docker/volumes/rmng-data/_data/data/hosts/   # one symlink per managed clone
"'
```

### 5.8 Restore the preset vars to the agent

Write the values captured in §5.4 into every clone's home, `PATH` excluded (§1.2):

```sh
ssh root@10.0.0.100 'pct exec 205 -- python3 - <<PY
import json, os, pathlib, urllib.request
vars_by_preset = json.load(open("/root/preset-vars.json"))
state = json.load(urllib.request.urlopen("http://127.0.0.1:9000/api/state"))
for h in state["hosts"]:
    entries = vars_by_preset.get(h.get("presetName")) or []
    lines = [e["key"] + "=" + e["value"] for e in entries if e.get("key") != "PATH"]
    if not lines:
        continue
    d = pathlib.Path("/srv/rmng-homes/.merged") / h["id"] / ".config/environment.d"
    d.mkdir(parents=True, exist_ok=True)
    f = d / "10-rmng-preset.conf"
    f.write_text("\n".join(lines) + "\n")
    os.chmod(f, 0o600)
    for p in (f, d, d.parent):
        os.chown(p, 1000, 1000)
    print("wrote", f)
PY'
```

Archived clones get the file too; it applies when they are unarchived. Then pick the agent up
on the running clones:

```sh
ssh root@10.0.0.100 'pct exec 205 -- bash -lc "
for c in \$(docker ps --format {{.Names}} | grep -v ^rmng); do
  docker exec -u rmng \$c bash -lc \"export XDG_RUNTIME_DIR=/run/user/1000
    systemctl --user daemon-reload && systemctl --user restart agent-wrapper.service\"
done"'
```

Verify on one clone that the agent process really has them:

```sh
docker exec -u rmng <clone> bash -lc \
  'tr "\0" "\n" < /proc/$(pgrep -u rmng -f agent-wrapper|head -1)/environ | grep TURBO_'
```

---

## 6. Tailscale — join the new CT

### 6.1 Install and join

`tailscale up` prints a URL that has to be opened and approved in a browser signed in to that
CT's tailnet. Nothing else in this runbook needs a person at a keyboard.

```sh
ssh root@10.0.0.100 'pct exec 205 -- bash -lc "
export DEBIAN_FRONTEND=noninteractive
curl -fsSL https://tailscale.com/install.sh | sh
systemctl enable --now tailscaled
tailscale up --hostname=pega-rmng
"'
```

Use the same hostname the old CT had (`ivan-rmng`, `pega-rmng`, `haoran-rmng`). The CT needs
`/dev/net/tun`, which §3.2 already passed in.

### 6.2 Accept the tailnet DNS

```sh
ssh root@10.0.0.100 'pct exec 205 -- bash -lc "
  tailscale set --accept-dns=true
  tailscale status --json | head -20
"'
```

All three CTs run with `accept-dns=true`. Without it the tailnet resolver has no upstream, so
public names stop resolving while `*.ts.net` names keep working — the opposite of the failure
you would expect.

### 6.3 Restore `tailscale serve`

Only CT 105 has a serve config today, and **CT 204 is missing the one CT 104 had** — restore
it there too.

```sh
ssh root@10.0.0.100 'pct exec 205 -- bash -lc "
  tailscale serve --bg http://127.0.0.1:9000
  tailscale serve status
"'
```

Expect `https://<hostname>.<tailnet>.ts.net (tailnet only)` proxying `/` to
`http://127.0.0.1:9000`. That URL is how people reach the dashboard; confirm it loads before
telling anyone the migration is done.

---

## 7. Verify

### 7.1 Rows

```sh
curl -s http://<new-ct-ip>:9000/api/state | python3 -c '
import sys, json
s = json.load(sys.stdin)
for h in sorted(s["hosts"], key=lambda h: h["id"]):
    print("{:<24} ds={} bt={} arch={} hl={} parent={} preset={} fwd={}".format(
        h["id"], h.get("dataset"), h.get("baseTag"), h.get("archived"),
        h.get("headless"), h.get("parent"), h.get("presetName"),
        len(h.get("forwards", []))))
print("muted:", s["mutedClones"])
print("board:", [(c["id"], c["cloneIds"]) for c in s["boardColumns"]])'
```

Every managed row must have a `dataset` and a `baseTag`. A row without them did not migrate
— go back to §2.4.

### 7.2 Homes and ownership

```sh
ssh root@10.0.0.100 'pct exec 205 -- bash -lc "
  ls /srv/rmng-homes/                       # one dir per clone, plus .merged .shared .skeleton
  ls /srv/rmng-homes/.merged/               # one merged view per clone
  ls -l /var/lib/docker/volumes/rmng-data/_data/data/hosts/
"'
```

Then check a home landed owned by the clone user, not root. A blind `chown -R` is **not** a
correct repair — a healthy home has a small legitimate minority of non-1000 files.

```sh
ssh root@10.0.0.100 'pct exec 205 -- bash -lc "
  find /srv/rmng-homes/<id>/upper -xdev -uid 1000 | wc -l    # want ~all of them
  find /srv/rmng-homes/<id>/upper -xdev ! -uid 1000 | wc -l  # want a small minority
  stat -c \"%u %g %a\" /srv/rmng-homes/.merged/<id>          # want 1000 1000 755
"'
```

### 7.3 Clones

```sh
ssh root@10.0.0.100 'pct exec 205 -- bash -lc "
  docker ps -a --format \"{{.Names}} {{.State}}\"
  for c in \$(docker ps --format \"{{.Names}}\" | grep -v ^rmng); do
    printf \"%s claude=%s codex=%s\n\" \$c \
      \"\$(docker exec -u rmng \$c test -f /home/rmng/.claude/.credentials.json && echo yes || echo NO)\" \
      \"\$(docker exec -u rmng \$c test -f /home/rmng/.codex/auth.json && echo yes || echo NO)\"
  done
"'
```

Archived clones must be `exited`. Every other managed clone must be `running` with both
credential files present.

### 7.4 A migrated clone's desktop

`/api/state` must show `daemonConnected: true` for every started headed clone. Then take a
real frame:

```sh
curl -s -XPOST http://<new-ct-ip>:9000/api/hosts/<clone>/mcp \
  -H 'Content-Type: application/json' -d '{"tool":"screenshot","args":{}}' \
  | python3 -c '
import sys, json, base64
r = json.load(sys.stdin)
items = r if isinstance(r, list) else r.get("content", [])
img = next(i for i in items if i.get("type") == "image")
open("/tmp/frame.jpg","wb").write(base64.b64decode(img["data"]))
print("wrote /tmp/frame.jpg")'
```

It must show the operator's real desktop — wallpaper, dock, their own apps — not the template
default.

### 7.5 The reboot test

The only check that exercises `remount_all`, and the one worth repeating on every CT.

```sh
ssh root@10.0.0.100 'pct reboot 205'
```

Then, with nothing touched afterwards: every dataset mounted, every overlay rebuilt, every
running clone on its full home, and the server log carrying one `binds the live home overlay`
line per clone it had to restart. `mknod /dev/zfs` is redone by the server on boot.

### 7.6 End to end

```sh
RMNG_E2E_SERVER=http://<new-ct-ip>:9000 RMNG_E2E_PRESET=<a preset name> \
  cargo run -p control-client --bin rmng_e2e
```

It creates, prebuilds, forks, rebases, archives, unarchives and deletes, asserts a live frame
from each new clone, and temporarily rewrites that preset's startup script, restoring it on
the way out.

---

## 8. After the window

### 8.1 Tell people the new address

The new CT takes a fresh DHCP lease, so its LAN address changes. The Tailscale name does not
(§6), so anyone using `https://<hostname>.<tailnet>.ts.net` needs no change — which is the
reason to finish §6.3 before announcing anything.

Remove the old node from the tailnet admin console once the new one is answering.

The old CT stays **stopped and not destroyed** until the new one has run a full working day.

### 8.2 Reclaim the dead images

Every clone now runs the one derived tag `rmng-p-<hash>`. The old per-clone commit images are
referenced by nothing and are the bulk of the CT's disk. Nothing deletes them for you.

```sh
ssh root@10.0.0.100 'pct exec 205 -- bash -lc "
  docker images --format \"{{.Repository}}:{{.Tag}}\"   # read this list first
  docker rmi <each dead clone-source tag>
  docker system df
"'
```

Do not `docker image prune -a`: it would take `pegasis0/rmng-template:latest` and the derived
tag out from under the running clones.

### 8.3 Inner Docker re-pulls

Each clone's inner Docker starts empty — the `rmng-dind-*` and `rmng-ctd-*` volumes are
deleted during migration. The first inner build or `docker run` in each clone re-pulls
through the `rmng-registry` mirror. Expect one slow first build per clone.

---

## 9. Rollback

**Start the old CT.** The migration only read it, so it is still the deployment it was. Stop
the new CT first — two servers must not drive two daemons at once — then re-enable tailscaled
on the old CT (`systemctl enable --now tailscaled`, then `tailscale up`), and destroy the new
CT and its homes dataset.

`pct rollback 105 pre-gen2` also undoes the `docker stop` / `systemctl stop` writes, if the
old CT looks wrong when it comes up.

There is nothing finer than this — no per-clone rollback, no partial undo.
