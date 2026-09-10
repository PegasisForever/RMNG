// Change a clone's Claude and Codex account/group after creation, network half.
//
// Two things live here: the config read that supplies the pool options, and the rule that says
// what the clone is bound to right now, which is what seeds both pickers. The swap itself
// belongs to the page, which owns the busy flag and the error banner, so it stays a callback.
// The markup is ChangeAccountModalView.
import { useEffect, useState } from "react";

import { ChangeAccountModalView } from "~/components/ChangeAccountModalView";
import { getConfig } from "~/lib/api";
import type { ClaudeUsage, Clone } from "~/lib/types";
import type { CloneGroup } from "~/lib/wire/CloneGroup";

/** The clone's pool: the shared binding, else a legacy sticky for a clone the server
 *  has not re-saved yet. */
export function currentGroup(clone: Clone): string | null {
  return clone.group ?? clone.claudeGroup ?? clone.codexGroup ?? null;
}

/** Current side selection ("auto", "none", or an email). A legacy `group:<name>`
 *  selection reads as "auto" — the group half of that binding now lives in
 *  {@link currentGroup}. A legacy clone with no account is effectively tokenless, so
 *  showing "none" lets choosing "auto" submit the swap that enrolls it in rotation. */
export function currentValue(clone: Clone): string {
  const sel = clone.claudeSelection ??
    (clone.claudeGroup ? `group:${clone.claudeGroup}` : undefined);
  if (!sel) return clone.claudeAccountEmail ?? "none";
  return sel.startsWith("group:") ? "auto" : sel;
}

export function currentCodexValue(clone: Clone): string {
  const sel = clone.codexSelection ??
    (clone.codexGroup ? `group:${clone.codexGroup}` : undefined);
  if (!sel) return clone.codexAccountEmail ?? "none";
  return sel.startsWith("group:") ? "auto" : sel;
}

export function ChangeAccountModalContainer({
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
  onSubmit: (claude: string, codex: string, group: string | null) => void;
}) {
  const [claudeValue, setClaudeValue] = useState(() => currentValue(clone));
  const [codexValue, setCodexValue] = useState(() => currentCodexValue(clone));
  const [groupValue, setGroupValue] = useState<string | null>(() => currentGroup(clone));
  const [groups, setGroups] = useState<CloneGroup[]>([]);

  useEffect(() => {
    getConfig()
      .then((c) => {
        setGroups(c.groups);
      })
      .catch(() => {
        // Config unreachable — only accounts (no group options).
      });
  }, []);

  return (
    <ChangeAccountModalView
      cloneName={clone.displayName ?? clone.id}
      accounts={accounts}
      groups={groups}
      codexAccounts={codexAccounts}
      groupValue={groupValue}
      claudeValue={claudeValue}
      codexValue={codexValue}
      busy={busy}
      onGroupChange={setGroupValue}
      onClaudeValueChange={setClaudeValue}
      onCodexValueChange={setCodexValue}
      onClose={onClose}
      onSubmit={() => onSubmit(claudeValue, codexValue, groupValue)}
    />
  );
}
