// The clone dialog's New-ticket tab: the ticket does not exist yet, so the dialog collects
// what Linear needs to open one — team key, priority, title, body.
//
// No assignee, unlike the board's own New-ticket dialog. A clone is work you are about to
// start, so the issue it opens is yours, and a field that could only ever say one thing is a
// field worth not drawing.
//
// The team dropdown is also the preset selector. Its keys come from the presets' own labels,
// so picking "DEV · preset 1" picks both, which is why this tab draws no preset control and no
// resolved-preset line.
import type { ReactNode } from "react";

import { cloneField, cloneLabel } from "~/components/cloneFieldStyles";
import { PrioritySelect } from "~/components/PrioritySelect";
import type { TeamKey } from "~/lib/cloneDraft";

export interface CloneNewTicketFieldsProps {
  /** Every team key the presets declare, with the preset that claims each. Empty blocks the
   *  tab: with no key there is no team to open the issue in and no preset to open it with. */
  teamKeys: TeamKey[];
  team: string;
  title: string;
  /** Linear's own priority: 0 unranked, 1 urgent, 2 high, 3 medium, 4 low. */
  priority: number;
  /** The markdown editor for the ticket body, as a slot. The real one is BlockNote, which is
   *  browser-only and lazy-loaded, so the dialog's container hands it in and a story hands in
   *  the same editor on a stub upload. */
  description: ReactNode;
  onTeamChange: (team: string) => void;
  onTitleChange: (title: string) => void;
  onPriorityChange: (priority: number) => void;
}

export function CloneNewTicketFields({
  teamKeys,
  team,
  title,
  priority,
  description,
  onTeamChange,
  onTitleChange,
  onPriorityChange,
}: CloneNewTicketFieldsProps) {
  return (
    <div className="mt-3 space-y-3">
      {/* Two equal columns: the team the ticket lands in, and how it is ranked when it gets
          there. Both are one-glance choices, unlike the title and the body under them. */}
      <div className="grid grid-cols-2 gap-2">
        <label className={`${cloneLabel} min-w-0`}>
          Team key
          <select
            value={team}
            onChange={(e) => onTeamChange(e.target.value)}
            disabled={teamKeys.length === 0}
            className={cloneField}
          >
            {teamKeys.length === 0 ? <option value="">None</option> : null}
            {teamKeys.map((t) => (
              <option key={t.key} value={t.key}>
                {t.key.toUpperCase()} · {t.preset.name}
              </option>
            ))}
          </select>
        </label>

        {/* Not a `<select>`: an option element holds nothing but text, and the priority is a
            glyph everywhere else on this board. */}
        <div className={`${cloneLabel} min-w-0`}>
          Priority
          <PrioritySelect value={priority} onChange={onPriorityChange} className={cloneField} />
        </div>
      </div>

      {teamKeys.length === 0 ? (
        <p className="text-[11px] font-normal text-red-600 dark:text-red-400">
          No preset declares a team key — add ticket-id prefixes to a preset in Settings.
        </p>
      ) : null}
      <label className={cloneLabel}>
        Title
        <input
          autoFocus
          value={title}
          onChange={(e) => onTitleChange(e.target.value)}
          placeholder="Short ticket title"
          className={cloneField}
        />
      </label>
      <div className={cloneLabel}>
        Description
        {/* `px-3` matches `cloneField`, so the body starts where the title above it does. The
            editor brings no side padding of its own (see `ticket-description` in app.css). */}
        <div className="mt-1 min-h-[6.5rem] rounded-md border border-slate-300 px-3 py-2 text-sm font-normal focus-within:border-emerald-500 dark:border-slate-600">
          {description}
        </div>
      </div>
    </div>
  );
}
