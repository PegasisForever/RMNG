// Step 1 of the first-run wizard: is this host able to run clones at all, and which private
// subnet do they get.
//
// The environment preflight arrives as a slot rather than as rows, because the probe behind it
// is a fetch and its verdict is what gates the wizard's Next button. The container passes
// `EnvChecklistContainer`; a story passes `EnvChecklistView` with fixture rows.
import type { ReactNode } from "react";

import { Field, settingsInput } from "~/components/SettingsFields";
import { subnetOk } from "~/lib/setupDraft";

/** The amber "cannot be changed after setup" callout for one-time fields. */
export function OneTimeWarning({ children }: { children: ReactNode }) {
  return (
    <div className="rounded border border-amber-300 dark:border-amber-900 bg-amber-50 dark:bg-amber-950/40 px-3 py-2 text-xs text-amber-800 dark:text-amber-400">
      {children}
    </div>
  );
}

export function SetupEnvironmentStep({
  subnet,
  envChecklist,
  onSubnetChange,
}: {
  /** The clone network's CIDR, as typed. The red hint below reads it through the same rule
   *  the Next button is gated on. */
  subnet: string;
  /** The environment preflight. A slot, because the probe is a fetch and this step is not
   *  allowed to run one. */
  envChecklist: ReactNode;
  onSubnetChange: (value: string) => void;
}) {
  return (
    <div className="space-y-4">
      <p className="text-sm text-slate-600 dark:text-slate-300">
        rmng drives your local Docker daemon over its unix socket. Confirm the environment
        is ready, then pick the private subnet for the clone network.
      </p>
      {envChecklist}

      <OneTimeWarning>
        The clone network subnet is baked into the <code>rmng</code> bridge and every
        clone's static IP at first-run setup — it{" "}
        <strong>cannot be changed after setup</strong>.
      </OneTimeWarning>
      <Field label="Clone network subnet (IPv4 CIDR, /16–/24)">
        <input
          value={subnet}
          onChange={(e) => onSubnetChange(e.target.value)}
          placeholder="10.99.0.0/24"
          spellCheck={false}
          className={settingsInput}
        />
        {subnet.trim() && !subnetOk(subnet) ? (
          <span className="mt-1 block text-[11px] text-red-600 dark:text-red-400">
            must be an IPv4 CIDR with a /16–/24 prefix, e.g. 10.99.0.0/24
          </span>
        ) : (
          <span className="mt-0.5 block text-xs text-slate-400 dark:text-slate-500">
            <code>.1</code> gateway, <code>.2</code> control-server, <code>.10+</code>{" "}
            clone pool.
          </span>
        )}
      </Field>
    </div>
  );
}
