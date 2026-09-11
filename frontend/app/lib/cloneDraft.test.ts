import { expect, test } from "bun:test";

import {
  cloneDialogBusy,
  cloneDialogReducer,
  cloneDialogValid,
  cloneRequest,
  emptyCloneDialog,
  linearKeyMissing,
  presetOf,
  teamKeysOf,
  type CloneDialog,
  type CloneDialogEvent,
  type CloneDraft,
} from "./cloneDraft";
import type { Operation } from "~/lib/types";
import type { PresetRedacted } from "~/lib/wire/PresetRedacted";

const preset = (over: Partial<PresetRedacted>): PresetRedacted => ({
  name: "work",
  labels: ["WE", "DEV"],
  linearKey: "lin_api_fixture",
  group: "pooled",
  defaultForkClone: "",
  agentPlaybook: "",
  globalPrompt: "",
  startupScript: "",
  dockerfile: "FROM pegasis0/rmng-template:latest",
  ...over,
});

const presets = [
  preset({}),
  preset({ name: "side", labels: ["AW"], linearKey: "", group: "" }),
  preset({ name: "bare", labels: [], linearKey: "", group: "" }),
];

/** A dialog with the config already in, then whatever the events say. */
function dialogOf(
  ps: PresetRedacted[],
  ...events: CloneDialogEvent[]
): CloneDialog {
  const config: CloneDialogEvent = { type: "config", presets: ps, groups: [] };
  return [config, ...events].reduce(cloneDialogReducer, emptyCloneDialog());
}
const dialog = (...events: CloneDialogEvent[]) => dialogOf(presets, ...events);

const edit = <K extends keyof CloneDraft>(key: K, value: CloneDraft[K]) =>
  ({ type: "edit", key, value }) as CloneDialogEvent;
const sources = (...ids: string[]): CloneDialogEvent => ({
  type: "sources",
  ids,
});
const op = (status: Operation["status"], message = ""): Operation => ({
  id: "op1",
  kind: "clone",
  target: "pega-we-142",
  status,
  step: "start",
  pct: 55,
  message,
  log: [],
  startedAt: 0,
});

test("each tab resolves its own preset", () => {
  expect(presetOf(dialog(edit("mode", "plain")))?.name).toBe("work");
  expect(
    presetOf(dialog(edit("mode", "create"), edit("team", "aw")))?.name,
  ).toBe("side");
  // Picking a team IS picking a preset, and a ticket prefix picks one on its own.
  expect(presetOf(dialog(edit("ticket", "we-142")))?.name).toBe("work");
  expect(presetOf(dialog(edit("ticket", "nonsense")))).toBeUndefined();
});

test("every label becomes a team key, and a shared key goes to the first preset", () => {
  expect(teamKeysOf(presets).map((t) => t.key)).toEqual(["we", "dev", "aw"]);
  const shadowed = [...presets, preset({ name: "late", labels: ["WE"] })];
  expect(teamKeysOf(shadowed).find((t) => t.key === "we")?.preset.name).toBe(
    "work",
  );
});

test("the hand-picked tabs open on the first preset, the new-ticket tab on its first team", () => {
  expect(dialog(edit("mode", "plain")).draft.plainPreset).toBe("work");
  expect(dialog(edit("mode", "template")).draft.templatePreset).toBe("work");
  expect(dialog(edit("mode", "create")).draft.team).toBe("we");
});

test("the fork source follows the preset's default clone, else the oldest", () => {
  const ticket = edit("ticket", "WE-142");
  expect(dialog(sources("a", "b"), ticket).draft.source).toBe("a");
  const pinned = [preset({ defaultForkClone: "b" }), ...presets.slice(1)];
  expect(dialogOf(pinned, sources("a", "b"), ticket).draft.source).toBe("b");
  // A default naming no forkable clone falls back to the oldest.
  const stale = [preset({ defaultForkClone: "gone" }), ...presets.slice(1)];
  expect(dialogOf(stale, sources("a", "b"), ticket).draft.source).toBe("a");
  // Blank until a preset resolves: nothing is forked from a guess.
  expect(dialog(sources("a", "b")).draft.source).toBeNull();
});

test("a source picked by hand sticks until it stops being forkable", () => {
  const picked = dialog(
    sources("a", "b"),
    edit("ticket", "WE-142"),
    edit("source", "b"),
  );
  expect(picked.draft.source).toBe("b");
  expect(cloneDialogReducer(picked, sources("a", "b")).draft.source).toBe("b");
  // "b" was deleted or archived: the pick goes back to following the preset.
  expect(cloneDialogReducer(picked, sources("a")).draft.source).toBe("a");
});

test("the pool and both accounts follow the preset until touched", () => {
  const s = dialog(edit("mode", "plain"));
  expect([s.draft.group, s.draft.claudeAccount, s.draft.codexAccount]).toEqual([
    "pooled",
    "auto",
    "auto",
  ]);
  // A preset naming no pool reads as every pool, which is what `none` sends.
  expect(
    dialog(edit("mode", "plain"), edit("plainPreset", "side")).draft.group,
  ).toBe("none");
  const byHand = cloneDialogReducer(s, edit("group", "other"));
  expect(
    cloneDialogReducer(byHand, edit("plainPreset", "side")).draft.group,
  ).toBe("other");
});

test("with nothing to fork, the template tab is the only one left", () => {
  expect(dialog(sources()).draft.mode).toBe("template");
});

test("a missing Linear key blocks the tabs that need one", () => {
  // `create` opens the issue with the resolved preset's own key, so another's is no help.
  expect(
    linearKeyMissing(dialog(edit("mode", "create"), edit("team", "aw"))),
  ).toBe(true);
  expect(
    linearKeyMissing(dialog(edit("mode", "create"), edit("team", "we"))),
  ).toBe(false);
  // `existing` only looks one up, and every configured key is tried in turn.
  expect(linearKeyMissing(dialog(edit("ticket", "WE-142")))).toBe(false);
  expect(linearKeyMissing(dialogOf([presets[1]], edit("ticket", "AW-1")))).toBe(
    true,
  );
  // Before the config lands there is nothing to warn about.
  expect(linearKeyMissing(emptyCloneDialog("WE-142"))).toBe(false);
});

test("the Create button waits for what the open tab needs", () => {
  expect(
    cloneDialogValid(dialog(sources("pega-we-142"), edit("ticket", "WE-142"))),
  ).toBe(true);
  // A prefix no preset claims is a request the server would refuse.
  expect(
    cloneDialogValid(dialog(sources("pega-we-142"), edit("ticket", "ZZ-1"))),
  ).toBe(false);
  const newTicket = dialog(
    sources("pega-we-142"),
    edit("mode", "create"),
    edit("team", "we"),
  );
  expect(cloneDialogValid(newTicket)).toBe(false);
  expect(
    cloneDialogValid(cloneDialogReducer(newTicket, edit("title", "x"))),
  ).toBe(true);
  // Only the template tab may go without a source clone.
  expect(
    cloneDialogValid(dialog(edit("mode", "plain"), edit("title", "x"))),
  ).toBe(false);
  expect(
    cloneDialogValid(dialog(edit("mode", "template"), edit("title", "x"))),
  ).toBe(true);
});

test("the dialog follows its operation and closes only when it settles", () => {
  const started = dialog(
    { type: "starting" },
    { type: "started", opId: "op1" },
  );
  expect(cloneDialogBusy(started)).toBe(true);
  // Between the POST and the first frame the op is not in the list yet.
  expect(cloneDialogReducer(started, { type: "op", op: undefined }).done).toBe(
    false,
  );
  const running = cloneDialogReducer(started, {
    type: "op",
    op: op("running"),
  });
  expect(running.done).toBe(false);
  expect(cloneDialogReducer(running, { type: "op", op: op("done") }).done).toBe(
    true,
  );
  // Finished ops are pruned seconds later, so one that vanished counts as done.
  expect(cloneDialogReducer(running, { type: "op", op: undefined }).done).toBe(
    true,
  );
  // A failed op stays failed when it is pruned, rather than closing over its own message.
  const failed = cloneDialogReducer(running, {
    type: "op",
    op: op("error", "no such preset"),
  });
  expect(failed.error).toBe("no such preset");
  const pruned = cloneDialogReducer(failed, { type: "op", op: undefined });
  expect([pruned.done, cloneDialogBusy(pruned)]).toEqual([false, false]);
});

test("the request carries the open tab's own fields", () => {
  const plain = cloneRequest(
    dialog(
      sources("pega-we-142"),
      edit("mode", "plain"),
      edit("title", "scratch"),
      edit("message", "go"),
      edit("rebuild", true),
    ),
  );
  expect(plain).toMatchObject({
    source: "pega-we-142",
    preset: "work",
    group: "pooled",
    claudeAccount: "auto",
    linear: { displayName: "scratch" },
    firstMessage: "go",
    rebuild: true,
    runStartupScript: true,
    headless: false,
  });
  // The template tab forks nothing.
  expect(
    cloneRequest(dialog(edit("mode", "template"), edit("title", "x"))).source,
  ).toBeUndefined();
  // A ticket tab sends Linear's own answer instead of the typed title.
  const ticket = cloneRequest(
    dialog(
      sources("pega-we-142"),
      edit("ticket", "WE-142"),
      edit("agentInstructions", " read the notes "),
    ),
    { ticket: "WE-142", displayName: "Encoder drops frames" },
  );
  expect(ticket.linear).toEqual({
    ticket: "WE-142",
    displayName: "Encoder drops frames",
  });
  expect(ticket.agentInstructions).toBe("read the notes");
  // Instruction overrides belong to the ticket tabs only.
  expect(
    cloneRequest(
      dialog(
        sources("pega-we-142"),
        edit("mode", "plain"),
        edit("title", "x"),
        edit("agentInstructions", "nope"),
      ),
    ).agentInstructions,
  ).toBeUndefined();
});
