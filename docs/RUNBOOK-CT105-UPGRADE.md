# Runbook — upgrading CT 105 (`pega-rmng`) across the CLIProxyAPI revert

**Target:** control-server `9ec205a` → `8b49f7a` (current `main`).
**Scope:** operational sequence, pre-flight checks, verification gates, rollback.
**Status:** PLAN ONLY. Nothing in this document has been executed. The read-only
probes quoted below were run against the live boxes on 2026-07-29; the mutating
commands were **not**.

Conventions follow [`DEPLOY.md`](DEPLOY.md). Every step is tagged **REVERSIBLE** or
**IRREVERSIBLE**.

---

## 0. Baseline correction — read this first

The brief for this work said CT 105 runs **`2df6370`**. **That is wrong**, and the
error is worth understanding because it is a booby-trap that will fire again.

```console
$ ssh root@10.0.0.100 'pct exec 105 -- docker inspect rmng \
    --format "{{index .Config.Labels \"org.opencontainers.image.revision\"}}"'
2df6370                                    # <-- CONTAINER label: STALE, WRONG

$ ssh root@10.0.0.100 'pct exec 105 -- docker image inspect pegasis0/rmng:latest \
    --format "{{index .Config.Labels \"org.opencontainers.image.revision\"}}"'
9ec205a                                    # <-- IMAGE label: correct

$ ssh root@10.0.0.100 'pct exec 105 -- curl -s http://127.0.0.1:9000/api/server/version'
{"currentRevision":"9ec205a","currentCreated":"2026-07-27T23:38:04Z", ...}
```

Corroborated three ways: the server binary's mtime inside the container is
`2026-07-27 23:39:21` (matching the `9ec205a` image build, not a 2026-07-06 one);
`5b03dbb`'s commit message states *"Both run 9ec205a — the commit right before the
sidecar split"*, naming CT 105 explicitly; and the coordinator independently probed
`POST /api/groups` → 422 and `GET /api/groupproxy` → 200, routes that do not exist
at `2df6370`.

**Why the container label lies.** The in-product Update path
(`update::self_upgrade_main` → `docker::create_and_start_from_spec`,
[`crates/control-server/src/docker.rs:1459`](../crates/control-server/src/docker.rs))
copies `config.labels` verbatim from the *old* container's inspect onto the new
one:

```rust
let body = ContainerCreateBody {
    hostname: c.hostname.clone(),
    env: c.env.clone(),
    labels: c.labels.clone(),          // <-- carried from the OLD container
    ...
    image: Some(spec.new_image_ref.clone()),
```

So every self-update pins the labels from whenever the container was last created
*by hand*. CT 105 was hand-created on `2df6370`, then self-updated forward; the
labels never moved.

**Operational consequence for this runbook:**
> **Never read a revision off `docker inspect <container>`.** Use
> `docker image inspect` or `GET /api/server/version`. Any gate below that checks a
> revision uses one of those two. After the upgrade CT 105's container label will
> *still* say `2df6370` unless the container is created fresh — which Step 4.4
> deliberately does, so the label finally becomes truthful again.

**Revised migration span: `9ec205a..HEAD` = 26 commits**, not ~110. This is a much
smaller and better-understood hop than the brief assumed, and CT 106 has already
run 19 of those 26.

### What this changes from my earlier analysis

| Previously reported | Corrected |
|---|---|
| CT 105 on `2df6370`, ~110-commit hop | CT 105 on `9ec205a`, **26-commit hop** |
| CT 105 predates CLIProxyAPI | CT 105 is **mid-CLIProxyAPI, in-process proxy** (pre-sidecar-split) |
| Sidecar `rmng-cliproxy` must be removed | **No sidecar exists on CT 105** — verified `docker ps -a --filter name=rmng-cliproxy` is empty. The proxy runs *in-process*; it dies when the container is replaced. `DEPLOY.md`'s "Upgrading a fleet that ran the retired `rmng-cliproxy` sidecar" step 1 is a **no-op here**, and its stated ordering rationale (stop the sidecar before copying credentials so a background refresh can't invalidate a copy) **does not protect CT 105** — see §2.7. |

Everything else in my analysis stands.

---

## 1. Pre-flight evidence from CT 106

CT 106 (`haoran-rmng`) ran this exact migration on **2026-07-29 ~00:05–00:30 UTC**.
It is now on `0ec84a1` and is the single best piece of evidence available.

### 1.1 Was it clean? Eventually — but not on the first attempt

The timeline reconstructed from file mtimes and container creation times:

```console
$ ssh root@10.0.0.100 'pct exec 106 -- docker exec rmng stat -c "%y %n" \
    /data/data/.token-unmigration-done /data/config.json /data/data/codex-accounts.json'
2026-07-29 00:06:20.407332045 +0000 /data/data/.token-unmigration-done
2026-07-29 00:06:20.406332036 +0000 /data/config.json
2026-07-29 00:06:20.406332036 +0000 /data/data/codex-accounts.json

$ ssh root@10.0.0.100 'pct exec 106 -- docker inspect rmng --format "{{.Created}}"'
2026-07-29T00:30:41.476261898Z
```

Backup taken 00:05:40, migration fired 00:06:20, **but the container now running was
created 00:30:41** — 24 minutes and at least one recreate later. Three container
directories under `/var/lib/docker/containers/` carry a `2026-07-29 00:24` mtime.
That gap is the operator finding and fixing `0ec84a1`:

> **`0ec84a1` — "apply the wrapper env drop-in unconditionally, not on env change"**
> *"The drop-in from 47f3c3a was written inside the `if changed` branch of the
> /etc/environment sync — so it only ran when that file's content actually moved. On
> CT 106 the file was already correct from the previous migration, the gate never
> fired, and the drop-in was never written. The clones stayed on the baked container
> env and chat stayed broken."*

So: **the first CT 106 attempt left every clone's chat broken**, and the fix had to
be written, built, published and redeployed mid-migration. That is the honest shape
of this migration's risk. CT 105 gets `0ec84a1` from the start, so this specific
failure should not recur — but plan the window for "a fix may be needed", not
"twenty minutes and done".

### 1.2 What the operator had to do by hand

`/root` on CT 106 shows the manual work:

```console
$ ssh root@10.0.0.100 'pct exec 106 -- ls -la /root/'
-rw-r--r-- 1 root root    38 Jul 29 00:05 .rmng-last-backup
drwxr-xr-x 2 root root     3 Jul 29 00:05 rmng-preupgrade-20260729-000540
-rw-r--r-- 1 root root     0 Jul 29 00:33 '&1'          # <-- a fumbled redirect
```

The backup was a single `rmng-data.tar` (20 MB, 35 entries) covering `config.json`,
`data/state.json`, the whole `data/cliproxy/` tree, `data/cliproxy-instances.json`,
`data/claude-accounts.json` and `data/codex-accounts.json`. **That entry list is the
right one** — §3 reuses it. (The stray `'&1'` file is a shell typo, harmless, and a
reminder to quote redirects carefully when running these through `pct exec`.)

No evidence of manual re-login: `data/.token-unmigration-done` contains
`unmigrated` and all six Claude accounts survived (§1.3).

### 1.3 Accounts survived — verified

```console
$ ssh root@10.0.0.100 'pct exec 106 -- docker exec rmng grep -c "\"email\"" \
    /data/data/claude-accounts.json /data/data/codex-accounts.json'
6      # claude-accounts.json
1      # codex-accounts.json
```

Cross-checked against the pre-migration auth-dir, which held exactly 6 `claude-*`
and 1 `codex-*` file (plus 2 `antigravity-*`, deliberately dropped). **Zero
credential loss.** No re-login was required.

### 1.4 Clone-side convergence is fully clean

All 8 clones have the drop-in and the stamp:

```console
$ ssh root@10.0.0.100 'pct exec 106 -- sh -c "for c in haoran-dev-283 haoran-dev-212 \
    haoran-eval haoran-dev-227 haoran-dev-270 haoran-dev-260 haoran-dev-257 haoran-dev-258; do
      printf \"%-20s stamp=%s dropin=%s\n\" \$c \
        \"\$(docker exec \$c cat /etc/rmng/wrapper-env 2>/dev/null || echo MISSING)\" \
        \"\$(docker exec \$c test -f /home/rmng/.config/systemd/user/agent-wrapper.service.d/10-rmng-retired-env.conf \
             && echo PRESENT || echo MISSING)\"
    done"'
haoran-dev-283  stamp=v1 ANTHROPIC_BASE_URL,ANTHROPIC_AUTH_TOKEN,CLAUDE_CODE_ENABLE_GATEWAY_MODEL_DISCOVERY,ANTHROPIC_DEFAULT_OPUS_MODEL  dropin=PRESENT
... (all 8 identical) ...
```

`/etc/environment` on every clone is down to `RMNG_PROXY_KEY` + `ANTHROPIC_MODEL`
— the four retired keys are gone. `~/.codex/config.toml` is clean (no
`model_provider`, no `[model_providers.rmng]`) **and preserved the operator's
`model_reasoning_effort = "high"`**, exactly as `da3b087`/`0456b6b` intended.
Credentials are present and freshly rotated on all 8.

> Note the drop-in lives at **`/home/rmng/.config/systemd/user/`** (a *user* unit),
> not `/etc/systemd/system/`. Checking the system path returns "No such file" on a
> perfectly healthy clone. Use the path above in any gate.

### 1.5 THE UNFIXED PROBLEM — pool bindings did not survive on CT 106

**This is the single most valuable finding in this document.**

CT 106's post-migration `state.json` shows **not one clone bound to a pool on the
Claude side**:

```console
$ # from CT 106 state.json
haoran-dev-270  claudeSel=hl877505@gmail.com  claudeGroup=None  codexSel=auto
haoran-dev-227  claudeSel=hl877505@gmail.com  claudeGroup=None  codexSel=auto
haoran-dev-260  claudeSel=hl877505@gmail.com  claudeGroup=None  codexSel=auto
haoran-dev-258  claudeSel=hl877505@gmail.com  claudeGroup=None  codexSel=auto
haoran-dev-283  claudeSel=hl877505@gmail.com  claudeGroup=None  codexSel=group:Default
haoran-dev-257  claudeSel=hl877505@gmail.com  claudeGroup=None  codexSel=auto
haoran-dev-212  claudeSel=auto                claudeGroup=None  codexSel=auto
haoran-eval     claudeSel=auto                claudeGroup=None  codexSel=auto
```

The pre-migration backup proves all 8 *were* bound:

```console
$ ssh root@10.0.0.100 'pct exec 106 -- sh -c "cd /tmp && mkdir bkchk && \
    tar -xf /root/rmng-preupgrade-20260729-000540/rmng-data.tar -C bkchk data/state.json && \
    grep -oE \"\\\"group\\\": *\\\"[^\\\"]*\\\"\" bkchk/data/state.json | sort | uniq -c"'
      8 "group": "Default"
```

Expected post-migration (per `5b03dbb`'s `heal_clone_bindings`): all 8 at
`group:Default`. Actual: 6 pinned to a bare **email**, 2 at `auto`, and exactly one
clone at `group:Default` on the *Codex* side only.

**Interpretation — and I flag my uncertainty explicitly.** Two readings fit:

1. **The heal ran but its result was later overwritten.** `hl877505@gmail.com` is a
   real member of pool `Default`, and 6 clones converging on one email is the exact
   signature of the sticky rotator picking a winner. If the heal wrote
   `group:Default` and the rotator then rewrote `claude_selection` to the resolved
   email, the pool binding is gone as a *binding* even though behaviour is correct
   today.
2. **The heal never fired for Claude.** `heal_clone_bindings` skips any clone where
   `claude_selection.is_some() || codex_selection.is_some()`. On the *second*
   container start (00:30), the stamp already existed, so the migration returned at
   the gate — whatever the first run left is what stuck.

I could not distinguish these from the surviving evidence: **CT 106's `rmng`
container logs only go back to the 00:30 start, so the actual migration log lines
from 00:06 are gone.** The one line that would settle it —
`"repointed N clone(s) at their former pool and M at 'auto'"` — is not recoverable.

**Why this matters enormously more on CT 105.** `47f3c3a` says it plainly:

> *"Benign on CT 106 (one pool, so `auto` and `group:Default` draw from the same
> accounts) and wrong anywhere pools differ, which is precisely CT 105."*

CT 106 having one pool means **the CT 106 run did not, and could not, exercise the
pool-carrying logic in any way that would reveal a failure.** A green CT 106 is *not*
evidence that pool carrying works. On CT 105, a clone that lands on `auto` instead of
`group:Personal` will be handed a **Medi** account — the wrong customer's credentials
on the wrong project's clone. This is the highest-severity risk in the whole
migration and it is the one thing the prior production run gives no confidence about.

**Mandatory mitigation:** Gate 4.6 below captures the exact per-clone pool map
*before* the upgrade and diffs it after. Treat any clone landing on `auto` whose
pre-image was `Personal` or `Medi` as a **failed gate**, not a cosmetic nit.

---

## 2. Difference analysis — why CT 105 is harder

Every dimension, and whether CT 106's run actually exercised it.

| # | Dimension | CT 106 | CT 105 | Exercised by CT 106? |
|---|---|---|---|---|
| 2.1 | **Credential pools** | 1 (`Default`) | **3** (`Default`, `Medi`, `Personal`) | **NO — the critical gap** |
| 2.2 | Clones (running) | 8 | **33** | Partially — 4× scale |
| 2.3 | State entries | 8, 0 archived | **39, 6 archived** | NO — no archived rows |
| 2.4 | Presets | 1 | **4**, each naming a pool | NO |
| 2.5 | Claude accounts | 6 | **8** across 2 pools | NO — cross-pool untested |
| 2.6 | Codex accounts | 1, in 1 pool | **1 identical account in all 3 pools** | **NO — dedup untested** |
| 2.7 | Proxy topology | sidecar container | **in-process** | **NO — different shutdown semantics** |
| 2.8 | Clone `Config.Env` | all 8 baked | **1 of 33 baked** | Yes (over-exercised) |
| 2.9 | Codex `config.toml` | had dead block | **has dead block** | Yes |
| 2.10 | Router-key spelling | ? | **legacy `router_keys`** | Unknown |
| 2.11 | Media socket | healthy | **orphaned inode (live bug)** | NO |
| 2.12 | Load at cutover | idle | **load 15.3, live turns** | NO |
| 2.13 | `/data` volume | 51 MB | **164 MB** | Yes (scale only) |
| 2.14 | Image source | pulled `:latest` | pulls same `:latest` | — |

### 2.1 Three pools — the headline difference

```console
$ ssh root@10.0.0.100 'pct exec 105 -- docker exec rmng find /data/data/cliproxy -maxdepth 1 -type d'
/data/data/cliproxy/Personal
/data/data/cliproxy/Medi
/data/data/cliproxy/Default
```

Config still carries the old provider-agnostic shape (`groups`, no `cloneGroups`):

```json
"groups": [ {"name": "Medi"}, {"name": "Personal"} ]
```

Note `Default` exists on disk as an auth-dir but is **not** in `config.groups`.
`rebuild_groups` derives pools from the *directories*, so the post-migration config
will have a `Default` pool that the operator never configured. Not harmful, but
expect it and don't mistake it for corruption.

### 2.5 / 2.6 Cross-pool accounts — dedup is live here, and untested

The Codex account `hello@talktomedi.com` (account_id `30e29785-…`) is present in
**all three** pools:

```console
$ ssh root@10.0.0.100 'pct exec 105 -- docker exec rmng sh -c \
    "for f in /data/data/cliproxy/*/auth/*.json; do printf \"%s|\" \$f; \
     tr -d \"\n\" < \$f | grep -oE \"\\\"(type|account_id)\\\":\\\"[^\\\"]*\\\"\"; echo; done"'
/data/data/cliproxy/Default/auth/codex-hello@talktomedi.com.json|"account_id":"30e29785-…" "type":"codex"
/data/data/cliproxy/Medi/auth/codex-b53f5136-hello@talktomedi.com-pro.json|"account_id":"30e29785-…" "type":"codex"
/data/data/cliproxy/Personal/auth/codex-b53f5136-hello@talktomedi.com-pro.json|"account_id":"30e29785-…" "type":"codex"
```

`dedupe()` keys on `(kind, email)` and keeps the **first in sorted group order** —
`Default` sorts before `Medi` before `Personal`. So:

- The surviving Codex credential comes from **`Default/`**.
- `rebuild_groups` only sees the *surviving* record, so `codexGroups` will contain
  **`Default` only** — `Medi` and `Personal` will have **no Codex pool at all**.
- Consequence in `token_unmigrate.rs`: the preset-carrying loop sets
  `preset.codex_account` only `if cfg.codex_groups.iter().any(|g| &g.name == pool)`.
  For the `medi` preset (pool `Medi`) and the three `Personal` presets, that test
  **fails** → their `codexAccount` stays blank.
- Same for `heal_clone_bindings`: `pick(codex_pools)` finds no `Medi`/`Personal`
  Codex pool → every clone gets `codex_selection = "auto"`.

**This is expected, correct-by-design behaviour** (a single-use refresh token must
live in exactly one store), **not** a bug — but it means **Codex pool bindings will
legitimately be lost on CT 105** and the operator should not chase it as a failure.
Two `WARN` lines naming `codex hello@talktomedi.com` should appear in the boot log.
Their **absence** would be the real red flag.

Claude accounts do **not** overlap across `Medi` and `Personal` — 5 in `Medi`, 5 in
`Personal`, disjoint emails — so Claude pools should both rebuild intact. That is the
part Gate 4.6 must confirm.

### 2.7 In-process proxy — the safety ordering does NOT apply

`DEPLOY.md` describes removing the sidecar *first* so its background token refreshes
can't invalidate a credential the migration just copied. On CT 105 there is no
sidecar; the proxy is in-process. Verified:

```console
$ ssh root@10.0.0.100 'pct exec 105 -- docker ps -a --filter name=rmng-cliproxy'
(empty)
```

The practical difference: the refresher dies at the same instant the container is
removed, so there is no window where a rotation races the copy — **provided the old
container is fully gone before the new one boots.** The `docker rm -f` in Step 4.4
guarantees that. It does mean an in-flight refresh can be cut mid-rotation; a
single-use refresh token consumed but not persisted is a **permanently dead
account**. Mitigation: Step 4.2 quiesces first, and the backup in §3 is taken with
the server *stopped*, not live.

CT 105's log shows this is not theoretical — refreshes and 60-second inference calls
are constantly in flight:

```
[Personal] msg="200 | 58.417s | POST \"/v1/messages?beta=true\"" request_id=af4c6da6
DEBUG cliproxy: usage poll poked (account change / manual refresh)
```

### 2.8 Only ONE clone has baked env — and that inverts a CT 106 assumption

```console
$ # 33 running clones surveyed; only one has the retired vars in Config.Env
pega-rmng-development-2   BASE_URL=1 AUTH_TOKEN=1 PROXY_KEY=1 GATEWAY=1
(all 32 others)           BASE_URL=0 AUTH_TOKEN=0 PROXY_KEY=0 GATEWAY=0
```

But **all 33** have them in `/etc/environment`:

```console
$ ssh root@10.0.0.100 'pct exec 105 -- docker exec pega-dev-268 grep -E "ANTHROPIC|CLAUDE_CODE" /etc/environment'
ANTHROPIC_BASE_URL=<redacted>
CLAUDE_CODE_ENABLE_GATEWAY_MODEL_DISCOVERY=1
ANTHROPIC_AUTH_TOKEN=<redacted>
ANTHROPIC_MODEL=opus[1m]
```

CT 106's failure mode was "file already correct, gate never fired, drop-in skipped".
On CT 105 the file is **not** correct, so the env-sync gate *will* fire on all 33 —
meaning **all 33 wrappers restart**, where on CT 106 the restart was suppressed.
This is the opposite convergence shape from the one CT 106 exercised. `0ec84a1`
makes the drop-in unconditional so both paths are covered, but expect a fleet-wide
`agent-wrapper` restart within ~30 s of the new server booting, and 33 interrupted
turns rather than 8.

### 2.10 Legacy router-key spelling — CT 105 has it

```console
$ ssh root@10.0.0.100 'pct exec 105 -- docker exec rmng head -c 60 /data/data/cliproxy-instances.json'
{ "instances": { "Personal": { "port": 9102, "inbound_key": ...
$ # key map spelled snake_case:
"router_keys"
```

CT 106 was already migrated to `routerKeys`. `3e80459` handles both spellings, and
its message says the bug was *"caught by deploying to the test CT: all three clones
had their identity key silently re-minted on first boot"*. Since `3e80459` is in
HEAD this should be fine — but CT 105 is the deployment that actually **needs** the
fix, and no production box has exercised it. Gate 4.7 checks it explicitly.

### 2.11 Media socket is ALREADY broken on CT 105 — pre-existing, and this upgrade fixes it

The listener inode and the on-disk inode disagree:

```console
$ ssh root@10.0.0.100 'pct exec 105 -- sh -c "docker exec rmng cat /proc/net/unix | grep rmng-sock; \
    ls -lai /var/lib/docker/volumes/rmng-sock/_data/clones.sock"'
… 1015894425 /srv/rmng-sock/clones.sock       # server is accept()ing on this inode
1368574 srwxrwxrwx 1 root root 0 Jul 28 23:30 clones.sock   # this is what's on disk
```

Exactly the fault `a532bb5` describes ("*This took out video for the CT 105 fleet…
the listener held inode 1015894425 while the file on disk was 1368574*"). Currently
**1** clone is connected. Any clone that restarts today can never reconnect its video.

Two operational consequences:
- **This is an argument for doing the upgrade**, not against it — `a532bb5` is the
  fix, and recreating the container re-binds the socket cleanly.
- **`/srv/rmng-sock` on CT 105 is polluted with 28 GB of unrelated directories**
  (`fused-target`, `gate-target`, `gatetmp`, owned by uid 1000). Some clone has been
  using the shared socket volume as scratch space. Do **not** back that volume up
  (§3 excludes it) and do not be alarmed by its size.

### 2.12 Load — schedule this deliberately

```console
$ ssh root@10.0.0.100 'pct exec 105 -- uptime'
 15:14:39 up 8 days,  1:41,  load average: 15.26, 14.02, 15.08
```

32 cores, load 15, with 20 `w-*` worker clones plus a live `pega-command-center`
session mid-turn. CT 106 was quiet. **Pick a genuinely idle window** — this is the
difference between "8 turns interrupted" and "33 turns interrupted, several of them
long-running autonomous loops mid-task".

---

## 3. Backup procedure

**Tag: REVERSIBLE (read-only).** Run all of §3 before touching anything.

`/data` is the docker **volume** `rmng-data`, so file copies need `docker exec` or a
volume mount. Two independent copies are taken, because they fail differently.

### 3.0 Set the working variables

```sh
ssh root@10.0.0.100
CT=105
STAMP=$(date -u +%Y%m%d-%H%M%S)
BK=/root/rmng-preupgrade-$STAMP
pct exec $CT -- mkdir -p $BK
```

> All commands below assume you are on the Proxmox host and prefix with
> `pct exec $CT --`. If you prefer to work inside the CT, `pct enter 105` and drop
> the prefix. **Quote redirects carefully** — CT 106 has a stray file literally named
> `&1` from a fumbled `2>&1` through `pct exec`.

### 3.1 Copy A — tar the live volume from inside the container

Mirrors what the CT 106 operator did, whose entry list proved sufficient.

```sh
pct exec $CT -- sh -c "docker exec rmng tar -cf - -C /data \
  config.json \
  data/state.json \
  data/claude-accounts.json \
  data/codex-accounts.json \
  data/cliproxy-instances.json \
  data/clone-tokens.json \
  data/cliproxy \
  data/ssh \
  data/notes \
  data/chats \
  > $BK/rmng-data.tar"
```

**Validated read-only:** `docker exec rmng tar -cf - -C /data . | wc -c` returns
`171878400` on the live box, so `tar` and `gzip` are present in the image and the
stream works. The explicit entry list above is ~110 MB (`data/cliproxy` alone is
106 MB); it deliberately **excludes** `data/detector-feedback` (21 MB, no consumer
found anywhere in `crates/`) and `data/uploads`/`data/hosts` (regenerated).

> Do **not** back up the `rmng-sock` volume. It is 28 GB of unrelated scratch on
> CT 105 (§2.11) and contains nothing recoverable.

### 3.2 Copy B — a helper container against the stopped volume

Copy A reads a **live** volume: `state.json` is rewritten by the server constantly
(mtime moved during my probing), so Copy A can catch a torn file. Copy B is taken in
Step 4.3 *after* the server is stopped and is the one rollback actually restores
from.

```sh
# RUN THIS IN STEP 4.3, NOT NOW — server must be stopped first.
pct exec $CT -- docker run --rm -v rmng-data:/src:ro -v $BK:/dst alpine \
  tar -cf /dst/rmng-data-quiesced.tar -C /src .
```

### 3.3 Container run-spec — read off the LIVE container, never from docs

**This is the authoritative recreate spec.** Captured 2026-07-29:

```sh
pct exec $CT -- docker inspect rmng > $BK/rmng-container-inspect.json
```

Live values (verified, do **not** copy from `DEPLOY.md` — they differ):

```
PRIV=true   INIT=<nil>   PID=host   RESTART=unless-stopped   NET=bridge
IMAGE=pegasis0/rmng:latest
BINDS=["/var/run/docker.sock:/var/run/docker.sock","rmng-data:/data","rmng-sock:/srv/rmng-sock"]
PORTS= 2222 445 9000 9001 9002 9003 9005
ENV=["RUST_LOG=info,tower_http=warn,clip=debug", PATH…, DEBIAN_FRONTEND=noninteractive]
DEVICES=[]
```

Two deviations from `DEPLOY.md` that matter:

1. **`INIT` is `<nil>`, not `true`.** `DEPLOY.md` documents `--init`. CT 105 does not
   have it (CT 106 does). Preserving `<nil>` means **omitting `--init`**. Adding it
   would be a behaviour change during an upgrade — don't. If you want `--init`, do it
   as a separate change afterwards.
2. **Ports 9002 and 9003 are published on CT 105 and are not in `DEPLOY.md`'s list.**
   CT 106 publishes only 2222/445/9000/9001/9005. 9002 is `AGENT_CONTROL_MCP_URL`,
   which every CT 105 clone has baked into its `Config.Env`
   (`AGENT_CONTROL_MCP_URL=http://rmng-control:9002`). **Keep both published.**
   Dropping them is not covered by any gate below and would break clone→server MCP
   for the whole fleet.

### 3.4 Record the pre-image of everything the gates check

```sh
pct exec $CT -- sh -c "docker exec rmng cat /data/data/state.json"  > /tmp/105-state-before.json
pct exec $CT -- sh -c "docker exec rmng cat /data/config.json"      > /tmp/105-config-before.json
pct exec $CT -- docker image inspect pegasis0/rmng:latest \
  --format '{{.Id}} {{index .Config.Labels "org.opencontainers.image.revision"}}' \
  > $BK/old-image-id.txt
pct exec $CT -- docker ps --format '{{.Names}}' | grep -v '^rmng' > $BK/clones-running.txt
```

**Capture the pool map — this is Gate 4.6's baseline and it is the single most
important artifact:**

```sh
pct exec $CT -- sh -c "docker exec rmng grep -oE '\"id\": \"[^\"]*\"|\"group\": \"[^\"]*\"' \
  /data/data/state.json" > $BK/pool-map-before.txt
```

Expected content (39 rows): 38 × `Personal`, 1 × `Medi` (`pega-dev-207`).

### 3.5 Pin the old image by digest so `:latest` can't strand you

**Critical, and easy to miss.** CT 105's container references the *moving* tag
`pegasis0/rmng:latest`. Docker Hub's `:latest` is currently `0ec84a1` and Step 4.1
will move it to HEAD. Once you `docker pull`, the local `:latest` tag **detaches from
the old image** and rollback by tag becomes impossible.

```sh
OLD_ID=$(pct exec $CT -- docker image inspect pegasis0/rmng:latest --format '{{.Id}}')
pct exec $CT -- docker tag $OLD_ID rmng:rollback-9ec205a
pct exec $CT -- docker image inspect rmng:rollback-9ec205a \
  --format '{{index .Config.Labels "org.opencontainers.image.revision"}}'
```

**PASS: prints `9ec205a`.** If it prints anything else, stop — you have tagged the
wrong image and rollback is not armed.

### 3.6 Verify the backup is good

```sh
# 1. tar is readable end-to-end and has the load-bearing entries
pct exec $CT -- tar -tf $BK/rmng-data.tar | grep -cE \
  '^(config.json|data/state.json|data/claude-accounts.json|data/codex-accounts.json|data/cliproxy-instances.json)$'
# PASS: 5

# 2. every auth file made it (19 expected on CT 105)
pct exec $CT -- tar -tf $BK/rmng-data.tar | grep -c 'data/cliproxy/.*/auth/.*\.json'
# PASS: 19

# 3. the extracted state.json parses and has 39 hosts
pct exec $CT -- sh -c "cd /tmp && rm -rf bkverify && mkdir bkverify && \
  tar -xf $BK/rmng-data.tar -C bkverify data/state.json config.json && \
  grep -c '\"id\":' bkverify/data/state.json"
# PASS: >= 39

# 4. config.json in the backup still has the OLD schema (proves it predates migration)
pct exec $CT -- grep -c '"groups"' /tmp/bkverify/config.json
# PASS: 1   (and "cloneGroups" must be ABSENT)
pct exec $CT -- grep -c '"cloneGroups"' /tmp/bkverify/config.json
# PASS: 0

pct exec $CT -- rm -rf /tmp/bkverify
```

A backup that fails any of these is not a backup. **Do not proceed past §3 without
all four passing and §3.5 printing `9ec205a`.**

---

## 4. The upgrade sequence

### 4.1 Get the image onto the box — **side-load, do not publish**

**Tag: REVERSIBLE.**

`scripts/publish-server.sh` pushes `pegasis0/rmng:latest`, and **both CT 105 and
CT 106 pull that exact reference**:

```console
$ pct exec 105 -- docker inspect rmng --format '{{.Config.Image}}'   → pegasis0/rmng:latest
$ pct exec 106 -- docker inspect rmng --format '{{.Config.Image}}'   → pegasis0/rmng
$ # and CT 106's serverImage config:
  "serverImage": "pegasis0/rmng:latest"
```

CT 106's `/api/server/version` currently reports `available:false` because its digest
matches `:latest`. **Publishing HEAD flips CT 106 to `available:true` and arms its
in-product Update button** — one operator click and a second production box hops
`0ec84a1 → 8b49f7a` with no backup and no plan. That is an unacceptable coupling for
a change of this size.

**Recommendation: side-load.** Build on a workstation, ship over SSH, tag locally
with an **immutable** name:

```sh
# on the build machine, at 8b49f7a:
docker build --build-arg GIT_SHA=8b49f7a \
             --build-arg BUILD_DATE=$(date -u +%Y-%m-%dT%H:%M:%SZ) \
             -t rmng:8b49f7a .
docker save rmng:8b49f7a | ssh root@10.0.0.100 'pct exec 105 -- docker load'
```

> **Verify this yourself before running.** I have not exercised `docker save … |
> ssh … pct exec … docker load` on this Proxmox host. `pct exec` stdin forwarding
> through a pipe is the part I am least sure of. If it misbehaves, the safe fallback
> is `docker save … | ssh root@10.0.0.100 'cat > /tmp/rmng-8b49f7a.tar'` then
> `pct push 105 /tmp/rmng-8b49f7a.tar /tmp/rmng.tar` and
> `pct exec 105 -- docker load -i /tmp/rmng.tar`. Note CT 120 already runs a
> locally-built `rmng:3a7cb70`, so local-tag deployment is an established pattern
> on this fleet.

Then repoint the config at the local tag **before** the swap, so the server's own
update machinery doesn't later pull `:latest` out from under you:

```sh
pct exec 105 -- curl -s -X PUT -H 'content-type: application/json' \
  -d '{"docker":{"serverImage":"rmng:8b49f7a"}}' http://127.0.0.1:9000/api/config
```

**Gate 4.1 — PASS:**
```sh
pct exec 105 -- docker image inspect rmng:8b49f7a \
  --format '{{index .Config.Labels "org.opencontainers.image.revision"}}'
# PASS: 8b49f7a
```

> **If you publish instead**, accept and plan the CT 106 coupling: either upgrade
> CT 106 first (it is smaller, already migrated, and a 7-commit hop `0ec84a1..HEAD`
> that includes the media-socket fix it will eventually want), or tell CT 106's
> operator not to press Update. Do not leave it to chance.
>
> Note also that `3a7cb70` on CT 120 is **not an ancestor of HEAD**
> (`git merge-base --is-ancestor 3a7cb70 HEAD` → false) — it is a pre-rebase variant
> missing `a532bb5`, differing in `docker.rs` + `media/src/sock.rs`. **CT 120 has not
> tested the media-socket fix.** Do not treat CT 120 as having validated `8b49f7a`.

### 4.2 Quiesce — **the most under-rated step**

**Tag: REVERSIBLE.**

CT 105 is at load 15 with 60-second inference calls in flight (§2.12). Before the
swap:

1. Announce the window. Every in-flight agent turn across 33 clones will be
   interrupted (§7).
2. Let long-running autonomous loops reach a checkpoint. There is no API to drain
   them; this is a human judgement call.
3. Confirm no server-side operation is running:
   ```sh
   pct exec 105 -- sh -c "docker exec rmng grep -o '\"operations\": *\[[^]]*\]' /data/data/state.json | head -c 200"
   # PASS: "operations": []   (a running pull/commit must finish first)
   ```
   Verified `[]` at time of writing.

### 4.3 Stop the server and take the quiesced backup

**Tag: REVERSIBLE.** Downtime starts here.

```sh
pct exec 105 -- docker stop rmng
pct exec 105 -- docker run --rm -v rmng-data:/src:ro -v $BK:/dst alpine \
  tar -cf /dst/rmng-data-quiesced.tar -C /src .
pct exec 105 -- tar -tf $BK/rmng-data-quiesced.tar | grep -c 'auth/.*\.json'
# PASS: 19
```

**Gate 4.3 — PASS:** `docker ps --filter name=rmng` shows `rmng` gone; the quiesced
tar exists and has 19 auth files. Clones keep running and keep serving — they are
siblings, not children.

**Rollback from here is trivial:** `docker start rmng`.

### 4.4 Swap the container

**Tag: IRREVERSIBLE — this is the point of no return. See §6.**

Recreate from the run-spec captured in §3.3 — **read off the live container, not
from `DEPLOY.md`**, with `--init` omitted and 9002/9003 published:

```sh
pct exec 105 -- docker rm -f rmng

pct exec 105 -- docker run -d --name rmng \
  --privileged --pid host --restart unless-stopped \
  -v /var/run/docker.sock:/var/run/docker.sock \
  -v rmng-data:/data \
  -v rmng-sock:/srv/rmng-sock \
  -p 9000:9000 -p 9001:9001 -p 9002:9002 -p 9003:9003 -p 9005:9005 \
  -p 445:445 -p 2222:2222 \
  -e RUST_LOG=info,tower_http=warn,clip=debug \
  rmng:8b49f7a
```

**Why this order.** `rm -f` before `run` because the name and all seven published
ports must be free. `rm -f` also guarantees the in-process CLIProxyAPI refresher is
dead before the new server reads the credentials it is about to take ownership of
(§2.7) — the same guarantee `DEPLOY.md` gets from removing the sidecar, obtained
differently.

**Why this is the irreversible step.** The instant the new server boots it runs
`unmigrate_group_proxy_tokens`, which writes `config.json`, both account stores, and
`state.json` in the **new** schema, then writes the `.token-unmigration-done` stamp.
The stamp makes the migration one-shot: it will never re-run, so a bad result cannot
be re-derived by restarting. Recovery from that point is restore-from-backup only.

### 4.5 Capture the boot log immediately — it is the only record

**Tag: REVERSIBLE (read-only), but time-critical.**

CT 106's migration log lines are **gone** (§1.5) because the container was recreated
before anyone saved them. Do not repeat that mistake:

```sh
sleep 45
pct exec 105 -- docker logs rmng > $BK/boot-after-upgrade.log 2>&1
pct exec 105 -- grep -iE 'token_unmigrate|repointed|carried|Antigravity|more than one group' \
  $BK/boot-after-upgrade.log
```

**Gate 4.5 — PASS requires all of:**

| Expected line | Why |
|---|---|
| `repointed N clone(s) at their former pool and M at 'auto'` | **N should be ≈39, M ≈0.** N=0/M=39 is the `47f3c3a` regression recurring — **FAIL, roll back.** |
| `carried K preset pool default(s) across` | K ≥ 4 (4 presets × Claude; Codex won't carry, §2.6) |
| `codex account hello@talktomedi.com appears in more than one group` ×2 | Confirms dedup ran (§2.6). **Absence = FAIL** |
| `N Antigravity (Gemini) account(s) found but NOT migrated` | N=6 on CT 105. Informational |
| No `ERROR` from `token_unmigrate` | A write failure leaves the stamp absent and retries next boot |

### 4.6 GATE — pool bindings (the one CT 106 could not validate)

**Tag: REVERSIBLE (read-only). This is the gate that matters most.**

```sh
pct exec 105 -- sh -c "docker exec rmng cat /data/data/state.json" > /tmp/105-state-after.json

python3 - <<'EOF'
import json
b=json.load(open('/tmp/105-state-before.json'))
a=json.load(open('/tmp/105-state-after.json'))
was={h['id']:h.get('group','') for h in b['hosts']}
bad=[]
for h in a['hosts']:
    old, sel = was.get(h['id'],''), h.get('claudeSelection')
    if old and sel != f'group:{old}':
        bad.append((h['id'], old, sel))
print(f"{len(a['hosts'])} hosts; {len(bad)} MISMATCHED")
for r in bad: print("  MISMATCH", r)
EOF
```

**PASS: `0 MISMATCHED`.** Every clone that was `Personal` is now
`claudeSelection: "group:Personal"`; `pega-dev-207` is `group:Medi`.

**FAIL — roll back — if any clone that was `Personal` or `Medi` shows `auto`.** That
means it will be handed an account from the wrong pool: a Medi credential on a
Personal clone, or vice versa. This is exactly the failure `47f3c3a` describes and
exactly what CT 106's single pool made invisible.

Codex selections landing on `auto` are **expected** (§2.6) and are not a failure.

Also confirm the pools rebuilt:

```sh
pct exec 105 -- sh -c "docker exec rmng grep -oE '\"(cloneGroups|codexGroups)\"' /data/config.json"
# PASS: both present
```
Expect `cloneGroups` = `Medi` + `Personal` (+ possibly `Default`), `codexGroups` =
`Default` only.

### 4.7 GATE — accounts present and non-empty

```sh
pct exec 105 -- sh -c "docker exec rmng grep -c '\"email\"' \
  /data/data/claude-accounts.json /data/data/codex-accounts.json"
```

**PASS: Claude ≥ 8, Codex = 1.** (Pre-migration auth-dirs hold 10 `claude-*` files
across 2 pools with disjoint emails, so 8–10 is the expected band; **0 or 1 is a
catastrophic FAIL** — roll back immediately, do not let a refresh persist an empty
store over the recovered one.)

Confirm no identity re-mint (§2.10):

```sh
pct exec 105 -- sh -c "docker exec rmng grep -c routerKeys /data/data/cliproxy-instances.json"
pct exec 105 -- docker inspect pega-dev-268 --format '{{json .Config.Env}}' | grep -o 'RMNG_PROXY_KEY=.\{0,12\}'
```
**PASS:** the key prefix still matches `$BK/rmng-container-inspect.json`-era values
for at least two clones. A changed key means `3e80459` failed and every clone lost
its identity — sub-clone detection and clone↔clone SSH break silently.

### 4.8 GATE — container health and API

```sh
pct exec 105 -- docker ps --filter name=rmng --format '{{.Names}} {{.Status}}'
# PASS: "rmng Up …", no restart loop

pct exec 105 -- curl -s http://127.0.0.1:9000/api/server/version
# PASS: {"currentRevision":"8b49f7a", ...}
#   NOTE: `docker inspect rmng` will NOW also read 8b49f7a, because §4.4 created the
#   container fresh rather than self-updating. This is the one moment the container
#   label is trustworthy — see §0.

pct exec 105 -- curl -s -o /dev/null -w '%{http_code}\n' http://127.0.0.1:9000/api/hosts
# PASS: 200

pct exec 105 -- docker ps --format '{{.Names}}' | grep -v '^rmng' | wc -l
# PASS: 33  (matches $BK/clones-running.txt — the swap must not have touched clones)
```

Media socket (the §2.11 pre-existing fault, now expected fixed):
```sh
pct exec 105 -- sh -c "docker exec rmng cat /proc/net/unix | grep rmng-sock; \
  ls -i /var/lib/docker/volumes/rmng-sock/_data/clones.sock"
# PASS: the listener inode and the on-disk inode MATCH.
```

### 4.9 GATE — clone convergence (allow ~60 s, two reconcile passes)

```sh
pct exec 105 -- sh -c 'ok=0; bad=""; for c in $(docker ps --format "{{.Names}}" | grep -v "^rmng"); do
  env_clean=$(docker exec $c grep -cE "ANTHROPIC_BASE_URL|ANTHROPIC_AUTH_TOKEN|GATEWAY_MODEL_DISCOVERY" /etc/environment 2>/dev/null || echo 9)
  dropin=$(docker exec $c test -f /home/rmng/.config/systemd/user/agent-wrapper.service.d/10-rmng-retired-env.conf && echo 1 || echo 0)
  cred=$(docker exec $c test -s /home/rmng/.claude/.credentials.json && echo 1 || echo 0)
  toml=$(docker exec $c grep -cE "^model_provider|model_providers.rmng" /home/rmng/.codex/config.toml 2>/dev/null || echo 0)
  if [ "$env_clean" = "0" ] && [ "$dropin" = "1" ] && [ "$cred" = "1" ] && [ "$toml" = "0" ]; then
    ok=$((ok+1)); else bad="$bad $c"; fi
done; echo "converged=$ok"; echo "NOT converged:$bad"'
```

**PASS: `converged=33`, `NOT converged:` empty.**

Each sub-check maps to a specific fix commit — if one lags, you know which:
`/etc/environment` clean → `c3fbf5a`; drop-in present → `47f3c3a`+`0ec84a1`;
credentials present → `bb2042b` (the reconciler used to **delete** them ~30 s after
they were written, so re-run this gate at T+2 min before declaring pass); no
`model_provider` in `config.toml` → `da3b087`.

Also confirm operator settings survived (`0456b6b`) — on CT 106
`model_reasoning_effort = "high"` was correctly preserved:
```sh
pct exec 105 -- docker exec pega-dev-268 cat /home/rmng/.codex/config.toml
# PASS: managed [mcp_servers.*] tables present; no model_provider/[model_providers.rmng];
#       any operator lines still there.
```

### 4.10 GATE — an agent turn actually completes

Everything above can pass while inference is still broken — that is precisely what
happened on the first CT 106 attempt (`API Error: 405`, `0ec84a1`). **This gate is
the one that proves the migration worked.** Nothing else substitutes for it.

```sh
# Inside a clone, bypassing any stale shell env, exactly as 47f3c3a diagnosed it:
pct exec 105 -- docker exec -u rmng pega-dev-268 \
  env -u ANTHROPIC_BASE_URL -u ANTHROPIC_AUTH_TOKEN \
  claude -p 'reply with the single word: ok'
# PASS: "ok". A 405/401/"not logged in" is a FAIL.
```

Then the same **through the wrapper**, which is what actually broke on CT 106 (the
shell worked while chat did not — the split that "wastes an afternoon"):

```
Send a one-line chat message from the RMNG dashboard to a clone and confirm the
reply arrives.
```
**PASS: the reply arrives.** Do this on **one clone per pool** — a `Personal` clone
and `pega-dev-207` (`Medi`) — so both pools' credentials are proven, which CT 106
structurally could not do.

### 4.11 GATE — the new features

```sh
pct exec 105 -- docker logs rmng 2>&1 | grep -i agentlog
# PASS: "agent-log scanner started (tokens + activity, every 15s)"
#   (verified present on CT 120 at 3a7cb70)

sleep 60
pct exec 105 -- sh -c "docker exec rmng grep -o 'cloneTokens.\{0,300\}' /data/data/state.json"
# PASS: cloneTokens is a NON-EMPTY map with per-clone inputTokens/outputTokens.
#   An empty {} after several minutes of fleet activity means the scanner cannot
#   reach the logs -- check data/hosts/<id> symlinks (requires --pid host, which
#   the 4.4 spec preserves).
```

Activity detection: a clone visibly mid-turn should show `working` in the sidebar.
`79afa38` derives this from the agents' own JSONL logs rather than the proxy, so it
should now light up even for a hand-run `claude` inside a clone — worth spot-checking,
since it is a genuinely new capability rather than a restored one.

---

## 5. Verification gates — summary

| Gate | Command | PASS | On FAIL |
|---|---|---|---|
| 3.5 | `docker image inspect rmng:rollback-9ec205a` | `9ec205a` | **Stop.** Rollback not armed |
| 3.6 | 4 backup checks | all 4 | **Stop.** Fix backup |
| 4.1 | `docker image inspect rmng:8b49f7a` | `8b49f7a` | Re-transfer image |
| 4.2 | `operations` in state.json | `[]` | Wait |
| 4.3 | quiesced tar, 19 auth files | 19 | `docker start rmng`, retry |
| 4.5 | boot log `repointed N…M` | N≈39, M≈0 | **Roll back** (§6.3) |
| 4.6 | pool-binding diff | `0 MISMATCHED` | **Roll back** (§6.3) |
| 4.7 | account counts | Claude ≥8, Codex 1 | **Roll back immediately** |
| 4.8 | container + API + 33 clones | Up / 8b49f7a / 200 / 33 | §6.2 or §6.3 |
| 4.9 | clone convergence | `converged=33` | Wait 2 passes, then investigate |
| 4.10 | agent turn, **both pools** | reply arrives | **Roll back** (§6.3) |
| 4.11 | agentlog + cloneTokens | scanner up, map non-empty | Non-blocking |

---

## 6. Rollback

### 6.1 Before Step 4.4 — free

Nothing has been mutated. `docker start rmng` (if 4.3 ran) and you are back on
`9ec205a`. **Tag: fully REVERSIBLE.**

### 6.2 After 4.4, container fails to start / API never answers

The new container never got far enough to migrate. Verify the stamp is absent:

```sh
pct exec 105 -- docker run --rm -v rmng-data:/d:ro alpine \
  ls -la /d/data/.token-unmigration-done
# "No such file" => no migration ran => a clean image-only rollback is safe.
```

```sh
pct exec 105 -- docker rm -f rmng
# re-run the 4.4 `docker run` with `rmng:rollback-9ec205a` as the final argument
```

**Tag: REVERSIBLE** *if and only if* the stamp is absent. If the stamp exists, the
data has been rewritten and you must use §6.3 — booting `9ec205a` against
new-schema data would have it read a `config.json` with no `groups` key and a
`state.json` with no `group` fields, stranding all 39 clones.

### 6.3 After 4.4, migration ran but a gate failed — full restore

**Tag: IRREVERSIBLE forward; this is the only recovery.**

```sh
pct exec 105 -- docker rm -f rmng

# Restore the quiesced volume snapshot wholesale.
pct exec 105 -- docker run --rm -v rmng-data:/dst -v $BK:/src alpine sh -c \
  'rm -rf /dst/* /dst/.token-unmigration-done && tar -xf /src/rmng-data-quiesced.tar -C /dst'

# Verify the OLD schema is back BEFORE starting anything.
pct exec 105 -- docker run --rm -v rmng-data:/d:ro alpine sh -c \
  'grep -c "\"groups\"" /d/config.json; grep -c cloneGroups /d/config.json; \
   ls /d/data/.token-unmigration-done 2>&1'
# PASS: 1 / 0 / "No such file"

# Recreate on the pinned old image.
pct exec 105 -- docker run -d --name rmng \
  --privileged --pid host --restart unless-stopped \
  -v /var/run/docker.sock:/var/run/docker.sock \
  -v rmng-data:/data -v rmng-sock:/srv/rmng-sock \
  -p 9000:9000 -p 9001:9001 -p 9002:9002 -p 9003:9003 -p 9005:9005 \
  -p 445:445 -p 2222:2222 \
  -e RUST_LOG=info,tower_http=warn,clip=debug \
  rmng:rollback-9ec205a
```

**Deleting `.token-unmigration-done` is mandatory.** Leaving it means a future
retry silently skips the migration and every clone loses its binding with nothing
logged.

**Then reconcile the clones back.** The 33 clones have by now had
`/etc/environment` rewritten and the drop-in installed. `9ec205a` will rewrite
`/etc/environment` back on its next pass, **but it knows nothing about the drop-in
and will not remove it** — and the drop-in sets `Environment=ANTHROPIC_BASE_URL=`
at the unit level, which *overrides* the restored value. **Rolled-back clones will
have a working shell and a broken wrapper**: the mirror image of the CT 106 failure.

Remove it by hand on every clone:

```sh
pct exec 105 -- sh -c 'for c in $(docker ps --format "{{.Names}}" | grep -v "^rmng"); do
  docker exec $c rm -f /home/rmng/.config/systemd/user/agent-wrapper.service.d/10-rmng-retired-env.conf /etc/rmng/wrapper-env
  docker exec $c runuser -u rmng -- env XDG_RUNTIME_DIR=/run/user/1000 systemctl --user daemon-reload
  docker exec $c runuser -u rmng -- env XDG_RUNTIME_DIR=/run/user/1000 systemctl --user restart agent-wrapper.service
done'
```

> **Verify this yourself before running.** I derived it from
> `agent_wrapper_env_dropin_script()` in `clone_reconcile.rs` and confirmed the
> paths on CT 106, but I have **not** executed the removal anywhere. Test it on one
> clone and confirm chat works before looping the fleet.

Also expect `~/.codex/config.toml` to be missing its `[model_providers.rmng]` block;
`9ec205a`'s reconciler overwrites `config.toml` wholesale, so it should restore
itself within ~30 s. **This will also destroy any operator hand-edits** (that
overwrite is the bug `0456b6b` fixed) — `model_reasoning_effort` and similar will be
lost on rollback and must be re-applied by hand.

### 6.4 Point of no return — stated plainly

**The point of no return is Step 4.4, the moment the new container starts.**

Rollback stops being *clean* — as opposed to *possible* — the instant
`.token-unmigration-done` is written (within ~2 s of boot). After that:

- `config.json` and both account stores are in the new schema; the stamp makes the
  transform one-shot and non-repeatable.
- The 33 clones have been mutated (`/etc/environment`, the drop-in, `config.toml`)
  and **nothing rolls them back automatically** (§6.3).
- **Refresh tokens are single-use.** The moment the new server refreshes an account,
  the token in your backup is dead. A restore then yields a store of invalid refresh
  tokens and **those accounts need manual re-login.** Practically: rollback is clean
  for roughly the first **10 minutes**, and degrades from there.

**Decide fast.** If Gate 4.6 or 4.10 fails, roll back within minutes rather than
debugging in place. Debugging is what `$BK/boot-after-upgrade.log` is for.

---

## 7. Expected user-visible impact

**Downtime — control-server:** ~30–60 s (4.4). Just the dashboard, API, video and
SSH bastion.

**Downtime — inference:** **this is the real outage, and it is much longer.** At
`9ec205a` the CLIProxyAPI router is **in-process**, so it dies with the container and
**every clone's model access is down for the whole window** — from `docker rm -f`
until each clone's reconcile pass installs credentials and restarts its wrapper.
Budget **2–3 minutes**, and note this is strictly worse than `DEPLOY.md`'s "*a
control-server restart never interrupts agent work*", which describes the **post**
migration steady state, not the migration itself.

**Killed in-flight turns:** **all of them, ~33 clones.** Unlike CT 106 (where
`/etc/environment` was already correct and the restart was suppressed), CT 105's env
*will* change on every clone, so the change-gated wrapper restart fires fleet-wide
(§2.8). At the observed load (15.3, several 40–60 s calls in flight) this is a
meaningful amount of lost work, including any autonomous loops mid-task.

**Manual work afterwards:**

1. **Codex pool bindings are gone** — expected, not a bug (§2.6). Presets `medi`,
   `hyperhost`, `wealthstack`, `personal` will have a blank `codexAccount` and every
   clone will be `codex: auto`. With exactly one Codex account fleet-wide the
   behaviour is identical; re-set them in Settings only if you want the intent
   recorded.
2. **Six Antigravity (Gemini) logins are dropped.** `antigravity-{hello@talktomedi.com,
   mediumaihealthcare@gmail.com, alvin@talktomedi.ca, pegasis.personal@gmail.com,
   test@talktomedi.com, xuhuixin2003@gmail.com}` have no credential-injection path.
   Files are left on disk, reported as a count. **Re-add under Claude or Codex if
   needed.** No CT 106 equivalent — it had 2 and nobody appears to have missed them.
3. **A `Default` Claude pool appears** that was never in `config.groups` (§2.1).
   Cosmetic; delete it in Settings if unwanted — `d4c20da`'s
   `heal_dangling_pool_bindings` will repoint any clone naming it at `auto` safely.
4. **Re-login should NOT be needed.** CT 106 needed none. But if Gate 4.7 shows a
   short count, the affected accounts must be re-imported from a signed-in clone.

**Refresh any open dashboard tab.** A browser tab loaded before the upgrade is
running the `9ec205a` bundle and will misbehave in three distinct ways:

- **Loud:** it calls `/api/groups`, `/api/groups/:name/accounts/login/*`,
  `/api/hosts/:id/group`, `/api/tokens`, `/api/usage/refresh` — all **removed** at
  HEAD. These 404. Annoying but self-announcing.
- **Silent:** it registers `es.addEventListener("tokens", …)`, and HEAD **no longer
  emits a `tokens` SSE event** (confirmed: `9ec205a`'s `web.rs` emits it,
  HEAD's does not). No error — the token panel simply freezes at its last value
  forever. This is the quiet failure mode and it looks exactly like "the numbers
  aren't updating".
- **DESTRUCTIVE — the one to actually worry about:** pressing **Save** in Settings
  on a stale tab sends `presets: [{name, labels, linearKey, group, vars, …}]`. HEAD's
  `merge_presets` ([`config.rs:864`](../crates/control-server/src/config.rs)) reads
  `claudeAccount`/`codexAccount` and **ignores `group` entirely**; a missing field
  yields `""`, and unlike `linearKey` a blank here does **not** keep the stored value
  (deliberately — blank is a meaningful "no opinion"). **Every preset's pool binding
  is silently wiped**, and because `unmigrate_group_proxy_tokens` is one-shot behind
  its stamp, **the migration cannot re-derive them.** Recovery is hand-editing
  `config.json` or restoring from backup.

  **Mitigation: close every RMNG dashboard tab across the whole team before Step 4.4
  and instruct people to hard-reload before touching Settings.** This race is
  unrecoverable and costs one click by someone who was not told.

---

## 8. Open uncertainties — verify before trusting

Stated explicitly rather than papered over:

1. **Why CT 106's clones show emails/`auto` rather than `group:Default` (§1.5).**
   Two plausible mechanisms, not distinguishable from surviving evidence because the
   migration-time logs are gone. This directly affects whether Gate 4.6 will pass.
   **If you can reproduce the migration on a CT 105 data copy in a scratch CT before
   the real run, do that** — it is the single highest-value pre-flight available, and
   it is the only way to test 3-pool carrying before betting production on it.

2. **`docker save | ssh | pct exec docker load` (§4.1)** — not exercised on this host.
   Fallback given.

3. **The rollback drop-in removal loop (§6.3)** — derived from source and path-checked
   on CT 106, never executed. Test on one clone first.

4. **Exact Claude account count after migration (§4.7).** 10 auth files across 2 pools
   with disjoint emails ⇒ 10 expected; the pre-existing `claude-accounts.json` (dated
   2026-07-21) may contain others. `≥ 8` is a deliberately loose band. Compare against
   `$BK/rmng-data.tar`'s copy rather than trusting the number.

5. **Ports 9002/9003 (§3.3).** Published on CT 105, absent from CT 106 and from
   `DEPLOY.md`. I preserved them because clone `Config.Env` references 9002. I could
   not determine what listens on **9003** — no `listen` config key maps to it
   (`{web:9000, video:9001, daemonMcp:9004, forward:9005, bastion:2222}`). **Preserve
   it anyway**; an upgrade is the wrong time to drop a published port whose purpose is
   unclear.

6. **`data/detector-feedback` (21 MB)** is excluded from the backup — no consumer
   found anywhere in `crates/`. If it turns out to matter, add it; it is cheap.
