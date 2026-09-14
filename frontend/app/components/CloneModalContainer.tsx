// The clone dialog, network half: the config read that supplies the presets and pools, the
// Linear work the ticket tabs need, the POST that starts the clone, and the operation it
// answers with. The dialog stays open on that operation and closes when it settles, which is
// why the op list is a prop. The form model is `~/lib/cloneDraft`, the operation is
// `~/lib/useOperation`, and the markup is CloneModalView.
//
// Those two are separate on purpose. The form's rules read the form; the operation's rules
// read the op list, and the only thing they ever wanted from the form is the word for what
// this tab does — passed in below as `failureLabel`. That one string is the whole seam, and
// it is not worth keeping a second copy of the op machine inside the form's reducer to save.
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
import { useOperation } from "~/lib/useOperation";
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
  /** Clones to fork from: live first, archived last; the dialog offers every managed row. */
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

  // Live first, archived last: the auto-pick (`sources[0]` in the draft) stays on live
  // work, while an archived home — quiescent, so the most stable template — is one
  // click away and keeps a preset default pointing at it working.
  const sources = useMemo(
    () => [
      ...clones.filter((c) => c.managed && !c.archived),
      ...clones.filter((c) => c.managed && c.archived),
    ],
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

  // Which key claims the chosen team: it stores an image pasted into the new-ticket body, so
  // the images land in the issue's own workspace, and it answers who can hold the ticket.
  const key = keyFor(state.presets, state.draft.team);
  const { assigneeId } = useAssignee(key, state.draft.team);
  // The started clone, from the POST to the frame that settles it. `start` is read afresh on
  // every run, so it sends the form as it stands at the click — including the Linear round
  // trip the ticket tabs make first, which the dialog is already busy through.
  const clone = useOperation(
    operations,
    () =>
      ticket().then((linear) =>
        onStart(state.draft.mode !== "template", cloneRequest(state, linear)),
      ),
    {
      // The template tab builds a clone from an image; the other three fork a live one.
      failureLabel:
        state.draft.mode === "template"
          ? "the clone failed"
          : "the fork failed",
    },
  );
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
        ...(assigneeId === "" ? {} : { assigneeId }),
      });
    return Promise.resolve(undefined);
  }

  /** The Create button. Whether the form is ready is this module's question; whether an
   *  attempt is already running is the operation's, and `run` answers that one itself. */
  function submit() {
    if (cloneDialogValid(state)) clone.run();
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
      busy={clone.busy}
      error={clone.error}
      operation={clone.op ?? null}
      onSubmit={submit}
      // The settled operation is what closes the dialog. `open` goes false, the frame plays
      // its exit, and `onClose` unmounts once the frames have run. A FAILED clone is not
      // settled for this purpose — the dialog stays up holding its error.
      open={!clone.done}
      onClose={onClose}
    />
  );
}
