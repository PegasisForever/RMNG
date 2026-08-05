# HTTP API reference — web port (default `:9000`)

The control-server's **port 2** serves the React management UI (a static SPA served from
disk), the JSON control API, and two SSE streams. It binds `0.0.0.0:{listen.web}`.

- **No auth.** All endpoints are open; the server is meant to sit behind a tailnet /
  firewall. Path params that hit the filesystem (`notes`, `uploads`) are validated as
  DNS labels / `<hex>.<ext>` to prevent traversal.
- **Source files:** routes + handlers in [crates/control-server/src/web.rs](../crates/control-server/src/web.rs);
  chat handlers in [chat.rs](../crates/control-server/src/chat.rs); persisted shapes in
  [files.rs](../crates/control-server/src/files.rs); wire types in
  [crates/wire/src/control.rs](../crates/wire/src/control.rs) and [config.rs](../crates/wire/src/config.rs).
- All request/response bodies are JSON unless noted (`/api/upload` is `multipart/form-data`).
- The frontend talks to this port using ts-rs-generated types in `frontend/app/lib/wire/`,
  kept byte-compatible with the Rust `wire` types.

## Endpoint summary

| Method | Path | Purpose | Success |
|---|---|---|---|
| GET | `/events` | Global state SSE plus named `stats`, `lxcStats`, `forwards`, and `version` events | 200 SSE `ControlState` |
| GET | `/api/state` | Single-shot persisted `ControlState` snapshot | 200 `ControlState` |
| GET | `/api/stats` | One-shot volatile per-clone `ContainerStats` map (same shape as SSE `stats`) | 200 `{hostId: ContainerStats}` |
| POST | `/api/activate` | Select the clone shown in the viewer | 200 `ControlState` |
| PUT | `/api/board` | Replace the board's columns | 200 `ControlState` |
| PUT | `/api/tickets/order` | Replace the ticket column's arrangement | 200 `ControlState` |
| PUT | `/api/clones/muted` | Replace the set of clones whose notifications are silenced | 200 `ControlState` |
| POST | `/api/clone` | Start a clone from an image (resolved Linear ticket / plain / raw hostname) | 200 `{ok, op}` |
| POST | `/api/delete` | Destroy a clone / unregister an unmanaged clone | 200 `Operation` |
| POST | `/api/hosts/:id/archive` | Stop and retain a managed clone | 200 `Operation` |
| POST | `/api/hosts/:id/unarchive` | Restart a retained archived clone | 200 `Operation` |
| PUT | `/api/hosts/:id/forwards` | Replace a clone's port-forward rules | 200 `ControlState` |
| POST | `/api/layout/activate` | Make a layout preset active and live-apply it to all running clones | 200 `{ok,applied,errors}` |
| GET | `/api/images` | List clone-source images (`rmng.image=1`) | 200 `ImageInfo[]` |
| POST | `/api/images/pull` | Pull the clone template from a registry (keeps its own `repo:tag`) | 200 `Operation` |
| POST | `/api/images/commit` | Commit a running clone to a new image | 200 `Operation` |
| POST | `/api/images/delete` | Remove a clone-source image | 200 `{ok}` |
| GET/PUT | `/api/notes/:id` | Fetch or save a clone's rich-text notes | 200 `[block]` / 204 |
| POST | `/api/upload` | Upload an image (multipart) | 200 `{url}` |
| GET | `/uploads/:file` | Serve an uploaded image | 200 binary |
| GET | `/api/ledger/search?q=` | Grep every clone's distilled transcripts, retired clones included | 200 `{hits,scannedBytes,truncated}` |
| GET | `/api/ledger/read?clone=&session=` | Read a byte range of one session's ledger | 200 `{clone,session,offset,len,size,text}` |
| POST | `/api/linear/upload-relay` | Replay one PUT to a Linear-signed bucket URL (multipart) | 200 `{ok,status}` |
| GET | `/api/linear/asset?url=` | Read one Linear-hosted image with a preset's key | 200 binary |
| GET/PUT | `/api/config` | Read redacted config or merge a partial update | 200 `AppConfigRedacted` / `{config,restartRequired,networkWarning?}` |
| POST | `/api/config/test` | Test a setting (`"docker"`, `"judge"`) | 200 `{ok,message}` |
| GET | `/api/setup/env` | Setup wizard environment preflight rows | 200 `SetupEnv` |
| POST | `/api/login/begin` | Start an account sign-in and get the URL to open | 200 `{url}` |
| POST | `/api/login/complete` | Redeem the pasted callback URL, store the account, join a pool | 200 `{ok,email}` |
| POST | `/api/{claude,codex}/refresh` | Force one usage poll (+ rotation pass) now | 200 `{ok,rateLimited,rotated}` |
| POST | `/api/{claude,codex}/swap` | Change a clone's account selection, hot | 200 `{ok,account,group,selection}` |
| POST | `/api/{claude,codex}/delete` | Remove an imported account by email | 200 `{ok,moved}` |
| POST | `/api/{claude,codex}/rotate` | Run one pool-rotation pass immediately | 200 `{ok}` |
| GET/POST | `/api/chat/:id` | Fetch chat snapshot or send a message to the clone's agent | 200 / 202 |
| GET | `/api/chat/:id/events` | Per-clone chat SSE | 200 SSE `ChatSnapshot` |
| POST | `/api/chat/:id/abort` | Abort the in-flight agent turn | 204 |
| GET | `/api/server/version` | Check the running image and remote update status | 200 `UpdateStatus` |
| POST | `/api/server/update` | Update the control-server image | 200 `Operation` |
| POST | `/api/server/restart` | Restart the control server | 200 `{ok}` |
| GET | `/*` | SPA fallback (embedded frontend) | 200 asset / `index.html` |

Error statuses: `400` validation, `404` unknown id/file, `409` chat busy / image still in
use, `500` server (I/O), `502` the Docker daemon or agent-wrapper is unreachable. Error bodies
are a plain string or `{error}`.

---

## State & SSE

### `GET /events`
Subscribe to all control-state changes. Emits a full `ControlState` JSON snapshot
immediately, then a fresh snapshot on every `store.mutate()`; a `ping` comment every 20 s
keeps the connection alive. This is what the dashboard subscribes to.

One named `version` event rides every connection, once, right after the snapshot:

```
event: version
data: {"buildId":"2ae7f50"}
```

`buildId` is the running image's git revision, read from its
`org.opencontainers.image.revision` label at startup. A server with no self container or no
label (a dev run, or an image built without `GIT_SHA`) reports a per-boot id instead, so the
value is always present and always changes across a restart onto a different build.

The dashboard remembers the first one it sees and reloads the page when a later connection
reports a different one. An upgrade drops every SSE connection, so one frame per connection
is enough; there is nothing to poll. This is what keeps a long-open tab from talking to a
server its bundle was not built against.

### `GET /api/state`
The current `ControlState` as a single-shot JSON snapshot — the same document as the first
default `/events` frame, without opening an SSE stream. For one-off readers (the `rmng` CLI's
`ps`/`ops`/`account ls`, scripts).

`ControlState` ([control.rs](../crates/wire/src/control.rs)):

| Field | Type | Notes |
|---|---|---|
| `selected` | `string?` | clone id shown in the viewer |
| `monitors` | `MonitorSpec[]` | legacy field, kept for JSON back-compat; no longer populated (always `[]`) — use `activeLayout` + the config's `layoutPresets` |
| `activeLayout` | `string` | name of the active layout preset, mirrored from config so the board rail's switcher updates over SSE |
| `layoutPresetNames` | `string[]` | names of all layout presets, in config order — drives the board rail's preset buttons |
| `hosts` | `Clone[]` | all registered clones (managed + unmanaged) |
| `boardColumns` | `BoardColumn[]` | the board's columns, left to right — see [below](#boardcolumns) |
| `operations` | `Operation[]` | in-flight + recent clone/delete/archive/unarchive/pull/commit/update jobs |
| `claudeAccounts` | `ClaudeUsage[]` | every imported account's token-free usage view — **both** providers in one flat list, tagged by `provider` |
| `codexResetMarks` | `CodexResetMark[]` | which Codex accounts have already spent a rate-limit reset this 7d window (cooldown bookkeeping) |
| `cloneTokens` | `{ [cloneId]: CloneTokens }` | all-time per-clone token totals + the transient Fable flag — see [below](#clonetokens) |

Despite its name `claudeAccounts` is provider-agnostic: the Claude and Codex pollers each
replace only their own rows (`clone_ops::replace_provider_views`), so a reader filters on
`provider` rather than assuming a list is one provider's. A row written before that field
existed has none, which means Claude. `ClaudeUsage` carries no tokens — only `email`, `active`,
the `fiveHour`/`sevenDay` windows, Claude's model-scoped `fable` window, `spend`, and Codex's
`resetCredits`.

`Clone` carries connection info (`id`, `host`, `port`, `username`, …), the `managed` flag
(true = a Docker container named after the clone id backs it; false = a plain unmanaged
row), `archived` (a retained, intentionally stopped managed clone), the `source` image
reference, the six account-binding fields (`claudeAccountEmail`/`claudeGroup`/`claudeSelection`
and the Codex twins — see [Accounts](#accounts-claude--codex)), Linear metadata (`linearWorkspace`,
`linearTicket`, `linearBranch`, …), the server-owned `monitorState` (`working`/`idle`/`offline`),
`unread` transition marker, and `forwards` (`PortForward[]` — the clone's persisted port-forward
rules; live status rides the `forwards` SSE event below, never `ControlState`).
`Operation` carries `id`, `kind` (clone/delete/archive/unarchive/pull/commit/update — a persisted
legacy `"bootstrap"` op still loads, aliased onto `pull`), `target`, `source`, `status`, `step`,
`pct`, a rolling `log`, and timestamps.

### `stats` event and `GET /api/stats`
The same `/events` connection multiplexes a second, named SSE event: `stats`, a live
`{ <hostId>: ContainerStats }` map for running **managed** clones only (a stopped or
unmanaged clone contributes no entry). `GET /api/stats` returns the exact latest snapshot for
one-off clients such as `rmng clone ls`; it does not wait for a monitor tick. `ContainerStats`
([control.rs](../crates/wire/src/control.rs)):
`cpuPct` (percentage of total host CPU capacity — 100 == every available core busy), plus
`memUsed`/`memLimit` in bytes. `memUsed` is RAM with reclaimable file cache excluded, plus
swap; tmpfs and shared-memory charges remain included. `memLimit` is the clone's RAM plus
swap limit, or `0` when a cgroup limit is unbounded or unavailable. Disk is intentionally not
part of this live event. Sampled by the Docker monitor poller
([monitor.rs](../crates/control-server/src/monitor.rs)); a new subscriber gets the latest map
immediately, then one push per tick — but only when the map actually changed (deduped by value,
not serialization, so an idle fleet doesn't wake subscribers). Deliberately kept out of
`ControlState`/`state.json`: these numbers move every tick, and every `ControlState` mutation
persists the file, so folding stats in would rewrite it on every poll.

### `lxcStats` event
The same connection also sends a named `lxcStats` event for the complete CT 105 LXC that hosts
RMNG, independent of the clone-only `stats` map. Its `LxcStats` payload has `cpuPct`, `memUsed`,
`memLimit`, and `diskUsed`. CPU is measured from the CT-root cgroup’s `cpu.stat` over the monitor
interval: `100` means CT 105's enforced 16-CPU capacity was busy. `memUsed` uses the
same RAM-plus-swap policy as clone stats but includes the control-server, Docker daemon, registry,
caches, and every other CT process. `diskUsed` is physical rootfs usage from CT-root `statvfs`; on
this CT’s ZFS rootfs it is compression-aware. There is intentionally no logical/pre-compression
disk value because it is not observable from the unprivileged LXC. `cpuPct` is `null` until a
second CPU sample establishes a rate; `diskUsed` is `null` when the rootfs stat is unavailable.
Like `stats`, this event is SSE-only and never writes `state.json`.

### `forwards` event
The same `/events` connection multiplexes a fourth, named SSE event: `forwards`, the volatile
port-forward **runtime** map — the live status of each clone's forward rules as the viewer
opens/closes its local listeners. A new subscriber gets the current snapshot immediately, then
one push per status change. It rides its own SSE-only bus (`crate::forward::ForwardBus`), so —
like `stats` — it never enters `ControlState`/`state.json`. The *desired* rules themselves are
persisted on `Clone.forwards` and edited via `PUT /api/hosts/:id/forwards`.

<a id="clonetokens"></a>
### `boardColumns` — the board's columns

Each column is `{ id, title, cloneIds }`, and the array order is the board's left-to-right
order. The operator makes, renames, reorders and deletes columns in Settings; dragging a card
rewrites `cloneIds`. Both paths write the whole list back through `PUT /api/board`.

Three rules keep the stored list from ever hiding a clone:

- A clone that no column claims is drawn in the **first** column. A newly created clone is
  therefore visible immediately, with nothing written here first.
- The **archived** column is not stored. Its contents come from each clone's `archived`
  flag, so dropping a card there calls `POST /api/hosts/:id/archive` and dragging one out
  calls the unarchive route; the card follows the server's answer, not the drag.
- An id whose clone no longer exists is ignored on render, and the next write drops it.

An empty list is legal and means the operator deleted every column. The frontend then draws
one default column, so a fresh install still shows its clones.

### `cloneTokens` — per-clone token accounting
`{ inputTokens, outputTokens, fableActive }` per clone id, accumulated by
[agentlog.rs](../crates/control-server/src/agentlog.rs) from the agent CLIs' own session
transcripts: `~/.claude/projects/<cwd-slug>/<uuid>.jsonl`,
`~/.claude/projects/<cwd-slug>/<uuid>/subagents/agent-*.jsonl`,
`~/.claude/projects/<cwd-slug>/<uuid>/subagents/workflows/<run>/agent-*.jsonl`, and
`~/.codex/sessions/YYYY/MM/DD/rollout-*.jsonl`. Every agent writes these regardless of who
launched it, which is why a hand-run `claude` is counted too.

**Subagents are the bulk of a busy clone's traffic**, not a footnote: 1,943 of one production
clone's 1,974 Claude transcripts are subagent logs, and on another 624 of 1,038 were a workflow's.
A subagent spawned by a subagent writes into the same flat `subagents/` directory as its parent,
so every generation lands one walk deep; a workflow instead gets a directory per run, two levels
below that, which is why the walk goes five deep and not three. Each provider gets its own file
budget, and a clone holding more logs than the budget has its most recently modified files read,
so the sessions actually being written are never the ones dropped.

The server reads them with **no `docker exec`**: [homes.rs](../crates/control-server/src/homes.rs)
already symlinks `<data_dir>/hosts/<clone-id>` → `/proc/<pid>/root/home/rmng` for every running
managed clone, so a 15 s timer does plain file reads, consuming only the bytes appended since the
last pass.

Three properties worth knowing before reading the numbers:

- **Both providers are summed.** One `↑`/`↓` pair per clone, Claude + Codex together. It answers
  *which clone is expensive*, not *what did this account spend* — for the latter use the
  per-account `fiveHour`/`sevenDay` windows on `claudeAccounts`.
- **Cache reads are excluded** from `inputTokens`; cache *creation* is included. One sampled
  response carried 210,735 cache-read tokens against a single real input token, so counting reads
  would drown the figure. Codex's `cachedInputTokens` is netted out for the same reason, and its
  reasoning tokens are added to output.
- **Responses are counted, not lines.** Claude writes one line per content block (a `thinking`
  line, a `text` line, one per `tool_use`), and every one of them repeats its response's whole
  `usage` object, with `output_tokens` climbing to the real figure only on the last. The scanner
  keys on `message.id` and credits each response its running maximum, so a response contributes
  once however many lines carry it. Summing lines instead inflated input by 2.4x on main sessions
  and 3.0x on subagent logs.
- **Cumulative since the clone's first scan, and persisted.** Both CLIs prune old sessions, so a
  figure re-derived from surviving logs would silently shrink over time. A clone's first pass
  adopts its existing logs *without* counting them (an arbitrary retained window is not a
  meaningful starting total), and the count climbs from zero with real traffic. Archiving a clone
  retains its total; deleting one drops it.

`fableActive` is a derived recency flag (5 min, matching the activity window), re-projected each
scan so it decays on its own. Claude-only — Codex records its model in `turn_context` rather than
in the usage event, and Fable is a Claude family regardless.

<a id="monitorstate"></a>
### `Clone.monitorState` — where `working` vs `idle` comes from
Docker liveness supplies `offline`. The other two answer one question about the clone's agent:
**will it get any further without a person?** A clone that finished its task reads `idle`, and so
does one that asked a question, one whose only running command never returns, and one wedged
inside a command that has hung. All four need the same thing, which is you.

**The unit of judgement is one agent session, not one clone.** A clone runs as many sessions as
its operator opened, they are independent, and the clone is `working` when any single one of them
is. Each session is decided on its own evidence alone, and no session ever sees another's. Pooling
them is what let one abandoned permission dialog mark a busy clone idle, and one interrupted tool
call mark it hung for hours.

Most sessions are settled from files alone, with no model involved. Claude Code keeps a live
registry at `~/.claude/sessions/<pid>.json`, and the server reads it through the symlink
[homes.rs](../crates/control-server/src/homes.rs) already maintains:

| what the registry says about a session | verdict |
|---|---|
| no live session in the clone at all | `idle` for the clone — nothing could wake it |
| this session `waiting` | this session is stuck — a dialog is up |
| this session `idle` | this session is stuck — sitting at its prompt |
| anything else | ask (below) |

The `idle` shortcut is exact rather than a guess: Claude Code publishes `shell`, not `idle`, the
moment a background task, dialog or queued request exists. A session record stores its pid as the
*container* numbers it, so liveness is checked against the container's own `/proc`, and
`procStart` guards a reused pid.

Every fold over the probe's event log is scoped to an owner, `(session, agent)`, and one rule
retires all of them. A turn boundary on a session (`Stop`, `UserPromptSubmit`, `SessionEnd`) ends
that session's main-agent evidence, and `SubagentStop` or `StopFailure` ends that subagent's. A
matching `PostToolUse` is not the only thing that ends a tool call: interrupting one fires no hook
at all, and only the next turn boundary says it is over.

**Cursor feeds the same table.** It publishes no registry, so a conversation is rebuilt from the
probe's own event stream instead: a turn that has ended reads `idle`, and one still open reads
`busy`. Two details are load-bearing. A Cursor subagent runs under its own conversation id, never
receives a prompt and never fires its own stop, so only a conversation that has received a
`beforeSubmitPrompt` counts as a session. And `~/.config/Cursor/code.lock` names the running
Cursor and is rewritten at every start, so its mtime dates events to the process that produced
them: without that cut, a Cursor killed mid-turn would read as working until the log rolled.

**The remaining clones get one model call.** The question is narrow enough to be mostly a property
of a command: `cargo build` returns, `npm run dev` does not, and reading a pipe returns only if a
machine and not a person is going to write it. The evidence comes from an activity probe the
reconciler installs in every clone (`~/.rmng/hook.py`), which records turn boundaries, subagent
starts and stops, the outstanding background-task set, and every tool call. An unmatched
`PreToolUse` is what tells a clone sitting inside a command from one that has finished. See
[stuck.rs](../crates/control-server/src/stuck.rs).

The probe is registered twice, under `.hooks` in `~/.claude/settings.json` and again in
`~/.cursor/hooks.json`. Cursor reads the Claude file too and converts it, but the converter has no
name for `postToolUseFailure` and drops it, and that is the only event Cursor fires when a tool
call fails: through the converter alone every failed tool call would stay unmatched forever and
the clone would read as blocked inside a command that had already finished. The Claude entries
also spell out `"matcher": "*"` rather than leaving it implicit, because Cursor's converter calls
`.split()` on the matcher without checking for one and registers nothing at all when it throws.
Cursor fires both registrations, so most events arrive twice and duplicates are dropped on read.

Answers are cached on the view with elapsed times bucketed, so a clone in one state is asked about
once rather than once per four-second tick. On a 32-clone fleet that was 381 calls over six hours.

**With no Codex account imported, no clone is ever reported `working`.** The file checks only
ever produce `idle` or "ask", and nothing answers the ask. That is deliberate: a guess in either
direction is worse than an honest "not working". Per-clone [`cloneTokens`](#clonetokens) accounting
is separate and unaffected. Import an account under Settings → Codex.

GPT answers, on an imported Codex account's ChatGPT plan (`gpt-5.6-luna` at medium reasoning
effort, both settable under Settings → Agents). There is no key to configure: the server already
holds that account's token to run the clones, and the judge spends the same weekly allowance they
do, so a fleet near its cap will feel it. `judge.codexEmail` names the account, and unset means
the first imported one. An account that gets deleted stops the judge rather than moving the spend
onto whoever sorts first.

The calls go to the Codex CLI's own endpoint, `POST https://chatgpt.com/backend-api/codex/responses`.
Three things about it are load-bearing and were measured against it: `stream: false` is refused with
`400 Stream must be set to true`, `store` must be false, and the `response.completed` event carries
an empty `output` array, so the answer is read off `response.output_item.done` instead. Median call
1.8s. `POST /api/config/test` with `{"what":"judge"}` puts one real question to the model and reports
what came back.

Two paragraphs of the prompt are worth 4.9 points on their own, one forbidding a hung verdict
inside a command's first minute and one requiring a worker the agent names to appear somewhere in
the view. Higher reasoning effort was measured instead and bought 0.4 points, so the wording
carries this, not thinking time.

Measured against a replay oracle over two runs on a live 32-clone fleet (2955 scorable samples, 25
real stalls), against the 5-minute token-idle rule this replaced:

| | accuracy | missed a stuck clone | false alarm | median lag | worst lag |
|---|---|---|---|---|---|
| token idle | 90.9% | 250 | 20 | 331s | 1487s |
| this | 96.7% | 44 | 54 | 31s | 92s |

Deciding per session was measured against pooling a clone's sessions into one view, on 2303
samples rebuilt from those same runs, with truth taken per session and a clone's truth the OR:

| | accuracy | missed a stuck clone | false alarm |
|---|---|---|---|
| pooled into one view per clone | 97.8% | 12 | 38 |
| one view per session | 98.2% | 14 | 27 |

They disagree on 15 samples and per-session is right on 12 (sign test p=0.035). It costs nothing:
the same number of calls, over 467 distinct questions rather than 517, because a view carries no
session id and is therefore shared by every session in the same state.

The known gap: a loop that ends when an external system reaches a state it never reaches (polling
CI that stays pending) reads as `working`. The model's reasoning is right and the world does not
cooperate.

**A parent clone is only `idle` when its sub clones are too.** State is decided per clone, so a
parent that handed work to a sub clone and is waiting on it looks quiet, and would read `idle`
while the work it started is still running. After each tick's states are computed, a working sub
clone lifts its parent's `idle` back to `working`
([monitor.rs](../crates/control-server/src/monitor.rs) `lift_sub_clone_activity`). `offline` is
never lifted, since that state is about the container. A sub clone the tick could not reach keeps
holding its parent by the state it is still displayed with. Parentage is one level deep
(`Clone.parent`), so a single pass carries everything.

**Every verdict is written down, so a wrong one can be scored later.** The decision itself is
recomputed each tick and kept nowhere, and the transcript ledger records what a session went on to
do without recording what the server believed while it did it. Those two halves together are what
it takes to call a verdict wrong, so
[stucklog.rs](../crates/control-server/src/stucklog.rs) appends one NDJSON line per decision to
`<data_dir>/stuck/<date>.ndjson`. There is no endpoint: these are files on the server, kept 30
days.

A line is written only when the decision is news, meaning the session's state changed, the session
went away, or the model was called for it. A fleet holding still writes nothing.

| field | what it says |
|---|---|
| `ts`, `clone`, `session` | this SERVER's clock, and who the line is about |
| `state`, `was` | `working`, `idle`, or `gone`, and what it replaced (`new` on a first sighting) |
| `decidedBy`, `why` | `files`, `model`, `cache`, `no-judge`, or `ask-failed`, and the reason |
| `status`, `waitingFor` | the registry status the verdict was read from |
| `promptAgeSeconds` | the CLONE's clock: how long since this session's last `UserPromptSubmit` |
| `view` | the exact JSON the model was asked, when it was asked anything |

To score a verdict, find the line where a session went `idle` and then the next line for that same
session. If it says `working`, the session resumed, and `promptAgeSeconds` on that line is how long
it had gone without a human prompt at the moment it came back. An age larger than the gap between
the two lines means nobody typed anything in between, so the earlier `idle` was wrong, and `view`
on the same line is the question to replay against a changed prompt.

The two clocks are never mixed. `ts` is the server's and every `*Seconds` field is the clone's,
which is why the comparison above is stated as a gap against an age rather than as two instants.

---

## Clone selection & the board

### `POST /api/activate` — body `{ "id": string | null }`
Set `selected` (or clear with `null`). Returns the updated `ControlState`. The media plane
re-targets port 1 to the newly selected clone.

### `PUT /api/board` — body `{ "columns": BoardColumn[] }`
Replace `boardColumns` wholesale. Returns the updated `ControlState`.

A column is `{ id, title, cloneIds }`. Two things are deliberately not stored. The archived
column is derived from each clone's `archived` flag, so a clone dropped there is archived
through `/api/hosts/:id/archive` and never appears in a stored column. A clone that no
column claims is drawn in the first one, which is how a newly created clone reaches the
board. Ids of deleted clones are ignored on render and dropped by the next write.

### `PUT /api/clones/muted` — body `{ "cloneIds": string[] }`
Replace the muted-clone set wholesale, the same bargain as the two above. Answers the new
`ControlState`, whose `mutedClones` is the set sorted and deduplicated.

Muting is a browser-notification filter and nothing else. A muted clone runs, reports, and flags
itself `unread` exactly as before; the only thing suppressed is the desktop notification the
frontend raises when it stops working. This server raises no notifications, so it holds the set
and broadcasts it, which is what lets one mute cover every open tab and the phone.

A muted clone's sub clones are silent too. That rule is applied where the notification is raised
(`frontend/app/lib/mute.ts`), not written into the stored set, so unmuting a parent restores its
children with no second write. Ids for clones that no longer exist are kept, not pruned.

### `PUT /api/tickets/order` (body `{ "ticketIds": string[] }`)
Replace `ticketOrder` wholesale. Returns the updated `ControlState`.

This is the operator's own arrangement of the ticket column, top to bottom, and it is the
only thing the server stores about tickets. Identifiers are lowercased on write, which is how
the browser compares them. An identifier no longer in Linear is ignored rather than pruned,
so a stale entry costs nothing. A ticket the stored order has never seen is drawn on top,
because new work should land where somebody looks rather than at the bottom of a queue.

---

## Clone lifecycle

### `POST /api/clone`
Start a clone container from a clone-source image. Runs async — returns an `Operation` id
immediately; progress flows over `/events`. After the clone is up the server kicks off the
agent's first message ([chat::kickoff_agent](../crates/control-server/src/chat.rs)).

Body (one of three modes + optional account/instructions):
```jsonc
{
  "image": "pegasis0/rmng-template:latest", // required: clone-source image reference (from GET /api/images)
  // -- pick ONE mode --
  "linear": { "workspace": "dev", "ticket": "DEV-123", "ticketUrl": "https://…",
              "branch": "…", "title": "…", "label": "…" },  // a ticket the CLIENT resolved, OR
  "plain":  { "title": "quick task", "message": "do X" },   // no ticket, OR
  "hostname": "w-cp-claude",        // raw clone under this exact hostname (fleet CLI mode)
  // -- optional --
  "preset": "<name>" | "auto",      // clone preset (env + Linear key). Linear mode:
                                    //   absent/"auto" auto-selects by the ticket's team
                                    //   prefix (400 listing the presets if nothing matches).
                                    //   Plain mode: REQUIRED while any presets exist.
                                    //   Hostname mode: OPTIONAL (fleet workers usually
                                    //   need none; a named preset still applies its env).
  "claudeAccount": "a@b.com",       // Claude account SELECTION, verbatim: an email, "auto",
                                    //   "none", or "group:<pool>". Absent ⇒ inherited from the
                                    //   parent clone's selection (sub clone), else "auto".
                                    //   Resolved at assign time, so an unknown email degrades
                                    //   to the best-scored account with a warning, not a 400.
  "codexAccount": "a@b.com",        // the Codex twin, same forms. Independent — a clone can
                                    //   hold one, both, or neither.
  "agentInstructions": "...",       // extra context for the agent-wrapper
  "claudeInstructions": "...",      // extra instructions for Claude Code
  "parent": "<clone-id>",           // nest as a sub clone under this clone (must be a managed
                                    //   top-level one — nesting is ONE level deep). Honoured in
                                    //   every mode; the web dialog's "sub clone of X" checkbox
                                    //   sends it. Omitted ⇒ the caller clone is auto-detected
                                    //   from its `X-RMNG-Proxy-Key` and nested under when it is
                                    //   itself top-level; no key (e.g. a browser) ⇒ top-level.
  "topLevel": true                  // force a top-level clone, skipping that auto-detection.
                                    //   Mutually exclusive with `parent` (400).
}
```
**Linear mode makes no Linear call here.** The client (the web dialog or the `rmng` CLI)
holds the preset keys, so it looks the issue up or opens it, moves it to In Progress, and
posts what came back. Every field is stored verbatim. Only `ticket` is required; an omitted
`workspace` falls back to the team part of the identifier.

`image` accepts a `repo:tag` reference (e.g. `pegasis0/rmng-template:latest`), a full `sha256:…` id, or a bare 64-hex id;
whatever form is passed is canonicalized to the reference and recorded on the clone as
`source`. The image must carry the `rmng.image=1` label (a raw non-image id is rejected). The
selected preset's vars are written into the clone's `/etc/environment`, plus `LINEAR_API_KEY=<preset
key>` (auths the clone's `linear` MCP). Hostname is derived (`pega-{ticket}` or a slug of the
plain title, with a numeric suffix on collision). Returns `{ "ok": true, "op": Operation }` or
`400 {error}`.

**Hostname mode** (what `rmng clone create` sends): the caller owns the exact hostname — a DNS
label, uniqueness enforced (`400` on a taken name) — with no ticket, no derived display name,
and no kickoff first message. The account selections, `agentInstructions`, and
`claudeInstructions` still apply. A sub clone created in this mode inherits its parent's account
*selections* and preset unless the request names them — the selection, not the resolved account,
so a parent on `auto` that landed on some email passes on `auto` and the child gets its own pick.
Every clone also receives Codex parity files: `~/.codex/AGENTS.md` with the same
disposable-sandbox guidance as Claude's shared `CLAUDE.md`, and the managed MCP tables **merged
into** `~/.codex/config.toml` (never overwriting it — see the note below) with the
local desktop MCP and the Linear MCP. Codex authenticates from `~/.codex/auth.json`, which the
server writes — there is no provider block in that config.

**RMNG only owns the MCP tables in `~/.codex/config.toml`.** That file is the operator's:
`model`, `approval_policy`, `sandbox_*`, `[profiles.*]`, and any `[mcp_servers.*]` of their own
are read and rewritten by nothing. The reconciler replaces exactly the `[mcp_servers.desktop]`
and `[mcp_servers.linear]` tables it manages and appends them, leaving every other line intact —
the same courtesy `~/.claude.json` already got (jq-merged, because Claude Code accumulates
project state there). A hand-edit to any other setting survives every reconcile pass.

> There is no `/api/clone/redeploy` endpoint any more. Clone binaries (`clone-daemon`,
> `agent-wrapper`, the `rmng` CLI) are installed by the control-server at create time, before
> the container boots, and refreshed on running managed clones by the clone reconciler after
> server upgrades. The reconciler also refreshes the Codex parity files above on old running
> clones. See [DEPLOY.md#upgrades](DEPLOY.md#upgrades).

### `POST /api/layout/activate` — body `{ "name": string }`
Make the named layout preset the active one and live-apply it to every running clone — no
session restart, no app loss. Validates `name` against `config.layoutPresets` (`400` if
unknown), persists it as `config.activeLayout`, mirrors `activeLayout` +
`layoutPresetNames` into `ControlState` (SSE), then pushes `ServerMsg::SetMonitors` with the
preset's monitors to every connected clone-daemon over the clone socket. Each daemon does a
make-before-break session swap (builds a fresh Mutter session with the new monitors, switches
capture + input to it, stops the old one) — running apps never close. Returns
`{ "ok": bool, "applied": string[], "errors": string[] }`. `ok` is currently always `true`
(the activation itself succeeded; per-clone results are in `applied`/`errors`). `applied`/
`errors` cover only clones whose daemon is currently connected: a daemon whose connection has
already dropped is silently skipped (absent from both lists), since the server only pushes to
currently-connected daemons. `errors` captures an immediate socket-send failure only (there is
no ack).

### `POST /api/delete` — body `{ "id": string }`
Destroy a managed clone (stops it with `SIGRTMIN+3`, removes the container and its
`rmng-dind-<id>` inner-Docker volume) or unregister an unmanaged clone. Returns the `Operation`;
progress over `/events`.

### `POST /api/hosts/:id/archive`
Gracefully stop a managed clone while retaining its container, volumes, notes, and chat history.
Returns an `archive` `Operation`. Unknown, unmanaged, already-archived, or concurrently-operated
clones return `400`.

### `POST /api/hosts/:id/unarchive`
Restart a retained archived clone. Returns an `unarchive` `Operation`; the clone's account
selections are retained, and the reconcile pass re-pushes its tokens once it is up.

---

## Images (clone-source templates) & setup

Clone sources are images labeled `rmng.image=1`, identified by their own `repo:tag` (e.g.
`pegasis0/rmng-template:latest`) — there is no local retag and no golden-CT / CoW model. `POST`
bodies (references contain `/` and `:`, so nothing uses path params).

### `GET /api/images` → `ImageInfo[]`
List clone-source images, newest first. Each `ImageInfo` carries `id` (`sha256:…`),
`reference` (the image's own `repo:tag`, e.g. `pegasis0/rmng-template:latest`), `size_bytes`,
`created_at`, `base` (true for the published clone template, `rmng.base=1`), `created_from`
(lineage, `rmng.created-from`), and `in_use_by` (clone ids of live clones whose `source` is this
image). `502` if the daemon is unreachable.

### `POST /api/images/pull` — body `{ "reference"?: string }`
Pull the clone template from a registry. The pulled image keeps its own `repo:tag` as the
clone-source reference — no local retag. `reference` is a registry `repo:tag` to pull from —
absent/blank defaults to `config.docker.templateReference` (default
`pegasis0/rmng-template:latest`); see
[DEPLOY.md#publishing-the-template](DEPLOY.md#publishing-the-template) for how that image is
built and published. Rejects a blank reference, a `repo@sha256:…` digest reference (pull a
`repo:tag` instead), and a duplicate pull already in flight for the same reference. Verifies
the pulled image carries the `rmng.image=1` label (else it isn't an RMNG template) and warns —
without refusing — if its `StopSignal` isn't `SIGRTMIN+3`. Re-pulling the same `repo:tag`
naturally moves the local tag onto the fresh image (standard `docker pull`) — that is the
refresh. Returns the driving `Operation` (kind `pull`, which the setup wizard's "Download
template" step watches for, showing aggregate byte progress). Replaces the retired in-product
`/api/images/bootstrap` build — no base OS is built in-product any more, only pulled pre-built.

### `POST /api/images/commit` — body `{ "host": string, "name": string }`
Commit a running managed clone (`host`) to a new clone-source image `<name>:latest` — the
DNS-label `name` is the full repo (kind `commit`). `docker commit` **excludes volume mounts**,
so the clone's inner-Docker state (`/var/lib/docker`) never enters the image — clones always
start with an empty inner Docker. On-disk credentials in the clone's home **are** baked in
(logged as a warning). Rejects a name that already exists.

### `POST /api/images/delete` — body `{ "reference": string }` → `{ok}`
Remove a clone-source image. `409` if any clone still runs on it (`in_use_by` non-empty) or a
running operation (clone/commit/pull) references it as its source or target; the daemon's own
"in use by a container" `409` is surfaced too. If the same image carries more than one tag,
deleting one `reference` only untags it while the others stay attached to the same layers — the
image re-lists under a remaining reference; delete again to actually free them.

### `GET /api/setup/env` → `SetupEnv`
The setup wizard's environment preflight: `{ rows: EnvCheckRow[] }`, each row `{ id, label,
ok, detail, required }`. Rows, in order: **Docker daemon** reachable (`dockerDaemon`,
required), **control-server container** detected (`selfContainer`, info — absence = dev mode),
**clone media socket mount** at `/srv/rmng-sock` (`sockMount`, required), **GPU render node**
`/dev/dri/renderD128` (`renderNode`, required), and **LXCFS** on the Docker host (`lxcfs`,
advisory — without it clones see host-wide `/proc` values). Cached from the Docker self-setup probe
(refreshed at startup and by `POST /api/config/test {docker}`).

---

## Notes & uploads

### `GET /api/notes/:id` → `[block]` &nbsp;·&nbsp; `POST /api/notes/:id` (204)
Per-clone rich-text notes (BlockNote block array), stored at `data/notes/{id}.json`. `:id`
must be a DNS label. GET returns `[]` if none.

### `POST /api/upload` (multipart `file`) → `{ "url": "/uploads/<hex>.<ext>" }`
Image upload (png/jpeg/gif/webp/svg/avif/bmp, ≤15 MB) → `data/uploads/`.

### `GET /uploads/:file`
Serve an uploaded image by its generated `<16-hex>.<ext>` name, with the right Content-Type.

### `POST /api/linear/upload-relay` (multipart `url`, `headers`, `file`) → `{ "ok": true, "status": 200 }`
Replay one PUT at the Google-signed URL Linear's `fileUpload` mutation handed the browser. The
page cannot send it itself: the bucket answers the preflight with `vary: Origin` and no
`access-control-allow-origin`. This route holds no key and never calls Linear's GraphQL.

`url` is the `uploadUrl`, `headers` is Linear's `headers[]` as JSON with the declared
`content-type` in front, and `file` is the bytes. Every header is forwarded verbatim, because
the signature covers them, minus `host`, `content-length`, `transfer-encoding`, and
`connection`. The PUT goes to whatever `url` names. The whole body is capped at 64 MB by
`DefaultBodyLimit`.

The hop gives up at 45 seconds, inside the signed URL's 60-second `X-Goog-Expires` window.

### `GET /api/linear/asset?url=<assetUrl>` → the image bytes
Read one Linear-hosted image and serve it same-origin. An `assetUrl` answers an unauthenticated
GET with 401, and `uploads.linear.app` leaves `authorization` out of its CORS allow-list, so
neither an `<img>` nor a `fetch` in the page can read one. This route fetches it with a
preset's Linear key, trying each configured key until one is allowed to. It GETs whatever `url`
names.

The `Content-Type` is whatever the upstream declared, forwarded as it arrived: Linear stores the
`contentType` its uploader sent and hands the same string back. The answer also carries
`Cache-Control: private, max-age=3600`, because an `assetUrl` addresses one immutable file.

The bytes are streamed through rather than collected, capped at 32 MB by a declared
`content-length` or by a running count without one. One 20-second deadline covers the whole
request, every key attempt and the body together, and a request past it is a 504.

Issue bodies keep the original `uploads.linear.app` URL. The browser rewrites IMAGE
destinations to this route when it renders and rewrites back before it saves, so nothing stored
in Linear points here. A link destination and a bare URL keep pointing at Linear, where they
stay readable and already load without a key.

---

## Transcript ledger

A distilled copy of every clone's Claude Code and Cursor transcripts, kept after the clone is gone. The
server tails `data/hosts/<id>/.claude/projects/<slug>/<session>.jsonl` every 30 seconds through
the symlinks the clone-home reconciler maintains, and writes one NDJSON record per event to
`data/ledger/<clone>/<session>.ndjson`. Nothing runs inside a clone and no model runs here.

One more pass runs at the start of a delete and an archive. The `hosts/<id>` symlink disappears
the moment the container stops, so anything not captured while the clone runs is unrecoverable.

Both endpoints have a CLI front end: `rmng ledger search` and `rmng ledger read`
(see [CLI.md](CLI.md)). The record shapes are `wire::ledger`, so the on-disk NDJSON line and
what the CLI parses are one definition.

**Record shape.** Every line names its clone, session, timestamp and kind first, so a grep hit
reads without needing its file header.

```json
{"clone":"pega-we-142","session":"49a5…","ts":"2026-08-01T10:00:00.000Z","kind":"user","text":"implement PER-26"}
{"clone":"pega-we-142","session":"49a5…","ts":"…","kind":"toolUse","tool":"Bash","toolId":"toolu_1","text":"{\"command\":\"cargo test\"}"}
{"clone":"pega-we-142","session":"49a5…","ts":"…","kind":"toolResult","toolId":"toolu_1","text":"412 passed"}
```

`kind` is one of `user`, `assistant`, `toolUse`, `toolResult`, `title` (the CLI's own session
title, written only when it changes) or `compact` (a compaction boundary, which the CLI appends
in place rather than rewriting the file). `sidechain: true` marks a subagent's turn. What never
reaches a record: base64 image data, replaced by `[image/jpeg, 373332 base64 bytes dropped]`,
and the model's thinking. Tool inputs are clipped at 800 characters, tool results at 2000, and
message text at 20000, each with the dropped byte count spelled out.

Measured on one three-day session: 132,486,201 raw bytes distil to 2,875,456, which is 2.2
percent, in 3,879 records.

**Names are spent for good.** A ledger directory outlives its clone, so `data/ledger/` is the
registry of every clone name ever used. A derived hostname skips those names, and `POST
/api/clone` with an exact `hostname` rejects one with a 400 naming the directory to remove.
Reusing a name would file two unrelated histories in one bucket.

### `GET /api/ledger/search` → `{ hits, scannedBytes, truncated }`

| Param | Meaning |
|---|---|
| `q` | Required. A case-insensitive substring, matched against the whole ledger line, so it reaches the text, the tool name and the kind alike |
| `clone` | One clone id. Absent searches every clone the ledger knows, live or retired |
| `since` / `until` | Epoch milliseconds, both inclusive. A record whose timestamp will not parse passes both bounds |
| `limit` | Hits to return. Default 50, capped at 500 |

Each hit is `{clone, session, ts, kind, offset, len, line}`. `offset` and `len` locate the line
inside `<clone>/<session>.ndjson`, which is what the read endpoint takes.

Files are searched newest-modified first, so a limit cuts the oldest sessions rather than the
most recent. `truncated` is true when the search stopped on the hit limit or on its 512 MB byte
budget, meaning there are more matches. Hits come back newest first.

### `GET /api/ledger/read` → `{ clone, session, offset, len, size, text }`

`clone` and `session` are required; `offset` defaults to 0 and `len` to 64 KB, capped at 1 MB.
Pass an offset below a hit's own to read what led up to it.

The range is snapped outward to line boundaries, so `text` is always whole NDJSON lines and
never a fragment of one. `offset` and `len` in the answer are the snapped ones, and `size` is
the whole file, so a caller can tell how much is left on either side. Reading past the end
returns an empty `text` rather than an error. An unknown clone or session is a 400.

---

## Configuration

### `GET /api/config` → `AppConfigRedacted`
The full config, preset Linear keys included as `linearKey: string`. The browser lists Linear
issues itself, so this is where it gets a key; the server answers only on a Tailscale-only
network, which is what makes that acceptable.
Everything else is returned verbatim — ports, `layoutPresets`/`activeLayout`, the `docker` block
(`socket`/`subnet`/`hostnamePrefix`/`cloneCpus`/`cloneMemoryMb`; no secret — the local daemon
socket needs none), `staticDir`/`cloneSocket`/`chroma`, `setupComplete`,
`agentPlaybook` (the editable agent playbook seeded with the shipped default and injected into new
clones — non-secret; a preset's optional `agentPlaybook` append rides along in each `presets` row),
the Claude/Codex poll config, and the two account pools `cloneGroups`/`codexGroups` (names +
member emails only — no credentials, so they pass through unredacted). See
[PROTOCOL.md](PROTOCOL.md#config-schema) for the schema.

### `PUT /api/config` (partial merge) → `{ config, restartRequired, networkWarning? }`
Deep-merge a partial config over the stored one, persist to disk at `0600`, apply live.
Returns the redacted config plus `restartRequired: boolean` — set when a restart-required
field changed (the four listen ports, `cloneSocket`, `docker.socket`, `staticDir`, `chroma`)
so the UI can prompt for a restart. `cloneSocket` still triggers this pre-latch (the server
bound the old path at startup) even though it is a one-time field (see below). A wizard-finish
flip (`setupComplete` false → true) materializes the lazy `rmng` network here; a failure is
non-fatal and echoed as `networkWarning`. Merge rules: an **empty string keeps** the stored
value; a non-empty string replaces it; `presets` rows merge by name (blank `linearKey` keeps
the stored one); `cloneGroups`/`codexGroups` are replaced **wholesale** (the pool editor always
posts the full list, so an omitted pool is a deletion and `[]` clears them — the two providers'
lists are independent). `docker.subnet` is validated as an IPv4 `/16`–`/24` CIDR. One-time fields
(`dataDir`, `cloneSocket`, `docker.subnet`) are locked once `setupComplete` latches (which
itself is a one-way latch).

### `POST /api/config/test` (body `{ "what", "value"?, "model"? }`) → `{ ok, message }`
Synchronously test a setting. `value` and `model` carry what the operator has typed but not saved,
so the verdict is about the field they are looking at rather than about the stored one.

| `what` | tests | `value` / `model` |
|---|---|---|
| `docker` | re-runs the Docker self-setup probe and collapses the environment report (daemon reachable, sock mount, render node) into one verdict. The row-by-row breakdown is `GET /api/setup/env` | neither |
| `judge` | puts one real stuck-detection question to GPT, which exercises the account lookup, the token refresh, the endpoint and the model name together | `value` = the account email (blank = the first imported), `model` = the GPT model |

---

<a id="accounts-claude--codex"></a>
## Accounts (Claude + Codex)

Clone model traffic never touches the control-server: each agent dials its provider directly and
authenticates from a credential file on disk. What the server owns is the **credential**, not the
route. Each imported account's full OAuth pair (access + single-use refresh token) lives only in
`data/claude-accounts.json` / `data/codex-accounts.json` (`0600`), and a clone receives **only the
current short-lived access token**, written into `~/.claude/.credentials.json` /
`~/.codex/auth.json` with an **empty refresh token** and a far-future expiry. That shape is
deliberate: with no refresh token the clone's CLI can never rotate — and thereby invalidate — the
token the server owns, and with a far-future expiry it never decides the token is dead and gives
up. The server re-pushes a fresh token to every clone on the account whenever a refresh rotates it
([claude.rs](../crates/control-server/src/claude.rs) `push_stale_tokens`). Both agents re-read
their credential file per request, so every push is a **hot swap** — no restart, no interrupted
turn.

A clone's binding is six optional fields on its `Clone` row, three per provider and fully
independent (one clone can run both, one, or neither):

| Field | Holds |
|---|---|
| `claudeSelection` / `codexSelection` | the operator's intent **verbatim**: an email, `auto`, `none`, or `group:<pool>` |
| `claudeAccountEmail` / `codexAccountEmail` | the account currently resolved from that selection — whose token is installed right now |
| `claudeGroup` / `codexGroup` | the pool the clone is being balanced within, when the selection is `group:<pool>` |

The selection is kept apart from the resolved account because the two answer different questions:
`claudeAccountEmail` alone can't distinguish an auto-managed clone (the server may hot-swap it) from
one pinned to that exact account, and a sub clone inherits the *selection* so a parent on `auto`
doesn't pin its children to whatever it happens to be running.

**Account pools are config, not endpoints.** They are the two `cloneGroups` / `codexGroups` lists
(each `{name, accounts: [email]}`), edited **wholesale** through `PUT /api/config` like any other
setting — the editor always sends the full list, so a plain array replace is the merge rule and an
empty array clears the pools. There are no dedicated group endpoints. A clone bound to a pool
sticks to its account (preserving its Anthropic prompt cache — an account switch cold-starts it)
until that account is exhausted or leaves the pool; the 10-minute rotator then moves it to the
least-loaded member.

### The twelve account endpoints

Symmetric across the two providers — `{claude,codex}` below is a literal path segment, and the two
sets differ only in which store and credential file they touch.

| Endpoint | Body | Returns | Does |
|---|---|---|---|
| `POST /api/login/begin` | `{provider}` | `{url}` | Mint a PKCE verifier and return the provider's authorize URL. Nothing is stored against an account yet, and the sign-in expires after 15 minutes if its callback never comes back |
| `POST /api/login/complete` | `{provider, pasted, group}` | `{ok, email}` | Redeem the pasted callback, store the account, and add it to `group` (empty for none). Kicks an immediate usage poll |
| `POST /api/{claude,codex}/refresh` | — | `{ok, rateLimited, rotated}` | Force one usage poll now, then a rotation pass. `rateLimited` is true if any account hit a 429 |
| `POST /api/{claude,codex}/swap` | `{host, account}` | `{ok, account, group, selection}` | Change a clone's selection. `account` is the verbatim selection; the reply echoes the resolved account + pool + normalized selection |
| `POST /api/{claude,codex}/delete` | `{account}` | `{ok, moved}` | Remove an imported account by email. `moved` is the ids of clones reassigned off it |
| `POST /api/{claude,codex}/rotate` | — | `{ok}` | Run one pool-rotation pass immediately (it otherwise runs every 10 min). Ops/testing |

**Swap** resolves the selection and acts immediately: `none` deletes the clone's credential file
(leaving it tokenless), a pool picks a member — stickily, keeping the incumbent account when it is
already an eligible member of the target pool — and an email or `auto` resolves a single account,
whose token is pushed before the row is updated. `400` for an unknown or unmanaged host, or when
the provider has no imported accounts at all; `502` when the push to the clone fails.

**Delete** refuses with `400` while any clone is **pinned** to that account (the message lists
them) — pinned meaning its selection *is* that email, which nothing but an operator edit can
change. Clones merely *running* it via `auto`/a pool are not blockers: they are reassigned by the
rotation pass the delete triggers, and any that can't be placed (the account was a pool's only
member) have the dangling reference cleared rather than being left pointed at a deleted account.

Errors from `login/*`, `delete`, and `rotate` are `{error}` JSON bodies (what the
frontend's `postJson` reads); `swap` returns a plain string, like the clone-lifecycle routes.

---

## Per-clone agent chat

The control-server proxies chat to each clone's agent-wrapper (`http://{host}:{agent_port}`,
default `:4096`), persisting history at `data/chats/{id}.json`.

| Endpoint | Body | Returns | Does |
|---|---|---|---|
| `GET /api/chat/:id` | — | `ChatSnapshot` | `{busy, activity, messages[]}` snapshot |
| `POST /api/chat/:id` | `{text}` | `202` / `409` if busy | Persist the user message, set busy, spawn the turn (opens the wrapper's `/events`, POSTs `/prompt`, relays activity, records the reply). Watchdog: 30 min hard / 3 min idle |
| `GET /api/chat/:id/events` | — | SSE `ChatSnapshot` | Snapshot + a fresh one on each message/activity/busy change; 20 s ping |
| `POST /api/chat/:id/abort` | — | `204` | Best-effort POST to the wrapper's `/abort`; clears busy |

`ChatMessage` = `{ id, role (user|assistant), text, ts }`.

---

## SPA fallback

### `GET /*`
Serves the installed React build from disk; unknown paths fall back to `index.html` for
client-side routing. The bundle is resolved at startup: `/usr/local/share/rmng/static` in the
image, else the repo dev build (`frontend/build/client`). A non-empty `staticDir` config field
(Settings → Advanced; restart-required) overrides that with any disk path (frontend
hot-reload during dev). If no frontend resolves anywhere, the route returns a 404 hint and the
API stays up.
