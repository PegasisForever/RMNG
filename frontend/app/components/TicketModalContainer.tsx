// Open a Linear issue from the board, network half. The Linear work is `~/lib/linear/intake`,
// shared with the clone dialog; the markup is TicketModalView.
//
// The team is held here rather than in the View because three impure things hang off it: the
// people lookup, the key that stores a pasted image, and what gets remembered for next time.
//
// The created ticket goes back up rather than being handled here: the column writes it into
// its own list and opens its panel, which is the page's call and not the dialog's.
import { lazy, Suspense, useMemo, useState } from "react";

import { TicketModalView } from "~/components/TicketModalView";
import { teamKeysOf } from "~/lib/cloneDraft";
import { keyFor, openTicket, startingTeam, useAssignee } from "~/lib/linear/intake";
import type { LinearTicket } from "~/lib/tickets";
import type { PresetRedacted } from "~/lib/wire/PresetRedacted";

// BlockNote is browser-only and heavy; the description field pulls it in on demand. The
// container is the import target, so the upload call it owns rides the same lazy chunk.
const MarkdownEditorContainer = lazy(() => import("~/components/MarkdownEditorContainer"));

export function TicketModalContainer({
  presets,
  onClose,
  onCreated,
}: {
  /** Configured presets (`config.presets`). Their labels are the dialog's team keys, and a
   *  non-empty `linearKey` says which of them can actually open a ticket. */
  presets: PresetRedacted[];
  onClose: () => void;
  /** The issue Linear answered with, once it exists. The dialog closes either way. */
  onCreated: (ticket: LinearTicket) => void;
}) {
  const teams = useMemo(() => teamKeysOf(presets), [presets]);
  // Read once on mount: storage is a session fact, and re-reading it would fight the dropdown.
  const [team, setTeam] = useState(() => startingTeam(teams));
  // The body lives here because the editor does: the slot reports markdown up on every
  // keystroke, and the View sends whatever it last said.
  const [description, setDescription] = useState("");

  // Whichever key claims the chosen team. It opens the issue and it stores the images pasted
  // into the body, so both follow the dropdown rather than being fixed when the dialog opened.
  const key = keyFor(presets, team);
  const { people, loading: peopleLoading, assigneeId, setAssigneeId } = useAssignee(key, team);

  return (
    <TicketModalView
      teams={teams}
      team={team}
      onTeamChange={setTeam}
      people={people}
      assigneeId={assigneeId}
      onAssigneeChange={setAssigneeId}
      peopleLoading={peopleLoading}
      description={description}
      descriptionEditor={
        <Suspense
          fallback={
            <p className="px-3 text-xs text-slate-400 dark:text-slate-500">Loading editor…</p>
          }
        >
          {/* Deliberately not keyed on the team: BlockNote captures its upload function once
              at mount, so remounting to move the image target would take the typed body with
              it. Images therefore go to whichever workspace the dialog opened on, and only a
              fleet whose presets point at different Linear workspaces can notice. */}
          <MarkdownEditorContainer
            onChange={setDescription}
            linearKey={key}
            placeholder="What needs doing — paste images, format freely"
          />
        </Suspense>
      }
      onClose={onClose}
      onCreate={(ticket) => openTicket(presets, ticket).then(onCreated)}
    />
  );
}
