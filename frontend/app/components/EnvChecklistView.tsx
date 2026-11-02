// The setup wizard's environment preflight, markup half: a list of checks (Docker socket
// reachable, render node present, sock mount, …), each pass or fail, with a Retry that
// re-probes the daemon.
//
// A failed REQUIRED check blocks the wizard and reads red; a failed advisory one reads amber
// and blocks nothing. Both are props here, so the states that need a broken host to reach in
// the real app are one story each. EnvChecklistContainer runs the probe.
import { Check, TriangleAlert, X } from "lucide-react";

import { BUTTON, DIVIDE, ERROR_TEXT, ErrorLine, LINE, R_PIECE, TEXT, TEXT_2, TEXT_3 } from "~/desk/control/ui";
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
        <span className={`text-xs font-medium ${TEXT_2}`}>
          Environment checks
        </span>
        <button
          type="button"
          onClick={onRetry}
          disabled={loading}
          className={`${BUTTON} px-2 py-1 text-xs disabled:opacity-40`}
        >
          {loading ? "Checking…" : "Retry"}
        </button>
      </div>

      {error ? (
        <div className={`overflow-hidden ${R_PIECE}`}>
          <ErrorLine message={`Couldn’t run the environment checks: ${error}`} />
        </div>
      ) : null}

      {rows === null && !error ? (
        <p className={`py-3 text-center text-xs ${TEXT_3}`}>Running checks…</p>
      ) : null}

      {rows ? (
        <ul className={`divide-y border ${R_PIECE} ${DIVIDE} ${LINE}`}>
          {rows.map((row) => {
            const failMuted = !row.ok && !row.required; // advisory warning
            return (
              <li key={row.id} className="flex items-start gap-2.5 px-3 py-2">
                <span
                  className={`mt-0.5 flex h-4 w-4 shrink-0 items-center justify-center rounded-full ${
                    row.ok
                      ? "bg-emerald-500/15 text-emerald-700 dark:bg-emerald-400/15 dark:text-emerald-300"
                      : failMuted
                        ? "bg-amber-500/15 text-amber-700 dark:bg-amber-400/15 dark:text-amber-300"
                        : "bg-rose-500/15 text-rose-700 dark:bg-rose-400/15 dark:text-rose-300"
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
                    <span className={`text-sm font-medium ${TEXT}`}>{row.label}</span>
                    {!row.required ? (
                      <span className={`shrink-0 rounded-md bg-black/5 px-1.5 py-0.5 text-[10px] font-semibold dark:bg-white/10 ${TEXT_2}`}>
                        optional
                      </span>
                    ) : null}
                  </div>
                  {row.detail ? (
                    <p
                      className={`mt-0.5 text-xs ${
                        row.ok ? TEXT_3 : failMuted ? "text-amber-700 dark:text-amber-300" : ERROR_TEXT
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
        <p className={`text-xs ${ERROR_TEXT}`}>
          Fix the failing required checks, then retry. Setup can’t continue until Docker is
          reachable.
        </p>
      ) : null}
    </div>
  );
}
