// The board dashboard. The clone board (control rail, operator columns) takes the width;
// the selected clone's notes over its agent chat float on its right edge, split
// three-to-one in favour of whichever of the two was touched last. The operator drags the
// panel's left edge to set how much of the width it takes.
//
// Pure, like AppShell: no fetch, no SSE, no browser-only imports. The notes and chat panes
// arrive as slots because both real implementations are lazy-loaded and client-only, so a
// story can fill them with fixtures.
//
// Below the `lg` breakpoint the two columns cannot share the screen, so the top bar's
// segmented control picks one of board / notes / chat instead.
import {
  type CSSProperties,
  type KeyboardEvent as ReactKeyboardEvent,
  type PointerEvent as ReactPointerEvent,
  type ReactNode,
  useEffect,
  useRef,
  useState,
} from "react";

import { Board, type BoardProps } from "~/components/Board";
import { BoardRail, type BoardRailProps } from "~/components/BoardRail";
import type { Clone } from "~/lib/types";

/** Which pane owns the screen below `lg`. Above it, the board and the side panel are
 *  both always visible. */
export type ShellPane = "board" | "notes" | "chat";

/** Which half of the side panel was touched last. It gets three quarters of the panel's
 *  height and the other gets a quarter, so whichever one is in use is the readable one. */
export type SideFocus = "notes" | "chat";

/** The floating cards' surface, and only theirs. The board scrolling underneath is what
 *  makes it read as glass, so the dialogs deliberately keep their opaque panel: over a
 *  dimmed backdrop the same material just looks murky.
 *
 *  Opacity and blur pull apart here rather than trade against each other. The fill is low so
 *  the columns show through, and the blur is what keeps text on top legible over them.
 *  Raising the fill is what kills it: at 70% the cards read as plain white panels, because
 *  the board is nearly white to begin with. The shadow is wide and faint for the same
 *  reason — a tight dark one draws a hard line the glass does not have.
 *
 *  Dark mode keeps that geometry but runs it in black at roughly five times the alpha. A
 *  shadow only shows as the difference between it and the surface under it, and against a
 *  slate-950 board the light-mode values come out to nothing. */
const CARD =
  "border border-slate-900/10 bg-white/25 shadow-[0_2px_16px_rgb(15_23_42_/_0.05),0_10px_50px_rgb(15_23_42_/_0.07)] backdrop-blur-sm dark:border-white/10 dark:bg-slate-900/25 dark:shadow-[0_2px_16px_rgb(0_0_0_/_0.26),0_10px_50px_rgb(0_0_0_/_0.35)]";

/** The side panel's width, as a percentage of the shell. The board pads its own right edge
 *  by the same number, so the two can never disagree and leave the last column stranded
 *  under the cards — which is why the panel has no pixel floor of its own. */
const SIDE_MIN = 20;
const SIDE_MAX = 60;
const SIDE_DEFAULT = 30;
const SIDE_KEY = "rmng.sidePanelWidth";

const clampSide = (pct: number) => Math.min(SIDE_MAX, Math.max(SIDE_MIN, pct));

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
  const [sideWidth, setSideWidth] = useState(SIDE_DEFAULT);
  // The live width during a drag. `sideWidth` lags a render behind, so the pointer handler
  // and the persist-on-release both read this instead.
  const widthRef = useRef(SIDE_DEFAULT);
  // The board and the panel together, which is what a percentage is a percentage of.
  const splitRef = useRef<HTMLDivElement | null>(null);

  // Restore after mount rather than in the initial state, so the server and the first
  // client render agree on the default.
  useEffect(() => {
    const stored = Number(window.localStorage.getItem(SIDE_KEY));
    if (!Number.isFinite(stored) || stored <= 0) return;
    const next = clampSide(stored);
    widthRef.current = next;
    setSideWidth(next);
  }, []);

  const applyWidth = (pct: number) => {
    const next = clampSide(pct);
    widthRef.current = next;
    setSideWidth(next);
  };

  const persistWidth = () => window.localStorage.setItem(SIDE_KEY, String(Math.round(widthRef.current)));

  /** Drag the panel's left edge. The pointer is captured by the handle, so the board's own
   *  drag sensor never sees the move and the pointer can leave the handle without dropping
   *  the resize. */
  const startResize = (event: ReactPointerEvent<HTMLDivElement>) => {
    event.preventDefault();
    const handle = event.currentTarget;
    const pointerId = event.pointerId;
    // `preventDefault` above suppresses the focus a click would normally give the handle,
    // and the arrow keys are useless without it.
    handle.focus();
    handle.setPointerCapture(pointerId);

    const track = (moved: globalThis.PointerEvent) => {
      const split = splitRef.current?.getBoundingClientRect();
      if (!split || split.width === 0) return;
      applyWidth(((split.right - moved.clientX) / split.width) * 100);
    };
    const stop = () => {
      handle.removeEventListener("pointermove", track);
      handle.removeEventListener("pointerup", stop);
      handle.removeEventListener("pointercancel", stop);
      if (handle.hasPointerCapture(pointerId)) handle.releasePointerCapture(pointerId);
      persistWidth();
    };

    handle.addEventListener("pointermove", track);
    handle.addEventListener("pointerup", stop);
    handle.addEventListener("pointercancel", stop);
  };

  /** Arrow keys move the edge in whole steps, so the panel is resizable without a pointer. */
  const nudgeResize = (event: ReactKeyboardEvent<HTMLDivElement>) => {
    const step = event.key === "ArrowLeft" ? 2 : event.key === "ArrowRight" ? -2 : 0;
    if (step === 0) return;
    event.preventDefault();
    applyWidth(widthRef.current + step);
    persistWidth();
  };

  const resetResize = () => {
    applyWidth(SIDE_DEFAULT);
    persistWidth();
  };

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

      {/* One variable sets the panel's width and the board's right padding, so the gutter
          always matches whatever the operator dragged the edge to. */}
      <div
        ref={splitRef}
        className="relative flex min-h-0 flex-1"
        style={{ "--side-panel-w": `${sideWidth}%` } as CSSProperties}
      >
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
          className={`w-full shrink-0 flex-col gap-3 p-3 lg:pointer-events-none lg:absolute lg:inset-y-0 lg:right-0 lg:z-20 lg:flex lg:w-[var(--side-panel-w,30%)] ${
            pane === "board" ? "hidden" : "flex"
          }`}
        >
          {/* The resize grip: a 16px hit area over the cards' own left edge, so the thing
              the operator drags is the edge they can see and it grabs from either side. It
              sits 6px out and 10px in rather than evenly, because the card's wide shadow
              reads as part of the card and an even split feels outside-heavy. The panel's
              own 12px padding is what leaves room for the outer half without reaching onto
              the board.
              `z-10` is load-bearing. The cards' `backdrop-filter` gives each one a stacking
              context, which promotes them to the same paint step as this positioned grip;
              tree order would then put them on top and swallow the half of the band that
              overlaps a card.
              Nothing is drawn here — the cursor is the affordance, and the focus ring is
              off because it would flash over the card on every grab. Double-click puts the
              width back. */}
          <div
            role="separator"
            aria-orientation="vertical"
            aria-label="Side panel width"
            aria-valuenow={Math.round(sideWidth)}
            aria-valuemin={SIDE_MIN}
            aria-valuemax={SIDE_MAX}
            tabIndex={0}
            onPointerDown={startResize}
            onKeyDown={nudgeResize}
            onDoubleClick={resetResize}
            className="pointer-events-auto absolute inset-y-0 left-1.5 z-10 hidden w-4 cursor-col-resize touch-none select-none outline-none lg:block"
          />

          {/* The two cards swap heights on focus. The easing overshoots its target and
              settles back, which is what makes the swap read as one card pushing the other
              down rather than as both being redrawn. The pair briefly sums past 100% at the
              peak; `flex-shrink` absorbs it, so nothing clips. */}
          {selectedClone ? (
            <>
              <section
                onFocusCapture={() => onSideFocusChange("notes")}
                onPointerDownCapture={() => onSideFocusChange("notes")}
                className={`pointer-events-auto min-h-0 flex-1 flex-col overflow-hidden rounded-2xl transition-[flex-basis] duration-300 ease-[cubic-bezier(0.34,1.25,0.64,1)] lg:flex lg:grow-0 ${CARD} ${
                  pane === "chat" ? "hidden" : "flex"
                } ${sideFocus === "notes" ? "lg:basis-3/4" : "lg:basis-1/4"}`}
              >
                <h2 className="shrink-0 truncate px-4 pt-3 text-sm font-semibold text-slate-900 dark:text-slate-100">
                  {selectedClone.id}
                </h2>
                <div className="min-h-0 flex-1 overflow-y-auto py-2">{notes}</div>
              </section>

              <section
                onFocusCapture={() => onSideFocusChange("chat")}
                onPointerDownCapture={() => onSideFocusChange("chat")}
                className={`pointer-events-auto min-h-0 flex-1 flex-col overflow-hidden rounded-2xl transition-[flex-basis] duration-300 ease-[cubic-bezier(0.34,1.25,0.64,1)] lg:flex lg:grow-0 ${CARD} ${
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
              className={`pointer-events-auto flex flex-1 items-center justify-center rounded-2xl px-6 text-center text-sm text-slate-500 dark:text-slate-400 ${CARD}`}
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
