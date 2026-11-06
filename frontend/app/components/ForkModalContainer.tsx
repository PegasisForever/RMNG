// The Fork dialog, network half: the config read that supplies the pools, the POST that
// starts the fork (`POST /api/fork`), and the operation it answers with. The dialog stays
// open on that operation and closes when it settles; the operator may close it before, and
// the op then goes to the page (`onLeave`). The form's rules are `~/lib/forkDraft.ts`, the
// markup is ForkModalView.
import { useEffect, useReducer, useState } from "react";

import { ForkModalView } from "~/components/ForkModalView";
import { getConfig } from "~/lib/api";
import { emptyForkDraft, forkParent, forkRequest, forkValid, hasTicket, type ForkDraft } from "~/lib/forkDraft";
import type { ClaudeUsage, Clone, Operation } from "~/lib/types";
import type { LeaveJob } from "~/lib/useLeftJobs";
import { useOperation } from "~/lib/useOperation";
import type { CloneGroup } from "~/lib/wire/CloneGroup";
import type { CloneRequest } from "~/lib/wire/CloneRequest";

type Edit = { [K in keyof ForkDraft]: { key: K; value: ForkDraft[K] } }[keyof ForkDraft];

export function ForkModalContainer({
  clone,
  ticketTitle,
  operations,
  accounts,
  onClose,
  onFork,
  onLeave,
}: {
  /** The clone being copied. */
  clone: Clone;
  /** Its ticket's title: Linear's own when the page has it, else what the clone stored. */
  ticketTitle: string | null;
  /** Live operations from the SSE state: the started fork is followed through these. */
  operations: Operation[];
  /** Imported accounts, both providers. */
  accounts: ClaudeUsage[];
  /** The dialog is finished: unmount it. `left`: it closed while the fork was being made,
   *  which runs on. */
  onClose: (left: boolean) => void;
  /** Starts the fork and resolves with the driving Operation. */
  onFork: (req: CloneRequest) => Promise<Operation>;
  /** Closed while the fork runs: the page follows it (`~/lib/useLeftJobs.ts`). */
  onLeave?: LeaveJob;
}) {
  const [draft, edit] = useReducer(
    (d: ForkDraft, e: Edit) => ({ ...d, [e.key]: e.value }),
    clone,
    emptyForkDraft,
  );
  const [groups, setGroups] = useState<CloneGroup[]>([]);
  useEffect(() => {
    getConfig().then(
      (c) => setGroups(c.groups),
      // Config unreachable: no pool options, the source's pool is kept
      () => {},
    );
  }, []);

  const valid = forkValid(clone, draft);
  // `start` is read afresh on every run, so it sends the form as it stands at the click
  const fork = useOperation(operations, () => onFork(forkRequest(clone, draft)), {
    failureLabel: "the fork failed",
  });

  return (
    <ForkModalView
      cloneId={clone.id}
      ticket={hasTicket(clone) ? { id: clone.linearTicket!, title: ticketTitle } : null}
      parentId={forkParent(clone)}
      draft={draft}
      onDraftChange={(key, value) => edit({ key, value } as Edit)}
      accounts={accounts}
      groups={groups}
      valid={valid}
      busy={fork.busy}
      error={fork.error}
      operation={fork.op ?? null}
      onSubmit={() => {
        if (valid) fork.run();
      }}
      // The settled operation closes the dialog; a FAILED fork is not settled for this
      // purpose, so the dialog stays up with its error
      open={!fork.done}
      onClose={() => {
        // Closed while the fork is being made: it goes on, and the page says if it fails
        const left = fork.phase === "running" && fork.started !== null;
        if (left) onLeave?.({ label: `Fork of ${clone.id}`, operation: fork.started! });
        onClose(left);
      }}
    />
  );
}
