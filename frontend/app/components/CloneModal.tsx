import { lazy, Suspense, useEffect, useMemo, useState } from "react";

import { AccountGroupSelect } from "~/components/AccountGroupSelect";
import { ImagePicker, rememberCloneImage } from "~/components/ImagePicker";
import { OperationProgress } from "~/components/OperationProgress";
import { getConfig, type ClonePayload } from "~/lib/api";
import type { Clone, Operation } from "~/lib/types";
import type { Group } from "~/lib/wire/Group";
import type { ImageInfo } from "~/lib/wire/ImageInfo";
import type { PresetRedacted } from "~/lib/wire/PresetRedacted";
import { parseTicketInput, workspaceBadge } from "~/lib/workspace";

// BlockNote is browser-only and heavy; the description field pulls it in on demand.
const MarkdownEditor = lazy(() => import("~/components/MarkdownEditor"));

/** What the dialog should do about the clone operation it started. */
export type OpPhase = "running" | "done" | "failed";

/**
 * Classify the started operation from the live op list. Exported for tests — this is the
 * rule that decides when the dialog closes, and it has one non-obvious case.
 *
 * Finished operations are PRUNED from `ControlState` shortly after they settle (8s after
 * Done, 60s after Error). A poll of the list can therefore miss the terminal frame entirely,
 * so **an op that has vanished after being seen counts as done** — the same rule the CLI's
 * waiter uses. `failed` is passed in as sticky state by the caller, because that vanish rule
 * would otherwise fire when a FAILED op is pruned and close the dialog over its own error.
 */
export function opPhase(
  op: Operation | undefined,
  everSeen: boolean,
  alreadyFailed: boolean,
): OpPhase {
  if (alreadyFailed || op?.status === "error") return "failed";
  if (op?.status === "done") return "done";
  if (!op && everSeen) return "done";
  return "running";
}

/**
 * The preset that will actually drive the clone, per tab — mirroring what the server does so
 * the dialog shows the truth rather than a guess. Exported for tests.
 *
 * - `plain`: whatever the operator picked by hand.
 * - `create`: implied by the chosen team key. The key comes from the presets' own labels, so
 *   picking a team IS picking a preset — which is why that tab has no preset dropdown.
 * - `existing`: auto-selected from the ticket-id prefix, mirroring the server's
 *   `pick_preset_by_prefix` (first preset in config order with a case-insensitively matching
 *   label). Undefined until a ticket parses, so the group control reads blank until then.
 */
export function resolvePreset(
  mode: "existing" | "create" | "plain",
  presets: PresetRedacted[],
  { plainPreset, team, ticketPrefix }: {
    plainPreset?: string;
    team?: string;
    ticketPrefix?: string;
  },
): PresetRedacted | undefined {
  if (mode === "plain") return presets.find((p) => p.name === plainPreset);
  if (mode === "create") {
    return team
      ? presets.find((p) => p.labels.some((l) => l.toLowerCase() === team.toLowerCase()))
      : undefined;
  }
  return ticketPrefix
    ? presets.find((p) => p.labels.some((l) => l.toLowerCase() === ticketPrefix))
    : undefined;
}

/**
 * Clone dialog. Pick a clone-source image, then one of three ticket modes: paste an
 * existing Linear ticket (link or `WE-142`); create a new ticket (team key + title +
 * rich-text description); or a plain no-ticket clone (title + optional first message).
 *
 * **The preset is never picked by hand in the ticket modes** — it follows the team key
 * (`pick_preset_by_prefix`, mirrored client-side), and the dialog shows which one resolved.
 * The account group follows the resolved preset's default; the group control is an
 * *override* that only matters when the operator wants a different pool.
 *
 * The hostname derives from the ticket id (`WE-142` → `pega-we-142`) or the title slug.
 * All resolved server-side.
 */
export function CloneModal({
  images,
  imagesLoading,
  operations,
  parentCandidate,
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
  onClose: () => void;
  /** Starts the clone and resolves with the driving Operation. The dialog stays open,
   *  showing its progress, until the operation settles. */
  onClone: (image: string, payload: ClonePayload) => Promise<Operation>;
}) {
  const [image, setImage] = useState<string | null>(null);
  const [mode, setMode] = useState<"existing" | "create" | "plain">("existing");
  const [ticket, setTicket] = useState("");
  // Linear team key for created tickets (e.g. "we" → WE-…). Picked from the presets' labels.
  const [team, setTeam] = useState("");
  const [title, setTitle] = useState("");
  const [description, setDescription] = useState("");
  const [message, setMessage] = useState("");
  const [agentInstructions, setAgentInstructions] = useState("");
  const [claudeInstructions, setClaudeInstructions] = useState("");
  // Account-group OVERRIDE. "" = follow the resolved preset's default (the server resolves
  // it). A non-empty value pins the clone to that pool regardless of preset.
  const [groupOverride, setGroupOverride] = useState("");
  // Account groups (from config), for the picker options.
  const [groups, setGroups] = useState<Group[]>([]);
  // Presets (from config). Only the no-ticket tab picks one by hand.
  const [presets, setPresets] = useState<PresetRedacted[]>([]);
  const [plainPreset, setPlainPreset] = useState("");
  // Config settled (loaded or failed). `presets` starts empty, which is indistinguishable
  // from "none configured" — without this the missing-key warning flashes on every open.
  const [configLoaded, setConfigLoaded] = useState(false);
  // Headless clone: no desktop; the viewer shows a tmux tab view instead of a video stream.
  const [headless, setHeadless] = useState(false);
  const [asSubClone, setAsSubClone] = useState(false);
  // The started clone operation: its id once the POST returns, plus a local error.
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

  // The no-ticket tab needs an explicit preset — default to the first one.
  useEffect(() => {
    if (mode === "plain" && plainPreset === "" && presets.length > 0) {
      setPlainPreset(presets[0].name);
    }
  }, [mode, presets, plainPreset]);

  const parsed = parseTicketInput(ticket);

  // Every distinct team key across the presets' labels, each mapped to the preset that
  // claims it — the first one in config order, mirroring the server's `pick_preset_by_prefix`.
  // This is the new-ticket tab's team dropdown AND its preset selector: they're the same choice.
  const teamKeys = useMemo(() => {
    const seen = new Map<string, PresetRedacted>();
    for (const p of presets) {
      for (const label of p.labels) {
        const key = label.toLowerCase();
        if (!seen.has(key)) seen.set(key, p);
      }
    }
    return [...seen.entries()].map(([key, preset]) => ({ key, preset }));
  }, [presets]);

  useEffect(() => {
    if (mode === "create" && team === "" && teamKeys.length > 0) setTeam(teamKeys[0].key);
  }, [mode, team, teamKeys]);

  const effectivePreset = resolvePreset(mode, presets, {
    plainPreset,
    team,
    ticketPrefix: parsed?.prefix,
  });

  // What the group control shows: the override if the operator set one, else the resolved
  // preset's group. Blank until a preset resolves — on the ticket tabs that's "nothing typed
  // yet", which is exactly what the empty dropdown should convey.
  const presetGroup = groups.find((g) => g.name === effectivePreset?.group)?.name ?? "";
  const shownGroup = groupOverride || presetGroup;

  // Both ticket modes need a Linear API key, but not the same one — mirror the server so the
  // dialog blocks exactly the requests it would reject. `create` opens the issue with the
  // *resolved* preset's key (`resolve_issue`), so that one preset must have it; `existing`
  // only fetches, and the server tries every preset's key in turn (`fetch_issue_any`), so any
  // one of them will do. `plain` never touches Linear.
  const linearKeyMissing =
    !configLoaded || mode === "plain"
      ? false
      : mode === "create"
        ? !effectivePreset?.linearKeySet
        : !presets.some((p) => p.linearKeySet);

  // A source image is always required; then: `existing` needs a parseable ticket AND a preset
  // that claims its prefix — with the preset dropdown gone there's no way to override the
  // auto-selection, so a prefix nothing claims is a request the server would 400; `create`
  // needs a team key + title; `plain` a title + a preset whenever any are configured.
  const modeValid =
    mode === "existing"
      ? !!parsed && (presets.length === 0 || !!effectivePreset)
      : mode === "create"
        ? title.trim().length > 0 && team.trim().length > 0
        : title.trim().length > 0 && (presets.length === 0 || !!plainPreset);
  const valid = !!image && modeValid && !linearKeyMissing;

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
      group: groupOverride || undefined,
      headless: headless || undefined,
      parent: asSubClone && parentCandidate ? parentCandidate.id : undefined,
    };
    const extra: { agentInstructions?: string; claudeInstructions?: string } = {};
    if (agentInstructions.trim()) extra.agentInstructions = agentInstructions.trim();
    if (claudeInstructions.trim()) extra.claudeInstructions = claudeInstructions.trim();

    const payload: ClonePayload =
      mode === "plain"
        ? {
            plain: { title: title.trim(), message: message.trim() },
            preset: plainPreset || undefined,
            ...common,
          }
        : mode === "existing"
          ? {
              ticket: ticket.trim(),
              ...extra,
              // No preset field: the server auto-selects by the ticket's team prefix.
              ...common,
            }
          : {
              create: { team: team.trim().toLowerCase(), title: title.trim(), description },
              ...extra,
              preset: effectivePreset?.name,
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

  const tab = (m: typeof mode, label: string) => (
    <button
      type="button"
      disabled={busy}
      onClick={() => setMode(m)}
      className={`flex-1 rounded px-2 py-1 disabled:opacity-50 ${
        mode === m
          ? "bg-white text-slate-900 shadow-sm dark:bg-slate-700 dark:text-slate-100"
          : "text-slate-500 hover:text-slate-700 dark:text-slate-400 dark:hover:text-slate-200"
      }`}
    >
      {label}
    </button>
  );

  const field =
    "mt-1 w-full rounded-md border border-slate-300 px-3 py-2 text-sm font-normal text-slate-900 dark:bg-slate-800 placeholder:text-slate-400 focus:border-emerald-500 focus:outline-none dark:border-slate-600 dark:text-slate-100 dark:placeholder:text-slate-500";
  const label = "block text-xs font-medium text-slate-500 dark:text-slate-400";

  return (
    <div
      className="fixed inset-0 z-50 flex items-center justify-center bg-slate-900/30 p-4"
      onClick={busy ? undefined : onClose}
    >
      {/* Fixed height, not content height: the three tabs differ a lot in field count, and a
          dialog that jumps as you switch tabs is disorienting. The body scrolls instead. */}
      <div
        className="flex h-[38rem] max-h-[90vh] w-full max-w-md flex-col rounded-xl border border-slate-200 bg-white p-5 shadow-xl dark:border-slate-700 dark:bg-slate-800"
        onClick={(e) => e.stopPropagation()}
        onKeyDown={(e) => {
          if (e.key === "Escape" && !busy) onClose();
        }}
      >
        <h3 className="shrink-0 text-sm font-semibold text-slate-900 dark:text-slate-100">
          New clone
        </h3>

        <div className="min-h-0 flex-1 overflow-y-auto pr-0.5">
          <div className="mt-3 text-xs font-medium text-slate-500 dark:text-slate-400">
            Source image
            <ImagePicker
              images={images}
              loading={imagesLoading}
              value={image}
              onChange={setImage}
            />
          </div>

          <div className="mt-3 flex gap-0.5 rounded-md bg-slate-100 p-0.5 text-xs font-medium dark:bg-slate-800">
            {tab("existing", "Existing ticket")}
            {tab("create", "New ticket")}
            {tab("plain", "No ticket")}
          </div>

          {mode === "existing" ? (
            <label className={`mt-3 ${label}`}>
              Linear ticket link or id
              <input
                autoFocus
                value={ticket}
                onChange={(e) => setTicket(e.target.value)}
                onKeyDown={(e) => {
                  if (e.key === "Enter") submit();
                }}
                placeholder="https://linear.app/…/issue/WE-142  or  WE-142"
                spellCheck={false}
                className={field}
              />
              {ticket && !parsed ? (
                <p className="mt-1 text-[11px] text-red-600 dark:text-red-400">
                  couldn’t find a ticket id (like WE-142) in that
                </p>
              ) : null}
              {parsed ? (
                <p className="mt-1.5 flex items-center gap-1.5 text-[11px] font-normal text-slate-500 dark:text-slate-400">
                  <span
                    className={`rounded px-1.5 py-0.5 font-medium ${workspaceBadge(parsed.prefix)}`}
                  >
                    {parsed.identifier}
                  </span>
                  <span aria-hidden>→</span>
                  <span className="font-mono text-slate-700 dark:text-slate-200">
                    {parsed.hostname}
                  </span>
                </p>
              ) : null}
            </label>
          ) : mode === "create" ? (
            <div className="mt-3 space-y-3">
              <label className={label}>
                Team key
                {teamKeys.length === 0 ? (
                  <p className="mt-1 text-[11px] font-normal text-red-600 dark:text-red-400">
                    No preset declares a team key — add ticket-id prefixes to a preset in
                    Settings.
                  </p>
                ) : (
                  <select
                    value={team}
                    onChange={(e) => setTeam(e.target.value)}
                    className={field}
                  >
                    {teamKeys.map((t) => (
                      <option key={t.key} value={t.key}>
                        {t.key.toUpperCase()} · {t.preset.name}
                      </option>
                    ))}
                  </select>
                )}
              </label>
              <label className={label}>
                Title
                <input
                  autoFocus
                  value={title}
                  onChange={(e) => setTitle(e.target.value)}
                  placeholder="Short ticket title"
                  className={field}
                />
              </label>
              <div className={label}>
                Description
                <div className="mt-1 min-h-[8rem] rounded-md border border-slate-300 py-2 text-sm font-normal focus-within:border-emerald-500 dark:border-slate-600">
                  <Suspense
                    fallback={
                      <p className="px-3 text-xs text-slate-400 dark:text-slate-500">
                        Loading editor…
                      </p>
                    }
                  >
                    <MarkdownEditor
                      onChange={setDescription}
                      placeholder="What needs doing — paste images, format freely"
                    />
                  </Suspense>
                </div>
              </div>
            </div>
          ) : (
            <div className="mt-3 space-y-3">
              <label className={label}>
                Title
                <input
                  autoFocus
                  value={title}
                  onChange={(e) => setTitle(e.target.value)}
                  onKeyDown={(e) => {
                    if (e.key === "Enter") submit();
                  }}
                  placeholder="Container title"
                  className={field}
                />
              </label>
              <label className={label}>
                First message to the agent
                <textarea
                  value={message}
                  onChange={(e) => setMessage(e.target.value)}
                  rows={3}
                  placeholder="Optional — leave empty to not auto-send a first message"
                  className={`resize-y ${field}`}
                />
              </label>
              {presets.length > 0 ? (
                <label className={label}>
                  Preset
                  <select
                    value={plainPreset}
                    onChange={(e) => setPlainPreset(e.target.value)}
                    className={field}
                  >
                    {presets.map((p) => (
                      <option key={p.name} value={p.name}>
                        {p.name}
                        {p.labels.length > 0 ? ` · ${p.labels.join(", ")}` : ""}
                      </option>
                    ))}
                  </select>
                </label>
              ) : null}
            </div>
          )}

          {/* The auto-selected preset, read-only — the ticket tabs never pick one by hand. */}
          {mode !== "plain" ? (
            <p className="mt-3 text-xs font-medium text-slate-500 dark:text-slate-400">
              Preset{" "}
              <span className="font-normal text-slate-700 dark:text-slate-200">
                {effectivePreset ? (
                  <>
                    <span className="font-medium">{effectivePreset.name}</span>
                    {effectivePreset.labels.length > 0
                      ? ` · ${effectivePreset.labels.join(", ")}`
                      : ""}
                  </>
                ) : mode === "existing" && parsed && presets.length > 0 ? (
                  // Blocking, not cosmetic: with no preset dropdown there's nothing to
                  // override the auto-selection with, so say what to fix.
                  <span className="text-red-600 dark:text-red-400">
                    no preset claims {parsed.prefix.toUpperCase()} — add it to a preset’s
                    ticket-id prefixes in Settings
                  </span>
                ) : (
                  <span className="text-slate-400 dark:text-slate-500">—</span>
                )}
              </span>
            </p>
          ) : null}

          {groups.length > 0 ? (
            <label className={`mt-3 ${label}`}>
              Account group override
              <AccountGroupSelect
                groups={groups}
                value={shownGroup}
                blankLabel={
                  mode === "plain" ? "Preset default" : "Follows the ticket’s preset"
                }
                onChange={setGroupOverride}
                className={field}
              />
            </label>
          ) : null}

          {linearKeyMissing ? (
            <p className="mt-3 text-[11px] text-red-600 dark:text-red-400">
              {presets.length === 0
                ? mode === "create"
                  ? "Creating a ticket needs a preset with a Linear API key — add one in Settings."
                  : "Looking up a ticket needs a preset with a Linear API key — add one in Settings."
                : mode === "create"
                  ? `Preset “${effectivePreset?.name ?? "—"}” has no Linear API key — creating a ticket needs one. Add it in Settings, or pick a team whose preset has one.`
                  : "No preset has a Linear API key — looking up a ticket needs one. Add it in Settings."}
            </p>
          ) : null}

          {mode !== "plain" ? (
            <div className="mt-3 space-y-3 text-xs">
              <label className={`${label} font-medium`}>
                Clone agent instructions
                <textarea
                  value={agentInstructions}
                  onChange={(e) => setAgentInstructions(e.target.value)}
                  rows={2}
                  placeholder={
                    'Appended to the default ("Follow your \"Implementing a ticket\" procedure"); takes precedence where they conflict.'
                  }
                  className={`resize-y ${field}`}
                />
              </label>
              <label className={`${label} font-medium`}>
                Claude Code instructions
                <textarea
                  value={claudeInstructions}
                  onChange={(e) => setClaudeInstructions(e.target.value)}
                  rows={3}
                  placeholder="Appended to the default (pull latest → switch to the feature branch → setup docs → implement); takes precedence where they conflict."
                  className={`resize-y ${field}`}
                />
              </label>
            </div>
          ) : null}

          <label className="mt-3 flex cursor-pointer items-center gap-2 text-xs font-medium text-slate-500 dark:text-slate-400">
            <input
              type="checkbox"
              checked={headless}
              onChange={(e) => setHeadless(e.target.checked)}
              className="h-3.5 w-3.5 rounded border-slate-300 text-emerald-600 focus:ring-emerald-500 dark:border-slate-600"
            />
            Headless (no desktop)
          </label>

          {parentCandidate ? (
            <label className="mt-2 flex cursor-pointer items-center gap-2 text-xs font-medium text-slate-500 dark:text-slate-400">
              <input
                type="checkbox"
                checked={asSubClone}
                onChange={(e) => setAsSubClone(e.target.checked)}
                className="h-3.5 w-3.5 rounded border-slate-300 text-emerald-600 focus:ring-emerald-500 dark:border-slate-600"
              />
              <span>
                Sub clone of{" "}
                <span className="font-mono text-slate-700 dark:text-slate-200">
                  {parentCandidate.displayName || parentCandidate.id}
                </span>
              </span>
            </label>
          ) : null}
        </div>

        {error ? (
          <p className="mt-3 shrink-0 text-[11px] text-red-600 dark:text-red-400">{error}</p>
        ) : null}

        {op ? (
          <div className="mt-3 shrink-0">
            <OperationProgress op={op} />
          </div>
        ) : null}

        <div className="mt-4 flex shrink-0 justify-end gap-2">
          <button
            type="button"
            onClick={onClose}
            disabled={busy}
            className="rounded-md px-3 py-1.5 text-sm text-slate-600 hover:bg-slate-100 disabled:opacity-40 dark:text-slate-300 dark:hover:bg-slate-800"
          >
            Cancel
          </button>
          <button
            type="button"
            onClick={submit}
            disabled={!valid || busy}
            className="rounded-md bg-emerald-600 px-4 py-1.5 text-sm font-medium text-white hover:bg-emerald-700 disabled:opacity-40"
          >
            {busy ? "Cloning…" : mode === "create" ? "Create & clone" : "Clone"}
          </button>
        </div>
      </div>
    </div>
  );
}
