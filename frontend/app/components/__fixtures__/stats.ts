// The numbers on a clone row, from the two sources that feed them: the volatile `stats` /
// `lxcStats` SSE buses, and the persisted per-clone token totals in `ControlState`.

import type { ContainerStats } from "~/lib/wire/ContainerStats";
import type { CloneTokens } from "~/lib/wire/CloneTokens";
import type { LxcStats } from "~/lib/wire/LxcStats";

import { cloneDualProvider, cloneIdle, cloneNoToken, cloneWorking } from "./clones";

const GiB = 1024 ** 3;

/** Live usage for the whole LXC that hosts RMNG, as the control rail draws it. */
export function makeLxcStats(overrides: Partial<LxcStats> = {}): LxcStats {
  return {
    cpuPct: 23,
    memUsed: BigInt(Math.round(18.7 * GiB)),
    memLimit: BigInt(264 * GiB),
    diskUsed: BigInt(Math.round(312.1 * GiB)),
    ...overrides,
  };
}

export const lxcStats: LxcStats = makeLxcStats();

/** One clone's live container usage. The limit is the per-clone allowance, which is what the
 *  sidebar normalizes CPU against. */
export function makeContainerStats(overrides: Partial<ContainerStats> = {}): ContainerStats {
  return {
    cpuPct: 12,
    memUsed: BigInt(Math.round(2.4 * GiB)),
    memLimit: BigInt(40 * GiB),
    ...overrides,
  };
}

/** The volatile `stats` SSE map. Offline and unmanaged clones are absent, which is how the
 *  real bus reports them. */
export const stats: Record<string, ContainerStats> = {
  [cloneWorking.id]: makeContainerStats({ cpuPct: 40, memUsed: BigInt(Math.round(5.1 * GiB)) }),
  [cloneIdle.id]: makeContainerStats({ cpuPct: 1.2, memUsed: BigInt(Math.round(1.4 * GiB)) }),
  [cloneNoToken.id]: makeContainerStats({ cpuPct: 0.3, memUsed: BigInt(Math.round(0.6 * GiB)) }),
  [cloneDualProvider.id]: makeContainerStats({
    cpuPct: 18,
    memUsed: BigInt(Math.round(3.2 * GiB)),
  }),
};

/** All-time input/output for one clone, summed across both providers, plus the transient
 *  "this clone just used Fable" flag. */
export function makeCloneTokens(overrides: Partial<CloneTokens> = {}): CloneTokens {
  return {
    inputTokens: BigInt(6_000_000),
    outputTokens: BigInt(210_000),
    fableActive: false,
    ...overrides,
  };
}

/** `ControlState.cloneTokens`. The figures deliberately span several orders of magnitude so
 *  the compact formatter's thresholds (k / M) are exercised on screen. */
export const cloneTokens: Record<string, CloneTokens> = {
  [cloneWorking.id]: makeCloneTokens({
    inputTokens: BigInt(24_100_000),
    outputTokens: BigInt(890_000),
    fableActive: true,
  }),
  [cloneIdle.id]: makeCloneTokens({
    inputTokens: BigInt(6_000_000),
    outputTokens: BigInt(210_000),
  }),
  [cloneDualProvider.id]: makeCloneTokens({
    inputTokens: BigInt(1_240_000),
    outputTokens: BigInt(96_400),
  }),
  // A clone that has barely run: sub-1k figures render as plain integers.
  [cloneNoToken.id]: makeCloneTokens({ inputTokens: BigInt(812), outputTokens: BigInt(47) }),
};
