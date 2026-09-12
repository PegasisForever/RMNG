// One side's account picker, shared by the clone modal and the per-clone change control.
// Value is one of: "" (follow the clone's group binding), "auto" (rotate — inside the
// clone's group when bound, fleet-wide otherwise), or an account email (a pin — any
// imported account, even outside the clone's pool). There is no tokenless option: a side
// with no pin and no provider members in scope simply gets no token. Group binding lives
// in its own single picker: a clone binds at most one pool, which feeds both sides.
import { DropdownSelect } from "~/components/DropdownSelect";
import type { DropdownRow } from "~/components/DropdownSelect";
import type { ClaudeUsage } from "~/lib/types";

/** "me@pegasis.site — 5h 12% · 7d 40%" (usage suffix only when known). */
export function accountLabel(a: ClaudeUsage): string {
  const bits: string[] = [];
  if (a.fiveHour) bits.push(`5h ${a.fiveHour.pct}%`);
  if (a.sevenDay) bits.push(`7d ${a.sevenDay.pct}%`);
  return bits.length ? `${a.email} — ${bits.join(" · ")}` : a.email;
}

export function AccountGroupSelect({
  accounts,
  value,
  onChange,
  className,
  blankLabel,
  showAuto = true,
  flat = false,
  label,
}: {
  /** Assignable accounts (imported accounts of this picker's provider). */
  accounts: ClaudeUsage[];
  value: string;
  onChange: (value: string) => void;
  className?: string;
  /** When set, adds a leading empty option with this label — for callers where "no value" is a
   *  meaningful state. The clone dialog's pickers are the case: blank means "follow the
   *  group binding".
   *  Omitted ⇒ no empty option, so the control always holds a concrete selection. */
  blankLabel?: string;
  /** Show the "auto" rotate option. The change control needs it; the clone dialog
   *  follows the group instead, so it hides it. */
  showAuto?: boolean;
  /** Render the accounts flat instead of under an "Accounts" header. The clone
   *  dialog wants a short list; the change control keeps the header. */
  flat?: boolean;
  /** Accessibility name (the wrapping caption is not programmatically linked). */
  label?: string;
}) {
  const rows: DropdownRow[] = [
    ...(blankLabel ? [{ value: "", label: blankLabel }] : []),
    ...(showAuto ? [{ value: "auto", label: "Auto (all accounts)" }] : []),
    ...(accounts.length > 0 && !flat ? [{ header: "Accounts" }] : []),
    ...accounts.map((a) => ({
      value: a.email,
      label: accountLabel(a),
    })),
  ];
  return (
    <DropdownSelect
      rows={rows}
      value={value}
      onChange={onChange}
      label={label}
      className={className}
    />
  );
}
