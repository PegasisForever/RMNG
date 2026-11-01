import { useEffect, useRef } from "react";

import type { CursorShape } from "~/lib/wire/CursorShape";
import { shapeToRgba } from "~/viewer/input";

/** The clone's cursor, drawn over the video: a touch screen has no cursor of its own. `x`,
 *  `y` are monitor pixels, and a monitor pixel is `scale` device pixels each way, as in the
 *  video, so the sprite is drawn at that scale too. */
export function Cursor({ shape, x, y, scale }: { shape: CursorShape; x: number; y: number; scale: number }) {
  const ref = useRef<HTMLCanvasElement>(null);
  useEffect(() => {
    const c = ref.current;
    if (!c || !shape.width || !shape.height) return;
    c.width = shape.width;
    c.height = shape.height;
    c.getContext("2d")?.putImageData(new ImageData(shapeToRgba(shape), shape.width, shape.height), 0, 0);
  }, [shape]);
  // CSS px per monitor pixel
  const k = scale / (typeof window === "undefined" ? 1 : window.devicePixelRatio || 1);
  return (
    <canvas
      ref={ref}
      aria-hidden
      className="pointer-events-none absolute left-0 top-0"
      style={{
        width: shape.width * k,
        height: shape.height * k,
        transform: `translate(${(x - shape.hotspot_x) * k}px, ${(y - shape.hotspot_y) * k}px)`,
      }}
    />
  );
}
