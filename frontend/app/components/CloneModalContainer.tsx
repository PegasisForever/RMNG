// Clone dialog, network half. Pick a clone-source image, then one of three ticket modes:
// paste an existing Linear ticket (link or `WE-142`); create a new ticket (team key + title +
// rich-text description); or a plain no-ticket clone (title + optional first message).
//
// Three things live here and nowhere below: the config read that supplies the presets and the
// account pools, the clone POST, and the operation the POST returns. The dialog stays open on
// that operation and closes only when it settles, which is why the op list is a prop rather
// than something the View could ever have. The markup is CloneModalView.
//
// **The preset is never picked by hand in the ticket modes** — it follows the team key
// (`pick_preset_by_prefix`, mirrored client-side in `~/lib/cloneDraft`), and the dialog shows
// which one resolved. The account group follows the resolved preset's default; the group
// control is an *override* that only matters when the operator wants a different pool.
//
// The hostname derives from the ticket id (`WE-142` → `pega-we-142`) or the title slug. All
// resolved server-side.
import { lazy, Suspense, useCallback, useEffect, useMemo, useState } from "react";

import { CloneModalView } from "~/components/CloneModalView";
import { getConfig, type ClonePayload } from "~/lib/api";
import {
  lastCloneImage,
  preferredCloneImage,
  rememberCloneImage,
} from "~/lib/lastCloneImage";
import {
  cloneDraftValid,
  emptyCloneDraft,
  linearKeyMissing,
  opPhase,
  resolvePreset,
  teamKeysOf,
  type CloneDraft,
} from "~/lib/cloneDraft";
import type { ClaudeUsage, Clone, Operation } from "~/lib/types";
import type { CloneGroup } from "~/lib/wire/CloneGroup";
import type { ImageInfo } from "~/lib/wire/ImageInfo";
import type { PresetRedacted } from "~/lib/wire/PresetRedacted";
import { parseTicketInput } from "~/lib/workspace";

// BlockNote is browser-only and heavy; the description field pulls it in on demand. The
// container is the import target, so the /api/upload call it owns rides the same lazy chunk.
const MarkdownEditorContainer = lazy(() => import("~/components/MarkdownEditorContainer"));

export function CloneModalContainer({
  images,
  imagesLoading,
  operations,
  parentCandidate,
  accounts,
  initialTicket = "",
  onClose,
  onClone,
}: {
  /** Clone-source images to pick from (from `listImages`). */
  images: ImageInfo[];
  imagesLoading: boolean;
  /** Live operations from the SSE state — the started clone op is tracked through these. */
  operations: Operation[];
  /** The currently selected clone, offered as a sub-clone parent. Null = nothing selected,
   *  or the selection can't be a parent (unmanaged, or already a sub clone). */
  parentCandidate: Clone | null;
  /** Imported accounts (both providers), so the two pickers can label each with its usage. */
  accounts: ClaudeUsage[];
  /** Seeds the existing-ticket field, e.g. from a ticket dragged onto a board column. A
   *  Linear URL is enough: the same parser reads an id out of a link or a bare `WE-142`,
   *  so the preset auto-selects from it exactly as it would from typing. */
  initialTicket?: string;
  onClose: () => void;
  /** Starts the clone and resolves with the driving Operation. The dialog stays open,
   *  showing its progress, until the operation settles. */
  onClone: (image: string, payload: ClonePayload) => Promise<Operation>;
}) {
  const [draft, setDraft] = useState<CloneDraft>(() => emptyCloneDraft(initialTicket));
  const update = useCallback(
    <K extends keyof CloneDraft>(key: K, value: CloneDraft[K]) =>
      setDraft((d) => ({ ...d, [key]: value })),
    [],
  );
  // The instant the dialog opened, for the image rows' ages. It does not tick: an image is
  // days old and nobody keeps this dialog open long enough for "6d ago" to turn into "7d".
  const [now] = useState(() => Date.now());

  // Which image a fresh dialog starts on. This is a session read (the last image actually
  // cloned from, remembered in localStorage), so it is resolved here and the picker is told
  // the answer. It re-runs when the list arrives, and skips whenever the operator has already
  // picked one that still exists.
  useEffect(() => {
    if (draft.image && images.some((i) => i.reference === draft.image)) return;
    const preferred = preferredCloneImage(images, lastCloneImage());
    if (preferred) update("image", preferred);
  }, [images, draft.image, update]);

  // Account pools and presets (from config).
  const [claudeGroups, setClaudeGroups] = useState<CloneGroup[]>([]);
  const [codexGroups, setCodexGroups] = useState<CloneGroup[]>([]);
  const [presets, setPresets] = useState<PresetRedacted[]>([]);
  // Config settled (loaded or failed). `presets` starts empty, which is indistinguishable
  // from "none configured" — without this the missing-key warning flashes on every open.
  const [configLoaded, setConfigLoaded] = useState(false);
  // The started clone operation: its id once the POST returns, plus a local error.
  const [opId, setOpId] = useState<string | null>(null);
  const [starting, setStarting] = useState(false);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    getConfig()
      .then((c) => {
        setPresets(c.presets);
        setClaudeGroups(c.cloneGroups);
        setCodexGroups(c.codexGroups);
      })
      .catch(() => {
        // Config unreachable — just no preset/group options.
      })
      .finally(() => setConfigLoaded(true));
  }, []);

  // The no-ticket tab needs an explicit preset — default to the first one.
  useEffect(() => {
    if (draft.mode === "plain" && draft.plainPreset === "" && presets.length > 0) {
      update("plainPreset", presets[0].name);
    }
  }, [draft.mode, draft.plainPreset, presets, update]);

  const teamKeys = useMemo(() => teamKeysOf(presets), [presets]);

  useEffect(() => {
    if (draft.mode === "create" && draft.team === "" && teamKeys.length > 0) {
      update("team", teamKeys[0].key);
    }
  }, [draft.mode, draft.team, teamKeys, update]);

  const parsedTicket = parseTicketInput(draft.ticket);
  const preset = resolvePreset(draft.mode, presets, {
    plainPreset: draft.plainPreset,
    team: draft.team,
    ticketPrefix: parsedTicket?.prefix,
  });
  const keyMissing = linearKeyMissing(draft.mode, presets, preset, configLoaded);
  const valid = cloneDraftValid(draft, {
    presets,
    preset,
    ticketParsed: !!parsedTicket,
    keyMissing,
  });

  // --- operation tracking ---------------------------------------------------------------
  // Once started, follow the op through the SSE frames and close only when it settles.
  // Finished ops are PRUNED from state a few seconds after they land, so an op that
  // disappears having previously been seen counts as done — the same rule the CLI's waiter
  // uses, and the reason a slow SSE frame can't strand the dialog open forever.
  const op = opId ? operations.find((o) => o.id === opId) : undefined;
  const [opSeen, setOpSeen] = useState(false);
  // Sticky: an op that errored has SETTLED. Without this the vanish-means-done rule above
  // would fire when the failed op is pruned (60s later) and close the dialog out from under
  // the error message.
  const [failed, setFailed] = useState(false);
  useEffect(() => {
    if (op) setOpSeen(true);
    if (op?.status === "error") {
      setFailed(true);
      setError(op.message || "the clone failed");
    }
  }, [op]);
  useEffect(() => {
    if (!opId) return;
    if (opPhase(op, opSeen, failed) === "done") onClose();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [opId, op, opSeen, failed]);

  const busy = starting || (!!opId && !failed);

  function submit() {
    const image = draft.image;
    if (!valid || busy || !image) return;
    // Clear the previous attempt so a retry after a failure tracks the NEW op, not the old
    // failed one (which is still in `operations` for another minute before it's pruned).
    setError(null);
    setOpId(null);
    setOpSeen(false);
    setFailed(false);
    setStarting(true);
    // A blank override means "let the server resolve it" (preset default → first group),
    // so it's omitted rather than sent as an empty name.
    const common = {
      claudeAccount: draft.claudeAccount || undefined,
      codexAccount: draft.codexAccount || undefined,
      headless: draft.headless || undefined,
      parent: draft.asSubClone && parentCandidate ? parentCandidate.id : undefined,
    };
    const extra: { agentInstructions?: string; claudeInstructions?: string } = {};
    if (draft.agentInstructions.trim()) extra.agentInstructions = draft.agentInstructions.trim();
    if (draft.claudeInstructions.trim()) extra.claudeInstructions = draft.claudeInstructions.trim();

    const payload: ClonePayload =
      draft.mode === "plain"
        ? {
            plain: { title: draft.title.trim(), message: draft.message.trim() },
            preset: draft.plainPreset || undefined,
            ...common,
          }
        : draft.mode === "existing"
          ? {
              ticket: draft.ticket.trim(),
              ...extra,
              // No preset field: the server auto-selects by the ticket's team prefix.
              ...common,
            }
          : {
              create: {
                team: draft.team.trim().toLowerCase(),
                title: draft.title.trim(),
                description: draft.description,
              },
              ...extra,
              preset: preset?.name,
              ...common,
            };

    onClone(image, payload)
      .then((started) => {
        rememberCloneImage(image);
        setOpId(started.id);
      })
      .catch((e: Error) => setError(e.message))
      .finally(() => setStarting(false));
  }

  return (
    <CloneModalView
      draft={draft}
      onDraftChange={update}
      images={images}
      imagesLoading={imagesLoading}
      now={now}
      accounts={accounts}
      claudeGroups={claudeGroups}
      codexGroups={codexGroups}
      presets={presets}
      teamKeys={teamKeys}
      parsedTicket={parsedTicket}
      preset={preset}
      linearKeyMissing={keyMissing}
      parentCandidate={parentCandidate}
      descriptionEditor={
        <Suspense
          fallback={
            <p className="px-3 text-xs text-slate-400 dark:text-slate-500">Loading editor…</p>
          }
        >
          <MarkdownEditorContainer
            onChange={(markdown) => update("description", markdown)}
            placeholder="What needs doing — paste images, format freely"
          />
        </Suspense>
      }
      valid={valid}
      busy={busy}
      error={error}
      operation={op ?? null}
      onSubmit={submit}
      onClose={onClose}
    />
  );
}
