// Clone dialog, network half. Pick a clone-source image, then one of three ticket modes:
// paste an existing Linear ticket (link or `WE-142`); create a new ticket (team key + title +
// rich-text description); or a plain no-ticket clone (title + optional first message).
//
// Four things live here and nowhere below: the config read that supplies the presets and the
// account pools, the Linear round trip that turns a ticket mode into a real issue, the clone
// POST, and the operation the POST returns. The dialog stays open on that operation and closes
// only when it settles, which is why the op list is a prop rather than something the View could
// ever have. The markup is CloneModalView.
//
// **Both ticket modes talk to Linear from here.** The existing-ticket tab looks the issue up by
// identifier, the new-ticket tab opens one, and either way it is moved to In Progress and the
// resolved metadata is posted to `/api/clone`. The server holds no key and makes no call. The
// move is best effort, the same as the server call it replaces: a workflow column is not worth
// failing a clone over.
//
// **The preset is never picked by hand in the ticket modes** — it follows the team key
// (`pick_preset_by_prefix`, mirrored client-side in `~/lib/cloneDraft`), and the dialog shows
// which one resolved. The account group follows the resolved preset's default; the group
// control is an *override* that only matters when the operator wants a different pool.
//
// The hostname derives from the ticket id (`WE-142` → `pega-we-142`) or the title slug. That
// one step stays server-side in every mode: it needs the live clone list to guarantee the
// hostname is free, which no client can see.
import { lazy, Suspense, useCallback, useEffect, useMemo, useState } from "react";

import { CloneModalView } from "~/components/CloneModalView";
import { getConfig, type ClonePayload } from "~/lib/api";
import { toLinearMarkdown } from "~/lib/linear/assets";
import {
  cloneLinearMeta,
  ensureInProgress,
  fetchIssueAny,
  issueRefOf,
  resolvedFromTicket,
  type ResolvedIssue,
} from "~/lib/linear/issues";
import { issueCreate, keysForTeam } from "~/lib/linear/mutations";
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

  // Which key stores an image pasted into the new-ticket body. The issue is opened with the
  // key of the preset that claims the chosen team, so its images belong in the same workspace.
  // BlockNote captures its upload function at mount, so what counts is the team the editor
  // mounted on; switching teams mid-draft leaves an already-pasted image where it was stored.
  const uploadKey = keysForTeam(presets, draft.team)[0] ?? "";
  // The one sentence the description slot shows before it can take a keystroke, whether the
  // wait is for the config or for BlockNote's own chunk.
  const editorLoading = (
    <p className="px-3 text-xs text-slate-400 dark:text-slate-500">Loading editor…</p>
  );

  /** The issue this clone is for, and the key proven to reach it.
   *
   *  Existing-ticket looks it up by identifier across every configured key. New-ticket opens
   *  one with the key of the preset that claims the team. Both answer the same pair, so the
   *  step after them does not care which tab is open. */
  async function resolveIssue(): Promise<{ issue: ResolvedIssue; key: string }> {
    if (draft.mode === "existing") {
      const ref = issueRefOf(draft.ticket);
      // Unreachable while `valid` gates the button on the same parse, and stated anyway
      // because this function is the one that would otherwise fetch `undefined`.
      if (!ref) throw new Error(`could not find a ticket id (like WE-142) in "${draft.ticket}"`);
      return fetchIssueAny(keysForTeam(presets, ref.prefix), ref);
    }
    const team = draft.team.trim();
    const key = keysForTeam(presets, team)[0] ?? "";
    // The editor holds every pasted image behind `/api/linear/asset`, because that is the only
    // source an `<img>` on this page can load. Linear gets the `uploads.linear.app` URL back,
    // so the issue reads correctly for everyone who is not on this LAN. This is the last place
    // the body is ours.
    const ticket = await issueCreate(key, {
      team,
      title: draft.title.trim(),
      description: toLinearMarkdown(draft.description),
      ...(draft.priority > 0 ? { priority: draft.priority } : {}),
      // No assignee: `issueCreate` falls back to the key's own owner, which is you, and the
      // clone about to be made is yours.
    });
    return { issue: resolvedFromTicket(ticket), key };
  }

  /** What `POST /api/clone` is sent, once Linear has answered.
   *
   *  The no-ticket tab reaches no network here at all. It has no issue to resolve, and its
   *  payload is the same one it has always sent. */
  async function buildPayload(
    common: { claudeAccount?: string; codexAccount?: string; headless?: boolean; parent?: string },
    extra: { agentInstructions?: string; claudeInstructions?: string },
  ): Promise<ClonePayload> {
    if (draft.mode === "plain") {
      return {
        plain: { title: draft.title.trim(), message: draft.message.trim() },
        preset: draft.plainPreset || undefined,
        ...common,
      };
    }
    const { issue, key } = await resolveIssue();
    // Best effort, exactly as the server call it replaces was: it only warned. A ticket that
    // will not move is still a ticket worth cloning.
    try {
      await ensureInProgress(key, issue);
    } catch (e) {
      console.warn(`could not move ${issue.identifier} to In Progress:`, e);
    }
    return {
      linear: cloneLinearMeta(issue),
      ...extra,
      // Omitted when no preset claims the team, which leaves the server to auto-select by the
      // ticket's prefix, the same fallback the `{ticket}` mode has always relied on.
      preset: preset?.name,
      ...common,
    };
  }

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

    buildPayload(common, extra)
      .then((payload) => onClone(image, payload))
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
        // Held back until the config lands, because the key is what decides where a pasted
        // image goes. An editor mounted without one uploads to this server's `/uploads`, and
        // BlockNote captures its upload function once at mount, so a key arriving a moment
        // later would not be used, and a LAN-only URL would reach a real Linear issue.
        configLoaded ? (
          <Suspense fallback={editorLoading}>
            <MarkdownEditorContainer
              onChange={(markdown) => update("description", markdown)}
              linearKey={uploadKey}
              placeholder="What needs doing — paste images, format freely"
            />
          </Suspense>
        ) : (
          editorLoading
        )
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
