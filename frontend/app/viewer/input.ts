// The browser viewer's input and cursor, on the page thread. Events come from the page's
// placeholder <canvas> (it stays in the DOM after `transferControlToOffscreen`, so pointer
// events and pointer lock work on it) and keys from `window`. Each event becomes an `InputMsg`
// that the page hands to the worker, which sends it on the socket.
//
// What it sends: Linux evdev keycodes, evdev buttons, wheel notches
// or smooth finger scroll, pointer lock with relative motion that engages by itself while the
// clone hides its cursor, the Ctrl+Alt+G / Ctrl+Alt+P chords, and the agent-warp rules.
import type { ScrollMode } from "../lib/wire/ScrollMode";
import type { CursorMeta } from "../lib/wire/CursorMeta";
import type { CursorShape } from "../lib/wire/CursorShape";
import type { InputMsg } from "../lib/wire/InputMsg";
import type { ViewMonitor } from "../lib/wire/ViewMonitor";
import { hostWindow } from "../lib/host";
import { evdevFor } from "./keycodes";

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
/** No smooth-scroll event for this long ends the scroll gesture (the browser has no end event).
 *  Past the 150 ms GTK 4 takes a fling's speed over, so the clone adds no fling of its own to
 *  the momentum the client already scrolled. */
const SCROLL_END_MS = 200;
/** Pixels Chrome reports for a line a macOS wheel turned (`isMacDetent`). */
const MAC_WHEEL_LINE_PX = 40;
/** Input made while the socket is down is sent when it is back, unless it is older than this
 *  (a release is always sent). */
const HOLD_MAX_MS = 2000;
/** A paste chord's key goes to the clone at most this long after its key press, also when no
 *  paste event comes (`InputOptions.pasteByEvent`). */
const PASTE_WAIT_MS = 100;

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

type WheelLike = {
  deltaMode: number;
  deltaX: number;
  deltaY: number;
  wheelDeltaX: number;
  wheelDeltaY: number;
  shiftKey?: boolean;
};

/** Whether a wheel event is one detent of a macOS mouse wheel (the rule Selkies uses). macOS
 *  reports a wheel's turn in lines as 16.16 fixed point, scaled by how fast it spins, and Chrome
 *  hands that over at 40 px a line: a detent is 4.0002 px, or 380.02 in a spin, never whole. A
 *  trackpad scrolls in whole pixels. Under Shift macOS turns the wheel sideways and rounds it to
 *  whole lines, a sideways multiple of 40 with nothing vertical. */
function isMacDetent(e: WheelLike): boolean {
  if (e.deltaMode !== 0) return e.deltaMode === 1;
  if (!Number.isInteger(e.deltaX) || !Number.isInteger(e.deltaY)) return true;
  return !!e.shiftKey && e.deltaY === 0 && e.deltaX !== 0 && e.deltaX % MAC_WHEEL_LINE_PX === 0;
}

const isWhole = (v: number) => Math.abs(v - Math.round(v)) < 1e-3;

/** Whether a wheel event comes from a mouse wheel on Linux. Chrome turns a wheel into a mouse
 *  wheel event whose offset is whole by type: 120 a notch, or a high-resolution wheel's part
 *  of 120 (15, 30). A touchpad is a scroll event in fractional pixels. The page sees the offset
 *  divided by the device pixel ratio, so a whole number there, too, is a wheel. */
function isLinuxWheel(e: WheelLike, dpr: number): boolean {
  if (e.deltaMode !== 0) return true;
  const whole = (v: number) => isWhole(v) || isWhole(v * dpr);
  return whole(e.deltaX) && whole(e.deltaY);
}

/** How one window reads its wheel events (`wheelMessages`). */
export interface WheelOptions {
  /** A finger scroll is still running, so this event belongs to it even if it looks like a
   *  wheel's. */
  fingers?: boolean;
  os?: "mac" | "linux" | "other";
  dpr?: number;
  /** The active layout's choice: guess per event, or take every event as one device. */
  mode?: ScrollMode;
  /** Scroll distance factor, from the layout's speed: 1 sends what the device reported. */
  scale?: number;
}

/** One wheel event → input messages. A mouse wheel becomes `axis` notches: on a Mac one per
 *  detent (`isMacDetent`), whatever distance macOS's acceleration gave it; on Linux and
 *  elsewhere its `wheelDelta` over 120 (a high-resolution wheel reports parts of a notch), with
 *  the fraction carried in `rem`. Anything else (a trackpad) becomes smooth finger scroll in
 *  CSS pixels. The layout's mode can take every event as one or the other, and its speed scales
 *  both. Browser and Mutter agree on the direction: positive is down and right. */
export function wheelMessages(e: WheelLike, rem: { x: number; y: number }, o: WheelOptions = {}): InputMsg[] {
  const { fingers = false, os = "other", dpr = 1, mode = "auto", scale = 1 } = o;
  const detent = os === "mac" && isMacDetent(e);
  const looksLikeWheel =
    os === "linux"
      ? isLinuxWheel(e, dpr)
      : (e.wheelDeltaX !== 0 || e.wheelDeltaY !== 0) &&
        e.wheelDeltaX % 120 === 0 &&
        e.wheelDeltaY % 120 === 0;
  const wheel =
    mode === "mouse" ||
    (mode === "auto" && (detent || e.deltaMode !== 0 || (!fingers && looksLikeWheel)));
  if (!wheel) {
    if (!e.deltaX && !e.deltaY) return [];
    const flags = AXIS_SOURCE_FINGER;
    return [{ kind: "axis_continuous", dx: e.deltaX * scale, dy: e.deltaY * scale, flags }];
  }
  const nx = (detent ? Math.sign(e.deltaX) : -e.wheelDeltaX / 120) * scale;
  const ny = (detent ? Math.sign(e.deltaY) : -e.wheelDeltaY / 120) * scale;
  // A turn the other way starts from zero, so a carried fraction cannot swallow its first notch.
  if (nx * rem.x < 0) rem.x = 0;
  if (ny * rem.y < 0) rem.y = 0;
  rem.x += nx;
  rem.y += ny;
  // The epsilon keeps a sum of parts (30 × 4/120) from landing just under a whole step.
  const sx = Math.trunc(rem.x + Math.sign(rem.x) * 1e-9);
  const sy = Math.trunc(rem.y + Math.sign(rem.y) * 1e-9);
  rem.x -= sx;
  rem.y -= sy;
  const out: InputMsg[] = [];
  if (sy) out.push({ kind: "axis", axis: 0, step: sy });
  if (sx) out.push({ kind: "axis", axis: 1, step: sx });
  return out;
}

/** Input made while the socket is down, oldest first, with when it was made. */
export type HeldInput = { msg: InputMsg; at: number }[];

const isRelease = (m: InputMsg) =>
  (m.kind === "key_code" || m.kind === "key" || m.kind === "button") && !m.pressed;

/** The held input that is still worth sending at `now`: what is at most HOLD_MAX_MS old, and
 *  every release. */
export function freshInput(held: HeldInput, now: number): HeldInput {
  return held.filter((h) => now - h.at <= HOLD_MAX_MS || isRelease(h.msg));
}

/** Hold `msg` until the socket is back. A pointer move right after another replaces it (the
 *  newest wins) and relative moves add up, so a drag keeps its shape. */
export function holdInput(held: HeldInput, msg: InputMsg, now: number): HeldInput {
  const out = freshInput(held, now);
  const last = out.at(-1)?.msg;
  if (msg.kind === "pointer_move" && last?.kind === "pointer_move") {
    out.pop();
  } else if (msg.kind === "pointer_relative" && last?.kind === "pointer_relative") {
    out.pop();
    msg = { ...msg, dx: last.dx + msg.dx, dy: last.dy + msg.dy };
  }
  out.push({ msg, at: now });
  return out;
}

/** Chrome's Keyboard Lock API, the browser window's (`~/lib/host.ts`); not in TypeScript's DOM
 *  types. */
const keyboard = () =>
  (hostWindow().navigator as Navigator & { keyboard?: { lock(): Promise<void>; unlock(): void } }).keyboard;

/** The document whose fullscreen this page uses: the /ng shell's around it, or its own. */
const fullscreenDoc = () => hostWindow().document;

/** Fullscreen on or off, on the screen the window is on. The page's own fullscreen hides
 *  the browser's address bar, which macOS fullscreen (the green button) keeps. A browser
 *  without Keyboard Lock is asked for `keyboardLock: "browser"` (Safari 26.4, Firefox 156): for
 *  as long as the fullscreen lasts, Esc and the browser's own shortcuts go to the clone, and a
 *  held Esc leaves, as with Chrome's Keyboard Lock. A browser that does not know the option
 *  ignores it. In the /ng shell's frame, the shell's page goes fullscreen, so a reload of the
 *  frame keeps it. */
export function toggleFullscreen() {
  const document = fullscreenDoc();
  if (document.fullscreenElement) void document.exitFullscreen();
  else {
    const options = keyboard() ? undefined : { keyboardLock: "browser" };
    // Locked here too, not only when an input sees the change: a page that takes no keys
    // now (`/ng` with its control panel open) still gets them once it does
    void document.documentElement
      .requestFullscreen(options as FullscreenOptions | undefined)
      .then(() => keyboard()?.lock())
      .catch(() => {});
  }
}

/** The keys Caps Lock changes: before one goes to the clone, its Caps Lock is brought in step. */
const LETTER = /^Key[A-Z]$/;

interface InputOptions {
  /** The placeholder canvas: the event target, the CSS cursor host and the pointer lock target. */
  canvas: HTMLCanvasElement;
  /** The drawn cursor (fixed position, no pointer events). */
  overlay: HTMLImageElement;
  monitor: number;
  /** Send one event. While the socket is down the worker holds it (`holdInput`). */
  send(msg: InputMsg): void;
  /** Send this keyboard's Caps Lock (on or off), for the clone's to follow: the clone-daemon
   *  reads the clone's own and taps it when the two differ (`wire::socket::LockState`). */
  sendLocks(caps: boolean): void;
  /** A key press or click: the user activation the browser wants for some calls (a clipboard
   *  write it refused before). */
  onGesture(): void;
  /** Whether the local clipboard reaches the page only in the paste event of a paste chord
   *  (Safari, Firefox: `./clipboard.ts`). Then the chord's key keeps its default action, which
   *  fires that event, and goes to the clone after it, behind the offer the event makes. */
  pasteByEvent(): boolean;
  /** The canvas's size in device pixels changed: the worker draws at this size. */
  onResize(width: number, height: number): void;
  /** A chord of the page's own (`~/ng/shortcut.ts`), which does not go to the clone. The press
   *  of its modifier (`holds`: Super, or Alt on a Mac) is held back until the next key or click:
   *  a Super or an Alt pressed and let go alone does something in the clone (GNOME's overview,
   *  an app's menu bar), so the chord's must never reach it. */
  chord?: { is(e: KeyboardEvent): boolean; holds: readonly string[]; run(): void };
}

export interface Input {
  /** The monitors of the view spec. No pointer moves are sent while this window's monitor is
   *  not among them. */
  setLayout(monitors: ViewMonitor[]): void;
  onCursor(c: CursorMeta): void;
  /** The active layout's scroll mode and speed (percent). */
  setScroll(mode: ScrollMode, speed: number): void;
  detach(): void;
}

/** The event flag that says whether a modifier key (by its DOM code) is down. */
export type ModFlag = "ctrlKey" | "shiftKey" | "altKey" | "metaKey";

export function modFlagOf(code: string): ModFlag | undefined {
  if (code === "ControlLeft" || code === "ControlRight") return "ctrlKey";
  if (code === "ShiftLeft" || code === "ShiftRight") return "shiftKey";
  if (code === "AltLeft" || code === "AltRight") return "altKey";
  if (code === "MetaLeft" || code === "MetaRight") return "metaKey";
  return undefined;
}

/** The held modifiers (evdev code → its flag) that `e` says are up: their release never came.
 *  macOS takes Ctrl+Space for itself (the next input source) and Chrome gets no keyup for the
 *  Ctrl, so the clone would hold Ctrl until the page reloads. Every key, button, wheel and move
 *  event says which modifiers are down, so the next one lets go of the rest. */
export function staleMods(held: ReadonlyMap<number, ModFlag>, e: Pick<KeyboardEvent, ModFlag>): number[] {
  return [...held].filter(([, flag]) => !e[flag]).map(([k]) => k);
}

export function attachInput(o: InputOptions): Input {
  const { canvas, overlay, monitor, send } = o;
  const isMac = /Mac/.test(navigator.platform);
  const os = isMac ? "mac" : /Linux/.test(navigator.platform) ? "linux" : "other";
  let layout: ViewMonitor[] = [];
  /** This window's monitor in the layout. */
  let size: { w: number; h: number } | null = null;
  /** The canvas in device pixels. */
  let box: { w: number; h: number } | null = null;
  const keys = new Set<number>();
  /** The modifiers among `keys`, with the flag an event says them down by (`staleMods`). */
  const mods = new Map<number, ModFlag>();
  const buttons = new Set<number>();
  const auto = new AutoLock(performance.now());
  /** Chrome refused the lock (the user left it with Esc): wait for a click before asking again. */
  let needGesture = false;
  let lockPending = false;
  let warpUntil = 0;
  let overlayUntil = 0;
  const rem = { x: 0, y: 0 };
  let scrollMode: ScrollMode = "auto";
  let scrollScale = 1;
  let scrollEnd: ReturnType<typeof setTimeout> | undefined;
  /** The chord's modifier's press, held back while the chord may follow (`InputOptions.chord`). */
  let heldMod: number | null = null;
  /** A paste chord's key press, held until the paste event it fires has made its offer. */
  let heldPaste: { keycode: number; timer: ReturnType<typeof setTimeout> } | null = null;
  /** Where a paste chord's paste goes. Safari pastes only into something editable, and fires
   *  no paste event otherwise, so the chord's key press moves the focus here, and the paste
   *  moves it back. Only for the chord: an editable focus would start an input method (IME)
   *  on every other key. */
  const pasteSink = document.createElement("div");
  pasteSink.contentEditable = "true";
  pasteSink.tabIndex = -1;
  pasteSink.ariaHidden = "true";
  pasteSink.style.cssText = "position:fixed;left:-100px;top:0;width:1px;height:1px;opacity:0";
  document.body.append(pasteSink);
  /** The cursor's `image-set` function: older Safari knows only the prefixed one. */
  const imageSet = CSS.supports("cursor", `image-set(url("data:,") 1x) 0 0, default`)
    ? "image-set"
    : "-webkit-image-set";

  // The remote cursor: last position (monitor pixels), shape, and what was built from it.
  let cursor = { x: 0, y: 0 };
  let shape: CursorShape | null = null;
  let hidden = false;
  /** The sprite built for the current letterbox scale; `tooBig` means the overlay draws it. */
  let built: { key: string; tooBig: boolean } | null = null;

  const locked = () => document.pointerLockElement === canvas;
  const now = () => performance.now();

  /** Release every key and button the clone holds from this window. */
  function releaseAll() {
    // A held paste key was never pressed in the clone.
    if (heldPaste) clearTimeout(heldPaste.timer);
    heldPaste = null;
    heldMod = null;
    pasteSink.blur();
    for (const k of keys) send({ kind: "key_code", keycode: k, pressed: false });
    for (const b of buttons) send({ kind: "button", button: b, pressed: false });
    keys.clear();
    mods.clear();
    buttons.clear();
  }

  function requestLock() {
    if (lockPending) return;
    lockPending = true;
    // Chrome answers with a promise. Safari returns nothing and tells of a refusal with a
    // `pointerlockerror` event (`onLockError`).
    const ask = (options?: PointerLockOptions) =>
      new Promise<void>((resolve) => resolve(canvas.requestPointerLock(options)));
    ask({ unadjustedMovement: true })
      .catch((e: DOMException) =>
        // Raw motion is not offered on every OS: take the plain lock then.
        e.name === "NotSupportedError" ? ask() : Promise.reject(e),
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
      : `${imageSet}(url("${url}") ${dpr}x) ${Math.round(s.hotspot_x * scale)} ${Math.round(s.hotspot_y * scale)}, default`;
    return { key, tooBig };
  }

  /** A key typed into the page's own terminal (xterm.js reads keys from a textarea). */
  const forPage = (e: KeyboardEvent) => e.target instanceof HTMLTextAreaElement;

  /** Let go of the held modifiers `e` says are up (`staleMods`). */
  function syncMods(e: KeyboardEvent | MouseEvent) {
    // AltGr (right Alt) can come as "AltGraph" with `altKey` off: it is down all the same
    const down = { ctrlKey: e.ctrlKey, shiftKey: e.shiftKey, metaKey: e.metaKey, altKey: e.altKey || e.getModifierState("AltGraph") };
    for (const k of staleMods(mods, down)) {
      mods.delete(k);
      if (keys.delete(k)) send({ kind: "key_code", keycode: k, pressed: false });
    }
  }

  /** Send the modifier's held press: another key or a click came, so it is no chord's. */
  function flushMod() {
    if (heldMod === null) return;
    keys.add(heldMod);
    send({ kind: "key_code", keycode: heldMod, pressed: true });
    heldMod = null;
  }

  function onKeyDown(e: KeyboardEvent) {
    if (forPage(e)) return;
    syncMods(e);
    if (o.chord?.is(e)) {
      e.preventDefault();
      if (e.repeat) return;
      // Its keys never reach the clone; what else is held is let go there
      releaseAll();
      o.chord.run();
      return;
    }
    // No monitor (terminal mode): a key outside xterm must not reach the clone's desktop.
    if (!size) return;
    o.onGesture();
    // Local chords, not forwarded. Their Ctrl and Alt already went to the clone, so release
    // them (and everything else held) first.
    if (e.ctrlKey && e.altKey && (e.code === "KeyG" || e.code === "KeyP")) {
      e.preventDefault();
      if (e.repeat) return;
      releaseAll();
      if (e.code === "KeyG") {
        toggleLock();
      } else {
        auto.forceRelease();
        if (locked()) document.exitPointerLock();
      }
      return;
    }
    if (e.code === "F11") {
      e.preventDefault();
      toggleFullscreen();
      return;
    }
    if (e.code === "CapsLock") return capsSync(e);
    const k = evdevFor(e.code, isMac);
    if (k === undefined) return;
    // A letter: the clone's Caps Lock is this keyboard's before it lands
    if (!e.repeat && LETTER.test(e.code)) o.sendLocks(e.getModifierState("CapsLock"));
    const paste =
      e.code === "KeyV" && (isMac ? e.metaKey : e.ctrlKey) && !e.altKey && o.pasteByEvent();
    if (!paste) e.preventDefault();
    // The clone runs its own key repeat, so the browser's repeats are not sent.
    if (e.repeat) return;
    if (o.chord?.holds.includes(e.code) && !keys.has(k)) {
      heldMod = k;
      return;
    }
    flushMod();
    // Held already: on macOS no keyup comes for a key released while Command is down, so a
    // second Cmd+C is a second press of a C the clone still holds. Release it first.
    if (keys.has(k)) send({ kind: "key_code", keycode: k, pressed: false });
    keys.add(k);
    const flag = modFlagOf(e.code);
    if (flag) mods.set(k, flag);
    if (paste) {
      pasteSink.focus({ preventScroll: true });
      holdPaste(k);
    } else send({ kind: "key_code", keycode: k, pressed: true });
  }

  function holdPaste(keycode: number) {
    flushPaste();
    heldPaste = { keycode, timer: setTimeout(flushPaste, PASTE_WAIT_MS) };
  }

  /** Send the held paste key's press. */
  function flushPaste() {
    if (!heldPaste) return;
    pasteSink.blur();
    clearTimeout(heldPaste.timer);
    send({ kind: "key_code", keycode: heldPaste.keycode, pressed: true });
    heldPaste = null;
  }

  // One task later, so every paste listener has run: the clipboard's sends the offer.
  const onPaste = () => {
    if (heldPaste) setTimeout(flushPaste, 0);
  };

  function onKeyUp(e: KeyboardEvent) {
    if (forPage(e) || !size) return;
    if (e.code === "CapsLock") return capsSync(e);
    const k = evdevFor(e.code, isMac);
    if (k === undefined) return;
    e.preventDefault();
    if (heldMod === k) {
      // The modifier alone: its tap goes to the clone now
      heldMod = null;
      send({ kind: "key_code", keycode: k, pressed: true });
      send({ kind: "key_code", keycode: k, pressed: false });
      return;
    }
    if (heldPaste?.keycode === k) flushPaste();
    mods.delete(k);
    if (!keys.delete(k)) return;
    send({ kind: "key_code", keycode: k, pressed: false });
    // Chrome on macOS fires no keyup for a key released while Command is down (Cmd+C sends
    // only C's keydown), so Command's own release ends every key pressed with it.
    if (isMac && (e.code === "MetaLeft" || e.code === "MetaRight")) releaseAll();
  }

  /** Caps Lock is not sent as a key: a press only flips the clone's lock, and once the two
   *  disagree every press keeps them apart. Its state is sent instead, on its down and on its
   *  up (one of them may still say the old state: X11 reports the new one only on the up, and
   *  Chrome on macOS sends a down when the lock goes on and an up when it goes off), and the
   *  clone's follows it. */
  function capsSync(e: KeyboardEvent) {
    e.preventDefault();
    o.sendLocks(e.getModifierState("CapsLock"));
  }

  function onPointerMove(e: PointerEvent) {
    syncMods(e);
    // Chrome already sends at most one pointermove per frame, so moves need no other merging.
    if (now() < warpUntil) return;
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
    syncMods(e);
    o.onGesture();
    flushMod();
    const b = BUTTONS[e.button];
    // A click is the gesture Chrome wants after the user left the lock with Esc.
    if (needGesture) {
      needGesture = false;
      if (size && auto.want(now())) requestLock();
    }
    if (b === undefined || buttons.has(b)) return;
    buttons.add(b);
    send({ kind: "button", button: b, pressed: true });
  }

  function onMouseUp(e: MouseEvent) {
    e.preventDefault(); // the back and forward buttons do not navigate
    const b = BUTTONS[e.button];
    if (b === undefined || !buttons.delete(b)) return;
    send({ kind: "button", button: b, pressed: false });
  }

  function onWheel(e: WheelEvent) {
    e.preventDefault(); // no page scroll, no Ctrl+wheel zoom
    syncMods(e);
    flushMod();
    const w = e as WheelEvent & { wheelDeltaX: number; wheelDeltaY: number };
    const msgs = wheelMessages(w, rem, {
      fingers: scrollEnd !== undefined,
      os,
      dpr: devicePixelRatio,
      mode: scrollMode,
      scale: scrollScale,
    });
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
  const onLockError = () => {
    needGesture = true;
  };
  const onLockChange = () => {
    // The lock ended while the page has focus and the policy still wants it: the user pressed
    // Esc. Treat it as Ctrl+Alt+P, or the next click would lock again.
    if (!locked() && document.hasFocus() && auto.want(now())) auto.forceRelease();
  };
  // Keyboard Lock only works in fullscreen, so take it on entry and give it back on exit.
  const onFullscreen = () => {
    if (fullscreenDoc().fullscreenElement) void keyboard()?.lock();
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
  document.addEventListener("pointerlockerror", onLockError);
  fullscreenDoc().addEventListener("fullscreenchange", onFullscreen);
  window.addEventListener("paste", onPaste);
  // Attached while the page is fullscreen already: the keys are locked from now
  if (fullscreenDoc().fullscreenElement) void keyboard()?.lock();
  const timer = setInterval(tick, 50);
  const resize = new ResizeObserver(([entry]) => {
    // Safari has no device-pixel box: there it is the CSS box at the device pixel ratio.
    const d = entry.devicePixelContentBoxSize?.[0];
    const c = entry.contentBoxSize[0];
    box = d
      ? { w: d.inlineSize, h: d.blockSize }
      : {
          w: Math.round(c.inlineSize * devicePixelRatio),
          h: Math.round(c.blockSize * devicePixelRatio),
        };
    built = null;
    o.onResize(box.w, box.h);
  });
  /** Without the device-pixel box, a new device pixel ratio (the window moved to a screen of
   *  another density) changes no CSS size: observe again then, which reports the size again. */
  let ratio: MediaQueryList | null = null;
  const onRatio = () => {
    resize.unobserve(canvas);
    resize.observe(canvas);
    watchRatio();
  };
  function watchRatio() {
    ratio?.removeEventListener("change", onRatio);
    ratio = matchMedia(`(resolution: ${devicePixelRatio}dppx)`);
    ratio.addEventListener("change", onRatio);
  }
  if ("devicePixelContentBoxSize" in ResizeObserverEntry.prototype) {
    resize.observe(canvas, { box: "device-pixel-content-box" });
  } else {
    resize.observe(canvas);
    watchRatio();
  }

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
    setScroll(mode, speed) {
      scrollMode = mode;
      scrollScale = speed / 100;
    },
    detach() {
      clearInterval(timer);
      resize.disconnect();
      clearTimeout(scrollEnd);
      if (heldPaste) clearTimeout(heldPaste.timer);
      pasteSink.remove();
      ratio?.removeEventListener("change", onRatio);
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
      document.removeEventListener("pointerlockerror", onLockError);
      fullscreenDoc().removeEventListener("fullscreenchange", onFullscreen);
      window.removeEventListener("paste", onPaste);
      if (locked()) document.exitPointerLock();
    },
  };
}
