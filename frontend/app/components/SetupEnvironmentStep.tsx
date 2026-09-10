// Step 1 of the first-run wizard: is this host able to run clones at all.
//
// The environment preflight arrives as a slot rather than as rows, because the probe behind it
// is a fetch and its verdict is what gates the wizard's Next button. The container passes
// `EnvChecklistContainer`; a story passes `EnvChecklistView` with fixture rows.
//
// (The clone-network subnet used to be picked here; it is hardcoded on the server now, so
// this step only confirms the environment is ready.)
import type { ReactNode } from "react";

export function SetupEnvironmentStep({
  envChecklist,
}: {
  /** The environment preflight. A slot, because the probe is a fetch and this step is not
   *  allowed to run one. */
  envChecklist: ReactNode;
}) {
  return (
    <div className="space-y-4">
      <p className="text-sm text-slate-600 dark:text-slate-300">
        rmng drives your local Docker daemon over its unix socket. Confirm the
        environment is ready, then continue.
      </p>
      {envChecklist}
      <p className="text-xs text-slate-400 dark:text-slate-500">
        Clones get static IPs on the <code>rmng</code> bridge (
        <code>10.99.0.0/24</code> — <code>.1</code> gateway, <code>.2</code>{" "}
        control-server, <code>.10+</code> clone pool).
      </p>
    </div>
  );
}
