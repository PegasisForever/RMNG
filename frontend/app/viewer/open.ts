import { getConfig } from "~/lib/api";

/** One screen from Chrome's Window Management API (`getScreenDetails`); not in TypeScript's
 *  DOM types. Coordinates are CSS pixels in the multi-screen space. */
export interface ScreenInfo {
  left: number;
  top: number;
  availLeft: number;
  availTop: number;
  availWidth: number;
  availHeight: number;
}

/** A monitor's place in the layout (unified-desktop pixels). */
interface Placed {
  id: number;
  x: number;
  y: number;
}

/** Pair monitors with screens by position: both sorted left to right (then top to bottom),
 *  the first monitor gets the first screen and so on. Monitors past the last screen get none. */
export function matchScreens<S extends ScreenInfo>(monitors: Placed[], screens: S[]): Map<number, S> {
  const ms = [...monitors].sort((a, b) => a.x - b.x || a.y - b.y);
  const ss = [...screens].sort((a, b) => a.left - b.left || a.top - b.top);
  return new Map(ms.slice(0, ss.length).map((m, i) => [m.id, ss[i]]));
}

/** The screens, or null when Chrome has no Window Management API or the operator refused it.
 *  The first call asks for the "window management" permission. */
export async function screens(): Promise<ScreenInfo[] | null> {
  const w = window as Window & { getScreenDetails?: () => Promise<{ screens: ScreenInfo[] }> };
  try {
    return (await w.getScreenDetails?.())?.screens ?? null;
  } catch {
    return null;
  }
}

/** Open one browser viewer window per monitor: of `monitors` (a view spec), or else of the
 *  active layout preset (the same preset the server picks: the active one, else the first).
 *  With the window-management permission each window opens on its own screen, covering it,
 *  and asks for fullscreen there (`fullscreen=1`). A window that is open already stays as it
 *  is, also on its old screen when the layout changed. Chrome lets one click open one pop-up, so with more than one monitor the operator
 *  allows pop-ups for this site once. */
export async function openViewerWindows(monitors?: Placed[]) {
  if (!monitors) {
    const cfg = await getConfig();
    const preset =
      cfg.layoutPresets.find((p) => p.name === cfg.activeLayout) ?? cfg.layoutPresets[0];
    // No preset at all: the server falls back to its built-in two monitors side by side.
    monitors = preset?.monitors.map((m, id) => ({ id, x: m.x, y: m.y })) ?? [
      { id: 0, x: 0, y: 0 },
      { id: 1, x: 1, y: 0 },
    ];
  }
  const placed = matchScreens(monitors, (await screens()) ?? []);
  for (const m of monitors) {
    const s = placed.get(m.id);
    const features = s
      ? `popup,left=${s.availLeft},top=${s.availTop},width=${s.availWidth},height=${s.availHeight}`
      : "popup";
    // An empty URL finds a window of this name that is open already without reloading it.
    const w = window.open("", `rmng-viewer-${m.id}`, features);
    if (w && w.location.href === "about:blank") {
      w.location.replace(`/viewer?monitor=${m.id}${s ? "&fullscreen=1" : ""}`);
    }
  }
}
