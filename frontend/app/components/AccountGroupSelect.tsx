// An account-group picker shared by the clone modal, the per-clone change control, and
// the preset editor. Under the group-proxy model a clone binds exactly one pool (a
// CLIProxyAPI instance) and always has one — the server guarantees at least one group
// exists (`config::normalize_groups` seeds "Default"), so there is no "none" option.
// CLIProxyAPI owns intra-group account selection + failover — the operator only picks
// the pool.
import type { Group } from "~/lib/wire/Group";

export function AccountGroupSelect({
  groups,
  value,
  onChange,
  className,
  blankLabel,
}: {
  /** Available account groups (from `config.groups`); never empty in practice. */
  groups: Group[];
  /** The selected group name. */
  value: string;
  onChange: (value: string) => void;
  className?: string;
  /**
   * Adds a leading `""` option with this label. Only for the clone dialog, where the
   * control is an *override*: blank means "let the server resolve it from the preset",
   * which is a different thing from the retired "None" (bind no group at all — no longer
   * expressible, since every clone binds one).
   */
  blankLabel?: string;
}) {
  return (
    <select value={value} onChange={(e) => onChange(e.target.value)} className={className}>
      {blankLabel !== undefined ? <option value="">{blankLabel}</option> : null}
      {groups.map((g) => (
        <option key={g.name} value={g.name}>
          {g.name}
        </option>
      ))}
    </select>
  );
}
