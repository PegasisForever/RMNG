# Control-server self-update & restart — design

Date: 2026-07-03
Status: approved design, ready for implementation planning

## Summary

Add in-product control over the control-server's own lifecycle, served by the
frontend the control-server itself hosts:

1. **Restart control-server** — a button that replaces the current
   "Run `docker restart rmng` to apply the changed port/socket/video settings"
   banner with an in-place restart.
2. **Update control-server** — a button that pulls the latest control-server
   image from Docker Hub and swaps the running container onto it.
3. **Version / update-available detection** — the server knows its own version
   and whether Hub has something newer, without pulling.
4. **Publish pipeline** — a script that builds and pushes the control-server
   image with version labels, so there is something newer to detect and pull.

The control-server already runs as a privileged container that drives the host
Docker daemon via bollard (`/var/run/docker.sock`), already pulls images from
Hub with streamed progress, already detects its own container id, and is
explicitly designed to be recreated (clones reach it by the `rmng-control` DNS
alias; boot runs `self_setup` + reconcile to re-adopt the running fleet). This
feature builds on that substrate.

### Out of scope (explicit decisions)

- **Automatic health-gated rollback** — no watching the new server for
  health and reverting. (A narrower create-error fallback *is* in scope; see
  §3.) Best-effort swap.
- **Digest pinning / signature verification** — the update target is the moving
  `:latest` tag; acceptable under the tailnet trust model. The API has no auth.

## Prerequisite / ordering note

The currently-running control-server predates this feature and has no
`start_update` code path. Therefore the **first** hop onto a feature-bearing
image must be the existing manual procedure
(`docker pull … && docker rm -f … && docker run …`). Every update after that is
in-product. This is documented in `docs/DEPLOY.md`.

---

## 1. Publish pipeline

**`scripts/publish-server.sh`** — mirrors `scripts/publish-template.sh`:

- Builds the root `Dockerfile` with build args:
  - `GIT_SHA=$(git rev-parse --short HEAD)`
  - `BUILD_DATE=$(date -u +%FT%TZ)`
- Tags `pegasis0/rmng:YYYYMMDD` (immutable dated) + `pegasis0/rmng:latest`
  (moving), pushes both.
- Repo overridable via first positional arg / env, same convention as the
  template script.
- Rollback = repoint the update reference at a prior dated tag.

**`Dockerfile`** (runtime stage) gains:

```dockerfile
ARG GIT_SHA
ARG BUILD_DATE
LABEL org.opencontainers.image.revision=$GIT_SHA \
      org.opencontainers.image.created=$BUILD_DATE \
      org.opencontainers.image.version=$GIT_SHA
```

A plain `docker build` with no build args → empty labels → UI shows a "dev
build" / unknown version, which is correct for local dev images.

---

## 2. Version / update-available detection

- **Current version:** the server inspects its own image's labels
  (`org.opencontainers.image.revision` + `.created`) via a daemon call it
  already can make (it resolves its own container + image today).
- **Available:** `DockerCtl::registry_digest(reference)` uses bollard's
  distribution query (`GET /distribution/{name}/json`) to fetch the remote
  manifest digest of the target ref **without pulling**, and compares it to the
  running image's `RepoDigest` for that repo. Different digest ⇒ update
  available.
  - Implementation note: confirm the exact bollard 0.19 method name for the
    distribution endpoint during implementation; fall back to a lightweight
    pull-to-compare only if the endpoint is unavailable.
- **Target config:** new `docker.serverImage` field (default
  `pegasis0/rmng:latest`) in `crates/wire/src/config.rs`, mirroring the existing
  `docker.templateReference`. **Not** a restart-required setting — it is read at
  check/update time, not wired at startup.
- **Cadence:** the frontend fetches status on mount + a manual **Check** button.
  No aggressive background polling (avoids Docker Hub rate limits).

---

## 3. Self-update swap mechanism

Chosen approach: **detached `self-upgrade` helper running the new image**
(the only approach that keeps all logic in Rust/bollard, needs no docker CLI or
shell reconstruction of run flags, and captures the run-spec from a live
self-inspect so it cannot drift).

### End-to-end sequence (happy path)

1. Operator clicks **Update** (enabled because detection reported "update
   available").
2. `POST /api/server/update` → `jobs::start_update` creates an
   `Operation { kind: Update }`, spawns `run_update`, returns the op.
3. `run_update`:
   1. **Pull** the target image (`pull_image`, streams progress → op, ~0–80%).
   2. **Capture** own run-spec via a full self-inspect
      (`inspect_container(self_id)`) → a `SelfSpec` struct (see below).
   3. **Persist handoff:** write `/data/update-handoff.json` (the `SelfSpec` +
      old container id/name + `old_image_id` + new image ref/digest + op id) and
      set a `pending_update` marker in `state.json` (target digest, op id,
      timestamp). Both live in the `/data` volume so they survive the swap.
   4. **Launch a detached helper** container *from the just-pulled image* with
      entrypoint `rmng-control-server self-upgrade /data/update-handoff.json`,
      mounting `docker.sock` + the data volume, `network: none`, named
      `rmng-self-upgrade`, **not** `rmng.managed`-labeled (kept out of managed
      sweeps, same reasoning as the lxcfs probe), pre-cleaned before launch.
   5. Op sits at ~85% "handing off to updater".
4. **Helper** (`self-upgrade` subcommand — compiled into the same binary, so the
   *new* version's swap code runs and proves the new image starts):
   1. Read `/data/update-handoff.json`.
   2. `stop_container(old)` (kills the old server) → `remove_container(old)`
      (frees the name + published ports 9000–9003).
   3. `create_container_from_spec(new_image, spec, name)` → `start_container(new)`.
   4. Success → best-effort `remove_container(self)`, exit 0.
   5. **Create-error fallback:** if create/start fails, recreate the container
      from `old_image_id` + the same spec and start it, so the host is never
      left with nothing running. Record the error in the handoff result. (This
      is a create-error safety net, **not** health-gated rollback — it does not
      watch whether the new server becomes healthy.)
5. **New server boots.** `update::reconcile_pending(app)` runs **immediately
   before** `jobs::fail_stale_ops` (ordering is load-bearing — otherwise
   `fail_stale_ops` would clobber the surviving update op as "interrupted"). It
   compares the new server's own running-image digest to the target and marks
   the surviving update op **Done** ("updated to `<ver>`") or **Error**, clears
   the marker, and force-removes any leftover handoff file / helper container.
   If the daemon is unreachable at boot, it completes optimistically ("digest
   unverified — daemon unreachable"), consistent with the best-effort posture.
6. **Frontend.** The `/events` SSE drops during the swap; the browser's native
   `EventSource` auto-reconnects once the new server is listening, receives a
   fresh `ControlState` with the completed op + new version, and shows
   "Updated to `<version>`."

### `SelfSpec` (capture → recreate)

Projected from `inspect_container(self)` into a serde struct written to the
handoff file (feeding an inspected `HostConfig` back into `create` is the
well-trodden bollard path; the only override is `image`):

- from `Config`: `hostname`, `env`, `labels`, `exposed_ports`, `stop_signal`,
  `stop_timeout` (**not** `image`).
- from `HostConfig`: `privileged`, `pid_mode` (`"host"`), `init`, `mounts` /
  `binds`, `port_bindings` (9000–9003), `restart_policy`, cpu/mem limits.
- from `NetworkSettings.networks`: networks + aliases (preserves
  `rmng-control`; `self_setup`'s re-attach at boot is a backstop).
- plus: `container_name` (captured from inspect, `/`-stripped — **no** hardcoded
  `rmng`), `old_image_id`, `new_image_ref` + resolved digest, `op_id`.

### Guards & housekeeping

- **One at a time:** `start_update` rejects if any `Operation` is `Running` (the
  swap kills the server, aborting every in-flight clone/pull/commit).
- **Helper is ephemeral infra:** deterministic name, not `rmng.managed`-labeled,
  `network: none`, mounts only `docker.sock` + the data volume, pre-cleaned
  before launch, self-removes at end, and reconcile force-removes it as a
  backstop.
- **Downtime:** a few seconds (old stop → new server listening on 9000). Running
  clones are untouched.

---

## 4. Restart control-server (in-place)

The `restart_required` settings
(`crates/control-server/src/config.rs::restart_required`) are all startup-wired
and re-read from `config.json` on boot: the four `listen` ports, `clone_socket`,
`docker.socket`, `static_dir`, `chroma`. So this is a plain **in-place
restart** — the programmatic twin of the `docker restart rmng` the banner tells
you to run — not a recreate.

- **Mechanism:** `POST /api/server/restart` → `DockerCtl::restart_self()` =
  bollard `restart_container(self_id)`. The daemon stops+starts the same
  container, which re-reads `config.json` on boot. The existing
  `--restart unless-stopped` policy is a natural backstop.
- **UI:** the banner at `frontend/app/components/SettingsPanel.tsx:393-395`
  becomes an inline **Restart control-server** button (with a confirm). Same
  "server goes away → `EventSource` auto-reconnects → back" UX as update.
- **Caveat (unchanged from today):** a plain restart does **not** change the
  container's *host-published* port mapping (fixed at `docker run`/compose
  time). Changing a `listen` port to something outside the published
  9000–9003 range still needs a host-level recreate. The button is exactly as
  capable as the `docker restart rmng` text it replaces.

---

## 5. Components changed / added

| Component | Role |
|---|---|
| `scripts/publish-server.sh` *(new)* | Build + label + dated/latest tag + push `pegasis0/rmng` |
| `Dockerfile` | `ARG GIT_SHA` / `ARG BUILD_DATE` → OCI `LABEL`s in the runtime stage |
| `crates/control-server/src/update.rs` *(new)* | `check_update()`, `SelfSpec` capture, helper launch, the `self-upgrade` subcommand entry, `reconcile_pending()` |
| `crates/control-server/src/docker.rs` | New primitives: full self-inspect → `SelfSpec`, generic `create_container_from_spec`, `registry_digest` (distribution query), `running_image_digest`, `restart_self` |
| `crates/control-server/src/jobs.rs` | `start_update` / `run_update` driving `Operation { kind: Update }` (mirrors `start_pull`) |
| `crates/control-server/src/main.rs` | Dispatch `argv[1] == "self-upgrade"` → helper path; else normal server + `reconcile_pending` before `fail_stale_ops` |
| `crates/control-server/src/web.rs` | Routes: `GET /api/server/version`, `POST /api/server/update`, `POST /api/server/restart` |
| `crates/wire/src/control.rs` | `OperationKind::Update`; `UpdateStatus` DTO |
| `crates/wire/src/config.rs` | `docker.serverImage` field (default `pegasis0/rmng:latest`) |
| `frontend/app/lib/api.ts` | `getUpdateStatus()`, `updateServer()`, `restartServer()` |
| `frontend/app/components/SettingsPanel.tsx` | New "Control-server" block (version, badge, Check/Update/Restart); banner → Restart button |
| `docs/DEPLOY.md` | Document first-update-is-manual + the new buttons |

`UpdateStatus` DTO fields: `current_version`, `current_revision`,
`current_digest`, `available: bool`, `remote_digest`, `last_checked`,
`reference`.

---

## 6. Edge cases

- **First update is manual** (running server predates the feature).
- **Downtime** of a few seconds during the swap; clones survive.
- **Create-error fallback** recreates the old container so the host is never
  bricked (distinct from health-gated rollback, which is out of scope).
- **Guard-all-ops:** update is blocked while any operation is running.
- **Daemon-down-at-boot:** reconcile completes the op optimistically
  (unverified).
- **Published-port caveat** for restart (host-level mapping unchanged).
- **No auth + privileged recreate:** accepted under the tailnet model; digest
  pinning declined.

---

## 7. Testing

- **Unit:**
  - `SelfSpec` projection (inspect → create) against a fixture
    `ContainerInspectResponse`.
  - Digest-compare logic (available vs up-to-date).
  - `UpdateStatus` projection.
  - `restart_required` matrix stays green (no behavior change).
- **Wire:** `cargo test -p wire` regenerates the TS types
  (`OperationKind`, `UpdateStatus`, config).
- **E2E on CT 106 (W6800)** — the only box the Docker path runs on:
  - Publish a v2 image with a bumped label to a test repo, click **Update**,
    verify the swap completes, running clones survive, the version flips, and the
    op reports Done.
  - **Restart:** change a `chroma` / `static_dir` setting and confirm it is
    applied after clicking the button.
- **Build/verify loop:**
  `cargo test -p wire && cargo build -p control-server && (cd frontend && bun run typecheck)`.
