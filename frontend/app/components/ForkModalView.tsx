// The Fork dialog's markup: a copy of one clone, from that clone's ⋮ menu. It renders from
// props alone (no config read, no POST, no operation stream), so every state it can be in
// is a story. ForkModalContainer owns those and hands the results down; the form's rules
// are `~/lib/forkDraft.ts`.
//
// A clone with a ticket: the fork is for the same ticket, shown and not changeable. A clone
// with no ticket: the fork's name. Then the first message, the sub clone box, and under
// "More options" the accounts and the options of the New clone dialog.
import { ChevronDown, ChevronRight } from "lucide-react";
import { useState } from "react";

import { CloneAccountFields } from "~/components/CloneAccountFields";
import { CloneOptionsRow } from "~/components/CloneOptionsRow";
import { ModalShell } from "~/components/ModalShell";
import { OperationProgress } from "~/components/OperationProgress";
import {
  cloneRow,
  cloneRowField,
  cloneRowLabel,
  cloneRowLabelTop,
  cloneRowTop,
  cloneSectionCaption,
} from "~/components/cloneFieldStyles";
import { BUTTON, DONE_BUTTON, ErrorLine, LINE, R_PIECE, TEXT, TEXT_2 } from "~/desk/control/ui";
import type { ForkDraft } from "~/lib/forkDraft";
import type { ClaudeUsage, Operation } from "~/lib/types";
import type { CloneGroup } from "~/lib/wire/CloneGroup";

export interface ForkModalViewProps {
  /** The clone being copied. */
  cloneId: string;
  /** The source's ticket ("WE-142") and its title, which the fork keeps. Null: the source has
   *  no ticket, and the fork needs a name. */
  ticket: { id: string; title: string | null } | null;
  /** The clone a sub clone fork goes under: the source, or the source's own parent. */
  parentId: string;
  draft: ForkDraft;
  onDraftChange: <K extends keyof ForkDraft>(key: K, value: ForkDraft[K]) => void;
  /** Imported accounts, both providers, for the two account pickers. */
  accounts: ClaudeUsage[];
  /** The configured pools (`config.groups`). */
  groups: CloneGroup[];

  /** The Fork button may fire. */
  valid: boolean;
  /** A fork is being started, or one is running. Locks the form and the Fork button. */
  busy: boolean;
  /** The failed attempt, in the dialog's own words. */
  error: string | null;
  /** The fork operation this dialog started, once it shows in the live op list. */
  operation: Operation | null;
  onSubmit: () => void;
  /** The dialog is on screen. The container makes it false when the fork settles, and the
   *  dialog plays its exit before the unmount. */
  open?: boolean;
  /** The dialog is finished: unmount it. The exit frames have already played. */
  onClose: () => void;
  /** Start with "More options" open (stories). */
  initialMore?: boolean;
}

export function ForkModalView({
  cloneId,
  ticket,
  parentId,
  draft,
  onDraftChange,
  accounts,
  groups,
  valid,
  busy,
  error,
  operation,
  onSubmit,
  open = true,
  onClose,
  initialMore = false,
}: ForkModalViewProps) {
  const [more, setMore] = useState(initialMore);
  return (
    // Close and Escape close it at any time: a fork runs on, on the server (the container
    // hands its job to the page)
    <ModalShell size="lg" open={open} onExited={onClose}>
      {(close) => (
        <>
          <h3 className={`shrink-0 text-[15px] font-semibold tracking-tight ${TEXT}`}>Fork {cloneId}</h3>
          <p className={`mt-1 shrink-0 text-xs ${TEXT_2}`}>
            Makes a new clone with a copy of the home of {cloneId}. {cloneId} keeps running.
          </p>

          <div className="mt-4 min-h-0 space-y-3 overflow-y-auto">
            {ticket ? (
              <div className={cloneRow}>
                <span className={cloneRowLabel}>Ticket</span>
                <span className={`min-w-0 truncate text-sm ${TEXT}`}>
                  {ticket.id}
                  {ticket.title ? <span className={TEXT_2}> · {ticket.title}</span> : null}
                </span>
              </div>
            ) : (
              <label className={cloneRow}>
                <span className={cloneRowLabel}>Name</span>
                <input
                  autoFocus
                  value={draft.name}
                  disabled={busy}
                  onChange={(e) => onDraftChange("name", e.target.value)}
                  onKeyDown={(e) => {
                    if (e.key === "Enter") onSubmit();
                  }}
                  placeholder="Name of the new clone"
                  className={cloneRowField}
                />
              </label>
            )}
            <label className={cloneRowTop}>
              <span className={cloneRowLabelTop}>First message to the agent</span>
              <textarea
                autoFocus={!!ticket}
                value={draft.message}
                disabled={busy}
                onChange={(e) => onDraftChange("message", e.target.value)}
                rows={3}
                placeholder="Optional. Empty: the agent does not start."
                className={`resize-y ${cloneRowField}`}
              />
            </label>
            <label className={`flex cursor-pointer items-center gap-2 text-xs font-medium ${TEXT_2}`}>
              <input
                type="checkbox"
                checked={draft.sub}
                disabled={busy}
                onChange={(e) => onDraftChange("sub", e.target.checked)}
                className="size-3.5 accent-emerald-600"
              />
              Sub clone: show it under {parentId}
            </label>

            <section className={`border-t pt-3 ${LINE}`}>
              <button
                type="button"
                onClick={() => setMore((m) => !m)}
                aria-expanded={more}
                className={`flex items-center gap-1 ${cloneSectionCaption}`}
              >
                {more ? <ChevronDown className="size-3.5" /> : <ChevronRight className="size-3.5" />}
                More options
              </button>
              {more ? (
                <div className="mt-3 space-y-3">
                  <CloneAccountFields
                    accounts={accounts}
                    groups={groups}
                    group={draft.group}
                    claudeAccount={draft.claudeAccount}
                    codexAccount={draft.codexAccount}
                    onGroupChange={(v) => onDraftChange("group", v)}
                    onClaudeAccountChange={(v) => onDraftChange("claudeAccount", v)}
                    onCodexAccountChange={(v) => onDraftChange("codexAccount", v)}
                  />
                  <CloneOptionsRow
                    headless={draft.headless}
                    onHeadlessChange={(v) => onDraftChange("headless", v)}
                    runStartupScript={draft.runStartupScript}
                    onRunStartupScriptChange={(v) => onDraftChange("runStartupScript", v)}
                    rebuild={draft.rebuild}
                    onRebuildChange={(v) => onDraftChange("rebuild", v)}
                  />
                </div>
              ) : null}
            </section>
          </div>

          {error ? (
            <div className={`mt-3 shrink-0 overflow-hidden ${R_PIECE}`}>
              <ErrorLine message={error} />
            </div>
          ) : null}

          {operation ? (
            <div className="mt-3 shrink-0">
              <OperationProgress op={operation} />
            </div>
          ) : null}

          <div className="mt-4 flex shrink-0 justify-end gap-2">
            <button type="button" onClick={() => close()} className={`${BUTTON} px-3 py-1.5 text-sm`}>
              Close
            </button>
            <button
              type="button"
              onClick={onSubmit}
              disabled={!valid || busy}
              className={`${DONE_BUTTON} px-4 py-1.5 text-sm font-medium`}
            >
              {busy ? "Forking…" : "Fork"}
            </button>
          </div>
        </>
      )}
    </ModalShell>
  );
}
