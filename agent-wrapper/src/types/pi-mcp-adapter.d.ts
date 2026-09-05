// Narrow declaration for pi-mcp-adapter.
//
// The package ships raw TypeScript as its entry (`index.ts`, loaded through pi's jiti), so
// pointing tsc at the real types drags its whole source tree into our typecheck and fails on
// its own missing @types. A `paths` entry routes the type resolution here instead.
//
// That mapping lives in tsconfig.typecheck.json, NOT tsconfig.json, because bun honors
// `paths` at runtime too and would import this declaration file instead of the real package.

import type { ExtensionAPI } from "@earendil-works/pi-coding-agent";

export interface McpAdapterOptions {
  /** Inline server config. Supplying it disables the adapter's own config-file discovery. */
  config?: Record<string, unknown>;
  configPath?: string;
}

/** Returns a pi extension factory that bridges the configured MCP servers into pi tools. */
export declare function createMcpAdapter(options?: McpAdapterOptions): (pi: ExtensionAPI) => void;
