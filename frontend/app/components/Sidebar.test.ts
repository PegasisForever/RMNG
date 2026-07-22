import { expect, test } from "bun:test";

import { formatLxcUsage, mergeActiveHostOrder, partitionHosts } from "./Sidebar";
import type { Host } from "~/lib/types";
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
