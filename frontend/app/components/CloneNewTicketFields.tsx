// The clone dialog's New-ticket tab: the ticket does not exist yet, so the dialog collects
// what Linear needs to open one — team key, title, body.
//
// The team dropdown is also the preset selector. Its keys come from the presets' own labels,
// so picking "DEV · preset 1" picks both, which is why this tab draws no preset control and no
// resolved-preset line.
import type { ReactNode } from "react";

import { cloneField, cloneLabel } from "~/components/cloneFieldStyles";
import type { TeamKey } from "~/lib/cloneDraft";

export function CloneNewTicketFields({
  teamKeys,
  team,
  title,
  description,
  onTeamChange,
  onTitleChange,
}: {
  /** Every team key the presets declare, with the preset that claims each. Empty blocks the
   *  tab: with no key there is no team to open the issue in and no preset to open it with. */
  teamKeys: TeamKey[];
  team: string;
  title: string;
  /** The markdown editor for the ticket body, as a slot. The real one is BlockNote, which is
   *  browser-only and lazy-loaded, so the dialog's container hands it in and a story hands in
   *  the same editor on a stub upload. */
  description: ReactNode;
  onTeamChange: (team: string) => void;
  onTitleChange: (title: string) => void;
}) {
  return (
    <div className="mt-3 space-y-3">
      <label className={cloneLabel}>
        Team key
        {teamKeys.length === 0 ? (
          <p className="mt-1 text-[11px] font-normal text-red-600 dark:text-red-400">
            No preset declares a team key — add ticket-id prefixes to a preset in Settings.
          </p>
        ) : (
          <select
            value={team}
            onChange={(e) => onTeamChange(e.target.value)}
            className={cloneField}
          >
            {teamKeys.map((t) => (
              <option key={t.key} value={t.key}>
                {t.key.toUpperCase()} · {t.preset.name}
              </option>
            ))}
          </select>
        )}
      </label>
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
        <div className="mt-1 min-h-[6.5rem] rounded-md border border-slate-300 py-2 text-sm font-normal focus-within:border-emerald-500 dark:border-slate-600">
          {description}
        </div>
      </div>
    </div>
  );
}
