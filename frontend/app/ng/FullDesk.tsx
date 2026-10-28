// The /ng page in every other orientation (the phone folded, or turned sideways, or any other
// screen): the clone's screen fills the display, the whole display is a trackpad, and three
// floating buttons (`./FloatingButtons.tsx`) open the rest:
//
// - Pages: the tab pages (usage, tickets, clones, notes, settings) over the screen.
// - Keyboard: the keys, floating over the bottom of the screen. The screen keeps its size; it
//   moves up as far as it takes to keep the cursor above the keys (`followAxis`).
// - Fit: the clone's monitor takes this display's size, at the phone's pixel ratio
//   (`phoneMonitorSize`; 2 screen px per clone px as upright, or what a long press on Fit set,
//   `./pixelRatio.ts`). Until then the clone keeps the size it had, shown at that same ratio in
//   the middle (smaller only where it does not fit), not stretched.
//
// Two fingers that pinch on the trackpad zoom the view (1× to MAX_ZOOM); the clone gets nothing.
// The view follows the cursor (`followAxis`): zoomed in, or with the keyboard up, it moves just
// enough to keep the cursor in sight.
//
// The connection and the input are `./remote.ts`'s, shared with the upright screen.
import { type PointerEvent, useEffect, useRef, useState } from "react";

import { Cursor } from "./Cursor";
import { ScreenNotes } from "./Desk";
import { FLOAT_BUTTON, FLOAT_GAP, FloatingButtons } from "./FloatingButtons";
import { GAP, TOOL_H, followAxis, keysLayout, phoneMonitorSize, screenFit } from "./geometry";
import { KeyboardView, type TouchKind } from "./KeyboardView";
import { NgPagesContainer } from "./pages/NgPagesContainer";
import { readPixelRatio, savePixelRatio } from "./pixelRatio";
import { RatioDialog } from "./RatioDialog";
import { useRemote } from "./remote";
import { isTyping, useLift } from "./systemKeyboard";
import { type NgTab, ToolRow } from "./ToolRow";

/** The floating keyboard takes at most this part of the display's height. */
const KEYBOARD_PART = 0.45;
/** The cursor stays this far (CSS px) inside the view: off its edges, above the keyboard. */
const CURSOR_MARGIN = 36;
/** The most the view zooms in. */
const MAX_ZOOM = 4;

type Props = {
  /** The display, CSS px, and the home bar's safe-area inset. */
  width: number;
  height: number;
  bottom: number;
};

export function FullDesk({ width, height, bottom }: Props) {
  const box = useRef<HTMLDivElement>(null);
  /** The view's zoom (1: the clone as large as the pixel ratio allows). */
  const [zoom, setZoom] = useState(1);
  const onZoom = (factor: number) => setZoom((z) => Math.min(MAX_ZOOM, Math.max(1, z * factor)));
  // No size of its own in the claim: the clone keeps the monitor it has until Fit
  const r = useRemote(box, undefined, onZoom);
  const [keyboard, setKeyboard] = useState(false);
  /** The pages over the screen, and the tab they show. */
  const [pages, setPages] = useState(false);
  const [tab, setTab] = useState<NgTab>("clones");
  /** Screen px per clone px (this phone's setting), and its dialog. */
  const [ratio, setRatio] = useState(readPixelRatio);
  const [ratioOpen, setRatioOpen] = useState(false);

  const dpr = typeof window === "undefined" ? 1 : window.devicePixelRatio || 1;
  const fit = screenFit(width, height, r.monitor.width, r.monitor.height, dpr, ratio);
  // The video's pixels stay those of the unzoomed view; a zoom scales the picture
  useEffect(() => {
    r.session.current?.resize(Math.round(r.monitor.width * fit.scale), Math.round(r.monitor.height * fit.scale));
  }, [fit.scale, r.monitor.width, r.monitor.height, r.session]);
  /** Device px per clone px as shown, and the screen's size on the display, CSS px. */
  const shown = fit.scale * zoom;
  r.scale.current = shown;
  const w = (r.monitor.width * shown) / dpr;
  const h = (r.monitor.height * shown) / dpr;

  const target = phoneMonitorSize(width, height, dpr, ratio);
  const fits = r.monitor.width === target.width && r.monitor.height === target.height;

  // The floating keyboard: the display's width, its keys square where there is room, no taller
  // than KEYBOARD_PART of the display
  const natural = keysLayout(width, 10_000, bottom, true);
  const keysH = Math.min(
    Math.ceil(natural.keys.top + natural.keys.height + 4 + bottom),
    Math.round(height * KEYBOARD_PART),
  );
  const visible = keyboard ? height - keysH : height;

  // Where the screen goes: as before, but moved as little as keeps the cursor in sight. A zoom
  // keeps the cursor where it was on the display.
  const at = useRef<{ x: number; y: number; shown: number } | null>(null);
  const cx = (r.pos.x * shown) / dpr;
  const cy = (r.pos.y * shown) / dpr;
  let prev = at.current ?? { x: (width - w) / 2, y: (height - h) / 2, shown };
  if (prev.shown !== shown) {
    const k = shown / prev.shown;
    prev = { x: prev.x + (r.pos.x * prev.shown) / dpr * (1 - k), y: prev.y + (r.pos.y * prev.shown) / dpr * (1 - k), shown };
  }
  const x = followAxis(prev.x, cx, w, width, width, CURSOR_MARGIN);
  const y = followAxis(prev.y, cy, h, visible, height, CURSOR_MARGIN);
  at.current = { x, y, shown };

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

  const place = { left: 0, top: 0, width: w, height: h };
  const chooseRatio = (next: number) => {
    savePixelRatio(next);
    setRatio(next);
    setZoom(1);
    r.claimMonitor(phoneMonitorSize(width, height, dpr, next));
  };
  return (
    <div className="absolute inset-0 touch-none select-none overflow-hidden bg-black" onPointerDownCapture={onDownCapture}>
      {/* The clone's screen, where the cursor stays in sight */}
      <div
        className="absolute left-0 top-0 transition-transform duration-100 ease-out"
        style={{ transform: `translate(${x}px, ${y}px)` }}
      >
        <div ref={box} className="absolute" style={place} />
        <div className="pointer-events-none absolute" style={place}>
          {r.shape && !r.hidden && !r.terminal && r.connected ? (
            <Cursor shape={r.shape} x={r.pos.x} y={r.pos.y} scale={shown} />
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
        onFit={() => {
          setZoom(1);
          r.claimMonitor(target);
        }}
        onFitSettings={() => setRatioOpen(true)}
      />
      {ratioOpen ? (
        <RatioDialog
          ratio={ratio}
          width={width}
          height={height}
          dpr={dpr}
          onChoose={chooseRatio}
          onClose={() => setRatioOpen(false)}
        />
      ) : null}
    </div>
  );
}
