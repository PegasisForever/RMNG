# Gen-2 production upgrade notes (CT 105)

Lessons from converting CT 101 on 2026-09-08. Read with `docs/GEN2-CLONES.md` §5.

## Do not convert by restore

Restoring the dump as privileged corrupted LXC namespace state: every
`pct exec` and `docker exec` silently ran on CT files instead of container
files (UTS joined, mount namespace did not). Unrecoverable in place.
Build a FRESH privileged CT instead and redeploy rmng into it.

## Fresh privileged CT recipe (proven)

- `pct create` with `--unprivileged 0`, Debian 13 template, `nesting=1,keyctl=1,fuse=1`.
- Do NOT set `lxc.apparmor.profile: unconfined` (default profile).
- Do NOT bind-mount host `/dev/zfs` — it breaks nested container mount joins
  the same silent way. Use `lxc.cgroup2.devices.allow: c 10:249 rwm` plus
  `mknod /dev/zfs c 10 249` inside the CT (major:minor from host `ls -l`).
  The control-server recreates the node at boot when missing.
- `devices.allow` for tun (10:200) and the mount entry for `/dev/net/tun` are safe.
- GPU `dev0`/`dev1` lines are safe.
- `mp0: <pool>/rmng-homes,mp=/srv/rmng-homes` is safe.
- Docker: pin `docker-ce`, `docker-ce-cli`, `containerd.io` to the versions
  running on the current production CT (`apt-mark hold`). Newer combos were
  suspected during debugging and cleared, but there is no reason to drift.
- The rmng server image needs `zfsutils-linux` (already in the Dockerfile).

## Host-level gotchas

- Raise `fs.inotify.max_user_watches` BEFORE booting: VS Code watchers in
  neighboring CTs can exhaust it (1.2M seen, mostly CT 106), which stops
  systemd from starting Doctrine network, logind, everything. 2M worked.
- Never touch a CT's mounts mid-restore (`umount -l`, manual `zfs mount`,
  deleting the lock conf). It corrupts LXC state beyond repair.
- `pct exec` can silently land on host files after such surgery. Health check:
  `pct exec <id> -- ls / | grep -cE '^(deleg|pve)$'` must print `0`.
  Direct SSH into the CT is the reliable channel.

## Data migration

- The only backup that matters is the whole-CT dump. Per-clone backup is skipped.
- Fresh CT means no rmng state: re-run first setup, re-pull the template image
  (12.8 GB, ~4 min through the mirror), re-push accounts.
- Migration itself is the boot Migrate ops (one clone at a time, jobs UI).
  Proven on CT 101: fork ~17 s, rebase ~37 s, delete with purge <60 s.
