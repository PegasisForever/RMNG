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

/** Current selection for a clone: the verbatim selection when recorded ("auto", "none",
 *  `group:<name>`, or an email), else derived from its group/account for legacy clones.
 *  A legacy clone with no account is effectively tokenless, so showing "none" lets
 *  choosing "auto" submit the swap that enrolls it in rotation. */
export function currentValue(clone: Clone): string {
  if (clone.claudeSelection) return clone.claudeSelection;
  if (clone.claudeGroup) return `group:${clone.claudeGroup}`;
  return clone.claudeAccountEmail ?? "none";
}

export function currentCodexValue(clone: Clone): string {
  if (clone.codexSelection) return clone.codexSelection;
  if (clone.codexGroup) return `group:${clone.codexGroup}`;
  return clone.codexAccountEmail ?? "none";
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
  onSubmit: (claude: string, codex: string) => void;
}) {
  const [claudeValue, setClaudeValue] = useState(() => currentValue(clone));
  const [codexValue, setCodexValue] = useState(() => currentCodexValue(clone));
  const [groups, setGroups] = useState<CloneGroup[]>([]);
  const [codexGroups, setCodexGroups] = useState<CloneGroup[]>([]);

  useEffect(() => {
    getConfig()
      .then((c) => {
        setGroups(c.cloneGroups);
        setCodexGroups(c.codexGroups);
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
      codexGroups={codexGroups}
      claudeValue={claudeValue}
      codexValue={codexValue}
      busy={busy}
      onClaudeValueChange={setClaudeValue}
      onCodexValueChange={setCodexValue}
      onClose={onClose}
      onSubmit={() => onSubmit(claudeValue, codexValue)}
    />
  );
}
