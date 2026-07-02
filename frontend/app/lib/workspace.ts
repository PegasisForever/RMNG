// Client-side helpers for Linear ticket prefixes. Any `XX-123`-style id parses —
// whether a preset's API key can actually reach the ticket is decided server-side
// at clone time.

/**
 * Badge palette — literal strings so the Tailwind compiler keeps them (no
 * dynamic `bg-${x}` construction).
 */
const PALETTE = [
  "bg-blue-100 text-blue-700",
  "bg-orange-100 text-orange-700",
  "bg-green-100 text-green-700",
  "bg-purple-100 text-purple-700",
  "bg-rose-100 text-rose-700",
  "bg-cyan-100 text-cyan-700",
  "bg-amber-100 text-amber-700",
] as const;

/** The four original workspaces keep their long-standing colors. */
const FIXED: Record<string, string> = {
  we: PALETTE[0],
  dev: PALETTE[1],
  hh: PALETTE[2],
  per: PALETTE[3],
};

/** Tailwind pill classes for a workspace badge; new names hash into the palette. */
export function workspaceBadge(prefix: string): string {
  const p = prefix.toLowerCase();
  if (FIXED[p]) return FIXED[p];
  let h = 0;
  for (const c of p) h = (h * 31 + c.charCodeAt(0)) >>> 0;
  return PALETTE[h % PALETTE.length];
}

/** Extract a `WE-142`-style ref from a pasted Linear link or bare id. */
export function parseTicketInput(
  input: string,
): { identifier: string; prefix: string; hostname: string } | null {
  const m = /\b([A-Za-z]{2,})-(\d+)\b/.exec(input.trim());
  if (!m) return null;
  const prefix = m[1].toLowerCase();
  return {
    identifier: `${m[1].toUpperCase()}-${m[2]}`,
    prefix,
    hostname: `pega-${prefix}-${m[2]}`,
  };
}
