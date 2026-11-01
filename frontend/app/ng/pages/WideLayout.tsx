// The /ng pages where they are wide (the pages over a large screen, or their own window): the
// dashboard's layout in place of the tabs. A row of columns that scrolls sideways: the rail (the
// server's numbers, Settings, the accounts' usage), the tickets', then the clones'. On the right,
// floating over the row, the side panels: the selected clone's notes over its assistant (the one
// touched last is the taller, as on the dashboard), or the open ticket in one panel. The row
// scrolls under them, and its end has room to bring the last column out from under them.
// Settings takes the place of the row and the side panels, beside the rail.
//
// Over the clone's screen the side panels go in a box of the screen's (`sideAt`): there they
// also slide in over the screen without the pages, when the mouse is at its right edge.
//
// Over the clone's screen (`glass`) each column is glass of its own, with nothing round them all,
// and the row takes the display's whole width: the columns scroll out to its edges, with the
// gap from them inside the row. In the pages' window, on its solid ground, they are flat
// hollows. The side panels are glass in both: they are over the columns. The row's scroll bar
// is hidden: a touchpad, a wheel with Shift, or a finger scrolls it.
//
// Pure: each part comes in as a slot (`./NgPagesContainer.tsx` fills them).
import { MonitorCog, Settings } from "lucide-react";
import { type ReactNode, useState } from "react";
import { createPortal } from "react-dom";

import { PopOutButton, type ServerUsage } from "../ToolRow";
import { COLUMN_GLASS, COLUMN_SOLID, ColumnFrameProvider, LIT, TEXT, TEXT_2 } from "./ui";
import { ServerStats } from "./UsageView";

/** At least this wide (CSS px), the pages take this layout. */
export const WIDE_MIN = 1000;

/** A side panel: glass over the columns, with the usual thick tint (not the columns' denser one),
 *  so the columns under it show through. */
/** A button in the rail's header. */
const RAIL_BUTTON = `border-black/10 bg-white/50 hover:bg-white/80 dark:border-white/10 dark:bg-white/[0.06] dark:hover:bg-white/10 ${TEXT_2}`;

const PANEL = `ng-glass ng-glass-thick rounded-2xl pointer-events-auto flex min-h-0 flex-col overflow-hidden transition-[flex-grow] duration-300 ease-out`;

export function WideLayout({
  glass,
  usage,
  settingsOpen,
  onSettings,
  onPopOut,
  onLayout,
  rail,
  board,
  side,
  sideAt,
  settings,
}: {
  /** Over the clone's screen: each column is glass. */
  glass: boolean;
  usage: ServerUsage | null;
  settingsOpen: boolean;
  /** Open Settings, or close it. */
  onSettings: () => void;
  onPopOut?: () => void;
  /** Open the Edit layout dialog; absent: no button (the pages' own window). */
  onLayout?: () => void;
  /** The accounts' usage. */
  rail: ReactNode;
  /** The columns, the tickets' first. */
  board: ReactNode;
  /** The side panels: the open ticket, or the clone's notes and its assistant. */
  side: SideProps;
  /** The screen's box for the side panels; absent: they float over the row's right end. */
  sideAt?: HTMLElement | null;
  settings: ReactNode;
}) {
  const frame = glass ? COLUMN_GLASS : COLUMN_SOLID;

  const railColumn = (
    <aside aria-label="Server and accounts" className={`flex h-full w-64 shrink-0 flex-col ${frame}`}>
      <div className="flex h-12 shrink-0 items-center gap-1.5 pl-3 pr-1.5">
        <h2 className={`min-w-0 flex-1 truncate text-sm font-semibold ${TEXT}`}>RMNG Control</h2>
        {onLayout ? (
          <button
            type="button"
            onClick={onLayout}
            aria-label="Edit layout"
            title="Edit layout"
            className={`flex size-9 items-center justify-center rounded-md border ${RAIL_BUTTON}`}
          >
            <MonitorCog aria-hidden className="size-[18px]" strokeWidth={1.75} />
          </button>
        ) : null}
        <button
          type="button"
          onClick={onSettings}
          aria-pressed={settingsOpen}
          aria-label="Settings"
          title="Settings"
          className={`flex size-9 items-center justify-center rounded-md border ${
            settingsOpen
              ? `border-transparent ${LIT}`
              : RAIL_BUTTON
          }`}
        >
          <Settings aria-hidden className="size-[18px]" strokeWidth={1.75} />
        </button>
        {onPopOut ? <PopOutButton onPopOut={onPopOut} /> : null}
      </div>
      <div className="shrink-0 px-2 pb-2">
        <ServerStats usage={usage} />
      </div>
      <div className="min-h-0 flex-1">{rail}</div>
    </aside>
  );

  if (settingsOpen) {
    return (
      <ColumnFrameProvider value={frame}>
        <div className={`flex h-full gap-2 ${glass ? "px-2" : ""}`}>
          {railColumn}
          <div className={`min-w-0 flex-1 ${frame}`}>{settings}</div>
        </div>
      </ColumnFrameProvider>
    );
  }

  return (
    <ColumnFrameProvider value={frame}>
      <div className="relative h-full">
        {/* Its columns keep only their up-and-down scrolls to themselves (`overscroll-y-`), so a
            sideways swipe over one scrolls the row; the row keeps its own, so the swipe does not
            go back in the browser's history */}
        <div
          aria-label="Board"
          className="absolute inset-0 overflow-x-auto overscroll-x-contain [scrollbar-width:none] [&::-webkit-scrollbar]:hidden"
        >
          <div className={`flex h-full w-max gap-2 ${glass ? "px-2" : ""}`}>
            {railColumn}
            {board}
            {/* Room to bring the last column out from under the side panels */}
            <div aria-hidden className="w-96 shrink-0" />
          </div>
        </div>
        {sideAt === undefined ? (
          <div className={`absolute inset-y-0 ${glass ? "right-2" : "right-0"}`}>
            <SidePanels {...side} />
          </div>
        ) : sideAt ? (
          createPortal(<SidePanels {...side} />, sideAt)
        ) : null}
      </div>
    </ColumnFrameProvider>
  );
}

type SideProps = { ticket?: ReactNode; notes: ReactNode; chat: ReactNode };

/** The side panels, the full height of their box: the open ticket, or the clone's notes over its
 *  assistant. Each is glass of its own, with its blur and its shadow. */
export function SidePanels({ ticket, notes, chat }: SideProps) {
  /** The side panel touched last: three parts of the height to its one. */
  const [focus, setFocus] = useState<"notes" | "chat">("notes");
  return (
    <div className="pointer-events-none flex h-full w-96 flex-col gap-2">
      {ticket ? (
        <section aria-label="Ticket" className={`flex-1 ${PANEL}`}>
          {ticket}
        </section>
      ) : (
        <>
          <section
            aria-label="Notes"
            onPointerDownCapture={() => setFocus("notes")}
            className={`${focus === "notes" ? "flex-[3]" : "flex-1"} ${PANEL}`}
          >
            {notes}
          </section>
          <section
            aria-label="Assistant"
            onPointerDownCapture={() => setFocus("chat")}
            className={`${focus === "chat" ? "flex-[3]" : "flex-1"} ${PANEL}`}
          >
            {chat}
          </section>
        </>
      )}
    </div>
  );
}
