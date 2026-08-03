// Open a Linear issue from the board, without leaving it.
//
// The column is the operator's inbox of work not started, so the thing missing from it was
// the ability to put work in. This is that: a team, a title, a body, and a priority.
//
// The team key is picked the way the clone dialog picks it, off the presets' own labels. A
// preset labelled `WE` owns team WE and its Linear key is what opens the issue, so choosing
// a team and choosing a key are the same choice and the dialog only asks it once.
//
// What the dialog does NOT ask is the state: a new ticket is pinned to Todo, because the
// column draws Todo and In Progress and anything else would be created and vanish in the same
// breath. The assignee IS asked, and starts on the key's own owner for the same reason: the
// column lists what that owner is assigned, so anybody else takes the ticket out of it. The
// dialog says so rather than refusing the choice.
//
// Pure: it takes presets and a create function, so a story can drive it with a spy. The team
// and the assignee are props rather than state, because the container fetches the assignable
// people for whichever team is chosen and remembers that team for next time. The description
// editor arrives as a slot for the same reason — it is browser-only and carries the upload
// call, so TicketModalContainer decides when and how it mounts.
import { useState, type ReactNode } from "react";

import { PrioritySelect } from "~/components/PrioritySelect";
import type { TeamKey } from "~/lib/cloneDraft";
import type { TicketPerson } from "~/lib/linear/people";
import { useModalEscape } from "~/lib/useModalEscape";

/** What the dialog sends. `priority` follows Linear: 1 urgent, 2 high, 3 medium, 4 low. */
export interface NewTicket {
  team: string;
  title: string;
  description: string;
  priority?: number;
  /** Linear's user UUID for the assignee. Blank leaves it to the key's own owner. */
  assigneeId?: string;
}

export interface TicketModalViewProps {
  /** Every team key the presets declare, with the preset that claims each. Empty means no
   *  preset declares one, and there is nothing to open a ticket in. */
  teams: TeamKey[];
  /** The chosen team key, lowercase as `teamKeysOf` returns it. */
  team: string;
  onTeamChange: (team: string) => void;
  /** Who can hold a ticket in the chosen team, the viewer first. Empty while the list is
   *  loading, and after a lookup that found nobody. */
  people: TicketPerson[];
  /** The chosen assignee's user id. Blank leaves the create to the key's own owner. */
  assigneeId: string;
  onAssigneeChange: (id: string) => void;
  /** The people lookup is in flight for the chosen team. */
  peopleLoading: boolean;
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
  teams,
  team,
  onTeamChange,
  people,
  assigneeId,
  onAssigneeChange,
  peopleLoading,
  description,
  descriptionEditor,
  onClose,
  onCreate,
}: TicketModalViewProps) {
  const [title, setTitle] = useState("");
  const [priority, setPriority] = useState(0);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  useModalEscape(() => {
    if (!busy) onClose();
  });

  const chosen = teams.find((t) => t.key === team);
  // Linear refuses a team whose preset carries no key, so the dialog says so first.
  const keyMissing = !!chosen && chosen.preset.linearKey === "";
  // Who the footer names. Absent while the list loads and when the lookup found nobody, both
  // of which leave the create to the key's own owner, which is to say: you.
  const assignee = people.find((p) => p.id === assigneeId);
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
      ...(assigneeId !== "" ? { assigneeId } : {}),
    })
      .then(() => onClose())
      .catch((e: Error) => {
        setError(e.message);
        setBusy(false);
      });
  };

  // `h-9` pins every field to one height. Left to their content they come out at two: a
  // `<select>` lays its text out at the browser's own `line-height: normal` and lands on 36px,
  // while an input and the priority button inherit `text-sm`'s 20px and land on 38px. Nobody
  // could see that until the three sat on one row.
  const field =
    "mt-1 h-9 w-full rounded-md border border-slate-300 px-3 py-2 text-sm font-normal text-slate-900 dark:bg-slate-800 placeholder:text-slate-400 focus:border-emerald-500 focus:outline-none dark:border-slate-600 dark:text-slate-100 dark:placeholder:text-slate-500";
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
          {/* The three properties of the ticket that are not its text, on one line and in
              three equal columns. A grid rather than three flexed children: they are the same
              width because they are one row of fields, not because their contents happen to
              balance. */}
          <div className="grid grid-cols-3 gap-2">
            <label className={`${label} min-w-0`}>
              Team key
              <select
                value={team}
                onChange={(e) => onTeamChange(e.target.value)}
                disabled={busy || teams.length === 0}
                className={field}
              >
                {teams.length === 0 ? <option value="">None</option> : null}
                {teams.map((t) => (
                  <option key={t.key} value={t.key}>
                    {t.key.toUpperCase()} · {t.preset.name}
                  </option>
                ))}
              </select>
            </label>

            {/* Not a `<select>`: the priority is read as a glyph everywhere else on the board,
                and an option element can hold nothing but text. */}
            <div className={`${label} min-w-0`}>
              Priority
              <PrioritySelect
                value={priority}
                onChange={setPriority}
                disabled={busy}
                className={field}
              />
            </div>

            <label className={`${label} min-w-0`}>
              Assignee
              <select
                value={assigneeId}
                onChange={(e) => onAssigneeChange(e.target.value)}
                disabled={busy || peopleLoading || people.length === 0}
                className={field}
              >
                {/* One blank option covers both nothing-yet cases, so the field is never empty
                    while it waits and never claims a name it does not have. The create still
                    works from it: an empty assignee falls back to the key's own owner. */}
                {peopleLoading || people.length === 0 ? (
                  <option value="">{peopleLoading ? "Loading…" : "You"}</option>
                ) : null}
                {/* You are "You" rather than your own name: it is the shortest true label, it
                    is the one the field falls back to before the list arrives, and a third of
                    this row is not wide enough for a name and a marker beside it. */}
                {people.map((person) => (
                  <option key={person.id} value={person.id}>
                    {person.isViewer ? "You" : person.name}
                  </option>
                ))}
              </select>
            </label>
          </div>

          {/* Both warnings are about the team, and both sit under the row rather than inside
              its narrowest column, where either would wrap to four lines. */}
          {teams.length === 0 ? (
            <p className="text-[11px] text-red-600 dark:text-red-400">
              No preset declares a team key. Add ticket-id prefixes to a preset in Settings.
            </p>
          ) : null}

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
            {/* `px-3` matches the inputs above, so the body starts where the title does. The
                editor brings no side padding of its own (see `ticket-description` in app.css),
                which is what leaves this box free to set its own. */}
            <div className="mt-1 min-h-[10rem] rounded-md border border-slate-300 px-3 py-2 text-sm font-normal focus-within:border-emerald-500 dark:border-slate-600">
              {descriptionEditor}
            </div>
          </div>

          {/* The state is not the operator's to choose, so it is stated rather than offered as
              a disabled field. The assignee is theirs, and picking anybody else takes the new
              ticket straight out of the column that opened it, which is worth saying before
              the click and not after. */}
          <p className="text-[11px] text-slate-400 dark:text-slate-500">
            Opens as Todo, assigned to {assignee && !assignee.isViewer ? assignee.name : "you"}.
          </p>
          {assignee && !assignee.isViewer ? (
            <p className="text-[11px] text-amber-600 dark:text-amber-400">
              This column lists the tickets assigned to you, so it will not appear in it.
            </p>
          ) : null}

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
