// The first-run wizard's seed and save rules. The reason this module is not `settingsDraft.ts`:
// each step patches only the fields that step edits. Getting that wrong silently corrupts a
// fresh install.
import { expect, test } from "bun:test";

import {
  nextDisabled,
  serverPatch,
  setupDraftFrom,
} from "./setupDraft";
import type { AppConfigRedacted } from "~/lib/wire/AppConfigRedacted";

function config(overrides: Partial<AppConfigRedacted> = {}): AppConfigRedacted {
  return {
    setupComplete: false,
    docker: {
      hostnamePrefix: "pega-",
      cloneCpus: 16,
      cloneMemoryMb: 32768,
      homesParent: "tank/rmng/homes",
    },
    claude: {},
    codex: { autoReset: false },
    groups: [],
    presets: [],
    ssh: { authorizedKeys: [] },
    agentPlaybook: "",
    globalPrompt: "",
    judge: { provider: "codex", geminiKey: "" },
    assistant: { url: "", serverUrl: "" },
    ...overrides,
  };
}

/** The step-2 patch, narrowed to the shape the tests read. The patch functions return
 *  `unknown` because they are request bodies, not values this app consumes. */
type ServerPatch = { docker: Record<string, unknown> };

test("step 2 patches only the three docker fields it edits", () => {
  const patch = serverPatch(setupDraftFrom(config())) as ServerPatch;
  expect(Object.keys(patch.docker).sort()).toEqual([
    "cloneCpus",
    "cloneMemoryMb",
    "hostnamePrefix",
  ]);
});

test("Next is blocked by a failing check and a save", () => {
  const args = { step: 0, saving: false, envOk: true };
  expect(nextDisabled(args)).toBe(false);
  expect(nextDisabled({ ...args, envOk: false })).toBe(true);
  expect(nextDisabled({ ...args, saving: true })).toBe(true);
  // The environment gate applies to step 1 only: a failing check does not lock step 2.
  expect(nextDisabled({ ...args, step: 1, envOk: false })).toBe(false);
});
