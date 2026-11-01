// The app's page, on any screen and in any orientation: one monitor of this browser's layout
// (`~/lib/clientLayout.ts`; `?monitor=`, 0 when absent) fills the window. On a desktop each
// monitor has a window of its own; a phone has one monitor, this page.
//
// It has two modes, chosen by the input used last (`./inputMode.ts`):
//
// - Touch: three floating buttons (`./FloatingButtons.tsx`) open the rest:
//   - Control panel: the control panel's tabs (usage, tickets, clones, notes,
//     settings) over the screen.
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
// - Mouse and keyboard: the mouse, the keys and the clipboard go to the clone
//   (`~/viewer/input.ts`). The same buttons, less the keyboard's, and on a large screen (the
//   control panel's wide layout) the side panels under them (the clone's notes and its
//   assistant), are one group at the right: it shows when the mode starts, then slides out of
//   the window to the right; the mouse at the window's right edge brings it back, and a move
//   away from it sends it out. While the mouse is over it, the mouse and the keys are its. A
//   chord (`./shortcut.ts`) shows the control panel, with the buttons above it, and hides it again;
//   so does a click beside them, and Escape when no dialog or menu is open.
//
// Where the browser can open windows, the control panel can move to a window of its own (`?control-panel`);
// while the page is fullscreen, on another screen where there is one (`./otherScreen.ts`), as
// one on this screen takes the page out of its fullscreen. A button opens the windows of the
// layout's other monitors while any is not open.
//
// It runs in the frame of the shell (`public/shell.html`) where the browser has
// fullscreen: the fullscreen, the key lock, the window's name and address are the shell's
// (`~/lib/host.ts`), so a reload onto a new build keeps them.
//
// The clone's screen shows at the pixel scale in the middle (smaller only where it does not
// fit), not stretched. This browser's layout is in force while it is in use: a window opens,
// comes to the front, or is touched or clicked.
//
// A headless clone has a terminal in place of a desktop: monitor 0's window shows it
// (`./TerminalScreen.tsx`), the others say where it is. Its keyboard is always under it, not over it.
//
// The connection and the input are `./remote.ts`'s; the drawing is `./ScreenView.tsx`'s.
import { type PointerEvent, useEffect, useRef, useState } from "react";

import { LayoutEditorContainer } from "~/components/LayoutEditorContainer";
import { type BrowserLayout, clientKind, ensureLayout, readLayout, saveLayout } from "~/lib/clientLayout";
import { resize } from "~/lib/monitorLayout";
import { hostWindow } from "~/lib/host";
import { useMonitorChannel, useOpenMonitors } from "~/lib/useMonitorChannel";
import { monitorWindowName, openMonitorWindows } from "~/viewer/open";
import { toggleFullscreen } from "~/viewer/input";
import { uploadToClone } from "~/viewer/upload";

import { attachEditKeys } from "./editKeys";
import { firstLayout, startScale } from "./firstLayout";
import { FLOAT_BUTTON, FLOAT_GAP } from "./FloatingButtons";
import { followAxis, type Insets, naturalKeysHeight, phoneMonitorSize, screenFit } from "./geometry";
import { useInputMode } from "./inputMode";
import { HANDLE_H } from "./KeyboardHandle";
import { type TouchKind } from "./KeyboardView";
import { readKeyboardDocked, readKeyboardHeight, saveKeyboardDocked, saveKeyboardHeight } from "./keyboardDock";
import { ScreenView, controlPanelBox } from "./ScreenView";
import { TerminalScreen } from "./TerminalScreen";
import { boxOnOtherScreen } from "./otherScreen";
import { ControlPanelContainer } from "./control/ControlPanelContainer";
import { WIDE_MIN } from "./control/WideLayout";
import { clampTo } from "./pointer";
import { useRemote } from "./remote";
import { isControlPanelChord, controlPanelChordLabel } from "./shortcut";
import { isTyping, useLift } from "./systemKeyboard";
import type { PanelTab } from "./ToolRow";

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
/** How long the right group shows when mouse and keyboard mode starts, ms. */
const INTRO_MS = 4000;
/** Mouse mode: the mouse this close to the window's right edge (CSS px) brings the right group
 *  in; this far (CSS px) left of the group, it sends it out again. */
const RIGHT_EDGE_PX = 2;
const GROUP_MARGIN_PX = 48;
/** Touch mode, wide: a finger that comes down this close to the right edge (CSS px) and moves
 *  this far left brings the side panels in. */
const SWIPE_EDGE_PX = 24;
const SWIPE_PX = 40;
/** How long the note of a finished upload stays (one that failed: twice this). */
const UPLOAD_NOTE_MS = 5000;
/** The control panel's own window: its name, and its size when it opens. */
const CONTROL_PANEL_WINDOW = "rmng-control-panel";
const CONTROL_PANEL_WINDOW_W = 560;
const CONTROL_PANEL_WINDOW_H = 900;

type Props = {
  /** The display, CSS px, and its unsafe edges (the clock bar, the home bar, a cut-out). */
  width: number;
  height: number;
  safe: Insets;
  /** The monitor this window shows. */
  monitor: number;
  /** The monitor has another number now (the Edit layout dialog renumbered them). */
  onMonitor: (monitor: number) => void;
};

type Size = { width: number; height: number };

export function FullDesk({ width, height, safe, monitor, onMonitor }: Props) {
  const { top, bottom } = safe;
  const box = useRef<HTMLDivElement>(null);
  const overlay = useRef<HTMLImageElement>(null);
  const panelRef = useRef<HTMLDivElement>(null);
  /** A desktop (it opens windows: one per monitor, and the control panel's) or a phone. */
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
  /** The control panel over the screen, and the tab it shows; the Edit layout dialog. */
  const [controlPanel, setControlPanel] = useState(false);
  const [tab, setTab] = useState<PanelTab>("clones");
  const [layoutOpen, setLayoutOpen] = useState(false);
  /** Mouse mode: the right group (the buttons, and the side panels) is in the window (the mode
   *  is new, or the mouse is at the right edge), and the mouse is over it. */
  const [peek, setPeek] = useState(false);
  const [overGroup, setOverGroup] = useState(false);
  /** The control panel's wide layout fits: the side panels can come in without it. */
  const wide = controlPanelBox(width, height, safe).width >= WIDE_MIN;
  /** Touch mode, wide: a swipe from the right edge brought the side panels in. Their box. */
  const [swiped, setSwiped] = useState(false);
  const [sideAt, setSideAt] = useState<HTMLDivElement | null>(null);
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
      (keysPart ? height * keysPart : Math.min(Math.ceil(naturalKeysHeight(width - safe.left - safe.right, bottom) + HANDLE_H), height * KEYBOARD_PART)),
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

  // A popped-out control panel window, while it is open
  const popout = useRef<Window | null>(null);
  /** The chord: the control panel, or its own window when it is open. */
  const toggleControlPanel = () => {
    const w = popout.current;
    if (w && !w.closed) w.focus();
    else setControlPanel((p) => !p);
  };
  /** The mouse and the keys go to the clone: mouse mode, nothing over the screen. */
  const mouseInput = mouse && !controlPanel && !layoutOpen && !(peek && overGroup);
  const r = useRemote({ box, overlay, monitor, kind, mouse: mouseInput, onZoom, onChord: toggleControlPanel });

  // The chord while the clone does not get the keys (`~/viewer/input.ts` takes it otherwise)
  const chordRef = useRef(toggleControlPanel);
  chordRef.current = toggleControlPanel;
  const mouseInputRef = useRef(mouseInput);
  mouseInputRef.current = mouseInput;
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      // The terminal took it already (`./TerminalScreen.tsx`)
      if (mouseInputRef.current || e.defaultPrevented || !isControlPanelChord(e)) return;
      e.preventDefault();
      if (!e.repeat) chordRef.current();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, []);

  // A Mac's edit shortcuts (Cmd+C, Cmd+V…) while the keyboard is locked and the page has the
  // keys: the page runs them itself (`./editKeys.ts`)
  useEffect(
    () =>
      attachEditKeys({
        active: () => !mouseInputRef.current,
        locked: () => hostWindow().document.fullscreenElement !== null,
      }),
    [],
  );

  // Escape: a dialog or a menu closes itself first (each has its own Escape); with none open,
  // the control panel closes. In a text box the first Escape only leaves the box, so a key
  // meant for the field (a rename's cancel) does not also close the panel.
  const controlPanelRef = useRef(controlPanel);
  controlPanelRef.current = controlPanel;
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key !== "Escape" || e.defaultPrevented || !controlPanelRef.current) return;
      if (document.querySelector("[data-modal], [role=dialog], [role=menu], [role=listbox]")) return;
      const t = e.target;
      if (t instanceof HTMLElement && (t.isContentEditable || t.matches("input, textarea, select"))) {
        t.blur();
        return;
      }
      e.preventDefault();
      setControlPanel(false);
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, []);

  // Files dropped on the clone's screen go to the selected clone's home, one after another
  // (`~/viewer/upload.ts`). A drop on the control panel, the right group or a dialog is theirs.
  const [upload, setUpload] = useState<string | null>(null);
  useEffect(() => {
    let clear: ReturnType<typeof setTimeout> | undefined;
    const say = (text: string, ms?: number) => {
      clearTimeout(clear);
      setUpload(text);
      if (ms) clear = setTimeout(() => setUpload(null), ms);
    };
    const ours = (e: DragEvent) =>
      !e.defaultPrevented &&
      (e.dataTransfer?.types.includes("Files") ?? false) &&
      !(e.target instanceof Element && e.target.closest("[data-right-group], [data-modal], [role=dialog]")) &&
      !(e.target instanceof Node && panelRef.current?.contains(e.target));
    const onOver = (e: DragEvent) => {
      if (!ours(e)) return;
      e.preventDefault();
      e.dataTransfer!.dropEffect = "copy";
    };
    const onDrop = async (e: DragEvent) => {
      if (!ours(e)) return;
      e.preventDefault();
      const done: string[] = [];
      for (const f of [...e.dataTransfer!.files]) {
        try {
          done.push(await uploadToClone(f, (p) => say(`Uploading ${f.name} to ~/ … ${Math.round(p * 100)}%`)));
        } catch (err) {
          say(`Upload of ${f.name} failed: ${(err as Error).message}`, UPLOAD_NOTE_MS * 2);
          return;
        }
      }
      say(`Uploaded to ~/: ${done.join(", ")}`, UPLOAD_NOTE_MS);
    };
    window.addEventListener("dragover", onOver);
    window.addEventListener("drop", onDrop);
    return () => {
      clearTimeout(clear);
      window.removeEventListener("dragover", onOver);
      window.removeEventListener("drop", onDrop);
    };
  }, []);

  /** Where the pointer was last, CSS px. */
  const pointer = useRef({ x: -Infinity, y: Infinity });
  useEffect(() => {
    const onMove = (e: globalThis.PointerEvent) => (pointer.current = { x: e.clientX, y: e.clientY });
    window.addEventListener("pointermove", onMove, { capture: true, passive: true });
    return () => window.removeEventListener("pointermove", onMove, { capture: true });
  }, []);
  // Mouse and keyboard mode starts: the right group shows for a while, then it slides out of
  // the window to the right. The mouse at the window's right edge brings it back; left of it
  // (by GROUP_MARGIN_PX, and with no button held, as a text selection's drag), out again.
  useEffect(() => {
    if (!mouse) {
      setPeek(false);
      setOverGroup(false);
      return;
    }
    setZoom(1);
    setPeek(true);
    let intro = true;
    /** The group's left edge, less the margin (its width is the page's to say). */
    const groupLeft = () =>
      window.innerWidth - (document.querySelector<HTMLElement>("[data-right-group]")?.offsetWidth ?? 0) - GROUP_MARGIN_PX;
    /** The start's time ends without sending it out while the mouse is over it. The move that
     *  started the mode came before this listener: it is in `pointer`. */
    let over = pointer.current.x >= groupLeft();
    setOverGroup(over);
    const t = setTimeout(() => {
      intro = false;
      if (!over) setPeek(false);
    }, INTRO_MS);
    const onMove = (e: globalThis.PointerEvent) => {
      if (e.pointerType !== "mouse") return;
      over = e.clientX >= groupLeft();
      setOverGroup(over);
      if (e.clientX >= window.innerWidth - RIGHT_EDGE_PX) setPeek(true);
      else if (!over && !intro && e.buttons === 0) setPeek(false);
    };
    window.addEventListener("pointermove", onMove, { capture: true, passive: true });
    return () => {
      clearTimeout(t);
      window.removeEventListener("pointermove", onMove, { capture: true });
    };
  }, [mouse]);

  // Touch mode, wide: a finger from the right edge, moving left, brings the side panels in
  // (`onDownCapture` starts it); a touch beside them sends them out
  const swipe = useRef<{ id: number; x: number; y: number } | null>(null);
  useEffect(() => {
    if (mouse || !wide) {
      setSwiped(false);
      return;
    }
    const onMove = (e: globalThis.PointerEvent) => {
      const s = swipe.current;
      if (!s || e.pointerId !== s.id) return;
      const dx = e.clientX - s.x;
      if (dx < -SWIPE_PX && Math.abs(dx) > Math.abs(e.clientY - s.y)) {
        swipe.current = null;
        setSwiped(true);
      }
    };
    const onEnd = (e: globalThis.PointerEvent) => {
      if (swipe.current?.id === e.pointerId) swipe.current = null;
    };
    window.addEventListener("pointermove", onMove, { capture: true, passive: true });
    window.addEventListener("pointerup", onEnd, { capture: true });
    window.addEventListener("pointercancel", onEnd, { capture: true });
    return () => {
      window.removeEventListener("pointermove", onMove, { capture: true });
      window.removeEventListener("pointerup", onEnd, { capture: true });
      window.removeEventListener("pointercancel", onEnd, { capture: true });
    };
  }, [mouse, wide]);
  const sideShown = wide && (controlPanel || (mouse ? peek : swiped));
  // Out of the window, the side panels keep no focus: the keys go back to the clone
  useEffect(() => {
    const a = document.activeElement;
    if (!sideShown && a instanceof HTMLElement && a.closest("[data-side-panels]")) a.blur();
  }, [sideShown]);

  // The Edit layout dialog finds this window by its name and its number
  useEffect(() => {
    if (windows) hostWindow().name = monitorWindowName(monitor);
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
    void openMonitorWindows(
      missing.map((id) => ({ id })),
      false,
    ).then(() => setTimeout(openMonitors.look, 1500));
  };
  const identify = useMonitorChannel(
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

  /** Where the view ends at the bottom: the keyboard's top, or the home bar's. */
  const visible = keysShown ? height - keysH : height - bottom;

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
  // With a mouse, the screen stays in the middle of its area: the cursor is the mouse's own. In
  // touch mode the screen may fill the display, but the cursor stays in the safe area: clear of
  // the sides' insets, above the home bar and the keyboard
  const x = mouse ? fit.left : followAxis(prev.x, cx, w, width - safe.right, width, CURSOR_MARGIN, safe.left);
  // The buttons are always there: the cursor stays below their row too, and the screen moves
  // down for it, its start no lower than the row's end (black above it). Docked, the screen
  // rests in the middle of its area above the keys, and moves the same way
  const buttonsEnd = top + FLOAT_GAP * 2 + FLOAT_BUTTON;
  const y = mouse
    ? fit.top
    : followAxis(prev.y, cy, h, dockedNow ? areaH : visible, dockedNow ? areaH : height, CURSOR_MARGIN, buttonsEnd);
  at.current = { x, y, shown };

  // The phone's keyboard lifts the control panel so all of the open one is above it
  const panel = controlPanelBox(width, height, safe);
  const lift = useLift(panel.top + panel.height);

  const onDownCapture = (e: PointerEvent<HTMLDivElement>) => {
    r.sounds.unlock();
    const target = e.target as Element;
    // Touch mode, wide: a finger at the right edge may be a swipe for the side panels (only when
    // there are panels to bring: none while no clone is selected), and a touch beside them while
    // they are in sends them out. Neither reaches the clone.
    if (!mouse && wide && e.pointerType === "touch" && !controlPanel) {
      if (swiped && !target.closest("[data-side-panels], [data-float-buttons]")) {
        setSwiped(false);
        e.stopPropagation();
        return;
      }
      if (!swiped && e.clientX >= window.innerWidth - safe.right - SWIPE_EDGE_PX && document.querySelector("[data-side-panels] section")) {
        swipe.current = { id: e.pointerId, x: e.clientX, y: e.clientY };
        e.stopPropagation();
        return;
      }
    }
    // A touch outside the control panel puts the phone's keyboard away
    if (isTyping() && !target.closest("[role=tabpanel]")) {
      (document.activeElement as HTMLElement).blur();
    }
    // With a mouse, a click beside the control panel and the buttons puts the control panel away. A menu of
    // theirs (a card's ⋯), a dropdown's list and a dialog of theirs are drawn on the body, out
    // of their box, and are theirs all the same.
    if (
      mouse &&
      controlPanel &&
      !layoutOpen &&
      !panelRef.current?.contains(target) &&
      !target.closest("[data-float-buttons], [data-side-panels], [role=menu], [role=listbox], [data-modal]")
    ) {
      setControlPanel(false);
    }
  };

  // The whole display is the trackpad (under the keyboard, the control panel and the buttons)
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
  /** Move the control panel to a window of its own (an open one is only brought to the front). While
   *  the page is fullscreen, on another screen where there is one (an open one is moved there). */
  const popOut = async () => {
    const box = fullscreen ? await boxOnOtherScreen(CONTROL_PANEL_WINDOW_W, CONTROL_PANEL_WINDOW_H) : null;
    const place = box ? `,left=${box.left},top=${box.top},width=${box.width},height=${box.height}` : "";
    const win = window.open("", CONTROL_PANEL_WINDOW, `popup,width=${CONTROL_PANEL_WINDOW_W},height=${CONTROL_PANEL_WINDOW_H}${place}`);
    // Blocked: the permission's question took the click's time; the next click opens it
    if (!win) return;
    if (win.location.href === "about:blank") win.location.replace("/app?control-panel");
    else if (box) win.moveTo(box.left, box.top);
    win.focus();
    popout.current = win;
    setControlPanel(false);
  };

  return (
    <ScreenView
      width={width}
      height={height}
      safe={safe}
      mode={mode}
      screen={{ x, y, width: w, height: h }}
      boxRef={box}
      terminal={
        r.termContent ? (
          monitor === 0 ? (
            // A new clone: new terminals, their tabs empty
            <TerminalScreen
              key={r.termContent.clone}
              sessions={r.termContent.sessions}
              send={r.sendTerm}
              sink={r.termSink}
              api={r.termApi}
              touch={!mouse}
              top={top}
              cursor={mouse ? null : r.termPos}
              onPad={pad}
              onChord={toggleControlPanel}
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
        note: r.note ?? upload,
      }}
      controlPanel={{
        open: controlPanel,
        lift,
        panelRef,
        content: (
          <ControlPanelContainer
            width={panel.width}
            glass
            usage={r.usage}
            badges={r.unread ? { clones: r.unread } : undefined}
            onPopOut={windows ? () => void popOut() : undefined}
            tab={controlPanel ? tab : null}
            onTab={(t) => (t ? setTab(t) : setControlPanel(false))}
            state={r.live.state}
            stats={r.live.stats}
            sound={r.soundReport}
            sideAt={wide ? sideAt : undefined}
            onLayout={() => setLayoutOpen(true)}
            notify={monitor === 0}
          />
        ),
      }}
      // In mouse mode the panels go with the group (`away`), so they do not slide on their own
      side={wide ? { shown: mouse || sideShown, ref: setSideAt } : null}
      buttons={{
        controlPanelOpen: controlPanel,
        keyboard: !mouse,
        keyboardOpen: keysShown,
        keyboardDocked: docked,
        // A browser that opens windows fits each monitor in the Edit layout dialog
        // A terminal fits by itself
        fit: r.termContent || (windows && fullscreen !== null) ? undefined : { fits, onFit: () => fitTo(target) },
        badge: r.unread,
        onControlPanel: () => setControlPanel((p) => !p),
        onKeyboard: () => !docked && setKeyboard((k) => !k),
        onKeyboardDock: toggleDock,
        onLayout: () => setLayoutOpen(true),
        fullscreen: fullscreen === null ? undefined : { on: fullscreen, toggle: toggleFullscreen },
        controlPanelKeys: controlPanelChordLabel(),
        monitors: missing.length ? { missing, onOpen: openMissing } : undefined,
      }}
      // The right group (the buttons, the side panels): in mouse mode out of the window unless
      // the mouse brought it in, or the control panel or the dialog are open
      away={mouse && !peek && !controlPanel && !layoutOpen}
      dialog={
        layoutOpen ? (
          <LayoutEditorContainer
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
      // In every monitor window of a desktop while the dialog is open, this one too (the number
      // is in a corner, clear of the dialog); a phone has one monitor, nothing to match
      identify={windows && (identify || layoutOpen) ? monitor : null}
      onPointerDownCapture={onDownCapture}
    />
  );
}
