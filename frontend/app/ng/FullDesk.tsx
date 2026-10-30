// The /ng page, on any screen and in any orientation: the clone's screen fills the display, and
// three floating buttons (`./FloatingButtons.tsx`) open the rest:
//
// - Pages: the tab pages (usage, tickets, clones, notes, settings) over the screen.
// - Keyboard: a tap shows or hides the keys, floating over the bottom of the screen. The screen
//   keeps its size; it moves up as far as it takes to keep the cursor above the keys
//   (`followAxis`). A long press docks the keyboard, or undocks it (`./keyboardDock.ts`). A long
//   press on the bar along the keyboard's top, then a drag, sets its height (`./KeyboardHandle.tsx`).
// - Fit: this phone's monitor (its layout: one monitor, `./phoneMonitor.ts`) takes the size of
//   the screen's area, at the phone's pixel ratio (`phoneMonitorSize`; 2 screen px per clone px,
//   or what a long press on Fit set, `./pixelRatio.ts`). The first time, it is that size from
//   the start. The clone's screen shows at that same ratio in the middle (smaller only where it
//   does not fit), not stretched.
//
// The phone's layout is in force while the phone is in use (`~/lib/clientLayout.ts`): it opens,
// comes to the front, or is touched.
//
// With the keyboard floating (or hidden), the whole display is a trackpad. Two fingers that pinch
// on it zoom the view (1× to MAX_ZOOM); the clone gets nothing. The view follows the cursor
// (`followAxis`): zoomed in, or with the keyboard up, it moves just enough to keep the cursor in
// sight.
//
// With the keyboard docked, it is always shown at the bottom, the clone's screen fits in the
// area above it, and that area is a touch screen: each finger goes to the clone as a touch.
//
// The connection and the input are `./remote.ts`'s.
import { type PointerEvent, useEffect, useRef, useState } from "react";

import { Cursor } from "./Cursor";
import { FLOAT_BUTTON, FLOAT_GAP, FloatingButtons } from "./FloatingButtons";
import { GAP, TOOL_H, followAxis, naturalKeysHeight, phoneMonitorSize, screenFit } from "./geometry";
import { HANDLE_H, KeyboardHandle } from "./KeyboardHandle";
import { KeyboardView, type TouchKind } from "./KeyboardView";
import { readKeyboardDocked, readKeyboardHeight, saveKeyboardDocked, saveKeyboardHeight } from "./keyboardDock";
import { NgPagesContainer } from "./pages/NgPagesContainer";
import { readPhoneMonitor, savePhoneMonitor } from "./phoneMonitor";
import { readPixelRatio, savePixelRatio } from "./pixelRatio";
import { clampTo } from "./pointer";
import { RatioDialog } from "./RatioDialog";
import { useRemote } from "./remote";
import { MONITOR } from "./session";
import { isTyping, useLift } from "./systemKeyboard";
import { type NgTab, ToolRow } from "./ToolRow";

/** The floating keyboard takes at most this part of the display's height. */
const KEYBOARD_PART = 0.45;
/** The cursor stays this far (CSS px) inside the view: off its edges, above the keyboard. */
const CURSOR_MARGIN = 36;
/** The most the view zooms in. */
const MAX_ZOOM = 4;
/** The keyboard's bar makes it no shorter than this (CSS px), and no taller than this part of
 *  the display. */
const MIN_KEYS_H = 160;
const MAX_KEYS_PART = 0.8;
/** Touch slots a touch screen offers, fingers at once. */
const TOUCH_SLOTS = 10;

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
  /** Screen px per clone px (this phone's setting), and its dialog. */
  const [ratio, setRatio] = useState(readPixelRatio);
  const [ratioOpen, setRatioOpen] = useState(false);
  /** The keyboard's height as a part of the display's (the bar on top of it sets it), and the
   *  height during a drag of that bar. */
  const [keysPart, setKeysPart] = useState(readKeyboardHeight);
  const [dragH, setDragH] = useState<number | null>(null);
  /** The floating keyboard is shown; the keyboard is docked (then always shown). */
  const [keyboard, setKeyboard] = useState(false);
  const [docked, setDocked] = useState(readKeyboardDocked);
  const keysShown = keyboard || docked;

  // The keyboard: the display's width; its keys square, no taller than KEYBOARD_PART of the
  // display, until its bar sets another height
  const keysH = Math.round(
    dragH ??
      (keysPart ? height * keysPart : Math.min(Math.ceil(naturalKeysHeight(width, bottom) + HANDLE_H), height * KEYBOARD_PART)),
  );
  /** The clone's screen's area: the display, or the part above the docked keyboard. */
  const areaH = docked ? height - keysH : height;
  const dpr = typeof window === "undefined" ? 1 : window.devicePixelRatio || 1;
  /** What Fit gives the clone's monitor: the screen's area at the pixel ratio. */
  const target = phoneMonitorSize(width, areaH, dpr, ratio);
  /** This phone's monitor size (its layout, `./phoneMonitor.ts`): what Fit set last, else the
   *  first target, kept from the start. */
  const [size, setSize] = useState(() => readPhoneMonitor() ?? target);
  useEffect(() => {
    if (!readPhoneMonitor()) savePhoneMonitor(size);
  }, [size]);
  const fits = size.width === target.width && size.height === target.height;

  const r = useRemote(box, () => size, onZoom);
  /** The pages over the screen, and the tab they show. */
  const [pages, setPages] = useState(false);
  const [tab, setTab] = useState<NgTab>("clones");

  const fit = screenFit(width, areaH, r.monitor.width, r.monitor.height, dpr, ratio);
  // The video's pixels stay those of the unzoomed view; a zoom scales the picture
  useEffect(() => {
    r.session.current?.resize(Math.round(r.monitor.width * fit.scale), Math.round(r.monitor.height * fit.scale));
  }, [fit.scale, r.monitor.width, r.monitor.height, r.session]);
  /** Device px per clone px as shown, and the screen's size on the display, CSS px. */
  const shown = fit.scale * zoom;
  r.scale.current = shown;
  const w = (r.monitor.width * shown) / dpr;
  const h = (r.monitor.height * shown) / dpr;

  /** The part of the display not under the keyboard. */
  const visible = keysShown ? height - keysH : height;

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
  // Docked, the screen stays in the middle of its area: there is no zoom, and no keyboard over it
  const x = docked ? fit.left : followAxis(prev.x, cx, w, width, width, CURSOR_MARGIN);
  const y = docked ? fit.top : followAxis(prev.y, cy, h, visible, height, CURSOR_MARGIN);
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

  // Docked, fingers on the screen's area go to the clone as a touch screen's touches, each in a
  // slot of its own from down to up; a finger beside the picture touches its nearest edge
  const slots = useRef(new Map<number, number>());
  const touchAt = (e: PointerEvent<HTMLDivElement>) => {
    const rect = box.current!.getBoundingClientRect();
    const m = r.monitor;
    return {
      monitor_id: MONITOR,
      x: clampTo(((e.clientX - rect.left) / rect.width) * m.width, m.width),
      y: clampTo(((e.clientY - rect.top) / rect.height) * m.height, m.height),
    };
  };
  const onScreenDown = (e: PointerEvent<HTMLDivElement>) => {
    const used = new Set(slots.current.values());
    const slot = [...Array(TOUCH_SLOTS).keys()].find((s) => !used.has(s));
    if (slot === undefined) return;
    if (e.pointerType === "mouse") e.currentTarget.setPointerCapture(e.pointerId);
    slots.current.set(e.pointerId, slot);
    r.session.current?.touch({ phase: "down", slot, ...touchAt(e) });
  };
  const onScreenMove = (e: PointerEvent<HTMLDivElement>) => {
    const slot = slots.current.get(e.pointerId);
    if (slot !== undefined) r.session.current?.touch({ phase: "motion", slot, ...touchAt(e) });
  };
  const onScreenUp = (e: PointerEvent<HTMLDivElement>) => {
    const slot = slots.current.get(e.pointerId);
    if (slot === undefined) return;
    slots.current.delete(e.pointerId);
    r.session.current?.touch({ phase: "up", slot });
  };

  const place = { left: 0, top: 0, width: w, height: h };
  /** Fit: the clone's monitor takes `next`, from now on. */
  const fitTo = (next: { width: number; height: number }) => {
    setZoom(1);
    setSize(next);
    r.claimMonitor(next);
  };
  const chooseRatio = (next: number) => {
    savePixelRatio(next);
    setRatio(next);
    fitTo(phoneMonitorSize(width, areaH, dpr, next));
  };
  const toggleDock = () => {
    const next = !docked;
    saveKeyboardDocked(next);
    setDocked(next);
    setKeyboard(false);
    setZoom(1);
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
      {docked ? (
        <div
          aria-label="Touch screen"
          className="absolute left-0 top-0"
          style={{ width, height: areaH }}
          onPointerDown={onScreenDown}
          onPointerMove={onScreenMove}
          onPointerUp={onScreenUp}
          onPointerCancel={onScreenUp}
        />
      ) : (
        <div
          aria-label="Trackpad"
          className="absolute inset-0"
          onPointerDown={pad("down")}
          onPointerMove={pad("move")}
          onPointerUp={pad("up")}
          onPointerCancel={pad("cancel")}
        />
      )}
      <div className="pointer-events-none absolute left-0 top-0" style={{ width, height: visible }}>
        <ScreenNotes dictation={r.dictation} heard={r.heard} note={r.note} bottom={(keysShown ? 0 : bottom) + 12} />
      </div>

      {keysShown ? (
        <div
          className={`absolute bottom-0 left-0 ${docked ? "bg-black" : "bg-black/75 backdrop-blur-xl"}`}
          style={{ width, height: keysH }}
        >
          <KeyboardHandle
            height={keysH}
            onHeight={(h) => setDragH(Math.max(MIN_KEYS_H, Math.min(height * MAX_KEYS_PART, h)))}
            onDone={() => {
              if (dragH === null) return;
              saveKeyboardHeight(dragH / height);
              setKeysPart(dragH / height);
              setDragH(null);
            }}
          />
          <KeyboardView
            width={width}
            height={keysH - HANDLE_H}
            bottom={bottom}
            fn={r.fn}
            mods={r.mods}
            caps={r.caps}
            onKeyTouch={r.onKeyTouch}
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
        keyboardOpen={keysShown}
        keyboardDocked={docked}
        fits={fits}
        badge={r.unread}
        onPages={() => setPages((p) => !p)}
        onKeyboard={() => !docked && setKeyboard((k) => !k)}
        onKeyboardDock={toggleDock}
        onFit={() => fitTo(target)}
        onFitSettings={() => setRatioOpen(true)}
      />
      {ratioOpen ? (
        <RatioDialog
          ratio={ratio}
          width={width}
          height={areaH}
          dpr={dpr}
          onChoose={chooseRatio}
          onClose={() => setRatioOpen(false)}
        />
      ) : null}
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
}: {
  dictation: string;
  heard: string;
  note: string | null;
  bottom: number;
}) {
  return (
    <>
      {dictation === "listening" || heard ? (
        <p className="pointer-events-none absolute inset-x-0 flex justify-center px-6" style={{ bottom }}>
          <span className="max-w-full truncate rounded-full bg-black/60 px-4 py-2 text-sm text-neutral-100 backdrop-blur-md">
            {heard || "Listening…"}
          </span>
        </p>
      ) : null}
      {note ? (
        <p className="pointer-events-none absolute inset-0 flex items-center justify-center p-6">
          <span className="rounded-full bg-black/50 px-4 py-2 text-center text-sm text-neutral-100 backdrop-blur-md">
            {note}
          </span>
        </p>
      ) : null}
    </>
  );
}
