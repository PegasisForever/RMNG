// Change a clone's group + per-side accounts after creation, markup half. One group
// picker feeds both sides; each side then picks auto (rotate inside the group when bound,
// fleet-wide otherwise), a pinned email, or none. Binding to a group lets the server move
// the clone to another member account when its current one exhausts (sticky otherwise).
//
// Controlled: the container seeds the group + both selections from what the clone is bound
// to now, and owns the swap calls. Nothing here reads the server, so each combination the
// operator can pick is a story.
import { AccountGroupSelect } from "~/components/AccountGroupSelect";
import { DropdownSelect } from "~/components/DropdownSelect";
import { ModalShell } from "~/components/ModalShell";
import type { ClaudeUsage } from "~/lib/types";
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
  /** "auto" (rotate in scope) or an email (pin). */
  claudeValue: string;
  codexValue: string;
  /** A swap is in flight. */
  busy: boolean;
  onGroupChange: (value: string | null) => void;
  onClaudeValueChange: (value: string) => void;
  onCodexValueChange: (value: string) => void;
  /** The dialog is finished: unmount it. The exit frames have already played. */
  onClose: () => void;
  /** Apply the two picks. The dialog plays its exit first, so this runs on a dialog the
   *  operator can no longer see — the page owns the swap and its error banner from here. */
  onSubmit: () => void;
}) {
  // The Codex picker only shows when Codex accounts exist or a pool could carry Codex
  // members; the title reflects both providers only when both are actually changeable.
  const showCodex = codexAccounts.length > 0 || groups.length > 0;

  return (
    <ModalShell size="sm" onExited={onClose}>
      {(close) => (
        <>
          <h3 className="text-sm font-semibold text-slate-900 dark:text-slate-100">
            {showCodex ? "Accounts" : "Claude account"} ·{" "}
            <span className="text-emerald-700 dark:text-emerald-400">
              {cloneName}
            </span>
          </h3>
          <p className="mt-1 text-xs text-slate-500 dark:text-slate-400">
            Pick a pool (both sides draw from it), then per side an account:
            auto rotates inside the pool, a single account pins it — even one
            outside the pool.
          </p>

          <label className="mt-4 block text-xs font-medium text-slate-600 dark:text-slate-300">
            Group
            <DropdownSelect
              rows={[
                { value: "", label: "Any group (all pools)" },
                ...groups.map((g) => ({
                  value: g.name,
                  label: `${g.name} (${g.accounts.length})`,
                })),
              ]}
              value={groupValue ?? ""}
              onChange={(value) => onGroupChange(value || null)}
              label="Group"
              className={select}
            />
          </label>

          <label className="mt-3 block text-xs font-medium text-slate-600 dark:text-slate-300">
            Claude account
            <AccountGroupSelect
              accounts={accounts}
              value={claudeValue}
              onChange={onClaudeValueChange}
              label="Claude account"
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
                label="Codex account"
                className={select}
              />
            </label>
          ) : null}

          <div className="mt-4 flex justify-end gap-2">
            <button
              type="button"
              onClick={() => close()}
              className="rounded-md px-3 py-1.5 text-sm text-slate-600 hover:bg-slate-100 dark:text-slate-300 dark:hover:bg-slate-800"
            >
              Cancel
            </button>
            {/* Apply closes too, so the exit frames run in front of the swap rather than
              after it: `close(onSubmit)` plays them and hands off. */}
            <button
              type="button"
              onClick={() => close(onSubmit)}
              disabled={busy}
              className="rounded-md bg-emerald-600 px-4 py-1.5 text-sm font-medium text-white hover:bg-emerald-700 disabled:opacity-40"
            >
              {busy ? "Applying…" : "Apply"}
            </button>
          </div>
        </>
      )}
    </ModalShell>
  );
}
