// The clone dialog's markup: four tabs (three fork modes plus template create), the
// account overrides, and the button bar. It renders from props alone — no config fetch,
// no fork/clone POST, no operation stream — so every state it can be in is a story.
// CloneModalContainer owns all three of those and hands the results down.
//
// The first three tabs fork the picked source clone; the fourth creates from a template
// image onto a fresh empty home dataset (headed unless asked, preset defaults underneath
// the account overrides).
//
// It is a glass dialog laid out as Settings (`~/desk/control/SettingsView.tsx`): the modes a
// rail of buttons on the left with Create at its foot (where Settings has Save), the form
// scrolling beside it. Its fields are drawn flat on the glass (`.on-glass`, `~/desk/glass.css`).
// Its corners: the dialog's 26 px, and 10 px for what sits 16 px in from them
// (`~/desk/GLASS.md`, Shape).
//
// The form is one editable model (`CloneDraft`) plus a single `onDraftChange`, rather than
// thirty value/onChange pairs. What is NOT in the draft is everything the server decides:
// the presets, the team keys, the preset a ticket prefix resolved to, whether the request
// would be rejected for a missing Linear key, and whether the button may fire at all.
import { Boxes, FilePlus, MessageSquare, Ticket } from "lucide-react";
import type { LucideIcon } from "lucide-react";

import { OperationProgress } from "~/components/OperationProgress";
import { CloneAccountFields } from "~/components/CloneAccountFields";
import {
  CloneExistingTicketFields,
  type ParsedTicket,
} from "~/components/CloneExistingTicketFields";
import { CloneNewTicketFields } from "~/components/CloneNewTicketFields";
import { CloneOptionsRow } from "~/components/CloneOptionsRow";
import { ClonePlainFields } from "~/components/ClonePlainFields";
import { DropdownSelect } from "~/components/DropdownSelect";
import { ModalShell } from "~/components/ModalShell";
import {
  cloneRow,
  cloneRowField,
  cloneRowLabel,
  cloneRowLabelTop,
  cloneRowTop,
  cloneSectionCaption,
} from "~/components/cloneFieldStyles";
import type { CloneDraft, CloneMode, TeamKey } from "~/lib/cloneDraft";
import type { ClaudeUsage, Clone, Operation } from "~/lib/types";
import type { CloneGroup } from "~/lib/wire/CloneGroup";
import type { PresetRedacted } from "~/lib/wire/PresetRedacted";

import type { ReactNode } from "react";

export interface CloneModalViewProps {
  /** The whole form, as one editable model. */
  draft: CloneDraft;
  /** Write one field back. The container holds the draft; this is how a keystroke reaches it. */
  onDraftChange: <K extends keyof CloneDraft>(
    key: K,
    value: CloneDraft[K],
  ) => void;

  /** Live clones to fork from. The container passes only forkable rows (managed, not
   *  archived); an empty list is the empty state, not an error. */
  clones: Clone[];
  /** Imported accounts, both providers in one flat list, so the two pickers can label each
   *  option with its usage. */
  accounts: ClaudeUsage[];
  /** The single configured pool list (`config.groups`). */
  groups: CloneGroup[];
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

  /** The markdown editor for the New-ticket body, as a slot: the real one is browser-only and
   *  lazy-loaded, so the container decides when and how it mounts. */
  descriptionEditor: ReactNode;

  /** The Fork button may fire. */
  valid: boolean;
  /** A fork is being started, or one is running. Locks the form and both buttons. */
  busy: boolean;
  /** The failed attempt, in the dialog's own words rather than the page banner's. */
  error: string | null;
  /** The fork operation this dialog started, once it shows up in the live op list. */
  operation: Operation | null;
  onSubmit: () => void;
  /** The dialog is on screen. The container flips it false when the clone operation settles,
   *  and the dialog plays its exit before the unmount rather than blinking out. */
  open?: boolean;
  /** The dialog is finished: unmount it. The exit frames have already played. */
  onClose: () => void;
}

/** The rail: which kind of clone this is going to be. The four entries are four different
 *  requests, not four views of one — the fields beside them change, and so does the server
 *  route the Create button ends up hitting. */
const MODES: { mode: CloneMode; label: string; icon: LucideIcon }[] = [
  { mode: "existing", label: "Existing ticket", icon: Ticket },
  { mode: "create", label: "New ticket", icon: FilePlus },
  { mode: "plain", label: "No ticket", icon: MessageSquare },
  { mode: "template", label: "From template", icon: Boxes },
];

/** The flat content on the glass (`~/desk/control/ui.tsx`), for the rail. */
const G = {
  title: "text-neutral-900 dark:text-white",
  text2: "text-black/60 dark:text-white/60",
  line: "border-black/10 dark:border-white/10",
  press: "hover:bg-black/5 active:bg-black/10 dark:hover:bg-white/5 dark:active:bg-white/10",
  lit: "bg-neutral-900 text-white dark:bg-white dark:text-neutral-900",
};

export function CloneModalView({
  draft,
  onDraftChange,
  clones,
  accounts,
  groups,
  presets,
  teamKeys,
  parsedTicket,
  preset,
  linearKeyMissing,
  descriptionEditor,
  valid,
  busy,
  error,
  operation,
  onSubmit,
  open = true,
  onClose,
}: CloneModalViewProps) {
  /** The form beside the rail: one mode's fields. */
  const form = (
    <>
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
        onPriorityChange={(priority) =>
          onDraftChange("priority", priority)
        }
      />
    ) : draft.mode === "plain" ? (
      <ClonePlainFields
        title={draft.title}
        message={draft.message}
        presets={presets}
        preset={draft.plainPreset}
        onTitleChange={(title) => onDraftChange("title", title)}
        onMessageChange={(message) =>
          onDraftChange("message", message)
        }
        onPresetChange={(name) => onDraftChange("plainPreset", name)}
        onSubmit={onSubmit}
      />
    ) : (
      <div className="space-y-3">
        <label className={cloneRow}>
          <span className={cloneRowLabel}>Clone title</span>
          <input
            value={draft.title}
            disabled={busy}
            onChange={(e) => onDraftChange("title", e.target.value)}
            placeholder="encoder-scratch"
            className={cloneRowField}
            onKeyDown={(e) => {
              if (e.key === "Enter") onSubmit();
            }}
          />
        </label>
        <label className={cloneRow}>
          <span className={cloneRowLabel}>Preset</span>
          <DropdownSelect
            rows={
              presets.length === 0
                ? [
                    {
                      value: "",
                      label: "No presets configured",
                      disabled: true,
                    },
                  ]
                : presets.map((p) => ({
                    value: p.name,
                    label: p.name,
                  }))
            }
            value={draft.templatePreset}
            onChange={(name) => onDraftChange("templatePreset", name)}
            disabled={busy || presets.length === 0}
            label="Preset"
            className={cloneRowField}
          />
        </label>
      </div>
    )}

    {/* The template tab takes the same account overrides as the fork tabs, minus a
    source to inherit from: the container fills them from the picked preset
    until touched. Headless lives beside the startup toggle, exactly as on
    the fork tabs. */}
    {draft.mode === "template" ? (
      <>
        <section className="border-t border-slate-100 pt-4 dark:border-slate-800">
          <h4 className={`${cloneSectionCaption} mb-3`}>Accounts</h4>
          <CloneAccountFields
            accounts={accounts}
            groups={groups}
            group={draft.group}
            claudeAccount={draft.claudeAccount}
            codexAccount={draft.codexAccount}
            onGroupChange={(value) => onDraftChange("group", value)}
            onClaudeAccountChange={(value) =>
              onDraftChange("claudeAccount", value)
            }
            onCodexAccountChange={(value) =>
              onDraftChange("codexAccount", value)
            }
          />
        </section>
        <section className="border-t border-slate-100 pt-4 dark:border-slate-800">
          <h4 className={`${cloneSectionCaption} mb-3`}>Options</h4>
          <CloneOptionsRow
            headless={draft.headless}
            onHeadlessChange={(headless) =>
              onDraftChange("headless", headless)
            }
            runStartupScript={draft.runStartupScript}
            onRunStartupScriptChange={(run) =>
              onDraftChange("runStartupScript", run)
            }
            rebuild={draft.rebuild}
            onRebuildChange={(rebuild) =>
              onDraftChange("rebuild", rebuild)
            }
          />
        </section>
      </>
    ) : null}

    {/* Fork source: blank until a preset resolves, then the preset's default fork
    clone (live or archived) fills in directly. Only the fork
    tabs take a source. */}
    {draft.mode === "template" ? null : (
      <section className="border-t border-slate-100 pt-4 dark:border-slate-800">
        <h4 className={`${cloneSectionCaption} mb-3`}>Source</h4>
        <label className={cloneRow}>
          <span className={cloneRowLabel}>Source clone to fork</span>
          <DropdownSelect
            rows={
              clones.length === 0
                ? [
                    {
                      value: "",
                      label: "No forkable clones",
                      disabled: true,
                    },
                  ]
                : clones.map((c) => ({
                    value: c.id,
                    label: c.archived ? `${c.id} (archived)` : c.id,
                  }))
            }
            value={draft.source ?? ""}
            onChange={(id) => onDraftChange("source", id || null)}
            disabled={busy}
            label="Source clone to fork"
            className={cloneRowField}
          />
        </label>
      </section>
    )}

    {draft.mode === "template" ? null : (
      <section className="border-t border-slate-100 pt-4 dark:border-slate-800">
        <h4 className={`${cloneSectionCaption} mb-3`}>Accounts</h4>
        <CloneAccountFields
          accounts={accounts}
          groups={groups}
          group={draft.group}
          claudeAccount={draft.claudeAccount}
          codexAccount={draft.codexAccount}
          onGroupChange={(value) => onDraftChange("group", value)}
          onClaudeAccountChange={(value) =>
            onDraftChange("claudeAccount", value)
          }
          onCodexAccountChange={(value) =>
            onDraftChange("codexAccount", value)
          }
        />
      </section>
    )}

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
    {draft.mode === "existing" || draft.mode === "create" ? (
      <section className="border-t border-slate-100 pt-4 dark:border-slate-800">
        {/* The checkbox decides whether the assistant gets a first message at all;
        the two boxes below only shape that message, so unchecked greys them out. */}
        <div className="mb-3 flex flex-wrap items-center gap-x-4 gap-y-1">
          <h4 className={cloneSectionCaption}>Instructions</h4>
          <label className="flex cursor-pointer items-center gap-2 text-xs font-medium text-slate-500 dark:text-slate-400">
            <input
              type="checkbox"
              checked={draft.kickoff}
              onChange={(e) =>
                onDraftChange("kickoff", e.target.checked)
              }
              disabled={busy}
              className="h-3.5 w-3.5 rounded border-slate-300 text-emerald-600 focus:ring-emerald-500 dark:border-slate-600"
            />
            Auto send first message to the assistant
          </label>
        </div>
        <div
          className={`space-y-3 text-xs ${draft.kickoff ? "" : "opacity-50"}`}
        >
          <label className={cloneRowTop}>
            <span className={cloneRowLabelTop}>
              Clone agent instructions
            </span>
            <textarea
              value={draft.agentInstructions}
              onChange={(e) =>
                onDraftChange("agentInstructions", e.target.value)
              }
              disabled={!draft.kickoff}
              rows={5}
              placeholder={
                'Appended to the default ("Follow your "Implementing a ticket" procedure"); takes precedence where they conflict.'
              }
              className={`resize-y disabled:cursor-not-allowed ${cloneRowField}`}
            />
          </label>
          <label className={cloneRowTop}>
            <span className={cloneRowLabelTop}>
              Claude Code instructions
            </span>
            <textarea
              value={draft.claudeInstructions}
              onChange={(e) =>
                onDraftChange("claudeInstructions", e.target.value)
              }
              disabled={!draft.kickoff}
              rows={5}
              placeholder="Appended to the default (pull latest → switch to the feature branch → setup docs → implement); takes precedence where they conflict."
              className={`resize-y disabled:cursor-not-allowed ${cloneRowField}`}
            />
          </label>
        </div>
      </section>
    ) : null}

    {draft.mode === "template" ? null : (
      <section className="border-t border-slate-100 pt-4 dark:border-slate-800">
        <h4 className={`${cloneSectionCaption} mb-3`}>Options</h4>
        <CloneOptionsRow
          headless={draft.headless}
          onHeadlessChange={(headless) =>
            onDraftChange("headless", headless)
          }
          runStartupScript={draft.runStartupScript}
          onRunStartupScriptChange={(run) =>
            onDraftChange("runStartupScript", run)
          }
          rebuild={draft.rebuild}
          onRebuildChange={(rebuild) =>
            onDraftChange("rebuild", rebuild)
          }
        />
      </section>
    )}
    </>
  );

  return (
    // Neither Cancel nor Escape may close over a running clone operation, so both are held by
    // the same `busy`.
    <ModalShell size="panel" dismissible={!busy} open={open} onExited={onClose} glass>
      {(close) => (
        // Settings' layout: the rail (16 px in from the dialog's 26 px corners, its buttons 10 px)
        <div className="on-glass flex min-h-0 flex-1">
          <nav aria-label="Clone modes" className={`flex w-52 shrink-0 flex-col gap-1 border-r p-4 ${G.line}`}>
            <h3 className={`px-2 pb-2 text-[15px] font-semibold tracking-tight ${G.title}`}>New clone</h3>
            {MODES.map(({ mode, label, icon: Icon }) => (
              <button
                key={mode}
                type="button"
                disabled={busy}
                onClick={() => onDraftChange("mode", mode)}
                aria-current={mode === draft.mode ? "page" : undefined}
                className={`flex h-9 items-center gap-2 rounded-[10px] px-2 text-left text-sm disabled:opacity-50 ${
                  mode === draft.mode ? `${G.lit} font-medium` : `${G.text2} ${G.press}`
                }`}
              >
                <Icon aria-hidden className="size-4 shrink-0" />
                <span className="truncate">{label}</span>
              </button>
            ))}
            <span className="flex-1" />
            <button
              type="button"
              onClick={() => close()}
              disabled={busy}
              className={`flex h-9 items-center justify-center rounded-[10px] text-sm disabled:opacity-40 ${G.text2} ${G.press}`}
            >
              Cancel
            </button>
            <button
              type="button"
              onClick={onSubmit}
              disabled={!valid || busy}
              className="flex h-9 items-center justify-center rounded-[10px] bg-emerald-600 text-sm font-medium text-white hover:bg-emerald-500 disabled:opacity-40"
            >
              {busy ? "Creating…" : "Create"}
            </button>
          </nav>
          <div className="flex min-w-0 flex-1 flex-col">
            <div className="min-h-0 flex-1 space-y-4 overflow-y-auto overscroll-y-contain p-4 [&>section:first-child]:border-t-0 [&>section:first-child]:pt-0">
              {form}
            </div>
            {error || operation ? (
              <div className={`shrink-0 space-y-2 border-t p-4 ${G.line}`}>
                {error ? <p className="text-[11px] text-rose-700 dark:text-rose-300">{error}</p> : null}
                {operation ? <OperationProgress op={operation} /> : null}
              </div>
            ) : null}
          </div>
        </div>
      )}
    </ModalShell>
  );
}
