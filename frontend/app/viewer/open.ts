import { getConfig } from "~/lib/api";

/** Open one browser viewer window per monitor of the active layout preset (the same preset
 *  the server picks: the active one, else the first). Chrome lets one click open one pop-up,
 *  so with more than one monitor the operator allows pop-ups for this site once. */
export async function openViewerWindows() {
  const cfg = await getConfig();
  const preset =
    cfg.layoutPresets.find((p) => p.name === cfg.activeLayout) ?? cfg.layoutPresets[0];
  // No preset at all: the server falls back to its built-in two monitors.
  const count = preset?.monitors.length ?? 2;
  for (let i = 0; i < count; i++) {
    window.open(`/viewer?monitor=${i}`, `rmng-viewer-${i}`, "popup");
  }
}
