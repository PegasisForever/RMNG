# rmng-cli

`rmng` — the fleet-management CLI for the RMNG control-server: hosts (`ps`, `select`),
clones (`clone`, `rm`), images (`image ls|pull|commit|rm`), Claude/Codex accounts
(`account ls|swap`), and operations (`ops`, `wait`), all over the **port-2 web API** via
[control-client](../control-client/README.md). Fleet management only — desktop automation
is the MCP's job ([docs/MCP.md](../../docs/MCP.md)).

- **Build:** `cargo build -p rmng-cli` (package `rmng-cli`, binary `rmng`).
- **Server resolution:** `--server` flag > optional `$RMNG_CONTROL_URL` >
  `http://localhost:9000`. Clones no longer receive that environment variable automatically.
- **In clones:** the control-server injects the binary as `/usr/local/bin/rmng` at
  clone-create time (`provision.rs` `CLONE_BINARIES`; the Dockerfile ships the payload at
  `/usr/local/share/rmng/rmng-cli`), so it's on PATH in every shell.
- **Output:** human tables on stdout; `--json` emits the wire types verbatim; progress and
  prompts go to stderr.

Full reference — every subcommand + flags, the `--json` contract, exit codes, and
`--wait`/`wait` semantics: [docs/CLI.md](../../docs/CLI.md).
