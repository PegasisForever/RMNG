import { expect, test } from "bun:test";

import { formatHostsUsageSummary } from "./Sidebar";
import type { ContainerStats } from "~/lib/wire/ContainerStats";

const GiB = 1024 ** 3;

test("formats aggregate host-capacity CPU and memory usage", () => {
  const stats: Record<string, ContainerStats> = {
    alpha: {
      cpuPct: 16,
      memUsed: BigInt(Math.round(1.2 * GiB)),
      memLimit: BigInt(16 * GiB),
    },
    beta: {
      cpuPct: 8,
      memUsed: BigInt(Math.round(2.4 * GiB)),
      memLimit: BigInt(16 * GiB),
    },
    unbounded: {
      cpuPct: 20,
      memUsed: BigInt(Math.round(9.9 * GiB)),
      memLimit: BigInt(0),
    },
  };

  expect(formatHostsUsageSummary(["alpha", "beta", "unbounded", "missing"], stats)).toEqual({
    cpu: "44%",
    mem: "13.5GB",
  });
});

test("retains precision for an aggregate below one percent", () => {
  const stats: Record<string, ContainerStats> = {
    alpha: {
      cpuPct: 0.4,
      memUsed: BigInt(Math.round(0.5 * GiB)),
      memLimit: BigInt(16 * GiB),
    },
    beta: {
      cpuPct: 0.2,
      memUsed: BigInt(Math.round(0.7 * GiB)),
      memLimit: BigInt(16 * GiB),
    },
  };

  expect(formatHostsUsageSummary(["alpha", "beta"], stats)).toEqual({
    cpu: "0.6%",
    mem: "1.2GB",
  });
});
