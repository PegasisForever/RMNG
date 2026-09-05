// Maps the control-server's neutral MCP descriptor (~/.config/rmng/mcp.json — the single
// source of truth, already headless-filtered) to the config shape pi-mcp-adapter takes.
// The other agents (Claude CLI / Codex / Cursor) get the same set rendered into their own
// config files by the control-server; this is the node-agent's consumer of that source.
//
// pi has no built-in MCP support, so the adapter is what turns these servers into tools the
// model can call. See agent-wrapper/README.md.

/** One entry in the descriptor JSON array written by the control-server. */
export interface McpDescriptor {
  name: string;
  url: string;
  /** When set, authenticate with `Authorization: Bearer <process.env[bearerEnv]>`. */
  bearerEnv?: string;
  /** node-agent hint: keep this server's tools in context every turn (e.g. `desktop`). */
  alwaysLoad?: boolean;
}

/** One server in the adapter's config. */
export interface McpAdapterServer {
  url: string;
  /** `eager` connects at startup instead of on first use. */
  lifecycle?: "eager" | "lazy";
  /** Promote this server's tools to first-class pi tools instead of hiding them behind the proxy. */
  directTools?: boolean;
  headers?: Record<string, string>;
}

export interface McpAdapterConfig {
  mcpServers: Record<string, McpAdapterServer>;
  /** The adapter's config accepts more keys than the wrapper sets (settings, oauth, …). */
  [key: string]: unknown;
}

/**
 * Build the adapter config from the descriptor entries. A server whose `bearerEnv` is set
 * but empty in the environment is skipped (e.g. `linear` on a clone with no `LINEAR_API_KEY`),
 * matching the behavior of the file-based agents (which only auth when the key is present).
 *
 * `alwaysLoad` becomes `directTools` plus an eager connection. The old flag kept a server's
 * tools in context every turn; promoting them to real pi tools is the closest equivalent, and
 * it is what the desktop server needs so screenshot and click are always callable.
 */
export function mcpConfigFromDescriptor(
  entries: McpDescriptor[],
  env: Record<string, string | undefined> = process.env,
): McpAdapterConfig {
  const mcpServers: Record<string, McpAdapterServer> = {};
  for (const e of entries) {
    if (!e || typeof e.name !== "string" || typeof e.url !== "string" || !e.name || !e.url) {
      continue;
    }
    let headers: Record<string, string> | undefined;
    if (e.bearerEnv) {
      const key = env[e.bearerEnv] ?? "";
      if (!key) continue; // no key ⇒ omit the server rather than register an unauthenticated one
      headers = { Authorization: `Bearer ${key}` };
    }
    mcpServers[e.name] = {
      url: e.url,
      ...(e.alwaysLoad ? { lifecycle: "eager" as const, directTools: true } : {}),
      ...(headers ? { headers } : {}),
    };
  }
  return { mcpServers };
}
