import { describe, expect, test } from "bun:test";

import type { ViewSpec } from "~/lib/wire/ViewSpec";

import { withFrameSize } from "./frameSize";

const spec: ViewSpec = {
  monitors: [
    { id: 0, x: 0, y: 0, width: 2560, height: 1440, primary: true },
    { id: 1, x: 2560, y: 0, width: 1920, height: 1080, primary: false },
  ],
  content: { mode: "desktop" },
};

describe("withFrameSize", () => {
  test("the window's monitor takes the picture's size while the clone still has the old one", () => {
    const s = withFrameSize(spec, 0, { width: 780, height: 1688 });
    expect(s.monitors[0]).toEqual({ id: 0, x: 0, y: 0, width: 780, height: 1688, primary: true });
    // The other monitor keeps the spec's size
    expect(s.monitors[1]).toEqual(spec.monitors[1]);
  });

  test("no picture yet, or one of the spec's size: the spec as it is", () => {
    expect(withFrameSize(spec, 0, null)).toBe(spec);
    expect(withFrameSize(spec, 1, { width: 1920, height: 1080 })).toBe(spec);
  });

  test("a terminal, or a monitor not in the spec: the spec as it is", () => {
    const term = { ...spec, content: { mode: "terminal", clone: "c", sessions: [] } } as unknown as ViewSpec;
    expect(withFrameSize(term, 0, { width: 10, height: 10 })).toBe(term);
    expect(withFrameSize(spec, 5, { width: 10, height: 10 })).toBe(spec);
    expect(withFrameSize({ ...spec, monitors: [] }, 0, { width: 10, height: 10 }).monitors).toEqual([]);
  });
});
