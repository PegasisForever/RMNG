// The clone dialog's markup: a source image, one of three ticket modes, the account
// overrides, and the button bar. It renders from props alone — no config fetch, no clone POST,
// no operation stream — so every state it can be in is a story. CloneModalContainer owns all
// three of those and hands the results down.
//
// The form is one editable model (`CloneDraft`) plus a single `onDraftChange`, rather than
// thirty value/onChange pairs. What is NOT in the draft is everything the server decides:
// the presets, the team keys, the preset a ticket prefix resolved to, whether the request
// would be rejected for a missing Linear key, and whether the button may fire at all.
import { ImagePicker } from "~/components/ImagePicker";
import { OperationProgress } from "~/components/OperationProgress";
import { CloneAccountFields } from "~/components/CloneAccountFields";
import { CloneExistingTicketFields, type ParsedTicket } from "~/components/CloneExistingTicketFields";
import { CloneModeTabs } from "~/components/CloneModeTabs";
import { CloneNewTicketFields } from "~/components/CloneNewTicketFields";
import { CloneOptionsRow } from "~/components/CloneOptionsRow";
import { ClonePlainFields } from "~/components/ClonePlainFields";
import { cloneField, cloneLabel } from "~/components/cloneFieldStyles";
import type { CloneDraft, TeamKey } from "~/lib/cloneDraft";
import type { ClaudeUsage, Clone, Operation } from "~/lib/types";
import type { CloneGroup } from "~/lib/wire/CloneGroup";
import type { ImageInfo } from "~/lib/wire/ImageInfo";
import type { PresetRedacted } from "~/lib/wire/PresetRedacted";
import { useModalEscape } from "~/lib/useModalEscape";

import type { ReactNode } from "react";

export interface CloneModalViewProps {
  /** The whole form, as one editable model. */
  draft: CloneDraft;
  /** Write one field back. The container holds the draft; this is how a keystroke reaches it. */
  onDraftChange: <K extends keyof CloneDraft>(key: K, value: CloneDraft[K]) => void;

  /** Clone-source images to pick from (from `listImages`). */
  images: ImageInfo[];
  imagesLoading: boolean;
  /** Wall-clock milliseconds, for the age each image row shows. Captured when the dialog
   *  opens rather than read per render: an image's age has no reason to tick, and a leaf
   *  that read the clock would draw something different in every story run. */
  now: number;
  /** Imported accounts, both providers in one flat list, so the two pickers can label each
   *  option with its usage. */
  accounts: ClaudeUsage[];
  /** Configured Claude pools (`config.cloneGroups`). */
  claudeGroups: CloneGroup[];
  /** Configured Codex pools (`config.codexGroups`). */
  codexGroups: CloneGroup[];
  /** Every configured preset, in config order. */
  presets: PresetRedacted[];
  /** The team keys the presets declare, for the New-ticket tab's dropdown. */
  teamKeys: TeamKey[];
  /** What `parseTicketInput` made of `draft.ticket`, or null when there is no id in it. */
  parsedTicket: ParsedTicket | null;
  /** The preset that will actually drive the clone, given the open tab. */
  preset: PresetRedacted | undefined;
  /** The request this tab would send needs a Linear API key nobody has configured. */
  linearKeyMissing: boolean;
  /** The currently selected clone, offered as a sub-clone parent. Null = nothing selected, or
   *  the selection can't be a parent (unmanaged, or already a sub clone). */
  parentCandidate: Clone | null;

  /** The markdown editor for the New-ticket body, as a slot: the real one is browser-only and
   *  lazy-loaded, so the container decides when and how it mounts. */
  descriptionEditor: ReactNode;

  /** The Clone button may fire. */
  valid: boolean;
  /** A clone is being started, or one is running. Locks the form and both buttons. */
  busy: boolean;
  /** The failed attempt, in the dialog's own words rather than the page banner's. */
  error: string | null;
  /** The clone operation this dialog started, once it shows up in the live op list. */
  operation: Operation | null;
  onSubmit: () => void;
  onClose: () => void;
}

export function CloneModalView({
  draft,
  onDraftChange,
  images,
  imagesLoading,
  now,
  accounts,
  claudeGroups,
  codexGroups,
  presets,
  teamKeys,
  parsedTicket,
  preset,
  linearKeyMissing,
  parentCandidate,
  descriptionEditor,
  valid,
  busy,
  error,
  operation,
  onSubmit,
  onClose,
}: CloneModalViewProps) {
  // Escape closes regardless of focus — a document-level listener since the backdrop click no
  // longer does (see below). Guarded the same as the backdrop was: no closing out from under a
  // running clone operation. While `busy` the dialog still holds its slot in the Escape stack,
  // so the keypress is swallowed rather than falling through to whatever is mounted beneath.
  useModalEscape(onClose, !busy);

  return (
    <div className="fixed inset-0 z-50 flex items-center justify-center bg-slate-900/30 p-4">
      {/* Backdrop is inert — clicking it must not close the dialog (nor could it while
          `busy`); only Cancel/Escape do, both guarded against closing over a running
          clone operation. One height for every tab. The pin is on the whole scroll body,
          not on the tab-specific block: the tabs also differ BELOW that block (No ticket
          shows no preset line and no instruction overrides), so pinning only the block
          still left this tab shorter. 49.5rem is the tallest tab (New ticket, ~786px);
          the shortest (No ticket, ~462px) simply carries the slack as empty space above
          the button bar, which stays pinned to the bottom. `max-h-[90vh]` is the fallback
          for a genuinely short viewport, not the normal path. */}
      <div className="flex max-h-[90vh] w-full max-w-lg flex-col rounded-xl border border-slate-200 bg-white p-5 shadow-xl dark:border-slate-700 dark:bg-slate-800">
        <h3 className="shrink-0 text-sm font-semibold text-slate-900 dark:text-slate-100">
          New clone
        </h3>

        <div className="h-[49.5rem] min-h-0 shrink overflow-y-auto pr-0.5">
          <div className="mt-3 text-xs font-medium text-slate-500 dark:text-slate-400">
            Source image
            <ImagePicker
              images={images}
              loading={imagesLoading}
              value={draft.image}
              now={now}
              onChange={(reference) => onDraftChange("image", reference)}
            />
          </div>

          <div className="mt-3">
            <CloneModeTabs
              mode={draft.mode}
              disabled={busy}
              onModeChange={(mode) => onDraftChange("mode", mode)}
            />
          </div>

          {/* No height pin on the tab block — the whole scroll body above carries it, so each
              tab is free to be its natural size. */}
          {draft.mode === "existing" ? (
            <CloneExistingTicketFields
              ticket={draft.ticket}
              parsed={parsedTicket}
              preset={preset}
              presets={presets}
              onTicketChange={(ticket) => onDraftChange("ticket", ticket)}
              onSubmit={onSubmit}
            />
          ) : draft.mode === "create" ? (
            <CloneNewTicketFields
              teamKeys={teamKeys}
              team={draft.team}
              title={draft.title}
              priority={draft.priority}
              description={descriptionEditor}
              onTeamChange={(team) => onDraftChange("team", team)}
              onTitleChange={(title) => onDraftChange("title", title)}
              onPriorityChange={(priority) => onDraftChange("priority", priority)}
            />
          ) : (
            <ClonePlainFields
              title={draft.title}
              message={draft.message}
              presets={presets}
              preset={draft.plainPreset}
              onTitleChange={(title) => onDraftChange("title", title)}
              onMessageChange={(message) => onDraftChange("message", message)}
              onPresetChange={(name) => onDraftChange("plainPreset", name)}
              onSubmit={onSubmit}
            />
          )}

          <CloneAccountFields
            accounts={accounts}
            claudeGroups={claudeGroups}
            codexGroups={codexGroups}
            preset={preset}
            claudeAccount={draft.claudeAccount}
            codexAccount={draft.codexAccount}
            onClaudeAccountChange={(value) => onDraftChange("claudeAccount", value)}
            onCodexAccountChange={(value) => onDraftChange("codexAccount", value)}
          />

          {linearKeyMissing ? (
            <p className="mt-3 text-[11px] text-red-600 dark:text-red-400">
              {presets.length === 0
                ? draft.mode === "create"
                  ? "Creating a ticket needs a preset with a Linear API key — add one in Settings."
                  : "Looking up a ticket needs a preset with a Linear API key — add one in Settings."
                : draft.mode === "create"
                  ? `Preset “${preset?.name ?? "—"}” has no Linear API key — creating a ticket needs one. Add it in Settings, or pick a team whose preset has one.`
                  : "No preset has a Linear API key — looking up a ticket needs one. Add it in Settings."}
            </p>
          ) : null}

          {/* Always visible (no expander), stacked — their placeholders are long enough that
              a half-width column truncates them to uselessness. Five rows each: they take
              prose, and the dialog has the room now that only the Existing tab carries the
              preset line. Still resizable. */}
          {draft.mode !== "plain" ? (
            <div className="mt-3 space-y-2 text-xs">
              <label className={`${cloneLabel} font-medium`}>
                Clone agent instructions
                <textarea
                  value={draft.agentInstructions}
                  onChange={(e) => onDraftChange("agentInstructions", e.target.value)}
                  rows={5}
                  placeholder={
                    'Appended to the default ("Follow your \"Implementing a ticket\" procedure"); takes precedence where they conflict.'
                  }
                  className={`resize-y ${cloneField}`}
                />
              </label>
              <label className={`${cloneLabel} font-medium`}>
                Claude Code instructions
                <textarea
                  value={draft.claudeInstructions}
                  onChange={(e) => onDraftChange("claudeInstructions", e.target.value)}
                  rows={5}
                  placeholder="Appended to the default (pull latest → switch to the feature branch → setup docs → implement); takes precedence where they conflict."
                  className={`resize-y ${cloneField}`}
                />
              </label>
            </div>
          ) : null}

          <CloneOptionsRow
            headless={draft.headless}
            parentCandidate={parentCandidate}
            asSubClone={draft.asSubClone}
            onHeadlessChange={(headless) => onDraftChange("headless", headless)}
            onAsSubCloneChange={(asSubClone) => onDraftChange("asSubClone", asSubClone)}
          />
        </div>

        {error ? (
          <p className="mt-3 shrink-0 text-[11px] text-red-600 dark:text-red-400">{error}</p>
        ) : null}

        {operation ? (
          <div className="mt-3 shrink-0">
            <OperationProgress op={operation} />
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
            onClick={onSubmit}
            disabled={!valid || busy}
            className="rounded-md bg-emerald-600 px-4 py-1.5 text-sm font-medium text-white hover:bg-emerald-700 disabled:opacity-40"
          >
            {busy ? "Cloning…" : draft.mode === "create" ? "Create & clone" : "Clone"}
          </button>
        </div>
      </div>
    </div>
  );
}
