// Open a Linear issue from the board, network half.
//
// Two things live here and nowhere below: the POST that creates the issue, and the markdown
// editor the body is typed into, which carries its own /api/upload call. Everything else,
// including which team is chosen and whether the button may fire, is TicketModalView.
//
// The created ticket goes back up rather than being handled here: the column draws it from
// the `/events` frame the server broadcasts, and opening its panel is the page's call.
import { lazy, Suspense, useState } from "react";

import { TicketModalView } from "~/components/TicketModalView";
import { createTicket } from "~/lib/api";
import type { LinearTicket } from "~/lib/tickets";
import type { PresetRedacted } from "~/lib/wire/PresetRedacted";

// BlockNote is browser-only and heavy; the description field pulls it in on demand. The
// container is the import target, so the /api/upload call it owns rides the same lazy chunk.
const MarkdownEditorContainer = lazy(() => import("~/components/MarkdownEditorContainer"));

export function TicketModalContainer({
  presets,
  onClose,
  onCreated,
}: {
  /** Configured presets (`config.presets`). Their labels are the dialog's team keys, and
   *  `linearKeySet` says which of them can actually open a ticket. */
  presets: PresetRedacted[];
  onClose: () => void;
  /** The issue Linear answered with, once it exists. The dialog closes either way. */
  onCreated: (ticket: LinearTicket) => void;
}) {
  // The body lives here because the editor does: the slot reports markdown up on every
  // keystroke, and the View sends whatever it last said.
  const [description, setDescription] = useState("");

  return (
    <TicketModalView
      presets={presets}
      description={description}
      descriptionEditor={
        <Suspense
          fallback={
            <p className="px-3 text-xs text-slate-400 dark:text-slate-500">Loading editor…</p>
          }
        >
          <MarkdownEditorContainer
            onChange={setDescription}
            placeholder="What needs doing — paste images, format freely"
          />
        </Suspense>
      }
      onClose={onClose}
      onCreate={(ticket) => createTicket(ticket).then(onCreated)}
    />
  );
}
