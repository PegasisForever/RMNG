// Change a clone's Claude and Codex account/group after creation (Codex picker shows
// only when Codex accounts/groups are configured). Mirrors the clone modal's picker
// (auto / account / group); binding to a group lets the server move the clone to
// another member account when its current one exhausts (sticky otherwise).
import { useEffect, useState } from "react";

import { AccountGroupSelect } from "~/components/AccountGroupSelect";
import { getConfig } from "~/lib/api";
import type { ClaudeUsage, Clone } from "~/lib/types";
import type { CloneGroup } from "~/lib/wire/CloneGroup";

/** Current selection for a clone: the verbatim selection when recorded ("auto", "none",
 *  `group:<name>`, or an email), else derived from its group/account for legacy clones.
 *  A legacy clone with no account is effectively tokenless, so showing "none" lets
 *  choosing "auto" submit the swap that enrolls it in rotation. */
export function currentValue(clone: Clone): string {
  if (clone.claudeSelection) return clone.claudeSelection;
  if (clone.claudeGroup) return `group:${clone.claudeGroup}`;
  return clone.claudeAccountEmail ?? "none";
}

export function currentCodexValue(clone: Clone): string {
  if (clone.codexSelection) return clone.codexSelection;
  if (clone.codexGroup) return `group:${clone.codexGroup}`;
  return clone.codexAccountEmail ?? "none";
}

export function ChangeAccountModal({
  clone,
  accounts,
  codexAccounts,
  busy,
  onClose,
  onSubmit,
}: {
  clone: Clone;
  /** Assignable accounts (imported Claude accounts). */
  accounts: ClaudeUsage[];
  /** Assignable Codex accounts. */
  codexAccounts: ClaudeUsage[];
  busy: boolean;
  onClose: () => void;
  onSubmit: (claude: string, codex: string) => void;
}) {
  const [value, setValue] = useState(() => currentValue(clone));
  const [groups, setGroups] = useState<CloneGroup[]>([]);
  const [codexValue, setCodexValue] = useState(() => currentCodexValue(clone));
  const [codexGroups, setCodexGroups] = useState<CloneGroup[]>([]);

  useEffect(() => {
    getConfig()
      .then((c) => {
        setGroups(c.cloneGroups);
        setCodexGroups(c.codexGroups);
      })
      .catch(() => {
        // Config unreachable — only accounts (no group options).
      });
  }, []);

  // The Codex picker only shows when Codex accounts/groups are configured; the title
  // reflects both providers only when both are actually changeable here.
  const showCodex = codexAccounts.length > 0 || codexGroups.length > 0;

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
          {showCodex ? "Accounts" : "Claude account"} · <span className="text-emerald-700 dark:text-emerald-400">{clone.displayName ?? clone.id}</span>
        </h3>
        <p className="mt-1 text-xs text-slate-500 dark:text-slate-400">
          Pick a single account, a group (stays on one account until it exhausts,
          then swaps to the least-used member), or “none” to remove this clone’s token.
        </p>

        <label className="mt-4 block text-xs font-medium text-slate-600 dark:text-slate-300">
          Claude account
          <AccountGroupSelect
            groups={groups}
            accounts={accounts}
            value={value}
            onChange={setValue}
            className="mt-1 w-full rounded-md border border-slate-300 px-3 py-2 text-sm font-normal text-slate-900 focus:border-emerald-500 focus:outline-none dark:border-slate-600 dark:bg-slate-800 dark:text-slate-100"
          />
        </label>

        {showCodex ? (
          <label className="mt-3 block text-xs font-medium text-slate-600 dark:text-slate-300">
            Codex account
            <AccountGroupSelect
              groups={codexGroups}
              accounts={codexAccounts}
              value={codexValue}
              onChange={setCodexValue}
              className="mt-1 w-full rounded-md border border-slate-300 px-3 py-2 text-sm font-normal text-slate-900 focus:border-emerald-500 focus:outline-none dark:border-slate-600 dark:bg-slate-800 dark:text-slate-100"
            />
          </label>
        ) : null}

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
            onClick={() => onSubmit(value, codexValue)}
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
