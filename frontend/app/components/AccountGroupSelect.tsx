// A Claude-account picker shared by the clone modal and the per-host change control.
// Value is one of: "auto" (server picks the best account), "none" (install no token),
// an account email, or "group:<name>" (binds the clone to a group, which the server
// rotates).
import type { CloneGroup } from "~/lib/wire/CloneGroup";
import type { ClaudeUsage } from "~/lib/types";

/** "me@pegasis.site — 5h 12% · 7d 40%" (usage suffix only when known). */
export function accountLabel(a: ClaudeUsage): string {
  const bits: string[] = [];
  if (a.fiveHour) bits.push(`5h ${a.fiveHour.pct}%`);
  if (a.sevenDay) bits.push(`7d ${a.sevenDay.pct}%`);
  return bits.length ? `${a.email} — ${bits.join(" · ")}` : a.email;
}

export function AccountGroupSelect({
  groups,
  accounts,
  value,
  onChange,
  recommended,
  className,
}: {
  groups: CloneGroup[];
  /** Assignable accounts (imported Claude accounts). */
  accounts: ClaudeUsage[];
  value: string;
  onChange: (value: string) => void;
  recommended?: string | null;
  className?: string;
}) {
  return (
    <select value={value} onChange={(e) => onChange(e.target.value)} className={className}>
      <option value="auto">Auto (best account)</option>
      <option value="none">None (no token)</option>
      {groups.length > 0 ? (
        <optgroup label="Groups">
          {groups.map((g) => (
            <option key={`group:${g.name}`} value={`group:${g.name}`}>
              Group: {g.name} ({g.accounts.length})
            </option>
          ))}
        </optgroup>
      ) : null}
      {accounts.length > 0 ? (
        <optgroup label="Accounts">
          {accounts.map((a) => (
            <option key={a.id} value={a.email}>
              {accountLabel(a)}
              {a.email === recommended ? " · recommended" : ""}
            </option>
          ))}
        </optgroup>
      ) : null}
    </select>
  );
}
