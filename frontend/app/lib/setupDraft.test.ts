// The first-run wizard's seed and save rules. Two of them are the reason this module is not
// `settingsDraft.ts`: each step patches only the fields that step edits, and the step-2 patch
// round-trips the layout presets the wizard never showed rather than rewriting them the way a
// full settings save does. Getting either wrong silently corrupts a fresh install.
import { expect, test } from "bun:test";

import {
  canPull,
  findPullOperation,
  isValidSubnet,
  layoutPresetsPatch,
  nextDisabled,
  pullReference,
  serverPatch,
  setupDraftFrom,
  subnetOk,
  subnetPatch,
} from "./setupDraft";
import type { Operation } from "~/lib/types";
import type { AppConfigRedacted } from "~/lib/wire/AppConfigRedacted";

function config(overrides: Partial<AppConfigRedacted> = {}): AppConfigRedacted {
  return {
    listen: { web: 9000, video: 9001, daemonMcp: 9004, forward: 9005, bastion: 2222 },
    agentPort: 4096,
    dataDir: "/data",
    staticDir: "",
    cloneSocket: "/srv/rmng-sock/clones.sock",
    setupComplete: false,
    layoutPresets: [
      { name: "Default", monitors: [{ width: 2560, height: 1440, x: 0, y: 0, primary: true }] },
    ],
    activeLayout: "Default",
    docker: {
      socket: "/var/run/docker.sock",
      subnet: "10.99.0.0/24",
      hostnamePrefix: "pega-",
      cloneCpus: 16,
      cloneMemoryMb: 32768,
      templateReference: "pegasis0/rmng-template:latest",
      serverImage: "pegasis0/rmng:latest",
      buildInfraEnabled: true,
      registryImage: "registry:2.8.3",
      buildkitImage: "moby/buildkit:v0.17.2",
      buildkitCacheGb: 40,
    },
    claude: { pollSecs: BigInt(600), pinnedEmail: null },
    codex: { pollSecs: BigInt(600), pinnedEmail: null, usagePolling: true, autoReset: false },
    cloneGroups: [],
    codexGroups: [],
    presets: [],
    chroma: "yuv420",
    ssh: { authorizedKeys: [], publicHost: "" },
    agentPlaybook: "",
    globalPrompt: "",
    ...overrides,
  };
}

/** The step-2 patch, narrowed to the shape the tests read. The patch functions return
 *  `unknown` because they are request bodies, not values this app consumes. */
type ServerPatch = {
  docker: Record<string, unknown>;
  layoutPresets: { name: string; monitors: { width: number; x: number }[] }[];
  chroma: string;
  agentPort: number;
};

function operation(overrides: Partial<Operation> = {}): Operation {
  return {
    id: "op-1",
    kind: "pull",
    target: "pegasis0/rmng-template:latest",
    status: "running",
    step: "download",
    pct: 10,
    message: "",
    log: [],
    startedAt: 0,
    ...overrides,
  };
}

test("a rig with no layout preset gets one 1080p monitor to edit", () => {
  expect(setupDraftFrom(config({ layoutPresets: [] })).monitors).toEqual([
    { width: 1920, height: 1080, x: 0, y: 0, primary: true },
  ]);
});

test("the seed copies the monitors rather than aliasing the config's", () => {
  const c = config();
  const draft = setupDraftFrom(c);
  draft.monitors[0].width = 1;
  expect(c.layoutPresets[0].monitors[0].width).toBe(2560);
});

test("the seed follows activeLayout, not the first preset", () => {
  const c = config({
    activeLayout: "Wide",
    layoutPresets: [
      { name: "Default", monitors: [{ width: 1280, height: 720, x: 0, y: 0, primary: true }] },
      { name: "Wide", monitors: [{ width: 3840, height: 2160, x: 0, y: 0, primary: true }] },
    ],
  });
  expect(setupDraftFrom(c).monitors[0].width).toBe(3840);
});

test("step 1 sends the trimmed subnet and nothing else", () => {
  const draft = { ...setupDraftFrom(config()), subnet: "  10.42.0.0/22  " };
  expect(subnetPatch(draft)).toEqual({ docker: { subnet: "10.42.0.0/22" } });
});

test("step 2 patches only the three docker fields it edits", () => {
  const patch = serverPatch(setupDraftFrom(config()), config()) as ServerPatch;
  expect(Object.keys(patch.docker).sort()).toEqual([
    "cloneCpus",
    "cloneMemoryMb",
    "hostnamePrefix",
  ]);
});

test("step 2 never sends the template reference — the pull is not a save", () => {
  const patch = serverPatch(setupDraftFrom(config()), config()) as ServerPatch;
  expect(JSON.stringify(patch)).not.toContain("rmng-template");
});

test("step 2 clamps the edited arrangement", () => {
  const draft = {
    ...setupDraftFrom(config()),
    monitors: [{ width: 0, height: 0, x: -5, y: -5, primary: true }],
  };
  expect(layoutPresetsPatch(draft, config())[0].monitors).toEqual([
    { width: 1, height: 1, x: 0, y: 0, primary: true },
  ]);
});

test("step 2 round-trips the presets the wizard never showed, untouched", () => {
  const c = config({
    activeLayout: "Default",
    layoutPresets: [
      { name: "Default", monitors: [{ width: 2560, height: 1440, x: 0, y: 0, primary: true }] },
      // Junk a hand-edited config can hold. A full settings save would clamp and trim it;
      // the wizard has no business rewriting a preset it never drew.
      { name: "  Legacy  ", monitors: [{ width: 0, height: 0, x: -8, y: 0, primary: false }] },
    ],
  });
  const presets = layoutPresetsPatch(setupDraftFrom(c), c);
  expect(presets).toHaveLength(2);
  expect(presets[1].name).toBe("  Legacy  ");
  expect(presets[1].monitors[0]).toEqual({ width: 0, height: 0, x: -8, y: 0, primary: false });
});

test("an active layout the config does not name is appended, not swapped in", () => {
  const c = config({
    activeLayout: "Default",
    layoutPresets: [
      { name: "Other", monitors: [{ width: 1280, height: 720, x: 0, y: 0, primary: true }] },
    ],
  });
  // `activeLayoutName` falls back to the FIRST preset, so this edits "Other" in place.
  const presets = layoutPresetsPatch(setupDraftFrom(c), c);
  expect(presets.map((p) => p.name)).toEqual(["Other"]);
});

test("a rig with no presets at all gets one named Default", () => {
  const c = config({ layoutPresets: [] });
  expect(layoutPresetsPatch(setupDraftFrom(c), c)).toEqual([
    { name: "Default", monitors: [{ width: 1920, height: 1080, x: 0, y: 0, primary: true }] },
  ]);
});

test("a /16 to /24 IPv4 CIDR is the only thing the subnet accepts", () => {
  expect(isValidSubnet("10.99.0.0/24")).toBe(true);
  expect(isValidSubnet("10.99.0.0/16")).toBe(true);
  expect(isValidSubnet("10.0.0.0/8")).toBe(false);
  expect(isValidSubnet("10.99.0.0/25")).toBe(false);
  expect(isValidSubnet("10.99.0/24")).toBe(false);
  expect(isValidSubnet("10.99.0.256/24")).toBe(false);
  expect(isValidSubnet("10.99.0.0/24/8")).toBe(false);
  expect(isValidSubnet("10.99.0.0")).toBe(false);
});

test("a blank subnet is not valid — the bridge needs one", () => {
  expect(subnetOk("   ")).toBe(false);
  expect(subnetOk(" 10.99.0.0/24 ")).toBe(true);
});

test("a blank template field pulls the configured reference", () => {
  const draft = { ...setupDraftFrom(config()), templateReference: "   " };
  expect(pullReference(draft, config())).toBe("pegasis0/rmng-template:latest");
});

test("the pull op is found by kind and target, and only once a pull was started", () => {
  const ops = [operation({ kind: "clone", target: "node20:latest" }), operation()];
  expect(findPullOperation(ops, "pegasis0/rmng-template:latest")?.kind).toBe("pull");
  expect(findPullOperation(ops, "node20:latest")).toBeUndefined();
  expect(findPullOperation(ops, null)).toBeUndefined();
});

test("Download is dead with nothing to pull, mid-pull, and once it is done", () => {
  const args = {
    templateReference: "pegasis0/rmng-template:latest",
    pulling: false,
    pullRunning: false,
    pullDone: false,
  };
  expect(canPull(args)).toBe(true);
  expect(canPull({ ...args, templateReference: "  " })).toBe(false);
  expect(canPull({ ...args, pulling: true })).toBe(false);
  expect(canPull({ ...args, pullRunning: true })).toBe(false);
  expect(canPull({ ...args, pullDone: true })).toBe(false);
});

test("Next is blocked by a failing check, a bad subnet, a save, and a running pull", () => {
  const args = { step: 0, saving: false, envOk: true, subnetOk: true, pullRunning: false };
  expect(nextDisabled(args)).toBe(false);
  expect(nextDisabled({ ...args, envOk: false })).toBe(true);
  expect(nextDisabled({ ...args, subnetOk: false })).toBe(true);
  expect(nextDisabled({ ...args, saving: true })).toBe(true);
  // The environment gate applies to step 1 only: a failing check does not lock step 2.
  expect(nextDisabled({ ...args, step: 1, envOk: false, subnetOk: false })).toBe(false);
  expect(nextDisabled({ ...args, step: 2, pullRunning: true })).toBe(true);
});
