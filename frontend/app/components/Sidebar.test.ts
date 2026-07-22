import { expect, test } from "bun:test";

import { formatHostsUsageSummary, mergeActiveHostOrder, partitionHosts } from "./Sidebar";
import type { Host } from "~/lib/types";
import type { ContainerStats } from "~/lib/wire/ContainerStats";

const GiB = 1024 ** 3;

test("formats aggregate host CPU and memory usage", () => {
  const stats: Record<string, ContainerStats> = {
    alpha: {
      cpuPct: 160,
      memUsed: BigInt(Math.round(1.2 * GiB)),
      memLimit: BigInt(8 * GiB),
      dockerDiskUsed: BigInt(42 * GiB),
    },
    beta: {
      cpuPct: 80,
      memUsed: BigInt(Math.round(2.4 * GiB)),
      memLimit: BigInt(8 * GiB),
      dockerDiskUsed: BigInt(42 * GiB),
    },
    ignoredWithoutLimit: {
      cpuPct: 20,
      memUsed: BigInt(Math.round(9.9 * GiB)),
      memLimit: BigInt(0),
      dockerDiskUsed: BigInt(99 * GiB),
    },
  };

  expect(formatHostsUsageSummary(["alpha", "beta", "missing"], stats, 4)).toEqual({
    cpu: "60%",
    mem: "3.6GB",
    disk: "42.0GB",
  });
});

test("partitions archived hosts and preserves their order during active reordering", () => {
  const hosts: Host[] = [
    { id: "alpha", host: "alpha", port: 3389, username: "", password: "" },
    { id: "bravo", host: "bravo", port: 3389, username: "", password: "", archived: true },
    { id: "charlie", host: "charlie", port: 3389, username: "", password: "" },
  ];

  expect(partitionHosts(hosts)).toMatchObject({
    activeHosts: [{ id: "alpha" }, { id: "charlie" }],
    archivedHosts: [{ id: "bravo" }],
  });
  expect(mergeActiveHostOrder(hosts, ["charlie", "alpha"])).toEqual([
    "charlie",
    "bravo",
    "alpha",
  ]);
});

test("formats aggregate host CPU as cores when clone CPU allowance is unlimited", () => {
  const stats: Record<string, ContainerStats> = {
    alpha: {
      cpuPct: 150,
      memUsed: BigInt(Math.round(0.5 * GiB)),
      memLimit: BigInt(8 * GiB),
      dockerDiskUsed: BigInt(0),
    },
    beta: {
      cpuPct: 50,
      memUsed: BigInt(Math.round(0.7 * GiB)),
      memLimit: BigInt(8 * GiB),
      dockerDiskUsed: BigInt(0),
    },
  };

  expect(formatHostsUsageSummary(["alpha", "beta"], stats, 0)).toEqual({
    cpu: "2.0c",
    mem: "1.2GB",
  });
});
