# `rmng` CLI reference — fleet management over the web port

The `rmng` binary ([crates/cli](../crates/cli/README.md), package `rmng-cli`) is the fleet
management surface: hosts, clones, images, Claude/Codex accounts, and operations, all over
the control-server's **port-2 web API** (via [control-client](../crates/control-client/README.md)).
It also carries the **operator/fleet desktop control** (`rmng desktop`, folded in from the
retired global MCP) and a docker-exec-style **`rmng exec`** — both reach clones through the
same web API, which proxies to the clone's daemon MCP / Docker exec. What stays elsewhere:
the **in-clone** agent's own desktop automation is the daemon MCP's job ([MCP.md](MCP.md)),
host-agent chat is the web API's (`/api/chat/:id`, [API.md](API.md#per-host-agent-chat)), and
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
`CLONE_BINARIES` copies it in before the container boots. Create-time injection is the
**sole delivery path** (the binswap hot-swap engine is retired), so clones created *before*
a server update don't have it — only clones created after.

## Server resolution

`--server <URL>` > `$RMNG_CONTROL_URL` > `http://localhost:9000`. Every clone gets
`RMNG_CONTROL_URL` preset in its session env (pointing at this control-server's web port),
so inside a clone `rmng ps` just works. Blank values fall through; a trailing `/` is
stripped. A connection failure prints the resolved base with a `set --server or
$RMNG_CONTROL_URL` hint.

## Global flags & output

- `--server <URL>` — control-server web-API origin (e.g. `http://rmng-control:9000`).
- `--json` — emit the raw **wire JSON verbatim** (pretty-printed) instead of a table. The
  shapes are the [`wire`](../crates/wire/src/control.rs) types exactly — no CLI-specific
  schema. Progress lines, prompts, and warnings go to **stderr**, so stdout stays clean for
  piping.

| Command (with `--json`) | Emits |
|---|---|
| `ps`, `select` | `ControlState` |
| `clone`, `rm`, `image pull`, `image commit` | the started `Operation` (the **terminal** `Operation` with `--wait`) |
| `wait` | the terminal `Operation` |
| `ops` | `Operation[]` |
| `image ls` | `ImageInfo[]` |
| `account ls` | `ClaudeUsage[]` |
| `account swap` | the API reply `{ok, account, group, selection}` |
| `image rm` | `{ok: true}` |

## Exit codes

| Code | Meaning |
|---|---|
| `0` | ok (including a "vanished" wait — see below) |
| `1` | API / transport error (also: `rm` confirmation declined) |
| `2` | usage error (clap) |
| `3` | the waited-on operation ended in **Error** |
| `4` | `--wait` / `wait` timed out |

## Commands

### `rmng ps`
Hosts table: `ID` (a `*` suffix marks the selected host), `STATE` (monitor verdict:
working/idle/offline), `AGENT` (the agent's self-report), `IMAGE` (source reference),
`CLAUDE` / `CODEX` (assigned account email, else the stored selection), `NOTE` (the agent's
state note, truncated).

### `rmng select <host|none>`
Point the operator's viewer at a host (`POST /api/activate`); `none` clears the selection.
An unknown host id errors (exit 1) with a pointer to `rmng ps`.

### `rmng clone --image <REF> --hostname <H> [--claude <SEL>] [--codex <SEL>] [--preset <P>] [--wait] [--timeout <N>]`
Create a clone under an **exact hostname** (a DNS label; `400` if taken) — the `POST
/api/clone` hostname mode: no ticket, no kickoff message. `--claude` / `--codex` take an
email, `auto`, `group:<name>`, or `none`; `--preset` names an env preset (optional — fleet
workers usually need none). Prints the started op id (follow with `rmng wait <op-id>`), or
blocks until done with `--wait`.

```sh
rmng clone --image pegasis0/rmng-template:latest --hostname w-cp-claude --claude auto --wait
```

### `rmng rm <host> [-y|--yes] [--wait] [--timeout <N>]`
Destroy a clone (container + volumes). Asks `[y/N]` on stderr unless `--yes`; declining
exits 1. **Refuses to run non-interactively without `--yes`** (stdin not a terminal).

### `rmng image ls|pull|commit|rm`
- `image ls` — clone-source images: `REFERENCE ID SIZE CREATED BASE FROM IN-USE-BY`.
- `image pull [reference] [--wait]` — pull the clone template from a registry; no reference
  = the configured `docker.templateReference`.
- `image commit <host> <name> [--wait]` — commit a running clone to a new clone-source
  image `<name>:latest`.
- `image rm <reference>` — remove a clone-source image (`409` while clones use it).

### `rmng account ls [--claude|--codex]`
Imported accounts with usage windows: `EMAIL PROVIDER ASSIGNABLE 5H 5H-RESETS 7D ERROR`.
Both providers by default; the flags conflict. Human output appends the configured
Claude/Codex account groups (from `GET /api/config`).

### `rmng account swap <host> <account> [--codex]`
Hot-swap a running clone's account (`POST /api/{claude,codex}/swap`). `<account>` is an
email, `auto`, `group:<name>`, or `none`; Claude by default, `--codex` for Codex.

### `rmng ops`
The current `operations[]`: in-flight + recently-finished clone/delete/pull/commit/update
jobs (`ID KIND TARGET STATUS STEP PCT MESSAGE`). Finished ops are pruned quickly — see below.

### `rmng wait <op-id> [--timeout <N>]`
Block until an operation reaches a terminal state (default timeout 600 s). Same semantics
as `--wait` on the starting command.

### `rmng desktop <clone> <verb>` (alias `dt`)
Drive any clone's desktop from an operator machine. The clone id is the first positional;
each verb maps 1:1 to a daemon-MCP tool, forwarded by the control-server to that clone's
daemon MCP (`http://{clone}:9004`). This is the operator-facing replacement for the retired
global MCP — see [MCP.md](MCP.md).

| Verb | Args | Daemon tool | Does |
|---|---|---|---|
| `screenshot` | `[--monitor N] [--out PATH]` | `screenshot` | JPEG of the monitor's latest frame |
| `monitors` | — | `list_monitors` | `[{id,width,height}]` |
| `windows` | — | `list_windows` | open windows (`id,title,wm_class,monitor,frame,…`) |
| `apps` | — | `list_apps` | installed launcher apps |
| `move` | `X Y [--monitor N] [--out PATH]` | `mouse_move` | eased glide to `x,y` |
| `click` | `[X Y] [--monitor N] [--out PATH]` | `left_click` | optional glide, then left click |
| `rclick` | `[X Y] [--monitor N] [--out PATH]` | `right_click` | right click |
| `mclick` | `[X Y] [--monitor N] [--out PATH]` | `middle_click` | middle click |
| `dclick` | `[X Y] [--monitor N] [--out PATH]` | `left_double_click` | left double-click |
| `scroll` | `AMOUNT [X Y] [--monitor N] [--out PATH]` | `scroll` | `amount` vertical notches |
| `key` | `"ctrl+c" [--out PATH]` | `key` | press a key combo |
| `type` | `"some text" [--out PATH]` | `type` | type a Unicode string |
| `launch` | `firefox.desktop [--out PATH]` | `launch_app` | launch an app by `.desktop` id |
| `movewin` | `<win-id> [--monitor N] [--mode maximize\|center-half] [--out PATH]` | `move_window` | move/place a window |

**Screenshot on every action.** Every **action verb** (`move`, `click`, `rclick`, `mclick`,
`dclick`, `scroll`, `key`, `type`, `launch`, `movewin`) — plus `screenshot` itself — always
produces a post-action JPEG: the CLI writes it to a file and prints the file's **absolute
path** on stdout, so the calling agent can `Read` it. Most action tools return the daemon's
settle-screenshot inline; for tools whose result carries no image (`type`, `launch`,
`movewin`) the CLI issues a follow-up `screenshot` (monitor `0` or `--monitor N`) so the
guarantee holds uniformly, printing any text/JSON result before the path. **Query verbs**
(`monitors`, `windows`, `apps`) print their JSON result and take no screenshot.

- `--monitor N` — which monitor to act on / screenshot (default `0`).
- `--out PATH` — where to write the JPEG. Default `$TMPDIR/rmng-<clone>-mon<N>.jpg`
  (`std::env::temp_dir()`), overwritten each call.

```sh
rmng desktop w-cp-claude screenshot          # → prints /tmp/rmng-w-cp-claude-mon0.jpg
rmng dt w-cp-claude click 640 480            # click, then prints the settle screenshot path
rmng dt w-cp-claude type "hello"             # types, follow-up screenshot, prints path
rmng dt w-cp-claude windows                  # prints JSON, no screenshot
```

### `rmng exec <clone> [-u|--user USER] [-w|--workdir DIR] [-e|--env KEY=VAL ...] -- <cmd> [args...]`
Run a **single non-interactive** command inside a clone, docker-exec style (no TTY). The
control-server runs it via the Docker exec primitive; `rmng ssh` covers interactive sessions.

- `--` separates rmng's own flags from the command argv; everything after it is the command.
- `-u|--user USER` — user to run as. Default **uid `1000`** (the clone's agent user — the
  same account `rmng ssh` lands as).
- `-w|--workdir DIR` — working directory for the command.
- `-e|--env KEY=VAL` — set an env var; **repeatable** (accumulates).
- **stdin passthrough:** a non-terminal stdin is read and forwarded, so
  `echo hi | rmng exec c -- cat` works.
- Command **stdout → CLI stdout**, **stderr → CLI stderr** (kept separate), and the CLI
  **exits with the command's own exit code**.
- Global `--json` — emit one `{exit_code, stdout, stderr}` object instead of splitting the
  streams onto stdout/stderr.

```sh
rmng exec w-cp-claude -- echo hi                      # stdout "hi", exit 0
rmng exec w-cp-claude -w /home/rmng -e FOO=bar -- env # runs `env` with FOO=bar in /home/rmng
echo hi | rmng exec w-cp-claude -- cat                # stdin passthrough
rmng exec w-cp-claude --json -- false                 # {"exit_code":1,"stdout":"","stderr":""}
```

## Wait semantics (`--wait` / `wait`)

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
- **Timeout** → exit 4 (the op may still be running — check `rmng ops`).
