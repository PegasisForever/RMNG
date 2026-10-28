// The /ng page in every other orientation (the phone folded, or turned sideways, or any other
// screen): the clone's screen fills the display, the whole display is a trackpad, and three
// floating buttons (`./FloatingButtons.tsx`) open the rest:
//
// - Pages: the tab pages (usage, tickets, clones, notes, settings) over the screen.
// - Keyboard: the keys, floating over the bottom of the screen. The screen keeps its size; it
//   moves up as far as it takes to keep the cursor above the keys (`cursorShift`).
// - Fit: the clone's monitor takes this display's size, at the scale the phone has upright
//   (`phoneMonitorSize`). Until then the clone keeps the size it had, shown at that same scale
//   in the middle (smaller only where it does not fit), not stretched.
//
// The connection and the input are `./remote.ts`'s, shared with the upright screen.
import { type PointerEvent, useEffect, useRef, useState } from "react";

import { Cursor } from "./Cursor";
import { ScreenNotes } from "./Desk";
import { FLOAT_BUTTON, FLOAT_GAP, FloatingButtons } from "./FloatingButtons";
import { GAP, PHONE_SCALE, TOOL_H, cursorShift, keysLayout, phoneMonitorSize, screenFit } from "./geometry";
import { KeyboardView, type TouchKind } from "./KeyboardView";
import { NgPagesContainer } from "./pages/NgPagesContainer";
import { useRemote } from "./remote";
import { isTyping, useLift } from "./systemKeyboard";
import { type NgTab, ToolRow } from "./ToolRow";

/** The floating keyboard takes at most this part of the display's height. */
const KEYBOARD_PART = 0.45;
/** The cursor stays this far (CSS px) above the floating keyboard. */
const CURSOR_MARGIN = 36;

type Props = {
  /** The display, CSS px, and the home bar's safe-area inset. */
  width: number;
  height: number;
  bottom: number;
};

export function FullDesk({ width, height, bottom }: Props) {
  const box = useRef<HTMLDivElement>(null);
  // No size of its own in the claim: the clone keeps the monitor it has until Fit
  const r = useRemote(box);
  const [keyboard, setKeyboard] = useState(false);
  /** The pages over the screen, and the tab they show. */
  const [pages, setPages] = useState(false);
  const [tab, setTab] = useState<NgTab>("clones");

  const dpr = typeof window === "undefined" ? 1 : window.devicePixelRatio || 1;
  const fit = screenFit(width, height, r.monitor.width, r.monitor.height, dpr, PHONE_SCALE);
  r.scale.current = fit.scale;
  useEffect(() => {
    r.session.current?.resize(Math.round(r.monitor.width * fit.scale), Math.round(r.monitor.height * fit.scale));
  }, [fit.scale, r.monitor.width, r.monitor.height, r.session]);

  const target = phoneMonitorSize(width, height, dpr);
  const fits = r.monitor.width === target.width && r.monitor.height === target.height;

  // The floating keyboard: the display's width, its keys square where there is room, no taller
  // than KEYBOARD_PART of the display
  const natural = keysLayout(width, 10_000, bottom, true);
  const keysH = Math.min(
    Math.ceil(natural.keys.top + natural.keys.height + 4 + bottom),
    Math.round(height * KEYBOARD_PART),
  );
  const visible = keyboard ? height - keysH : height;
  const cursorY = fit.top + (r.pos.y / r.monitor.height) * fit.height;
  const shift = keyboard ? cursorShift(cursorY, visible, fit.top + fit.height, CURSOR_MARGIN) : 0;

  // The pages, under the buttons' row
  const panel = {
    left: FLOAT_GAP,
    top: FLOAT_GAP * 2 + FLOAT_BUTTON,
    width: width - 2 * FLOAT_GAP,
    height: height - FLOAT_GAP * 3 - FLOAT_BUTTON - bottom,
  };
  const lift = useLift(panel.top + panel.height);

  const onDownCapture = (e: PointerEvent<HTMLDivElement>) => {
    r.claim();
    r.sounds.unlock();
    // A touch outside the pages puts the phone's keyboard away
    if (isTyping() && !(e.target as Element).closest("[role=tabpanel]")) {
      (document.activeElement as HTMLElement).blur();
    }
  };

  // The whole display is the trackpad (under the keyboard, the pages and the buttons)
  const pad = (kind: TouchKind) => (e: PointerEvent<HTMLDivElement>) => {
    if (kind === "down" && e.pointerType === "mouse") e.currentTarget.setPointerCapture(e.pointerId);
    r.onPadTouch(kind, e.pointerId, e.clientX, e.clientY);
  };

  const place = { left: fit.left, top: fit.top, width: fit.width, height: fit.height };
  return (
    <div className="absolute inset-0 touch-none select-none overflow-hidden bg-black" onPointerDownCapture={onDownCapture}>
      {/* The clone's screen, moved up while the keyboard would cover the cursor */}
      <div
        className="absolute inset-0 transition-transform duration-150 ease-out"
        style={{ transform: `translateY(${-shift}px)` }}
      >
        <div ref={box} className="absolute" style={place} />
        <div className="pointer-events-none absolute" style={place}>
          {r.shape && !r.hidden && !r.terminal && r.connected ? (
            <Cursor shape={r.shape} x={r.pos.x} y={r.pos.y} scale={fit.scale} />
          ) : null}
        </div>
      </div>
      <div
        aria-label="Trackpad"
        className="absolute inset-0"
        onPointerDown={pad("down")}
        onPointerMove={pad("move")}
        onPointerUp={pad("up")}
        onPointerCancel={pad("cancel")}
      />
      <ScreenNotes dictation={r.dictation} heard={r.heard} note={r.note} bottom={(keyboard ? keysH : bottom) + 12} />

      {keyboard ? (
        <div className="absolute bottom-0 left-0 bg-black/75 backdrop-blur-xl" style={{ width, height: keysH }}>
          <KeyboardView
            width={width}
            height={keysH}
            bottom={bottom}
            bare
            fn={r.fn}
            mods={r.mods}
            caps={r.caps}
            onKeyTouch={r.onKeyTouch}
            onButton={r.onButton}
            usage={r.usage}
            tab={null}
            onTab={() => {}}
            pad={r.padMode}
            dictation={r.dictation}
          />
        </div>
      ) : null}

      {/* The pages: mounted from the start, so their data is there when they open */}
      <div
        className={`absolute z-40 transition-transform duration-300 ease-out ${pages ? "" : "hidden"}`}
        style={{ ...panel, transform: `translateY(${-lift}px)` }}
      >
        <div className="absolute inset-0 overflow-hidden rounded-xl border border-neutral-800 bg-black/95 backdrop-blur-xl">
          <ToolRow
            box={{ left: 4, top: 4, width: panel.width - 8, height: TOOL_H }}
            usage={r.usage}
            tab={pages ? tab : null}
            onTab={setTab}
            badges={r.unread ? { clones: r.unread } : undefined}
          />
          <div
            className="absolute"
            style={{ left: 4, top: 4 + TOOL_H + GAP, width: panel.width - 8, height: panel.height - 8 - TOOL_H - GAP }}
          >
            <NgPagesContainer
              tab={pages ? tab : null}
              onTab={(t) => (t ? setTab(t) : setPages(false))}
              state={r.live.state}
              stats={r.live.stats}
              sound={r.soundReport}
            />
          </div>
        </div>
      </div>

      <FloatingButtons
        pagesOpen={pages}
        keyboardOpen={keyboard}
        fits={fits}
        badge={r.unread}
        onPages={() => setPages((p) => !p)}
        onKeyboard={() => setKeyboard((k) => !k)}
        onFit={() => r.claimMonitor(target)}
      />
    </div>
  );
}
