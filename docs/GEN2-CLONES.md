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
- No Commit: a template is base Dockerfile plus per-profile extra lines, built into a
  derived image tag. No binary blobs, no golden clone, no always-on cost.
- Lazy builds: the derived image builds on first create that needs it, keyed by a hash
  of lines plus base digest. Same lines twice means one build. One lock per tag so
  parallel creates share the build.
- Per-profile static env moves into the Dockerfile lines as `ENV`. Only per-clone
  dynamic keys (`RMNG_CONTROL_URL`, `RMNG_PROXY_KEY`, `machine-id`) stay create-time.
- Manual drift: experimental installs inside a running clone are transcribed into
  profile lines by hand. Drift drops silently on fork, rebase, and migration.
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

- Outer CT becomes privileged (one-way trip, see §5). Pass `/dev/zfs` in, install
  `zfsutils-linux` at the exact host version, keep `nesting=1,keyctl=1,fuse=1` and
  `apparmor unconfined`. Check `zfs list` works inside the CT.
- Host layout: one parent dataset, e.g. `tank/rmng/homes`, mounted into the CT once at
  `/srv/rmng-homes`. One child dataset per gen-2 clone: `tank/rmng/homes/<id>`.
- The CT root can now destroy any pool dataset. All ZFS calls go through one wrapper
  script locked to the `tank/rmng/homes/*` subtree. No raw `zfs destroy` anywhere else.

### 3.2 Gen-2 container spec

Same as gen-1 (`docker.rs` `create_clone_container`) plus one bind mount:
`<dataset-dir> -> /home/rmng`. Everything else (privileged, cpu/mem, shm, `rmng-sock`,
lxcfs binds, `rmng` bridge) is unchanged. Record the base image tag plus dataset
name on the clone row (new optional fields on `RmngClone`, serde-defaulted so old
`state.json` loads). No gen label: after migration every clone is gen-2, and the
dataset mount itself marks one.

### 3.3 Derived images

- Profile lines are text per template in config/state, edited in Settings by anyone:
  single user, trusted network, no auth.
- A template may hold one home seed snapshot. Creates clone from it by default and
  start with content; empty seed means a fresh home. Seed refresh is manual.
- Tag = `rmng-p-<hash(lines + base digest)>`. Create checks the tag, builds on miss
  with the existing buildkit setup, then proceeds. Base release changes the digest, so
  the next create auto-rebuilds. Always latest: no picking old tags.
- Build failure fails the create with logs attached. Fix lines or base, then retry.
  Optional prebuild button warms a tag without creating.
- Purge on delete: when no remaining clone (running or stopped old container)
  references a tag, `rmi` it. Refcount is a scan of clone rows.
- No secrets in lines. Linear keys and account picks stay on presets as today.

### 3.4 Flows

- Create (gen-2): resolve tag (build if miss) → `zfs create` dataset, or `zfs clone`
  from the template seed snapshot when one exists → `docker create`
  with the mount → write identity plus dynamic env → start → wait-ready. Failure trap
  destroys the container and the dataset, like today's volume cleanup.
- Fork: `zfs snapshot <src>@<ts>` → `zfs clone` to new dataset → create from the
  source's recorded base tag → start. Source keeps running. Overlay drift is
  silently dropped.
- Rebase: record the old tag → stop → remove the old container (the name equals the
  id, so both cannot exist together) → create from the new tag with the SAME dataset,
  same id → start → wait ready. New one healthy: purge the old tag when unused. New
  one fails: auto-recreate from the old tag on the same dataset.
- Delete: stop → remove container → `zfs destroy` dataset → destroy its origin
  snapshot when no other dataset references it (fork creates pair them 1:1, so this is
  exact in the common case; shared origins are kept) → `rmi` the base tag when no
  remaining clone references it.

### 3.5 Env and presets

Static preset vars move into profile lines. The `/etc/environment` writer
(`provision.rs` `clone_etc_environment_conf`) writes dynamic keys only. No retired-key
cleanup: gen-2 images never carried the old keys. Presets keep Linear identity, account selection, ticket
auto-select, playbook and prompt appends.

### 3.6 Homes browsing plus cross-clone view

Point `data/hosts/<id>` at the dataset dir for every clone. Same SMB share, works
while stopped. The `/proc/<pid>/root` reader is deleted with the rest of gen-1.

Every clone also mounts the homes parent dir at `/home/rmng/clones`, so any clone
reaches any other home at `~/clones/<id>`. One mount per clone, new ids appear
automatically, deleted ids vanish with their dataset. This replaces `cp`/`sync`:
read or copy straight across, no server round-trip. Accepted: every clone can read
every home including tokens, and each clone also sees itself at `~/clones/<self>`.
Keep an empty `clones` dir in the dataset so the mountpoint always exists.

## 4. Migration plan (one shot)

Decisions locked: no overlay-drift handling, no per-clone backup, whole fleet in one
window. Rollback is restore the whole outer LXC from its dump, nothing finer.
The new server version carries migration code only: it reads gen-1 homes to copy
them, but cannot run gen-1 clones.

1. Dump the entire outer LXC and verify the dump. This is backup and rollback.
2. Recreate the CT as privileged from that dump, fix shifted ownership, reinstall
   the daemon, re-apply mounts, verify `zfs list` plus nested `hello-world`.
3. Run the built-in rmng self-update to the gen-2 version.
4. On boot the new control-server auto-files one Migrate op per gen-1 clone in the
existing jobs UI (`OperationKind::Migrate`, step plus rolling log over SSE, same
plumbing as clone and delete). It stops the fleet, then migrates one clone at a time, same id. Archived
   clones stay stopped throughout:
   - `zfs create` the new dataset.
   - Copy home out of the stopped container (`docker cp <id>:/home/rmng`) into it.
   - Remove the old container. Fresh `rmng-dind-*` volumes: inner Docker state
     drops and re-pulls through the mirror. Say so in the window notice.
   - Create the gen-2 container from the base tag with the dataset at `/home/rmng`,
     fresh identity plus dynamic env, re-push accounts. Do not start yet.
   - Failures log and continue; one retry pass at the end.
5. Start the fleet, watch ready per clone.
6. After a clean pass, delete the gen-1 migration code with the rest of gen-1.

Time math: slowest single copy times fleet size, plus boot per clone.
One window, one per-clone report of bytes plus pass or fail.

## 5. Prerequisites (in order)

1. Rehearse on a SPARE CT first, never the live CT first: dump, restore as
   privileged, fix ownership, verify `zfs list` plus nested `hello-world`.
2. Create `tank/rmng/homes`, mount into the CT, smoke-test snapshot/clone/destroy
   timing on a tens-of-GB scratch dataset from inside the CT.
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
- Inner Docker state drops at migration and re-pulls.
