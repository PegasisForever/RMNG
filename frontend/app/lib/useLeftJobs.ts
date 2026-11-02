// The jobs of dialogs closed before their job ended. A dialog that starts a job (a clone, a
// rebase, a Linear ticket, an account swap) can be closed while it runs, with Close or Escape:
// the job is the server's, or a request already sent, so it runs on. The dialog hands it here
// as it goes, and a job that then fails says so where the operator still looks: the control
// panel's error line (`onError`). A job that ends well says nothing more; the board shows its
// result.
//
// Two kinds: a request (its promise rejects on a failure), and an operation (the POST answers
// with an `Operation`, and the SSE op list says how it ends). An operation is followed the way
// `./useOperation.ts` follows a dialog's own: an op that was seen and is gone from the list
// ended well, as finished ops are pruned seconds after they settle.
import { useEffect, useRef, useState } from "react";

import type { Operation } from "./types";

/** A job a dialog leaves running, and what it is called on the error line. */
export type LeftJob = { label: string } & ({ request: Promise<unknown> } | { operation: Promise<Operation> });

/** Hand a dialog's running job to the page. */
export type LeaveJob = (job: LeftJob) => void;

/** An operation followed: its id, its label, and whether the op list had it once. */
export interface Followed {
  id: string;
  label: string;
  seen: boolean;
}

/** One frame of the op list over the followed ops: the ones still followed, and the error
 *  lines of the ones that failed. */
export function stepFollowed(followed: Followed[], operations: Operation[]): { followed: Followed[]; errors: string[] } {
  const errors: string[] = [];
  const next: Followed[] = [];
  for (const f of followed) {
    const op = operations.find((o) => o.id === f.id);
    if (op?.status === "error") {
      errors.push(lateError(f.label, op.message));
      continue;
    }
    if (op?.status === "done" || (!op && f.seen)) continue;
    next.push(op && !f.seen ? { ...f, seen: true } : f);
  }
  return { followed: next, errors };
}

const lateError = (label: string, message: string | undefined) => `${label}: ${message || "it failed"}`;

/** Follow the jobs left by closed dialogs over the live op list; a failure goes to `onError`. */
export function useLeftJobs(operations: Operation[], onError: (message: string) => void): LeaveJob {
  const errorRef = useRef(onError);
  errorRef.current = onError;
  const [followed, setFollowed] = useState<Followed[]>([]);

  useEffect(() => {
    if (followed.length === 0) return;
    const step = stepFollowed(followed, operations);
    for (const e of step.errors) errorRef.current(e);
    if (step.followed.length !== followed.length || step.followed.some((f, i) => f !== followed[i])) {
      setFollowed(step.followed);
    }
  }, [followed, operations]);

  const leave = useRef<LeaveJob>((job) => {
    const fail = (e: Error) => errorRef.current(lateError(job.label, e.message));
    if ("request" in job) {
      job.request.catch(fail);
      return;
    }
    job.operation.then((op) => setFollowed((list) => [...list, { id: op.id, label: job.label, seen: false }]), fail);
  });
  return leave.current;
}
