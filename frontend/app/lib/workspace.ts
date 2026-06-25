// Shared client-side constants for the four Linear workspaces (ticket prefixes).
// Plain constants (no server code) so both components and routes can import them.
export const WORKSPACE_PREFIXES = ["we", "dev", "hh", "per"] as const;
export type WorkspacePrefix = (typeof WORKSPACE_PREFIXES)[number];

/**
 * Tailwind pill classes per workspace — written as literal strings so the
 * compiler keeps them (no dynamic `bg-${x}` construction). we=blue, dev=orange,
 * hh=green, per=purple.
 */
export const WORKSPACE_BADGE: Record<WorkspacePrefix, string> = {
  we: "bg-blue-100 text-blue-700",
  dev: "bg-orange-100 text-orange-700",
  hh: "bg-green-100 text-green-700",
  per: "bg-purple-100 text-purple-700",
};

export function isWorkspacePrefix(s: string): s is WorkspacePrefix {
  return (WORKSPACE_PREFIXES as readonly string[]).includes(s);
}

/** Extract a `WE-142` ref from a pasted Linear link or bare id, if supported. */
export function parseTicketInput(
  input: string,
): { identifier: string; prefix: WorkspacePrefix; hostname: string } | null {
  const m = /\b([A-Za-z]{2,})-(\d+)\b/.exec(input.trim());
  if (!m) return null;
  const prefix = m[1].toLowerCase();
  if (!isWorkspacePrefix(prefix)) return null;
  return {
    identifier: `${m[1].toUpperCase()}-${m[2]}`,
    prefix,
    hostname: `pega-${prefix}-${m[2]}`,
  };
}
