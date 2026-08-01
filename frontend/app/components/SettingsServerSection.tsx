// The Control-server section's body: which image this server is running, an on-demand update
// check, and the two buttons that replace or restart it.
//
// Every one of those is a server call the container owns, so this renders a status, a
// message and an optional operation, and reports three clicks. The update op's progress
// shows here rather than only in the rail: the server restarts itself partway through, and
// the operator wants to watch that from the panel that started it.
import { OperationProgress } from "~/components/OperationProgress";
import type { Operation } from "~/lib/types";
import type { UpdateStatus } from "~/lib/wire/UpdateStatus";

export function SettingsServerSection({
  status,
  message,
  operation,
  updateDisabled,
  onCheckUpdate,
  onUpdate,
  onRestart,
}: {
  /** The control-server's own version and update-available answer. Null before the first
   *  check lands, which is what leaves the version line reading "dev build". */
  status: UpdateStatus | null;
  /** The last thing the panel has to say about a check, an update or a restart. */
  message: string | null;
  /** The self-update operation, once it shows up in the live op list. */
  operation: Operation | null;
  /** The Update button is dead: nothing to update to, or an update is already running. */
  updateDisabled: boolean;
  onCheckUpdate: () => void;
  onUpdate: () => void;
  onRestart: () => void;
}) {
  return (
    <div className="space-y-2">
      <div className="text-xs text-slate-500 dark:text-slate-400">
        {status?.currentRevision ? (
          <>Version <code>{status.currentRevision}</code>{status.currentCreated ? ` · ${status.currentCreated}` : ""}</>
        ) : (
          "dev build (unversioned image)"
        )}
      </div>
      <div className="flex items-center gap-2">
        <button
          type="button"
          onClick={onCheckUpdate}
          className="rounded border border-slate-300 dark:border-slate-600 px-2.5 py-1.5 text-xs text-slate-600 dark:text-slate-300 hover:bg-slate-50 dark:hover:bg-slate-800"
        >
          Check for updates
        </button>
        <button
          type="button"
          onClick={onUpdate}
          disabled={updateDisabled}
          className="rounded bg-slate-900 px-3 py-1.5 text-xs font-medium text-white hover:bg-slate-700 disabled:opacity-50 dark:bg-slate-100 dark:text-slate-900 dark:hover:bg-white"
        >
          Update
        </button>
        <button
          type="button"
          onClick={onRestart}
          className="rounded border border-slate-300 dark:border-slate-600 px-2.5 py-1.5 text-xs text-slate-600 dark:text-slate-300 hover:bg-slate-50 dark:hover:bg-slate-800"
        >
          Restart
        </button>
        {status ? (
          <span className={`rounded px-1.5 py-0.5 text-[10px] font-semibold ${status.available ? "bg-amber-100 dark:bg-amber-900/40 text-amber-700 dark:text-amber-400" : "bg-emerald-100 dark:bg-emerald-900/40 text-emerald-700 dark:text-emerald-400"}`}>
            {status.available ? "update available" : "up to date"}
          </span>
        ) : null}
        {message ? <p className="text-xs text-slate-500 dark:text-slate-400">{message}</p> : null}
      </div>
      {operation ? <OperationProgress op={operation} /> : null}
    </div>
  );
}
