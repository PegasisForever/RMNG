// Change a clone's Claude and Codex account/group after creation, network half.
//
// The pools come from the live state (`ControlState.groups`), as a prop. What the clone is
// bound to right now seeds the pickers, read by `~/lib/accountBinding` as the card reads it.
// The swap is this dialog's own: the two POSTs, the busy flag and the error live here, and
// the dialog stays open on a refused swap, holding its error. It closes only when both sides
// took the change, so the control panel only opens it and is told when it is done
// (`onClose`). The markup is ChangeAccountModalView.
import { useState } from "react";

import { ChangeAccountModalView } from "~/components/ChangeAccountModalView";
import { accountBinding, clonePool } from "~/lib/accountBinding";
import { swapClaudeAccount, swapCodexAccount } from "~/lib/api";
import type { ClaudeUsage, Clone } from "~/lib/types";
import type { CloneGroup } from "~/lib/wire/CloneGroup";

export function ChangeAccountModalContainer({
    clone,
    accounts,
    codexAccounts,
    groups,
    onClose,
}: {
    clone: Clone;
    /** Assignable accounts (imported Claude accounts). */
    accounts: ClaudeUsage[];
    /** Assignable Codex accounts. */
    codexAccounts: ClaudeUsage[];
    /** The pools, from the live state (`ControlState.groups`). */
    groups: CloneGroup[];
    /** The dialog is finished (cancelled, or the swap is done): unmount it. */
    onClose: () => void;
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
    /** The swap is in flight. It stays set through the exit frames after a success. */
    const [busy, setBusy] = useState(false);
    /** The refused swap, in the server's words (`~/lib/api` reads them with `~/lib/serverError`). */
    const [error, setError] = useState<string | null>(null);
    /** Both sides took the change: the dialog plays its exit and unmounts. */
    const [done, setDone] = useState(false);

    /** The Apply button. Both sides go at once, with the picks as they stand at the click. */
    function submit() {
        if (busy) return;
        setBusy(true);
        setError(null);
        Promise.all([
            swapClaudeAccount(clone.id, claudeValue, groupValue),
            swapCodexAccount(clone.id, codexValue, groupValue),
        ]).then(
            () => setDone(true),
            (e: Error) => {
                setBusy(false);
                setError(e.message);
            },
        );
    }

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
            error={error}
            onGroupChange={setGroupValue}
            onClaudeValueChange={setClaudeValue}
            onCodexValueChange={setCodexValue}
            onSubmit={submit}
            // The finished swap is what closes the dialog: `open` goes false, the frame plays
            // its exit, and `onClose` unmounts once the frames have run. A refused swap is not
            // finished — the dialog stays up holding its error.
            open={!done}
            onClose={onClose}
        />
    );
}
