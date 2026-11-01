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
// - The pages (`pages.content`: the tool row and the open page, or the dashboard's layout), on a
//   glass panel at the right under the buttons.
// - The floating buttons, a dialog, and the monitor's number while the Edit layout dialog is
//   open in another window.
import type { PointerEvent, ReactNode, Ref } from "react";

import { MonitorNumber } from "~/components/MonitorNumber";
import type { CursorShape } from "~/lib/wire/CursorShape";

import { Cursor } from "./Cursor";
import type { DictationState } from "./dictation";
import { FLOAT_BUTTON, FLOAT_GAP, FloatingButtons } from "./FloatingButtons";
import { GAP } from "./geometry";
import type { InputMode } from "./inputMode";
import { HANDLE_H, KeyboardHandle } from "./KeyboardHandle";
import { PAGES_GLASS } from "./pages/ui";
import { KeyboardView, type KeyboardViewProps, type TouchKind, keyboardPanel } from "./KeyboardView";

/** The pages are at most this wide (CSS px), at the right, under the buttons. A large screen
 *  gives them the dashboard's layout (`./pages/WideLayout.tsx`). */
const PANEL_MAX_W = 1600;

type Box = { left: number; top: number; width: number; height: number };

/** Where the pages go on a display `width`×`height` with `bottom` kept for the home bar. */
export function pagesBox(width: number, height: number, bottom: number): Box {
  const w = Math.min(width - 2 * FLOAT_GAP, PANEL_MAX_W);
  return {
    left: width - FLOAT_GAP - w,
    top: FLOAT_GAP * 2 + FLOAT_BUTTON,
    width: w,
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
  /** The pages: shown or not, what they show (`./pages/NgPagesContainer.tsx`, for the panel's
   *  width), and how far the phone's keyboard lifts them. */
  pages: {
    open: boolean;
    lift: number;
    content: ReactNode;
    panelRef?: Ref<HTMLDivElement>;
  };
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
  pages,
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
  const panel = pagesBox(width, height, bottom);
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

      {/* The pages: mounted from the start, so their data is there when they open */}
      <div
        ref={pages.panelRef}
        className={`absolute z-40 transition-transform duration-300 ease-out ${pages.open ? "" : "hidden"}`}
        style={{ ...panel, transform: `translateY(${-pages.lift}px)` }}
      >
        {/* Glass (`./GLASS.md`); what is fixed in it (a sheet, a dialog) takes its space */}
        <div className={`absolute inset-0 overflow-hidden ${PAGES_GLASS}`}>{pages.content}</div>
      </div>

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
