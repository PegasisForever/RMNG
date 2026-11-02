// Change a clone's group + per-side accounts after creation, markup half. One group
// picker feeds both sides; each side then picks auto (rotate inside the group when bound,
// fleet-wide otherwise), a pinned email, or none. Binding to a group lets the server move
// the clone to another member account when its current one exhausts (sticky otherwise).
//
// Controlled: the container seeds the group + both selections from what the clone is bound
// to now, and owns the swap calls, their busy flag and their error. Nothing here reads the
// server, so each combination the operator can pick, and a refused swap, is a story.
import { AccountGroupSelect } from "~/components/AccountGroupSelect";
import { DropdownSelect } from "~/components/DropdownSelect";
import { ModalShell } from "~/components/ModalShell";
import { BUTTON, DONE_BUTTON, ErrorLine, FIELD, R_PIECE, TEXT, TEXT_2 } from "~/desk/control/ui";
import type { ClaudeUsage } from "~/lib/types";
import type { CloneGroup } from "~/lib/wire/CloneGroup";

/** A picker's trigger: the shared field (`~/desk/GLASS.md`, Flat content). */
const select = `mt-1 w-full ${FIELD} px-3 py-2 text-sm font-normal`;
/** A field's caption. */
const caption = `block text-xs font-medium ${TEXT_2}`;

export function ChangeAccountModalView({
  cloneName,
  accounts,
  groups,
  codexAccounts,
  groupValue,
  claudeValue,
  codexValue,
  busy,
  error,
  onGroupChange,
  onClaudeValueChange,
  onCodexValueChange,
  onSubmit,
  open = true,
  onClose,
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
  /** A swap is in flight. Locks Apply, Cancel and Escape. */
  busy: boolean;
  /** The refused swap, in the dialog's own words rather than the control panel's error line. */
  error: string | null;
  onGroupChange: (value: string | null) => void;
  onClaudeValueChange: (value: string) => void;
  onCodexValueChange: (value: string) => void;
  /** Apply the two picks. The dialog stays open, holding its error if the swap is refused. */
  onSubmit: () => void;
  /** The dialog is on screen. The container flips it false when the swap is done, and the
   *  dialog plays its exit before the unmount rather than blinking out. */
  open?: boolean;
  /** The dialog is finished: unmount it. The exit frames have already played. */
  onClose: () => void;
}) {
  // The Codex picker only shows when Codex accounts exist or a pool could carry Codex
  // members; the title reflects both providers only when both are actually changeable.
  const showCodex = codexAccounts.length > 0 || groups.length > 0;

  return (
    // Neither Cancel nor Escape may close over a swap in flight, so both are held by the
    // same `busy`: the buttons below, and `dismissible` for the key.
    <ModalShell size="sm" dismissible={!busy} open={open} onExited={onClose}>
      {(close) => (
        <>
          <h3 className={`text-[15px] font-semibold tracking-tight ${TEXT}`}>
            {showCodex ? "Accounts" : "Claude account"} ·{" "}
            <span className={TEXT_2}>{cloneName}</span>
          </h3>
          <p className={`mt-1 text-xs ${TEXT_2}`}>
            Pick a pool (both sides draw from it), then per side an account:
            auto rotates inside the pool, a single account pins it — even one
            outside the pool.
          </p>

          <label className={`mt-4 ${caption}`}>
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

          <label className={`mt-3 ${caption}`}>
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
            <label className={`mt-3 ${caption}`}>
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

          {error ? (
            <div className={`mt-3 overflow-hidden ${R_PIECE}`}>
              <ErrorLine message={error} />
            </div>
          ) : null}

          <div className="mt-4 flex justify-end gap-2">
            <button
              type="button"
              onClick={() => close()}
              disabled={busy}
              className={`${BUTTON} px-3 py-1.5 text-sm disabled:opacity-40`}
            >
              Cancel
            </button>
            <button
              type="button"
              onClick={onSubmit}
              disabled={busy}
              className={`${DONE_BUTTON} px-4 py-1.5 text-sm font-medium`}
            >
              {busy ? "Applying…" : "Apply"}
            </button>
          </div>
        </>
      )}
    </ModalShell>
  );
}
