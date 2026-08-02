// Client-side helpers for Linear ticket prefixes. Any `XX-123`-style id parses. Whether a
// preset's API key can actually reach the ticket is settled by the lookup in
// `~/lib/linear/issues`, which tries every configured key.

/**
 * Badge palette — literal strings so the Tailwind compiler keeps them (no
 * dynamic `bg-${x}` construction).
 */
const PALETTE = [
  "bg-blue-100 text-blue-700 dark:bg-blue-900/40 dark:text-blue-300",
  "bg-orange-100 text-orange-700 dark:bg-orange-900/40 dark:text-orange-300",
  "bg-green-100 text-green-700 dark:bg-green-900/40 dark:text-green-300",
  "bg-purple-100 text-purple-700 dark:bg-purple-900/40 dark:text-purple-300",
  "bg-rose-100 text-rose-700 dark:bg-rose-900/40 dark:text-rose-300",
  "bg-cyan-100 text-cyan-700 dark:bg-cyan-900/40 dark:text-cyan-300",
  "bg-amber-100 text-amber-700 dark:bg-amber-900/40 dark:text-amber-300",
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
