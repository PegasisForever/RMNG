import { expect, test } from "bun:test";

import { formatLxcUsage } from "./Sidebar";
import type { LxcStats } from "~/lib/wire/LxcStats";

const GiB = 1024 ** 3;

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
