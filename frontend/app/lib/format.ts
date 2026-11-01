// Small display formatters, plus the one session value they read from.
import type { LxcStats } from "~/lib/wire/LxcStats";

/** The operator's locale, for the formatters that take one.
 *
 *  Read it at a container boundary and pass the result down. A leaf that calls this itself
 *  renders differently on every machine and cannot be pinned by a story, which is the whole
 *  reason `ChatView` takes a locale instead of reaching for one. The
 *  fallback exists only because the type cannot say that the callers are client-only. */
export function browserLocale(): string {
  return typeof navigator === "undefined" ? "en-US" : navigator.language;
}

/** Human byte size, e.g. `1.4 GB`. A `sizeBytes` field is typed bigint, but the value
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

/** The whole-container totals as one line: CPU percentage, memory used, physical disk used.
 *  A metric the host could not sample reads as an em-dash rather than a zero, so an
 *  unavailable figure never passes for an idle one. Null in, null out. */
export function formatLxcUsage(
  stats: LxcStats | null,
): { cpu: string; mem: string; disk: string } | null {
  if (!stats) return null;

  const GiB = 1024 ** 3;
  const cpu =
    stats.cpuPct === null
      ? "—"
      : `${stats.cpuPct < 1 ? stats.cpuPct.toFixed(1) : Math.round(stats.cpuPct)}%`;
  const mem = `${(Number(stats.memUsed) / GiB).toFixed(1)}GB`;
  const disk = stats.diskUsed === null ? "—" : `${(Number(stats.diskUsed) / GiB).toFixed(1)}GB`;
  return { cpu, mem, disk };
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

/** Coarse "time until" a future instant, e.g. `2h 15m`, `3d 4h`, `5m`. */
export function until(ms: number): string {
  if (ms <= 0) return "now";
  const mins = Math.round(ms / 60_000);
  if (mins < 60) return `${mins}m`;
  const hours = Math.floor(mins / 60);
  if (hours < 24) return `${hours}h ${mins % 60}m`;
  return `${Math.floor(hours / 24)}d ${hours % 24}h`;
}
