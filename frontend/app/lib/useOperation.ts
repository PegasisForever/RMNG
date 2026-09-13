// One long server operation, followed from the POST that starts it to the frame that settles
// it. Starting one answers with an `Operation`; the id it carries is matched against the
// SSE-pushed op list until the op lands, fails, or is pruned.
//
// Three dialogs each carried their own copy of that: the clone dialog as reducer cases, the
// rebase dialog as five `useState` calls plus two effects, and the settings panel as a lone
// op id with no settle detection at all — its Update button stayed dead after a failed
// update, because nothing there could tell a failure from a pull still running. The rules
// live here now, and the copies are gone.
//
// The rules, in one place:
//   1. `run()` clears the last attempt before it starts, so a retry follows the NEW op.
//   2. An op that vanished after being seen counts as done (`opPhase` says why).
//   3. A failure is sticky, so a dialog never closes over its own error message.
//
// The rules are pure functions and the hook is a thin reading of them, so a test reaches
// every rule without a renderer.
import { useEffect, useReducer } from "react";

import type { Operation } from "~/lib/types";

/** Where an attempt stands. `idle` is before the first click and after a cleared one; the
 *  other three are what a dialog acts on. */
export type OpPhase = "idle" | "running" | "done" | "failed";

/** What has to be remembered between frames. Everything else about an attempt is read off
 *  the live op list, so there is no second copy of it to go stale. */
export interface OpAttempt {
  /** The started operation's id, once the POST answers. */
  opId: string | null;
  /** The POST is in flight: an op is coming but has no id yet. */
  starting: boolean;
  /** The op has stood in the list at least once. The list cannot be asked this later. */
  seen: boolean;
  /** The op reported an error. Sticky — see `opPhase`. */
  failed: boolean;
  /** The failed attempt, in the dialog's own words. */
  error: string | null;
}

export type OpEvent =
  /** The operator asked for it: clear the last attempt and start. */
  | { type: "start" }
  /** The POST answered with the driving operation. */
  | { type: "started"; opId: string }
  /** The POST itself failed, so there is no operation to follow. */
  | { type: "failed"; message: string }
  /** A frame of the op list, carrying this attempt's op or nothing. `failureLabel` names
   *  the work for an op that failed without saying why. */
  | { type: "op"; op: Operation | undefined; failureLabel?: string };

/** No attempt made. */
export const NO_ATTEMPT: OpAttempt = {
  opId: null,
  starting: false,
  seen: false,
  failed: false,
  error: null,
};

export function stepOperation(s: OpAttempt, e: OpEvent): OpAttempt {
  switch (e.type) {
    case "start":
      // Clear the last attempt so a retry tracks the new op, not the failed one still
      // sitting in the list for another minute.
      return { ...NO_ATTEMPT, starting: true };
    case "started":
      return { ...s, starting: false, opId: e.opId };
    case "failed":
      return { ...s, starting: false, error: e.message };
    case "op": {
      const seen = s.seen || !!e.op;
      const failed = s.failed || e.op?.status === "error";
      // The same state back when the frame said nothing new: the op list arrives afresh on
      // every SSE frame, and an attempt that is merely still running must not re-render.
      if (seen === s.seen && failed === s.failed) return s;
      return {
        ...s,
        seen,
        failed,
        error:
          failed && !s.failed
            ? e.op?.message || (e.failureLabel ?? "the operation failed")
            : s.error,
      };
    }
  }
}

/**
 * Where the attempt stands, given the op as this frame of the list has it.
 *
 * Finished operations are pruned from state shortly after they settle (8s after Done, 60s
 * after Error), so a poll can miss the terminal frame: **an op that vanished after being
 * seen counts as done**, the same rule the CLI's waiter uses. `failed` is sticky, because
 * that rule would otherwise close a dialog over its own error message.
 */
export function opPhase(s: OpAttempt, op: Operation | undefined): OpPhase {
  if (s.failed || op?.status === "error") return "failed";
  if (s.starting) return "running";
  // No op id and not starting: either nothing was ever asked for, or the POST itself failed.
  if (!s.opId) return s.error ? "failed" : "idle";
  if (op?.status === "done") return "done";
  if (!op && s.seen) return "done";
  return "running";
}

/** The form and both buttons lock. Raised from the first click and released only by a
 *  failure: an attempt that settled well leaves it raised, because the dialog that started
 *  it is already on its way out and must not unlock its form for the length of the exit
 *  frames. A failure is the one state the operator retries from, so it alone unlocks. */
export function opBusy(phase: OpPhase): boolean {
  return phase === "running" || phase === "done";
}

/** What the hook hands back. `op` is the live row, for whatever renders its progress. */
export interface OperationAttempt {
  op: Operation | undefined;
  phase: OpPhase;
  busy: boolean;
  /** The attempt settled well and there is nothing left to read. This is what a dialog
   *  inverts into `ModalShell`'s `open`, so the frame plays its exit and then unmounts. */
  done: boolean;
  error: string | null;
  /** Start, or start over. Ignored while an attempt is busy; what makes an attempt worth
   *  starting at all (a valid form) stays the caller's own question. */
  run: () => void;
}

/**
 * Follow one operation of one's own.
 *
 * `operations` is the live list from the SSE state. `start` is the POST, called afresh on
 * every `run`, so it may read whatever the form says at the moment of the click.
 * `failureLabel` names the work in the message an op that failed silently leaves behind.
 */
export function useOperation(
  operations: Operation[],
  start: () => Promise<Operation>,
  { failureLabel }: { failureLabel?: string } = {},
): OperationAttempt {
  const [attempt, dispatch] = useReducer(stepOperation, NO_ATTEMPT);
  const op = attempt.opId
    ? operations.find((o) => o.id === attempt.opId)
    : undefined;
  const phase = opPhase(attempt, op);

  // The one thing a frame has to leave behind: that this op stood in the list, and whether
  // it failed while it did. Both outlive the row itself, which is pruned seconds later.
  useEffect(() => {
    if (attempt.opId) dispatch({ type: "op", op, failureLabel });
  }, [attempt.opId, op, failureLabel]);

  function run() {
    if (opBusy(phase)) return;
    dispatch({ type: "start" });
    start().then(
      (started) => dispatch({ type: "started", opId: started.id }),
      (e: Error) => dispatch({ type: "failed", message: e.message }),
    );
  }

  return {
    op,
    phase,
    busy: opBusy(phase),
    done: phase === "done",
    error: attempt.error,
    run,
  };
}
