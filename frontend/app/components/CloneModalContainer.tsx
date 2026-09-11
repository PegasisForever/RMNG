// The clone dialog, network half: the config read that supplies the presets and pools, the
// Linear work the ticket tabs need, the POST that starts the clone, and the operation it
// answers with. The dialog stays open on that operation and closes when it settles, which is
// why the op list is a prop. The form model is `~/lib/cloneDraft`; the markup is
// CloneModalView.
import {
  lazy,
  Suspense,
  useCallback,
  useEffect,
  useMemo,
  useReducer,
} from "react";

import { CloneModalView } from "~/components/CloneModalView";
import { getConfig } from "~/lib/api";
import { keyFor, ticketForClone, useAssignee } from "~/lib/linear/intake";
import {
  cloneDialogBusy,
  cloneDialogReducer,
  cloneDialogValid,
  cloneRequest,
  emptyCloneDialog,
  linearKeyMissing,
  presetOf,
  teamKeysOf,
  type CloneDialogEvent,
  type CloneDraft,
} from "~/lib/cloneDraft";
import type { ClaudeUsage, Clone, Operation } from "~/lib/types";
import type { CloneRequest } from "~/lib/wire/CloneRequest";
import type { LinearMeta } from "~/lib/wire/LinearMeta";
import { parseTicketInput } from "~/lib/workspace";

// BlockNote is browser-only and heavy; the description field pulls it in on demand. The
// container is the import target, so the /api/upload call it owns rides the same lazy chunk.
const MarkdownEditorContainer = lazy(
  () => import("~/components/MarkdownEditorContainer"),
);

export function CloneModalContainer({
  clones,
  operations,
  accounts,
  initialTicket = "",
  initialSource = null,
  onClose,
  onStart,
}: {
  /** Live clones to fork from; the dialog offers only forkable rows (managed, not archived). */
  clones: Clone[];
  /** Live operations from the SSE state — the started op is followed through these. */
  operations: Operation[];
  /** Imported accounts (both providers), so the two pickers can label each with its usage. */
  accounts: ClaudeUsage[];
  /** Seeds the existing-ticket field, e.g. from a ticket dragged onto a board column. A
   *  Linear URL is enough: the same parser reads an id out of a link or a bare `WE-142`. */
  initialTicket?: string;
  /** Pre-selects a source clone, e.g. from the clone's own menu. Null = pick by hand. */
  initialSource?: string | null;
  onClose: () => void;
  /** Starts the clone and resolves with the driving Operation: a fork of the picked source
   *  clone, or (the template tab) a clone built from the preset's image. */
  onStart: (fork: boolean, req: CloneRequest) => Promise<Operation>;
}) {
  const [state, dispatch] = useReducer(cloneDialogReducer, null, () =>
    emptyCloneDialog(initialTicket, initialSource),
  );
  const onDraftChange = useCallback(
    <K extends keyof CloneDraft>(key: K, value: CloneDraft[K]) =>
      dispatch({ type: "edit", key, value } as CloneDialogEvent),
    [],
  );

  const sources = useMemo(
    () => clones.filter((c) => c.managed && !c.archived),
    [clones],
  );
  // Joined, so a new array on every SSE frame does not re-announce the same list.
  const sourceIds = sources.map((c) => c.id).join(" ");
  useEffect(() => {
    getConfig().then(
      (c) => dispatch({ type: "config", presets: c.presets, groups: c.groups }),
      // Config unreachable — just no preset or pool options.
      () => dispatch({ type: "config", presets: [], groups: [] }),
    );
  }, []);
  useEffect(() => {
    dispatch({ type: "sources", ids: sourceIds ? sourceIds.split(" ") : [] });
  }, [sourceIds]);

  const op = state.opId
    ? operations.find((o) => o.id === state.opId)
    : undefined;
  useEffect(() => {
    if (state.opId) dispatch({ type: "op", op });
  }, [state.opId, op]);
  useEffect(() => {
    if (state.done) onClose();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [state.done]);

  const busy = cloneDialogBusy(state);
  // Which key claims the chosen team: it stores an image pasted into the new-ticket body, so
  // the images land in the issue's own workspace, and it answers who can hold the ticket.
  const key = keyFor(state.presets, state.draft.team);
  const { assigneeId } = useAssignee(key, state.draft.team);
  const editorLoading = (
    <p className="px-3 text-xs text-slate-400 dark:text-slate-500">
      Loading editor…
    </p>
  );

  /** Linear's own answer for the ticket tabs. The other two reach no network here at all. */
  function ticket(): Promise<LinearMeta | undefined> {
    const d = state.draft;
    if (d.mode === "existing")
      return ticketForClone(state.presets, { ticket: d.ticket });
    if (d.mode === "create")
      return ticketForClone(state.presets, {
        team: d.team.trim(),
        title: d.title.trim(),
        description: d.description,
        ...(d.priority > 0 ? { priority: d.priority } : {}),
        ...(assigneeId !== "" ? { assigneeId } : {}),
      });
    return Promise.resolve(undefined);
  }

  function submit() {
    if (!cloneDialogValid(state) || busy) return;
    dispatch({ type: "starting" });
    ticket()
      .then((linear) =>
        onStart(state.draft.mode !== "template", cloneRequest(state, linear)),
      )
      .then(
        (started) => dispatch({ type: "started", opId: started.id }),
        (e: Error) => dispatch({ type: "failed", message: e.message }),
      );
  }

  return (
    <CloneModalView
      draft={state.draft}
      onDraftChange={onDraftChange}
      clones={sources}
      accounts={accounts}
      groups={state.groups}
      presets={state.presets}
      teamKeys={teamKeysOf(state.presets)}
      parsedTicket={parseTicketInput(state.draft.ticket)}
      preset={presetOf(state)}
      linearKeyMissing={linearKeyMissing(state)}
      descriptionEditor={
        // Held back until the config lands: the key decides where a pasted image goes, and
        // BlockNote captures its upload function once at mount.
        state.configLoaded ? (
          <Suspense fallback={editorLoading}>
            <MarkdownEditorContainer
              onChange={(markdown) => onDraftChange("description", markdown)}
              linearKey={key}
              placeholder="What needs doing — paste images, format freely"
            />
          </Suspense>
        ) : (
          editorLoading
        )
      }
      valid={cloneDialogValid(state)}
      busy={busy}
      error={state.error}
      operation={op ?? null}
      onSubmit={submit}
      onClose={onClose}
    />
  );
}
