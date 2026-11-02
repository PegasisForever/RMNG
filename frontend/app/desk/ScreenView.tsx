// The screen as it is drawn, from props alone (`./FullDesk.tsx` is its container: the
// connection, the input, the layout, the windows). In order, bottom to top:
//
// - The clone's screen, at `screen` (the container moves it to keep the cursor in sight). The
//   app draws the picture on a canvas in `boxRef`; a story puts a `picture` there instead.
// - Or, for a headless clone, its terminal (`terminal`), from the top down to the keyboard, with
//   no touch surface of this view's over it (it has its own: `./TerminalScreen.tsx`).
// - Touch mode's surface: the whole display as a trackpad, or, with the keyboard docked, the
//   area above it as a touch screen. Mouse mode has none: the mouse goes to the canvas.
// - The pinned columns' dock (`dock`, where the control panel puts them: `~/lib/dock.ts`), at
//   the left edge, the full height down to the keyboard, beside the clone's screen (the
//   container puts the screen right of it). Under the control panel and its backdrop.
// - The notes over the screen: what dictation hears at the bottom, and why
//   there is no screen to use in the middle.
// - The keyboard at the bottom (touch mode), with the bar that sets its height.
// - The control panel (`controlPanel.content`: the tool row and the open tab, or, wide, the
//   columns), on a glass panel at the right under the buttons, on a backdrop that blurs and darkens the screen.
//   They come in and go out as a dialog does.
// - The right group: the floating buttons in the top-right corner, and under them, wide, the
//   side panels (the clone's notes and its assistant): with the control panel, and without it
//   when the mouse or a swipe brings them in. Each panel is glass of its own, with its blur and
//   shadow, and no backdrop. In mouse mode the group slides out to the right and back as one
//   (`away`), the buttons and the panels by the same distance; in touch mode the buttons stay,
//   and the panels slide alone (`side.shown`).
// - The alerts of clones that stopped working (`alertsRef`, where the control panel puts them),
//   in the bottom-right corner of the safe area, above the keyboard: over the control panel and
//   its dialogs.
// - A dialog, and the monitor's number while the Edit layout dialog is open in another window.
//
// The clone's screen may fill the whole display; everything else stays in the safe area (`safe`:
// clear of the clock bar, the home bar, a camera's cut-out). The keyboard's and the terminal's
// grounds reach the edges; their keys and text do not.
import { type PointerEvent, type ReactNode, type Ref, useEffect, useState } from "react";

import { MODAL_ANIMATION } from "~/components/ModalShell";
import { MonitorNumber } from "~/components/MonitorNumber";
import { MODAL_EXIT_MS } from "~/lib/useModalExit";
import type { CursorShape } from "~/lib/wire/CursorShape";

import { Cursor } from "./Cursor";
import type { DictationState } from "./dictation";
import { FLOAT_BUTTON, FLOAT_GAP, FloatingButtons } from "./FloatingButtons";
import type { Insets } from "./geometry";
import type { InputMode } from "./inputMode";
import { HANDLE_H, KeyboardHandle } from "./KeyboardHandle";
import { CONTROL_PANEL_BACKDROP, CONTROL_PANEL_DIM, CONTROL_PANEL_GLASS } from "./control/ui";
import { WIDE_MIN } from "./control/WideLayout";
import { KeyboardView, type KeyboardViewProps, type TouchKind, keyboardPanel } from "./KeyboardView";

type Box = { left: number; top: number; width: number; height: number };

/** How far past its edge a glass panel's long shadow falls, CSS px (`./glass.css`). */
const SIDE_SHADOW = 48;

/** Where the control panel goes on a display `width`×`height` with `safe` its safe area: under the
 *  buttons, FLOAT_GAP in from the safe area's sides. Wide enough for the columns
 *  (`./control/WideLayout.tsx`), it takes the safe area's whole width instead: their columns
 *  scroll out to its edges, and keep the gap inside the row. */
export function controlPanelBox(width: number, height: number, safe: Insets): Box {
  const room = width - safe.left - safe.right;
  const inset = room - 2 * FLOAT_GAP;
  const top = safe.top + FLOAT_GAP * 2 + FLOAT_BUTTON;
  return {
    left: safe.left + (inset >= WIDE_MIN ? 0 : FLOAT_GAP),
    top,
    width: inset >= WIDE_MIN ? room : inset,
    height: height - top - FLOAT_GAP - safe.bottom,
  };
}

export type ScreenViewProps = {
  /** The display, CSS px, and its unsafe edges. */
  width: number;
  height: number;
  safe: Insets;
  mode: InputMode;
  /** The clone's screen on the display, CSS px. */
  screen: { x: number; y: number; width: number; height: number };
  /** The box the app draws the clone's picture in. */
  boxRef?: Ref<HTMLDivElement>;
  /** What shows in that box instead (a story's picture of a desktop). */
  picture?: ReactNode;
  /** A headless clone's terminal, in place of the screen (over the box, which stays for the
   *  canvas); null: the screen. */
  terminal?: ReactNode;
  /** The clone's cursor, drawn in touch mode (`x`, `y` in monitor px; `scale` device px per
   *  monitor px); null: none. */
  cursor: { shape: CursorShape; x: number; y: number; scale: number } | null;
  /** Mouse mode's drawn cursor, where the CSS cursor cannot show it. */
  overlayRef?: Ref<HTMLImageElement>;
  /** A finger on the trackpad (touch mode, keyboard not docked). */
  onPad: (kind: TouchKind, e: PointerEvent<HTMLDivElement>) => void;
  /** A finger on the touch screen (touch mode, keyboard docked). */
  onTouchScreen: (kind: TouchKind, e: PointerEvent<HTMLDivElement>) => void;
  /** The keyboard, when it shows: docked (under the screen) or floating (over it), its height
   *  (CSS px), its keys, and the bar on its top that sets the height. */
  keyboard: {
    docked: boolean;
    height: number;
    keys: Omit<KeyboardViewProps, "width" | "height" | "bottom">;
    onHeight: (height: number) => void;
    onHeightDone: () => void;
  } | null;
  /** Said over the screen: dictation's state and words, why there is no screen. */
  notes: { dictation: DictationState; heard: string; note: string | null };
  /** The control panel: shown or not, what it shows (`./control/ControlPanelContainer.tsx`, for the panel's
   *  width), and how far the phone's keyboard lifts them. */
  controlPanel: {
    open: boolean;
    lift: number;
    content: ReactNode;
    panelRef?: Ref<HTMLDivElement>;
  };
  /** Wide: the side panels' box (`ref`, where the control panel puts them; or a story's `content`), and
   *  whether they are in the window (else slid out to the right); null: none. */
  side?: { shown: boolean; ref?: Ref<HTMLDivElement>; content?: ReactNode } | null;
  /** The pinned columns' dock at the left edge: its width, CSS px (its columns' and the left
   *  inset's), and its box (`ref`, where the control panel puts the columns; or a story's
   *  `content`); null: none. */
  dock?: { width: number; ref?: Ref<HTMLDivElement>; content?: ReactNode } | null;
  /** The floating buttons; null: none. */
  buttons: Parameters<typeof FloatingButtons>[0] | null;
  /** The right group (the buttons and the side panels) slid out of the window to the right,
   *  and out of reach (mouse mode). */
  away?: boolean;
  /** The box for the alerts of clones that stopped working (`./CloneAlerts.tsx`, which the
   *  control panel puts there), or a story's `content`; null: none. */
  alerts?: { ref?: Ref<HTMLDivElement>; content?: ReactNode } | null;
  /** A dialog over everything (the Edit layout dialog). */
  dialog?: ReactNode;
  /** The monitor's number, large in the middle; null: not shown. */
  identify: number | null;
  onPointerDownCapture?: (e: PointerEvent<HTMLDivElement>) => void;
};

export function ScreenView({
  width,
  height,
  safe,
  mode,
  screen,
  boxRef,
  picture,
  terminal,
  cursor,
  overlayRef,
  onPad,
  onTouchScreen,
  keyboard,
  notes,
  controlPanel,
  side,
  dock,
  buttons,
  away = false,
  alerts,
  dialog,
  identify,
  onPointerDownCapture,
}: ScreenViewProps) {
  const mouse = mode === "mouse";
  const keysH = keyboard?.height ?? 0;
  /** The screen's area: the display, or the part above the docked keyboard. */
  const areaH = keyboard?.docked ? height - keysH : height;
  /** The part of the display not under the keyboard. */
  const visible = height - keysH;
  const { bottom } = safe;
  const panel = controlPanelBox(width, height, safe);
  /** The safe area's width. */
  const safeW = width - safe.left - safe.right;
  /** Where the clone's part of the display starts: right of the dock, or the safe area's left. */
  const dockW = dock?.width ?? 0;
  const left = Math.max(safe.left, dockW);
  const controlPanelShown = useControlPanelExit(controlPanel.open);
  const place = { left: 0, top: 0, width: screen.width, height: screen.height };
  const on = (f: typeof onPad, kind: TouchKind) => (e: PointerEvent<HTMLDivElement>) => f(kind, e);

  return (
    <div className="absolute inset-0 touch-none select-none overflow-hidden bg-black" onPointerDownCapture={onPointerDownCapture}>
      {/* The clone's screen */}
      <div
        className="absolute left-0 top-0 transition-transform duration-100 ease-out"
        style={{ transform: `translate(${screen.x}px, ${screen.y}px)` }}
      >
        <div ref={boxRef} className="absolute overflow-hidden" style={place}>
          {picture}
        </div>
        <div className="pointer-events-none absolute" style={place}>
          {cursor && !mouse ? <Cursor {...cursor} /> : null}
        </div>
      </div>
      <img ref={overlayRef} alt="" hidden className="pointer-events-none fixed left-0 top-0 max-w-none" />
      {terminal ? (
        // Down to the keyboard, or to the home bar; clear of the sides' insets (the terminal's own
        // bar keeps clear of the clock bar)
        <div className="absolute top-0" style={{ left, width: width - left - safe.right, height: keyboard ? visible : height - bottom }}>
          {terminal}
        </div>
      ) : mouse ? null : keyboard?.docked ? (
        <div
          aria-label="Touch screen"
          className="absolute left-0 top-0"
          style={{ width, height: areaH }}
          onPointerDown={on(onTouchScreen, "down")}
          onPointerMove={on(onTouchScreen, "move")}
          onPointerUp={on(onTouchScreen, "up")}
          onPointerCancel={on(onTouchScreen, "cancel")}
        />
      ) : (
        <div
          aria-label="Trackpad"
          className="absolute inset-0"
          onPointerDown={on(onPad, "down")}
          onPointerMove={on(onPad, "move")}
          onPointerUp={on(onPad, "up")}
          onPointerCancel={on(onPad, "cancel")}
        />
      )}
      {dock ? (
        // A solid ground: no clone's screen under it. Its columns stay clear of the insets
        <div
          ref={dock.ref}
          data-dock
          className="absolute left-0 top-0 bg-neutral-100 text-neutral-900 dark:bg-neutral-950 dark:text-white"
          style={{
            width: dockW,
            bottom: keyboard ? keysH : 0,
            paddingLeft: safe.left,
            paddingTop: safe.top,
            paddingBottom: keyboard ? 0 : bottom,
          }}
        >
          {dock.content}
        </div>
      ) : null}
      <div
        className="pointer-events-none absolute"
        style={{ left, top: safe.top, width: width - left - safe.right, height: visible - safe.top }}
      >
        <ScreenNotes {...notes} bottom={(keyboard ? 0 : bottom) + 12} />
      </div>

      {keyboard ? (
        <div
          className={`absolute bottom-0 left-0 ${keyboardPanel(keyboard.docked)}`}
          style={{ width, height: keysH }}
        >
          <KeyboardHandle height={keysH} onHeight={keyboard.onHeight} onDone={keyboard.onHeightDone} />
          <div style={{ marginLeft: safe.left }}>
            <KeyboardView width={safeW} height={keysH - HANDLE_H} bottom={bottom} {...keyboard.keys} />
          </div>
        </div>
      ) : null}

      {/* Under the control panel and the buttons while the control panel is open: the screen blurred and
          darkened, fading in and out with them as a dialog's backdrop does */}
      {controlPanelShown ? (
        <div
          aria-hidden
          className={`absolute inset-0 z-30 ${panel.width >= WIDE_MIN ? CONTROL_PANEL_BACKDROP : CONTROL_PANEL_DIM} ${
            controlPanel.open ? MODAL_ANIMATION.backdrop.enter : MODAL_ANIMATION.backdrop.exit
          }`}
        />
      ) : null}

      {/* The control panel: mounted from the start, so its data is there when it opens */}
      <div
        ref={controlPanel.panelRef}
        data-control-panel
        className={`absolute z-40 transition-transform duration-300 ease-out ${controlPanelShown ? "" : "hidden"}`}
        style={{ ...panel, transform: `translateY(${-controlPanel.lift}px)` }}
      >
        {/* Narrow, on one glass panel (`./GLASS.md`); wide, each column is glass of its own, with
            nothing round them all. What is fixed in it (a sheet, a dialog) takes its space. */}
        <div
          className={`absolute inset-0 ${panel.width >= WIDE_MIN ? "" : `overflow-hidden ${CONTROL_PANEL_GLASS}`} ${
            // In and out as a dialog is: a fade and a zoom
            controlPanel.open ? MODAL_ANIMATION.panel.enter : controlPanelShown ? MODAL_ANIMATION.panel.exit : ""
          }`}
        >
          {controlPanel.content}
        </div>
      </div>

      {/* The right group: one column at the right edge, the buttons over the side panels. Out,
          it moves right by its own width, its gap and its shadow: the buttons and the panels go
          the same distance. Out of reach of Tab while out. */}
      <div
        data-right-group
        inert={away}
        className={`pointer-events-none absolute inset-y-0 right-0 z-40 flex flex-col items-end transition-transform duration-300 ${
          // Out: a slow start, so the buttons (at the very edge) are seen going with the panels
          // rather than gone in the first frames; in: a fast start that slows at the end
          away ? "ease-in" : "ease-out"
        }`}
        style={{
          gap: FLOAT_GAP,
          paddingTop: FLOAT_GAP + safe.top,
          paddingRight: FLOAT_GAP + safe.right,
          paddingBottom: FLOAT_GAP + bottom,
          transform: away ? `translateX(calc(100% + ${SIDE_SHADOW}px))` : undefined,
        }}
      >
        {buttons ? <FloatingButtons {...buttons} /> : null}
        {side ? (
          // Touch mode slides the panels alone: past the edge by their gap and their shadow
          <div
            ref={side.ref}
            data-side-panels
            inert={!side.shown}
            className="pointer-events-none min-h-0 flex-1 transition-transform duration-300 ease-out"
            style={{ transform: side.shown ? "none" : `translateX(calc(100% + ${FLOAT_GAP + SIDE_SHADOW}px))` }}
          >
            {side.content}
          </div>
        ) : null}
      </div>
      {alerts ? (
        // Above the keyboard, or the home bar; touches beside the alerts go through
        <div
          ref={alerts.ref}
          data-clone-alerts
          className="pointer-events-none absolute z-[55] flex flex-col items-end justify-end"
          style={{
            top: safe.top + FLOAT_GAP,
            left: safe.left + FLOAT_GAP,
            right: safe.right + FLOAT_GAP,
            bottom: (keyboard ? keysH : bottom) + FLOAT_GAP,
          }}
        >
          {alerts.content}
        </div>
      ) : null}
      {dialog}
      <MonitorNumber monitor={identify} />
    </div>
  );
}

/** What is said over the clone's screen: the words dictation hears (at the bottom, `bottom` px
 *  up), and a note when there is no screen to use (in the middle). Touches go through both. */
function ScreenNotes({
  dictation,
  heard,
  note,
  bottom,
}: ScreenViewProps["notes"] & { bottom: number }) {
  const low = dictation === "listening" || heard ? heard || "Listening…" : null;
  return (
    <>
      {low ? (
        <p className="pointer-events-none absolute inset-x-0 flex justify-center px-6" style={{ bottom }}>
          <span className="glass glass-thick max-w-full truncate rounded-full px-4 py-2 text-sm">
            {low}
          </span>
        </p>
      ) : null}
      {note ? (
        <p className="pointer-events-none absolute inset-0 flex items-center justify-center p-6">
          <span className="glass glass-thick rounded-full px-4 py-2 text-center text-sm">
            {note}
          </span>
        </p>
      ) : null}
    </>
  );
}

/** Whether the control panel shows: while `open`, and for its exit animation after (the dialogs' one,
 *  `MODAL_EXIT_MS`; none where the screen asks for reduced motion). */
function useControlPanelExit(open: boolean) {
  const [shown, setShown] = useState(open);
  useEffect(() => {
    if (open) return setShown(true);
    const reduced = typeof window.matchMedia === "function" && window.matchMedia("(prefers-reduced-motion: reduce)").matches;
    if (reduced) return setShown(false);
    const t = window.setTimeout(() => setShown(false), MODAL_EXIT_MS);
    return () => window.clearTimeout(t);
  }, [open]);
  return open || shown;
}
