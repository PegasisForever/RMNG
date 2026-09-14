> **Archived.** Migration done 2026-09-13/14: CT 104 → 204 (8/8 clones), CT 106 → 206 (101/101), CT 105 → 205 (24/24). Kept as historical record only — the gen-1 code paths it refers to are deleted from the repo.
>
> **Rollback (§9) still reads ARMED at archive time.** The status below leaves the old CTs stopped (`onboot 0`), snapshotted (`pre-gen2`) as the rollback, and no repo commit records their destruction — verify on the Proxmox host before destroying anything. Open item: `tailscale serve` not set on CT 204/206 (Serve disabled for those tailnets); CT 205 has it.

# Runbook — migrating CT 104, CT 105 and CT 106 from gen-1 to gen-2

**Scope.** These three containers only, all on the Proxmox host `10.0.0.100`. Not a general
recipe.

**What happens.** A gen-1 clone keeps its home inside the container's writable layer; a gen-2
clone keeps it on a ZFS dataset outside the container. The new control-server cannot run
gen-1 clones, so each CT gets one window: build a new privileged CT, move the Docker state
into it, start the new server, and let it rewrite every clone.

**Status. All three are done.**

    CT 104 -> CT 204  `ivan-rmng`,   LAN 10.0.0.235, tailnet 100.116.208.33
                      2026-09-13, 8 of 8 clones passed
    CT 106 -> CT 206  `haoran-rmng`, LAN 10.0.0.210, tailnet 100.91.21.92
                      2026-09-13, 101 of 101 clones passed
    CT 105 -> CT 205  `pega-rmng`,   LAN 10.0.0.129, tailnet 100.99.199.38
                      2026-09-14, 24 of 24 clones passed

All three old CTs are stopped, `onboot 0`, docker and tailscale disabled, snapshotted, and are
the rollback (§9). One item is open on CT 204 and CT 206: `tailscale serve` is not set, because
Serve is disabled for those tailnets and enabling it needs a browser (§6.3). Each dashboard
answers on both of its addresses meanwhile. **CT 205 has its serve** — Serve was already enabled
on `tail0e863`, which is why §6.3 is a per-tailnet caveat and not a general one.

**Order was CT 104, CT 106, CT 105.** CT 106 was taken second rather than last: the parallel
copy in §5.3 removed the reason to fear its size, and it was the CT under the most pressure.

## The three boxes

| | CT 104 → 204 ✅ | CT 105 → 205 ✅ | CT 106 → 206 ✅ |
| --- | --- | --- | --- |
| IP of the OLD CT | 10.0.0.206 | 10.0.0.15 | 10.0.0.180 |
| IP of the NEW CT | 10.0.0.235 | 10.0.0.129 | 10.0.0.210 |
| rootfs used | 101 GB | 564 GB | 917 GB |
| Docker | 29.7.2 / containerd.io 2.3.3 | 29.6.1 / containerd.io 2.2.5 | 29.6.1 / containerd.io 2.2.5 |
| clone rows | 8 | 24 | 104 |
| hostname prefix | `ivan-` | `pega-` | `haoran-` |
| presets | 1 (`Medi`) | 4 (`medi`, `hyperhost`, `wealthstack`, `personal`) | 1 (`talktomedi`) |
| published ports | 445 2222 9000 9001 9005 | 445 2222 9000 9001 9005 | 445 2222 9000 9001 9005 |
| `/etc/docker/daemon.json` | none | `{"dns": […]}` | none |
| GPU devices to pass | render node + `/dev/kfd` | render node + `/dev/kfd` | render node only |
| tailnet | `tail8d43a5.ts.net` | `tail0e863.ts.net` | `tailc2613e.ts.net` |
| `tailscale serve` | — (restore it, §6.3) | yes → `:9000`, restored ✅ | none |

CT 105 also published 9002 and 9003 with nothing behind them; they were dropped from the new
run command.

Budget, **with §5.3 run in parallel**: the copy was 30 minutes for CT 106's 905 GiB and 40
minutes for CT 105's 564 GB (500 tasks, 0 failures); the per-clone migration in §5.7 was
another 2 h 30 m for CT 106's 101 clones and 1 h 01 m for CT 105's 24. Do not budget from size
— see §5.3, the cost tracks file count, and a serial copy of the same data would have run 4 to
6 hours.

**Pool space was never the constraint.** CT 105 started with 853 GiB free and *ended with 864
GiB* — dedup held the whole 564 GB copy to a few GiB, and §8.2's image reclaim gave back more
than the copy took. Do the §2.2 check anyway, but do not plan around needing a second full
copy's worth of headroom.

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

**An empty `lost:` line is the failure, not a clean result.** On CT 106 the two `bash -lc`
wrappers printed `Failed to create stream fd` and produced nothing, so the script cheerfully
reported that no packages were lost. Check both files hold ~1300 lines before believing it.
Running `dpkg-query` as the entrypoint instead of through a login shell works:

```sh
pct exec "$CT" -- docker exec "$CLONE" dpkg-query -W -f='${Package}\n'
pct exec "$CT" -- docker run --rm --entrypoint dpkg-query pegasis0/rmng-template:latest \
    -W -f='${Package}\n'
```

CT 106's real answer, for reference: it loses `rsync` and `sqlite3` at the package level, plus
`/usr/local/bin/sops` and `/usr/local/bin/aws`; it gains clang-21, firefox, google-cloud-cli,
llvm-21, onlyoffice, papirus-icon-theme and mission-center. Its `talktomedi` preset Dockerfile
is CT 204's `Medi` one plus one `apt-get install rsync sqlite3` line.

CT 105's answer, for reference — **measure every preset separately there, they differ a lot.**
Its clones ran nine different base images. The set common to all of them is `avahi-utils
flameshot gnome-themes-extra grim htop rclone rsync tree wl-clipboard` plus flameshot's qt6
stack; on top of that `hyperhost` carries the whole gtk4/gstreamer/wayland/drm **-dev**
toolchain (that is the preset `pega-rmng-development` builds RMNG under), and `wealthstack`
carries gtk3 + webkit2gtk **-dev** for `pega-git-visualizer`. `gained:` was empty for every
clone — these bases are strict supersets of the template.

Two CT 105 losses that no Dockerfile can carry, both worth naming out loud rather than
discovering later:

- `git-visualizer`, an apt package installed into one clone's base. One image serves a whole
  preset, so a per-clone package has nowhere to live.
- `/usr/local/bin/sendrec`, a hand-built 2.6 MB binary in 9 of 11 live clones and in no
  package repo. It went into `~/.local/bin/sendrec` in all 24 homes instead: that is back on
  `PATH` via Ubuntu's default `~/.profile`, and unlike `/usr/local/bin` the home survives
  every future rebase. Copy it out with `docker cp <clone>:/usr/local/bin/<bin> …` **before**
  §5.1, while the old CT still has it.

**The template ships no node at all** — not in any version of it so far. Every preset
Dockerfile has to install one; the CT 204 `Medi` file is the reference. Check with
`docker run --rm --entrypoint bash pegasis0/rmng-template:latest -c 'command -v node npm'`
before assuming otherwise.

**NodeSource silently gives you the wrong node once a major goes EOL.** CT 105's `wealthstack`
clones run node 20, so its Dockerfile started as the CT 204 block with `node_20.x` — and node
20 is past EOL, so `deb.nodesource.com/node_20.x` no longer lists a `nodejs` package. apt fell
straight through to **Ubuntu's own `nodejs` 22.22.1**, which passed the `/usr/bin/node
--version` check at the end of that `RUN` and then broke the *next* step with `npm: not
found`, because Ubuntu splits npm into a separate package. The failure names npm, not node,
and points at the corepack line rather than the real culprit two steps up.

For any EOL major, take the official tarball instead of NodeSource, and assert the version so
a wrong one fails the build rather than shipping quietly:

```dockerfile
RUN curl -fsSL -o /tmp/node.tar.xz \
      https://nodejs.org/dist/v20.20.2/node-v20.20.2-linux-x64.tar.xz \
 && tar -xJf /tmp/node.tar.xz -C /usr/local --strip-components=1 \
      --exclude=CHANGELOG.md --exclude=LICENSE --exclude=README.md \
 && rm -f /tmp/node.tar.xz \
 && /usr/local/bin/node --version | grep -qx v20.20.2 \
 && /usr/local/bin/npm --version
```

That lands node in `/usr/local/bin`, not `/usr/bin` — so the §8.4 check becomes
`command -v node` printing *either* path. The node-20 tarball also bundles corepack already,
so the corepack step needs `npm install -g corepack --force` or it fails on the existing
`/usr/local/bin/corepack`.

Then choose, per CT:

- **Accept the loss** and transcribe what matters into the preset Dockerfile as `RUN` lines
  on top of `FROM pegasis0/rmng-template:latest`. This is the intended path.
- **Publish the base**: `docker tag pega-template13:latest pegasis0/pega-template13:latest`,
  push it, and set the preset Dockerfile to `FROM pegasis0/pega-template13:latest`. Costs one
  upload of ~28 GB of commit layers.

A local-only tag cannot work: preset builds force `--pull`, and the CT's `rmng-registry` is a
pull-through proxy that refuses pushes.

### 1.2 The preset vars — nothing to decide

Gen-2 carries a preset `vars` list of its own, with the same `{key, value}` shape gen-1 used,
so a gen-1 `config.json` keeps its vars simply by being read. There is no capture step and no
restore step: the server writes them into each clone's `/etc/environment` at create and on
every resync.

Do **not** transcribe them into the preset Dockerfile. A Dockerfile `ENV` reaches `docker exec`
and nothing else — systemd is PID 1 in a clone and does not hand its own environment to the
services it starts, so an `ENV` never reaches an SSH login or the desktop session.
`/etc/environment` reaches all three: `pam_env` loads it for SSH logins and for the lingering
user manager (through the `/usr/lib/environment.d/99-environment.conf` symlink, so the GNOME
session and every unit under it inherit it), and the exec that starts the agent sources it
explicitly.

`PATH` needs no special handling. It used to, with per-shell rc drop-ins, only so fish would
find a node installed by nvm inside the home. Put node in the preset Dockerfile instead.

**And a gen-1 preset may carry a `PATH` var that has to be DROPPED, not read.** CT 106's
`talktomedi` preset had `PATH=/home/rmng/.nvm/versions/node/v26.4.0/bin:…`. Carried across it
would point every shell at a directory §8.4 deletes. CT 204's preset had no such var, so CT 106
was the first CT where §5.4 had to edit the vars rather than leave them alone. Replace it with
`COREPACK_HOME=/opt/corepack` and check the printed var list in §5.4 for a `PATH` entry.

One gap to know about: a bare `docker exec` does not run PAM, so it gets none of this — that
is the one entry path `/etc/environment` does not reach on its own. The server's own exec
handles it the same way you should:

```sh
docker exec -u rmng <clone> bash -lc 'set -a; . /etc/environment; set +a; <cmd>'
```

Without the source line a debugging `docker exec` sees an empty environment and can mislead
you into thinking the delivery failed. The agent, SSH and the desktop are all fine.

This needs a server built after the preset-vars change. The image logs its revision on boot —
`docker logs rmng | grep "running image revision"` — and the Settings preset card shows an
"Environment variables" editor when it is new enough.

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

# How much data moves. Read it off ZFS, NOT with du: walking a big CT takes the better part of
# an hour and tells you nothing more. Docker is essentially all of the rootfs.
$PVE "zfs list -o name,used,refquota rpool/data/subvol-$CT-disk-0"

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

### 2.3 Raise the host inotify limit — and persist it

A CT that exhausts the host's inotify budget will not finish booting. The failure names
neither inotify nor the limit: `systemd-networkd` exits `code: 28 (No space left on device)`,
restarts five times, gives up, and the CT comes up with **no IP at all**. Nothing is out of
disk.

```sh
ssh root@10.0.0.100 'sysctl -w fs.inotify.max_user_watches=2000000
  sysctl -w fs.inotify.max_user_instances=1048576
  printf "fs.inotify.max_user_watches=2000000\nfs.inotify.max_user_instances=1048576\n" \
    > /etc/sysctl.d/99-rmng-inotify.conf'
```

Write the file, not just the live value — this host was found back at the 65536 default with
no `/etc/sysctl.d` entry, so a `sysctl -w` alone had been lost. Check
`sysctl fs.inotify.max_user_watches` before every window; 14 running CTs is enough to hit it.

**Cause found after the CT 105 window — it was the clones.** A clone runs privileged with no
user namespace, so `systemd-sysctl` inside it writes the *host's* sysctls: inotify limits live
in the init-userns ucounts and the container shares them. The clone image carries GNOME
localsearch's `/usr/lib/sysctl.d/30-localsearch.conf`, which pins
`fs.inotify.max_user_watches = 65536` — written to raise the old 8192 default, and now
lowering a host that deliberately raised it. **Every clone start stomped the host.** That is
why only `max_user_watches` ever reverted while `max_user_instances` held, and why it looked
random: it tracked whenever a clone last booted.

Proof, if you need to re-confirm it: `sysctl -w fs.inotify.max_user_watches=1234567`,
`docker restart <any clone>`, wait ten seconds, read it back — 65536.

Fixed in `template/setup/10-desktop.sh` with `ln -sf /dev/null
/etc/sysctl.d/30-localsearch.conf` (the systemd mask; `/etc` wins over `/usr/lib` by name, and
it survives a package upgrade where `rm` would not). **Clones built before that template
change still stomp it** — mask it in a running one with
`docker exec <clone> ln -sf /dev/null /etc/sysctl.d/30-localsearch.conf`, which holds until
that clone is next rebased. Keep doing the `sysctl -w` check until every clone is on a
template newer than the fix.

Recovery, if a CT is already stuck this way: raise the limit, then
`pct exec <id> -- systemctl reset-failed && systemctl restart systemd-networkd`. No reboot.

### 2.4 Delete the container-less rows — not optional

Check for rows with no container, and delete them. CT 106 had three (`haoran-dev-621`,
`haoran-dev-635`, `ng-52378be7`); CT 104 had none. The migration reads the home out of the
container, so such a row fails on every pass and stays gen-1 — and **a leftover gen-1 row makes
every later control-server restart re-run the migration, stopping the whole fleet each time.**

Put this in a file on the old CT and run it there — the quoting does not survive being typed
through `ssh` and `pct exec`:

```python
import json, subprocess, urllib.request

conts = set(subprocess.run(["docker", "ps", "-a", "--format", "{{.Names}}"],
                           capture_output=True, text=True).stdout.split())
rows = {h["id"] for h in
        json.load(urllib.request.urlopen("http://127.0.0.1:9000/api/state"))["hosts"]}
print("rows:", len(rows), " containers:", len(conts))
print("ROWS WITH NO CONTAINER:", sorted(rows - conts))
```

The gen-1 API delete handles them too, despite the missing container — it simply sits at
`step: "removing the container"` for about a minute before the row goes. Delete them in the UI
or over the API before the window, or on the new server afterwards (the gen-2 delete tolerates
a missing container as well):

```sh
curl -s -XPOST http://<new-ct-ip>:9000/api/delete \
  -H 'Content-Type: application/json' -d '{"id":"haoran-dev-621"}'
```

### 2.4b Check every row's preset actually exists

A row can name a preset that is not in `config.json`. CT 105 had two — `pega-command-center`
and `pega-daily-catch-up` both carried `presetName: "personal"` with no `personal` preset
anywhere. `preset_dockerfile()` falls back to the bare template for an unknown name, so those
rows would have rebuilt onto an image with no node and picked up no vars at all, silently.

```sh
pct exec <ct> -- python3 -c '
import json, urllib.request
c = json.load(open("/var/lib/docker/volumes/rmng-data/_data/config.json"))
have = {p["name"] for p in c.get("presets", [])}
used = {h.get("presetName") for h in
        json.load(urllib.request.urlopen("http://127.0.0.1:9000/api/state"))["hosts"]}
print("preset names with no preset:", sorted(n for n in used - have if n))'
```

Fix it by adding the missing preset in §5.4 rather than by editing the rows — one new entry
copied from the nearest existing preset, with its own Dockerfile and no project vars.

### 2.5 Write the preset Dockerfiles

Decide the content now (§1.1, §1.2). A gen-1 preset has no Dockerfile field, but the gen-2
server reads one straight out of `config.json` — so §5.4 writes it into the moved file while
the server is still stopped, and the migration then builds every clone from it in the same
pass.

Do not put this off until after the window. `migrate_one` resolves the image through
`preset_dockerfile(row.presetName)`; if the field is still empty the whole fleet migrates onto
the bare `FROM pegasis0/rmng-template:latest` default, and putting the vars back afterwards
costs a rebase and a rebuild of every clone.

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

Then turn on dedup, before the CT is started and before anything is written to it:

```sh
ssh root@10.0.0.100 'zfs set dedup=blake3 rpool/data/subvol-205-disk-0
                     zfs get -o property,value,source dedup,compression rpool/data/subvol-205-disk-0'
```

Dedup covers only blocks written after it is on, so the earliest possible moment is the right
one — that way Docker and its images dedup too, not just the §5.3 copy.

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

**`systemctl is-active` is not the check — the directory listing is.** On CT 206 the unit
reported `active` and the daemon logged its whole api_extensions list, while
`/var/lib/lxcfs/proc/` did not exist and `mount | grep lxcfs` was empty. A plain
`systemctl restart lxcfs` fixed it: 11 mounts, `proc` and `sys` present. CT 204 did not show
this, so it is start-order dependent rather than a one-off. Restart once if the listing is
empty, and only believe the `ls`.

CT 205 hit it too — `is-active: active`, `/var/lib/lxcfs/proc/` absent, one restart gave 11
mounts. Two of three CTs now, so just make the restart unconditional and check the listing
after it.

If you hit the failure after the fact: install lxcfs, then `docker restart rmng` to re-file
the migration. The failing step is the home read, which happens before the old container is
removed, so a failed pass destroys nothing.

### 3.6 CT 105 only — the DNS daemon.json

CT 105 has `/etc/docker/daemon.json` with a `dns` key so clones resolve `*.ts.net`. Copy that
file across (see `PROXMOX-LXC.md` §2c) and `systemctl reload docker`.

---

## 4. Tailscale — stop the old daemon and take its identity

Each CT is its own node on its own tailnet, and the new CT does not re-enrol — it takes over
the old node (§6.1). Two daemons must never hold one node key at once, so the old one stops
first. Do this at the same time as §5.1.

```sh
ssh root@10.0.0.100 '
pct exec 105 -- bash -lc "
  tailscale serve status          # record it — §6.3 puts it back
  tailscale status --json | head -20   # record the node name and tailnet IP
  tailscale down
  systemctl disable --now tailscaled"
# the node identity; §6.1 unpacks it into the new CT
pct exec 105 -- tar cf - --numeric-owner -C /var/lib tailscale > /root/ct105-tailscale.tar
'
```

Take the tar **after** the daemon is down, so the state file is not being written as it is
read. Leave the node in the tailnet admin console — with the identity moved there is nothing
to remove, and deleting it there would revoke the key the new CT is about to use.

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
  # Set in §3.2. Confirm it, do not discover it here — dedup never applies retroactively.
  zfs get -o property,value,source dedup,compression rpool/data/subvol-205-disk-0
  zpool list -Hp -o alloc rpool          # note this number, compare after §5.3
'
```

`dedup blake3 local` is the only acceptable answer. If it says `off`, the CT has to be rebuilt
from §3.2 — there is no way to dedup what is already written.

The homes parent got its own `dedup=blake3` in §3.1. These are two separate datasets.

### 5.3 Move the Docker state — in parallel

Host-side pipes, no intermediate tarball. Reading through the old CT's namespace and writing
through the new one is what makes the uid shift correct: the archive records the inside uids
(0, 1000) and the privileged CT extracts them as the same numbers. No ownership fix step.

**Do not run it as one pipe.** One `tar` is the entire limit. Measured mid-copy on CT 106:

| | |
| --- | --- |
| extracting `tar` in the new CT | 100% of one core, 88% of it user time |
| reading `tar` in the old CT | 5% of one core |
| host | 32 cores, 31 idle |
| pool | one NVMe, 5 MB/s of reads, **zero** iowait |

The bulk of a gen-1 CT is hundreds of thousands of tiny files in the clone home directories
(`node_modules`, git objects), and one process creating them one at a time is CPU bound long
before the disk notices. A serial pipe opens at ~185 MB/s on the big image layers and then
falls to 20-40 MB/s for hours, so **an ETA taken in the first 15 minutes will be wrong by
hours**. Budget by file count, not by size.

Split it instead. The work divides cleanly, because the homes live in independent snapshot
directories — 546 of them on CT 106. Eight streams did 679 GB in **30 minutes** where serial
was tracking to 4-6 hours: 562 tasks, 0 failures, 257 minutes of CPU in 31 minutes of wall
time, 8.4x. Eight is the right number: at that point 22 of the 32 cores were busy.

Two rules the split has to obey:

1. **Copy the parent directories FIRST, with `--no-recursion`.** A worker that reaches a
   missing parent creates it with tar's default mode and never comes back to fix it. Extract
   `docker`, `docker/volumes`, `containerd`,
   `containerd/io.containerd.snapshotter.v1.overlayfs` and its `snapshots` as bare entries
   before any worker starts; tar does not touch the metadata of a directory that already
   exists and is not an archive member.
2. **One task per snapshot directory** — not fixed buckets. Sizes vary by two orders of
   magnitude, so a worker pool pulling the next directory when it frees up is what balances
   the streams.

```sh
# one task: "<KIND> <path relative to /var/lib>"
kind=${1%% *}; path=${1#* }
# The `docker` task hands every volume off to a task of its own, so it excludes them all here
# — including rmng-ctd-*/rmng-dind-*, which still get copied, just by their own workers.
# `docker/volumes/metadata.db` is NOT a volume and must stay in this task.
case "$kind" in EXCL) ex=(--exclude=docker/volumes/rmng-*) ;; PLAIN) ex=() ;; esac
pct exec 105 -- tar cf - --numeric-owner -C /var/lib "${ex[@]}" "$path" \
  | pct exec 205 -- tar xf - --numeric-owner -C /var/lib
```

The task list: one `EXCL docker` (mostly metadata once the volumes are split out), one per
volume under `docker/volumes/`, one per `containerd/` subdirectory except the overlayfs
snapshotter, one for that snapshotter's `metadata.db`, and one per
`containerd/io.containerd.snapshotter.v1.overlayfs/snapshots/<N>`. Then
`xargs -a tasks -d '\n' -n 1 -P 8 ./copy-one.sh`.

`tar` prints `socket ignored` warnings for X11, samba, MySQL and buildkit sockets. Expected —
CT 106 logged 1319 of them and nothing else.

**Do NOT drop the `rmng-ctd-*` and `rmng-dind-*` volumes from the task list.** There are two
per clone — 202 on CT 106, about 200 GB — and the migration empties them on arrival (§8.3),
which makes skipping them look free. It is not. The migration reads each home with `docker cp`, and Docker
resolves **every mount a container declares** before it will read anything out of it. With the
volumes gone all 101 clones failed at once:

```
operation failed: downloading /home/rmng from <clone>:
  Docker responded with status code 500: get rmng-ctd-<clone>: no such volume
```

Their *content* is discarded; their *existence* is not optional. (That `remove_volume`
tolerates a 404 says nothing about this — that is the delete path, not the read path.) Nothing
is damaged if you hit it: the read is the first step, so the pass ends 0 of N migrated with
every clone intact. To skip them anyway, recreate them empty before booting the server —

```sh
for c in $(docker ps -a --format '{{.Names}}' | grep -v '^rmng'); do
  docker volume create "rmng-ctd-$c"; docker volume create "rmng-dind-$c"
done
docker cp <a-clone>:/home/rmng/.bashrc /tmp/probe   # must succeed BEFORE §5.7
```

— which is exactly what the migration creates for a new clone. Verify with that `docker cp`
before starting the server, not after.

### 5.4 Point the config at the homes dataset, and write the Dockerfiles

Two edits to the moved `config.json`, with the server still stopped. The old API drops unknown
keys, so neither can be a `PUT`.

The preset `vars` usually need nothing here (§1.2) — gen-2 reads the gen-1 field as it stands.
The exception is a `PATH` var: CT 106's preset carried one pointing into `~/.nvm`, which §8.4
deletes. Drop it and add `COREPACK_HOME=/opt/corepack`. Read the printed var list below and
check for `PATH` before moving on.

**All three of CT 105's presets had one**, so assume every preset has a `PATH` var until you
have looked. Dropping it outright is safe even though these also listed `~/.cargo/bin` and
`~/.local/bin` — which hold cargo, rustc, rustup, uv and the `claude`, `codex` and `pi`
binaries. The server execs with `bash -lc`, and a login shell re-adds `~/.local/bin` from
Ubuntu's default `~/.profile` and `~/.cargo/bin` from rustup's `.bashrc` line. Verified on
CT 205 after the drop: `command -v claude` → `/home/rmng/.local/bin/claude` in all 11 running
clones.

Put the script in a file and `pct push` it — a `<<PY` heredoc through `ssh` and `pct exec`
mangles the quoting.

```python
import json

PATH = "/var/lib/docker/volumes/rmng-data/_data/config.json"
HOMES_PARENT = "rpool/rmng-homes-105"          # the dataset created in §3.1

c = json.load(open(PATH))
c["docker"]["homesParent"] = HOMES_PARENT

# Every preset needs Dockerfile text, because the migration builds each clone from it. Keep it
# to the base unless §1.1 found something worth carrying: env does NOT belong here.
for p in c.get("presets", []):
    p.setdefault("dockerfile", "FROM pegasis0/rmng-template:latest")

json.dump(c, open(PATH, "w"), indent=2)
print("homesParent =", c["docker"]["homesParent"])
for p in c.get("presets", []):
    print(p["name"], "vars:", [v["key"] for v in p.get("vars", [])])
    print(p["dockerfile"])
```

```sh
ssh root@10.0.0.100 'pct push 205 /root/ct205-config.py /root/ct205-config.py
                     pct exec 205 -- python3 /root/ct205-config.py'
```

Read the printed Dockerfile before moving on — it decides the image every clone is about to be
rebuilt on, and after §5.7 has run, changing it costs a rebase of the whole fleet. The printed
var names are the ones each clone will get in `/etc/environment`.

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

### 5.6 Build the preset images BEFORE the server boots

The gen-2 server files its `Migrate` ops the moment it starts, so there is no window to press
Rebuild in the UI — the first clone of each preset ends up pulling the template and building
the image while it migrates. That is where "the first clone takes several minutes longer"
comes from, and it puts a network pull inside the window: a slow or failed pull is then a
failed migration rather than a step you retry on its own.

Build them here instead, with the server still stopped. The derived tag is a pure function of
the Dockerfile text, so a tag built by hand is the exact one the migration looks for, and it
finds it already present.

```python
import json, subprocess

def fnv1a64(b):
    h = 0xcbf29ce484222325
    for x in b:
        h ^= x
        h = (h * 0x100000001b3) & 0xFFFFFFFFFFFFFFFF
    return h

c = json.load(open("/var/lib/docker/volumes/rmng-data/_data/config.json"))
for p in c["presets"]:
    text = p["dockerfile"]
    tag = "rmng-p-%016x" % fnv1a64(text.rstrip().encode())   # matches wire::dockerfile_tag
    print("==", p["name"], "->", tag)
    subprocess.run(["docker", "build", "--pull", "-t", tag, "-"],
                   input=text.encode(), check=True)
```

```sh
ssh root@10.0.0.100 'pct push 205 /root/ct205-prebuild.py /root/ct205-prebuild.py
                     pct exec 205 -- python3 /root/ct205-prebuild.py
                     pct exec 205 -- docker images --format "{{.Repository}}" | grep rmng-p'
```

`--pull` on purpose: this is the one moment a fresh base is wanted, and it is outside the
window's critical path. `rstrip()` matches the `trim_end()` the server canonicalizes with —
without it a trailing newline gives a different tag and the migration builds anyway.

Check the tag it prints against the one a clone ends up on (§7.1 `baseTag`). If they differ,
the text changed between the two steps and the build was wasted, not wrong.

### 5.7 Boot the gen-2 server

The old run flags plus the homes bind. `:shared` propagation is load-bearing — the datasets
are created from inside this container, and only a shared bind propagates their mounts into
dockerd's namespace.

**Rename the moved gen-1 server first.** §5.3 brought the old `rmng` container across with
everything else, so `docker run --name rmng` would fail with `Conflict. The container name
"/rmng" is already in use`. Renaming rather than deleting keeps it as the in-CT rollback.

```sh
ssh root@10.0.0.100 'pct exec 205 -- bash -lc "
docker rename rmng rmng-gen1
docker update --restart=no rmng-gen1        # it must never come back on its own
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
`MIGRATE_CONCURRENCY` at a time (`crates/control-server/src/jobs.rs`).

**That constant should be 8, not 4.** Measured on CT 206 while 4 ran: the host was 48% idle at
load 17 of 32 cores, so each migrating clone costs about 4 cores and 8 is what fills the
machine. Raising it needs a rebuild, so decide before the window rather than during it. Do not
go past 8 without measuring — the plain file copy in §5.3 stopped gaining there too.
CT 105 was deliberately left at 4: 24 rows at 4 at a time came to 1 h 01 m, and the ~20 minutes
8 would have saved did not justify building and pushing a one-off image on the last CT. It is
worth raising only for a CT on CT 106's scale. After the last one it starts every non-archived clone and re-pushes each
one's stored Claude and Codex tokens. Archived clones stay stopped. Failures log and
continue, and one retry pass runs at the end.

**Follow `docker logs -f rmng`, not the jobs UI.** An operation is pruned from the state 60 s
after it fails and 8 s after it succeeds, so on a fleet-sized window most ops will have
vanished from the UI. The run ends with one summary line:

```
gen-2 migration: 6 passed, 2 failed (haoran-rep-norow-arch, haoran-rep-norow-live), 3 started
```

With §5.6 done there is no build in here at all: every clone finds its tag already present
and goes straight to copying its home. Skip §5.6 and the first clone of each preset stalls for
several minutes while the daemon pulls the template and builds — do not restart anything while
it looks stuck.

If the server is killed mid-migration, the half-built dataset is orphaned and the next pass
fails with `cannot create '<parent>/<id>': dataset already exists`. That failure cleans up
after itself and the retry succeeds; `zfs destroy <parent>/<id>` clears it directly.

### 5.8 Restart the control-server once, after the last op

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

## 6. Tailscale — move the node to the new CT

### 6.1 Install, and carry the node identity across

Do **not** run a bare `tailscale up`. It would enrol a *new* node, which needs a browser
approval and gets the name back only because §4 freed it. Move the old CT's
`/var/lib/tailscale` instead: the new CT then *is* the same node — same tailnet IP, same
name, same ACL grants — and no person is needed at a keyboard.

Save the state in §4, in the same step that stops the old daemon:

```sh
ssh root@10.0.0.100 'pct exec 105 -- tar cf - --numeric-owner -C /var/lib tailscale \
                     > /root/ct105-tailscale.tar'
```

Reading through the old CT and writing through the new one keeps the uids right, the same way
§5.3 does — the old CT is unprivileged, the new one is not.

```sh
ssh root@10.0.0.100 '
pct exec 205 -- bash -lc "export DEBIAN_FRONTEND=noninteractive
                          curl -fsSL https://tailscale.com/install.sh | sh
                          systemctl stop tailscaled
                          rm -rf /var/lib/tailscale"
pct exec 205 -- tar xf - --numeric-owner -C /var/lib < /root/ct105-tailscale.tar
pct exec 205 -- bash -lc "systemctl enable --now tailscaled
                          tailscale status | head -3"
'
```

**It comes up stopped, and needs one `tailscale up`.** §4 ran `tailscale down`, and that is
recorded in the very state file this step moves — so the new daemon starts holding the right
identity with `WantRunning=false` and prints `Tailscale is stopped.` Run
`tailscale up --hostname=<name>`; with a valid node key it connects with no login prompt and
no browser. A `tailscale serve` issued before this silently does nothing.

The old daemon must already be down (§4) — two daemons on one node key fight over the
netmap. Expect the old tailnet IP back with no login prompt. The CT needs `/dev/net/tun`,
which §3.2 already passed in.

If the state is lost or refused, fall back to enrolling fresh — `tailscale up
--hostname=pega-rmng`, using the same hostname the old CT had (`ivan-rmng`, `pega-rmng`,
`haoran-rmng`) — and approve the printed URL in a browser signed in to that CT's tailnet.

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

### 6.3 Set `tailscale serve`

Set it on every CT, whatever §4 recorded. `tailscale serve` config lives in the tailnet's
policy for that node, not in `/var/lib/tailscale`, so it does **not** travel with the identity
moved in §6.1 — a CT that had one before the window comes up without it.

This is also the step that makes the LAN address change in §8.1 harmless: the serve URL is
stable across the move, the DHCP lease is not.

```sh
ssh root@10.0.0.100 'pct exec 205 -- bash -lc "
  tailscale serve --bg http://127.0.0.1:9000
  tailscale serve status
"'
```

Expect `https://<hostname>.<tailnet>.ts.net (tailnet only)` proxying `/` to
`http://127.0.0.1:9000`. That URL is how people reach the dashboard; confirm it loads before
telling anyone the migration is done.

**If it answers `Serve is not enabled on your tailnet`**, the feature is off for the whole
tailnet and no command fixes it — someone has to open the printed
`https://login.tailscale.com/f/serve?node=…` link and enable it. This is the one step in the
runbook that needs a person at a browser, and it is not on the critical path: the dashboard is
already reachable over the tailnet at `http://<tailnet-ip>:9000` without it. Each CT has its
own tailnet, so enabling it on one says nothing about the next.

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

**Check a RUNNING clone.** The home's own root directory is owned by root until the clone is
first started, so every archived clone reads `0 0 755` on `upper` and on the merged view while
everything inside it is `1000`. CT 206 showed 40 of 101 at uid 1000 — exactly its 40 running
clones — and CT 204 shows the same split. Not a fault, and not something to repair.

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
running clone on its full home. CT 206 came back with 101 zfs mounts, 101 overlays and all 40
clones running.

The signal to read is the **token push**, not the `binds the live home overlay` line — the
latter only appears for a clone the server had to restart, so a clean boot logs none of them:

```
claude token push done in 59ms: 40 pushed, 0 failed, 0 without live home
```

`0 without live home` is the assertion that every running clone has its overlay.

`mknod /dev/zfs` is redone by the server **inside its own container**, so `/dev/zfs` is missing
from the CT's own `/dev` after a reboot and that is correct — check it with
`docker exec rmng ls -l /dev/zfs`, not from the CT.

### 7.6 End to end

```sh
RMNG_E2E_SERVER=http://<new-ct-ip>:9000 RMNG_E2E_PRESET=<a preset name> \
  cargo run -p control-client --bin rmng_e2e
```

It creates, prebuilds, forks, rebases, archives, unarchives and deletes, asserts a live frame
from each new clone, and temporarily rewrites that preset's startup script, restoring it on
the way out.

---

### 7.7 Account pools — check every account is in one

The gen-2 config fold can leave the pool list holding one empty `Default` while every imported
account sits outside it. CT 204 came up exactly that way, and it is not cosmetic: with no
account in any pool the rotator has nothing to choose from and logs
`rotate: pool 'auto' has no under-cap account` for every clone, forever.

```sh
ssh root@10.0.0.100 'pct exec 205 -- python3 - <<PY
import json
d = "/var/lib/docker/volumes/rmng-data/_data/data"
c = json.load(open("/var/lib/docker/volumes/rmng-data/_data/config.json"))
pooled = {e for g in c["groups"] for e in g["accounts"]}
for side in ("claude", "codex"):
    rows = json.load(open(f"{d}/{side}-accounts.json"))
    rows = rows if isinstance(rows, list) else rows.get("accounts", [])
    loose = sorted({r["email"] for r in rows} - pooled)
    print(side, "ungrouped:", loose or "none")
PY'
```

Fix it in Settings by adding the listed addresses to a pool. One pool covers both providers —
membership is by email, so an address imported on both sides needs one entry, not two.

`grep "rotate\[auto\]" ` in the server log afterwards: a working pool logs real moves
(`ivan-dev-705 a@x -> b@y`), not the `no under-cap account` line.

## 8. After the window

### 8.1 Tell people the new address

The new CT takes a fresh DHCP lease, so its LAN address changes. The Tailscale name and
tailnet IP do not (§6.1 moves the node itself), so anyone using
`https://<hostname>.<tailnet>.ts.net` needs no change — which is the reason to finish §6.3
before announcing anything. Do not remove the old node in the tailnet admin console: it is
the same node, and deleting it revokes the key the new CT now runs on.

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
tag out from under the running clones. Name every tag instead.

CT 206's eight were `haoran-{base, base2, template, template2, template-authed, dev-231,
20260811, latest-fable-51}`, plus the `hello-world` left behind by the §3.4 smoke test. They
took the image store from 98.08 GB to 59.08 GB. What must stay: the derived `rmng-p-*` tag,
`pegasis0/rmng-template:latest` under it, `pegasis0/rmng:latest`, the untagged image the
renamed gen-1 server container still holds, `moby/buildkit` and `registry`.

CT 205's thirteen were `hyperhost-worker{3,4,5}` and `pega-template{3,5,6,7,8,9,10,11,12,13}`:
168.2 GB of images down to 83.2 GB. It keeps three derived tags, not one, because it has three
distinct preset Dockerfiles (`personal` shares `medi`'s text, so it resolves to the same tag).
`alpine:latest` was left alone at 4 MB rather than chased down.

**Look for stray junk in `rmng-data` while you are here.** CT 205 arrived carrying
`/var/lib/docker/volumes/rmng-data/_data/core.1944916`, an **18 GB core dump** from a crash on
the old CT that the copy faithfully reproduced. It is referenced by nothing. Check with
`ls -lnS /var/lib/docker/volumes/rmng-data/_data/ | head` and delete what is obviously dead.

### 8.3 Inner Docker re-pulls — leave them to the clone owners

Each clone's inner Docker starts empty — the `rmng-dind-*` and `rmng-ctd-*` volumes are
recreated empty during migration. The first inner build or `docker run` in each clone re-pulls
through the `rmng-registry` mirror. Expect one slow first build per clone.

**Do not restart what was running inside the clones.** Their owners do that. Say that the
inner Docker starts empty and stop there.

What matters is that **nothing is lost**, and that is worth telling people. The `medi` clones
run a compose stack whose two halves both live in the home, which the migration carries across
untouched:

    ~/Dev/docker-compose.yaml     the stack (mysql, redis, minio, redisinsight, and the
                                  research/voice/kestra services)
    ~/Dev/dev_data/               its bind-mounted data — mysql, redis and s3 state

Only the *images* go. Verified on CT 206: MySQL's data directory came back with its InnoDB
files and binlogs intact.

If you ever do bring one up, **name the services**. A bare `docker compose up` pulls for the
whole file and aborts the batch on the first failure, and the CT 106 stack has two images that
cannot be pulled here — `mediumai.azurecr.io/mediumai-research-core` needs an ACR login the
mirror cannot supply, and `talktomedi/kestra:local` was built locally and lived in the inner
Docker store the migration discards. Those two took the other seven services down with them on
all 18 clones. This works:

```sh
docker exec -u rmng <clone> bash -lc \
  'set -a; . /etc/environment; set +a; cd ~/Dev && docker compose up -d mysql redis minio redisinsight'
```

Ports are published inside the clone, so several clones running it at once do not collide.

### 8.4 Remove nvm — after the rebase, not before

The clone homes carry a whole nvm stack that predates the image having node: `~/.nvm`
(≈150 MB on CT 104, 148 MB on all 101 of CT 106's, ≈580 MB on CT 105), a `fisher` plugin set
(`fabioantunes/fish-nvm`, plus `edc/bass`, which is only there because fish-nvm needs it), and
three lines in `~/.bashrc`. Every home on both migrated CTs had it.

The fish plugin is the part that matters. It installs `node`, `npm`, `npx` and `yarn`
**functions**, and a fish function shadows the real binary — so a clone keeps resolving node
through nvm no matter what the image ships.

**Order is not optional.** node comes from the image (§1.1), and the home is what has nvm, so
a clone stripped before it is rebased has no node at all. Rebase every clone onto the preset
image first, confirm `/usr/bin/node` answers, and only then remove.

```sh
ssh root@10.0.0.100 'pct exec 205 -- docker exec -u rmng <clone> bash -lc "command -v node"'
# must print /usr/bin/node (or /usr/local/bin/node for a tarball install, §1.1),
# not /home/rmng/.nvm/...
```

**Check fish separately, and do not trust `command -v` there.** Before the strip, CT 205's
clones printed `command -v node` → `/usr/bin/node` in fish while `node --version` returned the
nvm version — the fish-nvm *function* shadows the binary, so the path looks right and the
version is wrong. On `pega-we-588a` that meant fish ran node 26.4.0 where the image ships
20.20.2. `docker exec -u rmng <clone> fish -lc 'echo (command -v node) (node --version)'`
shows both halves; after the strip they agree.

**`install -o 1000` fails in the privileged CT** with `install: invalid user: '1000'` — there
is no passwd entry for that uid there. Use `mkdir -p` + `cp` + numeric `chown 1000:1000`
when placing anything into a home from the CT side.

Removal, per home — the merged view, so it works on stopped and archived clones too:

```sh
d=/srv/rmng-homes/.merged/<clone>
rm -rf "$d/.nvm"
rm -f "$d"/.config/fish/functions/{nvm,node,npm,npx,yarn,__nvm_run,nvm_alias_command,nvm_alias_function,bass,fisher}.fish
rm -f "$d"/.config/fish/functions/__bass.py
rm -f "$d"/.config/fish/completions/{nvm,fisher}.fish
rm -f "$d/.config/fish/fish_plugins"
sed -i '/NVM_DIR/d; /nvm.sh/d; /nvm bash_completion/d' "$d/.bashrc"
sed -i '/_fisher_/d' "$d/.config/fish/fish_variables"
```

`fish_variables` holds fisher's bookkeeping (`_fisher_plugins` and one `_fisher_<owner>_files`
per plugin). Leaving it behind makes a future `fisher` think the plugins are still installed.

A shell that is already open keeps the functions it loaded; they go on the next one.

Afterwards fish must resolve `/usr/bin/node`. **Do not judge `yarn --version` from a bare
`docker exec`** — that path runs no PAM, so `COREPACK_HOME` is unset and corepack falls back to
yarn 1.22.22. It is the §1.2 gap, not a fault. Both of these are the real answers:

```sh
docker exec -u rmng <clone> bash -lc 'set -a; . /etc/environment; set +a; yarn --version'
docker exec <clone> bash -lc 'tr "\0" "\n" < /proc/$(pgrep -u rmng -f gnome-session | head -1)/environ'
```

The first prints 4.18.0; the second shows the preset's vars reaching the desktop session.

## 9. Rollback

**Start the old CT.** The migration only read it, so it is still the deployment it was. Stop
the new CT first — two servers must not drive two daemons at once — then re-enable tailscaled
on the old CT (`systemctl enable --now tailscaled`, then `tailscale up`), and destroy the new
CT and its homes dataset.

`pct rollback 105 pre-gen2` also undoes the `docker stop` / `systemctl stop` writes, if the
old CT looks wrong when it comes up.

There is nothing finer than this — no per-clone rollback, no partial undo.

### 9.1 Three things that bite on the way back

**The old CT's stored Claude tokens are dead if the new one ran.** A refresh token is
single-use: every refresh the new server did replaced the copy the old CT is holding. Roll
back a day later and those accounts fail on first use, and a rejected grant marks the account
dead. So before starting the old server, copy the new CT's accounts file over the old one's:

```sh
ssh root@10.0.0.100 '
D=/rpool/data/subvol-105-disk-0/var/lib/docker/volumes/rmng-data/_data/data
pct exec 205 -- cat /var/lib/docker/volumes/rmng-data/_data/data/claude-accounts.json > /tmp/ca.json
cp -a $D/claude-accounts.json $D/claude-accounts.json.stale
cp /tmp/ca.json $D/claude-accounts.json
chown 100000:100000 $D/claude-accounts.json   # the old CT is unprivileged; 0 inside is 100000 outside
chmod 600 $D/claude-accounts.json'
```

Do it with the old CT **stopped**, so nothing refreshes the stale copies first. Codex tokens
do not rotate on refresh, so they need no such repair.

**Start `rmng` before the two infra containers.** The server holds a static `10.99.0.2` on the
`rmng` docker network; `rmng-registry` and `rmng-buildkit` take the next free address. Start
them first and one of them gets `.2`, and the server then fails with `failed to set up
container networking: Address already in use` — which reads like a port clash and is not one.
Ports 445/2222/9000/9001/9005 will all test free while this happens.

**`pct rollback` leaves the rootfs unmounted.** Anything you want to edit from the host
(the accounts file above) needs `zfs mount rpool/data/subvol-<id>-disk-0` first; without it
the path simply does not exist and the error says nothing about mounting.
