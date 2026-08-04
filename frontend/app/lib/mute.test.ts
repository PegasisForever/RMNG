import { describe, expect, it } from "bun:test";

import { cloneIndex, isMuted, isMutedByParent, mutedSet, toggleMuted } from "./mute";
import type { Clone } from "./types";

function makeClone(id: string, parent?: string): Clone {
  return { id, host: id, managed: true, ...(parent ? { parent } : {}) } as Clone;
}

const parent = makeClone("pega-we-1");
const child = makeClone("pega-we-1-helper", "pega-we-1");
const other = makeClone("pega-dev-9");
const hosts = [parent, child, other];
const byId = cloneIndex(hosts);

describe("isMuted", () => {
  it("is false when nothing is muted", () => {
    const none = mutedSet([]);
    expect(hosts.every((h) => !isMuted(h, byId, none))).toBe(true);
  });

  it("silences the clone that was muted", () => {
    const silent = mutedSet(["pega-we-1"]);
    expect(isMuted(parent, byId, silent)).toBe(true);
    expect(isMuted(other, byId, silent)).toBe(false);
  });

  it("silences a sub clone through its parent", () => {
    const silent = mutedSet(["pega-we-1"]);
    expect(isMuted(child, byId, silent)).toBe(true);
    // The child is not in the stored set, so unmuting the parent restores it with no
    // second write.
    expect(silent.has(child.id)).toBe(false);
  });

  it("silences a sub clone muted on its own, leaving its parent audible", () => {
    const silent = mutedSet(["pega-we-1-helper"]);
    expect(isMuted(child, byId, silent)).toBe(true);
    expect(isMuted(parent, byId, silent)).toBe(false);
  });

  it("terminates on a parent chain that points at itself", () => {
    const loop = makeClone("a", "a");
    expect(isMuted(loop, cloneIndex([loop]), mutedSet(["b"]))).toBe(false);
  });

  it("treats a parent that no longer exists as no parent", () => {
    const orphan = makeClone("orphan", "gone");
    expect(isMuted(orphan, cloneIndex([orphan]), mutedSet(["gone"]))).toBe(false);
  });
});

describe("isMutedByParent", () => {
  it("is true only for a clone silent by inheritance", () => {
    const silent = mutedSet(["pega-we-1"]);
    expect(isMutedByParent(child, byId, silent)).toBe(true);
    // Muted in its own right, so the menu offers the toggle rather than an explanation.
    expect(isMutedByParent(parent, byId, silent)).toBe(false);
    expect(isMutedByParent(other, byId, silent)).toBe(false);
  });

  it("is false for a sub clone muted on its own under a muted parent", () => {
    const silent = mutedSet(["pega-we-1", "pega-we-1-helper"]);
    expect(isMutedByParent(child, byId, silent)).toBe(false);
  });
});

describe("toggleMuted", () => {
  it("adds, removes, and keeps one sorted list", () => {
    expect(toggleMuted([], "b")).toEqual(["b"]);
    expect(toggleMuted(["b"], "a")).toEqual(["a", "b"]);
    expect(toggleMuted(["a", "b"], "a")).toEqual(["b"]);
    expect(toggleMuted(["a"], "a")).toEqual([]);
  });

  it("does not mutate what it was given", () => {
    const current = ["a"];
    toggleMuted(current, "b");
    expect(current).toEqual(["a"]);
  });
});
