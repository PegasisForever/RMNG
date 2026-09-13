# Runbook — migrating CT 104, CT 105 and CT 106 from gen-1 to gen-2

**Scope.** These three containers only. CT 104 `ivan-rmng`, CT 105 `pega-rmng`, CT 106
`haoran-rmng`, all on the Proxmox host `10.0.0.100`. The procedure below is written
against what those three boxes actually contain. It is not a general recipe.

**What the migration is.** A gen-1 clone keeps its home inside the container's own
writable layer. A gen-2 clone keeps its home on a ZFS dataset outside the container.
The new control-server cannot run gen-1 clones, so the changeover is one window per CT:
move the Docker state into a new privileged CT, start the new server, and let it rewrite
every clone.

**Status.** Rehearsed end to end on replica CTs on 2026-09-13. Every number and every
command in this document was run. Where a step was proven on a replica and not on the
real box, the text says so.

---

## 0. Read this first — three facts the old docs got wrong

**0.1 All three CTs run the same server build: `592ad7e`.** The container label lies.
A self-update copies the old container's labels onto the new container, so the label
records the last *hand* create, not the running code.

```console
$ ssh root@10.0.0.100 'pct exec 105 -- docker inspect rmng \
    --format "{{index .Config.Labels \"org.opencontainers.image.revision\"}}"'
2df6370                                   # container label: WRONG

$ ssh root@10.0.0.100 'pct exec 105 -- curl -s http://127.0.0.1:9000/api/server/version'
{"currentRevision":"592ad7e","currentCreated":"2026-09-05T22:45:27Z",
 "currentDigest":"sha256:4ee9fc1077aa…"}  # the truth
```

Measured on 2026-09-13: CT 104, CT 105 and CT 106 all answer `592ad7e`, and CT 105 and
CT 106 report the same image digest `sha256:4ee9fc1077aa…` that CT 104 runs. Never read
a revision off `docker inspect <container>`.

**0.2 All three CTs use the containerd image store, not the classic store.**
`GEN2-CLONES.md` said "CT 105/106 are classic" and told you to force
`"storage-driver": "overlayfs"` with the containerd snapshotter disabled on the new CT.
That would give you a daemon that cannot see a single moved image.

```console
$ ssh root@10.0.0.100 'pct exec 105 -- docker info | grep -A1 "Storage Driver"'
 Storage Driver: overlayfs
  driver-type: io.containerd.snapshotter.v1     # containerd store

$ ssh root@10.0.0.100 'pct exec 105 -- ls /var/lib/docker/image'
identity-cache.db                                # no classic image metadata
```

Docker 29 defaults to the containerd store on a fresh install, which is what all three
CTs have. So the new CT needs **no `daemon.json` storage settings at all**. Write no
`storage-driver` key, and do not disable the snapshotter.

**0.2a The migration path needed three code changes; use a build that has them.**
CT 104's first real run exposed them:

| Change | Why | Where |
| --- | --- | --- |
| Hard links in the home archive | `extract_home_tar` stripped the archive's `rmng/` prefix from the entry path but not from the link target, so every home with a `uv` cache or `pnpm` store failed. **4 of CT 104's 8 clones**, up to 28 276 hard-linked files in one | `provision.rs` |
| Stream the home instead of buffering it | The whole archive sat in RAM — 11.0 GiB measured for a 12 GB home — which is what made one-at-a-time the only safe option | `docker.rs::download_tar_stream`, `provision.rs` |
| Migrate 4 clones at once | Serial migration of a real fleet runs for hours (CT 106 extrapolates to ~19 h) | `jobs.rs::migrate_pass` |
| Preserve file ownership | Without it every migrated home arrives root-owned (§0.2b) | `provision.rs::extract_home_tar` |
| `remount_all` used the dataset NAME as the overlay `upperdir` | A relative upperdir resolves to nothing, so after a CT reboot every clone silently came up on the bare template home | `home_overlay.rs` |
| `remount_all` never mounted the datasets | A CT reboot leaves them unmounted; nothing inside the CT runs `zfs mount -a` | `zfs.rs::ensure_mounted` |
| Clones raced the remount and kept a stale, empty home bind | Docker starts the clones and the server together; a clone that wins binds the bare mountpoint, and a private bind never sees the later mount | `remount_all` restarts any clone whose overlay it just established |

Plus a per-digest lock on the skeleton export, without which four concurrent migrations
race on the same directory.

`pegasis0/rmng:latest` (17ec0d9) does **not** have these. Check before you start:

```sh
docker exec rmng /usr/local/bin/rmng-control-server --version   # or read the image label
```

**0.2b The extractor dropped file ownership — check it before you trust a migration.**
`extract_home_tar` set `set_preserve_permissions` but not `set_preserve_ownerships`, and
the tar crate's default is to give every extracted file to the process doing the
extracting — root. CT 104's first run produced homes the clone user could not write to:

| | uid 1000 | uid 0 | other |
| --- | --- | --- | --- |
| original (read from the source container) | **553 312** | 776 | 439 |
| after migration | 35 | 554 501 | — |

The skeleton export already had `set_preserve_ownerships(true)` with a comment calling it
load-bearing; the migration path was simply missed. Note the 1 215 files that are
legitimately NOT uid 1000 — a blind `chown -R rmng:rmng` is not a correct repair.

Verify after every CT's migration, against the source container read the same way:

```sh
find /srv/rmng-homes/<id>/upper -xdev -uid 1000 | wc -l    # want ~all of them
find /srv/rmng-homes/<id>/upper -xdev ! -uid 1000 | wc -l  # want a small minority
```

**0.3 A migrated clone does NOT keep its own base image.** The server ignores the
clone's recorded `source` image and rebuilds from the row preset's Dockerfile
(`crates/control-server/src/provision.rs`, `migrate_one_inner`: `let _ = base_tag;`).
A gen-1 preset has no Dockerfile, so every clone lands on the default
`FROM pegasis0/rmng-template:latest`.

The home survives — it is copied out and becomes the overlay's upper layer. What is
dropped is everything the clone's base image held **outside** `/home/rmng`. §2 measures
that loss per CT and §3 says how to avoid it.

---

## 1. What is on the three boxes

Measured 2026-09-13.

| | CT 104 `ivan-rmng` | CT 105 `pega-rmng` | CT 106 `haoran-rmng` |
| --- | --- | --- | --- |
| IP | 10.0.0.206 | 10.0.0.15 | 10.0.0.180 |
| rootfs used | 101 GB | 612 GB | 917 GB |
| server | `592ad7e` | `592ad7e` | `592ad7e` |
| Docker | 29.7.2 / containerd.io 2.3.3 | 29.6.1 / containerd.io 2.2.5 | 29.6.1 / containerd.io 2.2.5 |
| image store | containerd | containerd | containerd |
| OS | Ubuntu 26.04 | Ubuntu 26.04 | Ubuntu 26.04 |
| CT privilege | unprivileged | unprivileged | unprivileged |
| clone rows | 8 | 30 | 104 |
| clone containers | 8 | 30 | 101 |
| volumes | 20 | 64 | 212 |
| hostname prefix | `ivan-` | `pega-` | `haoran-` |
| presets | 1 (`Medi`) | 3 (`medi`, `hyperhost`, `wealthstack`) | 1 (`talktomedi`) |
| account pools | none | `Medi`, `Personal` (+ codex `Personal`) | `Default` (+ codex `Default`) |
| published ports | 445 2222 9000 9001 9005 | 445 2222 9000 9001 **9002 9003** 9005 | 445 2222 9000 9001 9005 |
| `/etc/docker/daemon.json` | none | `{"dns": […]}` | none |
| `--init` on the rmng container | yes | **no** | yes |

CT 105's `9002` and `9003` publishes have nothing behind them — the `listen` config only
names 9000, 9001, 9004, 9005 and 2222. Drop them when you rebuild the run command.

### 1.1 Clone row types present

One line per shape the migration has to handle. A count of 0 means that CT has none.

| Row shape | 104 | 105 | 106 |
| --- | --- | --- | --- |
| live, headed, top-level, known preset | 5 | 12 | 14 |
| live, headed, top-level, **with port forwards** | 0 | 1 | 0 |
| live, headed, **sub clone** | 0 | 0 | 1 |
| live, **headless**, sub clone | 1 | 0 | 26 |
| archived, headed, top-level | 2 | 9 | 27 |
| archived, headed, top-level, **preset no longer exists** | 0 | 2 (`personal`) | 0 |
| archived, headed, sub clone | 0 | 0 | 1 |
| archived, headless, sub clone | 0 | 6 | 35 |
| **total rows** | **8** | **30** | **104** |

Cutting across those rows, CT 106 has three with **no container behind them**. They
cannot migrate (§3.7):

| Row | archived | headless | parent |
| --- | --- | --- | --- |
| `haoran-dev-621` | yes | no | — |
| `haoran-dev-635` | yes | no | — |
| `ng-52378be7` | **no** | yes | `haoran-dev-675` |

### 1.2 Clone home sizes

The migration reads each home into the server's memory in one piece
(`download_home_tar` returns a `Vec<u8>`), so the largest single home sets the peak
memory the server needs.

```console
$ ssh root@10.0.0.100 'pct exec 106 -- bash -lc \
    "for c in haoran-dev-721 haoran-dev-717; do p=\$(docker inspect -f {{.State.Pid}} \$c); \
     du -sb /proc/\$p/root/home/rmng; done"'
20807892185   # haoran-dev-721 — 20.8 GB
12857244106   # haoran-dev-717 — 12.9 GB
```

The fleet-wide maximum is larger. Measuring every container layer on CT 106 and then the
`/home/rmng` inside the five biggest:

```console
$ ssh root@10.0.0.100 'pct exec 106 -- bash -lc \
    "du -sb /var/lib/containerd/io.containerd.snapshotter.v1.overlayfs/snapshots/* \
     | sort -n | tail -5"'
…/snapshots/1326   44922430719
…/snapshots/1047   44219673632
$ # then, per snapshot: du -sb <snapshot>/fs/home/rmng
44758203444   # 44.8 GB — the largest single home on CT 106
44154569552   # 44.2 GB
35441340893   # 35.4 GB
```

CT 106 gives the server 256 GiB, so a 45 GB buffer fits with room. Measure the largest
home on each CT before its window anyway (§3.1) — the number is the memory floor, and it
grows every day the fleet runs.

Confirmed live during CT 104's migration: while copying a 12.0 GB home the control-server's
RSS sat at **11.0 GiB**, and the dataset's `upper/` was still empty — the whole archive is
held in memory before a single file is written.

It is also the slow part. Each clone is one in-memory download followed by an extract of
~450 000 files, so budget minutes per clone, not seconds: CT 104's eight clones at 10–13 GB
each took far longer than the 98 GB bulk copy that preceded them.

---

## 2. What each CT loses, and what it keeps

### 2.1 Kept

- Every clone's `/home/rmng`, byte for byte: files, modes, symlinks, dot-directories.
  Verified on the rehearsal by sha256 over a 64 MiB blob, a 0600 file, a nested symlink
  and a dot-config file in every clone.
- Every clone row and its metadata: id, display name, Linear ticket and branch, preset
  name, archived flag, headless flag, sub-clone parent, port forwards.
- The board columns, the muted-clone set, the ticket order.
- Every imported Claude and Codex account. They travel inside the `rmng-data` volume,
  which travels inside `/var/lib/docker`. No re-login.
- The account pools. The new server folds the split `cloneGroups` / `codexGroups` lists
  into one `groups` list, and folds each preset's `claudeAccount` / `codexAccount`
  default into one `group` field, on first load.
- Every image in the daemon, including each CT's own commit images
  (`pega-template13`, `haoran-20260811`, `ivan-dev-week2`, …). They stay on the daemon —
  they just stop being what the clones run.

### 2.2 Dropped

| Dropped | Why |
| --- | --- |
| Everything in the clone's base image outside `/home/rmng` | The gen-2 container is built from the preset Dockerfile, not the clone's `source` image (§0.3) |
| Preset `vars` (`PATH`, `TURBO_API`, `TURBO_TEAM`, `TURBO_TOKEN`, `ROLLBAR_ACCESS_TOKEN`) | Gen-2's `Preset` type has no `vars` field. `preset_env_vars` returns only `LINEAR_API_KEY` |
| Inner-Docker state (`rmng-dind-*`, `rmng-ctd-*` volumes) | Deleted per clone during the migration; the inner daemon re-pulls through the `rmng-registry` mirror |
| Container-level drift in the clone's writable layer outside `/home/rmng` | Only the home is copied |

**Measure the base-image loss per CT before deciding — it is smaller than it sounds.**
The clone images are `docker commit` chains (`pega-template13` is four layers of
8.34 GB + 5.34 GB + 320 MB + 13.9 GB), but most of what is in them is also in the current
published template, which has moved on since those commits were taken.

Measured for CT 104 (`ivan-dev-week2` against `pegasis0/rmng-template:latest`):

```console
$ # packages, diffed properly (LC_ALL=C, both sides sorted)
lost   (in clone, not in template):  hmcl
gained (in template, not in clone):  rclone rsync

$ # /opt
clone:     az containerd google mission-center onlyoffice rmng
template:  az google mission-center onlyoffice rmng zed.app

$ # /usr/local/bin
clone:     mission-center ngrok rmng sops
template:  ngrok rmng zed
```

So CT 104 loses exactly `hmcl` (which the template dropped on purpose, commit `db29ce8`),
`sops` 3.13.3, and two shims — and gains Zed, `rclone` and `rsync`. That is a `RUN` line
in the preset Dockerfile, not a reason to publish a 28 GB image.

Run the same diff for each CT before its window. Put this in a file and run it on the
Proxmox host — the quoting does not survive being typed through three shells:

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

Both `sort` calls need `LC_ALL=C` or `comm` reports "input is not in sorted order" and
prints nonsense.

### 2.3 Keeping the base image (optional, per CT)

The preset's Dockerfile is built with `pull` forced on
(`docker.rs::build_derived_image` sets `.pull("1")`), and the classic builder then fails
on any tag that only exists locally. Measured:

```console
$ docker tag debian:12-slim localonly-test:latest
$ DOCKER_BUILDKIT=0 docker build --pull -t x - <<< 'FROM localonly-test:latest'
failed to resolve reference "docker.io/library/localonly-test:latest": not found
$ DOCKER_BUILDKIT=0 docker build       -t x - <<< 'FROM localonly-test:latest'
Successfully built …                      # works without --pull
```

So `FROM pega-template13:latest` cannot work. The CT's own `rmng-registry` cannot take
the image either: it runs as a pull-through proxy
(`REGISTRY_PROXY_REMOTEURL=https://registry-1.docker.io`), which refuses pushes.

Two ways out, both to be decided per CT **before** the window:

1. **Publish the base.** `docker tag pega-template13:latest pegasis0/pega-template13:latest`,
   push it, then set the preset's Dockerfile to `FROM pegasis0/pega-template13:latest`.
   Costs one upload of 28 GB of commit layers (CT 105) or the equivalent per CT.
2. **Accept the loss** and transcribe the parts you want into the preset Dockerfile as
   ordinary `RUN` lines on top of `FROM pegasis0/rmng-template:latest`. This is the
   design's intent (`GEN2-CLONES.md` §1, "Manual drift").

A third way needs a one-line code change and is not part of this runbook: make
`ensure_image` pass `pull = force` instead of always `1`, so only the preset's Rebuild
button re-pulls the base. That would let `FROM pega-template13:latest` resolve locally.

### 2.4 Preset `vars`: the Dockerfile is not enough

All three CTs carry preset `vars` — `PATH`, `TURBO_API`, `TURBO_TEAM`, `TURBO_TOKEN`, and
on CT 105's `wealthstack` also `ROLLBAR_ACCESS_TOKEN`. Gen-1 wrote them into each clone's
`/etc/environment`; gen-2 writes only the dynamic keys there. `GEN2-CLONES.md` §1 says
static env moves into the preset Dockerfile as `ENV`. **Measured, that only half works.**

A gen-1 clone today — the agent process really has them:

```console
$ pct exec 106 -- docker exec -u rmng haoran-dev-721 bash -lc \
    'tr "\0" "\n" < /proc/$(pgrep -u rmng -f agent-wrapper|head -1)/environ'
PATH=/home/rmng/.local/bin:/usr/local/bin:/usr/bin:/bin
TURBO_API=http://10.0.0.101:3000
TURBO_TEAM=talktomedi
TURBO_TOKEN=…
```

A gen-2 clone built from a Dockerfile carrying `ENV RMNG_ENVTEST=…`:

```console
$ docker exec -u rmng <clone> bash -lc 'echo $RMNG_ENVTEST'
hello-from-dockerfile                     # a docker exec shell DOES see it
$ tr "\0" "\n" < /proc/<agent-wrapper pid>/environ | grep RMNG_ENVTEST
                                          # the AGENT does NOT
```

The agent runs under a `systemd --user` unit, which takes its environment from the user
manager, not from the container's `Config.Env`. So a Dockerfile `ENV` reaches interactive
`docker exec` shells and nothing the agent runs.

**The carrier that works is `~/.config/environment.d/`.** The user manager reads it, and
it lives in the home — so it survives migrate, fork and rebase, unlike anything in `/etc`.
Verified:

```console
$ printf 'TURBO_API=…\nTURBO_TEAM=…\nTURBO_TOKEN=…\n' > ~/.config/environment.d/10-rmng-preset.conf
$ systemctl --user daemon-reload && systemctl --user restart agent-wrapper.service
$ tr "\0" "\n" < /proc/<new agent pid>/environ | grep -E '^(TURBO_|RMNG_CONTROL_URL)'
TURBO_API=http://10.0.0.101:3000
TURBO_TEAM=talktomedi
TURBO_TOKEN=…
RMNG_CONTROL_URL=http://rmng-control:9000      # the server's own keys still there
```

So do both, and **exclude `PATH`**:

1. **Dockerfile `ENV` lines** for every var, including `PATH` — costs nothing, fixes
   `docker exec` shells. A `FROM` + `ENV` Dockerfile has no `RUN`, so the build cannot
   fail beyond the base pull.
2. **`~/.config/environment.d/10-rmng-preset.conf` in every clone's home** for the vars
   **except `PATH`**. Gen-1 did not apply the preset `PATH` to the agent unit either (its
   PATH was `/home/rmng/.local/bin:/opt/rmng/bin:/usr/local/bin:/usr/bin:/bin`), and
   setting it here would drop `/opt/rmng/bin` from the agent's PATH. Matching gen-1 means
   leaving PATH alone.

Both are scripted: `c4-config.py` renders the Dockerfile and stashes the values at §5.4,
`restore-preset-env.py` writes the home file at §5.8. **Capture the values before the
gen-2 server's first boot** — it drops the retired `vars` field the first time it rewrites
`config.json`.

> **`PUT /api/config` takes a BARE config object.** `GET` returns `{"config": {...}}`, the
> `PUT` does not accept that shape: a wrapped body returns `200` and silently ignores
> `presets`, leaving the Dockerfile unchanged. Send the fields at the top level.

## 3. Before the window

### 3.1 Measure

```sh
PVE="ssh root@10.0.0.100"
CT=105                                   # or 104 / 106

# 1. The running server revision (never the container label).
$PVE "pct exec $CT -- curl -s http://127.0.0.1:9000/api/server/version"

# 2. The Docker package versions to pin on the new CT.
$PVE "pct exec $CT -- dpkg -l | grep -E 'docker-ce |docker-ce-cli|containerd.io|buildx|compose'"

# 3. How much data moves. Use `du -shx`: without -x, du descends into every RUNNING
#    container's overlay merged mount and counts the whole clone filesystem again.
#    On CT 104 that read 126G for /var/lib/docker against a real 30G.
#    (After §5.1 stops the fleet the mounts are gone and plain `du -sh` agrees.)
$PVE "pct exec $CT -- du -shx /var/lib/docker /var/lib/containerd"

# 4. The largest clone home — this is the server's peak memory during the window.
#    /proc/<pid> only exists for RUNNING clones, so this misses every archived one.
$PVE "pct exec $CT -- bash -lc 'for c in \$(docker ps --format {{.Names}} | grep -v ^rmng); do
        p=\$(docker inspect -f {{.State.Pid}} \$c 2>/dev/null)
        printf \"%s \" \$c; du -sb /proc/\$p/root/home/rmng 2>/dev/null | cut -f1; done' | sort -k2 -n | tail -5"
#    …and this covers every container layer, running or not (slow, but complete):
$PVE "pct exec $CT -- du -sb /var/lib/containerd/io.containerd.snapshotter.v1.overlayfs/snapshots/* \
        | sort -n | tail -5"

# 5. The exact run command of the current server, so the new one matches it.
$PVE "pct exec $CT -- docker inspect rmng \
  --format '{{json .HostConfig.PortBindings}} {{json .HostConfig.Binds}} {{.HostConfig.Init}} {{json .Config.Env}}'"
```

Copy time: CT 104's real payload was **98 GB** (30 GB `/var/lib/docker` + 68 GB
`/var/lib/containerd`), not the 101 GB its dataset `used` suggests. Roughly 30 GB of that
is `rmng-dind-*` / `rmng-ctd-*` volumes the migration deletes on the spot — copied and
thrown away, but not worth deviating from the tested path to skip.

Throughput depends on the file mix: 95–110 MB/s on the rehearsal's image layers, closer to
55 MB/s on CT 104's real homes (about 450 000 files each). Budget the low figure:
CT 104 ≈ 30 min, CT 105 ≈ 3 h, CT 106 ≈ 4 h 30 min, plus the per-clone migration time
from §5.6.

### 3.2 Free the space

The new CT holds a second copy of the whole Docker state until you destroy the old CT.
Check the pool has room for it:

```sh
ssh root@10.0.0.100 'zfs list -o name,used,avail rpool; zpool list rpool'
```

On 2026-09-13, after the old vzdump archives were deleted, `rpool` has **966 GB free**
(1.81 T size, 890 G allocated, 47% capacity):

```console
$ ssh root@10.0.0.100 'zpool list -o name,size,alloc,free,cap,dedup rpool'
rpool  1.81T   890G   966G    47%   3.45x
```

That is enough for every CT even if dedup gives nothing: CT 104 needs 101 GB, CT 105
612 GB, CT 106 918 GB. Only CT 106 is close, and dedup plus destroying the earlier CTs as
you go (§7.2) leaves plenty of headroom. Space is no longer a gate — but re-read the
number before each window rather than trusting this one.

`rpool` runs deduplication (pool ratio 3.45x), and the CT 105 and CT 106 rootfs datasets
already have `dedup=blake3` set per dataset:

```console
$ ssh root@10.0.0.100 'zfs get -H -o name,value dedup rpool/data/subvol-105-disk-0     rpool/data/subvol-106-disk-0 rpool/data/subvol-104-disk-0'
rpool/data/subvol-105-disk-0   blake3
rpool/data/subvol-106-disk-0   blake3
rpool/data/subvol-104-disk-0   off
```

A new CT created with `pct create` inherits `dedup=off` from `rpool/data`. Set it on the
new CT's rootfs dataset **after `pct create` and before the copy** — dedup only applies
to blocks written after it is on (§3.2a has the measurement). The step belongs in §5.2:

```sh
ssh root@10.0.0.100 'zfs set dedup=blake3 rpool/data/subvol-205-disk-0'
```

It is a lasting property of that dataset, not a one-off: every later write to the new CT
pays the hash and the table lookup. That is the same deal CT 105 and CT 106 already run
under.

The copy then writes the same content with the same compression and record size, so it
resolves to blocks the pool already holds. Measured on this host on 2026-09-13, with two
scratch datasets at `dedup=blake3`:

```console
$ zfs create -o mountpoint=/srv/dedup-a -o dedup=blake3 rpool/dedup-a
$ zfs create -o mountpoint=/srv/dedup-b -o dedup=blake3 rpool/dedup-b
$ # 1.44 GB of incompressible files into A, then a tar round-trip into B
$ A0=$(zpool list -Hp -o alloc rpool)
$ tar cf - --numeric-owner -C /srv/dedup-a tree | tar xf - --numeric-owner -C /srv/dedup-b
$ A1=$(zpool list -Hp -o alloc rpool); echo $(( (A1-A0)/1024/1024 )) MiB
-107 MiB            # zero growth (the minus is other activity on the box)

$ zfs list -o name,used rpool/dedup-a rpool/dedup-b
rpool/dedup-a   1.44G
rpool/dedup-b   1.44G        # <- LIES: this is logical, not allocated
```

**Watch `zpool list -o alloc`, not `zfs list -o used`.** `zfs list` reports the logical
size and will show the new CT growing to the full nominal size even when the pool has
allocated nothing.

The same thing was then measured on a real 13.6 GB CT-to-CT move during the rehearsal,
with `dedup=blake3` set on the target CT's rootfs dataset before the copy:

```console
$ zfs set dedup=blake3 rpool/data/subvol-110-disk-0
$ # …then the §5.3 pipe…
alloc before: 1533 GiB
alloc after:  1534 GiB; grew 1086 MiB     # 13.6 GB copied, 1.1 GB allocated
$ zfs list -o name,used rpool/data/subvol-107-disk-0 rpool/data/subvol-110-disk-0
rpool/data/subvol-107-disk-0   13.6G
rpool/data/subvol-110-disk-0   13.6G      # logical again, not allocated
```

The condition is that the content is already in the pool's dedup table — which happens
when any dataset with dedup on holds it. CT 105 and CT 106 have `dedup=blake3` on their
own rootfs, so their whole contents qualify. CT 104 has `dedup=off`, so its blocks are in
the table only where another deduped CT happens to hold the same content; it is 101 GB
and fits either way.

The dedup table costs RAM. This pool's is already 13.1 M entries, 5.94 GB on disk and
4.98 GB in core (`zpool status -D rpool`). A second copy of existing blocks mostly raises
reference counts rather than adding entries, so the growth is small.

### 3.2a Dedup and compression: two datasets, both set before the first write

Gen-2 creates a ZFS dataset per clone, and `zfs.rs` sets **only `mountpoint`** on it:

```rust
run(&["create", "-o", &mp, &ds])?;                 // zfs::create
run(&["clone",  "-o", &mp, snapshot, &dst])?;      // zfs::clone_dataset (fork)
```

Everything else is inherited from the homes parent. Verified on the rehearsal — set
`dedup=blake3` on the parent alone and every child follows, including ones that already
existed and ones the server creates afterwards:

```console
$ zfs set dedup=blake3 rpool/rmng-homes-r106
$ zfs get -H -o name,value,source dedup -r rpool/rmng-homes-r106
rpool/rmng-homes-r106                       blake3  local
rpool/rmng-homes-r106/haoran-rep-live       blake3  inherited from rpool/rmng-homes-r106
…
$ # then a template create and a fork through the API:
rpool/rmng-homes-r106/haoran-dedup-probe        blake3  inherited from rpool/rmng-homes-r106
rpool/rmng-homes-r106/haoran-dedup-fork-probe   blake3  inherited from rpool/rmng-homes-r106
```

So there is **no code change to make**. One `zfs set` on the parent covers every clone
home, plus `.skeleton/`, `.shared/` and `.merged/`, which are plain directories in the
parent dataset rather than datasets of their own.

**Two datasets carry gen-2 data, and both need it:**

| Dataset | What it holds | CT 104 | CT 105 | CT 106 |
| --- | --- | --- | --- | --- |
| the CT rootfs (`rpool/data/subvol-<id>-disk-0`) | Docker images, the containerd store, the `rmng-data` / `rmng-dind-*` / `rmng-ctd-*` volumes | `off` | `blake3` | `blake3` |
| the homes parent (new) | every clone home, the skeleton, the shared pool | — | — | — |

A CT made by `pct create` inherits `dedup=off` from `rpool/data`, so the new CT's rootfs
needs `zfs set dedup=blake3` too (§5.2). Today the clone homes live inside the CT rootfs,
so on CT 105 and CT 106 they are deduped **right now**. Migrating them to a homes parent
that is not deduped gives that up.

What dedup is worth on this pool today:

```console
$ zpool status -D rpool
 dedup: DDT entries 13137881, size 5.95G on disk, 4.98G in core
 Total    12.5M    929G    481G    487G    77.4M   2.70T   1.53T   1.64T
         └ allocated: 487G                        └ referenced: 1.64T
```

1.64 TB of referenced data occupies 487 GB — about **1.15 TB saved on a 1.81 TB pool**,
and CT 105 plus CT 106 are the bulk of it. That saving is why this host fits at all.

**The order is not negotiable.** Dedup only covers blocks written while it is on. Turning
it on afterwards does nothing for what is already there. Measured with 1 GiB of data
nothing else on the pool had:

```console
$ # write it with dedup off, THEN turn dedup on, then copy it into a deduped dataset
1 GiB whose blocks predate dedup  -> pool grew 1007 MiB
$ # copy that copy, whose blocks were written with dedup on
the same GiB, now in the DDT      -> pool grew 12 MiB
```

So the homes parent must carry `dedup=blake3` **before** the migration writes the first
home into it (§4.1), and the new CT's rootfs must carry it before the §5.3 copy (§5.2).

**Do not change `compression` or `recordsize` on the homes parent.** Dedup matches blocks
as they are written to disk, so a different compression algorithm produces different
blocks and matches nothing. Measured, same 1 GiB of compressible data into two deduped
datasets:

```console
same compression (lz4 -> lz4):       pool grew -34 MiB   # full match
different compression (lz4 -> zstd): pool grew 736 MiB   # no match at all
```

`rpool` is `compression=on` (lz4) and `recordsize=128K`, and the children inherit both.
zstd would compress better in isolation (1.27x vs 1.00x on that sample) and cost you the
1.15 TB dedup saving. Leave them alone.

**One thing dedup is not needed for.** `feature@block_cloning` is active on this pool
(`BCLONE_RATIO 3.46x`), so a plain `cp` between two clone homes — across `~/clones/<id>`,
say — is already a reflink: instant and free, no data written. Dedup is for the copies
that *do* write, which is what the migration does 8 to 104 times.

### 3.3 The backup: a snapshot, not a fresh dump

**The migration never writes to the old CT.** §5.1 stops its fleet and its daemons, §5.3
reads it, and that is all. The old CT stays a complete, bootable gen-1 deployment for as
long as you leave it alone — so *it* is the rollback, and the job of a backup here is only
to survive losing the CT itself.

That makes a fresh whole-CT dump the wrong tool, for three measured reasons:

```console
$ ssh root@10.0.0.100 'cat /var/lib/vz/dump/vzdump-lxc-106-2026_09_12-02_32_07.log'
2026-09-12 02:32:07 INFO: Starting Backup of VM 106 (lxc)
2026-09-12 11:21:50 INFO: archive file size: 533.69GB
2026-09-12 11:21:52 INFO: Finished Backup of VM 106 (08:49:45)   # 8 h 50 m at 48 MiB/s

$ ssh root@10.0.0.100 'df -h --output=size,used,avail /var/lib/vz | tail -1'
 902G  659G  244G                                   # a second CT 106 dump does not fit
```

- **Time.** CT 106 takes ~8 h 50 m, CT 105 about 6 h, CT 104 53 min (measured / scaled
  from the two logs on the host).
- **Space.** The dump store has 244 GB free and a CT 106 dump is 534 GB. You cannot hold
  the existing one and a fresh one, so taking a fresh one means ~9 hours with **no**
  complete CT 106 backup at all.
- **It buys nothing the stopped CT does not already give you.**

Do this instead, **after §5.1 has stopped the daemons** so the snapshot is clean rather
than crash-consistent:

```sh
ssh root@10.0.0.100 'pct snapshot 105 pre-gen2 --description "before the gen-2 migration"'
```

It is instant and nearly free — the two CT 104 snapshots already on this host cost 2.44 GB
and 416 MB. It covers the only writes the procedure makes to the old CT (the `docker stop`
and `systemctl stop` in §5.1) and gives a named point to return to.

**There are no dumps on this host.** `/var/lib/vz/dump` was emptied on 2026-09-13 by
operator decision — the CT 104, CT 105 and CT 106 archives, plus four orphans, 657 GB in
total. The snapshot above is therefore the entire backup story, and it rests on the old CT
staying intact.

That is a deliberate trade, and it is defensible on this host: the dumps lived on the same
single NVMe as the CTs they backed up (`rpool` has one vdev, no mirror, and no other
storage is configured), so they never protected against losing the disk. What they covered
was operator error, and the snapshot plus the untouched old CT covers that too.

What it means in practice:

- **Do not destroy an old CT until its replacement has run a working day** (§7.1). Until
  the dumps existed there was a second chance; now there is not.
- If you want a cold copy back, take it before a window, not during one — and note the
  time and space from the numbers above.

### 3.4 Raise the host inotify limit

VS Code watchers inside the clones can exhaust `fs.inotify.max_user_watches` on the
Proxmox host, and a CT that hits it will not finish booting — systemd stops starting
units. 1.2 M watches were seen in use, mostly from CT 106. Set 2 M **before** booting
the new CT:

```sh
ssh root@10.0.0.100 'sysctl -w fs.inotify.max_user_watches=2000000
  echo fs.inotify.max_user_watches=2000000 > /etc/sysctl.d/99-rmng-inotify.conf'
```

### 3.5 Raise the keyring quota (unprivileged source CT only)

Not needed for the new privileged CT, but keep it if it is already set:

```sh
ssh root@10.0.0.100 'cat /etc/sysctl.d/99-rmng-keys.conf'
# kernel.keys.maxkeys = 20000
# kernel.keys.maxbytes = 2000000
```

### 3.6 Decide the base-image question (§2.3) and write the preset Dockerfiles (§2.4)

Do this on the **old** server, in Settings, before the window. Gen-1 presets have no
Dockerfile field, so the text has to be set after the new server boots — but decide the
content now, because every clone builds from it during the window.

### 3.7 Delete the container-less rows (CT 106 only) — not optional

`haoran-dev-621`, `haoran-dev-635` and `ng-52378be7` have rows but no containers. The
migration reads the home out of the container, so there is nothing to read:

```
operation failed: downloading /home/rmng from haoran-dev-621:
Docker responded with status code 404: No such container: haoran-dev-621
```

They fail on the first pass and again on the retry, and they stay gen-1 — which is not a
cosmetic leftover. **A leftover gen-1 row makes EVERY later control-server restart re-run
the migration, and that stops the whole fleet each time.** Measured on the rehearsal:

```
05:32:38  gen-2 migration: 2 gen-1 clone(s) detected, migrating one at a time: …
          …fleet stopped, both rows fail again, fleet restarted…
05:33:18  gen-2 migration: 0 passed, 2 failed (…), 3 started
```

So delete them. Either in the UI before the window, or afterwards on the new server —
the gen-2 delete tolerates a missing container, verified on the rehearsal:

```sh
curl -s -XPOST http://<new-ct-ip>:9000/api/delete \
  -H 'Content-Type: application/json' -d '{"id":"haoran-dev-621"}'
```

Once no gen-1 row is left, a restart files no migration and the fleet keeps running. A
failed migration leaves an empty directory under the homes parent but no dataset — the
error path destroys the half-built one. Remove the empty dirs by hand if they bother
you.

---

## 4. Build the new CT

Fresh and privileged. **Never restore the dump as privileged** — it corrupts the LXC
namespace state so that every `pct exec` and `docker exec` silently runs against CT files
instead of container files, and it cannot be repaired in place.

### 4.1 The homes dataset on the Proxmox host

**Set `dedup=blake3` here, at create time.** Everything gen-2 writes lands under this
dataset, and the control-server sets only `mountpoint` on the datasets it creates
(`zfs.rs::create`, `zfs.rs::clone_dataset`) — every other property is inherited. So this
one line decides dedup and compression for every clone home the fleet will ever have.
See §3.2a for why it must be now and not later.

```sh
ssh root@10.0.0.100 '
  ls -l /dev/zfs                     # note major:minor, 10:249 on this host
  zfs create -o mountpoint=/srv/rmng-homes-105 -o dedup=blake3 rpool/rmng-homes-105
  zfs get -o property,value,source dedup,compression,recordsize rpool/rmng-homes-105
'
```

Expect `dedup blake3 local`, `compression on inherited from rpool`, `recordsize 128K
default`. Do **not** set `compression` or `recordsize` to anything else — see §3.2a.

### 4.2 Create the CT

```sh
ssh root@10.0.0.100 '
pct create 205 local:vztmpl/ubuntu-26.04-standard_26.04-1_amd64.tar.zst \
  --hostname pega-rmng --cores 32 --cpulimit 24 --memory 262144 --swap 10000 \
  --rootfs local-zfs:1024 --net0 name=eth0,bridge=vmbr0,ip=dhcp,type=veth \
  --features nesting=1,keyctl=1,fuse=1 --unprivileged 0 --ostype ubuntu --onboot 1 \
  --mp0 /srv/rmng-homes-105,mp=/srv/rmng-homes
'
```

Match `cores`, `cpulimit`, `memory`, `swap` and rootfs size to the CT you are replacing
(§1). `cores` must be at least `docker.cloneCpus` (16 on all three CTs) or the daemon
refuses to create a clone.

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

Copy the `dev0` / `dev1` lines from the CT you are replacing: CT 104 and CT 105 pass both
the render node and `/dev/kfd`, CT 106 passes only the render node. The render node is
not optional — the setup wizard's environment check requires it and the video plane needs
it at run time.

**The three AppArmor lines are required on Ubuntu 26.04, privileged or not.** The retired
`GEN2-PROD-NOTES.md` said to leave them out; that was measured on a Debian 13 CT only.
Without them on an Ubuntu 26.04 CT:

```console
$ pct exec 205 -- docker run --rm hello-world
docker: Error response from daemon: AppArmor enabled on system but the docker-default
profile could not be loaded: … apparmor_parser: Access denied. You need policy admin
privileges to manage profiles.
```

`10:249` is the ZFS device node; use whatever `ls -l /dev/zfs` printed. Do **not**
bind-mount the host's `/dev/zfs` with `lxc.mount.entry` — that breaks nested container
mount joins the same silent way the restore-as-privileged path does.

### 4.3 First boot

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

### 4.4 Docker, pinned to the source CT's versions

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

`Storage Driver: overlayfs` with `driver-type: io.containerd.snapshotter.v1` is the
right answer. Write **no** `daemon.json` storage keys (§0.2).

`export DEBIAN_FRONTEND=noninteractive` on every apt step. A run killed mid-dialog
leaves an orphaned apt holding the dpkg lock — check `ps` inside the CT and run
`dpkg --configure -a` before retrying.

Do **not** install `zfsutils-linux` in the CT. Its `zfs-dkms` post-install builds kernel
modules and fails under a shared-kernel LXC. The server image bundles its own `zfs`; the
CT needs only the `/dev/zfs` node. The cost is that your own shell in the CT has no `zfs`
command — run `zfs` from the Proxmox host instead.

### 4.5 lxcfs — REQUIRED, and the one that bit us

**All three CTs run lxcfs, so the new CT must too.** Every clone container carries bind
mounts from `/var/lib/lxcfs/proc/*` over its own `/proc` files (`PROXMOX-LXC.md` §2b).
Those binds are container config: they travel with the container and they are resolved on
the NEW host. Without lxcfs there, the bind sources do not exist and Docker cannot
prepare the container filesystem at all.

It does not fail in an obvious way. On CT 204 it looked like this — every clone, both
passes:

```
operation failed: downloading /home/rmng from ivan-dev-725:
Docker responded with status code 404: Could not find the file /home/rmng in container ivan-dev-725
```

`docker cp` could not read **any** path from those containers, not even `/etc/hostname`,
while the underlying snapshot mounted by hand and had the whole filesystem in it. The
honest error only appears on `docker start`:

```
Error response from daemon: invalid mount config for type "bind":
bind source path does not exist: /var/lib/lxcfs/proc/meminfo
```

Install it **before** booting the gen-2 server. The CT feature `fuse=1` (§4.2) is needed,
and Ubuntu's unit carries `ConditionVirtualization=!container`, so inside a CT it is
silently skipped until you drop that condition:

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

Check the source CT first and match it:

```sh
ssh root@10.0.0.100 'pct exec 104 -- ls /var/lib/lxcfs/proc/'
```

Recovery if you hit this after the fact is cheap: install lxcfs, then `docker restart rmng`
to re-file the migration. The failing step is the home READ, which happens before the old
container is removed, so a failed pass destroys nothing.

That recovery works for any failure, and it is worth knowing why it is safe. A failed
`Migrate` tears its own dataset down (`migrate_one`'s error arm calls `teardown_merged`
then `zfs destroy`), so a retry starts clean. Two consequences:

- **Killing the server mid-migration does NOT get that cleanup** — the process dies before
  the error arm runs and the half-built dataset is orphaned. The next pass then fails with
  `cannot create '<parent>/<id>': dataset already exists` — and *that* failure runs the
  error arm, destroys the orphan, and lets the retry pass succeed. Seen on CT 104; it
  resolves itself in one extra round, but `zfs destroy <parent>/<id>` clears it directly.
- The teardown is unconditional: a failed migrate destroys the dataset at that id **even
  if it did not create it**. Harmless during migration (a gen-1 row has no dataset to
  lose), but do not point a `Migrate` at a row that already has one.

### 4.6 CT 105 only — the DNS daemon.json

CT 105 has `/etc/docker/daemon.json` with a `dns` key so clones resolve `*.ts.net`.
Copy that file across (see `PROXMOX-LXC.md` §2c) and reload Docker.

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

**`containerd` must be in the stop list.** `systemctl stop docker docker.socket` leaves
`containerd.service` running, and with the containerd image store that daemon owns
`/var/lib/containerd` — the directory you are about to copy, including its bolt metadata
DB. Measured: after `stop docker docker.socket`, `systemctl is-active containerd` still
printed `active`. Stop `docker.socket` first, or socket activation restarts the daemon
mid-copy.

Use `bash -lc`. A bare `sh -c` has no `docker` on `PATH`, so the fleet never stops and
the copy runs against live containers.

Now take the snapshot, with the daemons down so it is clean (§3.3):

```sh
ssh root@10.0.0.100 'pct snapshot 105 pre-gen2 --description "before the gen-2 migration"
                     pct listsnapshot 105'
```

### 5.2 Clear the new CT's own Docker state

The `hello-world` test in §4.4 wrote an engine id and containerd metadata. Extracting the
old state on top of it mixes two stores.

```sh
ssh root@10.0.0.100 '
  pct exec 205 -- bash -lc "systemctl stop docker.socket docker containerd
                            rm -rf /var/lib/docker /var/lib/containerd"
  # Required, and required NOW — dedup covers only blocks written after it is on (§3.2a).
  # This is the CT rootfs: Docker images, the containerd store, the rmng-* volumes.
  zfs set dedup=blake3 rpool/data/subvol-205-disk-0
  zfs get -o property,value,source dedup,compression rpool/data/subvol-205-disk-0
  zpool list -Hp -o alloc rpool          # note this number, compare after §5.3
'
```

The homes parent got its own `dedup=blake3` back in §4.1. These are two separate
datasets; setting one does not set the other.

### 5.3 Move the Docker state

One host-side pipe. No intermediate tarball, so no second copy on disk.

```sh
ssh root@10.0.0.100 '
  time (pct exec 105 -- tar cf - --numeric-owner -C /var/lib docker containerd \
      | pct exec 205 -- tar xf - --numeric-owner -C /var/lib)
'
```

`pct exec` passes binary data through unchanged (verified by md5 round-trip). Reading
through the old CT's namespace and writing through the new one is what makes the uid
shift correct: the archive records the *inside* uids (0, 1000), and the privileged CT
extracts them as the same numbers. No ownership fix step.

`tar` prints `socket ignored` warnings for X11, samba and buildkit sockets inside the
snapshots. They are expected and harmless.

Measured on the rehearsal: 13.1 GB in 2 m 16 s and 13.6 GB in 2 m 04 s — about 95 to
110 MB/s, ZFS to ZFS on one pool, with a scrub running. Budget the slower figure.

### 5.4 Point the moved config at the homes dataset

The old API drops unknown keys, so this cannot be a `PUT`. Edit the moved file with the
server still stopped:

```sh
ssh root@10.0.0.100 'pct exec 205 -- python3 - <<PY
import json
p = "/var/lib/docker/volumes/rmng-data/_data/config.json"
c = json.load(open(p))
c["docker"]["homesParent"] = "rpool/rmng-homes-105"
json.dump(c, open(p, "w"), indent=2)
print("homesParent =", c["docker"]["homesParent"])
PY'
```

The default is `tank/rmng/homes`, which fits nobody here. A first create with the wrong
parent fails with `no such pool`.

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

The old run flags plus the homes bind. `:shared` propagation is load-bearing — the
datasets are created from inside this container and only a shared bind propagates their
mounts into dockerd's namespace.

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

On boot the server:

1. folds the retired config fields (`clone_groups`/`codex_groups` → `groups`, per-preset
   provider defaults → one `group`) and rewrites `config.json`;
2. recreates `/dev/zfs` if the node is missing;
3. creates the shared pool at `/srv/rmng-homes/.shared`;
4. logs `gen-2 migration: N gen-1 clone(s) detected, migrating one at a time: …`;
5. stops every non-archived clone, then files one `Migrate` op per row in the jobs UI.

Per clone the op does: stop → `zfs create <parent>/<id>` → read `/home/rmng` out of the
stopped container → extract it into the dataset's `upper/` → remove the old container →
remove its `rmng-dind-*` and `rmng-ctd-*` volumes → build the preset image → export the
image's home as the overlay's skeleton → mount the overlay → create the gen-2 container
→ stop it.

After the last clone it starts every non-archived migrated clone and re-pushes each
one's stored Claude and Codex tokens. Archived clones stay stopped throughout.

Failures log and continue. One retry pass runs at the end.

**Follow `docker logs -f rmng`, not the jobs UI.** An operation is pruned from the state
60 s after it fails and 8 s after it succeeds (`jobs.rs`: `PRUNE_ERROR_MS`,
`PRUNE_DONE_MS`), so on a fleet-sized window almost every op will have vanished from the
UI by the time you look. The log keeps every `operation failed:` line, and the run ends
with one summary line:

```
gen-2 migration: 6 passed, 2 failed (haoran-rep-norow-arch, haoran-rep-norow-live), 3 started
```

**The FIRST clone takes several minutes longer than the rest.** Its op sits on
`build | Step 1/1 : FROM pegasis0/rmng-template:latest` while the daemon pulls the
current published template (about 11 GB unpacked). Every later clone reuses the tag —
all gen-1 presets resolve to the same default Dockerfile text, so the whole fleet shares
one derived image and one build. Do not restart anything while it looks stuck.

### 5.7 Restart the control-server once, after the last op

**This step is required, not hygiene.** The `data/hosts/<id>` symlinks — the ones the
`clones` SMB share, the ledger, the token scanner and the stuck detector all read
through — are written by a one-shot boot pass (`homes::sync_all`). That pass runs at the
same moment the migration starts, when no clone has a dataset yet, so it writes nothing.
The migration job itself never calls `homes::ensure_now` (the create and unarchive jobs
do; `run_migrate` does not). The directory therefore stays empty for the rest of the
server's life.

Measured on the rehearsal:

```console
$ pct exec 205 -- ls /var/lib/docker/volumes/rmng-data/_data/data/hosts/
total 0                                   # right after a clean 5/5 migration
$ pct exec 205 -- docker restart rmng
$ pct exec 205 -- ls /var/lib/docker/volumes/rmng-data/_data/data/hosts/
pega-rep-arch -> /srv/rmng-homes/.merged/pega-rep-arch
pega-rep-fwd  -> /srv/rmng-homes/.merged/pega-rep-fwd
…                                          # all five, after one restart
```

So: wait for `gen-2 migration: N passed, 0 failed`, then `docker restart rmng`, then
check the directory has one symlink per managed clone.

### 5.8 Restore the preset vars to the agent

The migration leaves the agent without the preset's `TURBO_*` / `ROLLBAR_*` vars (§2.4).
Put them back in every clone's home, where they survive fork and rebase:

```sh
ssh root@10.0.0.100 'pct exec 205 -- python3 /root/restore-preset-env.py'
```

The script reads the values stashed by §5.4, matches each clone to its preset, and writes
`~/.config/environment.d/10-rmng-preset.conf` (0600, uid 1000) into the clone's merged
home — `PATH` excluded, for the reason in §2.4. Archived clones get the file too; it
applies when they are unarchived.

Then pick the agent up on the running clones:

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

## 6. Verify

### 6.1 Rows

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
print("board:", [(c["id"], c["cloneIds"]) for c in s["boardColumns"]])
print("ops:", [(o["target"], o["status"], o["message"][:60]) for o in s["operations"]])'
```

Every managed row must have a `dataset` and a `baseTag`. A row without them did not
migrate — go back to §3.7.

### 6.2 Homes

```sh
ssh root@10.0.0.100 'pct exec 205 -- bash -lc "
  ls /srv/rmng-homes/                       # one dir per clone, plus .merged .shared .skeleton
  ls /srv/rmng-homes/.merged/               # one merged view per clone
  grep -c \" /srv/rmng-homes/.merged/\" /proc/self/mountinfo
  ls -l /var/lib/docker/volumes/rmng-data/_data/data/hosts/   # one symlink per clone
"'
```

Compare a file you know against its pre-migration checksum. On the rehearsal every
clone's 64 MiB blob, 0600 file, nested symlink and dot-config file matched exactly.

### 6.3 Clones

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

### 6.4 A migrated clone's desktop

`/api/state` must show `daemonConnected: true` for every started headed clone. Then take
a real frame from one:

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

### 6.5 End to end

Run the shipped gen-2 suite against the new server. It creates, prebuilds, forks,
rebases, archives, unarchives and deletes, and asserts a live frame from each new clone.

```sh
RMNG_E2E_SERVER=http://<new-ct-ip>:9000 RMNG_E2E_PRESET=<a preset name> \
  cargo run -p control-client --bin rmng_e2e
```

It temporarily rewrites that preset's startup script and restores it on the way out,
success or failure.

---

## 6.6 After ANY restart of the new CT: restart the clones

Rebooting the CT breaks every clone's home until you restart the clone containers. Seen on
CT 204 immediately after the reboot that moved its address: the dashboard was fine and
every clone showed a **black screen**.

**Cause 1 — the clones race the overlay remount.** Home overlays do not survive a CT
reboot; the server re-establishes them at boot (`home_overlay::remount_all`). But Docker
starts every `restart: unless-stopped` container at the same time, and a clone that wins
binds `<homes>/.merged/<id>` while it is still an empty directory. The clone's bind is
private, so the server's later mount never propagates in. Measured:

```console
container started: 2026-09-13T09:02:33.205691948Z    # the clone
server started:    2026-09-13T09:02:33.289668396Z    # 84 ms later
$ docker exec <clone> ls -A /home/rmng | wc -l        # 9
$ ls -A /srv/rmng-homes/.merged/<clone> | wc -l       # 23
```

**Cause 2 — a stale payload stamp hides a missing unit.** `rmng-session-holder.service`
is shipped by the server into the clone's home, not baked into the template, and the push
is gated on `/opt/rmng/.payload-hash`. The stamp was present and current while the unit
was absent, so the reconciler skipped the push and the clone-daemon crash-looped:

```
no session holder at /run/user/1000/rmng-session-holder.sock; starting the unit
systemctl --user start rmng-session-holder failed: Unit rmng-session-holder.service not found.
Error: no session holder … after starting it
rmng-clone-daemon.service: Scheduled restart job, restart counter is at 23.
```

Both causes are fixed in the build described in §0.2a: `remount_all` now mounts the
datasets, uses the dataset DIRECTORY as the overlay `upperdir`, and restarts any clone
whose overlay it had to establish. On an older build, recover by hand, in order:

```sh
ssh root@10.0.0.100 'pct exec 205 -- bash -lc "
  # 1. rebind the live overlay
  for c in \$(docker ps --format {{.Names}} | grep -v ^rmng); do docker restart \$c; done
  # 2. clear the stamp so the payload really re-pushes
  for c in \$(docker ps --format {{.Names}} | grep -v ^rmng); do
    docker exec \$c rm -f /opt/rmng/.payload-hash; done
  # 3. boot pass re-converges every clone
  docker restart rmng
"'
```

Then wait for `sync-all (boot): converged N clones` and check
`daemonConnected` is true for every headed clone.

Both are product bugs worth fixing rather than documenting: the clone's home bind should
be `rslave` (or the server should restart clones after `remount_all`), and the payload
stamp should be written only after the payload is verified on disk.

## 7. After the window

### 7.1 Take the old CT's address, or tell people the new one

The new CT gets a new MAC and therefore a new DHCP lease. Two choices:

- Leave the new address and tell everyone the dashboard moved. Simplest.
- Or copy the old CT's `hwaddr` into the new CT's `net0` so it inherits the lease. Then
  the old CT must **stay stopped** — two CTs with one MAC fight over the same lease.

Either way the old CT stays **stopped and not destroyed** until the new one has run a
full working day. Nothing must write to its Docker state after the copy.

### 7.2 Reclaim the dead images

Every clone now runs the one derived tag `rmng-p-<hash>`. The old per-clone commit
images (`pega-template6…13`, `hyperhost-worker3…5`, `haoran-*`, `ivan-*`) are referenced
by nothing and are the bulk of the CT's disk. Nothing deletes them for you: the purge
path only runs on delete and rebase, and the migration does not call it.

Leave them until the fleet has run a working day, then:

```sh
ssh root@10.0.0.100 'pct exec 205 -- bash -lc "
  docker images --format \"{{.Repository}}:{{.Tag}}\"   # read this list first
  docker rmi <each dead clone-source tag>
  docker system df
"'
```

Do not `docker image prune -a`: it would also take `pegasis0/rmng-template:latest` and
the derived tag out from under the running clones.

### 7.3 Inner Docker re-pulls

Each clone's inner Docker started empty (§2.2). The first inner build or `docker run` in
each clone re-pulls through the `rmng-registry` mirror. Expect a slow first build per
clone and nothing else.

## 8. Rollback

**Start the old CT.** The migration only read it (§3.3), so it is still the deployment it
was. Stop the new CT first — two servers must not drive two daemons at once — and if you
copied the MAC (§7.1), put it back on the old CT. Then destroy the new CT and its homes
dataset.

`pct rollback 105 pre-gen2` undoes the `docker stop` / `systemctl stop` writes as well, if
the old CT looks wrong when it comes up.

Restoring a dump is the last resort, for when the old CT itself is gone. It is hours of
restore and it loses everything done since the dump was taken.

There is nothing finer than this — no per-clone rollback, no partial undo.

---

## 9. Rehearsal record

Three full rounds on spare CTs on the same host, 2026-09-13 — one per production CT.

- **CT 107** — the gen-1 replica, rebuilt for each round. Ubuntu 26.04, unprivileged,
  Docker pinned to the round's production version, server `pegasis0/rmng:20260905`
  (= `592ad7e`, the exact build all three production CTs run), the round's real
  `config.json` with secrets replaced, the era template (`:20260706` for CT 105,
  `:20260728` for CT 104 and CT 106) retagged to `:latest` as its ONLY tag, per-clone
  `source` values rewritten to local-only commit-image names like production has, and one
  clone per row shape that CT carries. Every home seeded with a 64 MiB blob, a 0600 file,
  a nested symlink and a dot-config marker, so the copy can be checked by checksum.
- **CT 108 / CT 110 / CT 112** — the fresh privileged targets, one per round, built by §4.

Findings that changed this runbook are called out where they belong: §0.1 (the revision),
§0.2 (the image store), §0.3 (the base image), §2.3 (`--pull` blocks a local `FROM`),
§3.2 (dedup makes the copy almost free), §3.7 (a stuck gen-1 row bounces the fleet on
every restart), §4.2 (AppArmor on Ubuntu), §5.1 (`containerd` in the stop list),
§5.2 (clear the target first), §5.3 (the one-pipe move), §5.6 (ops prune out of the UI),
§5.7 (the missing home links).

### 9.0 Three things the rehearsal did NOT cover

- **lxcfs — and it cost the first real run.** The stage CT was built without lxcfs, so its
  clones carried no lxcfs binds and the target did not need it either. All three
  production CTs DO run lxcfs, and CT 104's first migration pass failed 8/8 because of it
  (§4.5). **A replica must mirror the source CT's lxcfs state**, or the rehearsal proves
  less than it appears to.
- **Scale.** The replicas carry 3 to 8 clones with ~2 GB homes. CT 106 has 104 rows and
  homes up to 44.8 GB. Nothing in the mechanism is per-fleet-size, but the window will be
  hours, not minutes, and the server holds one whole home in memory at a time (§1.2).
- **Real base images.** The replicas' clones were created from the published template and
  retagged, so §0.3's base-image loss is real but invisible on the replica: the old and
  new bases are close to the same image. On production the gap is 28 GB of commit layers
  (§2.2). That part must be decided, not tested.

### 9.1 Round 1 — the CT 105 shape

| | |
| --- | --- |
| Docker state moved | 13.1 GB in 2 m 16 s (~95 MB/s) |
| Migration window | 7 m 21 s wall for 5 clones with ~2 GB homes |
| Result | `gen-2 migration: 5 passed, 0 failed (), 2 started` |
| Home data | 20 of 20 sha256 checksums matched across the 5 clones; modes, a nested symlink and a dot-config marker all intact |
| Rows | archived, headless, sub-clone parent, the two port forwards, the muted set and both board columns all survived |
| Pools | `cloneGroups` + `codexGroups` folded into `groups` (`Medi` 5, `Personal` 6); all three presets got a `group` |
| Base tag | **all five clones got the same tag `rmng-p-52f68d…`** — one build for the whole fleet, because every gen-1 preset resolves to the same default Dockerfile text |
| Accounts | both credential files present in every started clone |
| `/etc/environment` | dynamic keys plus `LINEAR_API_KEY` only — the preset `PATH` and `TURBO_*` vars were gone, as §2.2 predicts |
| Home links | empty until the control-server was restarted (§5.7) |
| E2E | `RMNG_E2E_PRESET=medi … rmng_e2e` → **E2E PASS**: create, prebuild, fork, create-with-rebuild, archive, rebase, unarchive, delete, plus a live 1920×1080 frame from both new clones |

The window time scales with clone count and home size, not with the CT's total size. Most
of round 1's 7 minutes was the one-off template pull and skeleton export; the per-clone
part after that was under a minute each at 2 GB.

These rounds ran the SERIAL, buffering migration path and homes with no hard links, so
they measured neither of the two things that actually bit CT 104 (§0.2a).

### 9.2 Round 2 — the CT 106 shape

Eight rows on the stage CT: live headed top-level, live headed sub clone, live headless
sub clone, archived headed top-level, archived headed sub clone, archived headless sub
clone, and two rows whose containers were removed by hand (one live, one archived).

| | |
| --- | --- |
| Docker state moved | 13.6 GB in 2 m 04 s; pool allocation grew **1086 MiB** with `dedup=blake3` on the target dataset |
| Migration window | 7 m 34 s for 8 rows |
| Result | `gen-2 migration: 6 passed, 2 failed (haoran-rep-norow-arch, haoran-rep-norow-live), 3 started` |
| The two failures | exactly the container-less rows, with `404: No such container`. Both left an empty directory under the homes parent and no dataset |
| Restart behaviour | a later control-server restart re-ran the migration on the two stuck rows, **stopping and restarting the whole fleet** (§3.7) |
| Remediation | `POST /api/delete` removed both rows despite the missing container; the next restart filed no migration and the fleet stayed up |
| Home data | 24 of 24 checksums matched across the 6 migrated clones |
| Rows | headless, both sub-clone kinds and both board columns survived |
| E2E | **E2E PASS** with `RMNG_E2E_PRESET=talktomedi` |

### 9.3 Round 3 — the CT 104 shape

Three rows (live headed top-level, live headless sub clone, archived headed top-level),
one preset, **no account pools at all**, Docker 29.7.2 / containerd.io 2.3.3 on both CTs.

| | |
| --- | --- |
| Docker state moved | 13.6 GB in 1 m 54 s; pool allocation grew 733 MiB |
| Migration window | 4 m 50 s for 3 rows |
| Result | `gen-2 migration: 3 passed, 0 failed (), 2 started` |
| Home data | 12 of 12 checksums matched |
| Pools | with no pools configured, the preset's retired `claudeAccount: "auto"` folded to `group: "none"` — the documented fallback, no error |
| E2E | **E2E PASS** with `RMNG_E2E_PRESET=Medi` |

### 9.4 The migrated clones themselves

The e2e exercises clones it creates. These checks were run against clones that were
actually migrated:

- `daemonConnected: true` on every migrated headed clone, `false` on the headless ones
  and on the archived ones — which is correct in all three cases.
- A `screenshot` through `POST /api/hosts/<id>/mcp` returned a real 1920×1080 GNOME
  desktop from `haoran-rep-live`, `haoran-rep-sub-live` and `ivan-rep-live`.
- Both credential files (`~/.claude/.credentials.json`, `~/.codex/auth.json`) plus
  `~/.pi/agent/auth.json` present in every started clone.
- `/etc/environment` carried `RMNG_CONTROL_URL`, a fresh per-clone `RMNG_PROXY_KEY`,
  `LINEAR_API_KEY` and `ANTHROPIC_MODEL`, and nothing else.
- The container binds were the gen-2 shape: `.merged/<id> → /home/rmng`,
  `/srv/rmng-homes → /home/rmng/clones`, `.shared → /home/rmng/shared`, plus the fresh
  `rmng-dind-*` / `rmng-ctd-*` volumes.

**Fake accounts do exercise the token re-push.** The old rehearsal note said they could
not. They can, with two properties:

- `expiresAt` far in the future, so `fresh_access_token` returns early and never calls
  the provider;
- the Codex `accessToken` shaped as a JWT (starting `eyJ`), or `apply_clone_token` bails
  with `refusing to apply a non-JWT codex access token`.

With those, both `~/.claude/.credentials.json` and `~/.codex/auth.json` (and
`~/.pi/agent/auth.json`) land in the clone. Never copy a real refresh token into a
rehearsal CT: a refresh token is single-use, and a rehearsal refresh would kill the
production account.

---

### 9.6 CT 104 — the first real migration (2026-09-13)

Ran with the fixed build (§0.2a). CT 104 → **CT 204** (`ivan-rmng`, 10.0.0.125).

| | |
| --- | --- |
| Bulk copy | 98 GB in **18 m 47 s**; pool allocation grew **29 GiB**, not 98, with `dedup=blake3` on both new datasets |
| Migration | **7 passed, 0 failed, 6 started** in **13 m 43 s** for 7 clones, 4 at a time |
| Serial baseline | the same path one-at-a-time took **9 minutes for a single clone** — about 4.6× |
| Server memory | **0.04 GiB** RSS with four clones copying at once, against **11.0 GiB** for one before streaming |
| Homes | 8 of 8 datasets; five of six live clones matched their pre-migration byte/file counts to within 22 KB, the sixth to 0.11 % (see below) |
| Hard links | `ivan-dev-640`'s 28 276 internal `uv`-cache links came across **exactly** |
| Rows | archived flags, the headless sub clone, the parent link and both board columns all survived |
| Clones | 6 running with both credential files, 2 archived correctly stopped, 5 headed clones `daemonConnected`, live 1920×1080 frames |
| Agent env | `TURBO_API` / `TURBO_TEAM` / `TURBO_TOKEN` restored via §5.8 and confirmed in the agent's own `/proc/<pid>/environ` |

Three things to expect when reading the numbers:

- **`find -links +1` drops after migration on some clones** (`ivan-dev-640a`: 5 060 → 0)
  without any data loss. Those inodes had their other link OUTSIDE `/home/rmng`, so the
  archive carries them as plain files. The byte totals stayed flat, which is the proof —
  had they become copies the totals would have grown.
- **A small negative byte delta is shutdown churn, not loss.** The pre-migration manifest
  is taken while the clones still run; between it and the fleet stop, caches and logs
  move. Five clones drifted under 22 KB, `ivan-dev-640` by 14 MB over 15 files (0.11 %).
  Take the manifest immediately before §5.1 if you want it tighter.
- **`du -shx`, always.** Measured with the fleet up, plain `du` descends into every running
  container's overlay mount and double counts: CT 104 read 126 GB against a real 30 GB,
  and CT 106 read 271 GB for `/var/lib/docker` with a 526 GB "dind+ctd volumes" figure
  that is larger than the directory containing it. Measure with the fleet stopped.

### 9.7 CT 104, second run — the one that stuck

The first run (§9.6) migrated cleanly but produced unusable clones: every home arrived
root-owned (§0.2b), and the CT reboot that followed left them showing the bare template
(§6.6). Both were code bugs, not procedure. Fixed, rebuilt, and the whole migration redone
from CT 104's untouched containers.

| | |
| --- | --- |
| Re-copy | 98 GB in **17 m 28 s**; pool grew **24 GiB** |
| Migration | **8 passed, 0 failed** in **14 m 58 s**, 4 at a time |
| Ownership | `ivan-dev-640` **553 323 / 775 / 439** against the source's **553 312 / 776 / 439** — the drift is shutdown churn plus the files the server writes after create |
| Writability | every home writable by the clone user; `/home/rmng` itself `uid=1000 gid=1000 mode=755` |
| Desktops | the real desktop renders — wallpaper, dock, the operator's own apps — not the template default |
| **Reboot test** | CT rebooted with nothing touched afterwards: 9 datasets mounted, 8 overlays rebuilt, all 6 running clones on full homes, and the server logged `binds the live home overlay` **6 times** — it restarted every clone that raced the mount |

The reboot test is the one worth repeating on CT 105 and CT 106. It is the only check that
exercises `remount_all`, and all three of its bugs were invisible until a CT actually
restarted.

### 9.5 The rehearsal CTs are still up

Left running so the evidence can be inspected. They hold nothing of value; destroy them
when you are done reading:

```sh
ssh root@10.0.0.100 '
  for id in 107 108 110 112; do pct stop $id; pct destroy $id; done
  zfs destroy -r rpool/rmng-homes-r104
  zfs destroy -r rpool/rmng-homes-r105
  zfs destroy -r rpool/rmng-homes-r106
'
```

| CT | what it is | address |
| --- | --- | --- |
| 107 | the gen-1 stage, last rebuilt as the CT 104 shape | 10.0.0.194 |
| 108 | round 1 target (CT 105 shape) | 10.0.0.69 |
| 110 | round 2 target (CT 106 shape) | 10.0.0.70 |
| 112 | round 3 target (CT 104 shape) | 10.0.0.142 |

## 10. Order, and the decisions that are not mine

Do **CT 104 first**. It is the smallest (101 GB), it has one preset and no account pools,
and it fits in the free pool space as it stands. Then **CT 105**, then **CT 106**. CT 106
is last because it is the largest (917 GB), has the most rows (104), and is the only one
with rows that cannot migrate (§3.7).

Four things must be decided by a person before the first window, and none of them has a
safe default:

1. **The base image (§2.3).** Publish each CT's commit image to a registry and point the
   preset at it, or accept that every clone lands on the published template and
   re-install what matters. This is the only irreversible loss in the whole procedure.
2. **The preset vars (§2.4).** Which of `PATH`, `TURBO_*` and `ROLLBAR_ACCESS_TOKEN` go
   into each preset's Dockerfile. `PATH` at least is not optional — without it the
   clone's Node install is off the path.
3. **Dedup on both new datasets (§3.2a).** Not really a decision any more — it is the
   default answer — but it has to be done at the right moment, before the first write to
   each. Pool space itself is no longer a gate at 966 GB free (§3.2).
4. **The address (§7.1).** New IP and tell everyone, or move the MAC and keep the old
   lease.
