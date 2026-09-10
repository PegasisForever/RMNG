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
      seedSnapshot: null,
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
        claudeAccount: "group:pooled",
        codexAccount: "",
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
    judge: { codexModel: "gpt-5.6-luna", codexEmail: null },
    ...overrides,
  };
}

/** The patch, narrowed to the shape the tests read. `settingsPatch` returns `unknown` because
 *  it is a request body, not a value this app consumes. */
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
    claudeAccount: string;
    codexAccount: string;
    agentPlaybook: string;
    globalPrompt: string;
    dockerfile: string;
  }[];
  judge: { codexModel: string; codexEmail: string | null };
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

test("a blank Linear key is sent as-is, clearing the stored one", () => {
  const draft = settingsDraftFrom(config());
  draft.presets = [{ ...draft.presets[0], linearKey: "" }];
  expect(patch(draft).presets[0].linearKey).toBe("");
});

test("a blank account default is sent as-is, because blank is a real answer", () => {
  // Unlike the key, blank here means "no default — let the clone decide", which is different
  // from pinning the preset to `auto`.
  const draft = settingsDraftFrom(config());
  draft.presets = [{ ...draft.presets[0], claudeAccount: "" }];

  expect(patch(draft).presets[0].claudeAccount).toBe("");
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

test("clearing the judge's Codex account sends null, not a blank the server would ignore", () => {
  // An empty string means "keep stored" on the way in, so going back to "the first imported
  // account" after picking one has to be sent as null. Same rule as a pinned account email.
  const seeded = settingsDraftFrom(config());
  expect(seeded.judge.codexEmail).toBe("");
  expect(patch(seeded).judge.codexEmail).toBe(null);

  const picked = patch({
    ...seeded,
    judge: { codexModel: "gpt-5.6-luna", codexEmail: "alex@example.com" },
  });
  expect(picked.judge.codexEmail).toBe("alex@example.com");
});
