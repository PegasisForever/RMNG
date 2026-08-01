// The setup wizard's environment preflight, markup half: a list of checks (Docker socket
// reachable, render node present, sock mount, …), each pass or fail, with a Retry that
// re-probes the daemon.
//
// A failed REQUIRED check blocks the wizard and reads red; a failed advisory one reads amber
// and blocks nothing. Both are props here, so the states that need a broken host to reach in
// the real app are one story each. EnvChecklistContainer runs the probe.
import { Check, TriangleAlert, X } from "lucide-react";

import type { EnvCheckRow } from "~/lib/wire/EnvCheckRow";

export function EnvChecklistView({
  rows,
  loading,
  error,
  onRetry,
}: {
  /** The probe's rows, or null before the first answer lands. Null with no error is the
   *  running state; an empty array is a host that reported no checks at all. */
  rows: EnvCheckRow[] | null;
  /** A probe is in flight. */
  loading: boolean;
  /** The probe itself failed, as opposed to a check inside it. */
  error: string | null;
  onRetry: () => void;
}) {
  return (
    <div className="space-y-3">
      <div className="flex items-center justify-between">
        <span className="text-xs font-medium text-slate-500 dark:text-slate-400">
          Environment checks
        </span>
        <button
          type="button"
          onClick={onRetry}
          disabled={loading}
          className="rounded border border-slate-300 px-2 py-1 text-xs text-slate-600 hover:bg-slate-50 disabled:opacity-50 dark:border-slate-600 dark:text-slate-300 dark:hover:bg-slate-800"
        >
          {loading ? "Checking…" : "Retry"}
        </button>
      </div>

      {error ? (
        <div className="rounded border border-red-200 bg-red-50 px-3 py-2 text-xs text-red-700 dark:border-red-900 dark:bg-red-950/40 dark:text-red-400">
          Couldn’t run the environment checks: {error}
        </div>
      ) : null}

      {rows === null && !error ? (
        <p className="py-3 text-center text-xs text-slate-400 dark:text-slate-500">Running checks…</p>
      ) : null}

      {rows ? (
        <ul className="divide-y divide-slate-100 rounded border border-slate-200 dark:divide-slate-800 dark:border-slate-700">
          {rows.map((row) => {
            const failMuted = !row.ok && !row.required; // advisory warning
            return (
              <li key={row.id} className="flex items-start gap-2.5 px-3 py-2">
                <span
                  className={`mt-0.5 flex h-4 w-4 shrink-0 items-center justify-center rounded-full ${
                    row.ok
                      ? "bg-emerald-100 text-emerald-700 dark:bg-emerald-900/40 dark:text-emerald-400"
                      : failMuted
                        ? "bg-amber-100 text-amber-700 dark:bg-amber-900/40 dark:text-amber-400"
                        : "bg-red-100 text-red-700 dark:bg-red-900/40 dark:text-red-400"
                  }`}
                  aria-hidden
                >
                  {row.ok ? (
                    <Check className="size-3" />
                  ) : failMuted ? (
                    <TriangleAlert className="size-3" />
                  ) : (
                    <X className="size-3" />
                  )}
                </span>
                <div className="min-w-0 flex-1">
                  <div className="flex items-center gap-2">
                    <span className="text-sm font-medium text-slate-800 dark:text-slate-100">{row.label}</span>
                    {!row.required ? (
                      <span className="shrink-0 rounded bg-slate-100 px-1.5 py-0.5 text-[10px] font-semibold text-slate-400 dark:bg-slate-800 dark:text-slate-500">
                        optional
                      </span>
                    ) : null}
                  </div>
                  {row.detail ? (
                    <p
                      className={`mt-0.5 text-xs ${
                        row.ok ? "text-slate-400 dark:text-slate-500" : failMuted ? "text-amber-700 dark:text-amber-400" : "text-red-600 dark:text-red-400"
                      }`}
                    >
                      {row.detail}
                    </p>
                  ) : null}
                </div>
              </li>
            );
          })}
        </ul>
      ) : null}

      {rows && rows.some((r) => r.required && !r.ok) ? (
        <p className="text-xs text-red-600 dark:text-red-400">
          Fix the failing required checks, then retry. Setup can’t continue until Docker is
          reachable.
        </p>
      ) : null}
    </div>
  );
}
