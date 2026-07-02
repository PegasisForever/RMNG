// Change a clone's Claude account/group after creation. Mirrors the clone modal's
// picker (auto / account / group); binding to a group enrolls the clone in rotation.
import { useEffect, useState } from "react";

import { AccountGroupSelect } from "~/components/AccountGroupSelect";
import { getConfig } from "~/lib/api";
import type { ClaudeUsage, Host } from "~/lib/types";
import type { CloneGroup } from "~/lib/wire/CloneGroup";

/** Current selection for a host: the verbatim selection when recorded ("auto", "none",
 *  `group:<name>`, or an email), else derived from its group/account for legacy hosts. */
function currentValue(host: Host): string {
  if (host.claudeSelection) return host.claudeSelection;
  if (host.claudeGroup) return `group:${host.claudeGroup}`;
  return host.claudeAccountEmail ?? "auto";
}

export function ChangeAccountModal({
  host,
  accounts,
  busy,
  onClose,
  onSubmit,
}: {
  host: Host;
  /** Assignable accounts (imported Claude accounts). */
  accounts: ClaudeUsage[];
  busy: boolean;
  onClose: () => void;
  onSubmit: (value: string) => void;
}) {
  const [value, setValue] = useState(() => currentValue(host));
  const [groups, setGroups] = useState<CloneGroup[]>([]);

  useEffect(() => {
    getConfig()
      .then((c) => setGroups(c.cloneGroups))
      .catch(() => {
        // Config unreachable — only accounts (no group options).
      });
  }, []);

  return (
    <div
      className="fixed inset-0 z-50 flex items-center justify-center bg-slate-900/30 p-4"
      onClick={onClose}
    >
      <div
        className="w-full max-w-md rounded-xl border border-slate-200 bg-white p-5 shadow-xl"
        onClick={(e) => e.stopPropagation()}
        onKeyDown={(e) => {
          if (e.key === "Escape") onClose();
        }}
      >
        <h3 className="text-sm font-semibold text-slate-900">
          Claude account · <span className="text-emerald-700">{host.displayName ?? host.id}</span>
        </h3>
        <p className="mt-1 text-xs text-slate-500">
          Pick a single account, a group to rotate within every 10 minutes, or “none”
          to remove this clone’s token.
        </p>

        <label className="mt-4 block text-xs font-medium text-slate-600">
          Account or group
          <AccountGroupSelect
            groups={groups}
            accounts={accounts}
            value={value}
            onChange={setValue}
            className="mt-1 w-full rounded-md border border-slate-300 px-3 py-2 text-sm font-normal text-slate-900 focus:border-emerald-500 focus:outline-none"
          />
        </label>

        <div className="mt-4 flex justify-end gap-2">
          <button
            type="button"
            onClick={onClose}
            className="rounded-md px-3 py-1.5 text-sm text-slate-600 hover:bg-slate-100"
          >
            Cancel
          </button>
          <button
            type="button"
            onClick={() => onSubmit(value)}
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
