import { expect, test } from "bun:test";

import { opPhase, resolvePreset } from "./CloneModal";
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
  { name: "work", labels: ["WE", "DEV"], linearKeySet: true, vars: [], agentPlaybook: "", globalPrompt: "" },
  { name: "side", labels: ["AW"], linearKeySet: false, vars: [], agentPlaybook: "", globalPrompt: "" },
  { name: "bare", labels: [], linearKeySet: false, vars: [], agentPlaybook: "", globalPrompt: "" },
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
