import { getConfig } from "~/lib/api";

/** Open one browser viewer window per monitor: of `monitors` (a view spec), or else of the
 *  active layout preset (the same preset the server picks: the active one, else the first).
 *  Each is a plain window at Chrome's default place and size; the operator moves, sizes and
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
  // Chrome blocks every pop-up after the first one of a click, and says so only with a small
  // icon in the address bar.
  if (blocked > 0) {
    alert(
      `Chrome blocked ${blocked} viewer window(s). Click the blocked pop-up icon at the right ` +
        `of the address bar, choose "Always allow pop-ups", then open the viewer again.`,
    );
  }
}
