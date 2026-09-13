# Gen-2 clones

Gen-1 is today's clone: home lives in the container overlay layer, fork means file copy,
template means `docker commit`. Gen-1 is end of life: it runs until migration, then its
code is deleted. No dual paths, no compat layer.

Gen-2 is the new clone: home lives on its own ZFS dataset, fork is snapshot plus clone,
template is text plus tags. All new work goes here.

This is an internal tool doc. Rough edges are fine where noted.

## 1. What gen-2 gives you

- Instant fork at any size: `zfs snapshot` plus `zfs clone` of the home dataset, then
  `docker create`. Seconds plus boot, no file copy, no source downtime.
- Rebase: swap the system image under a kept home dataset. Stop, create from the new
  tag with the same dataset, start, keep the old stopped container until ready passes.
- No Commit: a template is a preset's full Dockerfile, built into an image tag.
  No binary blobs, no golden clone, no always-on cost.
- Lazy builds: the preset image builds on first create that needs it, keyed by a hash
  of the file text. Same text twice means one build; same text NEVER rebuilds (a base
  release under the same tag does not invalidate it). Refresh is manual: edit the
  Dockerfile or hit the preset's rebuild button. One lock per tag so parallel creates
  share the build.
- All static env lives in the Dockerfile as `ENV`, secrets included. Only per-clone
  dynamic keys (`RMNG_CONTROL_URL`, `RMNG_PROXY_KEY`, `ANTHROPIC_MODEL`) plus the
  Linear key (a visible preset field, injected at runtime) stay create-time.
- Manual drift: experimental installs inside a running clone are transcribed into
  the preset Dockerfile by hand. Drift drops silently on fork, rebase, and migration.
- Browsing works stopped: the home dataset is a plain dir on the CT, so `data/hosts`
  and SMB keep working when the clone is stopped. Better than gen-1.

## 2. Gen-1 end of life

Gen-1 runs unchanged until the migration window. No new features, no fork, no rebase,
no backports. After migration passes, the gen-1 provision path, seed file-copy path,
`/proc`-link home reader, `rmng clone cp`, `rmng clone sync`, the `--seed` flag, the
streaming local-dir upload route, and the Commit path are all deleted. Laptop-to-clone
push goes over SMB. Partial-dir copy has no replacement: fork takes the whole home.

## 3. Architecture

### 3.1 Host and outer CT

- Outer CT becomes privileged (one-way trip, see §5). Do NOT bind-mount host
  `/dev/zfs` (`lxc.mount.entry` for it breaks nested container mount joins: every
  `docker exec` silently lands on CT files). Instead pass `lxc.cgroup2.devices.allow:
  c 10:249 rwm` and create the node inside the CT with `mknod /dev/zfs c 10 249`
  (find the major:minor via host `ls -l /dev/zfs`). The node lives on the CT `/dev`
  tmpfs, so the control-server re-creates it at boot when missing. Keep
  `nesting=1,keyctl=1,fuse=1`. On an Ubuntu CT the `lxc.apparmor.profile: unconfined`
  block from `PROXMOX-LXC.md` §1 is still required, privileged or not — without it
  nested `docker run` dies with "the docker-default profile could not be loaded".
  Do NOT install `zfsutils-linux` in the CT: its `zfs-dkms` post-install builds kernel
  modules and fails under a shared kernel. The server image carries its own `zfs`, so
  the CT needs only the device node; run `zfs list` from the Proxmox host instead.
- Host layout: one parent dataset, e.g. `tank/rmng/homes` (pool name differs per
  host: `docker.homes_parent` config knob, default `tank/rmng/homes`), mounted into
  the CT once at `/srv/rmng-homes`. One child dataset per gen-2 clone:
  `<parent>/<id>`, created with `-o mountpoint=/srv/rmng-homes/<id>` so it lands
  under the bind (children otherwise auto-mount at the pool path).
- **The parent carries the storage properties; the server sets none of them.**
  `zfs::create` and `zfs::clone_dataset` pass `-o mountpoint` and nothing else, so
  `dedup`, `compression` and `recordsize` are inherited by every clone home, by
  `.skeleton/`, `.shared/` and `.merged/` (plain dirs in the parent), and by every
  fork. Create the parent with `dedup=blake3` and leave `compression`/`recordsize`
  alone: dedup matches blocks as written, so a different compression algorithm matches
  nothing, and dedup covers only blocks written after it is switched on.
  `RUNBOOK-GEN1-TO-GEN2.md` §3.1 has the create command.
- The rmng container bind-mounts the homes dir shared
  (`-v /srv/rmng-homes:/srv/rmng-homes:shared`). LOAD-BEARING: datasets are
  created from inside that container, and only a shared bind propagates their
  mounts into dockerd's namespace (plus server-side home reads/writes).
- The CT root can now destroy any pool dataset. All ZFS calls go through one wrapper
  locked to the configured parent subtree. No raw `zfs destroy` anywhere else.

### 3.2 Gen-2 container spec

The home is an OVERLAY, not a plain dataset bind (`home_overlay.rs`):

- `<homes>/.skeleton/<image-digest>/` — the image's own `/home/rmng`, exported once per
  image and shared by every clone on it. This is the read-only lower.
- `<dataset>/upper` + `<dataset>/work` — the clone's delta. The upper cannot be the
  dataset root: overlayfs needs its workdir on the same filesystem but outside the upper.
- `<homes>/.merged/<id>` — the merged view. THIS is what binds at `/home/rmng`.

A fresh dataset is empty, so a plain bind would shadow the template's whole home layer
(user units, toolchains, default configs). The overlay keeps the template as the single
source and makes rebase a swap of the lower under the same upper.

The mounts die with a CT reboot (host mounts outlive containers, not reboots), so boot
re-establishes every clone's merged view (`home_overlay::remount_all`).

Three further binds beyond gen-1: `<homes>/.merged` at `/clones` (§3.6),
`<homes>/.shared` at `/shared`, and the unchanged per-clone `rmng-dind-*` /
`rmng-ctd-*` volumes. Everything else (privileged, cpu/mem, shm, `rmng-sock`, lxcfs
binds, `rmng` bridge) is unchanged. Record the base image tag plus dataset name on the
clone row (new optional fields on `RmngClone`, serde-defaulted so old `state.json`
loads). No gen label: after migration every clone is gen-2, and the dataset mount itself
marks one.

### 3.3 Preset images

- Each preset carries its own FULL Dockerfile, edited on its Settings card by anyone:
  single user, trusted network, no auth. Default: `FROM pegasis0/rmng-template:latest`.
  The FROM line may name any image, not only clone sources.
- Tag = `rmng-p-<hash(file text)>`. Create ensures the tag, building the text VERBATIM
  on miss (no FROM rewrite, no digest pinning, nothing appended). Same text twice means
  one build; same text never rebuilds. The preset card's Rebuild button warms a tag
  from the editor's current text without creating.
- Build failure fails the create with logs attached. Fix the Dockerfile, then retry.
- Purge on delete: when no remaining clone references a tag, `rmi` it. Refcount is a
  scan of clone rows.
- Secrets ARE allowed in the file (ENV lines bake into the layers). Accepted: anyone
  with daemon access can read them from layer history. The Linear key stays OUT of the
  file: it remains a preset field, injected at runtime as `LINEAR_API_KEY`.

### 3.4 Flows

- Create (gen-2): resolve tag (build if miss) → `zfs create` dataset → ensure the
  image's skeleton export → mount the overlay → `docker create` with the merged view at
  `/home/rmng` → write identity plus dynamic env → start → wait-ready. Failure trap
  tears down the merged mount and destroys the container and the dataset, like today's
  volume cleanup.
- Fork: `zfs snapshot <src>@<ts>` → `zfs clone` to new dataset → create from the
  TARGET preset's Dockerfile (built lazily inside the create) → start. The source
  contributes only its home. Same preset reuses the source tag with zero rebuild,
  because the text hashes the same. Source keeps running. Overlay drift is
  silently dropped.
- Rebase: record the old tag → stop → remove the old container (the name equals the
  id, so both cannot exist together) → ensure the target preset's image (build on
  miss, or forced by the rebuild checkbox) → create from it with the SAME dataset,
  same id → start → wait ready. The clone's own preset bindings stay — rebase swaps
  the image only. New one healthy: purge the old tag when unused. New
  one fails: auto-recreate from the old tag on the same dataset.
- Delete: stop → remove container → `zfs destroy` dataset → destroy its origin
  snapshot when no other dataset references it (fork creates pair them 1:1, so this is
  exact in the common case; shared origins are kept) → `rmi` the base tag when no
  remaining clone references it.

### 3.5 Env and presets

Env is the server's job, not the image's. A preset carries a `vars` list, and
`provision.rs` `clone_etc_environment_conf` writes it into the clone's `/etc/environment`
at create and on every resync — so an edit reaches a running fleet without a rebuild or a
rebase, and `clone_reconcile` restarts the agent when the file really changed.

`/etc/environment` is the only carrier that reaches every way into a clone. systemd is PID 1
there and does not hand its own environment to the services it starts, so a Dockerfile `ENV`
reaches `docker exec` and nothing else — not an SSH login, not the desktop. `/etc/environment`
is read by `pam_env` for SSH logins AND for the lingering user manager (through the
`/usr/lib/environment.d/99-environment.conf` symlink, so the GNOME session and every unit
under it inherit it), and the exec that starts the agent sources it explicitly.

`PATH` is not special-cased. It was, with per-shell rc drop-ins, only so fish would find a
node installed by nvm inside the home; clones take node from the image now.

Presets also keep their name, Linear identity (a regular visible field — blank clears it),
account selection, ticket auto-select, Dockerfile, and playbook/prompt appends.

### 3.6 Homes browsing plus cross-clone view

Point `data/hosts/<id>` at the clone's MERGED view (`<homes>/.merged/<id>`) for every
clone. Same SMB share, works while stopped. The `/proc/<pid>/root` reader is deleted
with the rest of gen-1.

Every clone also mounts `<homes>/.merged` at `/clones`, reached as `~/clones/<id>`, so
any clone reaches any other home. This replaces `cp`/`sync`: read or copy straight
across, no server round-trip. Accepted: every clone can read every home including
tokens, and each clone also sees itself at `~/clones/<self>`.

Both this and the shared pool mount OUTSIDE the home, with `~/clones` and `~/shared`
left as symlinks (`home_overlay::ensure_home_links`, applied at create and to the whole
fleet at boot). GNOME's file manager puts a sidebar row on every mount whose path is
under the home directory, and each sibling home is its own overlay mount — so mounting
the browse root at `~/clones` gave every clone's Files window one row per clone in the
fleet, eleven rows on CT 204. Measured in a clone: a mount under the home is listed, the
same mount outside it is not, and so is one whose path holds a dot component. Removing
GNOME's disk monitor does NOT help — GIO falls back to a built-in monitor with the same
rule.

Mount the `.merged` root, not the homes parent. The parent holds one ZFS *dataset* dir
per clone, and a dataset dir contains the overlay's `upper`/`work` pair, not a home —
mounting it shows `~/clones/<id>/upper/...` instead of `~/clones/<id>/...`, and keeps
showing the mountpoint dir of every clone whose dataset has been destroyed. `.merged`
holds exactly one entry per clone with a live home, and each entry IS that home: the
same set `data/hosts` links to, so the SMB share and this bind show one view.

The bind must be `rslave`. Each sibling home is its own overlay mount *under* the
source, and Docker's default private bind copies only the mounts that exist when the
container starts: a clone created later appears as an empty directory and stays empty
for the life of the container. As a slave of the CT's `<homes>` peer group the view
tracks the CT — homes appear as clones are created, vanish as they are deleted — while
mounts made inside a clone still never escape to the CT.

## 4. Migration (one shot, per CT)

The plan is now a runbook against the three boxes that actually have to move:
**[RUNBOOK-GEN1-TO-GEN2.md](RUNBOOK-GEN1-TO-GEN2.md)** (CT 104, CT 105, CT 106). It
carries the measured state of each box, the per-clone loss list, the commands, the
verification gates and the rollback. Read it instead of planning from here.

The shape, in one paragraph: dump the CT, build a FRESH privileged CT (never restore the
dump as privileged — it corrupts LXC namespace state), move `/var/lib/docker` **and**
`/var/lib/containerd` into it through one `pct exec … | pct exec …` pipe, set
`docker.homesParent` on the moved config with the server stopped, then boot the gen-2
image with `-v /srv/rmng-homes:/srv/rmng-homes:shared` added to the old run flags. On
boot the server files one `Migrate` op per gen-1 row (`OperationKind::Migrate`, same jobs
plumbing as clone and delete), stops the fleet, and migrates clones
`jobs::MIGRATE_CONCURRENCY` (4) at a time:

- `zfs create` the new dataset;
- stream the home out of the stopped container into the dataset's overlay `upper/`
  (streamed, not buffered: a whole home in RAM is what used to force one clone at a time);
- remove the old container and its `rmng-dind-*` / `rmng-ctd-*` volumes (inner Docker
  state drops and re-pulls through the mirror);
- build the ROW PRESET's Dockerfile and create the gen-2 container from it — **not** from
  the clone's old `source` image, which is ignored;
- stop it. Archived clones stay stopped throughout.

Failures log and continue, with one retry pass at the end. Then the non-archived clones
start and their stored accounts are re-pushed. Restart the control-server once afterwards
so the `data/hosts/<id>` links are written (the migration job does not write them).

Stage 4 — deleting the gen-1 code — happens after a clean pass on all three CTs.

## 5. Prerequisites (in order)

1. Rehearse on SPARE CTs, never the live CT first. A replica must mirror the source CT's
   lxcfs state, or it proves less than it appears to.
2. Create the homes dataset, mount it into the CT, smoke-test snapshot/clone/destroy
   timing from inside the CT.
3. Land the store dataset plus base-tag fields, then create/fork/rebase/delete for gen-2.
4. Run the migration, then delete gen-1 code.

## 6. Accepted rough edges

- No per-release home versions: creates take latest home. Rollback of a home is
  manual from pre-edit snapshots.
- Snapshot of a live home is crash-consistent only. Fine for dev homes, not live DBs.
- No tag rollback by hand: old tags purge when unused. Rebase holds the old tag
  until the new container passes ready, then purges it.
- Fork, rebase, and migration silently drop overlay drift. No merge, no warning.
- Fork takes the whole home, caches and all. Blocks are shared so disk is fine.
- Any clone reads any home via `~/clones`, tokens included. Chosen over access control.
- Secrets baked into preset images are readable from layer history by anyone with
  daemon access. Accepted over a secrets pipeline.
- Same Dockerfile text never rebuilds: a base release under the same tag does not
  invalidate the preset image. Refresh is manual (edit or rebuild button).
- Inner Docker state drops at migration and re-pulls.

## 7. UI (decided, not built)

Clones come into being exactly two ways: template create (base image plus empty
home) or fork (snapshot plus clone of a live source). No other create path.

- New clone modal: source picker lists CLONES only, never templates. Creating
  always forks the selected clone (snapshot plus clone, source keeps running).
  No image picker in this modal.
- Template create: separate button plus separate modal, for the bootstrap case
  (first clone, clean start). Title plus preset only; the base shown comes from the
  preset's FROM line. Starts empty; the agent pulls the repo itself.
  CLI/API template create stays regardless.
- Seed snapshot: DELETED as redundant. Every modal create forks a live source,
  so the source snapshot covers starting content. Pending code removal:
  `seed_snapshot` config field plus the `CloneFromSnapshot` home-source path.
- Clone menu: Rebase lives here. The dialog picks a preset (never an image list —
  the list stays internal for garbage collection) plus a rebuild checkbox, and tracks
  the op to settle. No separate fork item; the new clone modal covers forking.
- Open UI items: none.
