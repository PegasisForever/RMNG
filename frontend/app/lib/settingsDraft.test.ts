// The settings form's seed and save rules. These are the conditions a many-way component
// split is most likely to drop, because none of them belongs to any one section: what a blank
// config becomes, what a save trims, what it drops, and what it refuses to send at all.
import { expect, test } from "bun:test";

import { orderedAccounts, settingsDraftFrom, settingsPatch } from "./settingsDraft";
import type { ClaudeUsage } from "~/lib/types";
import type { AppConfigRedacted } from "~/lib/wire/AppConfigRedacted";

function config(overrides: Partial<AppConfigRedacted> = {}): AppConfigRedacted {
  return {
    listen: { web: 9000, video: 9001, daemonMcp: 9004, forward: 9005, bastion: 2222 },
    agentPort: 4096,
    dataDir: "/data",
    staticDir: "",
    cloneSocket: "/srv/rmng-sock/clones.sock",
    setupComplete: true,
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
    claude: { pollSecs: BigInt(600), pinnedEmail: "alex@example.com" },
    codex: { pollSecs: BigInt(600), pinnedEmail: null, usagePolling: true, autoReset: false },
    cloneGroups: [{ name: "pooled", accounts: ["alex@example.com"] }],
    codexGroups: [],
    presets: [
      {
        name: "webapp",
        labels: ["WE", "frontend"],
        linearKey: "lin_api_fixture",
        claudeAccount: "group:pooled",
        codexAccount: "",
        vars: [{ key: "NODE_ENV", value: "development" }],
        agentPlaybook: "",
        globalPrompt: "",
      },
    ],
    chroma: "yuv420",
    ssh: { authorizedKeys: ["ssh-ed25519 AAAA me@laptop"], publicHost: "rmng.example.com" },
    agentPlaybook: "playbook",
    globalPrompt: "prompt",
    judge: { codexModel: "gpt-5.6-luna", codexEmail: null },
    ...overrides,
  };
}

/** The patch, narrowed to the shape the tests read. `settingsPatch` returns `unknown` because
 *  it is a request body, not a value this app consumes. */
type Patch = {
  docker: { subnet?: string; hostnamePrefix: string };
  claude: { pollSecs: number; pinnedEmail: string | null };
  codex: { pinnedEmail: string | null };
  cloneGroups: { name: string; accounts: string[] }[];
  codexGroups: { name: string; accounts: string[] }[];
  layoutPresets: {
    name: string;
    monitors: { width: number; height: number; x: number; y: number; primary: boolean }[];
  }[];
  presets: {
    name: string;
    labels: string[];
    linearKey: string;
    claudeAccount: string;
    vars: { key: string; value: string }[];
  }[];
  judge: { codexModel: string; codexEmail: string | null };
};

const patch = (draft: ReturnType<typeof settingsDraftFrom>, setupComplete = true) =>
  settingsPatch(draft, setupComplete) as Patch;

// --- seeding the form ---------------------------------------------------------------------

test("a rig with no layout preset is given one to edit", () => {
  // An empty list would leave the operator with nothing to type into, and no way to make the
  // first arrangement.
  const draft = settingsDraftFrom(config({ layoutPresets: [] }));

  expect(draft.layoutPresets).toEqual([
    { name: "Default", monitors: [{ width: 1920, height: 1080, x: 0, y: 0, primary: true }] },
  ]);
});

test("a preset's labels become the comma-separated string the operator types", () => {
  expect(settingsDraftFrom(config()).presets[0].labels).toBe("WE, frontend");
});

test("the stored Linear key never reaches the form, only whether there is one", () => {
  // It is write-only: the server sends a boolean, and a blank input means "keep it".
  const preset = settingsDraftFrom(config()).presets[0];

  expect(preset.linearKey).toBe("");
  expect(preset.keySet).toBe(true);
});

test("a null pinned email becomes a blank field rather than the string null", () => {
  expect(settingsDraftFrom(config()).codex.pinnedEmail).toBe("");
});

test("poll intervals arrive as bigint and are edited as numbers", () => {
  expect(settingsDraftFrom(config()).claude.pollSecs).toBe(600);
});

test("the form never shares an array with the config it was seeded from", () => {
  // Both sides would otherwise be the same object, and a save that re-seeds from the server's
  // answer would be comparing a value against itself.
  const c = config();
  const draft = settingsDraftFrom(c);

  expect(draft.claudeGroups[0].accounts).not.toBe(c.cloneGroups[0].accounts);
  expect(draft.presets[0].vars[0]).not.toBe(c.presets[0].vars[0]);
  expect(draft.layoutPresets[0].monitors[0]).not.toBe(c.layoutPresets[0].monitors[0]);
  expect(draft.listen).not.toBe(c.listen);
});

// --- what a save sends --------------------------------------------------------------------

test("the subnet is only sent before first-run setup finishes", () => {
  // It is baked into the rmng bridge and every clone's static IP, so afterwards the server
  // rejects a change anyway. Omitted rather than sent unchanged.
  const draft = settingsDraftFrom(config());

  expect(patch(draft, true).docker.subnet).toBeUndefined();
  expect(patch(draft, false).docker.subnet).toBe("10.99.0.0/24");
});

test("a half-typed pool is dropped rather than saved unnamed", () => {
  const draft = settingsDraftFrom(config());
  draft.claudeGroups = [
    { name: "  pooled  ", accounts: ["alex@example.com"] },
    { name: "   ", accounts: ["sam@example.com"] },
  ];

  expect(patch(draft).cloneGroups).toEqual([
    { name: "pooled", accounts: ["alex@example.com"] },
  ]);
});

test("repeated pool members are deduped", () => {
  // The checkbox editor cannot produce a duplicate, but a hand-edited config can, and a
  // repeated email would skew group selection.
  const draft = settingsDraftFrom(config());
  draft.codexGroups = [
    { name: "team", accounts: ["a@x.com", "a@x.com", "b@x.com"] },
  ];

  expect(patch(draft).codexGroups).toEqual([{ name: "team", accounts: ["a@x.com", "b@x.com"] }]);
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

test("an env var with no key is dropped, and the key is trimmed", () => {
  const draft = settingsDraftFrom(config());
  draft.presets = [
    {
      ...draft.presets[0],
      vars: [
        { key: "  NODE_ENV  ", value: " development " },
        { key: "   ", value: "orphan" },
      ],
    },
  ];

  // Only the key is trimmed: a value's leading space can be meaningful.
  expect(patch(draft).presets[0].vars).toEqual([{ key: "NODE_ENV", value: " development " }]);
});

test("a blank Linear key is sent as-is, meaning keep the stored one", () => {
  expect(patch(settingsDraftFrom(config())).presets[0].linearKey).toBe("");
});

test("a blank account default is sent as-is, because blank is a real answer", () => {
  // Unlike the key, blank here means "no default — let the clone decide", which is different
  // from pinning the preset to `auto`.
  const draft = settingsDraftFrom(config());
  draft.presets = [{ ...draft.presets[0], claudeAccount: "" }];

  expect(patch(draft).presets[0].claudeAccount).toBe("");
});

test("a blank pinned email is sent as null rather than an empty string", () => {
  const draft = settingsDraftFrom(config());
  draft.claude = { ...draft.claude, pinnedEmail: "" };

  expect(patch(draft).claude.pinnedEmail).toBeNull();
  expect(patch(draft).codex.pinnedEmail).toBeNull();
});

test("an unnamed layout preset is dropped and negative geometry is clamped", () => {
  const draft = settingsDraftFrom(config());
  draft.layoutPresets = [
    { name: "  Dual  ", monitors: [{ width: 0, height: -5, x: -100, y: -1, primary: true }] },
    { name: "", monitors: [{ width: 1920, height: 1080, x: 0, y: 0, primary: false }] },
  ];

  const saved = patch(draft).layoutPresets;
  expect(saved).toHaveLength(1);
  expect(saved[0].name).toBe("Dual");
  expect(saved[0].monitors[0]).toEqual({ width: 1, height: 1, x: 0, y: 0, primary: true });
});

test("the cosmetic account order is never part of the patch", () => {
  // The pool is unordered as far as the server is concerned, so the order stays in the
  // browser. Nothing in the patch names it.
  expect(Object.keys(patch(settingsDraftFrom(config())))).not.toContain("acctOrder");
});

// --- splitting the account list -----------------------------------------------------------

const account = (email: string, provider?: "claude" | "codex"): ClaudeUsage => ({
  id: `${provider ?? "claude"}|${email}`,
  email,
  provider,
  active: true,
  assignable: true,
  lastUpdated: 0,
});

test("a row with no provider counts as Claude", () => {
  // `provider` was added to the row after the fact, so an older row has none.
  const rows = orderedAccounts([account("legacy"), account("new", "claude")], {});

  expect(rows.claude.map((a) => a.email)).toEqual(["legacy", "new"]);
  expect(rows.codex).toEqual([]);
});

test("each provider's list follows its own saved order", () => {
  const accounts = [
    account("a@x.com", "claude"),
    account("b@x.com", "claude"),
    account("c@x.com", "codex"),
    account("d@x.com", "codex"),
  ];

  const rows = orderedAccounts(accounts, {
    claude: ["claude|b@x.com", "claude|a@x.com"],
    codex: ["codex|d@x.com", "codex|c@x.com"],
  });

  expect(rows.claude.map((a) => a.email)).toEqual(["b@x.com", "a@x.com"]);
  expect(rows.codex.map((a) => a.email)).toEqual(["d@x.com", "c@x.com"]);
});

test("a freshly imported account lands after the ordered ones, not first", () => {
  const accounts = [account("fresh@x.com", "claude"), account("a@x.com", "claude")];

  const rows = orderedAccounts(accounts, { claude: ["claude|a@x.com"] });

  expect(rows.claude.map((a) => a.email)).toEqual(["a@x.com", "fresh@x.com"]);
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
