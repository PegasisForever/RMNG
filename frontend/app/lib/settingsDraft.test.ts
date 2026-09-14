// The settings form's seed and save rules. These are the conditions a many-way component
// split is most likely to drop, because none of them belongs to any one section: what a blank
// config becomes, what a save trims, what it drops, and what it refuses to send at all.
import { expect, test } from "bun:test";

import { settingsDraftFrom, settingsPatch } from "./settingsDraft";
import type { AppConfigRedacted } from "~/lib/wire/AppConfigRedacted";

function config(overrides: Partial<AppConfigRedacted> = {}): AppConfigRedacted {
  return {
    setupComplete: true,
    layoutPresets: [
      {
        name: "Default",
        monitors: [{ width: 2560, height: 1440, x: 0, y: 0, primary: true }],
      },
    ],
    activeLayout: "Default",
    docker: {
      hostnamePrefix: "pega-",
      cloneCpus: 16,
      cloneMemoryMb: 32768,
      homesParent: "tank/rmng/homes",
    },
    claude: {},
    codex: { autoReset: false },
    groups: [{ name: "pooled", accounts: ["alex@example.com"] }],
    presets: [
      {
        name: "webapp",
        labels: ["WE", "frontend"],
        linearKey: "lin_api_fixture",
        group: "pooled",
        defaultForkClone: "",
        vars: [{ key: "TURBO_TEAM", value: "talktomedi" }],
        agentPlaybook: "",
        globalPrompt: "",
        startupScript: "",
        dockerfile: "FROM pegasis0/rmng-template:latest",
      },
    ],
    chroma: "yuv420",
    ssh: {
      authorizedKeys: ["ssh-ed25519 AAAA me@laptop"],
    },
    agentPlaybook: "playbook",
    globalPrompt: "prompt",
    judge: { provider: "codex", geminiKey: "" },
    ...overrides,
  };
}

/** The patch, narrowed to the shape the tests read. */
type Patch = {
  docker: { hostnamePrefix: string };
  codex: { autoReset: boolean };
  groups: { name: string; accounts: string[] }[];
  layoutPresets: {
    name: string;
    monitors: {
      width: number;
      height: number;
      x: number;
      y: number;
      primary: boolean;
    }[];
  }[];
  presets: {
    name: string;
    labels: string[];
    linearKey: string;
    group: string;
    defaultForkClone: string;
    vars: { key: string; value: string }[];
    agentPlaybook: string;
    globalPrompt: string;
    dockerfile: string;
  }[];
  judge: { provider: string; geminiKey: string };
};

const patch = (
  draft: ReturnType<typeof settingsDraftFrom>,
  setupComplete = true,
) => settingsPatch(draft, setupComplete) as Patch;

// --- seeding the form ---------------------------------------------------------------------

test("a rig with no layout preset is given one to edit", () => {
  // An empty list would leave the operator with nothing to type into, and no way to make the
  // first arrangement.
  const draft = settingsDraftFrom(config({ layoutPresets: [] }));

  expect(draft.layoutPresets).toEqual([
    {
      name: "Default",
      monitors: [{ width: 1920, height: 1080, x: 0, y: 0, primary: true }],
    },
  ]);
});

test("a preset with no pool default is seeded with the first pool", () => {
  // A preset always names a default: a blank from an older server reads as the first
  // pool, while `"none"` (any group) seeds as-is.
  const blank = config({
    presets: [{ ...config().presets[0], group: "" }],
  });
  expect(settingsDraftFrom(blank).presets[0].group).toBe("pooled");

  const any = config({
    presets: [{ ...config().presets[0], group: "none" }],
  });
  expect(settingsDraftFrom(any).presets[0].group).toBe("none");
});

test("the form never shares an array with the config it was seeded from", () => {
  // Both sides would otherwise be the same object, and a save that re-seeds from the server's
  // answer would be comparing a value against itself.
  const c = config();
  const draft = settingsDraftFrom(c);

  expect(draft.groups[0].accounts).not.toBe(c.groups[0].accounts);
  expect(draft.layoutPresets[0].monitors[0]).not.toBe(
    c.layoutPresets[0].monitors[0],
  );
});

// --- what a save sends --------------------------------------------------------------------

test("the docker patch names only the fields the panel still edits", () => {
  // Hostname prefix + sizing. The subnet, socket, and images are hardcoded on the server.
  const draft = settingsDraftFrom(config());

  expect(Object.keys(patch(draft, true).docker).sort()).toEqual([
    "cloneCpus",
    "cloneMemoryMb",
    "hostnamePrefix",
  ]);
});

test("a half-typed pool is dropped rather than saved unnamed", () => {
  const draft = settingsDraftFrom(config());
  draft.groups = [
    { name: "  pooled  ", accounts: ["alex@example.com"] },
    { name: "   ", accounts: ["sam@example.com"] },
  ];

  expect(patch(draft).groups).toEqual([
    { name: "pooled", accounts: ["alex@example.com"] },
  ]);
});

test("repeated pool members are deduped", () => {
  // The checkbox editor cannot produce a duplicate, but a hand-edited config can, and a
  // repeated email would skew group selection.
  const draft = settingsDraftFrom(config());
  draft.groups = [
    { name: "team", accounts: ["a@x.com", "a@x.com", "b@x.com"] },
  ];

  expect(patch(draft).groups).toEqual([
    { name: "team", accounts: ["a@x.com", "b@x.com"] },
  ]);
});

test("a half-typed preset is dropped, and the rest are trimmed", () => {
  const draft = settingsDraftFrom(config());
  draft.presets = [
    { ...draft.presets[0], name: "  webapp  " },
    { ...draft.presets[0], name: "" },
  ];

  const saved = patch(draft).presets;
  expect(saved).toHaveLength(1);
  expect(saved[0].name).toBe("webapp");
});

test("the labels string is split back into team keys, blanks dropped", () => {
  const draft = settingsDraftFrom(config());
  draft.presets = [{ ...draft.presets[0], labels: " WE , , frontend ,, " }];

  expect(patch(draft).presets[0].labels).toEqual(["WE", "frontend"]);
});

test("a blank Dockerfile resets to the default base on save", () => {
  const draft = settingsDraftFrom(config());
  draft.presets = [{ ...draft.presets[0], dockerfile: "   " }];

  expect(patch(draft).presets[0].dockerfile).toBe(
    "FROM pegasis0/rmng-template:latest",
  );
});

test("preset env vars survive the round trip, half-typed rows included", () => {
  // The rows go to the server as the operator left them: it is the server that trims keys
  // and drops the blank-key ones, so the form does not have to police a row still being
  // typed. A row whose VALUE is blank is a real setting (`KEY=` clears an inherited value)
  // and must reach the patch intact.
  const draft = settingsDraftFrom(config());
  expect(draft.presets[0].vars).toEqual([
    { key: "TURBO_TEAM", value: "talktomedi" },
  ]);

  draft.presets = [
    {
      ...draft.presets[0],
      vars: [
        { key: "TURBO_TEAM", value: "talktomedi" },
        { key: "", value: "" },
        { key: "BLANK", value: "" },
      ],
    },
  ];
  expect(patch(draft).presets[0].vars).toEqual([
    { key: "TURBO_TEAM", value: "talktomedi" },
    { key: "", value: "" },
    { key: "BLANK", value: "" },
  ]);
});

test("a blank Linear key is sent as-is, clearing the stored one", () => {
  const draft = settingsDraftFrom(config());
  draft.presets = [{ ...draft.presets[0], linearKey: "" }];
  expect(patch(draft).presets[0].linearKey).toBe("");
});

test("a blank pool default falls back to the first pool", () => {
  // A preset always names a default: a blank (only reachable from an older save) reads
  // as the first pool, while `"none"` (any group) is a real choice that survives as-is.
  const draft = settingsDraftFrom(config());
  draft.presets = [{ ...draft.presets[0], group: "" }];

  expect(patch(draft).presets[0].group).toBe("pooled");
});

test("any group survives the round trip", () => {
  const draft = settingsDraftFrom(config());
  draft.presets = [{ ...draft.presets[0], group: "none" }];

  expect(patch(draft).presets[0].group).toBe("none");
});

test("the fork default is trimmed, blank means oldest", () => {
  const draft = settingsDraftFrom(config());
  draft.presets = [{ ...draft.presets[0], defaultForkClone: "  pega-x  " }];
  expect(patch(draft).presets[0].defaultForkClone).toBe("pega-x");

  const blank = settingsDraftFrom(config());
  blank.presets = [{ ...blank.presets[0], defaultForkClone: "   " }];
  expect(patch(blank).presets[0].defaultForkClone).toBe("");
});

test("an unnamed layout preset is dropped and negative geometry is clamped", () => {
  const draft = settingsDraftFrom(config());
  draft.layoutPresets = [
    {
      name: "  Dual  ",
      monitors: [{ width: 0, height: -5, x: -100, y: -1, primary: true }],
    },
    {
      name: "",
      monitors: [{ width: 1920, height: 1080, x: 0, y: 0, primary: false }],
    },
  ];

  const saved = patch(draft).layoutPresets;
  expect(saved).toHaveLength(1);
  expect(saved[0].name).toBe("Dual");
  expect(saved[0].monitors[0]).toEqual({
    width: 1,
    height: 1,
    x: 0,
    y: 0,
    primary: true,
  });
});

test("the cosmetic account order is never part of the patch", () => {
  // The pool is unordered as far as the server is concerned, so the order stays in the
  // browser. Nothing in the patch names it.
  expect(Object.keys(patch(settingsDraftFrom(config())))).not.toContain(
    "acctOrder",
  );
});

test("the judge key round-trips verbatim like a Linear key", () => {
  const seeded = settingsDraftFrom(config());
  expect(seeded.judge.provider).toBe("codex");
  expect(seeded.judge.geminiKey).toBe("");
  expect(patch(seeded).judge.geminiKey).toBe("");

  const withKey = settingsDraftFrom(
    config({ judge: { provider: "gemini", geminiKey: "K" } }),
  );
  expect(withKey.judge.provider).toBe("gemini");
  expect(patch(withKey).judge.geminiKey).toBe("K");
});
