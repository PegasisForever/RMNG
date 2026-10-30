// The /ng page, on any screen and in any orientation: one monitor of this browser's layout
// (`~/lib/clientLayout.ts`; `?monitor=`, 0 when absent) fills the window. On a desktop each
// monitor has an /ng window of its own; a phone has one monitor, this page.
//
// It has two modes, chosen by the input used last (`./inputMode.ts`):
//
// - Touch: three floating buttons (`./FloatingButtons.tsx`) open the rest:
//   - Pages: the tab pages (usage, tickets, clones, notes, settings) over the screen.
//   - Keyboard: a tap shows or hides the keys, floating over the bottom of the screen. The screen
//     keeps its size; it moves up as far as it takes to keep the cursor above the keys
//     (`followAxis`). A long press docks the keyboard, or undocks it (`./keyboardDock.ts`). A
//     long press on the bar along the keyboard's top, then a drag, sets its height
//     (`./KeyboardHandle.tsx`).
//   - Fit: this window's monitor takes the size of the screen's area, at its pixel scale
//     (`phoneMonitorSize`; the screen pixels one clone pixel takes). A long press opens the Edit
//     layout dialog: the pixel scales, and on a desktop the other monitors and their windows.
//   With the keyboard floating (or hidden), the whole display is a trackpad. Two fingers that
//   pinch on it zoom the view (1× to MAX_ZOOM); the clone gets nothing. The view follows the
//   cursor (`followAxis`): zoomed in, or with the keyboard up, it moves just enough to keep the
//   cursor in sight. With the keyboard docked, it is always shown at the bottom, the clone's
//   screen fits in the area above it, and that area is a touch screen: each finger goes to the
//   clone as a touch.
// - Mouse and keyboard: the mouse, the keys and the clipboard go to the clone as in the viewer
//   (`~/viewer/input.ts`). No buttons float over the screen: a chord (`./shortcut.ts`) shows the
//   pages, with the buttons above them, and hides them again; so does a click beside them.
//
// Where the browser can open windows, the pages can move to a window of their own (`?pages`).
//
// The clone's screen shows at the pixel scale in the middle (smaller only where it does not
// fit), not stretched. This browser's layout is in force while it is in use: a window opens,
// comes to the front, or is touched or clicked.
//
// The connection and the input are `./remote.ts`'s; the drawing is `./NgScreenView.tsx`'s.
import { type PointerEvent, useEffect, useRef, useState } from "react";

import { LayoutEditorContainer } from "~/components/LayoutEditorContainer";
import { type BrowserLayout, clientKind, ensureLayout, readLayout, saveLayout } from "~/lib/clientLayout";
import { resize } from "~/lib/monitorLayout";
import { useViewerChannel } from "~/lib/useViewerChannel";
import { toggleFullscreen } from "~/viewer/input";

import { firstLayout, startScale } from "./firstLayout";
import { followAxis, naturalKeysHeight, phoneMonitorSize, screenFit } from "./geometry";
import { useInputMode } from "./inputMode";
import { HANDLE_H } from "./KeyboardHandle";
import { type TouchKind } from "./KeyboardView";
import { readKeyboardDocked, readKeyboardHeight, saveKeyboardDocked, saveKeyboardHeight } from "./keyboardDock";
import { NgScreenView, pagesBox } from "./NgScreenView";
import { NgPagesContainer } from "./pages/NgPagesContainer";
import { clampTo } from "./pointer";
import { useRemote } from "./remote";
import { isPagesChord, pagesChordLabel } from "./shortcut";
import { isTyping, useLift } from "./systemKeyboard";
import type { NgTab } from "./ToolRow";

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
/** How long the chord's hint shows when mouse and keyboard mode starts, ms. */
const HINT_MS = 4000;
/** The pages' own window: its name, and its size when it opens. */
const PAGES_WINDOW = "rmng-pages";
const PAGES_WINDOW_FEATURES = "popup,width=560,height=900";

type Props = {
  /** The display, CSS px, and the home bar's safe-area inset. */
  width: number;
  height: number;
  bottom: number;
  /** The monitor this window shows. */
  monitor: number;
  /** The monitor has another number now (the Edit layout dialog renumbered them). */
  onMonitor: (monitor: number) => void;
};

type Size = { width: number; height: number };

export function FullDesk({ width, height, bottom, monitor, onMonitor }: Props) {
  const box = useRef<HTMLDivElement>(null);
  const overlay = useRef<HTMLImageElement>(null);
  const panelRef = useRef<HTMLDivElement>(null);
  /** A desktop (it opens windows: one per monitor, and the pages') or a phone. */
  const [kind] = useState(clientKind);
  const windows = kind === "desktop";
  const mode = useInputMode();
  const mouse = mode === "mouse";
  /** The view's zoom (1: the clone as large as the pixel scale allows). */
  const [zoom, setZoom] = useState(1);
  const onZoom = (factor: number) => setZoom((z) => Math.min(MAX_ZOOM, Math.max(1, z * factor)));
  /** The keyboard's height as a part of the display's (the bar on top of it sets it), and the
   *  height during a drag of that bar. */
  const [keysPart, setKeysPart] = useState(readKeyboardHeight);
  const [dragH, setDragH] = useState<number | null>(null);
  /** The floating keyboard is shown; the keyboard is docked (then always shown, in touch mode). */
  const [keyboard, setKeyboard] = useState(false);
  const [docked, setDocked] = useState(readKeyboardDocked);
  const dockedNow = docked && !mouse;
  const keysShown = !mouse && (keyboard || docked);
  /** The pages over the screen, and the tab they show; the Edit layout dialog. */
  const [pages, setPages] = useState(false);
  const [tab, setTab] = useState<NgTab>("clones");
  const [layoutOpen, setLayoutOpen] = useState(false);
  /** The chord's hint, while mouse and keyboard mode is new. */
  const [hint, setHint] = useState(false);
  /** The page is fullscreen (its own fullscreen); null where the browser has none. */
  const [fullscreen, setFullscreen] = useState<boolean | null>(null);
  useEffect(() => {
    if (!document.fullscreenEnabled) return;
    const on = () => setFullscreen(document.fullscreenElement !== null);
    on();
    document.addEventListener("fullscreenchange", on);
    return () => document.removeEventListener("fullscreenchange", on);
  }, []);

  // The keyboard: the display's width; its keys square, no taller than KEYBOARD_PART of the
  // display, until its bar sets another height
  const keysH = Math.round(
    dragH ??
      (keysPart ? height * keysPart : Math.min(Math.ceil(naturalKeysHeight(width, bottom) + HANDLE_H), height * KEYBOARD_PART)),
  );
  /** The clone's screen's area: the display, or the part above the docked keyboard. */
  const areaH = dockedNow ? height - keysH : height;
  const dpr = typeof window === "undefined" ? 1 : window.devicePixelRatio || 1;

  /** This browser's layout, shared by its windows; null until it has one. */
  const [layout, setLayout] = useState<BrowserLayout | null>(readLayout);
  const fitAt = (scale: number) => phoneMonitorSize(width, areaH, dpr, scale);
  const fitAtRef = useRef(fitAt);
  fitAtRef.current = fitAt;
  useEffect(() => {
    // The first time: one monitor, this window's size (`./firstLayout.ts`); a phone keeps one
    void ensureLayout(kind, () => firstLayout(kind, (s) => fitAtRef.current(s))).then(setLayout);
    // Another window of this browser changed it
    const onStorage = () => setLayout(readLayout());
    window.addEventListener("storage", onStorage);
    return () => window.removeEventListener("storage", onStorage);
  }, [kind]);
  /** Screen px per clone px in this window. */
  const scale = layout?.scales[monitor] ?? startScale(kind);
  /** What Fit gives this window's monitor: the screen's area at the pixel scale. */
  const target = fitAt(scale);
  const mine = layout?.monitors[monitor];
  const fits = !mine || (mine.width === target.width && mine.height === target.height);

  // A popped-out pages window, while it is open
  const popout = useRef<Window | null>(null);
  /** The chord: the pages, or their own window when it is open. */
  const togglePages = () => {
    const w = popout.current;
    if (w && !w.closed) w.focus();
    else setPages((p) => !p);
  };
  /** The mouse and the keys go to the clone: mouse mode, nothing over the screen. */
  const mouseInput = mouse && !pages && !layoutOpen;
  const r = useRemote({ box, overlay, monitor, kind, mouse: mouseInput, onZoom, onChord: togglePages });

  // The chord while the clone does not get the keys (the viewer's input takes it otherwise)
  const chordRef = useRef(togglePages);
  chordRef.current = togglePages;
  const mouseInputRef = useRef(mouseInput);
  mouseInputRef.current = mouseInput;
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (mouseInputRef.current || !isPagesChord(e)) return;
      e.preventDefault();
      if (!e.repeat) chordRef.current();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, []);

  // Mouse and keyboard mode starts: say how to reach the pages, for a while
  useEffect(() => {
    if (!mouse) return;
    setZoom(1);
    setHint(true);
    const t = setTimeout(() => setHint(false), HINT_MS);
    return () => clearTimeout(t);
  }, [mouse]);

  // The Edit layout dialog finds this window by its name and its number
  useEffect(() => {
    if (windows) window.name = `rmng-viewer-${monitor}`;
  }, [windows, monitor]);
  const identify = useViewerChannel(
    monitor,
    // The size Fit would give, at the pixel scale, so a match gives just that
    () => ({ width: target.width * scale, height: target.height * scale }),
    (next) => {
      if (next !== null) return onMonitor(next);
      // Its monitor was removed: the window closes, where the page opened it
      window.close();
      setTimeout(() => onMonitor(0), 300);
    },
  );

  const fit = screenFit(width, areaH, r.monitor.width, r.monitor.height, dpr, scale);
  // The video's pixels stay those of the unzoomed view; a zoom scales the picture
  useEffect(() => {
    r.session.current?.resize(Math.round(r.monitor.width * fit.scale), Math.round(r.monitor.height * fit.scale));
  }, [fit.scale, r.monitor.width, r.monitor.height, r.session]);
  /** Device px per clone px as shown, and the screen's size on the display, CSS px. */
  const shown = fit.scale * (mouse ? 1 : zoom);
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
  // Docked, or with a mouse, the screen stays in the middle of its area: there is no zoom, and
  // no keyboard over it
  const still = dockedNow || mouse;
  const x = still ? fit.left : followAxis(prev.x, cx, w, width, width, CURSOR_MARGIN);
  const y = still ? fit.top : followAxis(prev.y, cy, h, visible, height, CURSOR_MARGIN);
  at.current = { x, y, shown };

  // The phone's keyboard lifts the pages so all of the open one is above it
  const panel = pagesBox(width, height, bottom);
  const lift = useLift(panel.top + panel.height);

  const onDownCapture = (e: PointerEvent<HTMLDivElement>) => {
    r.sounds.unlock();
    const target = e.target as Element;
    // A touch outside the pages puts the phone's keyboard away
    if (isTyping() && !target.closest("[role=tabpanel]")) {
      (document.activeElement as HTMLElement).blur();
    }
    // With a mouse, a click beside the pages and the buttons puts the pages away
    if (mouse && pages && !layoutOpen && !panelRef.current?.contains(target) && !target.closest("[data-ng-buttons]")) {
      setPages(false);
    }
  };

  // The whole display is the trackpad (under the keyboard, the pages and the buttons)
  const pad = (kind: TouchKind, e: PointerEvent<HTMLDivElement>) => {
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
      monitor_id: monitor,
      x: clampTo(((e.clientX - rect.left) / rect.width) * m.width, m.width),
      y: clampTo(((e.clientY - rect.top) / rect.height) * m.height, m.height),
    };
  };
  const onTouchScreen = (kind: TouchKind, e: PointerEvent<HTMLDivElement>) =>
    kind === "down" ? onScreenDown(e) : kind === "move" ? onScreenMove(e) : onScreenUp(e);
  function onScreenDown(e: PointerEvent<HTMLDivElement>) {
    const used = new Set(slots.current.values());
    const slot = [...Array(TOUCH_SLOTS).keys()].find((s) => !used.has(s));
    if (slot === undefined) return;
    if (e.pointerType === "mouse") e.currentTarget.setPointerCapture(e.pointerId);
    slots.current.set(e.pointerId, slot);
    r.session.current?.touch({ phase: "down", slot, ...touchAt(e) });
  }
  function onScreenMove(e: PointerEvent<HTMLDivElement>) {
    const slot = slots.current.get(e.pointerId);
    if (slot !== undefined) r.session.current?.touch({ phase: "motion", slot, ...touchAt(e) });
  }
  function onScreenUp(e: PointerEvent<HTMLDivElement>) {
    const slot = slots.current.get(e.pointerId);
    if (slot === undefined) return;
    slots.current.delete(e.pointerId);
    r.session.current?.touch({ phase: "up", slot });
  }

  /** A new layout: kept, shown, and in force at once. */
  const apply = (next: BrowserLayout) => {
    saveLayout(next);
    setLayout(next);
    r.claim();
  };
  /** Fit: this window's monitor takes `next`, from now on; the others keep their sides. */
  const fitTo = (next: Size) => {
    setZoom(1);
    const l = readLayout() ?? layout;
    if (!l?.monitors[monitor]) return;
    apply({ ...l, monitors: resize(l.monitors, l.monitors.map((m, i) => (i === monitor ? next : m))) });
  };
  const toggleDock = () => {
    const next = !docked;
    saveKeyboardDocked(next);
    setDocked(next);
    setKeyboard(false);
    setZoom(1);
  };
  /** Move the pages to a window of their own (an open one is only brought to the front). */
  const popOut = () => {
    const win = window.open("", PAGES_WINDOW, PAGES_WINDOW_FEATURES);
    if (!win) return;
    if (win.location.href === "about:blank") win.location.replace("/ng?pages");
    win.focus();
    popout.current = win;
    setPages(false);
  };

  return (
    <NgScreenView
      width={width}
      height={height}
      bottom={bottom}
      mode={mode}
      screen={{ x, y, width: w, height: h }}
      boxRef={box}
      cursor={r.shape && !r.hidden && !r.terminal && r.connected ? { shape: r.shape, x: r.pos.x, y: r.pos.y, scale: shown } : null}
      overlayRef={overlay}
      onPad={pad}
      onTouchScreen={onTouchScreen}
      keyboard={
        keysShown
          ? {
              docked: dockedNow,
              height: keysH,
              keys: { fn: r.fn, mods: r.mods, caps: r.caps, onKeyTouch: r.onKeyTouch, pad: r.padMode, dictation: r.dictation },
              onHeight: (h) => setDragH(Math.max(MIN_KEYS_H, Math.min(height * MAX_KEYS_PART, h))),
              onHeightDone: () => {
                if (dragH === null) return;
                saveKeyboardHeight(dragH / height);
                setKeysPart(dragH / height);
                setDragH(null);
              },
            }
          : null
      }
      notes={{
        dictation: r.dictation,
        heard: r.heard,
        note: r.note,
        hint: hint && !pages ? `${pagesChordLabel()} shows the pages` : null,
      }}
      pages={{
        open: pages,
        tab,
        onTab: setTab,
        usage: r.usage,
        badges: r.unread ? { clones: r.unread } : undefined,
        onPopOut: windows ? popOut : undefined,
        lift,
        panelRef,
        content: (
          <NgPagesContainer
            tab={pages ? tab : null}
            onTab={(t) => (t ? setTab(t) : setPages(false))}
            state={r.live.state}
            stats={r.live.stats}
            sound={r.soundReport}
          />
        ),
      }}
      // In mouse mode the buttons show only with the pages
      buttons={
        !mouse || pages
          ? {
              pagesOpen: pages,
              keyboard: !mouse,
              keyboardOpen: keysShown,
              keyboardDocked: docked,
              fits,
              badge: r.unread,
              onPages: () => setPages((p) => !p),
              onKeyboard: () => !docked && setKeyboard((k) => !k),
              onKeyboardDock: toggleDock,
              onFit: () => fitTo(target),
              onFitSettings: () => setLayoutOpen(true),
              fullscreen: fullscreen === null ? undefined : { on: fullscreen, toggle: toggleFullscreen },
            }
          : null
      }
      dialog={
        layoutOpen ? (
          <LayoutEditorContainer
            page="/ng"
            windows={windows}
            here={monitor}
            onApplied={(l) => {
              setLayout(l);
              r.claim();
            }}
            onClose={() => setLayoutOpen(false)}
          />
        ) : null
      }
      // Not over the dialog: there its box says "here"
      identify={identify && !layoutOpen ? monitor : null}
      onPointerDownCapture={onDownCapture}
    />
  );
}
