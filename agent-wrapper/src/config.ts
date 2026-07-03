// Wrapper configuration, all from the environment so the systemd unit (and CoW
// clones inheriting it) is the single source of truth. Everything has a sane
// default so `bun run src/server.ts` works on a fresh container with no env.

function uid(): number {
  try {
    return process.getuid?.() ?? 1000;
  } catch {
    return 1000;
  }
}

const runtimeDir = process.env.XDG_RUNTIME_DIR ?? `/run/user/${uid()}`;

export const CONFIG = {
  /** HTTP listen port — the control-server connects to http://<container-ip>:<port>. */
  port: Number(process.env.AGENT_PORT ?? 4096),
  /** Claude model id. The container's logged-in `claude` subscription is used for auth. */
  model: process.env.AGENT_MODEL ?? "claude-opus-4-8",
  /** JS runtime the SDK uses to run the bundled Claude Code CLI. Ignored when
   * `claudeExecutable` points at a standalone (native) binary — the SDK spawns it directly. */
  executable: (process.env.AGENT_EXECUTABLE as "node" | "bun" | "deno" | undefined) ?? "node",

  /** Path to the Claude Code CLI the SDK spawns. We ship this wrapper as a
   * `bun build --compile` single-exec, so the SDK can't resolve its own bundled
   * `cli.js` (it lives in the bunfs virtual FS, and the optional native-CLI package
   * isn't compiled in) — `query()` throws "Native CLI binary … not found". The clone
   * has the standalone Claude Code installed at ~/.local/bin/claude (see
   * provision-clone.sh); point the SDK at it. A non-`.js` path ⇒ the SDK spawns it as
   * a native binary directly (no node needed), which matches the standalone install. */
  claudeExecutable: process.env.CLAUDE_CODE_BIN ?? `${process.env.HOME ?? "/home/rmng"}/.local/bin/claude`,

  /** Per-node desktop MCP (HTTP) — the clone-daemon serves the computer-use tools
   * (screenshot/click/key/type/window-mgmt) locally, sharing its Mutter session. */
  daemonMcpUrl: process.env.DAEMON_MCP_URL ?? "http://127.0.0.1:9004",

  /** control-server per-clone MCP (HTTP) — exposes set_state; resolves THIS host by
   * source IP. The rmng control-server serves it on the clone_mcp port (9002). */
  controlMcpUrl: process.env.AGENT_CONTROL_MCP_URL ?? "http://10.60.0.1:9002",

  /** Graphical-session env (kept for reference; the clone-daemon has its own). */
  runtimeDir,
  dbusAddress: process.env.DBUS_SESSION_BUS_ADDRESS ?? `unix:path=${runtimeDir}/bus`,

  /** Linear hosted MCP — one server; the key is the clone's preset Linear key,
   * injected as LINEAR_API_KEY at clone creation. Empty => the server is skipped. */
  linearApiKey: process.env.LINEAR_API_KEY ?? "",

  /** Editable agent playbook injected by the control-server at clone creation. The wrapper
   * reads this at startup; absent ⇒ the baked-in default (see instructions.ts). */
  instructionsPath:
    process.env.AGENT_INSTRUCTIONS_PATH ??
    `${process.env.HOME ?? "/home/rmng"}/.config/rmng/agent-instructions.md`,
} as const;
