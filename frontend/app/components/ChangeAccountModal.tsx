// Change a clone's account-group binding after creation. Under the group-proxy model a
// clone binds exactly one pool (a CLIProxyAPI instance) or none; changing it is a pure
// map update on the control-server — no clone-side change and no restart. CLIProxyAPI
// owns intra-group account selection + failover.
import { useEffect, useState } from "react";

import { AccountGroupSelect, NO_GROUP } from "~/components/AccountGroupSelect";
import { getConfig } from "~/lib/api";
import type { Host } from "~/lib/types";
import type { Group } from "~/lib/wire/Group";

/** The host's current binding as a picker value: its group name, or "none". */
export function currentValue(host: Host): string {
  return host.group ?? NO_GROUP;
}

export function ChangeAccountModal({
  host,
  busy,
  onClose,
  onSubmit,
}: {
  host: Host;
  busy: boolean;
  onClose: () => void;
  /** The new binding: a group name, or `null` to clear it. */
  onSubmit: (group: string | null) => void;
}) {
  const [value, setValue] = useState(() => currentValue(host));
  const [groups, setGroups] = useState<Group[]>([]);

  useEffect(() => {
    getConfig()
      .then((c) => setGroups(c.groups))
      .catch(() => {
        // Config unreachable — only the current value / "none" are offered.
      });
  }, []);

  return (
    <div
      className="fixed inset-0 z-50 flex items-center justify-center bg-slate-900/30 p-4"
      onClick={onClose}
    >
      <div
        className="w-full max-w-md rounded-xl border border-slate-200 bg-white p-5 shadow-xl dark:border-slate-700 dark:bg-slate-800"
        onClick={(e) => e.stopPropagation()}
        onKeyDown={(e) => {
          if (e.key === "Escape") onClose();
        }}
      >
        <h3 className="text-sm font-semibold text-slate-900 dark:text-slate-100">
          Account group ·{" "}
          <span className="text-emerald-700 dark:text-emerald-400">{host.displayName ?? host.id}</span>
        </h3>
        <p className="mt-1 text-xs text-slate-500 dark:text-slate-400">
          Bind this clone to an account pool, or “none” for no inference. The change is a
          routing update — no clone restart, and it takes effect on the next request.
        </p>

        <label className="mt-4 block text-xs font-medium text-slate-600 dark:text-slate-300">
          Account group
          <AccountGroupSelect
            groups={groups}
            value={value}
            onChange={setValue}
            className="mt-1 w-full rounded-md border border-slate-300 px-3 py-2 text-sm font-normal text-slate-900 focus:border-emerald-500 focus:outline-none dark:border-slate-600 dark:bg-slate-800 dark:text-slate-100"
          />
        </label>

        <div className="mt-4 flex justify-end gap-2">
          <button
            type="button"
            onClick={onClose}
            className="rounded-md px-3 py-1.5 text-sm text-slate-600 hover:bg-slate-100 dark:text-slate-300 dark:hover:bg-slate-800"
          >
            Cancel
          </button>
          <button
            type="button"
            onClick={() => onSubmit(value === NO_GROUP ? null : value)}
            disabled={busy}
            className="rounded-md bg-emerald-600 px-4 py-1.5 text-sm font-medium text-white hover:bg-emerald-700 disabled:opacity-40"
          >
            {busy ? "Applying…" : "Apply"}
          </button>
        </div>
      </div>
    </div>
  );
}
