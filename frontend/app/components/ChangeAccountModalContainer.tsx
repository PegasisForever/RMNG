// Change a clone's Claude and Codex account/group after creation, network half.
//
// The config read that supplies the pool options lives here. What the clone is bound to right
// now seeds the pickers, read by `~/lib/accountBinding` as the card reads it. The swap itself
// belongs to the page, which owns the busy flag and the error banner, so it stays a callback.
// The markup is ChangeAccountModalView.
import { useEffect, useState } from "react";

import { ChangeAccountModalView } from "~/components/ChangeAccountModalView";
import { accountBinding, clonePool } from "~/lib/accountBinding";
import { getConfig } from "~/lib/api";
import type { ClaudeUsage, Clone } from "~/lib/types";
import type { CloneGroup } from "~/lib/wire/CloneGroup";

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
    const [claudeValue, setClaudeValue] = useState(
        () => accountBinding(clone, "claude").selection,
    );
    const [codexValue, setCodexValue] = useState(
        () => accountBinding(clone, "codex").selection,
    );
    const [groupValue, setGroupValue] = useState<string | null>(() =>
        clonePool(clone),
    );
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
