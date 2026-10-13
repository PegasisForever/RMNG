// The browser viewer's input and cursor, on the page thread. Events come from the page's
// placeholder <canvas> (it stays in the DOM after `transferControlToOffscreen`, so pointer
// events and pointer lock work on it) and keys from `window`. Each event becomes an `InputMsg`
// that the page hands to the worker, which sends it on the socket.
//
// What it sends: Linux evdev keycodes, evdev buttons, wheel notches
// or smooth finger scroll, pointer lock with relative motion that engages by itself while the
// clone hides its cursor, the Ctrl+Alt+G / Ctrl+Alt+P chords, and the agent-warp rules.
import type { CursorMeta } from "../lib/wire/CursorMeta";
import type { CursorShape } from "../lib/wire/CursorShape";
import type { InputMsg } from "../lib/wire/InputMsg";
import type { ViewMonitor } from "../lib/wire/ViewMonitor";
import { CODE_TO_EVDEV } from "./keycodes";

/** Engage the automatic pointer lock after the remote cursor stays hidden this long. */
export const ENGAGE_MS = 180;
/** Release it after the remote cursor stays visible this long. */
export const RELEASE_MS = 300;
/** After an agent-driven move (warp), local motion is not sent for this long. */
const WARP_SUPPRESS_MS = 500;
/** After an agent-driven move, the remote cursor is drawn on the overlay for this long. */
const AGENT_CURSOR_SHOW_MS = 1000;
/** Chrome ignores a CSS cursor larger than this many CSS pixels; larger cursors use the overlay. */
const CSS_CURSOR_MAX = 128;
/** No smooth-scroll event for this long ends the scroll gesture (the browser has no end event). */
const SCROLL_END_MS = 150;

/** Evdev buttons by `MouseEvent.button`: left, middle, right, back, forward. */
const BUTTONS = [0x110, 0x112, 0x111, 0x113, 0x114];
/** Mutter `NotifyPointerAxis` flags (`wire::socket::axis_flags`). */
const AXIS_FINISH = 1;
const AXIS_SOURCE_FINGER = 4;

/** Where a monitor of `monW`×`monH` is drawn in a box of `boxW`×`boxH` (scale to fit): box
 *  pixels per monitor pixel, and the offset of the picture in the box, in whole box pixels so
 *  that at scale 1 each monitor pixel lands on one box pixel. The worker draws with it in
 *  device pixels, and input maps back through it, so the two always agree. */
export function letterbox(boxW: number, boxH: number, monW: number, monH: number) {
  const scale = Math.min(boxW / monW, boxH / monH);
  return {
    scale,
    offX: Math.round((boxW - monW * scale) / 2),
    offY: Math.round((boxH - monH * scale) / 2),
  };
}

/** A page point (CSS pixels, e.g. `clientX`) → monitor pixels, not clamped. The canvas covers
 *  `rect` (CSS pixels) and holds `box` device pixels, in which the worker draws the monitor
 *  through `letterbox`. */
export function toMonitor(
  px: number,
  py: number,
  rect: { left: number; top: number; width: number; height: number },
  box: { w: number; h: number },
  monW: number,
  monH: number,
) {
  const { scale, offX, offY } = letterbox(box.w, box.h, monW, monH);
  return {
    x: (((px - rect.left) * box.w) / rect.width - offX) / scale,
    y: (((py - rect.top) * box.h) / rect.height - offY) / scale,
  };
}

/** `v` held to the pixels of an axis `size` pixels long: 0 to `size - 1`. */
function clampPixel(v: number, size: number): number {
  return Math.min(Math.max(v, 0), Math.max(size - 1, 0));
}

/** A drag at unclamped `origin`-monitor coords → the monitor it is over and the coords there.
 *  The point is lifted into desktop coords through the
 *  layout; dead space (a gap, or outside the desktop) pins it to the origin's edge. Null when
 *  the origin is not in the layout. */
export function routeDrag(
  layout: ViewMonitor[],
  origin: number,
  mx: number,
  my: number,
): { id: number; x: number; y: number } | null {
  const o = layout.find((m) => m.id === origin);
  if (!o) return null;
  const ux = o.x + mx;
  const uy = o.y + my;
  for (const m of layout) {
    if (ux >= m.x && ux < m.x + m.width && uy >= m.y && uy < m.y + m.height) {
      return { id: m.id, x: ux - m.x, y: uy - m.y };
    }
  }
  return { id: origin, x: clampPixel(mx, o.width), y: clampPixel(my, o.height) };
}

/** The cursor sprite as straight RGBA. The daemon sends SPA cursor bytes, which are BGRA
 *  premultiplied despite the field name. */
export function shapeToRgba(shape: CursorShape): Uint8ClampedArray<ArrayBuffer> {
  const src = Uint8Array.from(atob(shape.rgba), (c) => c.charCodeAt(0));
  const out = new Uint8ClampedArray(shape.width * shape.height * 4);
  for (let i = 0; i < out.length; i += 4) {
    const a = src[i + 3];
    const k = a ? 255 / a : 0;
    out[i] = src[i + 2] * k;
    out[i + 1] = src[i + 1] * k;
    out[i + 2] = src[i] * k;
    out[i + 3] = a;
  }
  return out;
}

/** The automatic pointer-lock policy (times in ms):
 *  engage after the remote cursor stays hidden ENGAGE_MS, release after it stays visible
 *  RELEASE_MS. The chords override it until the policy agrees with the override again. */
export class AutoLock {
  private remoteHidden = false;
  private since: number;
  private autoWant = false;
  private overridden: boolean | null = null;

  constructor(now: number) {
    this.since = now;
  }

  /** Latch a visibility change (only updates that say something about visibility). */
  onRemoteCursor(hidden: boolean, now: number) {
    if (hidden !== this.remoteHidden) {
      this.remoteHidden = hidden;
      this.since = now;
    }
  }

  /** Ctrl+Alt+G: flip the effective state; returns the new desire. */
  toggle(now: number) {
    this.overridden = !this.want(now);
    return this.overridden;
  }

  /** Ctrl+Alt+P: force release. */
  forceRelease() {
    this.overridden = false;
  }

  want(now: number) {
    const dwell = now - this.since;
    if (this.remoteHidden) {
      if (dwell >= ENGAGE_MS) this.autoWant = true;
    } else if (dwell >= RELEASE_MS) {
      this.autoWant = false;
    }
    if (this.overridden === this.autoWant) this.overridden = null;
    return this.overridden ?? this.autoWant;
  }
}

/** One wheel event → input messages. A mouse wheel (Chrome reports `wheelDelta` in multiples
 *  of 120 per notch) becomes `axis` notches with the fraction carried in `rem`; anything finer (a trackpad) becomes smooth finger scroll in
 *  CSS pixels. `fingers`: a finger scroll is still running, so this event belongs to it even
 *  if its delta happens to look like a notch. Browser and Mutter agree on the direction:
 *  positive is down and right. */
export function wheelMessages(
  e: { deltaMode: number; deltaX: number; deltaY: number; wheelDeltaX: number; wheelDeltaY: number },
  rem: { x: number; y: number },
  fingers = false,
): InputMsg[] {
  const notched =
    !fingers &&
    (e.wheelDeltaX !== 0 || e.wheelDeltaY !== 0) &&
    e.wheelDeltaX % 120 === 0 &&
    e.wheelDeltaY % 120 === 0;
  if (e.deltaMode === 0 && !notched) {
    if (!e.deltaX && !e.deltaY) return [];
    return [{ kind: "axis_continuous", dx: e.deltaX, dy: e.deltaY, flags: AXIS_SOURCE_FINGER }];
  }
  rem.x += -e.wheelDeltaX / 120;
  rem.y += -e.wheelDeltaY / 120;
  const sx = Math.trunc(rem.x);
  const sy = Math.trunc(rem.y);
  rem.x -= sx;
  rem.y -= sy;
  const out: InputMsg[] = [];
  if (sy) out.push({ kind: "axis", axis: 0, step: sy });
  if (sx) out.push({ kind: "axis", axis: 1, step: sx });
  return out;
}

/** Fullscreen on or off, on the screen the window is on. */
function toggleFullscreen() {
  if (document.fullscreenElement) void document.exitFullscreen();
  else void document.documentElement.requestFullscreen();
}

/** Chrome's Keyboard Lock API; not in TypeScript's DOM types. */
const keyboard = () =>
  (navigator as Navigator & { keyboard?: { lock(): Promise<void>; unlock(): void } }).keyboard;

interface InputOptions {
  /** The placeholder canvas: the event target, the CSS cursor host and the pointer lock target. */
  canvas: HTMLCanvasElement;
  /** The drawn cursor (fixed position, no pointer events). */
  overlay: HTMLImageElement;
  monitor: number;
  send(msg: InputMsg): void;
  /** Ctrl+Alt+S: show or hide the stats overlay. */
  onToggleStats(): void;
  /** A key press or click: the user activation Chrome wants for some calls (a clipboard
   *  write it refused before). */
  onGesture(): void;
  /** The canvas's size in device pixels changed: the worker draws at this size. */
  onResize(width: number, height: number): void;
}

export interface Input {
  /** The monitors of the view spec. No pointer moves are sent while this window's monitor is
   *  not among them. */
  setLayout(monitors: ViewMonitor[]): void;
  onCursor(c: CursorMeta): void;
  setConnected(connected: boolean): void;
  detach(): void;
}

export function attachInput(o: InputOptions): Input {
  const { canvas, overlay, monitor, send } = o;
  const isMac = /Mac/.test(navigator.platform);
  let layout: ViewMonitor[] = [];
  /** This window's monitor in the layout. */
  let size: { w: number; h: number } | null = null;
  /** The canvas in device pixels. */
  let box: { w: number; h: number } | null = null;
  let connected = false;
  const keys = new Set<number>();
  const buttons = new Set<number>();
  const auto = new AutoLock(performance.now());
  /** Chrome refused the lock (the user left it with Esc): wait for a click before asking again. */
  let needGesture = false;
  let lockPending = false;
  let warpUntil = 0;
  let overlayUntil = 0;
  const rem = { x: 0, y: 0 };
  let scrollEnd: ReturnType<typeof setTimeout> | undefined;

  // The remote cursor: last position (monitor pixels), shape, and what was built from it.
  let cursor = { x: 0, y: 0 };
  let shape: CursorShape | null = null;
  let hidden = false;
  /** The sprite built for the current letterbox scale; `tooBig` means the overlay draws it. */
  let built: { key: string; tooBig: boolean } | null = null;

  const locked = () => document.pointerLockElement === canvas;
  const now = () => performance.now();

  /** Release every key and button the clone holds from this window. While the socket is down
   *  there is nothing to release: the server released them when the socket closed. */
  function releaseAll() {
    if (!connected) return;
    for (const k of keys) send({ kind: "key_code", keycode: k, pressed: false });
    for (const b of buttons) send({ kind: "button", button: b, pressed: false });
    keys.clear();
    buttons.clear();
  }

  function requestLock() {
    if (lockPending) return;
    lockPending = true;
    canvas
      .requestPointerLock({ unadjustedMovement: true })
      .catch((e: DOMException) =>
        // Raw motion is not offered on every OS: take the plain lock then.
        e.name === "NotSupportedError" ? canvas.requestPointerLock() : Promise.reject(e),
      )
      .catch(() => {
        needGesture = true;
      })
      .finally(() => {
        lockPending = false;
      });
  }

  /** Make the actual lock follow the policy (`auto_lock::lock_action`): no focused, visible
   *  page means release. Also shows or hides the cursor overlay. */
  function tick() {
    const t = now();
    // No monitor (terminal mode): never lock, whatever the last desktop clone's cursor was.
    const want = size !== null && auto.want(t);
    const target = document.hasFocus() && document.visibilityState === "visible";
    if (want && target && !locked() && !needGesture) requestLock();
    else if ((!want || !target) && locked()) document.exitPointerLock();
    drawCursor(t);
  }

  /** The CSS cursor (sharp at the letterbox scale and device pixel ratio) or, when it is too
   *  large for Chrome or an agent just moved it, the overlay. */
  function drawCursor(t: number) {
    const rect = canvas.getBoundingClientRect();
    if (!size || !box || !shape || hidden) {
      canvas.style.cursor = hidden ? "none" : "";
      overlay.hidden = true;
      return;
    }
    // The letterbox in device pixels, turned into CSS pixels.
    const lb = letterbox(box.w, box.h, size.w, size.h);
    const k = rect.width / box.w;
    const [scale, offX, offY] = [lb.scale * k, lb.offX * k, lb.offY * k];
    const dpr = devicePixelRatio;
    const key = `${scale}/${dpr}`;
    if (built?.key !== key) built = buildCursor(shape, scale, dpr, key);
    const showOverlay = !locked() && (built.tooBig || t < overlayUntil);
    overlay.hidden = !showOverlay;
    if (showOverlay) {
      overlay.style.left = `${rect.left + offX + (cursor.x - shape.hotspot_x) * scale}px`;
      overlay.style.top = `${rect.top + offY + (cursor.y - shape.hotspot_y) * scale}px`;
    }
  }

  function buildCursor(s: CursorShape, scale: number, dpr: number, key: string) {
    const src = new OffscreenCanvas(s.width, s.height);
    src.getContext("2d")!.putImageData(new ImageData(shapeToRgba(s), s.width, s.height), 0, 0);
    const out = document.createElement("canvas");
    out.width = Math.max(1, Math.round(s.width * scale * dpr));
    out.height = Math.max(1, Math.round(s.height * scale * dpr));
    out.getContext("2d")!.drawImage(src, 0, 0, out.width, out.height);
    const url = out.toDataURL("image/png");
    overlay.src = url;
    overlay.style.width = `${s.width * scale}px`;
    overlay.style.height = `${s.height * scale}px`;
    const tooBig = s.width * scale > CSS_CURSOR_MAX || s.height * scale > CSS_CURSOR_MAX;
    canvas.style.cursor = tooBig
      ? "none"
      : `image-set(url("${url}") ${dpr}x) ${Math.round(s.hotspot_x * scale)} ${Math.round(s.hotspot_y * scale)}, default`;
    return { key, tooBig };
  }

  /** A key typed into the page's own terminal (xterm.js reads keys from a textarea). */
  const forPage = (e: KeyboardEvent) => e.target instanceof HTMLTextAreaElement;

  function onKeyDown(e: KeyboardEvent) {
    // No monitor (terminal mode): a key outside xterm must not reach the clone's desktop.
    if (forPage(e) || !size) return;
    o.onGesture();
    // Local chords, not forwarded. Their Ctrl and Alt already went to the clone, so release
    // them (and everything else held) first.
    if (e.ctrlKey && e.altKey && (e.code === "KeyG" || e.code === "KeyP" || e.code === "KeyS")) {
      e.preventDefault();
      if (e.repeat) return;
      releaseAll();
      if (e.code === "KeyG") {
        toggleLock();
      } else if (e.code === "KeyP") {
        auto.forceRelease();
        if (locked()) document.exitPointerLock();
      } else {
        o.onToggleStats();
      }
      return;
    }
    if (e.code === "F11") {
      e.preventDefault();
      toggleFullscreen();
      return;
    }
    const k = CODE_TO_EVDEV[e.code];
    if (k === undefined) return;
    e.preventDefault();
    // The clone runs its own key repeat, so the browser's repeats are not sent.
    if (e.repeat || !connected) return;
    // Held already: on macOS no keyup comes for a key released while Command is down, so a
    // second Cmd+C is a second press of a C the clone still holds. Release it first.
    if (keys.has(k)) send({ kind: "key_code", keycode: k, pressed: false });
    keys.add(k);
    send({ kind: "key_code", keycode: k, pressed: true });
  }

  function onKeyUp(e: KeyboardEvent) {
    if (forPage(e) || !size) return;
    const k = CODE_TO_EVDEV[e.code];
    if (k === undefined) return;
    e.preventDefault();
    if (!connected || !keys.delete(k)) return;
    send({ kind: "key_code", keycode: k, pressed: false });
    // Chrome on macOS fires no keyup for a key released while Command is down (Cmd+C sends
    // only C's keydown), so Command's own release ends every key pressed with it.
    if (isMac && (k === 125 || k === 126)) releaseAll();
  }

  function onPointerMove(e: PointerEvent) {
    // Chrome already sends at most one pointermove per frame, so moves need no other merging.
    if (!connected || now() < warpUntil) return;
    if (locked()) {
      if (e.movementX || e.movementY) {
        send({ kind: "pointer_relative", dx: e.movementX, dy: e.movementY });
      }
      return;
    }
    if (!size || !box) return;
    const p = toMonitor(e.clientX, e.clientY, canvas.getBoundingClientRect(), box, size.w, size.h);
    if (buttons.size) {
      // A drag: Chrome keeps sending the moves to the window the button was pressed in, also
      // past its edge, so a drag that leaves the window goes on in the neighbouring monitor.
      const r = routeDrag(layout, monitor, p.x, p.y);
      if (r) send({ kind: "pointer_move", monitor_id: r.id, x: r.x, y: r.y });
      return;
    }
    // `w` is the next monitor's first pixel (`routeDrag`), so the last one here is `w - 1`.
    send({ kind: "pointer_move", monitor_id: monitor, x: clampPixel(p.x, size.w), y: clampPixel(p.y, size.h) });
  }

  // Buttons use mouse events: a pointer event reports a second button pressed during a first
  // one as a move, not as a press.
  function onMouseDown(e: MouseEvent) {
    e.preventDefault(); // no middle-button autoscroll, no text selection
    o.onGesture();
    const b = BUTTONS[e.button];
    // A click is the gesture Chrome wants after the user left the lock with Esc.
    if (needGesture) {
      needGesture = false;
      if (size && auto.want(now())) requestLock();
    }
    if (b === undefined || !connected || buttons.has(b)) return;
    buttons.add(b);
    send({ kind: "button", button: b, pressed: true });
  }

  function onMouseUp(e: MouseEvent) {
    e.preventDefault(); // the back and forward buttons do not navigate
    const b = BUTTONS[e.button];
    if (b === undefined || !connected || !buttons.delete(b)) return;
    send({ kind: "button", button: b, pressed: false });
  }

  function onWheel(e: WheelEvent) {
    e.preventDefault(); // no page scroll, no Ctrl+wheel zoom
    if (!connected) return;
    const w = e as WheelEvent & { wheelDeltaX: number; wheelDeltaY: number };
    const msgs = wheelMessages(w, rem, scrollEnd !== undefined);
    for (const m of msgs) send(m);
    if (msgs[0]?.kind === "axis_continuous") {
      // Fingers lifted (no more events): finish the gesture so the clone's kinetic scroll ends.
      clearTimeout(scrollEnd);
      scrollEnd = setTimeout(() => {
        scrollEnd = undefined;
        send({ kind: "axis_continuous", dx: 0, dy: 0, flags: AXIS_FINISH | AXIS_SOURCE_FINGER });
      }, SCROLL_END_MS);
    }
  }

  const noMenu = (e: Event) => e.preventDefault();
  const onBlur = () => releaseAll();
  const onVisibility = () => {
    if (document.visibilityState === "hidden") releaseAll();
  };
  const onLockChange = () => {
    // The lock ended while the page has focus and the policy still wants it: the user pressed
    // Esc. Treat it as Ctrl+Alt+P, or the next click would lock again.
    if (!locked() && document.hasFocus() && auto.want(now())) auto.forceRelease();
  };
  // Keyboard Lock only works in fullscreen, so take it on entry and give it back on exit.
  const onFullscreen = () => {
    if (document.fullscreenElement) void keyboard()?.lock();
    else keyboard()?.unlock();
  };

  function toggleLock() {
    needGesture = false;
    if (auto.toggle(now())) requestLock();
    else if (locked()) document.exitPointerLock();
  }

  window.addEventListener("keydown", onKeyDown);
  window.addEventListener("keyup", onKeyUp);
  window.addEventListener("pointermove", onPointerMove);
  window.addEventListener("mouseup", onMouseUp);
  window.addEventListener("blur", onBlur);
  canvas.addEventListener("mousedown", onMouseDown);
  canvas.addEventListener("wheel", onWheel, { passive: false });
  canvas.addEventListener("contextmenu", noMenu);
  document.addEventListener("visibilitychange", onVisibility);
  document.addEventListener("pointerlockchange", onLockChange);
  document.addEventListener("fullscreenchange", onFullscreen);
  const timer = setInterval(tick, 50);
  const resize = new ResizeObserver(([entry]) => {
    const d = entry.devicePixelContentBoxSize[0];
    box = { w: d.inlineSize, h: d.blockSize };
    built = null;
    o.onResize(box.w, box.h);
  });
  resize.observe(canvas, { box: "device-pixel-content-box" });

  return {
    setLayout(monitors) {
      layout = monitors;
      const m = monitors.find((v) => v.id === monitor);
      size = m ? { w: m.width, h: m.height } : null;
    },
    onCursor(c) {
      if (c.monitor_id !== monitor) return;
      const t = now();
      if (c.hidden) {
        hidden = true;
        auto.onRemoteCursor(true, t);
      } else if (c.shape) {
        hidden = false;
        auto.onRemoteCursor(false, t);
        shape = c.shape;
        built = null;
      }
      if (c.warp) {
        warpUntil = t + WARP_SUPPRESS_MS;
        overlayUntil = t + AGENT_CURSOR_SHOW_MS;
      }
      cursor = { x: c.x, y: c.y };
      tick();
    },
    setConnected(c) {
      connected = c;
      // The server releases what a socket held when it closes; start the next one clean.
      if (!c) {
        keys.clear();
        buttons.clear();
      }
    },
    detach() {
      clearInterval(timer);
      resize.disconnect();
      clearTimeout(scrollEnd);
      window.removeEventListener("keydown", onKeyDown);
      window.removeEventListener("keyup", onKeyUp);
      window.removeEventListener("pointermove", onPointerMove);
      window.removeEventListener("mouseup", onMouseUp);
      window.removeEventListener("blur", onBlur);
      canvas.removeEventListener("mousedown", onMouseDown);
      canvas.removeEventListener("wheel", onWheel);
      canvas.removeEventListener("contextmenu", noMenu);
      document.removeEventListener("visibilitychange", onVisibility);
      document.removeEventListener("pointerlockchange", onLockChange);
      document.removeEventListener("fullscreenchange", onFullscreen);
      if (locked()) document.exitPointerLock();
    },
  };
}
