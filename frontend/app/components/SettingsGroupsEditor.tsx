// A provider's account pools: a name plus a checkbox grid of member emails. One component,
// rendered once per provider, because the two sections are the same editor over two
// independent pools.
//
// Membership lives in the config and is saved through the same `PUT /api/config` as
// everything else on this panel, not a dedicated endpoint, so the whole list is one prop and
// one `onChange` like every other section here.
import { settingsInput } from "~/components/SettingsFields";
import { newGroup, type GroupDraft } from "~/lib/settingsDraft";

export function SettingsGroupsEditor({
  groups,
  accountEmails,
  noAccountsHint,
  onChange,
}: {
  groups: GroupDraft[];
  /** This provider's imported emails, in the operator's saved order. Each becomes one
   *  checkbox in every pool. */
  accountEmails: string[];
  /** What a pool says when there is nothing to put in it. It names the provider, because
   *  importing a Claude account does not help a Codex pool. */
  noAccountsHint: string;
  onChange: (groups: GroupDraft[]) => void;
}) {
  const replace = (i: number, next: Partial<GroupDraft>) =>
    onChange(groups.map((g, j) => (j === i ? { ...g, ...next } : g)));

  return (
    <div className="space-y-3">
      {groups.length === 0 ? (
        <p className="text-xs text-slate-400 dark:text-slate-500">No groups.</p>
      ) : null}
      {groups.map((g, i) => (
        <div key={i} className="rounded border border-slate-200 dark:border-slate-700 p-3">
          <div className="flex items-center gap-2">
            <input
              value={g.name}
              onChange={(e) => replace(i, { name: e.target.value })}
              placeholder="group name"
              className={settingsInput}
            />
            <button
              type="button"
              onClick={() => onChange(groups.filter((_, j) => j !== i))}
              className="shrink-0 rounded px-2 py-1 text-xs text-slate-500 dark:text-slate-400 hover:bg-slate-100 dark:hover:bg-slate-800"
            >
              Remove
            </button>
          </div>
          {accountEmails.length === 0 ? (
            <p className="mt-2 text-xs text-slate-400 dark:text-slate-500">{noAccountsHint}</p>
          ) : (
            <div className="mt-2 flex flex-wrap gap-x-4 gap-y-1.5">
              {accountEmails.map((email) => (
                <label key={email} className="flex items-center gap-1.5 text-xs text-slate-600 dark:text-slate-300">
                  <input
                    type="checkbox"
                    checked={g.accounts.includes(email)}
                    onChange={() =>
                      replace(i, {
                        accounts: g.accounts.includes(email)
                          ? g.accounts.filter((e) => e !== email)
                          : [...g.accounts, email],
                      })
                    }
                  />
                  {email}
                </label>
              ))}
            </div>
          )}
        </div>
      ))}
      <button
        type="button"
        onClick={() => onChange([...groups, newGroup()])}
        className="rounded border border-slate-300 dark:border-slate-600 px-2 py-1 text-xs text-slate-600 dark:text-slate-300 hover:bg-slate-50 dark:hover:bg-slate-800"
      >
        + Add group
      </button>
    </div>
  );
}
