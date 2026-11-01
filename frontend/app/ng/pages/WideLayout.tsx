// The /ng pages where they are wide (a desktop's pages window, or the pages over a large
// screen): the dashboard's layout in place of the tabs. The rail at the left (the server's
// numbers, Settings, the accounts' usage), the board in the middle (the tickets' column, then
// the clones' columns, scrolling sideways), and the side panel at the right (the selected
// clone's notes over its assistant, or the open ticket). Settings takes the board's and the side
// panel's place while it is open.
//
// Pure: each part comes in as a slot (`./NgPagesContainer.tsx` fills them).
import { Settings } from "lucide-react";
import type { ReactNode } from "react";

import { PopOutButton, type ServerUsage } from "../ToolRow";
import { LINE, LIT, TEXT, TEXT_2 } from "./ui";
import { ServerStats } from "./UsageView";

/** At least this wide (CSS px), the pages take this layout. */
export const WIDE_MIN = 1000;

/** A part of the layout: a hollow with its own scrolling. */
const PART = `min-h-0 overflow-hidden rounded-xl border ${LINE}`;

export function WideLayout({
  usage,
  settingsOpen,
  onSettings,
  onPopOut,
  rail,
  board,
  side,
  settings,
}: {
  usage: ServerUsage | null;
  settingsOpen: boolean;
  /** Open Settings, or close it. */
  onSettings: () => void;
  onPopOut?: () => void;
  /** The accounts' usage. */
  rail: ReactNode;
  /** The columns, the tickets' first. */
  board: ReactNode;
  side: ReactNode;
  settings: ReactNode;
}) {
  return (
    <div className="flex h-full min-h-0 gap-2">
      <aside aria-label="Server and accounts" className="flex w-64 shrink-0 flex-col gap-2">
        <div className="flex h-9 shrink-0 items-center gap-1.5 pl-1">
          <h2 className={`min-w-0 flex-1 truncate text-sm font-semibold ${TEXT}`}>RMNG Control</h2>
          <button
            type="button"
            onClick={onSettings}
            aria-pressed={settingsOpen}
            aria-label="Settings"
            title="Settings"
            className={`flex size-9 items-center justify-center rounded-md border ${
              settingsOpen
                ? `border-transparent ${LIT}`
                : `border-black/10 bg-white/50 hover:bg-white/80 dark:border-white/10 dark:bg-white/[0.06] dark:hover:bg-white/10 ${TEXT_2}`
            }`}
          >
            <Settings aria-hidden className="size-[18px]" strokeWidth={1.75} />
          </button>
          {onPopOut ? <PopOutButton onPopOut={onPopOut} /> : null}
        </div>
        <ServerStats usage={usage} />
        <div className={`flex-1 ${PART}`}>{rail}</div>
      </aside>
      {settingsOpen ? (
        <div className={`min-w-0 flex-1 ${PART}`}>{settings}</div>
      ) : (
        <>
          <div aria-label="Board" className="min-w-0 flex-1 overflow-x-auto overscroll-contain">
            <div className="flex h-full gap-2">{board}</div>
          </div>
          <aside aria-label="Notes" className={`relative flex w-96 shrink-0 flex-col ${PART}`}>
            {side}
          </aside>
        </>
      )}
    </div>
  );
}
