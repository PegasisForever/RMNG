import { getConfig } from "~/lib/api";

/** Open one browser viewer window per monitor: of `monitors` (a view spec), or else of the
 *  active layout preset (the same preset the server picks: the active one, else the first).
 *  Each is a plain window at the browser's default place and size; the operator moves, sizes and
 *  fullscreens it. A window that is open already stays as it is. */
export async function openViewerWindows(monitors?: { id: number }[]) {
  if (!monitors) {
    const cfg = await getConfig();
    const preset =
      cfg.layoutPresets.find((p) => p.name === cfg.activeLayout) ?? cfg.layoutPresets[0];
    // No preset at all: the server falls back to its built-in two monitors.
    monitors = preset?.monitors.map((_, id) => ({ id })) ?? [{ id: 0 }, { id: 1 }];
  }
  let blocked = 0;
  for (const m of monitors) {
    // `popup` makes it a window of its own rather than a tab. An empty URL finds a window of
    // this name that is open already without reloading it.
    const w = window.open("", `rmng-viewer-${m.id}`, "popup");
    if (!w) blocked++;
    else if (w.location.href === "about:blank") w.location.replace(`/viewer?monitor=${m.id}`);
  }
  // Chrome and Safari block every pop-up after the first one of a click, and say so only with
  // a small icon in the address bar.
  if (blocked > 0) alert(`The browser blocked ${blocked} viewer window(s). ${allowPopups()}`);
}

/** How to let this site open pop-ups, in the browser the operator uses. */
function allowPopups(): string {
  const ua = navigator.userAgent;
  if (/Firefox\//.test(ua)) {
    return `Click "Preferences" in the bar above the page, choose "Allow pop-ups for" this site, then open the viewer again.`;
  }
  // Safari: "Safari/" with no Chrome or Edge in the user agent.
  if (/Safari\//.test(ua) && !/Chrom(e|ium)\/|Edg\//.test(ua)) {
    return `In Safari, open Settings → Websites → Pop-up Windows, set this site to "Allow", then open the viewer again.`;
  }
  return `Click the blocked pop-up icon at the right of the address bar, choose "Always allow pop-ups", then open the viewer again.`;
}
