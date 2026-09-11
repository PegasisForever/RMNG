// One side's account picker, shared by the clone modal and the per-clone change control.
// Value is one of: "" (follow the preset or source), "auto" (rotate — inside the clone's
// group when bound, fleet-wide otherwise), or an account email (a pin — any imported
// account, even outside the clone's pool). There is no tokenless option: a side with no
// pin and no provider members in scope simply gets no token. Group binding lives in its
// own single picker: a clone binds at most one pool, which feeds both sides.
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
}: {
  /** Assignable accounts (imported accounts of this picker's provider). */
  accounts: ClaudeUsage[];
  value: string;
  onChange: (value: string) => void;
  className?: string;
  /** When set, adds a leading empty option with this label — for callers where "no value" is a
   *  meaningful state distinct from `auto`. The clone pickers are the case: blank means "follow
   *  the preset", whereas `auto` is a real choice that overrides it.
   *  Omitted ⇒ no empty option, so the control always holds a concrete selection. */
  blankLabel?: string;
}) {
  return (
    <select
      value={value}
      onChange={(e) => onChange(e.target.value)}
      className={className}
    >
      {blankLabel ? <option value="">{blankLabel}</option> : null}
      <option value="auto">Auto (all accounts)</option>
      {accounts.length > 0 ? (
        <optgroup label="Accounts">
          {accounts.map((a) => (
            <option key={a.id} value={a.email}>
              {accountLabel(a)}
            </option>
          ))}
        </optgroup>
      ) : null}
    </select>
  );
}
