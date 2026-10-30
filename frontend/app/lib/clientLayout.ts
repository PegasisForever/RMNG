// This browser's own monitor layout, and its claims to the server.
//
// Each browser keeps one layout of its own, in its storage (`rmng.layout`), which all its
// windows share: the dashboard, the viewer windows and the `/ng` windows. A browser that can
// open windows of its own (a desktop: `canOpenWindows`) has up to MAX_MONITORS monitors, one
// window each; a phone or a tablet has one, the `/ng` page. The server keeps no list: it has
// only the layout of the client used last (`POST /api/layout/claim`, `ControlState.layout`).
//
// A client claims on each real action of the operator in it: a touch, a click, a key, a wheel,
// the mouse moving, and the window being opened or coming to the front. Only while the window
// is in the foreground: a phone browser woken in the background (Android does that when the
// charger goes in) must not take the clones from the desktop. A client never claims because
// it sees another client's layout, so two clients never take turns by themselves.
import { claimLayout } from "~/lib/api";
import type { ControlState } from "~/lib/types";
import type { MonitorSpec } from "~/lib/wire/MonitorSpec";
import type { ScrollMode } from "~/lib/wire/ScrollMode";

import { MAX_MONITORS, normalize } from "./monitorLayout";

export type ClientKind = "desktop" | "phone";

/** Whether this browser can open windows of its own, one per monitor (and the pages, out of
 *  `/ng`). A phone or a tablet opens a tab in their place, or nothing. */
export function canOpenWindows(nav: Pick<Navigator, "userAgent" | "platform" | "maxTouchPoints"> = navigator): boolean {
  const data = (nav as { userAgentData?: { mobile?: boolean } }).userAgentData;
  if (data?.mobile) return false;
  if (/Android|iPhone|iPad|iPod|Mobile/.test(nav.userAgent)) return false;
  // An iPad says it is a Mac, but a Mac has no touch screen
  return !(nav.platform === "MacIntel" && nav.maxTouchPoints > 1);
}

/** What this browser counts as: a desktop when it can open windows, else a phone. */
export function clientKind(): ClientKind {
  return canOpenWindows() ? "desktop" : "phone";
}

/** What a client claims, besides who it is. */
export type LayoutBody = { monitors: MonitorSpec[]; scrollMode: ScrollMode; scrollSpeed: number };

/** This browser's layout: what it claims, and for each monitor (`scales[i]` for monitor i) how
 *  many screen pixels one clone pixel takes in its window (this browser's alone, not
 *  claimed). */
export type BrowserLayout = LayoutBody & { scales: number[] };

/** The pixel scales the dialog offers. */
export const PIXEL_SCALES = [1, 1.25, 1.5, 2, 2.5, 3] as const;

/** The desktop layout before this browser has one: the server's default (`wire::default_monitors`):
 *  two 2560×1440 side by side, the primary one on the right. */
export const DEFAULT_DESKTOP_LAYOUT: BrowserLayout = {
  monitors: [
    { width: 2560, height: 1440, x: 2560, y: 0, primary: true },
    { width: 2560, height: 1440, x: 0, y: 0, primary: false },
  ],
  scrollMode: "auto",
  scrollSpeed: 100,
  scales: [1, 1],
};

const isScale = (v: unknown): v is number => (PIXEL_SCALES as readonly unknown[]).includes(v);

const DESKTOP_KEY = "rmng.layout";

function read(key: string): string | null {
  try {
    return localStorage.getItem(key);
  } catch {
    return null;
  }
}

function write(key: string, value: string) {
  try {
    localStorage.setItem(key, value);
  } catch {
    // no storage (a private window): it lasts until the page closes
  }
}

/** This browser's id as a `kind` client, made once and kept. */
export function clientId(kind: ClientKind): string {
  const key = `rmng.client.${kind}`;
  let id = read(key);
  if (!id) {
    id = typeof crypto.randomUUID === "function" ? crypto.randomUUID() : `${Date.now()}-${Math.random()}`;
    write(key, id);
  }
  return id;
}

/** The layout this browser keeps; null before it has one. */
export function readLayout(): BrowserLayout | null {
  const raw = read(DESKTOP_KEY);
  if (!raw) return null;
  try {
    // `pixelScale`: one scale for every monitor, as a build of 2026-10-29 kept it
    const l = JSON.parse(raw) as Partial<BrowserLayout> & { pixelScale?: number };
    if (!Array.isArray(l.monitors) || !l.monitors.length) return null;
    const monitors = normalize(l.monitors.slice(0, MAX_MONITORS));
    const fallback = isScale(l.pixelScale) ? l.pixelScale : 1;
    return {
      monitors,
      scrollMode: l.scrollMode ?? "auto",
      scrollSpeed: Number.isFinite(l.scrollSpeed) ? l.scrollSpeed! : 100,
      scales: monitors.map((_, i) => (isScale(l.scales?.[i]) ? l.scales![i] : fallback)),
    };
  } catch {
    return null;
  }
}

export function saveLayout(layout: BrowserLayout) {
  write(DESKTOP_KEY, JSON.stringify({ ...layout, monitors: normalize(layout.monitors) }));
}

/** This browser's layout, made the first time from `seed` (the default: DEFAULT_DESKTOP_LAYOUT).
 *  A desktop starts from the layout in force instead when that is a desktop's (a browser that
 *  comes later starts where the last one left the clones; a server of an older build hands its
 *  active layout preset over that way). A phone keeps one monitor. */
export async function ensureLayout(
  kind: ClientKind = "desktop",
  seed: () => BrowserLayout = () => DEFAULT_DESKTOP_LAYOUT,
): Promise<BrowserLayout> {
  const have = readLayout();
  if (have) {
    if (kind === "desktop" || have.monitors.length === 1) return have;
    const one = { ...have, monitors: have.monitors.slice(0, 1), scales: have.scales.slice(0, 1) };
    saveLayout(one);
    return one;
  }
  let first = seed();
  if (kind === "desktop") {
    try {
      const state = (await (await fetch("/api/state")).json()) as ControlState;
      const l = state.layout;
      if (l?.label === "desktop" && l.monitors.length) {
        first = { monitors: l.monitors, scrollMode: l.scrollMode, scrollSpeed: l.scrollSpeed, scales: l.monitors.map(() => 1) };
      }
    } catch {
      // the seed, then
    }
  }
  // Another window of this browser may have made it meanwhile
  if (!readLayout()) saveLayout(first);
  return readLayout() ?? first;
}

/** What this browser claims: its stored layout, without the pixel scales; null before it has
 *  one (`ensureLayout`). */
export function layoutClaim(): LayoutBody | null {
  const l = readLayout();
  return l && { monitors: l.monitors, scrollMode: l.scrollMode, scrollSpeed: l.scrollSpeed };
}

/** The channel the Edit layout dialog talks to the monitor windows of this browser on (the
 *  viewer windows and the `/ng` windows). */
export const VIEWER_CHANNEL = "rmng-viewer";
/** How often the open Edit layout dialog says so on VIEWER_CHANNEL, ms. */
export const IDENTIFY_EVERY_MS = 1000;

/** On VIEWER_CHANNEL: the dashboard asks, and each viewer window answers with its monitor and
 *  its content's size in screen (device) pixels; the monitors were renumbered (`map[old]` is
 *  the new number; null: removed), so each window follows its monitor, or closes; and the Edit
 *  layout dialog is open (`identify`, said every IDENTIFY_EVERY_MS while it is), so each window
 *  shows its monitor's number, until `identify-off` or until the word stops coming. */
export type ViewerChannelMsg =
  | { t: "ask-size" }
  | { t: "identify" }
  | { t: "identify-off" }
  | { t: "size"; monitor: number; width: number; height: number }
  | { t: "renumber"; map: (number | null)[] };

/** A client's claims are at most this far apart, unless forced: the server changes nothing for
 *  a layout it has already, so more would be noise. */
const CLAIM_EVERY_MS = 2000;

/** The operator's actions that claim. */
const ACTIONS = ["pointerdown", "pointermove", "keydown", "wheel", "touchstart"] as const;

/** The window is the one the operator looks at. */
const shown = () => document.visibilityState === "visible";

/**
 * Claim the layout for this client on each action of the operator in this window, while it is
 * in the foreground (see the top of this file). `layout` says what to claim, asked at each
 * claim; null claims nothing. `claim(true)` claims now, as after the layout changed.
 */
export function attachLayoutClaims(
  kind: ClientKind,
  layout: () => LayoutBody | null,
): { claim: (force?: boolean) => void; detach: () => void } {
  const client = clientId(kind);
  let last = -Infinity;
  const claim = (force = false) => {
    const t = performance.now();
    if (!force && t - last < CLAIM_EVERY_MS) return;
    const body = layout();
    if (!body) return;
    last = t;
    claimLayout({ client, label: kind, ...body }).catch(() => {});
  };
  // An action counts while the window shows; the window opening or coming to the front counts
  // only when it has the focus as well, so a browser woken in the background never claims
  const onAction = () => shown() && claim();
  const onFront = () => shown() && document.hasFocus() && claim();
  for (const a of ACTIONS) window.addEventListener(a, onAction, { capture: true, passive: true });
  document.addEventListener("visibilitychange", onFront);
  window.addEventListener("focus", onFront);
  onFront();
  return {
    claim,
    detach() {
      for (const a of ACTIONS) window.removeEventListener(a, onAction, { capture: true });
      document.removeEventListener("visibilitychange", onFront);
      window.removeEventListener("focus", onFront);
    },
  };
}
