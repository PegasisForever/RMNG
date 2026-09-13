// The operation lifecycle, driven through the real rules the three dialogs now share. These
// cases came off `cloneDraft.test.ts`, where they drove the clone dialog's own copy of the
// machine. The two that matter most are the pruned op and the sticky failure: both are about
// a row that is no longer in the list, which is the one thing the list cannot be asked.
import { expect, test } from "bun:test";

import {
  NO_ATTEMPT,
  opBusy,
  opPhase,
  stepOperation,
  type OpAttempt,
  type OpEvent,
} from "./useOperation";
import type { Operation } from "~/lib/types";

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

const attempt = (...events: OpEvent[]): OpAttempt =>
  events.reduce(stepOperation, NO_ATTEMPT);

/** An attempt with its op already in flight and once seen, which is where most rules bite. */
const running = () =>
  attempt(
    { type: "start" },
    { type: "started", opId: "op1" },
    { type: "op", op: op("running") },
  );

test("nothing has been asked for yet", () => {
  expect(opPhase(NO_ATTEMPT, undefined)).toBe("idle");
  expect(opBusy(opPhase(NO_ATTEMPT, undefined))).toBe(false);
});

test("an attempt follows its operation and settles only when the op does", () => {
  const started = attempt({ type: "start" }, { type: "started", opId: "op1" });
  expect(opBusy(opPhase(started, undefined))).toBe(true);
  // Between the POST and the first frame the op is not in the list yet.
  expect(opPhase(started, undefined)).toBe("running");
  expect(opPhase(running(), op("running"))).toBe("running");
  expect(opPhase(running(), op("done"))).toBe("done");
  // Finished ops are pruned seconds later, so one that vanished counts as done.
  expect(opPhase(running(), undefined)).toBe("done");
});

test("a failed op stays failed when it is pruned", () => {
  const failed = stepOperation(running(), {
    type: "op",
    op: op("error", "no such preset"),
  });
  expect(failed.error).toBe("no such preset");
  // Rather than closing the dialog over its own error message.
  expect(opPhase(failed, undefined)).toBe("failed");
  expect(opBusy(opPhase(failed, undefined))).toBe(false);
});

test("an op that failed silently is named by the caller's label", () => {
  const failed = stepOperation(running(), {
    type: "op",
    op: op("error"),
    failureLabel: "the fork failed",
  });
  expect(failed.error).toBe("the fork failed");
  // The op's own words win when it has any.
  expect(
    stepOperation(running(), {
      type: "op",
      op: op("error", "no space left"),
      failureLabel: "the fork failed",
    }).error,
  ).toBe("no space left");
});

test("a POST that never answers is a failed attempt with no op to follow", () => {
  const dead = attempt({ type: "start" }, { type: "failed", message: "boom" });
  expect([opPhase(dead, undefined), dead.error]).toEqual(["failed", "boom"]);
  expect(opBusy(opPhase(dead, undefined))).toBe(false);
});

test("starting again clears the last attempt", () => {
  const failed = stepOperation(running(), {
    type: "op",
    op: op("error", "no such preset"),
  });
  const retry = stepOperation(failed, { type: "start" });
  // Nothing of the failed attempt survives: not its error, not its sticky failure, and not
  // its op id, which is what keeps the old row (still in the list for another minute) from
  // being read as this attempt's.
  expect(retry).toEqual({ ...NO_ATTEMPT, starting: true });
  expect(opPhase(retry, undefined)).toBe("running");
  expect(
    opPhase(stepOperation(retry, { type: "started", opId: "op2" }), undefined),
  ).toBe("running");
});

test("a frame with nothing new in it is the same state back", () => {
  const s = running();
  expect(stepOperation(s, { type: "op", op: op("running") })).toBe(s);
  expect(stepOperation(s, { type: "op", op: undefined })).toBe(s);
});
