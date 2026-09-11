# `rmng` CLI reference — fleet management over the web port

The `rmng` binary ([crates/cli](../crates/cli/README.md), package `rmng-cli`) is the fleet
management surface: clones, imported accounts and their pools, and operations, all over
the control-server's **port-2 web API** (via [control-client](../crates/control-client/README.md)).
It also carries the **operator/fleet desktop control** (`rmng desktop`, folded in from the
retired global MCP) and a docker-exec-style **`rmng clone exec`** — both reach clones through the
same web API, which proxies to the clone's daemon MCP / Docker exec. What stays elsewhere:
the **in-clone** agent's own desktop automation is the daemon MCP's job ([MCP.md](MCP.md)),
clone-agent chat is the web API's (`/api/chat/:id`, [API.md](API.md#per-clone-agent-chat)), and
code moves via git.

- **Source files:** command tree in [crates/cli/src/args.rs](../crates/cli/src/args.rs);
  handlers in [commands.rs](../crates/cli/src/commands.rs); wait machinery in
  [wait.rs](../crates/cli/src/wait.rs).
- **Build:** `cargo build -p rmng-cli` → `target/debug/rmng`.

## Where it lives

The control-server injects the CLI into **every clone at create time** as
`/usr/local/bin/rmng` — on PATH in every shell (`/opt/rmng/bin`, where the service binaries
go, is not). The Dockerfile builds `-p rmng-cli` and ships the payload at
`/usr/local/share/rmng/rmng-cli`; [`provision.rs`](../crates/control-server/src/provision.rs)'s
`CLONE_BINARIES` copies it in before the container boots. The clone reconciler also refreshes
this binary on already-running managed clones after a control-server update.

Codex itself is template-installed under the clone user, and the control-server retries a
missing standalone Codex CLI install at clone creation and from the clone reconciler for old
running clones. RMNG gives Codex parity with Claude's shared clone context by managing
`~/.codex/AGENTS.md` and the MCP tables in `~/.codex/config.toml`: Codex gets the same disposable-sandbox
guidance, the local desktop daemon MCP (`desktop`), and Linear (`linear`, using
`LINEAR_API_KEY`). Codex authenticates from `~/.codex/auth.json`, which the control-server
writes with the short-lived access token of the account the clone is assigned. The clone
reconciler refreshes those files on old running clones.

## Server resolution

`--server <URL>` > `$RMNG_CONTROL_URL` > `http://localhost:9000`. The control-server sets
`RMNG_CONTROL_URL` in every clone's `/etc/environment`, so a bare `rmng …` inside a clone
auto-resolves the server with no `--server`. Blank values fall through; a trailing `/` is
stripped. A connection failure prints the resolved base with a `set --server or
$RMNG_CONTROL_URL` hint.

## Global flags & output

- `--server <URL>` — control-server web-API origin (e.g. `http://rmng-control:9000`).
- `--json` — machine-readable JSON, honored by **every** command (progress/prompts/warnings go
  to stderr, so stdout stays clean). Most commands emit the [`wire`](../crates/wire/src/control.rs)
  types verbatim; the exceptions carry a small CLI-owned shape (below). Under `--json`, **errors
  are JSON too** — `{"error": {"message", "hint"}}` on stderr, with the same exit codes.

| Command (with `--json`) | Emits |
| --- | --- |
| `clone ls` | `{ selected, clones: [Clone + {stats, accounts, column}], operations }` (CLI shape — includes the metrics the table shows) |
| `board ls` | `BoardColumn[]`, resolved the way the dashboard draws them |
| `board move` | the resolved `BoardColumn[]` after the move |
| `clone select`, `account swap`, `account rm` | small status object (`{selected}` / the `{ok, account, group, selection}` / `{ok, moved}` reply) |
| `clone ssh` | `{ command, mode: "direct"\|"bastion" }` |
| `clone create-plain`, `clone fork` | the started `Operation` (the **terminal** `Operation` with `--wait`, plus a `clone` field holding the finished record once it has an address) |
| `clone rm`, `clone archive`, `clone restore` | the started `Operation` (the **terminal** `Operation` with `--wait`) |
| `clone cp` | `{ bytes, dst }` |
| `clone self` | the caller's `Clone` record, or exit 1 outside a clone |
| `op wait` | the terminal `Operation` |
| `op ls` | `Operation[]` |
| `account ls` | `ClaudeUsage[]` |
| `desktop` (screenshot/action) | `{ screenshot: <path>, text? }`; query verbs → the tool's JSON |

## Exit codes

| Code | Meaning |
| --- | --- |
| `0` | ok (including a "vanished" wait — see below) |
| `1` | API / transport error (also: `rm` confirmation declined) |
| `2` | usage error (clap) |
| `3` | the waited-on operation ended in **Error** |
| `4` | `--wait` / `op wait` timed out |

## Commands

The surface is **noun → verb**. Nouns: `clone`, `account`, `op`, `ledger`, `board`,
`desktop`.
The target is always a positional **clone id** (the first column of `rmng clone ls`).

### `rmng clone ls`

Clones table: `ID` (a `*` suffix marks the selected clone), `COLUMN` (the board column the
clone is drawn in; blank on a sub clone, which is drawn under its parent's card), `IP` (the current Docker bridge
address when available), `IMAGE` (source reference), `PRESET`, `CLAUDE` and `CODEX` (the account
each provider is running — the resolved email, falling back to the selection when none is
assigned yet), live `CPU` and `RAM`, and lifecycle `STATUS`. Sub clones are indented under their
parent. CPU/RAM are volatile snapshots for sampled active managed clones.
`rmng clone ls --json` returns the CLI shape `{ selected, clones: [Clone + {stats, accounts, column}],
operations }` — so the metrics the table shows are available to a machine reader too.

Each clone also carries a derived `accounts` object, one entry per provider:

```json
"accounts": {
  "claude": { "selection": "auto", "email": "me@example.com", "pool": null },
  "codex":  { "selection": "group:gpt", "email": null, "pool": "gpt" }
}
```

`selection` is the operator's intent verbatim (`auto` / `none` / `group:<pool>` / an email),
`email` is the account actually installed, and `pool` is set only when the selection names one.
All three can be null: a clone that has never been assigned has no selection, and `email` stays
null while an `auto` selection is still unresolved. **Read `selection`, not `email`, to decide
whether a clone may be swapped out from under you** — an `auto` clone with a resolved email can
still move at the next rotation, whereas a pinned one cannot. The same six fields are also
present flat on the clone object (`claudeSelection`, `claudeAccountEmail`, `claudeGroup`, and
the Codex twins); `accounts` is a convenience view over them, not extra data.

### Creating clones — two verbs

Both build from a preset image: `create-plain` onto a fresh home, `fork` onto a copy of a
live clone's home. The server names the clone and picks up whatever the flags leave open.
Each prints the started op id (follow with `rmng op wait <op-id>`), or blocks with `--wait`.

- `rmng clone create-plain --title <T> [--preset <P>]` — `--preset` is required when any
  presets are configured.
- `rmng clone fork [SOURCE] [--preset <P>] [--claude-account <SEL>] [--codex-account <SEL>]
  [--headless]` — an omitted source is the preset's default fork clone where it is still
  forkable, else the oldest forkable clone. An omitted preset keeps the source's; naming one
  moves the fork to that preset's account pool. A selection is an email (pin), `auto`, `none`
  (no token), or `group:<pool>`.

**Shared flags:** `--message <M>` | `--message-file <PATH>` (first message auto-sent to the
agent; omitted ⇒ nothing is sent), `--column <NAME>`, `--no-startup-script`, `--wait`
`[--timeout <N>]`.

`--column` files the new clone at the **top** of that column, by title or id. The name is
resolved before anything is created, so a typo costs no clone. The id is written to the board
as soon as the operation starts, which is why this needs no `--wait`: the board ignores an id
matching no clone, and the card appears at the top the moment the clone does. Naming an
archive column files the clone there without archiving it, since archiving something that is
still being created would race its own creation.

```sh
rmng clone create-plain --title 'Fix the flaky login test' --preset work --wait
rmng clone fork --preset work --message 'carry on from here'
```

### `rmng clone rm <CLONE> [-y|--yes] [--wait] [--timeout <N>]`

Destroy a clone (container + volumes; cascades to its sub clones). Asks `[y/N]` on stderr unless
`-y`; declining exits 1. **Refuses to run non-interactively without `-y`** (stdin not a terminal).

### `rmng clone archive <CLONE>` / `rmng clone restore <CLONE>` `[--wait] [--timeout <N>]`

Stop a managed clone while retaining its container/volumes/notes/chat, then restart it later.
Reversible, no confirmation. The server refuses unknown / unmanaged / already-in-state clones.

### `rmng clone ssh <CLONE>`

Print the ready-to-paste `ssh` command for a usable managed clone (working/idle/not-yet-sampled).
From inside a clone it prints a direct command; otherwise a bastion jump. Unmanaged/archived/
offline clones are refused. `--json` → `{ command, mode }`.

### `rmng clone exec <CLONE> [-u <user>] [-w <dir>] [-e KEY=VAL]… -- <cmd…>`

Run one non-interactive command inside a clone (docker-exec style); forwards piped stdin and
passes through the command's exit code. `--json` emits one object with the captured streams.

### `rmng board ls`

The dashboard's columns, left to right: `COLUMN ID ARCHIVES CLONES CONTENTS`. The view is
**resolved**, not the raw stored list, so a clone nobody has filed appears in the column the
board draws it in rather than nowhere. A board nobody has arranged yet reports the two columns
the dashboard draws by default, `Clones` and `Archived`.

`--json` emits `BoardColumn[]` in the same resolved form.

### `rmng board move <CLONE> <COLUMN> [--wait]`

Move a clone to the **top** of a column. `COLUMN` is what a person reads off the board, so
`"In Progress"` works; the stored id (`in-progress`) works too, and both ignore case and
surrounding space. An unknown name lists the columns that do exist and changes nothing.

**An archive column archives.** Dropping a card into one on the dashboard archives the clone
and dragging it out again restores it, so this does the same: moving into `Archived` stops the
clone, moving it back out starts it. `--wait` blocks on that lifecycle operation. Without it
the move is filed immediately and the archive runs in the background.

A sub clone is refused. The board draws it under its parent's card and never files it, so
filing one would write an id no column ever draws. Move the parent instead.

### `rmng clone cp <SRC> <CLONE>:<DST-DIR> [--exclude <name>]…`

Copy a directory into a clone at an absolute path. `SRC` takes two forms, and which one you
use decides where the bytes travel.

**`<CLONE>:<DIR>`, clone to clone.** The server can already see every running clone's home
(the same links the SMB share is built on), so it does the copy itself, with `rclone` in
parallel across files. Nothing enters this process, a socket, or the Docker API. Use this
for a large tree. Both paths must sit under `/home/rmng`, and both clones must be running.
An image without rclone falls back to `cp -a`, which is slower on a source tree and the only
one of the two that preserves hardlinks.

**A local directory, streaming.** `tar` writes into the request body and the server passes
the archive to the Docker daemon, so the project streams through without either end
buffering it, and the route is exempt from the 64MB body cap the JSON routes carry. This is
the only form available from a machine the server cannot see, such as an operator laptop,
and it is the only one that reaches a path outside `/home/rmng`.

Files and directories both arrive owned as they were at the source, which for a copy between
clone agents is the same agent user on the far side. Directories take a second pass, because
the rclone Ubuntu ships has no directory metadata and would otherwise leave every one of them
owned by the server's root: a tree whose files are writable but whose directories are not
lets an agent edit code and fail to create a single new file.

Measured clone to clone on CT 101: 266 MB over 23,470 files takes **2.1s** out of band
against **5.8s** streamed, and 4 GB in five files **1.4s** against **10.0s**. Bulk data is
dominated by the transport and a large file count by per-file work, so the out-of-band form
wins on both, for different reasons.

ZFS block cloning does not help here even where the pool supports it. `FICLONE` returns
`EPERM` inside an unprivileged LXC, on a plain dataset as much as through overlayfs, though
the same `cp --reflink=always` succeeds on the Proxmox host. Where the pool runs dedup, a
duplicated tree still costs little space; it costs the write.

The destination is created if missing. Nothing is deleted there and nothing is copied back:
an existing directory receives these files on top of what it already holds.

`--exclude <name>` is anchored at the top of SRC and matches a directory there and nowhere
below it. That matters for a JavaScript project: unanchored, `--exclude dist` would also
strike every `node_modules/*/dist`, and the copy would look complete while importing nothing.

    rmng clone cp /home/rmng/proj agt-1a2b:/home/rmng/proj --exclude target --exclude .venv

`--json` → `{ bytes, dst }`. `--exclude` is anchored at the top of SRC either way.

### `rmng clone sync <CLONE>:<SRC-DIR> <CLONE>:<DST-DIR> [--exclude <name>]…`

`cp` with deletion: the destination ends up matching the source, so a file it holds and the
source does not is removed. Everything else is `cp`'s clone-to-clone form, including the
anchored excludes and the ownership handling.

An excluded name is left alone rather than deleted, which is what lets a destination keep its
own `target/` through a sync that excludes it.

Two restrictions, both because deletion is involved. The source must be a clone, since a
streamed archive tells the server what it holds and never what it lacks; use `cp` to send a
local directory. And the destination cannot be `/home/rmng` itself, where a sync would delete
`Desktop`, `.ssh`, `.claude` and everything else the source happens not to have.

    rmng clone sync pega-we-142:/home/rmng/proj agt-1a2b:/home/rmng/proj --exclude target

### `rmng clone self`

Print the calling clone's own id, or the whole record with `--json`. Identity is the
per-clone router key in this process's environment, the same proof the server trusts for
sub-clone nesting, so it needs no hostname convention. Outside a clone it prints nothing and
exits 1.

### `rmng clone select <CLONE>` / `rmng clone select --none`

Point the operator's viewer at a clone (`POST /api/activate`); `--none` clears it. **Operator-only
— it does not change which clone your other commands target.** Unknown id errors (exit 1).

### `rmng image …` (removed)

Gen-1 image commands (`image ls|pull|rm`) are gone: gen-2 clones build their image from the
preset Dockerfile on demand, and unused tags are purged automatically on delete.

### `rmng account ls [--provider claude|codex]`

Read-only listing of imported accounts and usage windows: `EMAIL PROVIDER ASSIGNABLE 5H
5H-RESETS 7D FABLE ERROR`. Both providers by default; `--provider` filters to one.

`--json` emits a flat `ClaudeUsage[]` (both providers in one array, tagged by `provider`; a row
written before that field existed has none, which means Claude). Each row's `id` is
`<email>|<orgUuid>` for Claude and `codex:<accountId>` for Codex — **treat it as opaque**: it is
scoped to the server's account store, so an account re-imported after a delete may not keep its
previous id. Key off `email` + `provider` if you need a stable identity across imports.

### `rmng account swap <CLONE> <ACCOUNT> [--codex]`

Hot-swap a clone's account for one provider (`POST /api/{claude,codex}/swap`). `<ACCOUNT>` is a
selection verbatim: an email (pin it), `auto` (the server picks and may re-pick), `none` (install
no token — the clone boots provably tokenless), or `group:<pool>` (bind it to a named pool and
let the rotator balance it). The token is written into the clone's credential file immediately;
nothing restarts, because the agents re-read those files per request.

### `rmng account rm <ACCOUNT> [--codex]`

Delete an imported account by email. Refused (`400`) while any clone is explicitly **pinned** to
it — that pin is an operator decision, not a rotation, so it is never silently undone. Clones on
`auto` or a pool are moved to another account first; the reply's `moved` lists them.

### `rmng op ls`

The current `operations[]`: in-flight + recently-finished clone/fork/rebase/delete/archive/
restore/prebuild/update jobs (`ID KIND TARGET STATUS STEP PCT MESSAGE`). Finished ops are pruned quickly.

### `rmng op wait <op-id> [--timeout <N>]`

Block until an operation reaches a terminal state (default timeout 600 s). Same semantics as
`--wait` on the starting command.

### `rmng ledger search <PATTERN> [--clone <id>] [--since <when>] [--until <when>] [--sidechain | --no-sidechain] [--agent <id>] [--limit <N>]`

Search the distilled transcripts of every clone the ledger knows, retired clones included. The
control-server tails each running clone's Claude Code and Cursor transcripts and keeps a greppable copy
under `data/ledger/<clone>/<session>.ndjson`, so this answers "how did we do this last time"
even when the clone that did it is gone. See [API.md](API.md#transcript-ledger) for the record
shape and what gets dropped.

`PATTERN` is a case-insensitive substring matched against the whole ledger line, so it reaches
the text, the tool name and the kind alike. The search runs on the server: what comes back is
the matching lines, not the corpus.

Columns are `CLONE WHEN KIND SESSION OFFSET TEXT`. The session and offset are there because they
are the two arguments `ledger read` takes, so a hit worth following up is already a command you
can copy. A record's newlines show as `⏎` to keep one record on one row.

`--since`/`--until` take a duration ago (`90m`, `6h`, `2d`, `3w`) or epoch milliseconds. Hits
come back newest first, capped at `--limit` (default 50, server maximum 500); a search that
stopped early says so on stderr.

The ledger holds a session's subagent turns alongside the conversation, and on a session that
delegates heavily the subagents are most of it. `--sidechain` keeps only those, `--no-sidechain`
only the conversation, and `--agent <id>` reads back one subagent's whole run. The id is the
`agentId` on a hit, which `--json` shows.

```
rmng ledger search "va-api" --since 2d
rmng ledger search "SignatureDoesNotMatch" --clone pega-we-142 --json | jq '.hits[].ts'
rmng ledger search "Here is my review" --sidechain --json | jq -r '.hits[].line | fromjson.agentId'
```

### `rmng ledger read <CLONE> <SESSION> [--offset <N>] [--len <N>]`

Print a byte range of one session's ledger, for the conversation around a hit. Pass a hit's own
offset to re-read that line, or less to read what led up to it. The range is snapped outward to
line boundaries, so stdout is always whole NDJSON lines and never a fragment of one.

Default `--offset 0`, `--len 65536`, server maximum 1 MiB. Stdout is the NDJSON alone and the
`bytes A..B of C` envelope goes to stderr, so the pipe works with no flag:

```
rmng ledger read pega-we-142 793f5eac-bbe3-4d3c-b923-29980dcf570d --offset 4096 | jq -r '.kind + ": " + .text'
```

### `rmng desktop <clone> <verb>`

Drive any clone's desktop from an operator machine. The clone id is the first positional;
each verb maps 1:1 to a daemon-MCP tool, forwarded by the control-server to that clone's
daemon MCP (`http://{clone}:9004`). This is the operator-facing replacement for the retired
global MCP — see [MCP.md](MCP.md).

Verbs marked **⤢** also take `[--resolution WxH | --native]` (see "Coordinate space" below).

| Verb | Args | Daemon tool | Does |
| --- | --- | --- | --- |
| `screenshot` ⤢ | `[--monitor N] [--out PATH]` | `screenshot` | JPEG of the monitor's latest frame |
| `monitors` | — | `list_monitors` | `[{id,width,height,native_width,native_height}]` |
| `windows` | — | `list_windows` | open windows (`id,title,wm_class,monitor,frame,…`) |
| `move` ⤢ | `X Y [--monitor N] [--out PATH]` | `mouse_move` | eased glide to `x,y` |
| `click` ⤢ | `[X Y] [--monitor N] [--out PATH]` | `left_click` | optional glide, then left click |
| `right-click` ⤢ | `[X Y] [--monitor N] [--out PATH]` | `right_click` | right click |
| `middle-click` ⤢ | `[X Y] [--monitor N] [--out PATH]` | `middle_click` | middle click |
| `double-click` ⤢ | `[X Y] [--monitor N] [--out PATH]` | `left_double_click` | left double-click |
| `scroll` ⤢ | `AMOUNT [X Y] [--monitor N] [--out PATH]` | `scroll` | `amount` vertical notches |
| `key` | `"ctrl+c" [--out PATH]` | `key` | press a key combo |
| `type` | `"some text" [--out PATH]` | `type` | type a Unicode string |
| `move-window` | `<win-id> [--monitor N] [--mode maximize\|center-half]` | `move_window` | move/place a window |

> To **launch a GUI app** on the clone desktop, use `rmng clone exec -d <clone> -- <app>` (the
> `rmng clone exec` section below) — it runs detached and inherits the clone's desktop session env.

**Screenshot on every action.** Every **action verb** (`move`, `click`, `right-click`,
`middle-click`, `double-click`, `scroll`, `key`, `type`, `move-window`) — plus
`screenshot` itself — always produces a post-action JPEG: the CLI writes it to a file and prints
the file's **absolute path** on stdout (or `{screenshot, text}` under `--json`), so the calling
agent can `Read` it. Most action tools return the daemon's settle-screenshot inline; for tools
whose result carries no image (`type`, `move-window`) the CLI issues a follow-up
`screenshot`. **Query verbs** (`monitors`, `windows`) print their JSON result and take no
screenshot.

- `--monitor N` — which monitor to act on / screenshot (default `0`).
- `--out PATH` — where to write the JPEG. Default `$TMPDIR/rmng-<clone>-mon<N>.jpg`
  (`std::env::temp_dir()`), overwritten each call.

**Coordinate space.** Screenshots come back at **1920×1080** by default (1080p-height,
aspect-preserving) regardless of the monitor's native resolution, and `X Y` are read in that
same space — so you can click straight off the image without converting anything. The daemon
owns the scaling (see [MCP.md](MCP.md)); the CLI just forwards your choice, which means the
image and the coordinates can never disagree.

- `--resolution WxH` — use this space instead, for both the coordinates and the returned image
  (e.g. `--resolution 1280x720` to cut tokens further). Capped at the monitor's native size.
- `--native` — use the monitor's native resolution (e.g. 2560×1440). Mutually exclusive with
  `--resolution`.

Pass the *same* flag to the action and to any screenshot you read coordinates off — each call is
independent, so mixing spaces between calls will misplace clicks. Within one call the action and
its settle screenshot always agree.

```sh
rmng desktop w-cp-claude screenshot          # → prints /tmp/rmng-w-cp-claude-mon0.jpg (1920×1080)
rmng desktop w-cp-claude click 640 480       # click, then prints the settle screenshot path
rmng desktop w-cp-claude screenshot --native # full-res 2560×1440 capture
rmng desktop w-cp-claude click 1707 640 --native   # …and a click in that same native space
rmng desktop w-cp-claude type "hello"        # types, follow-up screenshot, prints path
rmng desktop w-cp-claude windows             # prints JSON, no screenshot
```

### `rmng clone exec <clone> [-u|--user USER] [-w|--workdir DIR] [-e|--env KEY=VAL ...] [-d|--detach] -- <cmd> [args...]`

Run a **single non-interactive** command inside a clone, docker-exec style (no TTY). The
control-server runs it via the Docker exec primitive; `rmng clone ssh` covers interactive sessions.

- `--` separates rmng's own flags from the command argv; everything after it is the command.
- `-u|--user USER` — user to run as. Default **uid `1000`** (the clone's agent user — the
  same account `rmng ssh` lands as).
- `-w|--workdir DIR` — working directory for the command.
- `-e|--env KEY=VAL` — set an env var; **repeatable** (accumulates). Wins over the session env.
- `-d|--detach` — **fire-and-forget**: launch the command in the background and return
  immediately, with no captured output. For GUI apps on the clone desktop (see below). Ignores stdin.
- **Desktop session env (default user):** when running as the agent user, the command inherits the
  clone's live `systemd --user` session env — `WAYLAND_DISPLAY`, `DISPLAY`, `XDG_RUNTIME_DIR`,
  `DBUS_SESSION_BUS_ADDRESS`, the session `PATH` (with `~/.local/bin`), and the agent vars — so GUI
  apps and the in-clone `claude` CLI just work with no `-e`. (A headed clone only; a headless clone
  has no graphical session, so `WAYLAND_DISPLAY`/`DISPLAY` are absent.)
- **stdin passthrough:** a non-terminal stdin is read and forwarded, so
  `echo hi | rmng clone exec c -- cat` works (not in `--detach`).
- Command **stdout → CLI stdout**, **stderr → CLI stderr** (kept separate), and the CLI
  **exits with the command's own exit code** (detached always exits 0 once spawned).
- Global `--json` — emit one `{exit_code, stdout, stderr}` object instead of splitting the
  streams onto stdout/stderr.

```sh
rmng clone exec w-cp-claude -- echo hi                      # stdout "hi", exit 0
rmng clone exec w-cp-claude -w /home/rmng -e FOO=bar -- env # runs `env` with FOO=bar in /home/rmng
echo hi | rmng clone exec w-cp-claude -- cat                # stdin passthrough
rmng clone exec w-cp-claude --json -- false                 # {"exit_code":1,"stdout":"","stderr":""}
rmng clone exec -d w-cp-claude -- gnome-text-editor         # launch a GUI app on the desktop, detached
```

## Wait semantics (`--wait` / `op wait`)

Waiting rides the **`/events` SSE stream**, not polling: the server **prunes** finished ops
from state shortly after they settle (**8 s** after `Done`, **60 s** after `Error` —
`jobs.rs` `PRUNE_DONE_MS`/`PRUNE_ERROR_MS`), so a poll loop could miss the terminal frame
entirely. Every terminal transition is broadcast as a state frame before the prune, so a
subscriber normally sees it. While waiting, a progress line (`[op] step pct% message`) is
printed to stderr whenever the step or whole-percent changes.

- **Done** → exit 0 (`--json`: the terminal `Operation`).
- **Error** → the op's message on stderr, exit 3.
- **Vanished** — the op disappeared without a terminal frame (broadcast-channel lag, an op
  already pruned before the first frame, or the SSE stream ending under a server restart):
  reported as a **warning + exit 0** — overwhelmingly the Done-prune corner.
- **Timeout** → exit 4 (the op may still be running — check `rmng op ls`).

## Seed a new clone

`rmng clone create-plain --title worker --wait`

Repeat `--seed` to copy more directories from the calling clone into the same paths.
The server finishes the copies before the create operation reports success.
Seed directories must sit below `/home/rmng` and must not overlap.
The copy preserves git metadata, uncommitted files, symbolic links, modes, and ownership.
Large trees use up to eight copy workers.
Trees with hardlinks use one worker to preserve links across directories.
The operation log reports the time spent copying seed directories separately from total clone creation time.
The server keeps replaced template directories as `.rmng-seed-backup-N` in the new clone's home.
A failed copy fails the create operation and retains the clone for inspection.
The copy reads live files and does not freeze the source clone.
