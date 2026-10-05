// Maps the control-server's neutral MCP descriptor (~/.config/rmng/mcp.json — the single
// source of truth, already headless-filtered) to the server entries pi's built-in MCP
// extension takes. The other agents (Claude CLI / Codex / Cursor) get the same set rendered
// into their own config files by the control-server; this is the pi agent's consumer of that
// source.
//
// pi connects the servers itself and names their tools `mcp__<server>__<tool>`. See
// agent-wrapper/README.md.

import type { McpServerConfig } from "@earendil-works/pi-coding-agent";

/** One entry in the descriptor JSON array written by the control-server. */
export interface McpDescriptor {
        name: string;
        url: string;
        /** When set, authenticate with `Authorization: Bearer <process.env[bearerEnv]>`. */
        bearerEnv?: string;
        /** Declare this server's tools to the model directly (e.g. `desktop`), so screenshot and
         *  click are callable on the first turn without a search. */
        directTools?: boolean;
        /** Written by the control-server alongside `directTools`. pi connects every server when
         *  the session starts and waits for `direct` ones before the first prompt, so it adds
         *  nothing here and is ignored. */
        lifecycle?: "eager" | "lazy";
}

/**
 * Build pi's server configs from the descriptor entries. A server whose `bearerEnv` is set
 * but empty in the environment is skipped (e.g. `linear` on a clone with no `LINEAR_API_KEY`),
 * matching the behavior of the file-based agents (which only auth when the key is present).
 *
 * A `directTools` server gets `direct` exposure: its tools are declared to the model like
 * built-in ones. Every other server gets `deferred`: its tools stay out of the declarations
 * until pi's `tool_search` loads a match, which keeps a large server such as Linear from
 * filling every request.
 */
export function mcpServersFromDescriptor(
        entries: McpDescriptor[],
        env: Record<string, string | undefined> = process.env,
): Record<string, McpServerConfig> {
        const servers: Record<string, McpServerConfig> = {};
        for (const e of entries) {
                if (
                        !e ||
                        typeof e.name !== "string" ||
                        typeof e.url !== "string" ||
                        !e.name ||
                        !e.url
                ) {
                        continue;
                }
                let headers: Record<string, string> | undefined;
                if (e.bearerEnv) {
                        const key = env[e.bearerEnv] ?? "";
                        if (!key) continue; // no key ⇒ omit the server rather than register an unauthenticated one
                        headers = { Authorization: `Bearer ${key}` };
                }
                servers[e.name] = {
                        url: e.url,
                        exposure: e.directTools ? "direct" : "deferred",
                        ...(headers ? { headers } : {}),
                };
        }
        return servers;
}
