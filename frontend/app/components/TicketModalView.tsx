// Open a Linear issue from the board, without leaving it.
//
// The column is the operator's inbox of work not started, so the thing missing from it was
// the ability to put work in. This is that: a team, a title, a body, and a priority.
//
// The team key is picked the way the clone dialog picks it, off the presets' own labels. A
// preset labelled `WE` owns team WE and its Linear key is what opens the issue, so choosing
// a team and choosing a key are the same choice and the dialog only asks it once.
//
// What the dialog does NOT ask: the state and the assignee. The server pins both (Todo, and
// the key's owner) because the column lists open issues assigned to the key's owner. A
// ticket created any other way would be created and vanish in the same breath.
//
// Pure: it takes presets and a create function, so a story can drive it with a spy. The
// description editor arrives as a slot for the same reason — it is browser-only and carries
// the /api/upload call, so TicketModalContainer decides when and how it mounts.
import { useMemo, useState, type ReactNode } from "react";

import { PRIORITY_LABEL, PriorityIcon } from "~/components/TicketColumn";
import { teamKeysOf } from "~/lib/cloneDraft";
import { useModalEscape } from "~/lib/useModalEscape";
import type { PresetRedacted } from "~/lib/wire/PresetRedacted";

/** What the dialog sends. `priority` follows Linear: 1 urgent, 2 high, 3 medium, 4 low. */
export interface NewTicket {
  team: string;
  title: string;
  description: string;
  priority?: number;
}

export interface TicketModalViewProps {
  /** The configured presets. Their labels are the team keys, and `linearKeySet` says which
   *  of them can actually open a ticket. */
  presets: PresetRedacted[];
  /** The body as markdown, as the editor slot last reported it. Sent with the ticket. */
  description: string;
  /** The markdown editor for the body, as a slot: the real one is browser-only and
   *  lazy-loaded, so the container decides when and how it mounts. */
  descriptionEditor: ReactNode;
  onClose: () => void;
  /** Open it. Rejecting leaves the dialog up with the message; resolving closes it. */
  onCreate: (ticket: NewTicket) => Promise<unknown>;
}

export function TicketModalView({
  presets,
  description,
  descriptionEditor,
  onClose,
  onCreate,
}: TicketModalViewProps) {
  const teams = useMemo(() => teamKeysOf(presets), [presets]);
  const [team, setTeam] = useState(() => teams[0]?.key ?? "");
  const [title, setTitle] = useState("");
  const [priority, setPriority] = useState(0);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  useModalEscape(() => {
    if (!busy) onClose();
  });

  const chosen = teams.find((t) => t.key === team);
  // The server refuses a team whose preset carries no key, so the dialog says so first.
  const keyMissing = !!chosen && !chosen.preset.linearKeySet;
  const canSubmit = !busy && !keyMissing && team !== "" && title.trim() !== "";

  const submit = () => {
    if (!canSubmit) return;
    setBusy(true);
    setError(null);
    onCreate({
      team: team.toUpperCase(),
      title: title.trim(),
      description,
      ...(priority > 0 ? { priority } : {}),
    })
      .then(() => onClose())
      .catch((e: Error) => {
        setError(e.message);
        setBusy(false);
      });
  };

  const field =
    "mt-1 w-full rounded-md border border-slate-300 px-3 py-2 text-sm font-normal text-slate-900 dark:bg-slate-800 placeholder:text-slate-400 focus:border-emerald-500 focus:outline-none dark:border-slate-600 dark:text-slate-100 dark:placeholder:text-slate-500";
  const label = "block text-xs font-medium text-slate-500 dark:text-slate-400";

  return (
    // Backdrop is inert, like the clone dialog's: only Cancel and Escape close this, and
    // neither does while a create is in flight.
    <div className="fixed inset-0 z-50 flex items-center justify-center bg-slate-900/30 p-4">
      <div className="flex max-h-[90vh] w-full max-w-lg flex-col rounded-xl border border-slate-200 bg-white p-5 shadow-xl dark:border-slate-700 dark:bg-slate-800">
        <h3 className="shrink-0 text-sm font-semibold text-slate-900 dark:text-slate-100">
          New ticket
        </h3>

        <div className="min-h-0 shrink space-y-3 overflow-y-auto pr-0.5 pt-3">
          <div className="flex gap-3">
            <label className={`${label} w-32 shrink-0`}>
              Team key
              {teams.length === 0 ? (
                <p className="mt-1 text-[11px] font-normal text-red-600 dark:text-red-400">
                  No preset declares a team key. Add ticket-id prefixes to a preset in
                  Settings.
                </p>
              ) : (
                <select
                  value={team}
                  onChange={(e) => setTeam(e.target.value)}
                  disabled={busy}
                  className={field}
                >
                  {teams.map((t) => (
                    <option key={t.key} value={t.key}>
                      {t.key.toUpperCase()} · {t.preset.name}
                    </option>
                  ))}
                </select>
              )}
            </label>

            <label className={`${label} min-w-0 flex-1`}>
              Priority
              <select
                value={priority}
                onChange={(e) => setPriority(Number(e.target.value))}
                disabled={busy}
                className={field}
              >
                <option value={0}>No priority</option>
                {[1, 2, 3, 4].map((level) => (
                  <option key={level} value={level}>
                    {PRIORITY_LABEL[level]}
                  </option>
                ))}
              </select>
            </label>
          </div>

          {keyMissing ? (
            <p className="text-[11px] text-red-600 dark:text-red-400">
              Preset “{chosen?.preset.name}” has no Linear API key. Add it in Settings, or
              pick a team whose preset has one.
            </p>
          ) : null}

          <label className={label}>
            Title
            <input
              autoFocus
              value={title}
              onChange={(e) => setTitle(e.target.value)}
              onKeyDown={(e) => {
                if (e.key === "Enter") submit();
              }}
              disabled={busy}
              placeholder="Short ticket title"
              className={field}
            />
          </label>

          <div className={label}>
            Description
            <div className="mt-1 min-h-[10rem] rounded-md border border-slate-300 py-2 text-sm font-normal focus-within:border-emerald-500 dark:border-slate-600">
              {descriptionEditor}
            </div>
          </div>

          {/* What the operator does not get to choose, said once rather than as two disabled
              fields. Both are what makes the new ticket land in this column. */}
          <p className="flex items-center gap-1.5 text-[11px] text-slate-400 dark:text-slate-500">
            {priority > 0 ? <PriorityIcon level={priority} /> : null}
            Opens as Todo, assigned to you.
          </p>

          {error ? (
            <p className="rounded-md bg-red-50 px-2 py-1.5 text-xs text-red-700 dark:bg-red-950 dark:text-red-300">
              {error}
            </p>
          ) : null}
        </div>

        <div className="mt-4 flex shrink-0 justify-end gap-2">
          <button
            type="button"
            onClick={onClose}
            disabled={busy}
            className="cursor-pointer rounded-lg px-3 py-1.5 text-xs font-medium text-slate-500 hover:bg-slate-100 disabled:opacity-50 dark:text-slate-400 dark:hover:bg-slate-700"
          >
            Cancel
          </button>
          <button
            type="button"
            onClick={submit}
            disabled={!canSubmit}
            className="cursor-pointer rounded-lg bg-emerald-600 px-3 py-1.5 text-xs font-medium text-white hover:bg-emerald-700 disabled:opacity-50"
          >
            {busy ? "Creating…" : "Create ticket"}
          </button>
        </div>
      </div>
    </div>
  );
}
