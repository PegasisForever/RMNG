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

      <div className="flex min-h-0 flex-1">
        <div
          className={`min-w-0 flex-1 lg:block ${pane === "board" ? "block" : "hidden"}`}
        >
          <Board {...board} rail={<BoardRail {...rail} />} />
        </div>

        {/* Side panel: notes over chat, each taking half the height. */}
        <aside
          className={`w-full shrink-0 flex-col border-l border-slate-200 bg-white lg:flex lg:w-[30%] lg:min-w-80 dark:border-slate-700 dark:bg-slate-900 ${
            pane === "board" ? "hidden" : "flex"
          }`}
        >
          {selectedClone ? (
            <>
              <div className="hidden shrink-0 items-baseline gap-2 border-b border-slate-100 px-4 py-3 lg:flex dark:border-slate-800">
                <h2 className="min-w-0 flex-1 truncate text-sm font-semibold text-slate-900 dark:text-slate-100">
                  {selectedClone.id}
                </h2>
              </div>

              <section
                onFocusCapture={() => onSideFocusChange("notes")}
                onPointerDownCapture={() => onSideFocusChange("notes")}
                className={`min-h-0 flex-1 flex-col overflow-hidden transition-[flex-basis] duration-200 lg:flex lg:grow-0 ${
                  pane === "chat" ? "hidden" : "flex"
                } ${sideFocus === "notes" ? "lg:basis-3/4" : "lg:basis-1/4"}`}
              >
                <h3 className="shrink-0 px-4 pt-3 text-[11px] font-semibold uppercase tracking-wide text-slate-400 dark:text-slate-500">
                  Notes
                </h3>
                <div className="min-h-0 flex-1 overflow-y-auto py-2">{notes}</div>
              </section>

              <section
                onFocusCapture={() => onSideFocusChange("chat")}
                onPointerDownCapture={() => onSideFocusChange("chat")}
                className={`min-h-0 flex-1 flex-col overflow-hidden border-t border-slate-200 bg-slate-50/50 transition-[flex-basis] duration-200 lg:flex lg:grow-0 dark:border-slate-700 dark:bg-slate-900/50 ${
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
            <div className="flex flex-1 items-center justify-center px-6 text-center text-sm text-slate-400 dark:text-slate-500">
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
