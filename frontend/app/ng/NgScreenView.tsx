// The /ng screen as it is drawn, from props alone (`./FullDesk.tsx` is its container: the
// connection, the input, the layout, the windows). In order, bottom to top:
//
// - The clone's screen, at `screen` (the container moves it to keep the cursor in sight). The
//   app draws the picture on a canvas in `boxRef`; a story puts a `picture` there instead.
// - Or, for a headless clone, its terminal (`terminal`), from the top down to the keyboard, with
//   no touch surface of this view's over it (it has its own: `./NgTerminal.tsx`).
// - Touch mode's surface: the whole display as a trackpad, or, with the keyboard docked, the
//   area above it as a touch screen. Mouse mode has none: the mouse goes to the canvas.
// - The notes over the screen: what dictation hears at the bottom, and why
//   there is no screen to use in the middle.
// - The keyboard at the bottom (touch mode), with the bar that sets its height.
// - The control panel (`controlPanel.content`: the tool row and the open page, or the dashboard's layout), on a
//   glass panel at the right under the buttons, on a backdrop that blurs and darkens the screen.
//   They come in and go out as a dialog does.
// - Wide, the side panels (the clone's notes and its assistant) at the right, under the buttons:
//   with the control panel, and without it when the mouse brings them in. Each is glass of its own,
//   with its blur and shadow, and no backdrop; they slide in from the right and out again.
// - The floating buttons, a dialog, and the monitor's number while the Edit layout dialog is
//   open in another window.
import { type PointerEvent, type ReactNode, type Ref, useEffect, useState } from "react";

import { MODAL_ANIMATION } from "~/components/ModalShell";
import { MonitorNumber } from "~/components/MonitorNumber";
import { MODAL_EXIT_MS } from "~/lib/useModalExit";
import type { CursorShape } from "~/lib/wire/CursorShape";

import { Cursor } from "./Cursor";
import type { DictationState } from "./dictation";
import { FLOAT_BUTTON, FLOAT_GAP, FloatingButtons } from "./FloatingButtons";
import { GAP } from "./geometry";
import type { InputMode } from "./inputMode";
import { HANDLE_H, KeyboardHandle } from "./KeyboardHandle";
import { CONTROL_PANEL_BACKDROP, CONTROL_PANEL_GLASS } from "./control/ui";
import { WIDE_MIN } from "./control/WideLayout";
import { KeyboardView, type KeyboardViewProps, type TouchKind, keyboardPanel } from "./KeyboardView";

type Box = { left: number; top: number; width: number; height: number };

/** How far past its edge a glass panel's long shadow falls, CSS px (`./glass.css`). */
const SIDE_SHADOW = 48;

/** Where the control panel goes on a display `width`×`height` with `bottom` kept for the home bar: under
 *  the buttons, FLOAT_GAP in from the sides. Wide enough for the dashboard's layout
 *  (`./control/WideLayout.tsx`), they take the display's whole width instead: their columns scroll
 *  out to its edges, and keep the gap inside the row. */
export function controlPanelBox(width: number, height: number, bottom: number): Box {
  const inset = width - 2 * FLOAT_GAP;
  return {
    left: inset >= WIDE_MIN ? 0 : FLOAT_GAP,
    top: FLOAT_GAP * 2 + FLOAT_BUTTON,
    width: inset >= WIDE_MIN ? width : inset,
    height: height - FLOAT_GAP * 3 - FLOAT_BUTTON - bottom,
  };
}

export type NgScreenViewProps = {
  /** The display, CSS px, and the home bar's safe-area inset. */
  width: number;
  height: number;
  bottom: number;
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
  /** The floating buttons (`away`: slid up out of the window); null: none. */
  buttons: Parameters<typeof FloatingButtons>[0] | null;
  /** A dialog over everything (the Edit layout dialog). */
  dialog?: ReactNode;
  /** The monitor's number, large in the middle; null: not shown. */
  identify: number | null;
  onPointerDownCapture?: (e: PointerEvent<HTMLDivElement>) => void;
};

export function NgScreenView({
  width,
  height,
  bottom,
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
  buttons,
  dialog,
  identify,
  onPointerDownCapture,
}: NgScreenViewProps) {
  const mouse = mode === "mouse";
  const keysH = keyboard?.height ?? 0;
  /** The screen's area: the display, or the part above the docked keyboard. */
  const areaH = keyboard?.docked ? height - keysH : height;
  /** The part of the display not under the keyboard. */
  const visible = height - keysH;
  const panel = controlPanelBox(width, height, bottom);
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
        // Down to the keyboard, or to the home bar
        <div className="absolute left-0 top-0" style={{ width, height: keyboard ? visible : height - bottom }}>
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
      <div className="pointer-events-none absolute left-0 top-0" style={{ width, height: visible }}>
        <ScreenNotes {...notes} bottom={(keyboard ? 0 : bottom) + 12} />
      </div>

      {keyboard ? (
        <div
          className={`absolute bottom-0 left-0 ${keyboardPanel(keyboard.docked)}`}
          style={{ width, height: keysH }}
        >
          <KeyboardHandle height={keysH} onHeight={keyboard.onHeight} onDone={keyboard.onHeightDone} />
          <KeyboardView width={width} height={keysH - HANDLE_H} bottom={bottom} {...keyboard.keys} />
        </div>
      ) : null}

      {/* Under the control panel and the buttons while the control panel is open: the screen blurred and
          darkened, fading in and out with them as a dialog's backdrop does */}
      {controlPanelShown ? (
        <div
          aria-hidden
          className={`absolute inset-0 z-30 ${CONTROL_PANEL_BACKDROP} ${
            controlPanel.open ? MODAL_ANIMATION.backdrop.enter : MODAL_ANIMATION.backdrop.exit
          }`}
        />
      ) : null}

      {/* The control panel: mounted from the start, so its data is there when it opens */}
      <div
        ref={controlPanel.panelRef}
        className={`absolute z-40 transition-transform duration-300 ease-out ${controlPanelShown ? "" : "hidden"}`}
        style={{ ...panel, transform: `translateY(${-controlPanel.lift}px)` }}
      >
        {/* Narrow, on one glass panel (`./GLASS.md`); wide, each column is glass of its own, with
            nothing round them all. What is fixed in it (a sheet, a dialog) takes its space. */}
        <div
          className={`absolute inset-0 ${panel.width >= WIDE_MIN ? "" : `overflow-hidden ${CONTROL_PANEL_GLASS}`} ${
            // In and out as a dialog is: from a blur, and back into one
            controlPanel.open ? MODAL_ANIMATION.panel.enter : controlPanelShown ? MODAL_ANIMATION.panel.exit : ""
          }`}
        >
          {controlPanel.content}
        </div>
      </div>

      {side ? (
        // Out: past the window's edge by its gap and its shadow; and out of reach of Tab
        <div
          ref={side.ref}
          data-ng-side
          inert={!side.shown}
          className="pointer-events-none absolute z-40 transition-transform duration-300 ease-out"
          style={{
            top: panel.top,
            right: FLOAT_GAP,
            height: panel.height,
            transform: side.shown ? "none" : `translateX(calc(100% + ${FLOAT_GAP + SIDE_SHADOW}px))`,
          }}
        >
          {side.content}
        </div>
      ) : null}

      {buttons ? <FloatingButtons {...buttons} /> : null}
      {dialog}
      {identify !== null ? <MonitorNumber monitor={identify} /> : null}
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
}: NgScreenViewProps["notes"] & { bottom: number }) {
  const low = dictation === "listening" || heard ? heard || "Listening…" : null;
  return (
    <>
      {low ? (
        <p className="pointer-events-none absolute inset-x-0 flex justify-center px-6" style={{ bottom }}>
          <span className="ng-glass ng-glass-thick max-w-full truncate rounded-full px-4 py-2 text-sm">
            {low}
          </span>
        </p>
      ) : null}
      {note ? (
        <p className="pointer-events-none absolute inset-0 flex items-center justify-center p-6">
          <span className="ng-glass ng-glass-thick rounded-full px-4 py-2 text-center text-sm">
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
