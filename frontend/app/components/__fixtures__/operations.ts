// Long-running server jobs, as the Activity list and the busy row states read them.

import type { Operation } from "~/lib/types";

import { cloneIdle } from "./clones";

export function makeOperation(overrides: Partial<Operation> = {}): Operation {
  return {
    id: "op-1",
    kind: "clone",
    target: "pega-per-9",
    status: "running",
    step: "provision",
    pct: 45,
    message: "Provisioning container…",
    log: ["pulling layers"],
    startedAt: 1_700_000_000_000,
    ...overrides,
  };
}

/** A running clone op (drives the Activity list + disables the + Clone button). */
export const cloneOperation: Operation = makeOperation({
  id: "op-clone-1",
  source: "pegasis0/rmng-template:latest",
  log: ["pulling layers", "creating container", "starting gnome session"],
});

/** A running delete op targeting an existing clone (shows the row's busy state). */
export const deleteOperation: Operation = makeOperation({
  id: "op-delete-1",
  kind: "delete",
  target: cloneIdle.id,
  step: "stopping",
  pct: 30,
  message: "Stopping container…",
  log: ["stopping container"],
});
