import { useEffect, useRef } from "react";

import type { CursorShape } from "~/lib/wire/CursorShape";
import { shapeToRgba } from "~/viewer/input";

/** The clone's cursor, drawn over the video: a touch screen has no cursor of its own. `x`,
 *  `y` are monitor pixels, and a monitor pixel is one device pixel, so the sprite is drawn at
 *  its own size in device pixels. */
export function Cursor({ shape, x, y }: { shape: CursorShape; x: number; y: number }) {
  const ref = useRef<HTMLCanvasElement>(null);
  useEffect(() => {
    const c = ref.current;
    if (!c || !shape.width || !shape.height) return;
    c.width = shape.width;
    c.height = shape.height;
    c.getContext("2d")?.putImageData(new ImageData(shapeToRgba(shape), shape.width, shape.height), 0, 0);
  }, [shape]);
  const dpr = typeof window === "undefined" ? 1 : window.devicePixelRatio || 1;
  return (
    <canvas
      ref={ref}
      aria-hidden
      className="pointer-events-none absolute left-0 top-0"
      style={{
        width: shape.width / dpr,
        height: shape.height / dpr,
        transform: `translate(${(x - shape.hotspot_x) / dpr}px, ${(y - shape.hotspot_y) / dpr}px)`,
      }}
    />
  );
}
