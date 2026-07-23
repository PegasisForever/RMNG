// Maps the control-server's neutral MCP descriptor (~/.config/rmng/mcp.json — the single
// source of truth, already headless-filtered) to the Claude Agent SDK's `mcpServers` shape.
// The other agents (Claude CLI / Codex / OpenCode) get the same set rendered into their own
// config files by the control-server; this is the node-agent's consumer of that source.

import type { McpServerConfig } from "@anthropic-ai/claude-agent-sdk";

/** One entry in the descriptor JSON array written by the control-server. */
export interface McpDescriptor {
  name: string;
  url: string;
  /** When set, authenticate with `Authorization: Bearer <process.env[bearerEnv]>`. */
  bearerEnv?: string;
  /** node-agent hint: keep this server's tools in context every turn (e.g. `desktop`). */
  alwaysLoad?: boolean;
}

/**
 * Build the SDK `mcpServers` map from the descriptor entries. A server whose `bearerEnv` is set
 * but empty in the environment is skipped (e.g. `linear` on a clone with no `LINEAR_API_KEY`),
 * matching the behavior of the file-based agents (which only auth when the key is present).
 */
export function mcpServersFromDescriptor(
  entries: McpDescriptor[],
  env: Record<string, string | undefined> = process.env,
): Record<string, McpServerConfig> {
  const servers: Record<string, McpServerConfig> = {};
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
    servers[e.name] = {
      type: "http",
      url: e.url,
      ...(e.alwaysLoad ? { alwaysLoad: true } : {}),
      ...(headers ? { headers } : {}),
    };
  }
  return servers;
}
