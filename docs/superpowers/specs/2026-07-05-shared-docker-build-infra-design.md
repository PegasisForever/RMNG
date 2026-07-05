# Shared Docker build infra for clones (pull-through mirror + remote BuildKit) — design

**Date:** 2026-07-05
**Status:** Approved (brainstorm), pending implementation plan
**Relates:** the per-clone nested-Docker isolation (`rmng-dind-*` / `rmng-ctd-*` volumes) established in
`crates/control-server/src/docker.rs`; the SSH reconciler pattern in `crates/control-server/src/ssh.rs`.

## Summary

Give every clone a **shared Docker Hub pull-through cache** and a **shared build-layer cache**, so base-image
pulls hit Hub once per fleet (no rate limits, gotcha #9) and identical `RUN`/`COPY` layers are built once and
reused across clones. Two long-lived infra containers, created and owned by the control-server on the existing
`rmng` bridge:

- **`rmng-registry`** — a `registry:2` pull-through cache for `registry-1.docker.io`.
- **`rmng-buildkit`** — a shared `moby/buildkit` daemon; each clone's `docker build` routes to it via a
  `--driver remote` buildx builder, sharing one layer cache across the whole fleet.

Deployment is **automatic**: when the new control-server starts it ensures both volumes + both containers
(idempotent create/start/recreate-on-drift), exactly as it already lazily ensures the `rmng` network. **Existing
clones are migrated live** by a new background reconciler (modeled on `ssh::run`) that pushes the mirror config +
buildx builder into every running clone with no downtime — no clone restart, no dropped containers or builds.

## Requirements (locked during brainstorm)

| Decision | Choice |
|---|---|
| Build-cache mechanism | **Shared remote BuildKit** — one `rmng-buildkit`, clones use `--driver remote`. Transparent `docker build` (no per-build flags). |
| Pull-through mirror | `registry:2` proxy of `registry-1.docker.io` only (dockerd `registry-mirrors` applies to `docker.io` only). |
| Reachability | Both infra containers on the `rmng` bridge; clones dial them by container DNS name (`rmng-registry`, `rmng-buildkit`) — same mechanism clones already use for `rmng-control`. |
| Auto-start | Control-server ensures volumes + containers at startup (`ensure_build_infra`, gated on `setup_complete && daemon_ok`), idempotent. |
| Migration of existing clones | **Live**, via a new `buildinfra::run` reconciler loop; applied inline at provision for new clones. |
| BuildKit resource cap | **Uncapped** (no `buildkit_cpus`/`buildkit_memory_mb` knobs) — small trusted fleet. |
| Image pinning | **Readable pinned version tags** (`registry:2.8.3`, `moby/buildkit:v0.17.x`), config-overridable (operator may substitute a digest). |
| Cache GC | BuildKit GC keep-bytes cap via `buildkit_cache_gb` (default 40). |
| Config surface | New `DockerConfig` fields in config.json only (no-env-settings invariant); master toggle `build_infra_enabled` (default `true`). |

**Explicitly out of scope (YAGNI):** mirroring registries other than Docker Hub (ghcr/gcr — not supported by
`registry-mirrors` anyway); mTLS on the BuildKit GRPC endpoint (plaintext on the trusted private bridge, matching
the "trust the private network" posture of the 9000 API); per-clone build quotas / BuildKit resource caps; a
wizard/Settings UI (config.json + defaults only); pushing *built* images to the shared registry (it is a
pull-through cache only, not a fleet image store).

## Architecture

```
                     Docker host daemon (one, shared)
  ┌──────────────────────────────────────────────────────────────────┐
  │  control-server ──ensures──▶ rmng-registry (registry:2 proxy)     │
  │      │                          └─vol rmng-registry-data          │
  │      └──ensures──────────────▶ rmng-buildkit (moby/buildkit)      │
  │                                   └─vol rmng-buildkit-cache        │
  │                                                                    │
  │        rmng bridge (DNS by container name)                         │
  │   ┌──────────────┐   ┌──────────────┐   ┌──────────────┐          │
  │   │ clone A      │   │ clone B      │   │ clone C      │  …        │
  │   │ inner dockerd│   │ inner dockerd│   │ inner dockerd│          │
  │   │  ▲ mirror    │   │  ▲ mirror    │   │  ▲ mirror    │          │
  │   │  │ builder───┼───┼──┐           │   │              │          │
  │   └──┼───────────┘   └──┼┼──────────┘   └──────────────┘          │
  │      │ registry-mirrors │└─ buildx --driver remote ──▶ rmng-buildkit
  │      └──────────────────┴──── http://rmng-registry:5000 ──▶ rmng-registry
  └──────────────────────────────────────────────────────────────────┘
```

- Infra containers carry a new label **`rmng.infra=1`**, **not** `rmng.managed=1` — so they are excluded from
  `list_managed_containers` clone sweeps and from the boot reconciler's "managed container with no host row"
  warning (`crates/control-server/src/main.rs`, the boot reconcile block). Same posture as the deliberately
  unlabeled `rmng-self-upgrade` helper (`docker.rs` `launch_upgrade_helper`).
- Per clone, two pieces of state live in the **clone's writable layer / home** and therefore **persist across
  stop/start** — so migration is apply-once, and the reconciler only re-verifies cheaply:
  1. `/etc/docker/daemon.json` — the mirror config.
  2. `~rmng/.docker/buildx/` — the remote builder instance.

### Reachability & DNS

Clones already resolve other containers on the `rmng` bridge by name (the clone media socket note: "the
container name is the DNS address"; `CONTROL_ALIAS = rmng-control` proves it). So `rmng-registry` /
`rmng-buildkit` are reachable by name from every clone with **no new mounts and no new ports**. BuildKit GRPC is
plaintext TCP on the bridge (`tcp://rmng-buildkit:1234`); the registry serves HTTP on `:5000`.

## Components

### New: infra ensure in `crates/control-server/src/docker.rs`

`DockerCtl::ensure_build_infra(&self) -> Result<()>`, sibling to `ensure_network` (`docker.rs:754`) and reusing
`ensure_volume` (`docker.rs:1337`):

1. `ensure_volume("rmng-registry-data")`, `ensure_volume("rmng-buildkit-cache")`.
2. For each of the two containers: inspect by name; **create** if absent, **start** if stopped, **recreate**
   (stop→remove→create→start) if the running spec has drifted from the desired spec (image ref changed, or the
   BuildKit GC cap changed). Attach to `NETWORK`. Label `rmng.infra=1`. `restart: unless-stopped`.
   - `rmng-registry`: image `docker.registry_image`; env `REGISTRY_PROXY_REMOTEURL=https://registry-1.docker.io`;
     mount `rmng-registry-data` → `/var/lib/registry`.
   - `rmng-buildkit`: image `docker.buildkit_image`; `--privileged`; args
     `--addr tcp://0.0.0.0:1234 --config /etc/buildkit/buildkitd.toml`; mount `rmng-buildkit-cache` →
     `/var/lib/buildkit`; the `buildkitd.toml` (GC `keepBytes = buildkit_cache_gb * GiB`) written into the
     container via `upload_tar` before start.

Called from the startup path in `main.rs` right after `self_setup` populates the env / ensures the network,
gated on `config.setup_complete && report.daemon_ok && config.docker.build_infra_enabled`. Non-fatal and
bounded, same posture as the existing `self_setup` / boot-reconcile blocks (`main.rs:79-154`): a down daemon
logs a warning and retries next boot.

### New: `crates/control-server/src/buildinfra.rs` (the clone reconciler)

Modeled directly on `ssh.rs` (`ssh::run` loop + per-clone push). One background task spawned from `main.rs`
next to `tokio::spawn(ssh::run(app.clone()))` (`main.rs:169`):

- Loop every N seconds (match `ssh`/`homes` cadence). Skip entirely if `!build_infra_enabled`.
- List running managed clones (`list_managed_containers`, running only).
- Per clone, idempotently:
  - **`ensure_clone_mirror`** — read the clone's `/etc/docker/daemon.json` via `exec` (empty if absent), merge
    `registry-mirrors: ["http://rmng-registry:5000"]` + `insecure-registries: ["rmng-registry:5000"]` in Rust
    (the HTTP mirror *requires* the insecure entry), and if changed write it back via `upload_tar` and
    `kill -HUP` the inner dockerd (`pkill -HUP dockerd` via `exec_script`). Both keys are SIGHUP-reloadable, so
    **no container or in-flight build is dropped**. If already present → no write, no HUP.
  - **`ensure_clone_builder`** — as uid 1000 (`exec_script` with the clone user), `docker buildx inspect rmng`;
    if absent, `docker buildx create --name rmng --driver remote --driver-opt default-load=true
    tcp://rmng-buildkit:1234 --use`. `default-load=true` keeps `docker build -t x . && docker run x` working
    transparently (the remote driver otherwise leaves the result only in BuildKit).

Same idempotent push is invoked **inline at provision** so new clones get it immediately rather than waiting a
reconciler tick — call site in `clone_container_after_create` (`provision.rs:315`), after the existing
identity/preset/SSH injection block (`provision.rs:419-420`), once the container is up.

### `crates/wire/src/config.rs` — `DockerConfig`

New fields on `DockerConfig` (`config.rs:186`), each with a `default_*` matching the existing
`template_reference`/`server_image` pattern (`config.rs:208-219,243`):

- `build_infra_enabled: bool` (default `true`) — master switch (ensure + reconciler both honor it).
- `registry_image: String` (default `"registry:2.8.3"`).
- `buildkit_image: String` (default `"moby/buildkit:v0.17.x"` — exact patch pinned during implementation).
- `buildkit_cache_gb: u32` (default `40`).

Serde defaults so existing config.json files load unchanged (older deploys get the feature on next boot).

### Docs

- `docs/DEPLOY.md` — a short "Shared build cache & Docker Hub mirror" section: what the two `rmng-infra`
  containers are, that they start automatically, the `build_infra_enabled` toggle, and the two operator-visible
  facts (builds run on the shared `rmng-buildkit`; if it is down, in-clone `docker build` fails until it is
  back, with `docker buildx use default` as the local fallback).
- `docs/PROXMOX-LXC.md` / gotchas — note that the pull-through mirror is the fix for the Hub rate-limit
  (gotcha #9) and that the per-clone `rmng-dind-*`/`rmng-ctd-*` isolation is unchanged (the shared cache is via
  BuildKit/registry, **not** a shared `/var/lib/docker`, which concurrent daemons cannot share).

## Error handling / edges

- **Daemon down at startup** — `ensure_build_infra` skipped/warned, retried next boot; identical to
  `ensure_network`'s posture. Clones without the mirror still work (direct Hub pulls); without the builder,
  `docker build` fails until the reconciler reaches them.
- **`rmng-buildkit` down at build time** — the remote builder fails with a clear connect error; the clone's
  local `default` builder remains registered as the manual fallback (`docker buildx use default`). Documented.
- **Pre-existing per-clone `daemon.json`** — merged, not clobbered (read → set two keys → write back). If the
  two keys already match, the reconciler makes no write and sends no HUP (true idempotency).
- **HTTP mirror without the insecure entry** — dockerd would attempt HTTPS to `rmng-registry:5000` and fail;
  the merge always writes **both** `registry-mirrors` and `insecure-registries`. Asserted by a unit test.
- **Config drift on the infra containers** (image/tag or GC cap changed via config) — `ensure_build_infra`
  detects the drift and recreates the affected container; the cache volume is preserved across the recreate.
- **`docker commit` of a clone** — bakes the (harmless) `daemon.json`/buildx state into the derived image; both
  re-point at the same stable infra names, so a clone made from that image still works. The `rmng-dind-*` /
  `rmng-ctd-*` volumes remain deliberately un-committed (unchanged from today).
- **Subnet change** — infra containers are re-ensured on `NETWORK`; a recreate re-attaches them to the new
  bridge, same as clones.

## Testing

- **Unit (Rust)** — the `daemon.json` merge: (a) empty/absent file → both keys written; (b) pre-existing file
  with unrelated keys → keys added, others preserved; (c) already-applied → no change (drives the no-HUP path).
  Pure, table-style, matching the `render_smb_conf` / `entries_to_remove` test convention.
- **Unit (Rust)** — config defaults + round-trip: an old config.json (without the new fields) deserializes with
  `build_infra_enabled = true` and the pinned image/GC defaults.
- **Unit (Rust)** — desired-vs-running drift comparison for `ensure_build_infra` (image ref / GC cap change ⇒
  recreate; identical ⇒ no-op), matching the like-for-like reconcile comparisons already in `docker.rs`.
- **End-to-end (the real proof) — on a freshly-provisioned Proxmox LXC:** stand up a brand-new Ubuntu 26.04
  unprivileged LXC on the Proxmox box at `root@10.0.0.100` following `docs/PROXMOX-LXC.md` verbatim (nesting +
  `keyctl` + `fuse`, `renderD128` passthrough, AppArmor-unconfined conf lines; raise the host keyring sysctls;
  install Docker CE; verify `overlay2` + render node + `hello-world`), then `docker compose up -d` this branch's
  control-server image and run the wizard. This gives a clean host with **no pre-existing infra containers or
  clones**, so it exercises first-boot auto-start and provisioning from zero — not a warm box. Then:
  1. **Auto-start from zero:** on first boot assert `rmng-registry` + `rmng-buildkit` come up (`rmng.infra=1`,
     running, on the `rmng` bridge) with no operator action.
  2. **Transparent build via shared BuildKit:** create a fresh clone → `docker build` a small Dockerfile in it →
     confirm it routes to `rmng-buildkit` (buildkit logs the build) and `docker run` of the result works
     (default-load).
  3. **Cross-clone layer-cache hit:** second clone, identical Dockerfile → confirm the layer cache hits (build
     near-instant; buildkit reports cached steps).
  4. **Pull-through mirror:** `docker pull ubuntu:26.04` in one clone populates `rmng-registry`; a second clone's
     pull is served from the cache (registry logs a hit, no Hub round-trip).
  5. **Live migration of a pre-existing clone:** create a clone and start an inner container in it *before*
     upgrading; then restart/redeploy the control-server and confirm the `buildinfra` reconciler live-applies
     the mirror + builder (`daemon.json` + `docker buildx ls` show them) with the clone's pre-existing inner
     container still running the whole time (no drop — verify `docker ps` inside the clone before/after and that
     no SIGHUP-restart of the inner dockerd tore it down).
  6. **Toggle off:** set `build_infra_enabled=false`, restart → confirm the reconciler stops touching clones
     (infra containers may remain; document the exact teardown behavior chosen in the plan).
  7. **Teardown:** destroy the throwaway LXC when done (leave `10.0.0.100` clean).

  This is the pass/fail gate; it cannot be faked in a unit test. Capture the concrete `pct`/`docker` command
  transcript into the plan's E2E task so it is reproducible.

## Open items carried into the plan

1. **Exact `moby/buildkit` + `registry` patch tags** — pin the current stable patch when implementing on
   CT 106 (verify each pulls cleanly).
2. **BuildKit GRPC endpoint detail** — confirm the `--driver remote tcp://…` builder connects plaintext without
   client certs on the bridge; if buildx insists on TLS for the remote driver, fall back to exposing BuildKit
   over the shared `/srv/rmng-sock` unix socket instead of TCP (same volume clones already mount).
3. **Reconciler cadence + cost** — pick the loop interval and confirm the per-clone idempotent checks (one
   `exec` read + one `buildx inspect`) are cheap enough to run fleet-wide on that cadence; align with `ssh.rs`.
4. **`daemon.json` write mechanism** — `upload_tar` (tar overlay of the single file) vs a here-doc via
   `exec_script`; pick the one consistent with how `ssh.rs`/provision already write per-clone files.
5. **`build_infra_enabled = false` semantics** — define precisely what the toggle does to (a) already-created
   infra containers (leave running / stop / remove) and (b) already-migrated clones (leave their
   `daemon.json`/builder in place / revert them). Default proposal: the reconciler simply stops acting (leaves
   both infra containers and clone config in place — a pure "stop managing," no destructive teardown); confirm
   in the plan.
