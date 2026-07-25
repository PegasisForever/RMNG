import { expect, test } from "bun:test";

import {
  flattenTreeOrder,
  formatLxcUsage,
  groupSubClones,
  mergeActiveCloneOrder,
  partitionClones,
} from "./Sidebar";
import type { Clone } from "~/lib/types";
import type { LxcStats } from "~/lib/wire/LxcStats";

const GiB = 1024 ** 3;

const clone = (id: string, parent?: string): Clone => ({
  id,
  host: id,
  port: 3389,
  username: "",
  password: "",
  managed: true,
  parent,
});

test("formats whole-LXC CPU, memory, and physical disk usage", () => {
  const stats: LxcStats = {
    cpuPct: 16,
    memUsed: BigInt(Math.round(13.5 * GiB)),
    memLimit: BigInt(264 * GiB),
    diskUsed: BigInt(Math.round(312.4 * GiB)),
  };

  expect(formatLxcUsage(stats)).toEqual({
    cpu: "16%",
    mem: "13.5GB",
    disk: "312.4GB",
  });
});

test("keeps unavailable LXC rate and disk visibly unavailable", () => {
  const stats: LxcStats = {
    cpuPct: null,
    memUsed: BigInt(Math.round(0.5 * GiB)),
    memLimit: BigInt(0),
    diskUsed: null,
  };

  expect(formatLxcUsage(stats)).toEqual({
    cpu: "—",
    mem: "0.5GB",
    disk: "—",
  });
  expect(formatLxcUsage(null)).toBeNull();
});

test("partitions archived clones and preserves their order during active reordering", () => {
  const hosts: Clone[] = [
    { id: "alpha", host: "alpha", port: 3389, username: "", password: "" },
    { id: "bravo", host: "bravo", port: 3389, username: "", password: "", archived: true },
    { id: "charlie", host: "charlie", port: 3389, username: "", password: "" },
  ];

  expect(partitionClones(hosts)).toMatchObject({
    activeClones: [{ id: "alpha" }, { id: "charlie" }],
    archivedClones: [{ id: "bravo" }],
  });
  expect(mergeActiveCloneOrder(hosts, ["charlie", "alpha"])).toEqual([
    "charlie",
    "bravo",
    "alpha",
  ]);
});

test("groups sub clones under their parent, preserving order", () => {
  const active = [clone("p1"), clone("c1", "p1"), clone("p2"), clone("c2", "p1")];
  const { topLevel, childrenByParent } = groupSubClones(active);
  expect(topLevel.map((h) => h.id)).toEqual(["p1", "p2"]);
  expect(childrenByParent.get("p1")?.map((h) => h.id)).toEqual(["c1", "c2"]);
  expect(childrenByParent.has("p2")).toBe(false);
});

test("a child whose parent is not an active clone renders at top level", () => {
  // parent "gone" isn't in the active set (archived/deleted) — the child must not disappear.
  const active = [clone("orphan", "gone"), clone("p1")];
  const { topLevel, childrenByParent } = groupSubClones(active);
  expect(topLevel.map((h) => h.id)).toEqual(["orphan", "p1"]);
  expect(childrenByParent.size).toBe(0);
});

test("flattenTreeOrder keeps each parent's children directly under it", () => {
  const active = [clone("p1"), clone("c1", "p1"), clone("p2")];
  const { childrenByParent } = groupSubClones(active);
  // Reorder top-level to [p2, p1]; p1's child rides along under p1.
  expect(flattenTreeOrder(["p2", "p1"], childrenByParent)).toEqual(["p2", "p1", "c1"]);
});
