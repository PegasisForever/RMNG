# rmng-cli

`rmng` — the fleet-management CLI for the RMNG control-server. Uniform **noun → verb**:
`rmng <noun> <verb> [<clone>] [flags]`. Nouns: `clone` (the fleet unit — `ls`, `create`, `rm`,
`archive`, `restore`, `ssh`, `exec`, `bind`, `select`), `image` (`ls|pull|commit|rm`), `account`
(`ls`), `op` (`ls|wait`), and `desktop <clone> <verb>` (computer use). All over the **port-2 web
API** via [control-client](../control-client/README.md).

- **Build:** `cargo build -p rmng-cli` (package `rmng-cli`, binary `rmng`).
- **Server resolution:** `--server` flag > `$RMNG_CONTROL_URL` > `http://localhost:9000`. The
  control-server sets `RMNG_CONTROL_URL` in every clone's environment, so a bare `rmng …` inside
  a clone auto-resolves the server — no `--server` needed.
- **In clones:** the control-server injects the binary as `/usr/local/bin/rmng` at clone-create
  time (`provision.rs` `CLONE_BINARIES`; the Dockerfile ships the payload at
  `/usr/local/share/rmng/rmng-cli`), so it's on PATH in every shell.
- **Output:** human tables/prose on stdout; `--json` is honored by **every** command (errors are
  JSON too, on stderr). `rmng clone ls --json` returns one object per clone with `stats` +
  `tokens` nested (the metrics the table shows). Progress and prompts go to stderr.

Full reference — every subcommand + flags, the `--json` contract, exit codes, and
`--wait`/`op wait` semantics: [docs/CLI.md](../../docs/CLI.md).
