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
  /** JS runtime the SDK uses to run the bundled Claude Code CLI. node is the most battle-tested. */
  executable: (process.env.AGENT_EXECUTABLE as "node" | "bun" | "deno" | undefined) ?? "node",

  /** Per-node desktop MCP (HTTP) — the clone-daemon serves the computer-use tools
   * (screenshot/click/key/type/window-mgmt) locally, sharing its Mutter session. */
  daemonMcpUrl: process.env.DAEMON_MCP_URL ?? "http://127.0.0.1:9004",

  /** control-server per-clone MCP (HTTP) — exposes set_state; resolves THIS host by
   * source IP. The rmng control-server serves it on the clone_mcp port (9002). */
  controlMcpUrl: process.env.AGENT_CONTROL_MCP_URL ?? "http://10.60.0.1:9002",

  /** Graphical-session env (kept for reference; the clone-daemon has its own). */
  runtimeDir,
  dbusAddress: process.env.DBUS_SESSION_BUS_ADDRESS ?? `unix:path=${runtimeDir}/bus`,

  /** Linear hosted MCPs — one per workspace API key. Empty key => that server is skipped. */
  linear: {
    we: process.env.LINEAR_WE_API_KEY ?? "",
    dev: process.env.LINEAR_DEV_API_KEY ?? "",
    hh: process.env.LINEAR_HH_API_KEY ?? "",
    per: process.env.LINEAR_PER_API_KEY ?? "",
  },
} as const;
