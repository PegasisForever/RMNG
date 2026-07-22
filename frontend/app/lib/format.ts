// Small display formatters shared by the image UI.

/** Human byte size, e.g. `1.4 GB`. `sizeBytes` on `ImageInfo` is a bigint. */
export function formatBytes(bytes: bigint | number): string {
  let n = typeof bytes === "bigint" ? Number(bytes) : bytes;
  if (!Number.isFinite(n) || n <= 0) return "0 B";
  const units = ["B", "KB", "MB", "GB", "TB"];
  let i = 0;
  while (n >= 1024 && i < units.length - 1) {
    n /= 1024;
    i++;
  }
  return `${n >= 100 || i === 0 ? Math.round(n) : n.toFixed(1)} ${units[i]}`;
}

/** Coarse "time ago" from an ISO timestamp, e.g. `3d ago`, `just now`. */
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

export function relativeAge(iso: string): string {
  const then = Date.parse(iso);
  if (Number.isNaN(then)) return "—";
  const secs = Math.max(0, (Date.now() - then) / 1000);
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
