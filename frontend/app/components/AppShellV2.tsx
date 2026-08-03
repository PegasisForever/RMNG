// The board dashboard. The clone board (control rail, operator columns) takes the width;
// the selected clone's notes over its agent chat float on its right edge, split
// three-to-one in favour of whichever of the two was touched last. The operator drags the
// panel's left edge to set how much of the width it takes.
//
// Pure, like AppShell: no fetch, no SSE, no browser-only imports, and it reads and writes no
// storage. From `~/lib/sidePanelWidth` it takes only the pure half, `clampSideWidth` and the
// three width constants. That module's `storedSideWidth` and `rememberSideWidth` are the ones
// that touch localStorage, they belong to the container, and neither is called here.
//
// The notes and chat panes arrive as slots because both real implementations are lazy-loaded
// and client-only, so a story can fill them with fixtures.
//
// Dialogs are NOT a slot. Every one of them is `fixed inset-0`, so the shell never lays one
// out and a slot would be pass-through and nothing else. The container mounts them beside
// this shell, the way the phone's container does, and this shell reports the tap that opens
// one through an ordinary callback.
//
// This shell only ever renders at `lg` and up. Anything narrower gets the phone UI, which
// is a different component tree the route picks in JavaScript (see `useIsMobile`), so there
// is no small-screen fallback to carry here.
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
import { GLASS_FILL, GLASS_OUTLINE, GLASS_SHADOW } from "~/lib/glass";
import { clampSideWidth, SIDE_DEFAULT, SIDE_MAX, SIDE_MIN } from "~/lib/sidePanelWidth";
import type { Clone } from "~/lib/types";

/** Which half of the side panel was touched last. It gets three quarters of the panel's
 *  height and the other gets a quarter, so whichever one is in use is the readable one. */
export type SideFocus = "notes" | "chat";

/** The floating cards' surface. The board scrolling underneath is what makes it read as
 *  glass, so the dialogs deliberately keep their opaque panel: over a dimmed backdrop the
 *  same material just looks murky. */
const CARD = `${GLASS_OUTLINE} ${GLASS_FILL} ${GLASS_SHADOW}`;

export interface AppShellV2Props {
  /** Everything the board draws and does, minus the rail it renders for itself. */
  board: Omit<BoardProps, "rail">;
  /** The board's fixed left column, minus the locale the shell hands it. */
  rail: Omit<BoardRailProps, "locale">;
  /** Formats the rail's usage-bar reset tooltips. Read once by the container (the operator's
   *  `navigator.language`) and handed down, so the preview's toolbar can pin it. */
  locale: string;
  /** The clone whose notes and chat fill the side panel; null shows the empty state. */
  selectedClone: Clone | null;
  /** A selected Linear ticket takes the side panel for itself, as one card instead of two.
   *  A ticket is a different subject from the clone, not a third thing about it, so the two
   *  do not share the space. The clone stays selected underneath and its cards come back
   *  when the ticket closes. */
  ticket?: ReactNode;
  /** Last failed action, shown as a banner above everything. */
  error?: string | null;
  /** The side panel's split: the focused half takes 3/4 of the height. Reported on the
   *  first click or focus inside either half, so the divider follows the work. */
  sideFocus: SideFocus;
  onSideFocusChange: (focus: SideFocus) => void;
  /** The notes editor for `selectedClone`. */
  notes: ReactNode;
  /** `selectedClone`'s own Linear ticket, which takes the notes card's place when it has one.
   *  It arrives as its own slot rather than through `notes` because the card is drawn
   *  differently: a ticket names itself in its own header, so the card drops the clone id and
   *  the padding it wraps notes in, and the panel gets the whole card the way it does when a
   *  ticket takes the side panel outright. */
  cloneTicket?: ReactNode;
  /** The agent chat for `selectedClone`. */
  chat: ReactNode;
  /** The width the panel opens at, as a percentage of the shell. The container resolves it
   *  (the operator's remembered one, or the default) and the drag below takes it from there. */
  initialSideWidth: number;
  /** A resize settled: dropped, nudged with an arrow key, or reset by double-click. The
   *  container is what remembers it. Fires with the settled percentage, unrounded. */
  onSideWidthCommit: (pct: number) => void;
}

export function AppShellV2({
  board,
  rail,
  locale,
  selectedClone,
  ticket,
  error = null,
  sideFocus,
  onSideFocusChange,
  notes,
  cloneTicket,
  chat,
  initialSideWidth,
  onSideWidthCommit,
}: AppShellV2Props) {
  const [sideWidth, setSideWidth] = useState(initialSideWidth);
  // The live width during a drag. `sideWidth` lags a render behind, so the pointer handler
  // and the persist-on-release both read this instead.
  const widthRef = useRef(initialSideWidth);
  // The board and the panel together, which is what a percentage is a percentage of.
  const splitRef = useRef<HTMLDivElement | null>(null);
  // The floating panel itself. A drag needs its left edge, which is not the same as the
  // percentage: the width has a 20rem floor the percentage does not know about.
  const panelRef = useRef<HTMLElement | null>(null);

  // Adopt the width the container resolved. It arrives a render after mount rather than in
  // the initial state, so the server and the first client render agree on the default.
  useEffect(() => {
    widthRef.current = initialSideWidth;
    setSideWidth(initialSideWidth);
  }, [initialSideWidth]);

  const applyWidth = (pct: number) => {
    const next = clampSideWidth(pct);
    widthRef.current = next;
    setSideWidth(next);
  };

  const persistWidth = () => onSideWidthCommit(widthRef.current);

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

    // How far inside the panel's left edge the pointer landed. The grip is a band, not a
    // line, so the pointer starts several pixels right of the edge it moves. Treating the
    // pointer as the edge would snap the panel narrower by that much on the first move.
    // Holding the offset keeps the edge under the same part of the grip for the whole drag.
    const panel = panelRef.current?.getBoundingClientRect();
    const grab = panel ? event.clientX - panel.left : 0;

    const track = (moved: globalThis.PointerEvent) => {
      const split = splitRef.current?.getBoundingClientRect();
      if (!split || split.width === 0) return;
      applyWidth(((split.right - (moved.clientX - grab)) / split.width) * 100);
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

  return (
    <div className="flex h-screen flex-col bg-slate-50 dark:bg-slate-950">
      {error ? (
        <div className="shrink-0 border-b border-red-200 bg-red-50 px-4 py-2 text-sm text-red-700 dark:border-red-900 dark:bg-red-950/40 dark:text-red-400">
          {error}
        </div>
      ) : null}

      {/* One variable sets the panel's width and the board's right padding, so the gutter
          always matches whatever the operator dragged the edge to. */}
      <div
        ref={splitRef}
        className="relative flex min-h-0 flex-1"
        style={{ "--side-panel-w": `${sideWidth}%` } as CSSProperties}
      >
        {/* The board takes the whole width; the side panel floats over its right edge. The
            strip's own right padding is what keeps the last column reachable: without it a
            column at the end could never be scrolled out from under the panel. With nothing
            selected there is no panel, so the board keeps that width too. */}
        <div className="min-w-0 flex-1">
          <Board
            {...board}
            rail={<BoardRail {...rail} locale={locale} />}
            gutterRight={!!selectedClone}
          />
        </div>

        {/* Notes over chat, as two cards floating on the board. The container ignores the
            pointer so the gaps between the cards belong to the board underneath; each card
            takes it back.

            The width is the same expression the board's gutter uses, so the two cannot
            disagree and strand the last column: the custom property is what the operator
            drags, and 20rem is the floor below which the cards stop being usable. */}
        {selectedClone || ticket ? (
          <aside
            ref={panelRef}
            className="pointer-events-none absolute inset-y-0 right-0 z-20 flex w-[max(var(--side-panel-w,30%),20rem)] shrink-0 flex-col gap-3 p-3"
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
              className="pointer-events-auto absolute inset-y-0 left-1.5 z-10 w-4 cursor-col-resize touch-none select-none outline-none"
            />

            {ticket ? (
              <section className={`pointer-events-auto flex min-h-0 flex-1 flex-col overflow-hidden rounded-2xl ${CARD}`}>
                {ticket}
              </section>
            ) : null}

            {/* The two cards swap heights on focus. The easing overshoots its target and
                settles back, which is what makes the swap read as one card pushing the other
                down rather than as both being redrawn. The pair briefly sums past 100% at the
                peak; `flex-shrink` absorbs it, so nothing clips. */}
            {!ticket && selectedClone ? (
            <>
            <section
              onFocusCapture={() => onSideFocusChange("notes")}
              onPointerDownCapture={() => onSideFocusChange("notes")}
              className={`pointer-events-auto flex min-h-0 flex-1 grow-0 flex-col overflow-hidden rounded-2xl transition-[flex-basis] duration-300 ease-[cubic-bezier(0.34,1.25,0.64,1)] ${CARD} ${
                sideFocus === "notes" ? "basis-3/4" : "basis-1/4"
              }`}
            >
              {/* A ticket fills the card on its own, exactly as it does when one takes the
                  whole side panel. No clone id over it: the ticket's own header is the card's
                  title, and a hostname above that would name a second subject for a card that
                  has one. */}
              {cloneTicket ?? (
                <>
                  <h2 className="shrink-0 truncate px-4 pt-3 text-sm font-semibold text-slate-900 dark:text-slate-100">
                    {selectedClone.id}
                  </h2>
                  <div className="min-h-0 flex-1 overflow-y-auto py-2">{notes}</div>
                </>
              )}
            </section>

            <section
              onFocusCapture={() => onSideFocusChange("chat")}
              onPointerDownCapture={() => onSideFocusChange("chat")}
              className={`pointer-events-auto flex min-h-0 flex-1 grow-0 flex-col overflow-hidden rounded-2xl transition-[flex-basis] duration-300 ease-[cubic-bezier(0.34,1.25,0.64,1)] ${CARD} ${
                sideFocus === "chat" ? "basis-3/4" : "basis-1/4"
              }`}
            >
              <h3 className="shrink-0 px-4 pt-3 text-[11px] font-semibold uppercase tracking-wide text-slate-400 dark:text-slate-500">
                Assistant
              </h3>
              {chat}
            </section>
            </>
            ) : null}
          </aside>
        ) : null}
      </div>
    </div>
  );
}

export default AppShellV2;
