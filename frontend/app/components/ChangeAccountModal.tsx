// Change a clone's account-group binding after creation. Under the group-proxy model a
// clone binds exactly one pool (a CLIProxyAPI instance); changing it is a pure map update
// on the control-server — no clone-side change and no restart. CLIProxyAPI owns
// intra-group account selection + failover.
import { useEffect, useState } from "react";

import { AccountGroupSelect } from "~/components/AccountGroupSelect";
import { getConfig } from "~/lib/api";
import type { Clone } from "~/lib/types";
import type { Group } from "~/lib/wire/Group";
import { useModalEscape } from "~/lib/useModalEscape";

/**
 * The clone's current binding as a picker value, falling back to the first configured group.
 * Every clone binds a group — the server repoints blank/dangling bindings at load and on
 * every reconciler pass — but a row can still read blank in the window before that runs, so
 * preselect a real group rather than showing a blank select.
 */
export function currentValue(clone: Clone, groups: Group[]): string {
  return clone.group || groups[0]?.name || "";
}

export function ChangeAccountModal({
  clone,
  busy,
  onClose,
  onSubmit,
}: {
  clone: Clone;
  busy: boolean;
  onClose: () => void;
  /** The new binding: a group name. */
  onSubmit: (group: string) => void;
}) {
  const [groups, setGroups] = useState<Group[]>([]);
  const [value, setValue] = useState(() => clone.group);

  useEffect(() => {
    getConfig()
      .then((c) => {
        setGroups(c.groups);
        // A blank binding has nothing selected until the list arrives; land on the first
        // real group so Apply can't submit an empty name.
        setValue((v) => v || currentValue(clone, c.groups));
      })
      .catch(() => {
        // Config unreachable — the select stays empty and Apply is disabled below.
      });
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  // Escape closes regardless of focus — a document-level listener since the backdrop
  // click no longer does (see below). Stacked so a modal opened on top of this one owns
  // Escape instead of both closing at once.
  useModalEscape(onClose);

  return (
    <div className="fixed inset-0 z-50 flex items-center justify-center bg-slate-900/30 p-4">
      {/* Backdrop is inert — clicking it must not close the dialog, only Cancel/Escape do. */}
      <div className="w-full max-w-md rounded-xl border border-slate-200 bg-white p-5 shadow-xl dark:border-slate-700 dark:bg-slate-800">
        <h3 className="text-sm font-semibold text-slate-900 dark:text-slate-100">
          Account group ·{" "}
          <span className="text-emerald-700 dark:text-emerald-400">{clone.displayName ?? clone.id}</span>
        </h3>
        <p className="mt-1 text-xs text-slate-500 dark:text-slate-400">
          Bind this clone to an account pool. The change is a routing update — no clone
          restart, and it takes effect on the next request.
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
            onClick={() => onSubmit(value)}
            disabled={busy || !value}
            className="rounded-md bg-emerald-600 px-4 py-1.5 text-sm font-medium text-white hover:bg-emerald-700 disabled:opacity-40"
          >
            {busy ? "Applying…" : "Apply"}
          </button>
        </div>
      </div>
    </div>
  );
}
