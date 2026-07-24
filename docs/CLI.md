# `rmng` CLI reference — fleet management over the web port

The `rmng` binary ([crates/cli](../crates/cli/README.md), package `rmng-cli`) is the fleet
management surface: clones, images, account groups and imported accounts, and operations, all over
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
`~/.codex/AGENTS.md` and `~/.codex/config.toml`: Codex gets the same disposable-sandbox
guidance, the local desktop daemon MCP (`desktop`), and Linear (`linear`, using
`LINEAR_API_KEY`). Its model requests route through the control-server's clone-specific
CLIProxyAPI endpoint. The clone reconciler refreshes those files on old running clones.

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
|---|---|
| `clone ls` | `{ selected, clones: [Clone + {stats, tokens}], operations }` (CLI shape — includes the metrics the table shows) |
| `clone select`, `clone bind` | small status object (`{selected}` / the `{ok, group}` reply) |
| `clone ssh` | `{ command, mode: "direct"\|"bastion" }` |
| `clone create`, `clone rm`, `clone archive`, `clone restore`, `image pull`, `image commit` | the started `Operation` (the **terminal** `Operation` with `--wait`) |
| `op wait` | the terminal `Operation` |
| `op ls` | `Operation[]` |
| `image ls` | `ImageInfo[]` |
| `account ls` | `ClaudeUsage[]` |
| `image rm` | `{ok: true}` |
| `desktop` (screenshot/action) | `{ screenshot: <path>, text? }`; query verbs → the tool's JSON |

## Exit codes

| Code | Meaning |
|---|---|
| `0` | ok (including a "vanished" wait — see below) |
| `1` | API / transport error (also: `rm` confirmation declined) |
| `2` | usage error (clap) |
| `3` | the waited-on operation ended in **Error** |
| `4` | `--wait` / `op wait` timed out |

## Commands

The surface is **noun → verb**. Nouns: `clone`, `image`, `account`, `op`, `desktop`. The target
is always a positional **clone id** (the first column of `rmng clone ls`).

### `rmng clone ls`
Clones table: `ID` (a `*` suffix marks the selected clone), `IP` (the current Docker bridge
address when available), `IMAGE` (source reference), `PRESET`, `GROUP` (its CLIProxyAPI account
pool), live `CPU` and `RAM`, cumulative `TOK-IN` / `TOK-OUT`, and lifecycle `STATUS`. Sub clones
are indented under their parent. CPU/RAM are volatile snapshots for sampled active managed clones.
`rmng clone ls --json` returns the CLI shape `{ selected, clones: [Clone + {stats, tokens}],
operations }` — so the metrics the table shows are available to a machine reader too.

### `rmng clone create <HOSTNAME> --from <IMAGE> [--group <G>|--no-group] [--preset <P>|--no-preset] [--headless] [--parent <C>|--top-level] [--wait] [--timeout <N>]`
Create a clone under an **exact hostname** (a DNS label; `400` if taken). **Run from inside a
clone, the new clone auto-nests as a sub clone under the caller AND inherits the caller's account
group + env preset by default.** Overrides: `--group <name>`/`--no-group`, `--preset
<name>`/`--no-preset`, `--parent <clone>` (nest under a specific top-level clone), `--top-level`
(force top-level, skipping inheritance). `--from` names the clone-source image (`rmng image ls`).
Prints the started op id (follow with `rmng op wait <op-id>`), or blocks with `--wait`.

```sh
rmng clone create w-cp --from pegasis0/rmng-template:latest --wait
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

### `rmng clone bind <CLONE> <GROUP>` / `rmng clone bind <CLONE> --none`
(Re)bind a clone to one provider-agnostic account group (`POST /api/hosts/:id/group`), or clear
it with `--none`. Pure routing change; account onboarding/refresh stays frontend/API.

### `rmng clone select <CLONE>` / `rmng clone select --none`
Point the operator's viewer at a clone (`POST /api/activate`); `--none` clears it. **Operator-only
— it does not change which clone your other commands target.** Unknown id errors (exit 1).

### `rmng image ls|pull|commit|rm`
- `image ls` — clone-source images: `REFERENCE ID SIZE CREATED BASE FROM IN-USE-BY`.
- `image pull [reference] [--wait]` — pull the clone template; no reference = the configured
  `docker.templateReference`.
- `image commit <CLONE> --as <NAME> [--wait]` — commit a running clone to `<name>:latest`.
- `image rm <reference>` — remove a clone-source image (`409` while clones use it).

### `rmng account ls [--provider claude|codex|gemini]`
Read-only listing of imported accounts and usage windows: `GROUP EMAIL PROVIDER ASSIGNABLE 5H
5H-RESETS 7D FABLE ERROR`. All providers by default; `--provider` filters to one. Gemini
(Antigravity) can be a presence-only row (its upstream exposes no pollable quota).

### `rmng op ls`
The current `operations[]`: in-flight + recently-finished clone/delete/archive/restore/pull/
commit/update jobs (`ID KIND TARGET STATUS STEP PCT MESSAGE`). Finished ops are pruned quickly.

### `rmng op wait <op-id> [--timeout <N>]`
Block until an operation reaches a terminal state (default timeout 600 s). Same semantics as
`--wait` on the starting command.

### `rmng desktop <clone> <verb>`
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
| `right-click` | `[X Y] [--monitor N] [--out PATH]` | `right_click` | right click |
| `middle-click` | `[X Y] [--monitor N] [--out PATH]` | `middle_click` | middle click |
| `double-click` | `[X Y] [--monitor N] [--out PATH]` | `left_double_click` | left double-click |
| `scroll` | `AMOUNT [X Y] [--monitor N] [--out PATH]` | `scroll` | `amount` vertical notches |
| `key` | `"ctrl+c" [--out PATH]` | `key` | press a key combo |
| `type` | `"some text" [--out PATH]` | `type` | type a Unicode string |
| `launch` | `firefox.desktop` | `launch_app` | launch an app by `.desktop` id |
| `move-window` | `<win-id> [--monitor N] [--mode maximize\|center-half]` | `move_window` | move/place a window |

**Screenshot on every action.** Every **action verb** (`move`, `click`, `right-click`,
`middle-click`, `double-click`, `scroll`, `key`, `type`, `launch`, `move-window`) — plus
`screenshot` itself — always produces a post-action JPEG: the CLI writes it to a file and prints
the file's **absolute path** on stdout (or `{screenshot, text}` under `--json`), so the calling
agent can `Read` it. Most action tools return the daemon's settle-screenshot inline; for tools
whose result carries no image (`type`, `launch`, `move-window`) the CLI issues a follow-up
`screenshot`. **Query verbs** (`monitors`, `windows`, `apps`) print their JSON result and take no
screenshot.

- `--monitor N` — which monitor to act on / screenshot (default `0`).
- `--out PATH` — where to write the JPEG. Default `$TMPDIR/rmng-<clone>-mon<N>.jpg`
  (`std::env::temp_dir()`), overwritten each call.

```sh
rmng desktop w-cp-claude screenshot          # → prints /tmp/rmng-w-cp-claude-mon0.jpg
rmng desktop w-cp-claude click 640 480       # click, then prints the settle screenshot path
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
