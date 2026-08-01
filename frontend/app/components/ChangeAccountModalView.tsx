// Change a clone's Claude and Codex account/group after creation, markup half. Mirrors the
// clone dialog's picker (auto / account / group); binding to a group lets the server move the
// clone to another member account when its current one exhausts (sticky otherwise).
//
// Controlled: the container seeds both selections from what the clone is bound to now, and
// owns the swap calls. Nothing here reads the server, so each combination the operator can
// pick is a story.
import { AccountGroupSelect } from "~/components/AccountGroupSelect";
import type { ClaudeUsage } from "~/lib/types";
import { useModalEscape } from "~/lib/useModalEscape";
import type { CloneGroup } from "~/lib/wire/CloneGroup";

const select =
  "mt-1 w-full rounded-md border border-slate-300 px-3 py-2 text-sm font-normal text-slate-900 focus:border-emerald-500 focus:outline-none dark:border-slate-600 dark:bg-slate-800 dark:text-slate-100";

export function ChangeAccountModalView({
  cloneName,
  accounts,
  groups,
  codexAccounts,
  codexGroups,
  claudeValue,
  codexValue,
  busy,
  onClaudeValueChange,
  onCodexValueChange,
  onClose,
  onSubmit,
}: {
  /** The clone this is about, as the heading names it (display name, else id). */
  cloneName: string;
  /** Assignable Claude accounts. */
  accounts: ClaudeUsage[];
  /** Configured Claude pools (`config.cloneGroups`). */
  groups: CloneGroup[];
  /** Assignable Codex accounts. */
  codexAccounts: ClaudeUsage[];
  /** Configured Codex pools (`config.codexGroups`). */
  codexGroups: CloneGroup[];
  /** "auto", "none", an email, or `group:<name>`. */
  claudeValue: string;
  codexValue: string;
  /** A swap is in flight. */
  busy: boolean;
  onClaudeValueChange: (value: string) => void;
  onCodexValueChange: (value: string) => void;
  onClose: () => void;
  onSubmit: () => void;
}) {
  // The Codex picker only shows when Codex accounts/groups are configured; the title
  // reflects both providers only when both are actually changeable here.
  const showCodex = codexAccounts.length > 0 || codexGroups.length > 0;

  // Escape closes regardless of focus — a document-level listener since the backdrop click no
  // longer does (see below), and since nothing here autofocuses: on open the focus is still on
  // the ⋯-menu item that launched this, so a React `onKeyDown` on the panel would never fire.
  // Stacked so a modal opened on top of this one owns Escape instead of both closing at once.
  useModalEscape(onClose);

  return (
    <div className="fixed inset-0 z-50 flex items-center justify-center bg-slate-900/30 p-4">
      {/* Backdrop is inert — clicking it must not close the dialog, only Cancel/Escape do. */}
      <div className="w-full max-w-md rounded-xl border border-slate-200 bg-white p-5 shadow-xl dark:border-slate-700 dark:bg-slate-800">
        <h3 className="text-sm font-semibold text-slate-900 dark:text-slate-100">
          {showCodex ? "Accounts" : "Claude account"} · <span className="text-emerald-700 dark:text-emerald-400">{cloneName}</span>
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
            value={claudeValue}
            onChange={onClaudeValueChange}
            className={select}
          />
        </label>

        {showCodex ? (
          <label className="mt-3 block text-xs font-medium text-slate-600 dark:text-slate-300">
            Codex account
            <AccountGroupSelect
              groups={codexGroups}
              accounts={codexAccounts}
              value={codexValue}
              onChange={onCodexValueChange}
              className={select}
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
            onClick={onSubmit}
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
