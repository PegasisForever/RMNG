import { expect, test } from "bun:test";

import { formatHostUsage } from "./SidebarHost";
import type { ContainerStats } from "~/lib/wire/ContainerStats";

const GiB = 1024 ** 3;

test("formats a direct sample with sub-percent CPU and no memory cap", () => {
  const stats: ContainerStats = {
    cpuPct: 0.4,
    memUsed: BigInt(Math.round(2.5 * GiB)),
    memLimit: BigInt(0),
  };

  expect(formatHostUsage(stats)).toEqual({ cpu: "0.4%", mem: "2.5GB" });
});

test("has no metric labels before the first sample", () => {
  expect(formatHostUsage(undefined)).toBeNull();
});
