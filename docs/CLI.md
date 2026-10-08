# `rmng` — managing the clone fleet from the command line

`rmng` manages clones, imported accounts and their pools, operations, the board, the
transcript ledger, and any clone's desktop. Everything goes over the control-server's web
API, so it needs neither Docker nor root.

**Inside a clone it is already there**, at `/usr/local/bin/rmng` and on PATH in every shell,
resolving the server from `$RMNG_CONTROL_URL` — a bare `rmng …` just works. The control-server
injects the binary at create time and refreshes it on running clones after a server update.

**Outside the fleet** (an assistant on another machine, an operator laptop) there is no
`$RMNG_CONTROL_URL`, so pass `--server http://<rmng-host>:9000` on every call. Everything
works the same except the verbs that need the caller to be a clone: `clone self` exits 1, a
clone you create is never a sub clone of yours, and `clone ssh` prints the bastion form.
`rmng guide` prints this whole document.

What is not here: the in-clone agent's own desktop automation is the daemon MCP's job
([MCP.md](MCP.md)), the chat panel's assistant is the web API's
([API.md](API.md#per-clone-assistant-chat)), and code moves via git. Source:
[args.rs](../crates/cli/src/args.rs), [commands.rs](../crates/cli/src/commands.rs). Build:
`cargo build -p rmng-cli`.

## Headed vs headless clones

Every clone is one of two kinds, fixed at creation. A **headed** clone (the default) has a
full GUI desktop: `rmng desktop` drives it and the viewer streams it. A **headless** clone
(`--headless` at create) has no desktop, only a terminal — lighter and faster to boot, and
`rmng desktop` does not work on it. Pick headless for pure coding or CLI work, headed only
when the task needs a browser or a GUI.

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
| `clone create <kind>`, `clone fork` | the started `Operation` (the **terminal** `Operation` with `--wait`, plus a `clone` field holding the finished record once it has an address). The ticket kinds add `ticket: { identifier, title, url, opened }`, where `opened` says this run opened the ticket. With `--dry-run`: `{ dryRun, route, request, ticket? }` and nothing is created |
| `clone rm`, `clone rebase`, `clone archive`, `clone restore` | the started `Operation` (the **terminal** `Operation` with `--wait`) |
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
`desktop`, plus `guide`, which prints this document.
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

### Creating clones: `rmng clone create <kind>` and `rmng clone fork`

`rmng clone create` has one kind for each tab of the dashboard's "New clone" dialog. It sends
the same request the dialog sends, so a clone made either way is the same.

| Kind | Dialog tab | What it does |
| --- | --- | --- |
| `create ticket <TICKET>` | Existing ticket | Looks up a Linear ticket (`WE-142` or a Linear link), moves it to In Progress, forks a clone named after it, and sends the ticket to the assistant. |
| `create new-ticket --title <T> [--team <KEY>]` | New ticket | Opens a Linear ticket, then does what `create ticket` does. |
| `create no-ticket --title <T>` | No ticket | Forks a clone under a title of your own. |
| `create template --title <T> --preset <P>` | From template | Builds a clone from a preset's image onto an empty home. No fork. |

`rmng clone fork [SOURCE]` has no dialog tab. It copies a clone as it is: the copy keeps the
source's ticket and is named after the source (`pega-dev-123` → `pega-dev-123a`, then `…b`).
A source has only 27 names, and a retired name is never given out again, so for many copies of
one clone use `create no-ticket --source <clone>` with a different title each time.

**Flags for every kind (and `fork`).** Each is optional. What you leave out, the server
takes from the preset, as it does for the dialog.

- `--group <POOL>`: the account pool both providers draw from, or `none` for every pool.
  Omitted: the preset's pool.
- `--claude-account <A>`, `--codex-account <A>`: an email (pin), `auto`, or `none` (no
  token). Omitted: a fresh pick in the pool.
- `--headless`: no desktop (see [Headed vs headless clones](#headed-vs-headless-clones)).
- `--rebuild`: build the preset image again, with a fresh base pull.
- `--no-startup-script`: do not run the preset's startup script.
- `--column <NAME>`: file the new clone at the **top** of that board column, by title or
  id. The name is checked before anything is created, so a typo costs no clone. Naming an
  archive column files the clone there without archiving it.
- `--wait [--timeout <N>]`: block until the clone is ready. Without it the command prints
  the operation id; follow it with `rmng op wait <op-id>`.
- `--dry-run`: check everything and print the request as JSON, but create nothing: no clone,
  no ticket, no move to In Progress. A ticket is still looked up, so a wrong id shows here.

**Flags for the three forking kinds** (`ticket`, `new-ticket`, `no-ticket`):

- `--source <CLONE>`: the clone whose home is copied (live or archived). Omitted: the clone
  the dashboard's dialog picks: the preset's default fork clone where that clone still
  exists, else the newest live clone. `--dry-run` shows the pick.
- `--parent <CLONE>`: draw the new clone under this clone's card on the dashboard.

**The two ticket kinds.**

- The preset is the one whose label is the ticket's team (`WE-142` → a preset labelled
  `WE`). `--preset <P>` picks another one. With presets configured and none labelled with
  the team, the command stops, as the dialog does.
- The ticket is sent to the assistant once the clone is up. `--no-kickoff` does not send it.
  `--agent-instructions <TEXT>` adds instructions for the assistant, and
  `--claude-instructions <TEXT>` adds instructions it passes on to Claude Code. Both are sent
  with the ticket, so they cannot be used with `--no-kickoff`.
- A ticket already in progress (or in review) is not moved back.
- `new-ticket` takes `--team <KEY>` (omitted: the only team a preset is labelled with),
  `--description <MD>` or `--description-file <PATH>` (`-` for stdin),
  `--priority urgent|high|medium|low`, and `--assignee <email, name, or me>` (omitted: the
  owner of the team's Linear key). The ticket goes into the team's first Todo state.
- If the clone cannot start after `new-ticket` opened its ticket, the error names the ticket.
  Run `rmng clone create ticket <ID>` to try again without a second ticket.
- The Linear calls go from the CLI straight to `api.linear.app`, with the presets' keys from
  `GET /api/config`, the same way the dashboard does. The machine needs internet access.

**The two kinds without a ticket.** `--title` names the clone: `--title 'Fix login'` gives
`<prefix>fix-login`. A second clone with the same title gets the next letter. `--message <M>`
or `--message-file <PATH>` sends the assistant a first message; omitted, nothing is sent.
`no-ticket` and `template` use the first configured preset unless `--preset` names another
one, as the dialog's tabs start on the first preset. `fork` also takes `--message`; when the copy has
a ticket, the ticket link is sent instead of the message.

```sh
rmng clone create ticket WE-142 --wait
rmng clone create new-ticket --team we --title 'Fix the flaky login test' \
  --description-file notes.md --priority high --column 'In Progress'
rmng clone create no-ticket --title 'Try the new parser' --source pega-dev-123 --headless
rmng clone create template --title 'Clean base' --preset work --rebuild --wait
rmng clone fork pega-dev-123 --parent pega-dev-123 --message 'carry on from here'
rmng clone create ticket WE-142 --dry-run   # check first, create nothing
```

### `rmng clone rm <CLONE> [-y|--yes] [--wait] [--timeout <N>]`

Destroy a clone (container + volumes; cascades to its sub clones). Asks `[y/N]` on stderr unless
`-y`; declining exits 1. **Refuses to run non-interactively without `-y`** (stdin not a terminal).

### `rmng clone archive <CLONE>` / `rmng clone restore <CLONE>` `[--wait] [--timeout <N>]`

Stop a managed clone while retaining its container/volumes/notes/chat, then restart it later.
Reversible, no confirmation. The server refuses unknown / unmanaged / already-in-state clones.

### `rmng clone rebase <CLONE> --preset <NAME> [--rebuild] [--wait] [--timeout <N>]`

Swap the system image under a clone while keeping the clone: same id, same home. The clone's
home lives on its own ZFS dataset, not inside the container, so the container can be replaced
without touching it. Use this to pick up a new base image or a preset Dockerfile edit on a
clone you do not want to recreate.

`--preset <NAME>` is **required** — it names the preset whose image the clone moves onto, and
the clone's own preset bindings (accounts, playbook, env) are unchanged by the move. The
preset image is built on first use; `--rebuild` forces a rebuild even when the tag already
exists, which is what you want after editing the preset's Dockerfile, because identical
Dockerfile text never rebuilds on its own.

The server stops the clone, removes the old container, creates a new one from the target image
on the same dataset, and starts it. If the new container fails to come up, it is recreated from
the previous image automatically. Anything the clone held **outside** `/home/rmng` — packages
installed by hand into the running container, files under `/opt` or `/usr/local` — is dropped
without warning; transcribe it into the preset Dockerfile first.

Prints the started op id (follow with `rmng op wait <op-id>`), or blocks with `--wait`
(default timeout 600 s).

    rmng clone rebase pega-dev-123 --preset work --wait
    rmng clone rebase pega-dev-123 --preset work --rebuild --wait

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

### Moving files between clones

There is no copy verb. Every clone already has every other clone's home mounted at
`~/clones/<id>`, so read or copy straight across with ordinary `cp`/`rsync` — no server round
trip, no command. `~/shared` is the fleet-wide drop box, visible to every clone and over SMB.

    cp -a ~/clones/pega-we-142/proj ~/proj

### `rmng clone self`

Print the calling clone's own id, or the whole record with `--json`. Identity is the
per-clone router key in this process's environment, the same proof the server trusts for
sub-clone nesting, so it needs no hostname convention. Outside a clone it prints nothing and
exits 1.

### `rmng clone select <CLONE>` / `rmng clone select --none`

Point the operator's viewer at a clone (`POST /api/activate`); `--none` clears it. **Operator-only
— it does not change which clone your other commands target.** Unknown id errors (exit 1).

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

Every verb that returns a screenshot needs `--resolution`, and the verbs that take `X Y` also
need `--cursor-coordinate-space` (see "Screenshot size and cursor space" below). In the table,
**R** marks `--resolution` and **C** marks `--cursor-coordinate-space`.

| Verb | Args | Daemon tool | Does |
| --- | --- | --- | --- |
| `screenshot` | `R [--monitor N] [--out PATH]` | `screenshot` | JPEG of the monitor's latest frame |
| `monitors` | — | `list_monitors` | `[{id,width,height,native_width,native_height}]` |
| `windows` | — | `list_windows` | open windows (`id,title,wm_class,monitor,frame,…`) |
| `move` | `X Y R C [--monitor N] [--out PATH]` | `mouse_move` | eased glide to `x,y` |
| `click` | `[X Y] R C [--monitor N] [--out PATH]` | `left_click` | optional glide, then left click |
| `right-click` | `[X Y] R C [--monitor N] [--out PATH]` | `right_click` | right click |
| `middle-click` | `[X Y] R C [--monitor N] [--out PATH]` | `middle_click` | middle click |
| `double-click` | `[X Y] R C [--monitor N] [--out PATH]` | `left_double_click` | left double-click |
| `scroll` | `AMOUNT [X Y] R C [--monitor N] [--out PATH]` | `scroll` | `amount` vertical notches, positive is down |
| `key` | `"ctrl+c" R [--out PATH]` | `key` | press a key combo |
| `type` | `"some text" R [--out PATH]` | `type` | type a Unicode string |
| `move-window` | `<win-id> [--monitor N] [--mode maximize\|center-half]` | `move_window` | move/place a window |

> To **launch a GUI app** on the clone desktop, use `rmng clone exec -d <clone> -- <app>` (the
> `rmng clone exec` section below) — it runs detached and inherits the clone's desktop session env.

**Screenshot on every action.** Every **action verb** (`move`, `click`, `right-click`,
`middle-click`, `double-click`, `scroll`, `key`, `type`, `move-window`) — plus
`screenshot` itself — always produces a post-action JPEG: the CLI writes it to a file and prints
the file's **absolute path** on stdout (or `{screenshot, text}` under `--json`), so the calling
agent can `Read` it. **Query verbs** (`monitors`, `windows`) print their JSON result and take no
screenshot.

- `--monitor N` — which monitor to act on / screenshot (default: the first).
- `--out PATH` — where to write the JPEG. Default `$TMPDIR/rmng-<clone>-mon<N>.jpg`
  (`std::env::temp_dir()`), overwritten each call.

**Screenshot size and cursor space.**

- `--resolution <W>x<H>` or `--resolution native` — the size of the screenshot. A screen larger
  than W×H is scaled down, keeping its shape, until it fits inside W×H. A screen that already
  fits is not scaled, and nothing is scaled up. `native` is the screen's own size. Always use
  `--resolution 1920x1080` (1080p) unless you have a special reason. Examples for
  `1920x1080`: a 2560×1440 screen gives 1920×1080, a 3440×1440 ultrawide gives 1920×802, a
  2560×1600 screen gives 1728×1080, and a 1280×720 screen stays 1280×720.
- `--cursor-coordinate-space <W>x<H>` or `--cursor-coordinate-space native` — the units of
  `X Y`. `native` means pixels of the screenshot that `--resolution` gives. `<W>x<H>` lays a
  W×H grid over the whole screenshot: with `999x999`, `0 0` is the top-left corner,
  `999 999` the bottom-right corner, and `499 499` the middle, whatever the screen size. If you
  are Medi GPT, use `--cursor-coordinate-space 999x999`; otherwise use `native`.

Each call stands alone. Pass the same `--resolution` to a screenshot and to the actions whose
`X Y` you read off it. Within one call, the action and its screenshot always agree.

The CLI does this arithmetic itself: it reads the screen size with `list_monitors`, then sends
the daemon an exact screenshot size (never larger than the screen) and `X Y` already in that
size. The daemon's own `resolution` argument and default are described in [MCP.md](MCP.md).

```sh
rmng desktop w-cp-claude screenshot --resolution 1920x1080   # prints /tmp/rmng-w-cp-claude-mon0.jpg
rmng desktop w-cp-claude click 640 480 --resolution 1920x1080 --cursor-coordinate-space native
rmng desktop w-cp-claude click 333 444 --resolution 1920x1080 --cursor-coordinate-space 999x999
rmng desktop w-cp-claude screenshot --resolution native      # the screen's own size, e.g. 2560×1440
rmng desktop w-cp-claude type "hello" --resolution 1920x1080 # types, then prints the screenshot path
rmng desktop w-cp-claude windows                             # prints JSON, no screenshot
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
