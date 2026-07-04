# Detector-feedback SMB share — design

**Date:** 2026-07-04
**Status:** Approved (brainstorm), pending implementation plan
**Builds on:** [`2026-07-03-smb-clone-homes-design.md`](2026-07-03-smb-clone-homes-design.md) — the same
in-container `smbd` that serves the `clones` share.

## Summary

Add a second, code-defined SMB share — `[feedback]` — to the `smbd` the control-server already
runs (port 445), rooted at the control-server's `<data_dir>/detector-feedback` directory. This
**formalizes** a share that is currently hand-rolled/manual in deployment into `smb.rs`, exactly
the way `[clones]` is already defined and rendered in code. A client browsing
`smb://<host>/feedback` sees the detector-feedback records (JSON + screenshots) written by
`POST /api/detector-feedback`, read-write, so a human can inspect *and* curate (prune reviewed
records) them while tuning the detector.

## Requirements (locked during brainstorm)

| Decision | Choice |
|---|---|
| What | A second SMB share `[feedback]`, in the same `smbd`, alongside `[clones]` |
| Share root | `<data_dir>/detector-feedback` (the folder `save_detector_feedback` writes) |
| Access mode | **Read-write** — humans can read and delete/curate records over SMB |
| Created-file ownership | **root-owned** — matches what the API writer creates; no per-record ownership need |
| Auth | Same fixed built-in `rmng` credential + `valid users = rmng` as `[clones]` |
| On/off | **Always on**, no toggle/config surface; does **not** depend on `pid: host` |
| Port | **445** (unchanged) — same `smbd`, no new published port |
| Supervision | Unchanged — the existing `smb.rs` supervisor already covers both shares |

**Explicitly out of scope (YAGNI):** a config.json toggle, wizard/Settings UI, a separate
credential or port for feedback, read-only mode, per-host sub-shares.

## Why a dedicated share (not the whole data dir)

The share is scoped to `detector-feedback` **only** — deliberately not the whole `data_dir`.
`data_dir` also holds `state.json` and the **`claude-accounts.json` secret store**
(see `docs/PROTOCOL.md`, `data_dir` row). A blanket share of `data_dir` would expose those
credentials over SMB. A folder-scoped share is the safe boundary, and this constraint is the
reason the design must not generalize to `data_dir`.

Two rejected alternatives:

- **Fold into the `clones` share** (e.g. a `data/hosts/_feedback` symlink) — pollutes the
  clone-id namespace with a fake "clone" entry and drags the `wide links` / `follow symlinks`
  / `force user = root` config (which exists *only* for `/proc/<pid>/root` traversal) onto an
  unrelated plain directory. Conflates two things.
- **One broad `data_dir` share** — leaks the secret store, as above.

## Architecture

```
Finder / SMB client ──smb://<host>:445/feedback──▶ smbd (in control-server container)
                                                      │  share path = <data_dir>/detector-feedback
                                                      ▼
                                    data/detector-feedback/{<id>.json, <id>.jpg}
                                      (written by save_detector_feedback via
                                       POST /api/detector-feedback)
```

Unlike `[clones]`, the feedback share serves a **plain local directory** in the container
filesystem. There is no `/proc` symlink traversal, so it needs none of the ptrace/`wide links`
machinery and works regardless of whether the container has `pid: host`.

## Components

### `crates/control-server/src/smb.rs`

- **`render_smb_conf`** gains a second parameter — the feedback share root — and appends a
  `[feedback]` section after `[clones]`. Stays pure and unit-testable; only the two `path=`
  lines vary, every other line is literal.

  ```ini
  [feedback]
     path = <feedback_root>
     read only = no
     force user = root          # dir is control-server-owned (root); matches the API writer
     force group = rmng
     valid users = rmng
  ```

  No `wide links` / `follow symlinks` / `inherit owner` — those are `[clones]`-only, for
  `/proc` traversal. `force user = root` here is not for traversal but because the directory
  and its API-written files are root-owned; running the share as root gives read-write over
  them and keeps SMB-created files owned identically to API-created ones.

- **`run`** resolves both roots (hosts root as today, plus the feedback root), `create_dir_all`s
  **both** (the feedback dir may not exist until the first report arrives), then renders one
  `smb.conf` containing both sections. Account provisioning, the supervisor, backoff, port, and
  logging are unchanged and shared across both shares.

### `crates/control-server/src/files.rs`

- Add `pub const DETECTOR_FEEDBACK_DIR: &str = "detector-feedback";` and use it in
  `save_detector_feedback` (replacing the inline `"detector-feedback"` string) so the writer
  and the SMB share path single-source the folder name and can never drift — the same
  discipline `[clones]` uses via `homes::hosts_root`.

- `smb.rs` resolves the feedback root from the config `data_dir` + this constant (mirroring
  `absolute_hosts_root`), so config-driven `data_dir` changes move the share with the writer.

## Ownership / access

The feedback directory is created by the control-server, which runs as root in the container,
so the directory and its records are root-owned. `force user = root` on the share makes `smbd`
operate as root for this share, giving the authenticated `rmng` client read-write over those
root-owned records and creating any new files root-owned — identical to the API writer. No
`inherit owner` is needed because, unlike `[clones]`, there is no requirement that records be
owned by any particular clone user.

Trust posture is unchanged from `[clones]`: a single credential-gated `rmng` user, on a share
served as root, on a trusted private network.

## Error handling / edges

- **Feedback dir absent at startup** — `run` `create_dir_all`s it before rendering, so the
  share mounts and is simply empty until the first `POST /api/detector-feedback` lands.
- **Host port 445 already bound** — unchanged from the `clones` design; still the single
  published-port prerequisite. Adding this share publishes no new port.
- **smbd binary/config error** — surfaced via the existing `smb` tracing target; the supervisor
  retries with capped backoff. Unchanged.
- **Concurrent API write during an SMB browse** — records are whole-file writes (`<id>.json` /
  `<id>.jpg`) with random ids; a client deleting or reading one record does not interfere with
  the API creating a differently-id'd one. No locking added.

## Docs changes

- **`docs/DEPLOY.md`** — under the SMB / "browsing clone homes" section, add the `feedback`
  share (`smb://<host>/feedback`, same `rmng` credential) next to `clones`; note it exposes the
  detector-feedback records and is read-write.
- **`crates/control-server/src/smb.rs`** module doc + **`crates/control-server/README.md`** —
  update the "one read-write share" framing to "two shares: `clones` and `feedback`."
- **`crates/control-server/src/main.rs`** header comment (line ~5) — the "`clones` share on 445"
  note becomes "`clones` + `feedback` shares on 445."

## Testing

- **Unit (Rust) — `render_smb_conf`** — extend the existing assertions to cover the new
  `[feedback]` section and its load-bearing lines (`[feedback]`, `path = <feedback_root>`,
  `read only = no`, `force user = root`, `valid users = rmng`), and that the feedback `path`
  interpolates the passed root (parallel to the existing `[clones]` path test).
- **Integration / manual (on CT 106)** — `docker compose up --build`, generate a detector
  feedback record (or drop a file into `data/detector-feedback`), then from a client
  `smb://<host>/feedback` → confirm the records list, a record reads, and a record created **and
  deleted** over SMB behaves (read-write proof). This is the real gate; it can't be faked in a
  unit test.

## Open items carried into the plan

1. **Share name** — `feedback` (chosen). Confirm no collision with any existing manual share of
   the same name during rollout on deployments that currently hand-roll it.
2. **`render_smb_conf` signature** — two `&Path` args vs a small struct. Decide in the plan
   (two args is fine for two shares; revisit only if a third share appears).
