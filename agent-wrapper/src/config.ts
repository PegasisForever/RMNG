// Wrapper configuration, all from the environment so the systemd unit (and CoW
// clones inheriting it) is the single source of truth. Everything has a sane
// default so `bun run src/server.ts` works on a fresh container with no env.

import { existsSync } from "node:fs";

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
  /** Model for the session, resolved against pi's built-in `openai-codex` catalog. Fixed on
   * purpose: the fleet runs one model, and the clone's pushed Codex token is what authorizes
   * it. Qualified with the provider so a same-named model on another provider can't win. */
  model: "openai-codex/gpt-5.6-luna",

  /** Reasoning effort. pi maps this through the model's thinkingLevelMap to
   * `reasoning.effort`. The Fast speed tier rides separately, see serviceTier.ts. */
  thinkingLevel: "xhigh",

  /** The Codex credential the control-server pushes (codex.rs `apply_clone_token`). Read on
   * every request so a rotated token lands without a restart. See auth.ts. */
  codexAuthPath: process.env.CODEX_AUTH_PATH ?? `${process.env.HOME ?? "/home/rmng"}/.codex/auth.json`,

  /** pi's own config directory. Kept inside the clone's home so sessions, settings, and any
   * operator-installed pi packages survive a wrapper restart. */
  agentDir: process.env.PI_CODING_AGENT_DIR ?? `${process.env.HOME ?? "/home/rmng"}/.pi/agent`,

  /** Per-node desktop MCP (HTTP) — the clone-daemon serves the computer-use tools
   * (screenshot/click/key/type/window-mgmt) locally, sharing its Mutter session. */
  daemonMcpUrl: process.env.DAEMON_MCP_URL ?? "http://127.0.0.1:9004",

  /** A headless clone has no desktop: the control-server DELETES both gnome-headless.service and
   * rmng-clone-daemon.service at create time (control-server `provision.rs` HEADLESS_DISABLE_SCRIPT),
   * so nothing serves the desktop MCP on :9004. Detect that by the absence of the clone-daemon user
   * unit — a create-time-stable signal (unlike a TCP probe, it can't misfire during the boot race
   * before the daemon has bound its port). When headless, `mcpConfig()` skips the `desktop` server
   * so the adapter doesn't eagerly connect to a dead endpoint. */
  headless: !existsSync(
    `${process.env.HOME ?? "/home/rmng"}/.config/systemd/user/rmng-clone-daemon.service`,
  ),

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

  /** The control-server-written MCP descriptor — the single source of truth for the managed
   * server set (`desktop`+`linear`), already headless-filtered. The wrapper reads this at
   * startup and maps it to pi-mcp-adapter's config; absent ⇒ the built-in fallback in server.ts. */
  mcpConfigPath:
    process.env.RMNG_MCP_CONFIG_PATH ??
    `${process.env.HOME ?? "/home/rmng"}/.config/rmng/mcp.json`,
} as const;
