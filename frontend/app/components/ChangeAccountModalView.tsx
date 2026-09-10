// Change a clone's group + per-side accounts after creation, markup half. One group
// picker feeds both sides; each side then picks auto (rotate inside the group when bound,
// fleet-wide otherwise), a pinned email, or none. Binding to a group lets the server move
// the clone to another member account when its current one exhausts (sticky otherwise).
//
// Controlled: the container seeds the group + both selections from what the clone is bound
// to now, and owns the swap calls. Nothing here reads the server, so each combination the
// operator can pick is a story.
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
  groupValue,
  claudeValue,
  codexValue,
  busy,
  onGroupChange,
  onClaudeValueChange,
  onCodexValueChange,
  onClose,
  onSubmit,
}: {
  /** The clone this is about, as the heading names it (display name, else id). */
  cloneName: string;
  /** Assignable Claude accounts. */
  accounts: ClaudeUsage[];
  /** The single configured pool list (`config.groups`) — one binding feeds both sides. */
  groups: CloneGroup[];
  /** Assignable Codex accounts. */
  codexAccounts: ClaudeUsage[];
  /** The bound pool name, or null for no pool. */
  groupValue: string | null;
  /** "auto", "none", or an email. */
  claudeValue: string;
  codexValue: string;
  /** A swap is in flight. */
  busy: boolean;
  onGroupChange: (value: string | null) => void;
  onClaudeValueChange: (value: string) => void;
  onCodexValueChange: (value: string) => void;
  onClose: () => void;
  onSubmit: () => void;
}) {
  // The Codex picker only shows when Codex accounts exist or a pool could carry Codex
  // members; the title reflects both providers only when both are actually changeable.
  const showCodex = codexAccounts.length > 0 || groups.length > 0;

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
          Pick a pool (both sides draw from it), then per side an account: auto rotates
          inside the pool, a single account pins it, “none” removes this clone’s token.
        </p>

        <label className="mt-4 block text-xs font-medium text-slate-600 dark:text-slate-300">
          Group
          <select
            value={groupValue ?? ""}
            onChange={(e) => onGroupChange(e.target.value || null)}
            className={select}
          >
            <option value="">None (no pool)</option>
            {groups.map((g) => (
              <option key={g.name} value={g.name}>
                {g.name} ({g.accounts.length})
              </option>
            ))}
          </select>
        </label>

        <label className="mt-3 block text-xs font-medium text-slate-600 dark:text-slate-300">
          Claude account
          <AccountGroupSelect
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
