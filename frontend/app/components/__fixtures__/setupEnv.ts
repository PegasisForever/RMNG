// The setup wizard's environment preflight rows, as `GET /api/setup/env` returns them.
//
// The states here need a broken host to reach in the real app, which is the whole reason they
// are fixtures: a required check that fails blocks setup, an advisory one only warns, and
// telling those two apart on screen is the checklist's job.

import type { EnvCheckRow } from "~/lib/wire/EnvCheckRow";

export function makeEnvCheckRow(overrides: Partial<EnvCheckRow> = {}): EnvCheckRow {
  return {
    id: "dockerSocket",
    label: "Docker socket reachable",
    ok: true,
    detail: "",
    required: true,
    ...overrides,
  };
}

/** A host that passes everything, which is what the operator should normally see. */
export function makeEnvRows(): EnvCheckRow[] {
  return [
    makeEnvCheckRow({
      id: "dockerSocket",
      label: "Docker socket reachable",
      detail: "Engine 27.1.1 at /var/run/docker.sock",
    }),
    makeEnvCheckRow({
      id: "renderNode",
      label: "Render node present",
      detail: "/dev/dri/renderD128",
    }),
    makeEnvCheckRow({
      id: "cloneSocketMount",
      label: "Clone socket directory mounted",
      detail: "/srv/rmng-sock",
    }),
    makeEnvCheckRow({
      id: "kvm",
      label: "Nested virtualization",
      ok: true,
      detail: "/dev/kvm",
      required: false,
    }),
  ];
}
