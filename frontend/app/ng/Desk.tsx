// The /ng page on the iPhone Duo, unfolded and upright: the selected clone's screen on top, the
// keyboard below, and an open tab's page in the keyboard's place. The connection and the input
// are `./remote.ts`'s, which the other orientations (`./FullDesk.tsx`) share; this lays them out.
import { type PointerEvent, useCallback, useEffect, useRef, useState } from "react";

import { Cursor } from "./Cursor";
import { KeyboardView } from "./KeyboardView";
import { keysLayout, screenFit } from "./geometry";
import { clampTo } from "./pointer";
import { NgPagesContainer } from "./pages/NgPagesContainer";
import { DUO_MONITOR, useRemote } from "./remote";
import { MONITOR } from "./session";
import { isTyping, useLift } from "./systemKeyboard";
import type { NgTab } from "./ToolRow";

/** The keyboard area at the bottom of the screen, CSS px; the clone's screen has the rest
 *  (539 px on the Duo, the layout's monitor at 2×). */
const KEYS_HEIGHT = 412;
/** Touch slots a touch screen offers, fingers at once. */
const TOUCH_SLOTS = 10;

type Props = {
  /** The whole screen, CSS px, and the home bar's safe-area inset. */
  width: number;
  height: number;
  bottom: number;
};

/** Upright the clone has the iPhone Duo layout's monitor, whatever another orientation gave it. */
const duoSize = () => DUO_MONITOR;

export function Desk({ width, height, bottom }: Props) {
  const box = useRef<HTMLDivElement>(null);
  const r = useRemote(box, duoSize);
  /** The open tool-row tab; null shows the keyboard. */
  const [tab, setTab] = useState<NgTab | null>(null);

  const dpr = typeof window === "undefined" ? 1 : window.devicePixelRatio || 1;
  const fit = screenFit(width, height - KEYS_HEIGHT, r.monitor.width, r.monitor.height, dpr);
  r.scale.current = fit.scale;

  // The video's box in device pixels
  useEffect(() => {
    r.session.current?.resize(Math.round(r.monitor.width * fit.scale), Math.round(r.monitor.height * fit.scale));
  }, [fit.scale, r.monitor.width, r.monitor.height, r.session]);

  // A tab opens its page in the keyboard's place; tapped again, the keyboard comes back
  const onTab = useCallback((t: NgTab) => setTab((open) => (open === t ? null : t)), []);
  /** Where an open tab's page goes: the keys' and the mouse buttons' place. */
  const pageBox = keysLayout(width, KEYS_HEIGHT, bottom).page;

  // While the phone's keyboard is up for a text box on a tab page, the whole screen moves up
  // so that page is all above the keyboard (its bottom edge is the home bar's top)
  const lift = useLift(height - bottom);
  // A touch outside the page puts the phone's keyboard away
  const onDownCapture = (e: PointerEvent<HTMLDivElement>) => {
    r.claim();
    r.sounds.unlock();
    if (isTyping() && !(e.target as Element).closest("[role=tabpanel]")) {
      (document.activeElement as HTMLElement).blur();
    }
  };

  const place = { left: fit.left, top: fit.top, width: fit.width, height: fit.height };

  // Fingers on the screen go to the clone as a touch screen's touches, each in a slot of its
  // own from down to up
  const slots = useRef(new Map<number, number>());
  const touchAt = (e: PointerEvent<HTMLDivElement>) => {
    const rect = e.currentTarget.getBoundingClientRect();
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

  return (
    <div
      className="absolute inset-0 bg-black transition-transform duration-300 ease-out"
      style={{ transform: `translateY(${-lift}px)` }}
      onPointerDownCapture={onDownCapture}
    >
      <div className="relative overflow-hidden" style={{ height: height - KEYS_HEIGHT }}>
        {/* The session puts the video in here, and touches on it go to the clone; the cursor
            and the note come after it, on top, and let touches through */}
        <div
          ref={box}
          className="absolute"
          style={place}
          onPointerDown={onScreenDown}
          onPointerMove={onScreenMove}
          onPointerUp={onScreenUp}
          onPointerCancel={onScreenUp}
        />
        <div className="pointer-events-none absolute" style={place}>
          {r.shape && !r.hidden && !r.terminal && r.connected ? (
            <Cursor shape={r.shape} x={r.pos.x} y={r.pos.y} scale={fit.scale} />
          ) : null}
        </div>
        <ScreenNotes dictation={r.dictation} heard={r.heard} note={r.note} />
      </div>
      <KeyboardView
        width={width}
        height={KEYS_HEIGHT}
        bottom={bottom}
        fn={r.fn}
        mods={r.mods}
        caps={r.caps}
        onKeyTouch={r.onKeyTouch}
        onButton={r.onButton}
        usage={r.usage}
        tab={tab}
        onTab={onTab}
        badges={r.unread ? { clones: r.unread } : undefined}
        pad={r.padMode}
        dictation={r.dictation}
      />
      {/* Mounted from the start, so the pages' data is there when one opens */}
      <div
        className={`absolute ${tab ? "" : "hidden"}`}
        style={{ ...pageBox, top: height - KEYS_HEIGHT + pageBox.top }}
      >
        <NgPagesContainer
          tab={tab}
          onTab={setTab}
          state={r.live.state}
          stats={r.live.stats}
          sound={r.soundReport}
        />
      </div>
    </div>
  );
}

/** What is said over the clone's screen: the words dictation hears (at the bottom, `bottom` px
 *  up), and a note when there is no screen to use (in the middle). Touches go through both. */
export function ScreenNotes({
  dictation,
  heard,
  note,
  bottom = 12,
}: {
  dictation: string;
  heard: string;
  note: string | null;
  bottom?: number;
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
