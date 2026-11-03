// The size this window shows its monitor at (`./remote.ts`). No React.
import type { ViewSpec } from "~/lib/wire/ViewSpec";

type Size = { width: number; height: number };

/** `spec` with this window's monitor at the size of the picture it shows (`frame`). The server's
 *  spec has the layout in force at once, while the clone takes a moment to rebuild its monitors
 *  (or, after a switch, still has the layout it was last viewed with): sized from the spec, the
 *  screen took the new size first and showed the old picture with bars in it, and the clicks
 *  missed, until the new picture came. Sized from the picture, it changes once. */
export function withFrameSize(spec: ViewSpec, monitor: number, frame: Size | null): ViewSpec {
  if (!frame || spec.content.mode !== "desktop") return spec;
  const m = spec.monitors.find((v) => v.id === monitor);
  if (!m || (m.width === frame.width && m.height === frame.height)) return spec;
  return { ...spec, monitors: spec.monitors.map((v) => (v.id === monitor ? { ...v, ...frame } : v)) };
}
