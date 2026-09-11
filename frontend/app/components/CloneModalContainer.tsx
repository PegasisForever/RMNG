// Clone dialog, network half. Four tabs: three fork a live source clone (an existing
// Linear ticket, a new ticket, or plain no-ticket), and the fourth creates from a template
// image onto a fresh empty home dataset.
//
// Fork submit files `POST /api/fork` with the ticket mode's answer plus
// preset/accounts/instructions; omitted fields inherit the source's bindings server-side.
// Template submit files `POST /api/clone` in plain mode (title + preset), headed unless
// asked, with the preset's defaults underneath the account overrides.
//
// Four things live here and nowhere below: the config read that supplies the presets and
// the account pools, the fork/clone POSTs, and the operation the POST returns. The dialog
// stays open on that operation and closes only when it settles, which is why the op list
// is a prop rather than something the View could ever have. The markup is CloneModalView.
import {
  lazy,
  Suspense,
  useCallback,
  useEffect,
  useMemo,
  useState,
} from "react";

import { CloneModalView } from "~/components/CloneModalView";
import { getConfig, type ClonePayload } from "~/lib/api";
import { keysForTeam, issueCreate } from "~/lib/linear/mutations";
import {
  cloneLinearMeta,
  ensureInProgress,
  fetchIssueAny,
  issueRefOf,
  resolvedFromTicket,
  type ResolvedIssue,
} from "~/lib/linear/issues";
import { toLinearMarkdown } from "~/lib/linear/assets";
import type { ForkPayload } from "~/lib/api";
import {
  cloneDraftValid,
  emptyCloneDraft,
  linearKeyMissing,
  opPhase,
  resolveForkSource,
  resolvePreset,
  teamKeysOf,
  type CloneDraft,
} from "~/lib/cloneDraft";
import type { ClaudeUsage, Clone, Operation } from "~/lib/types";
import type { CloneGroup } from "~/lib/wire/CloneGroup";
import type { PresetRedacted } from "~/lib/wire/PresetRedacted";
import { parseTicketInput } from "~/lib/workspace";

// BlockNote is browser-only and heavy; the description field pulls it in on demand. The
// container is the import target, so the /api/upload call it owns rides the same lazy chunk.
const MarkdownEditorContainer = lazy(
  () => import("~/components/MarkdownEditorContainer"),
);

export function CloneModalContainer({
  clones,
  clonesLoading,
  operations,
  accounts,
  initialTicket = "",
  initialSource = null,
  onClose,
  onFork,
  onClone,
}: {
  /** Live clones to fork from; the dialog shows only forkable rows (managed, not archived). */
  clones: Clone[];
  clonesLoading: boolean;
  /** Live operations from the SSE state — the started fork op is tracked through these. */
  operations: Operation[];
  /** Imported accounts (both providers), so the two pickers can label each with its usage. */
  accounts: ClaudeUsage[];
  /** Seeds the existing-ticket field, e.g. from a ticket dragged onto a board column. A
   *  Linear URL is enough: the same parser reads an id out of a link or a bare `WE-142`,
   *  so the preset auto-selects from it exactly as it would from typing. */
  initialTicket?: string;
  /** Pre-selects a source clone, e.g. from the clone's own menu. Null = pick by hand. */
  initialSource?: string | null;
  onClose: () => void;
  /** Starts the fork and resolves with the driving Operation. The dialog stays open,
   *  showing its progress, until the operation settles. Payload carries the ticket
   *  mode's answer plus preset/accounts/instructions; omitted fields inherit server-side. */
  onFork: (
    source: string,
    headless: boolean,
    payload: ForkPayload,
  ) => Promise<Operation>;
  /** Starts a template clone and resolves with the driving Operation. Same lifecycle as
   *  a fork: the dialog stays open, showing its progress, until the operation settles. */
  onClone: (payload: ClonePayload) => Promise<Operation>;
}) {
  const [draft, setDraft] = useState<CloneDraft>(() => ({
    ...emptyCloneDraft(initialTicket),
    source: initialSource,
  }));
  // A source arriving via `initialSource` (the clone's own menu) counts as the
  // operator's pick: resolving a preset must not move it.
  const [sourceTouched, setSourceTouched] = useState(initialSource != null);
  // Same stickiness for the account picks: the effects below fill them from the
  // resolved preset, but a hand pick stays whatever preset resolves later.
  const [groupTouched, setGroupTouched] = useState(false);
  const [claudeTouched, setClaudeTouched] = useState(false);
  const [codexTouched, setCodexTouched] = useState(false);
  const update = useCallback(
    <K extends keyof CloneDraft>(key: K, value: CloneDraft[K]) =>
      setDraft((d) => ({ ...d, [key]: value })),
    [],
  );
  // The View's writer: picking a source or an account pick by hand pins it, so a
  // later preset resolving no longer moves it. Every other field writes straight through.
  const onDraftChange = useCallback(
    <K extends keyof CloneDraft>(key: K, value: CloneDraft[K]) => {
      if (key === "source") setSourceTouched(true);
      if (key === "group") setGroupTouched(true);
      if (key === "claudeAccount") setClaudeTouched(true);
      if (key === "codexAccount") setCodexTouched(true);
      setDraft((d) => ({ ...d, [key]: value }));
    },
    [],
  );
  // Only live managed clones can be forked: archived ones are stopped and retained, and
  // unmanaged rows are not ours to snapshot.
  const sources = useMemo(
    () => clones.filter((c) => c.managed && !c.archived),
    [clones],
  );
  useEffect(() => {
    if (!clonesLoading && sources.length === 0 && draft.mode !== "template") {
      update("mode", "template");
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [clonesLoading, sources.length]);

  // Account pools and presets (from config).
  const [groups, setGroups] = useState<CloneGroup[]>([]);
  const [presets, setPresets] = useState<PresetRedacted[]>([]);
  // Config settled (loaded or failed). `presets` starts empty, which is indistinguishable
  // from "none configured" — without this the missing-key warning flashes on every open.
  const [configLoaded, setConfigLoaded] = useState(false);
  // The started fork operation: its id once the POST returns, plus a local error.
  const [opId, setOpId] = useState<string | null>(null);
  const [starting, setStarting] = useState(false);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    getConfig()
      .then((c) => {
        setPresets(c.presets);
        setGroups(c.groups);
      })
      .catch(() => {
        // Config unreachable — just no preset/group options.
      })
      .finally(() => setConfigLoaded(true));
  }, []);

  // The no-ticket and template tabs need an explicit preset — default to the first one.
  useEffect(() => {
    if (
      draft.mode === "plain" &&
      draft.plainPreset === "" &&
      presets.length > 0
    ) {
      update("plainPreset", presets[0].name);
    }
    if (
      draft.mode === "template" &&
      draft.templatePreset === "" &&
      presets.length > 0
    ) {
      update("templatePreset", presets[0].name);
    }
  }, [draft.mode, draft.plainPreset, draft.templatePreset, presets, update]);

  const teamKeys = useMemo(() => teamKeysOf(presets), [presets]);

  useEffect(() => {
    if (draft.mode === "create" && draft.team === "" && teamKeys.length > 0) {
      update("team", teamKeys[0].key);
    }
  }, [draft.mode, draft.team, teamKeys, update]);

  const parsedTicket = parseTicketInput(draft.ticket);
  const preset = resolvePreset(draft.mode, presets, {
    plainPreset: draft.plainPreset,
    templatePreset: draft.templatePreset,
    team: draft.team,
    ticketPrefix: parsedTicket?.prefix,
  });
  // Fork source follows the resolved preset until the operator picks one by hand: the
  // preset's default fork clone where it is still forkable, else the oldest forkable
  // clone. Blank until a preset resolves (nothing to follow yet) — except with no
  // presets configured, where the oldest forkable clone is the only answer. A hand pick
  // (or `initialSource`) sticks across preset changes; a pick that stops qualifying
  // (clone deleted or archived) falls back to auto.
  const presetDefault = preset?.defaultForkClone;
  useEffect(() => {
    if (!configLoaded || sources.length === 0 || draft.mode === "template")
      return;
    const ids = sources.map((c) => c.id);
    if (sourceTouched) {
      if (draft.source && ids.includes(draft.source)) return;
      setSourceTouched(false);
    }
    if (!preset && presets.length > 0) {
      if (draft.source !== null) update("source", null);
      return;
    }
    const next = resolveForkSource(presetDefault, ids);
    if ((next ?? null) !== draft.source) update("source", next);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [
    configLoaded,
    preset,
    presetDefault,
    presets.length,
    sources,
    draft.mode,
  ]);
  // Account picks follow the resolved preset until touched by hand (see above): the
  // preset's pool fills the group box directly and both sides read Auto.
  useEffect(() => {
    if (!configLoaded || !preset) return;
    if (!groupTouched) {
      const g = preset.group.trim();
      const next = g === "" || g.toLowerCase() === "none" ? "none" : g;
      if (draft.group !== next) update("group", next);
    }
    if (!claudeTouched && draft.claudeAccount !== "auto")
      update("claudeAccount", "auto");
    if (!codexTouched && draft.codexAccount !== "auto")
      update("codexAccount", "auto");
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [configLoaded, preset, groupTouched, claudeTouched, codexTouched]);
  // Account picks follow the resolved preset until touched by hand: the preset's pool
  // fills the group box directly and both sides read Auto — no "preset default"
  // pseudo-option. Blank until a preset resolves (or none is configured, where blank
  // keeps today's server-side fallback). A fork therefore binds the detected preset's
  // pool rather than inheriting the source's.
  const keyMissing = linearKeyMissing(
    draft.mode,
    presets,
    preset,
    configLoaded,
  );
  const valid = cloneDraftValid(draft, {
    presets,
    preset,
    ticketParsed: !!parsedTicket,
    keyMissing,
    needsSource: draft.mode !== "template",
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
      setError(
        op.message ||
          (draft.mode === "template" ? "the clone failed" : "the fork failed"),
      );
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
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
    <p className="px-3 text-xs text-slate-400 dark:text-slate-500">
      Loading editor…
    </p>
  );

  /** The issue this fork is for, and the key proven to reach it.
   *
   *  Existing-ticket looks it up by identifier across every configured key. New-ticket opens
   *  one with the key of the preset that claims the team. Both answer the same pair, so the
   *  step after them does not care which tab is open. */
  async function resolveIssue(): Promise<{
    issue: ResolvedIssue;
    key: string;
  }> {
    if (draft.mode === "existing") {
      const ref = issueRefOf(draft.ticket);
      // Unreachable while `valid` gates the button on the same parse, and stated anyway
      // because this function is the one that would otherwise fetch `undefined`.
      if (!ref)
        throw new Error(
          `could not find a ticket id (like WE-142) in "${draft.ticket}"`,
        );
      return fetchIssueAny(keysForTeam(presets, ref.prefix), ref);
    }
    const team = draft.team.trim();
    const key = keysForTeam(presets, team)[0] ?? "";
    const ticket = await issueCreate(key, {
      team,
      title: draft.title.trim(),
      description: toLinearMarkdown(draft.description),
      ...(draft.priority > 0 ? { priority: draft.priority } : {}),
    });
    return { issue: resolvedFromTicket(ticket), key };
  }

  /** What `POST /api/fork` is sent, once Linear has answered.
   *
   *  The no-ticket tab reaches no network here at all. Ticket tabs resolve the issue,
   *  move it to In Progress best-effort, and send its metadata; the server applies it
   *  onto the fork, replacing the source's ticket context. */
  async function buildForkPayload(): Promise<ForkPayload> {
    const base: ForkPayload = {
      ...(preset ? { preset: preset.name } : {}),
      runStartupScript: draft.runStartupScript,
      ...(draft.rebuild ? { rebuild: true } : {}),
      ...(draft.claudeAccount ? { claudeAccount: draft.claudeAccount } : {}),
      ...(draft.codexAccount ? { codexAccount: draft.codexAccount } : {}),
      ...(draft.group
        ? { group: draft.group === "none" ? null : draft.group }
        : {}),
      ...(draft.mode !== "plain" && draft.agentInstructions.trim()
        ? { agentInstructions: draft.agentInstructions.trim() }
        : {}),
      ...(draft.mode !== "plain" && draft.claudeInstructions.trim()
        ? { claudeInstructions: draft.claudeInstructions.trim() }
        : {}),
    };
    if (draft.mode === "plain") {
      return {
        ...base,
        linear: { displayName: draft.title.trim() || undefined },
        ...(draft.message.trim() ? { firstMessage: draft.message.trim() } : {}),
      };
    }
    const { issue, key } = await resolveIssue();
    try {
      await ensureInProgress(key, issue);
    } catch (e) {
      console.warn(`could not move ${issue.identifier} to In Progress:`, e);
    }
    return { ...base, linear: cloneLinearMeta(issue) };
  }

  function submit() {
    if (!valid || busy) return;
    // Clear the previous attempt so a retry after a failure tracks the NEW op, not the old
    // failed one (which is still in `operations` for another minute before it's pruned).
    setError(null);
    setOpId(null);
    setOpSeen(false);
    setFailed(false);
    setStarting(true);
    if (draft.mode === "template") {
      const payload: ClonePayload = {
        plain: { title: draft.title.trim(), message: "" },
        ...(draft.templatePreset ? { preset: draft.templatePreset } : {}),
        runStartupScript: draft.runStartupScript,
        ...(draft.rebuild ? { rebuild: true } : {}),
        ...(draft.headless ? { headless: true } : {}),
        ...(draft.group
          ? { group: draft.group === "none" ? null : draft.group }
          : {}),
        ...(draft.claudeAccount ? { claudeAccount: draft.claudeAccount } : {}),
        ...(draft.codexAccount ? { codexAccount: draft.codexAccount } : {}),
      };
      onClone(payload)
        .then((started) => setOpId(started.id))
        .catch((e: Error) => setError(e.message))
        .finally(() => setStarting(false));
      return;
    }
    const source = draft.source;
    if (!source) return;
    // Ticket tabs resolve Linear first (existing: lookup, new: open + move to In
    // Progress); the answer rides the fork payload and replaces the source context.
    // Plain mode sends only its display name + optional first message.
    buildForkPayload()
      .then((payload) => onFork(source, draft.headless, payload))
      .then((started) => setOpId(started.id))
      .catch((e: Error) => setError(e.message))
      .finally(() => setStarting(false));
  }

  return (
    <CloneModalView
      draft={draft}
      onDraftChange={onDraftChange}
      clones={sources}
      clonesLoading={clonesLoading}
      accounts={accounts}
      groups={groups}
      presets={presets}
      teamKeys={teamKeys}
      parsedTicket={parsedTicket}
      preset={preset}
      linearKeyMissing={keyMissing}
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
