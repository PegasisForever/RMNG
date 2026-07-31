// The board dashboard. Seventy percent of the width is the clone board (control rail,
// operator columns, archived column); the last thirty is the selected clone's notes over
// its agent chat, split three-to-one in favour of whichever of the two was touched last.
//
// Pure, like AppShell: no fetch, no SSE, no browser-only imports. The notes and chat panes
// arrive as slots because both real implementations are lazy-loaded and client-only, so a
// story can fill them with fixtures.
//
// Below the `lg` breakpoint the two columns cannot share the screen, so the top bar's
// segmented control picks one of board / notes / chat instead.
import type { ReactNode } from "react";

import { Board, type BoardProps } from "~/components/Board";
import { BoardRail, type BoardRailProps } from "~/components/BoardRail";
import type { Clone } from "~/lib/types";

/** Which pane owns the screen below `lg`. Above it, the board and the side panel are
 *  both always visible. */
export type ShellPane = "board" | "notes" | "chat";

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
  pane: ShellPane;
  onPaneChange: (pane: ShellPane) => void;
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
  pane,
  onPaneChange,
  sideFocus,
  onSideFocusChange,
  notes,
  chat,
  overlays,
}: AppShellV2Props) {
  const tab = (value: ShellPane, label: string) => (
    <button
      type="button"
      onClick={() => onPaneChange(value)}
      aria-pressed={pane === value}
      className={`rounded px-3 py-1 ${
        pane === value
          ? "bg-white text-slate-900 shadow-sm dark:bg-slate-700 dark:text-slate-100"
          : "text-slate-500 hover:text-slate-700 dark:text-slate-400 dark:hover:text-slate-200"
      }`}
    >
      {label}
    </button>
  );

  return (
    <div className="flex h-screen flex-col bg-slate-50 dark:bg-slate-950">
      {error ? (
        <div className="shrink-0 border-b border-red-200 bg-red-50 px-4 py-2 text-sm text-red-700 dark:border-red-900 dark:bg-red-950/40 dark:text-red-400">
          {error}
        </div>
      ) : null}

      {/* Pane switcher: the only header below `lg`, where board and side panel cannot share
          the screen. */}
      <div className="flex shrink-0 items-center gap-2 border-b border-slate-200 bg-white px-3 py-2 lg:hidden dark:border-slate-700 dark:bg-slate-900">
        <span className="min-w-0 flex-1 truncate text-sm font-semibold text-slate-800 dark:text-slate-100">
          {selectedClone ? selectedClone.id : "rmng control"}
        </span>
        <div className="flex shrink-0 gap-0.5 rounded-md bg-slate-100 p-0.5 text-xs font-medium dark:bg-slate-800">
          {tab("board", "Board")}
          {tab("notes", "Notes")}
          {tab("chat", "Chat")}
        </div>
      </div>

      <div className="relative flex min-h-0 flex-1">
        {/* The board takes the whole width; the side panel floats over its right edge. The
            strip's own right padding is what keeps the last column reachable: without it a
            column at the end could never be scrolled out from under the panel. */}
        <div className={`min-w-0 flex-1 lg:block ${pane === "board" ? "block" : "hidden"}`}>
          <Board {...board} rail={<BoardRail {...rail} />} gutterRight />
        </div>

        {/* Notes over chat, as two cards floating on the board. The container ignores the
            pointer so the gaps between the cards belong to the board underneath; each card
            takes it back. Below `lg` there is no room to float anything, so the panel drops
            back into the flow as an ordinary full-width pane. */}
        <aside
          className={`w-full shrink-0 flex-col gap-3 p-3 lg:pointer-events-none lg:absolute lg:inset-y-0 lg:right-0 lg:z-20 lg:flex lg:w-[30%] lg:min-w-80 ${
            pane === "board" ? "hidden" : "flex"
          }`}
        >
          {selectedClone ? (
            <>
              <section
                onFocusCapture={() => onSideFocusChange("notes")}
                onPointerDownCapture={() => onSideFocusChange("notes")}
                className={`pointer-events-auto min-h-0 flex-1 flex-col overflow-hidden rounded-2xl shadow-xl ring-1 transition-[flex-basis] duration-200 lg:flex lg:grow-0 ${CARD} ${
                  pane === "chat" ? "hidden" : "flex"
                } ${sideFocus === "notes" ? "lg:basis-3/4" : "lg:basis-1/4"}`}
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
                className={`pointer-events-auto min-h-0 flex-1 flex-col overflow-hidden rounded-2xl shadow-xl ring-1 transition-[flex-basis] duration-200 lg:flex lg:grow-0 ${CARD} ${
                  pane === "notes" ? "hidden" : "flex"
                } ${sideFocus === "chat" ? "lg:basis-3/4" : "lg:basis-1/4"}`}
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
