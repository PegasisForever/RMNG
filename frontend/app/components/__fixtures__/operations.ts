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

/** A template pull, as the setup wizard follows it. Its `target` IS the pulled reference,
 *  which is how the wizard finds this op among the live ones. Override `status` and `pct` for
 *  the finished state. */
export function makePullOperation(overrides: Partial<Operation> = {}): Operation {
  return makeOperation({
    id: "op-pull-1",
    kind: "pull",
    target: "pegasis0/rmng-template:latest",
    status: "running",
    step: "download",
    pct: 62,
    message: "Downloading layers…",
    log: [
      "pulling pegasis0/rmng-template:latest",
      "6f2c1a3b: downloading 2.1 GB / 6.8 GB",
    ],
    ...overrides,
  });
}

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
