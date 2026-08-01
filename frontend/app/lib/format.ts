// Small display formatters, plus the one session value they read from.

/** The operator's locale, for the formatters that take one.
 *
 *  Read it at a container boundary and pass the result down. A leaf that calls this itself
 *  renders differently on every machine and cannot be pinned by a story, which is the whole
 *  reason `resetTooltip` and `ChatView` take a locale instead of reaching for one. The
 *  fallback exists only because the type cannot say that the callers are client-only. */
export function browserLocale(): string {
  return typeof navigator === "undefined" ? "en-US" : navigator.language;
}

/** Human byte size, e.g. `1.4 GB`. `sizeBytes` on `ImageInfo` is typed bigint, but the value
 * that arrives can be a plain number (JSON events carry `u64` as one) or a string (an arg
 * edited in Storybook's Controls panel comes back as one), so coerce with `Number` the way
 * `formatTokenCount` does rather than trusting the declared type. */
export function formatBytes(bytes: bigint | number): string {
  let n = Number(bytes);
  if (!Number.isFinite(n) || n <= 0) return "0 B";
  const units = ["B", "KB", "MB", "GB", "TB"];
  let i = 0;
  while (n >= 1024 && i < units.length - 1) {
    n /= 1024;
    i++;
  }
  return `${n >= 100 || i === 0 ? Math.round(n) : n.toFixed(1)} ${units[i]}`;
}

/** Compact integer count, e.g. `12.4k` or `3.1M`. Wire `u64` values are typed as bigint
 * but JSON events arrive as JavaScript numbers, so accept both representations. */
export function formatTokenCount(value: bigint | number): string {
  const n = Number(value);
  if (!Number.isFinite(n) || n <= 0) return "0";
  if (n < 1_000) return Math.floor(n).toLocaleString();
  const units = ["k", "M", "B", "T"];
  let scaled = n;
  let index = -1;
  while (scaled >= 1_000 && index < units.length - 1) {
    scaled /= 1_000;
    index++;
  }
  return `${scaled >= 100 ? Math.round(scaled) : scaled.toFixed(1)}${units[index]}`;
}

/** Coarse "time ago" from an ISO timestamp, e.g. `3d ago`, `just now`.
 *
 *  `now` is passed in rather than read here, for the same reason `resetTooltip` takes one:
 *  a formatter that reads the clock makes its caller a different component on every render,
 *  and no story can pin what it draws. Read the clock at a container boundary. */
export function relativeAge(iso: string, now: number): string {
  const then = Date.parse(iso);
  if (Number.isNaN(then)) return "—";
  const secs = Math.max(0, (now - then) / 1000);
  if (secs < 60) return "just now";
  const mins = secs / 60;
  if (mins < 60) return `${Math.floor(mins)}m ago`;
  const hours = mins / 60;
  if (hours < 24) return `${Math.floor(hours)}h ago`;
  const days = hours / 24;
  if (days < 30) return `${Math.floor(days)}d ago`;
  const months = days / 30;
  if (months < 12) return `${Math.floor(months)}mo ago`;
  return `${Math.floor(days / 365)}y ago`;
}

/** Coarse "time until" a future instant, e.g. `2h 15m`, `3d 4h`, `5m`. */
function until(ms: number): string {
  if (ms <= 0) return "now";
  const mins = Math.round(ms / 60_000);
  if (mins < 60) return `${mins}m`;
  const hours = Math.floor(mins / 60);
  if (hours < 24) return `${hours}h ${mins % 60}m`;
  return `${Math.floor(hours / 24)}d ${hours % 24}h`;
}

/** Usage-bar hover tooltip: the concrete reset time in the viewer's local time zone plus a
 *  countdown, e.g. `Resets Jul 24, 3:45 PM EDT (in 2h 15m)`.
 *
 *  `locale` and `now` are both passed in rather than read here, so the same arguments always
 *  produce the same string: `undefined` for the locale would take the browser's, which turns
 *  one story into "Jul 24, 3:45 PM" on one machine and "24 Jul, 15:45" on the next. The time
 *  ZONE is still the viewer's own, on purpose — a reset time is only useful against the clock
 *  the operator is reading, and `timeZoneName` says which zone that is.
 *
 *  Returns null for a missing or unparseable timestamp so the caller can omit the tooltip
 *  entirely. */
export function resetTooltip(iso: string | null, now: number, locale: string): string | null {
  if (!iso) return null;
  const t = Date.parse(iso);
  if (Number.isNaN(t)) return null;
  const at = new Date(t).toLocaleString(locale, {
    month: "short",
    day: "numeric",
    hour: "numeric",
    minute: "2-digit",
    timeZoneName: "short",
  });
  return t <= now ? `Reset ${at}` : `Resets ${at} (in ${until(t - now)})`;
}
