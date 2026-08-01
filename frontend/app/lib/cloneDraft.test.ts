import { expect, test } from "bun:test";

import {
  cloneDraftValid,
  emptyCloneDraft,
  linearKeyMissing,
  opPhase,
  resolvePreset,
  teamKeysOf,
  type CloneDraft,
} from "./cloneDraft";
import type { Operation } from "~/lib/types";
import type { PresetRedacted } from "~/lib/wire/PresetRedacted";

const op = (status: Operation["status"]): Operation => ({
  id: "op1",
  kind: "clone",
  target: "pega-we-142",
  status,
  step: "start",
  pct: 55,
  message: "starting the container",
  log: [],
  startedAt: 0,
});

test("a running op keeps the dialog open", () => {
  expect(opPhase(op("running"), true, false)).toBe("running");
});

test("the dialog waits for the FIRST frame rather than closing immediately", () => {
  // Between the POST returning and the next SSE frame, the op isn't in the list yet. Closing
  // here would defeat the whole point of the progress bar.
  expect(opPhase(undefined, false, false)).toBe("running");
});

test("a done op closes the dialog", () => {
  expect(opPhase(op("done"), true, false)).toBe("done");
});

test("an op that vanished after being seen counts as done", () => {
  // Finished ops are pruned from state ~8s after they settle, so a missed terminal frame
  // must not strand the dialog open.
  expect(opPhase(undefined, true, false)).toBe("done");
});

test("an error keeps the dialog open with its message", () => {
  expect(opPhase(op("error"), true, false)).toBe("failed");
});

test("a pruned FAILED op stays failed rather than reading as done", () => {
  // The sticky flag exists for exactly this: without it, the vanish-means-done rule would
  // fire 60s later and close the dialog over its own error message.
  expect(opPhase(undefined, true, true)).toBe("failed");
});

// --- preset resolution (mirrors the server, per tab) ---------------------------------

const presets: PresetRedacted[] = [
  { name: "work", labels: ["WE", "DEV"], linearKeySet: true, claudeAccount: "group:pooled", codexAccount: "", vars: [], agentPlaybook: "", globalPrompt: "" },
  { name: "side", labels: ["AW"], linearKeySet: false, claudeAccount: "", codexAccount: "", vars: [], agentPlaybook: "", globalPrompt: "" },
  { name: "bare", labels: [], linearKeySet: false, claudeAccount: "", codexAccount: "", vars: [], agentPlaybook: "", globalPrompt: "" },
];

test("the no-ticket tab uses the hand-picked preset", () => {
  expect(resolvePreset("plain", presets, { plainPreset: "side" })?.name).toBe("side");
});

test("the new-ticket tab derives the preset from the team key", () => {
  // Picking a team IS picking a preset — which is why that tab has no preset dropdown.
  expect(resolvePreset("create", presets, { team: "aw" })?.name).toBe("side");
  expect(resolvePreset("create", presets, { team: "dev" })?.name).toBe("work");
});

test("the existing-ticket tab auto-selects by the ticket prefix, case-insensitively", () => {
  expect(resolvePreset("existing", presets, { ticketPrefix: "we" })?.name).toBe("work");
});

test("nothing resolves until there is something to resolve from", () => {
  // Both ticket tabs read undefined before input, which is what leaves the resolved-preset
  // line blank rather than naming a preset the clone might not get.
  expect(resolvePreset("existing", presets, {})).toBeUndefined();
  expect(resolvePreset("create", presets, {})).toBeUndefined();
});

test("a prefix no preset claims resolves to nothing", () => {
  // The server would 400 listing the configured presets; the dialog says so up front.
  expect(resolvePreset("existing", presets, { ticketPrefix: "zzz" })).toBeUndefined();
});

test("a preset with no labels never auto-matches", () => {
  // Matches `pick_preset_by_prefix`: an unlabelled preset is opt-in only.
  expect(resolvePreset("existing", presets, { ticketPrefix: "" })).toBeUndefined();
  expect(resolvePreset("create", presets, { team: "" })).toBeUndefined();
});

// --- team keys (the New-ticket tab's dropdown, which is also its preset selector) --------

test("every label becomes a team key, lowercased", () => {
  expect(teamKeysOf(presets).map((t) => t.key)).toEqual(["we", "dev", "aw"]);
});

test("a key claimed by two presets goes to the first in config order", () => {
  // Mirrors `pick_preset_by_prefix`. Picking the team has to pick the same preset the server
  // would, or the dialog names one preset and the clone gets another.
  const shadowed: PresetRedacted[] = [
    ...presets,
    { name: "late", labels: ["WE"], linearKeySet: true, claudeAccount: "", codexAccount: "", vars: [], agentPlaybook: "", globalPrompt: "" },
  ];

  expect(teamKeysOf(shadowed).find((t) => t.key === "we")?.preset.name).toBe("work");
  expect(teamKeysOf(shadowed)).toHaveLength(3);
});

test("an unlabelled preset contributes no team key", () => {
  // `bare` has no labels, so the dropdown cannot offer it and nothing auto-selects it.
  expect(teamKeysOf(presets).map((t) => t.preset.name)).not.toContain("bare");
});

test("no presets means no team keys rather than a crash", () => {
  expect(teamKeysOf([])).toEqual([]);
});

// --- the missing-Linear-key rule (mirrors the server, per tab) ---------------------------

const work = presets[0];
const side = presets[1];

test("the warning stays down until the config has landed", () => {
  // `presets` is empty for one round trip, which is indistinguishable from "none configured".
  // Without the gate the warning flashes on every open.
  expect(linearKeyMissing("existing", [], undefined, false)).toBe(false);
  expect(linearKeyMissing("create", [], undefined, false)).toBe(false);
});

test("the no-ticket tab never needs a Linear key", () => {
  expect(linearKeyMissing("plain", [], undefined, true)).toBe(false);
});

test("creating a ticket needs the RESOLVED preset's own key", () => {
  // `resolve_issue` opens the issue with one preset's key, so another preset having one is
  // no help.
  expect(linearKeyMissing("create", presets, work, true)).toBe(false);
  expect(linearKeyMissing("create", presets, side, true)).toBe(true);
  expect(linearKeyMissing("create", presets, undefined, true)).toBe(true);
});

test("looking a ticket up needs ANY preset's key", () => {
  // `fetch_issue_any` tries every configured key in turn, so the resolved preset is beside
  // the point — including when nothing has resolved yet.
  expect(linearKeyMissing("existing", presets, undefined, true)).toBe(false);
  expect(linearKeyMissing("existing", [side], side, true)).toBe(true);
  expect(linearKeyMissing("existing", [], undefined, true)).toBe(true);
});

// --- whether the Clone button may fire ---------------------------------------------------

const draft = (overrides: Partial<CloneDraft> = {}): CloneDraft => ({
  ...emptyCloneDraft(),
  image: "pegasis0/rmng-template:latest",
  ...overrides,
});

const check = (
  d: CloneDraft,
  overrides: Partial<{
    presets: PresetRedacted[];
    preset: PresetRedacted | undefined;
    ticketParsed: boolean;
    keyMissing: boolean;
  }> = {},
) =>
  cloneDraftValid(d, {
    presets,
    preset: undefined,
    ticketParsed: false,
    keyMissing: false,
    ...overrides,
  });

test("no source image blocks every tab", () => {
  // The image is the one field shared by all three requests, and the picker can be empty.
  expect(check(draft({ image: null }), { ticketParsed: true, preset: work })).toBe(false);
  expect(check(draft({ image: null, mode: "create", team: "we", title: "x" }))).toBe(false);
  expect(check(draft({ image: null, mode: "plain", title: "x", plainPreset: "work" }))).toBe(false);
});

test("an existing ticket needs both a parse and a preset that claims its prefix", () => {
  // With the preset dropdown gone there is no way to override the auto-selection, so a prefix
  // nothing claims is a request the server would 400.
  expect(check(draft({ ticket: "WE-142" }), { ticketParsed: true, preset: work })).toBe(true);
  expect(check(draft({ ticket: "WE-142" }), { ticketParsed: true, preset: undefined })).toBe(false);
  expect(check(draft({ ticket: "nonsense" }), { ticketParsed: false, preset: work })).toBe(false);
});

test("with no presets configured at all, a parseable ticket is enough", () => {
  expect(check(draft({ ticket: "WE-142" }), { presets: [], ticketParsed: true })).toBe(true);
});

test("a new ticket needs a team key and a title", () => {
  expect(check(draft({ mode: "create", team: "we", title: "Tighten the row" }))).toBe(true);
  expect(check(draft({ mode: "create", team: "we", title: "   " }))).toBe(false);
  expect(check(draft({ mode: "create", team: "", title: "Tighten the row" }))).toBe(false);
});

test("a no-ticket clone needs a title, and a preset whenever any are configured", () => {
  expect(check(draft({ mode: "plain", title: "scratch", plainPreset: "work" }))).toBe(true);
  expect(check(draft({ mode: "plain", title: "scratch", plainPreset: "" }))).toBe(false);
  expect(check(draft({ mode: "plain", title: "", plainPreset: "work" }))).toBe(false);
  // Nothing configured, so there is no preset to pick and the title carries the form.
  expect(check(draft({ mode: "plain", title: "scratch" }), { presets: [] })).toBe(true);
});

test("a missing Linear key blocks an otherwise complete form", () => {
  // The warning and the dead button are the same rule seen twice, which is why the button
  // cannot go live while the message is up.
  expect(
    check(draft({ mode: "create", team: "aw", title: "Encoder spike" }), {
      preset: side,
      keyMissing: true,
    }),
  ).toBe(false);
  expect(check(draft({ ticket: "WE-142" }), { ticketParsed: true, preset: work, keyMissing: true })).toBe(
    false,
  );
});
