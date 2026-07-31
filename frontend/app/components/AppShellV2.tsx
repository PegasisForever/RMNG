// The board dashboard. Seventy percent of the width is the clone board (control rail,
// operator columns, archived column); the last thirty is the selected clone's notes over
// its agent chat, split three-to-one in favour of whichever of the two was touched last.
//
// Pure, like AppShell: no fetch, no SSE, no browser-only imports. The notes and chat panes
// arrive as slots because both real implementations are lazy-loaded and client-only, so a
// story can fill them with fixtures.
//
// This shell only ever renders at `lg` and up. Anything narrower gets the phone UI, which
// is a different component tree the route picks in JavaScript (see `useIsMobile`), so there
// is no small-screen fallback to carry here.
import type { ReactNode } from "react";

import { Board, type BoardProps } from "~/components/Board";
import { BoardRail, type BoardRailProps } from "~/components/BoardRail";
import type { Clone } from "~/lib/types";

/** Which half of the side panel was touched last. It gets three quarters of the panel's
 *  height and the other gets a quarter, so whichever one is in use is the readable one. */
export type SideFocus = "notes" | "chat";

/** The floating cards' surface. Translucent so the columns scrolling underneath stay
 *  visible, blurred so the text on top stays readable over whatever passes below.
 *
 *  The two settings trade against each other. More opacity buys legibility and costs the
 *  effect; more blur buys legibility and costs the shapes that make it read as depth. The
 *  board is nearly white already, so this sits lower on both than it looks like it should:
 *  at 70% and `blur-xl` the cards read as plain white panels. */
const CARD =
  "bg-white/55 ring-white/70 backdrop-blur-md dark:bg-slate-900/55 dark:ring-white/10";

export interface AppShellV2Props {
  /** Everything the board draws and does, minus the rail it renders for itself. */
  board: Omit<BoardProps, "rail">;
  /** The board's fixed left column. */
  rail: BoardRailProps;
  /** The clone whose notes and chat fill the side panel; null shows the empty state. */
  selectedClone: Clone | null;
  /** Last failed action, shown as a banner above everything. */
  error?: string | null;
  /** The side panel's split: the focused half takes 3/4 of the height. Reported on the
   *  first click or focus inside either half, so the divider follows the work. */
  sideFocus: SideFocus;
  onSideFocusChange: (focus: SideFocus) => void;
  /** The notes editor for `selectedClone`. */
  notes: ReactNode;
  /** The agent chat for `selectedClone`. */
  chat: ReactNode;
  /** Modals and other overlays. They position themselves (`fixed inset-0`). */
  overlays?: ReactNode;
}

export function AppShellV2({
  board,
  rail,
  selectedClone,
  error = null,
  sideFocus,
  onSideFocusChange,
  notes,
  chat,
  overlays,
}: AppShellV2Props) {
  return (
    <div className="flex h-screen flex-col bg-slate-50 dark:bg-slate-950">
      {error ? (
        <div className="shrink-0 border-b border-red-200 bg-red-50 px-4 py-2 text-sm text-red-700 dark:border-red-900 dark:bg-red-950/40 dark:text-red-400">
          {error}
        </div>
      ) : null}

      <div className="relative flex min-h-0 flex-1">
        {/* The board takes the whole width; the side panel floats over its right edge. The
            strip's own right padding is what keeps the last column reachable: without it a
            column at the end could never be scrolled out from under the panel. */}
        <div className="min-w-0 flex-1">
          <Board {...board} rail={<BoardRail {...rail} />} gutterRight />
        </div>

        {/* Notes over chat, as two cards floating on the board. The container ignores the
            pointer so the gaps between the cards belong to the board underneath; each card
            takes it back. */}
        <aside className="pointer-events-none absolute inset-y-0 right-0 z-20 flex w-[30%] min-w-80 shrink-0 flex-col gap-3 p-3">
          {selectedClone ? (
            <>
              <section
                onFocusCapture={() => onSideFocusChange("notes")}
                onPointerDownCapture={() => onSideFocusChange("notes")}
                className={`pointer-events-auto flex min-h-0 flex-1 grow-0 flex-col overflow-hidden rounded-2xl shadow-xl ring-1 transition-[flex-basis] duration-200 ${CARD} ${
                  sideFocus === "notes" ? "basis-3/4" : "basis-1/4"
                }`}
              >
                <div className="flex shrink-0 items-baseline gap-2 px-4 pt-3">
                  <h2 className="min-w-0 flex-1 truncate text-sm font-semibold text-slate-900 dark:text-slate-100">
                    {selectedClone.id}
                  </h2>
                  <h3 className="shrink-0 text-[11px] font-semibold uppercase tracking-wide text-slate-400 dark:text-slate-500">
                    Notes
                  </h3>
                </div>
                <div className="min-h-0 flex-1 overflow-y-auto py-2">{notes}</div>
              </section>

              <section
                onFocusCapture={() => onSideFocusChange("chat")}
                onPointerDownCapture={() => onSideFocusChange("chat")}
                className={`pointer-events-auto flex min-h-0 flex-1 grow-0 flex-col overflow-hidden rounded-2xl shadow-xl ring-1 transition-[flex-basis] duration-200 ${CARD} ${
                  sideFocus === "chat" ? "basis-3/4" : "basis-1/4"
                }`}
              >
                <h3 className="shrink-0 px-4 pt-3 text-[11px] font-semibold uppercase tracking-wide text-slate-400 dark:text-slate-500">
                  Assistant
                </h3>
                {chat}
              </section>
            </>
          ) : (
            <div
              className={`pointer-events-auto flex flex-1 items-center justify-center rounded-2xl px-6 text-center text-sm text-slate-500 shadow-xl ring-1 dark:text-slate-400 ${CARD}`}
            >
              Select a clone to open its notes.
            </div>
          )}
        </aside>
      </div>

      {overlays}
    </div>
  );
}

export default AppShellV2;
