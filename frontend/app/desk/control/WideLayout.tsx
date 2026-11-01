// The control panel where it is wide (over a large screen, or in its own window): columns in
// place of the tabs. A row of columns that scrolls sideways: the rail (the server's numbers,
// Settings, the accounts' usage), the tickets', then the clones'. On the right, floating over
// the row, the side panels: the selected clone's notes over its assistant (the one touched last
// is the taller), or the open ticket in one panel. The row
// scrolls under them, and its end has room to bring the last column out from under them.
// Settings takes the place of the row and the side panels, beside the rail.
//
// Over the clone's screen the side panels go in a box of the screen's (`sideAt`): there they
// also slide in over the screen without the control panel, when the mouse is at its right edge.
//
// Over the clone's screen (`glass`) each column is glass of its own, with nothing round them all,
// and the row takes the display's whole width: the columns scroll out to its edges, with the
// gap from them inside the row. In the control panel's window, on its solid ground, they are flat
// hollows. The side panels are glass in both: they are over the columns. The row's scroll bar
// is hidden: a touchpad, a wheel with Shift, or a finger scrolls it.
//
// Pure: each part comes in as a slot (`./ControlPanelContainer.tsx` fills them).
import { MonitorCog, Settings } from "lucide-react";

import { clampSideWidth, SIDE_DEFAULT } from "~/lib/sidePanelWidth";
import {
  type KeyboardEvent as ReactKeyboardEvent,
  type PointerEvent as ReactPointerEvent,
  type ReactNode,
  useEffect,
  useRef,
  useState,
} from "react";
import { createPortal } from "react-dom";

import { PopOutButton, type ServerUsage } from "../ToolRow";
import { COLUMN_GLASS, COLUMN_SOLID, ColumnFrameProvider, LIT, NO_SCROLLBARS, ON_GLASS, R_PIECE, TEXT, TEXT_2 } from "./ui";
import { ServerStats } from "./UsageView";

/** At least this wide (CSS px), the control panel takes this layout. */
export const WIDE_MIN = 1000;

/** A button in the rail's header. */
const RAIL_BUTTON = `border-black/10 bg-white/50 hover:bg-white/80 dark:border-white/10 dark:bg-white/[0.06] dark:hover:bg-white/10 ${TEXT_2}`;

/** A side panel: glass over the columns, with the usual thick tint (not the columns' denser one),
 *  so the columns under it show through. */
const PANEL = `glass glass-thick rounded-2xl pointer-events-auto flex min-h-0 flex-col overflow-hidden`;

export function WideLayout({
  width,
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
  onSideWidthCommit,
  settings,
}: {
  /** The layout's width, CSS px: the side panels' width is a part of it. */
  width: number;
  /** Over the clone's screen: each column is glass. */
  glass: boolean;
  usage: ServerUsage | null;
  settingsOpen: boolean;
  /** Open Settings, or close it. */
  onSettings: () => void;
  onPopOut?: () => void;
  /** Open the Edit layout dialog; absent: no button (the control panel's own window). */
  onLayout?: () => void;
  /** The accounts' usage. */
  rail: ReactNode;
  /** The columns, the tickets' first. */
  board: ReactNode;
  /** The side panels: the open ticket, or the clone's notes and its assistant. */
  side: SideProps;
  /** The screen's box for the side panels; absent: they float over the row's right end. */
  sideAt?: HTMLElement | null;
  /** A drag, a key or a double click set the side panels' width (a part, %). */
  onSideWidthCommit: (pct: number) => void;
  settings: ReactNode;
}) {
  const frame = glass ? COLUMN_GLASS : COLUMN_SOLID;
  /** The rail in the control panel's own window: on the window's ground, with no frame (its cards
   *  and the server's numbers have their own) */
  const railFrame = glass ? COLUMN_GLASS : "overflow-hidden";
  // The side panels' width: the container's (stored) to start, then this one's while it is set
  const [pct, setPct] = useState(side.widthPct);
  const pctRef = useRef(side.widthPct);
  useEffect(() => {
    pctRef.current = side.widthPct;
    setPct(side.widthPct);
  }, [side.widthPct]);
  const setSidePct = (next: number) => {
    pctRef.current = clampSideWidth(next);
    setPct(pctRef.current);
  };
  const sideW = sideWidthPx(width, pct);
  const hasSide = !!side.ticket || side.notes !== null;
  const panels = (
    <SidePanels
      ticket={side.ticket}
      notes={side.notes}
      chat={side.chat}
      width={sideW}
      onWidth={(px) => setSidePct((px / width) * 100)}
      onWidthDone={() => onSideWidthCommit(pctRef.current)}
      onWidthReset={() => {
        setSidePct(SIDE_DEFAULT);
        onSideWidthCommit(SIDE_DEFAULT);
      }}
    />
  );

  const railColumn = (
    <aside aria-label="Server and accounts" className={`flex h-full w-64 shrink-0 flex-col ${railFrame}`}>
      <div className="flex h-12 shrink-0 items-center gap-1.5 pl-3 pr-1.5">
        <h2 className={`min-w-0 flex-1 truncate text-sm font-semibold ${TEXT}`}>RMNG Control</h2>
        {onLayout ? (
          <button
            type="button"
            onClick={onLayout}
            aria-label="Edit layout"
            title="Edit layout"
            className={`flex size-9 items-center justify-center ${R_PIECE} border ${RAIL_BUTTON}`}
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
          className={`flex size-9 items-center justify-center ${R_PIECE} border ${
            settingsOpen
              ? `border-transparent ${LIT}`
              : RAIL_BUTTON
          }`}
        >
          <Settings aria-hidden className="size-[18px]" strokeWidth={1.75} />
        </button>
        {onPopOut ? <PopOutButton onPopOut={onPopOut} /> : null}
      </div>
      <div className="shrink-0 px-1.5 pb-1.5">
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
            {hasSide ? <div aria-hidden className="shrink-0" style={{ width: sideW }} /> : null}
          </div>
        </div>
        {sideAt === undefined ? (
          <div className={`absolute inset-y-0 ${glass ? "right-2" : "right-0"}`}>
            {panels}
          </div>
        ) : sideAt ? (
          createPortal(panels, sideAt)
        ) : null}
      </div>
    </ColumnFrameProvider>
  );
}

type SideProps = {
  /** An open ticket: it takes the whole side. */
  ticket?: ReactNode;
  /** The clone's notes card; null when no clone is selected (then no side panels). */
  notes: ReactNode | null;
  chat: ReactNode;
  /** The side's width, a part (%) of the layout's (`~/lib/sidePanelWidth`). */
  widthPct: number;
};

/** The side panels' width, CSS px: `pct` of the layout's `width`, no less than 20rem. */
export const sideWidthPx = (width: number, pct: number) => Math.round(Math.max((width * pct) / 100, 320));

/** The side panels, the full height of their box: the open ticket, or the clone's notes over its assistant. The one touched or focused last
 *  takes three quarters of the height, with an ease that goes past and comes back. A band on
 *  their left edge sets their width: a drag, the arrow keys, a double click back to the start.
 *  Each panel is glass of its own, with its blur and its shadow. */
export function SidePanels({
  ticket,
  notes,
  chat,
  width,
  onWidth,
  onWidthDone,
  onWidthReset,
}: Omit<SideProps, "widthPct"> & {
  /** CSS px. */
  width: number;
  /** The new width during a drag, CSS px from the panels' right edge to the pointer. */
  onWidth: (px: number) => void;
  onWidthDone: () => void;
  onWidthReset: () => void;
}) {
  /** The side panel touched last: three parts of the height to its one. */
  const [focus, setFocus] = useState<"notes" | "chat">("notes");
  const box = useRef<HTMLDivElement>(null);
  if (!ticket && notes === null) return null;

  /** Drag the left edge; the pointer is the band's, so the board's drags never see it. Where
   *  in the band it was taken stays under the pointer. */
  const startResize = (e: ReactPointerEvent<HTMLDivElement>) => {
    e.preventDefault();
    const band = e.currentTarget;
    band.focus();
    band.setPointerCapture(e.pointerId);
    const r = box.current!.getBoundingClientRect();
    const grab = e.clientX - r.left;
    const track = (m: globalThis.PointerEvent) => onWidth(r.right - (m.clientX - grab));
    const stop = () => {
      band.removeEventListener("pointermove", track);
      band.removeEventListener("pointerup", stop);
      band.removeEventListener("pointercancel", stop);
      onWidthDone();
    };
    band.addEventListener("pointermove", track);
    band.addEventListener("pointerup", stop);
    band.addEventListener("pointercancel", stop);
  };
  const nudge = (e: ReactKeyboardEvent<HTMLDivElement>) => {
    const step = e.key === "ArrowLeft" ? 24 : e.key === "ArrowRight" ? -24 : 0;
    if (!step) return;
    e.preventDefault();
    onWidth(width + step);
    onWidthDone();
  };
  /** Each card's share of the height; the ease goes past its end and comes back. */
  const half = (on: boolean) =>
    `flex-1 grow-0 transition-[flex-basis] duration-300 ease-[cubic-bezier(0.34,1.25,0.64,1)] ${on ? "basis-3/4" : "basis-1/4"}`;

  return (
    // The notes, the chat and a ticket scroll with no bars, as the board's row does
    // Text in them can be selected and typed (WebKit inherits the screen's `select-none`), not
    // on their buttons
    <div
      ref={box}
      className={`pointer-events-none relative flex h-full select-text flex-col gap-2 [&_button]:select-none ${NO_SCROLLBARS} ${ON_GLASS}`}
      style={{ width }}
    >
      {/* The band: over the panels' left edge, more in than out, as the shadow reads as the
          panel's. Over the panels' blur (a stacking context each) only with a z of its own */}
      <div
        role="separator"
        aria-orientation="vertical"
        aria-label="Side panel width"
        tabIndex={0}
        onPointerDown={startResize}
        onKeyDown={nudge}
        onDoubleClick={onWidthReset}
        className="pointer-events-auto absolute inset-y-0 -left-1.5 z-10 w-4 cursor-col-resize touch-none select-none outline-none"
      />
      {ticket ? (
        <section aria-label="Ticket" className={`flex-1 ${PANEL}`}>
          {ticket}
        </section>
      ) : (
        <>
          <section
            aria-label="Notes"
            onFocusCapture={() => setFocus("notes")}
            onPointerDownCapture={() => setFocus("notes")}
            className={`${half(focus === "notes")} ${PANEL}`}
          >
            {notes}
          </section>
          <section
            aria-label="Assistant"
            onFocusCapture={() => setFocus("chat")}
            onPointerDownCapture={() => setFocus("chat")}
            className={`${half(focus === "chat")} ${PANEL}`}
          >
            {chat}
          </section>
        </>
      )}
    </div>
  );
}
