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

import { ERROR_TEXT } from "~/desk/control/ui";
import {
        cloneEditorBox,
        cloneRow,
        cloneRowField,
        cloneRowLabel,
        cloneRowLabelTop,
        cloneRowTop,
} from "~/components/cloneFieldStyles";
import { DropdownSelect } from "~/components/DropdownSelect";
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
                <div className="space-y-3">
                        <label className={cloneRow}>
                                <span className={cloneRowLabel}>Team key</span>
                                <div className="min-w-0">
                                        <DropdownSelect
                                                rows={
                                                        teamKeys.length === 0
                                                                ? [
                                                                          {
                                                                                  value: "",
                                                                                  label: "None",
                                                                                  disabled: true,
                                                                          },
                                                                  ]
                                                                : teamKeys.map(
                                                                          (
                                                                                  t,
                                                                          ) => ({
                                                                                  value: t.key,
                                                                                  label: `${t.key.toUpperCase()} · ${t.preset.name}`,
                                                                          }),
                                                                  )
                                                }
                                                value={team}
                                                onChange={onTeamChange}
                                                disabled={teamKeys.length === 0}
                                                label="Team key"
                                                className={cloneRowField}
                                        />
                                        {teamKeys.length === 0 ? (
                                                <p className={`mt-1 text-[11px] font-normal ${ERROR_TEXT}`}>
                                                        No preset declares a
                                                        team key — add ticket-id
                                                        prefixes to a preset in
                                                        Settings.
                                                </p>
                                        ) : null}
                                </div>
                        </label>

                        <div className={cloneRow}>
                                <span className={cloneRowLabel}>Priority</span>
                                {/* Not a `<select>`: an option element holds nothing but text, and the priority is a
            glyph everywhere else on this board. */}
                                <PrioritySelect
                                        value={priority}
                                        onChange={onPriorityChange}
                                        className={cloneRowField}
                                />
                        </div>

                        <label className={cloneRow}>
                                <span className={cloneRowLabel}>Title</span>
                                <input
                                        autoFocus
                                        value={title}
                                        onChange={(e) =>
                                                onTitleChange(e.target.value)
                                        }
                                        placeholder="Short ticket title"
                                        className={cloneRowField}
                                />
                        </label>
                        <div className={cloneRowTop}>
                                <span className={cloneRowLabelTop}>
                                        Description
                                </span>
                                {/* `px-3` matches the inputs, so the body starts where the title above it does. The
            editor brings no side padding of its own (see `ticket-description` in app.css). */}
                                <div className={`min-h-[6.5rem] ${cloneEditorBox}`}>
                                        {description}
                                </div>
                        </div>
                </div>
        );
}
