// Open a Linear issue from the board, network half.
//
// Two things live here and nowhere below: the `issueCreate` mutation, and the markdown editor
// the body is typed into, which carries its own image upload. Everything else, including
// which team is chosen and whether the button may fire, is TicketModalView.
//
// The created ticket goes back up rather than being handled here: the column writes it into
// its own list and opens its panel, which is the page's call and not the dialog's.
import { lazy, Suspense, useState } from "react";

import { TicketModalView } from "~/components/TicketModalView";
import { issueCreate, keysForTeam } from "~/lib/linear/mutations";
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
  // The body lives here because the editor does: the slot reports markdown up on every
  // keystroke, and the View sends whatever it last said.
  const [description, setDescription] = useState("");

  // Which key stores a pasted image. The team belongs to the View, which owns the dropdown,
  // and the slot is built before the operator has touched it. So this is the first key in
  // config order, which is the team the dropdown starts on. A fleet whose presets point at
  // different Linear workspaces can therefore store an image in one workspace and reference
  // it from an issue in another. Single-workspace fleets, which is every one so far, are
  // unaffected.
  const uploadKey = keysForTeam(presets, "")[0] ?? "";

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
            linearKey={uploadKey}
            placeholder="What needs doing — paste images, format freely"
          />
        </Suspense>
      }
      onClose={onClose}
      onCreate={(ticket) => issueCreate(keysForTeam(presets, ticket.team)[0] ?? "", ticket).then(onCreated)}
    />
  );
}
