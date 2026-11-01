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
//   cursor (`followAxis`): zoomed in, with the keyboard up, or with the cursor under the
//   buttons' row, it moves just enough to keep the cursor in sight. With the keyboard docked,
//   it is always shown at the bottom, the clone's screen fits in the area above it, and that
//   area is a touch screen: each finger goes to the clone as a touch.
// - Mouse and keyboard: the mouse, the keys and the clipboard go to the clone as in the viewer
//   (`~/viewer/input.ts`). The same buttons, less the keyboard's, show when the mode starts,
//   then slide out of the window to the right; the mouse at the window's right edge, by their
//   row, brings them back. A
//   chord (`./shortcut.ts`) shows the pages, with the buttons above them, and hides them again;
//   so does a click beside them.
//
// Where the browser can open windows, the pages can move to a window of their own (`?pages`);
// while the page is fullscreen, on another screen where there is one (`./otherScreen.ts`), as
// one on this screen takes the page out of its fullscreen. A button opens the windows of the
// layout's other monitors while any is not open.
//
// It runs in the frame of the /ng shell (`public/ng-shell.html`) where the browser has
// fullscreen: the fullscreen, the key lock, the window's name and address are the shell's
// (`~/lib/host.ts`), so a reload onto a new build keeps them.
//
// The clone's screen shows at the pixel scale in the middle (smaller only where it does not
// fit), not stretched. This browser's layout is in force while it is in use: a window opens,
// comes to the front, or is touched or clicked.
//
// A headless clone has a terminal in place of a desktop: monitor 0's window shows it
// (`./NgTerminal.tsx`), the others say where it is. Its keyboard is always under it, not over it.
//
// The connection and the input are `./remote.ts`'s; the drawing is `./NgScreenView.tsx`'s.
import { type PointerEvent, useEffect, useRef, useState } from "react";

import { LayoutEditorContainer } from "~/components/LayoutEditorContainer";
import { type BrowserLayout, clientKind, ensureLayout, readLayout, saveLayout } from "~/lib/clientLayout";
import { resize } from "~/lib/monitorLayout";
import { hostWindow } from "~/lib/host";
import { useOpenMonitors, useViewerChannel } from "~/lib/useViewerChannel";
import { openViewerWindows } from "~/viewer/open";
import { toggleFullscreen } from "~/viewer/input";

import { firstLayout, startScale } from "./firstLayout";
import { FLOAT_BUTTON, FLOAT_GAP } from "./FloatingButtons";
import { followAxis, naturalKeysHeight, phoneMonitorSize, screenFit } from "./geometry";
import { useInputMode } from "./inputMode";
import { HANDLE_H } from "./KeyboardHandle";
import { type TouchKind } from "./KeyboardView";
import { readKeyboardDocked, readKeyboardHeight, saveKeyboardDocked, saveKeyboardHeight } from "./keyboardDock";
import { NgScreenView, pagesBox } from "./NgScreenView";
import { NgTerminal } from "./NgTerminal";
import { boxOnOtherScreen } from "./otherScreen";
import { NgPagesContainer } from "./pages/NgPagesContainer";
import { clampTo } from "./pointer";
import { useRemote } from "./remote";
import { isPagesChord } from "./shortcut";
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
/** How long the buttons show when mouse and keyboard mode starts, ms. */
const INTRO_MS = 4000;
/** Mouse mode: the mouse this close to the window's right edge (CSS px), by the buttons' row,
 *  brings the buttons in; this much (CSS px) away from their row, it sends them away again. */
const RIGHT_EDGE_PX = 2;
const BUTTONS_MARGIN_PX = 24;
/** The pages' own window: its name, and its size when it opens. */
const PAGES_WINDOW = "rmng-pages";
const PAGES_WINDOW_W = 560;
const PAGES_WINDOW_H = 900;

type Props = {
  /** The display, CSS px, and the clock bar's and the home bar's safe-area insets. */
  width: number;
  height: number;
  top: number;
  bottom: number;
  /** The monitor this window shows. */
  monitor: number;
  /** The monitor has another number now (the Edit layout dialog renumbered them). */
  onMonitor: (monitor: number) => void;
};

type Size = { width: number; height: number };

export function FullDesk({ width, height, top, bottom, monitor, onMonitor }: Props) {
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
  /** Mouse mode: the buttons are in the window (the mode is new, or the mouse is at the top). */
  const [peek, setPeek] = useState(false);
  /** The page is fullscreen (its own fullscreen); null where the browser has none. */
  const [fullscreen, setFullscreen] = useState<boolean | null>(null);
  useEffect(() => {
    // The shell's fullscreen, when this page is in its frame (`~/lib/host.ts`)
    const doc = hostWindow().document;
    if (!doc.fullscreenEnabled) return;
    const on = () => setFullscreen(doc.fullscreenElement !== null);
    on();
    doc.addEventListener("fullscreenchange", on);
    return () => doc.removeEventListener("fullscreenchange", on);
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
      // The terminal took it already (`./NgTerminal.tsx`)
      if (mouseInputRef.current || e.defaultPrevented || !isPagesChord(e)) return;
      e.preventDefault();
      if (!e.repeat) chordRef.current();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, []);

  /** Where the pointer was last, CSS px. */
  const pointer = useRef({ x: -Infinity, y: Infinity });
  useEffect(() => {
    const onMove = (e: globalThis.PointerEvent) => (pointer.current = { x: e.clientX, y: e.clientY });
    window.addEventListener("pointermove", onMove, { capture: true, passive: true });
    return () => window.removeEventListener("pointermove", onMove, { capture: true });
  }, []);
  /** The buttons' row ends this far down (CSS px): under the clock bar, a gap, the buttons, a gap. */
  const rowEnd = useRef(0);
  rowEnd.current = top + FLOAT_GAP * 2 + FLOAT_BUTTON;
  // Mouse and keyboard mode starts: the buttons show for a while, then they slide out of the
  // window to the right. The mouse at the window's right edge, as high as their row, brings
  // them back; away from their row it sends them away again.
  useEffect(() => {
    if (!mouse) return;
    setZoom(1);
    setPeek(true);
    let intro = true;
    /** The mouse is at the buttons' row, or by it. Their row's width is the page's to say. */
    const isNear = ({ x, y }: { x: number; y: number }) => {
      const row = document.querySelector<HTMLElement>("[data-ng-buttons]")?.offsetWidth ?? 0;
      return y <= rowEnd.current + BUTTONS_MARGIN_PX && x >= window.innerWidth - FLOAT_GAP - row - BUTTONS_MARGIN_PX;
    };
    /** The start's time ends without sending them away while the mouse is by them. The move
     *  that started the mode came before this listener: it is in `pointer`. */
    let near = isNear(pointer.current);
    const t = setTimeout(() => {
      intro = false;
      if (!near) setPeek(false);
    }, INTRO_MS);
    const onMove = (e: globalThis.PointerEvent) => {
      if (e.pointerType !== "mouse") return;
      const at = { x: e.clientX, y: e.clientY };
      near = isNear(at);
      if (at.x >= window.innerWidth - RIGHT_EDGE_PX && at.y <= rowEnd.current) setPeek(true);
      else if (!near && !intro) setPeek(false);
    };
    window.addEventListener("pointermove", onMove, { capture: true, passive: true });
    return () => {
      clearTimeout(t);
      window.removeEventListener("pointermove", onMove, { capture: true });
    };
  }, [mouse]);

  // The Edit layout dialog finds this window by its name and its number
  useEffect(() => {
    if (windows) hostWindow().name = `rmng-viewer-${monitor}`;
    // Several windows: each says its monitor in its title
    hostWindow().document.title = windows && monitor > 0 ? `RMNG monitor ${monitor}` : "RMNG";
  }, [windows, monitor]);
  // A desktop layout of several monitors: which have a window open, so a button can open the
  // others' (the first open of the site shows monitor 0 only)
  const many = windows && (layout?.monitors.length ?? 0) > 1;
  const openMonitors = useOpenMonitors(many);
  const missing =
    many && layout && openMonitors.open
      ? layout.monitors.map((_, i) => i).filter((i) => i !== monitor && !openMonitors.open!.has(i))
      : [];
  const openMissing = () => {
    // One window a click where the browser blocks the rest: the button stays for them
    void openViewerWindows(
      missing.map((id) => ({ id })),
      "/ng",
      false,
    ).then(() => setTimeout(openMonitors.look, 1500));
  };
  const identify = useViewerChannel(
    monitor,
    // The size Fit would give, at the pixel scale, so a match gives just that
    () => ({ width: target.width * scale, height: target.height * scale }),
    (next) => {
      if (next !== null) return onMonitor(next);
      // Its monitor was removed: the window closes, where the page opened it
      hostWindow().close();
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
  // In touch mode the buttons are always there: the cursor stays below their row too, and the
  // screen moves down for it, its start no lower than the row's end (black above it)
  const buttonsEnd = top + FLOAT_GAP * 2 + FLOAT_BUTTON;
  const y = still ? fit.top : followAxis(prev.y, cy, h, visible, height, CURSOR_MARGIN, buttonsEnd);
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
  /** Move the pages to a window of their own (an open one is only brought to the front). While
   *  the page is fullscreen, on another screen where there is one (an open one is moved there). */
  const popOut = async () => {
    const box = fullscreen ? await boxOnOtherScreen(PAGES_WINDOW_W, PAGES_WINDOW_H) : null;
    const place = box ? `,left=${box.left},top=${box.top},width=${box.width},height=${box.height}` : "";
    const win = window.open("", PAGES_WINDOW, `popup,width=${PAGES_WINDOW_W},height=${PAGES_WINDOW_H}${place}`);
    // Blocked: the permission's question took the click's time; the next click opens it
    if (!win) return;
    if (win.location.href === "about:blank") win.location.replace("/ng/app?pages");
    else if (box) win.moveTo(box.left, box.top);
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
      terminal={
        r.termContent ? (
          monitor === 0 ? (
            // A new clone: new terminals, their tabs empty
            <NgTerminal
              key={r.termContent.clone}
              sessions={r.termContent.sessions}
              send={r.sendTerm}
              sink={r.termSink}
              api={r.termApi}
              touch={!mouse}
              top={top}
              cursor={mouse ? null : r.termPos}
              onPad={pad}
              onChord={togglePages}
            />
          ) : (
            <div className="absolute inset-0 bg-black" />
          )
        ) : null
      }
      cursor={r.shape && !r.hidden && !r.terminal && r.connected ? { shape: r.shape, x: r.pos.x, y: r.pos.y, scale: shown } : null}
      overlayRef={overlay}
      onPad={pad}
      onTouchScreen={onTouchScreen}
      keyboard={
        keysShown
          ? {
              // Under the terminal, never over it
              docked: dockedNow || r.termContent !== null,
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
      }}
      pages={{
        open: pages,
        tab,
        onTab: setTab,
        usage: r.usage,
        badges: r.unread ? { clones: r.unread } : undefined,
        onPopOut: windows ? () => void popOut() : undefined,
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
      buttons={{
        pagesOpen: pages,
        keyboard: !mouse,
        keyboardOpen: keysShown,
        keyboardDocked: docked,
        // A browser that opens windows fits each monitor in the Edit layout dialog
        // A terminal fits by itself
        fit: r.termContent || (windows && fullscreen !== null) ? undefined : { fits, onFit: () => fitTo(target) },
        badge: r.unread,
        onPages: () => setPages((p) => !p),
        onKeyboard: () => !docked && setKeyboard((k) => !k),
        onKeyboardDock: toggleDock,
        onLayout: () => setLayoutOpen(true),
        fullscreen: fullscreen === null ? undefined : { on: fullscreen, toggle: toggleFullscreen },
        monitors: missing.length ? { missing, onOpen: openMissing } : undefined,
        // In mouse mode, out of the window unless the mouse brought them in, or the pages or
        // the dialog are open
        away: mouse && !peek && !pages && !layoutOpen,
      }}
      dialog={
        layoutOpen ? (
          <LayoutEditorContainer
            page="/ng"
            windows={windows}
            here={monitor}
            glass
            onApplied={(l) => {
              setLayout(l);
              r.claim();
            }}
            onClose={() => setLayoutOpen(false)}
          />
        ) : null
      }
      // In every monitor window of a desktop while the dialog is open, this one too (the number
      // is in a corner, clear of the dialog); a phone has one monitor, nothing to match
      identify={windows && (identify || layoutOpen) ? monitor : null}
      onPointerDownCapture={onDownCapture}
    />
  );
}
