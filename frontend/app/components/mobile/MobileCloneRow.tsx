// One clone in the phone's list: its status, its name, and what tells it apart from the
// clone above it. The whole row is the tap target, at the 64px minimum a thumb wants.
//
// A list item rather than a card, because nothing here moves: the board's columns become the
// section headers on the phone, and a row cannot be dragged out of one.
import { ChevronRight } from "lucide-react";

import { CloneStatusDot } from "~/components/mobile/CloneStatus";
import type { Clone } from "~/lib/types";

/** Ticket and account, the two things that say which clone this is when several share a
 *  shape. Empty when the clone has neither, and the row then shows its id instead. */
function subtitle(clone: Clone): string {
  return [clone.linearTicket, clone.claudeAccountEmail].filter(Boolean).join(" · ");
}

export function MobileCloneRow({
  clone,
  onSelect,
}: {
  clone: Clone;
  /** Open this clone's screen. The container activates it server-side and swaps the page. */
  onSelect: (clone: Clone) => void;
}) {
  return (
    <li>
      <button
        type="button"
        onClick={() => onSelect(clone)}
        className="flex min-h-16 w-full items-center gap-3 px-4 py-3 text-left active:bg-slate-100 dark:active:bg-slate-800"
      >
        <CloneStatusDot clone={clone} />
        <span className="min-w-0 flex-1">
          <span className="block truncate text-sm font-medium text-slate-800 dark:text-slate-100">
            {clone.displayName ?? clone.id}
          </span>
          <span className="block truncate text-xs text-slate-500 dark:text-slate-400">
            {subtitle(clone) || clone.id}
          </span>
        </span>
        <ChevronRight aria-hidden className="size-4 shrink-0 text-slate-300 dark:text-slate-600" />
      </button>
    </li>
  );
}
