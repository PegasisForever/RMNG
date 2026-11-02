// The Control-server section's body: which image this server is running, an on-demand update
// check, and the two buttons that replace or restart it.
//
// Every one of those is a server call the container owns, so this renders a status, a
// message and an optional operation, and reports three clicks. The update op's progress
// shows here rather than only in the rail: the server restarts itself partway through, and
// the operator wants to watch that from the panel that started it.
import { OperationProgress } from "~/components/OperationProgress";
import { settingsButton } from "~/components/SettingsFields";
import { DONE_BUTTON, TEXT_2 } from "~/desk/control/ui";
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
      <div className={`text-xs ${TEXT_2}`}>
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
          className={settingsButton}
        >
          Check for updates
        </button>
        <button
          type="button"
          onClick={onUpdate}
          disabled={updateDisabled}
          className={`${DONE_BUTTON} px-3 py-1.5 text-xs font-medium`}
        >
          Update
        </button>
        <button
          type="button"
          onClick={onRestart}
          className={settingsButton}
        >
          Restart
        </button>
        {status ? (
          <span className={`rounded-md px-1.5 py-0.5 text-[10px] font-semibold ${status.available ? "bg-amber-500/15 text-amber-700 dark:text-amber-300" : "bg-emerald-500/15 text-emerald-700 dark:text-emerald-300"}`}>
            {status.available ? "update available" : "up to date"}
          </span>
        ) : null}
        {message ? <p className={`text-xs ${TEXT_2}`}>{message}</p> : null}
      </div>
      {operation ? <OperationProgress op={operation} /> : null}
    </div>
  );
}
