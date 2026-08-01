// The settings overlay's markup: a scrolling stack of sections over a pinned footer. It
// renders from props alone — no config fetch, no save, no update check, no localStorage — so
// every state it can be in is a story. SettingsPanelContainer owns all of those and hands the
// results down.
//
// The form is one editable model (`SettingsDraft`) plus a single `onDraftChange`, rather than
// forty value/onChange pairs. What is NOT in the draft is everything the server decides:
// whether first-run setup has finished, what the last save said about needing a restart, the
// control-server's own version, and the result of the Docker probe.
//
// Sections that read as one control (the two prompt textareas, the chroma picker, the board
// columns, the images list) stay here. Everything with its own states, or rendered twice for
// the two providers, is its own component with its own story.
import { X } from "lucide-react";
import { useState } from "react";

import { BoardColumnsEditor } from "~/components/BoardColumnsEditor";
import { ImagesSection } from "~/components/ImagesSection";
import { SettingsAccountList } from "~/components/SettingsAccountList";
import { SettingsAdvancedSection } from "~/components/SettingsAdvancedSection";
import { SettingsDockerSection } from "~/components/SettingsDockerSection";
import { Field, Section, settingsInput } from "~/components/SettingsFields";
import { SettingsGroupsEditor } from "~/components/SettingsGroupsEditor";
import { SettingsLayoutPresets } from "~/components/SettingsLayoutPresets";
import { SettingsPresetList } from "~/components/SettingsPresetList";
import { SettingsProviderFields } from "~/components/SettingsProviderFields";
import { SettingsServerSection } from "~/components/SettingsServerSection";
import { SettingsSshSection } from "~/components/SettingsSshSection";
import type { AcctOrder } from "~/lib/accountOrder";
import type { BoardColumn } from "~/lib/board";
import { orderedAccounts, type SettingsDraft } from "~/lib/settingsDraft";
import type { ClaudeUsage, Operation } from "~/lib/types";
import { useModalEscape } from "~/lib/useModalEscape";
import type { ChromaMode } from "~/lib/wire/ChromaMode";
import type { ImageInfo } from "~/lib/wire/ImageInfo";
import type { UpdateStatus } from "~/lib/wire/UpdateStatus";

export interface SettingsPanelViewProps {
  /** The whole form, as one editable model. Null while the config is still in flight, which
   *  is what draws the loading state and hides the footer. */
  draft: SettingsDraft | null;
  /** Write one field back. The container holds the draft; this is how a keystroke reaches it. */
  onDraftChange: <K extends keyof SettingsDraft>(key: K, value: SettingsDraft[K]) => void;

  /** Live per-account usage (from `ControlState.claudeAccounts`). Despite the field name it
   *  carries BOTH providers' rows tagged by `provider`; the two account and group sections
   *  split it by that tag. Used here only for the emails. */
  accounts: ClaudeUsage[];
  /** The operator's own cosmetic ordering of those rows, per provider. It orders the two
   *  account lists and their group checkboxes. The preset pickers below take the list
   *  unordered, the way a dropdown of options rather than a list of rows wants it. */
  accountOrder: AcctOrder;
  /** The new order for one provider's rows, after a drag. */
  onReorderAccounts: (provider: "claude" | "codex", orderedIds: string[]) => void;
  /** Delete an imported Claude account by email (removes its stored token; reassigns clones). */
  onDeleteAccount: (email: string) => void;
  /** Delete an imported Codex account by email. */
  onDeleteCodexAccount: (email: string) => void;
  /** Open the import-from-a-clone modal. Accounts are never OAuth'd in the browser — the
   *  control-server harvests the tokens off a clone that's already signed in. */
  onImportAccount: () => void;

  /** First-run setup has finished, so the clone subnet is read-only. */
  setupComplete: boolean;
  /** The last failed load or save, in the panel's own banner. */
  error: string | null;
  /** The last save touched a port/socket/video setting, so the restart banner stands until a
   *  later save clears it. */
  restartRequired: boolean;
  saving: boolean;
  /** The post-save confirmation, which the container clears after a beat. */
  saved: boolean;
  onSave: () => void;
  onClose: () => void;

  /** The control-server's own version and update-available answer. */
  serverStatus: UpdateStatus | null;
  /** The last thing the panel has to say about a check, an update or a restart. */
  serverMessage: string | null;
  /** The self-update operation, once it shows up in the live op list. */
  updateOperation: Operation | null;
  /** The Update button is dead: nothing to update to, or an update is already running. */
  updateDisabled: boolean;
  onCheckUpdate: () => void;
  onUpdateServer: () => void;
  onRestartServer: () => void;

  /** The result of the last Docker probe. */
  testMessage: string | null;
  onTestDocker: () => void;

  /** Clone-source images (moved here from the sidebar). */
  images: ImageInfo[];
  imagesLoading: boolean;
  /** True while a template-pull op is running (disables the pull action). */
  pullBusy: boolean;
  /** Wall-clock milliseconds, for each image row's age. */
  now: number;
  /** Re-pull the configured template reference. The container confirms first. */
  onPullLatestImage: () => void;
  /** Pull some other reference. The container asks which one. */
  onPullOtherImage: () => void;
  /** Delete an image. The container confirms first. */
  onDeleteImage: (reference: string) => void;

  /** The dashboard board's columns, left to right. Omit to hide the section entirely,
   *  which is what a page without a board does. */
  boardColumns?: BoardColumn[];
  /** Clones per column id, so a delete can say what it displaces. */
  boardColumnCounts?: Record<string, number>;
  onAddBoardColumn?: (title: string) => void;
  onRenameBoardColumn?: (columnId: string, title: string) => void;
  onSetBoardColumnArchive?: (columnId: string, archive: boolean) => void;
  onDeleteBoardColumn?: (columnId: string) => void;
  onReorderBoardColumns?: (columnIds: string[]) => void;
}

export function SettingsPanelView({
  draft,
  onDraftChange,
  accounts,
  accountOrder,
  onReorderAccounts,
  onDeleteAccount,
  onDeleteCodexAccount,
  onImportAccount,
  setupComplete,
  error,
  restartRequired,
  saving,
  saved,
  onSave,
  onClose,
  serverStatus,
  serverMessage,
  updateOperation,
  updateDisabled,
  onCheckUpdate,
  onUpdateServer,
  onRestartServer,
  testMessage,
  onTestDocker,
  images,
  imagesLoading,
  pullBusy,
  now,
  onPullLatestImage,
  onPullOtherImage,
  onDeleteImage,
  boardColumns,
  boardColumnCounts,
  onAddBoardColumn,
  onRenameBoardColumn,
  onSetBoardColumnArchive,
  onDeleteBoardColumn,
  onReorderBoardColumns,
}: SettingsPanelViewProps) {
  // Escape closes. Stacked: the import modal opens ON TOP of this panel (z-60 over z-50),
  // and without the stack one Escape would close both — losing the panel as collateral for
  // dismissing the dialog above it. The stack is LIFO, so the import modal (mounted later)
  // owns Escape while it is up; this panel takes it back when the dialog unmounts.
  useModalEscape(onClose);

  // Whether the ports-and-directories block is expanded. Ephemeral: it is not part of the
  // form, nothing outside this panel reads it, and it resets every time the panel opens.
  const [advanced, setAdvanced] = useState(false);

  const rows = orderedAccounts(accounts, accountOrder);

  return (
    <div className="fixed inset-0 z-50 flex items-center justify-center bg-slate-900/30 p-4">
      {/* Backdrop is inert — clicking it must not close the panel, only the ✕/Cancel
          buttons and Escape (handled above) do. */}
      <div className="flex max-h-[90vh] w-full max-w-2xl flex-col overflow-hidden rounded-xl border border-slate-200 dark:border-slate-700 bg-white dark:bg-slate-800 shadow-xl">
        {/* Scrollable body. The footer lives outside this so it stays flush to the
            panel's bottom edge instead of floating above the scroll container's padding. */}
        <div className="min-h-0 flex-1 overflow-y-auto p-5">
        <div className="mb-2 flex items-center justify-between">
          <h2 className="text-base font-semibold text-slate-900 dark:text-slate-100">Settings</h2>
          <button
            type="button"
            onClick={onClose}
            className="rounded p-1 text-slate-400 dark:text-slate-500 hover:bg-slate-100 dark:hover:bg-slate-800 hover:text-slate-600 dark:hover:text-slate-300"
            aria-label="Close"
          >
            <X className="size-4" />
          </button>
        </div>

        {error ? (
          <div className="mb-3 rounded border border-red-200 dark:border-red-900 bg-red-50 dark:bg-red-950/40 px-3 py-2 text-xs text-red-700 dark:text-red-400">
            {error}
          </div>
        ) : null}

        {restartRequired ? (
          <div className="mb-3 flex items-center gap-3 rounded border border-amber-300 dark:border-amber-900 bg-amber-50 dark:bg-amber-950/40 px-3 py-2 text-xs text-amber-800 dark:text-amber-400">
            <span>Changed port/socket/video settings need a restart to apply.</span>
            <button
              type="button"
              onClick={onRestartServer}
              className="rounded border border-amber-400 dark:border-amber-700 px-2 py-1 text-xs font-medium text-amber-800 dark:text-amber-300 hover:bg-amber-100 dark:hover:bg-amber-900/40"
            >
              Restart control-server
            </button>
          </div>
        ) : null}

        {!draft ? (
          <p className="py-8 text-center text-sm text-slate-400 dark:text-slate-500">Loading…</p>
        ) : (
          <div className="space-y-4">
            {/* Board columns — the dashboard's swim lanes. Clones move between them by drag
                on the board itself; the columns are made and ordered here. */}
            {boardColumns ? (
              <Section
                title="Board columns"
                effect="immediate"
                hint="The dashboard board's columns, left to right. Drag to reorder. Tick archive to make a column stop and keep the clones dropped into it, and start them again when they are dragged out. A clone in no column shows up in the first one."
              >
                <BoardColumnsEditor
                  columns={boardColumns}
                  counts={boardColumnCounts}
                  onAddColumn={(title) => onAddBoardColumn?.(title)}
                  onRenameColumn={(id, title) => onRenameBoardColumn?.(id, title)}
                  onSetArchive={(id, archive) => onSetBoardColumnArchive?.(id, archive)}
                  onDeleteColumn={(id) => onDeleteBoardColumn?.(id)}
                  onReorderColumns={(ids) => onReorderBoardColumns?.(ids)}
                />
              </Section>
            ) : null}

            {/* Layout presets — named monitor arrangements; switch the active one from
                the sidebar. Each preset uses the same editor as before. */}
            <Section
              title="Layout presets"
              effect="immediate"
              hint="Named monitor arrangements. Switch the active preset from the sidebar — running clones reconfigure live without closing apps."
            >
              <SettingsLayoutPresets
                presets={draft.layoutPresets}
                onChange={(presets) => onDraftChange("layoutPresets", presets)}
              />
            </Section>

            {/* Global agent prompt (layer a) — the shared operating memory EVERY agent reads
                as its native global rules (CLAUDE.md / AGENTS.md). Kept in sync into existing
                clones by the reconciler. */}
            <Section
              title="Global agent prompt (all presets)"
              effect="immediate"
              hint="General engineering guidance written to every agent's native rules file (Claude CLAUDE.md, Codex AGENTS.md, and read by the node-agent). Applies to all presets; edits sync into existing clones. Keep desktop/Cursor ticket procedure OUT of this — that belongs in the node-agent prompt below (the inner Cursor Claude Code reads this file and would recurse)."
            >
              <textarea
                value={draft.globalPrompt}
                onChange={(e) => onDraftChange("globalPrompt", e.target.value)}
                spellCheck={false}
                rows={12}
                className="w-full rounded border border-slate-300 dark:border-slate-600 px-2 py-1 font-mono text-xs focus:border-slate-400 dark:focus:border-slate-500 focus:outline-none dark:bg-slate-800 dark:text-slate-100"
              />
            </Section>

            {/* Node-agent additional prompt (layer b) — the desktop agent's operating notes /
                ticket procedure, appended to its system prompt at clone time. Node-agent only. */}
            <Section
              title="Node-agent additional prompt (all presets)"
              effect="immediate"
              hint="Extra system-prompt append for the node-agent ONLY (the desktop agent's operating notes + ticket procedure). Not given to Claude/Codex. Applies to newly created clones (existing clones keep what they were created with)."
            >
              <textarea
                value={draft.agentPlaybook}
                onChange={(e) => onDraftChange("agentPlaybook", e.target.value)}
                spellCheck={false}
                rows={16}
                className="w-full rounded border border-slate-300 dark:border-slate-600 px-2 py-1 font-mono text-xs focus:border-slate-400 dark:focus:border-slate-500 focus:outline-none dark:bg-slate-800 dark:text-slate-100"
              />
            </Section>

            {/* Presets — Linear identity (key + auto-select ticket-id prefixes) + env vars,
                picked (or prefix-matched) at clone time. */}
            <Section
              title="Presets"
              effect="immediate"
              hint="A preset = Linear API key + the ticket-id prefixes (Linear team keys, e.g. DEV) that auto-select it + env vars, written to the clone's session env at creation. The key is also injected as LINEAR_API_KEY (auths the clone's `linear` MCP). Cloning from a ticket auto-picks by the ticket's team prefix (DEV-196 → DEV); other clones require an explicit pick."
            >
              <SettingsPresetList
                presets={draft.presets}
                accounts={accounts}
                claudeGroups={draft.claudeGroups}
                codexGroups={draft.codexGroups}
                onChange={(presets) => onDraftChange("presets", presets)}
              />
            </Section>

            {/* Control-server — its own image version + an on-demand update check. */}
            <Section title="Control-server" effect="restart" hint="Update to the latest published image, or restart to apply changed startup settings.">
              <SettingsServerSection
                status={serverStatus}
                message={serverMessage}
                operation={updateOperation}
                updateDisabled={updateDisabled}
                onCheckUpdate={onCheckUpdate}
                onUpdate={onUpdateServer}
                onRestart={onRestartServer}
              />
            </Section>

            {/* Docker / Clones. */}
            <Section title="Docker / Clones">
              <SettingsDockerSection
                hostnamePrefix={draft.hostnamePrefix}
                templateReference={draft.templateReference}
                subnet={draft.subnet}
                subnetLocked={setupComplete}
                cloneCpus={draft.cloneCpus}
                cloneMemoryMb={draft.cloneMemoryMb}
                testMessage={testMessage}
                onHostnamePrefixChange={(v) => onDraftChange("hostnamePrefix", v)}
                onTemplateReferenceChange={(v) => onDraftChange("templateReference", v)}
                onSubnetChange={(v) => onDraftChange("subnet", v)}
                onCloneCpusChange={(v) => onDraftChange("cloneCpus", v)}
                onCloneMemoryMbChange={(v) => onDraftChange("cloneMemoryMb", v)}
                onTest={onTestDocker}
              />
            </Section>

            {/* Images — clone-source templates (pull from a registry / delete). Moved
                here from the sidebar. Prefills the pull prompt from the Template
                reference field above. */}
            <Section
              title="Images"
              effect="immediate"
              hint="Clone-source images (rmng.image=1). Pull the template from a registry (it keeps its own repo:tag) or delete an unused one; a live clone running on an image blocks its delete."
            >
              <ImagesSection
                images={images}
                loading={imagesLoading}
                pullBusy={pullBusy}
                templateRef={draft.templateReference}
                now={now}
                onPullLatest={onPullLatestImage}
                onPullOther={onPullOtherImage}
                onDelete={onDeleteImage}
              />
            </Section>

            {/* Claude. */}
            <Section title="Claude">
              <SettingsProviderFields
                pollSecs={draft.claude.pollSecs}
                pinnedEmail={draft.claude.pinnedEmail}
                onPollSecsChange={(v) => onDraftChange("claude", { ...draft.claude, pollSecs: v })}
                onPinnedEmailChange={(v) =>
                  onDraftChange("claude", { ...draft.claude, pinnedEmail: v })
                }
              />
            </Section>

            {/* Imported Claude accounts — the pool clones/groups draw from; deletable here.
                There's no in-browser login: the control-server harvests an account's tokens
                off a clone that's already signed in, hence "Import account" rather than "Add". */}
            <Section
              title="Claude accounts"
              effect="immediate"
              hint="Imported accounts available to clones and groups. Deleting one removes its stored token and reassigns clones running it (a clone pinned to it must be reassigned first)."
            >
              <SettingsAccountList
                accounts={rows.claude}
                onDelete={onDeleteAccount}
                onReorder={(ids) => onReorderAccounts("claude", ids)}
                onImport={onImportAccount}
              />
            </Section>

            {/* Claude groups (named account pools; sticky — a clone moves only when its
                account exhausts). Saved with the rest of the form via PUT /api/config. */}
            <Section
              title="Claude groups"
              effect="immediate"
              hint="A pool of accounts. A clone bound to a group keeps its account (preserving its prompt cache) until that account is exhausted (80% 5h or 95% 7d), then moves to the least-used member."
            >
              <SettingsGroupsEditor
                groups={draft.claudeGroups}
                accountEmails={rows.claude.map((a) => a.email)}
                noAccountsHint="Import some accounts first to add them to a group."
                onChange={(groups) => onDraftChange("claudeGroups", groups)}
              />
            </Section>

            {/* Codex. */}
            <Section title="Codex">
              <SettingsProviderFields
                pollSecs={draft.codex.pollSecs}
                pinnedEmail={draft.codex.pinnedEmail}
                onPollSecsChange={(v) => onDraftChange("codex", { ...draft.codex, pollSecs: v })}
                onPinnedEmailChange={(v) =>
                  onDraftChange("codex", { ...draft.codex, pinnedEmail: v })
                }
                codexToggles={{
                  usagePolling: draft.codex.usagePolling,
                  autoReset: draft.codex.autoReset,
                  onUsagePollingChange: (v) =>
                    onDraftChange("codex", { ...draft.codex, usagePolling: v }),
                  onAutoResetChange: (v) => onDraftChange("codex", { ...draft.codex, autoReset: v }),
                }}
              />
            </Section>

            {/* Imported Codex accounts — deletable here (twin of the Claude accounts list).
                Importing is provider-picked inside the same modal, so there's no separate
                entry point here. */}
            <Section
              title="Codex accounts"
              effect="immediate"
              hint="Imported Codex accounts. Deleting one removes its stored token and reassigns clones running it (a clone pinned to it must be reassigned first)."
            >
              <SettingsAccountList
                accounts={rows.codex}
                onDelete={onDeleteCodexAccount}
                onReorder={(ids) => onReorderAccounts("codex", ids)}
              />
            </Section>

            {/* Codex groups (named account pools) — the Codex twin of the Claude groups above.
                Kept as a separate list because the two providers' pools are independent: a
                clone binds one of each. */}
            <Section
              title="Codex groups"
              effect="immediate"
              hint="A pool of Codex accounts. A clone bound to a group keeps its account until that account passes 95% of its weekly (7d) limit, then moves to the least-used member."
            >
              <SettingsGroupsEditor
                groups={draft.codexGroups}
                accountEmails={rows.codex.map((a) => a.email)}
                noAccountsHint="Import some Codex accounts first to add them to a group."
                onChange={(groups) => onDraftChange("codexGroups", groups)}
              />
            </Section>

            {/* Video — chroma subsampling for the viewer stream (server-wide, chosen at launch). */}
            <Section
              title="Video"
              effect="restart"
              hint="Chroma subsampling for the port-1 viewer stream, server-wide. 4:4:4 recovers full chroma via AVC444 packing (a double-height stream reassembled on the GPU); keep monitors ≤1440p in that mode."
            >
              <Field label="Chroma mode">
                <select
                  value={draft.chroma}
                  onChange={(e) => onDraftChange("chroma", e.target.value as ChromaMode)}
                  className={settingsInput}
                >
                  <option value="yuv420">4:2:0 (default)</option>
                  <option value="yuv444">4:4:4 (AVC444, ≤1440p/monitor)</option>
                </select>
              </Field>
            </Section>

            {/* SSH Access — public keys installed on the bastion + every clone, so
                "Copy SSH command" (per-clone) and `rmng ssh <clone>` work with no
                laptop-side config. Keys apply live (bastion re-render + push to running
                clones); the bastion port itself is fixed at startup, shown for reference. */}
            <Section
              title="SSH Access"
              effect="immediate"
              hint="Paste public keys to allow `ssh -J rmng@<host>:<port> rmng@<clone>` (or the per-clone Copy SSH command button). Installed on the bastion and every clone; propagates to running clones within ~10s (or immediately on save)."
            >
              <SettingsSshSection
                authorizedKeys={draft.ssh.authorizedKeys}
                publicHost={draft.ssh.publicHost}
                bastionPort={draft.listen.bastion}
                onAuthorizedKeysChange={(keys) =>
                  onDraftChange("ssh", { ...draft.ssh, authorizedKeys: keys })
                }
                onPublicHostChange={(host) => onDraftChange("ssh", { ...draft.ssh, publicHost: host })}
              />
            </Section>

            {/* Advanced (ports + dirs; need a full control-server restart). */}
            <Section title="Advanced">
              <SettingsAdvancedSection
                listen={draft.listen}
                agentPort={draft.agentPort}
                dataDir={draft.dataDir}
                staticDir={draft.staticDir}
                cloneSocket={draft.cloneSocket}
                open={advanced}
                onOpenChange={setAdvanced}
                onListenChange={(listen) => onDraftChange("listen", listen)}
                onAgentPortChange={(port) => onDraftChange("agentPort", port)}
                onStaticDirChange={(dir) => onDraftChange("staticDir", dir)}
              />
            </Section>
          </div>
        )}
        </div>

        {/* Footer — a flex sibling of the scroll body, so it's always pinned flush to
            the panel's bottom edge. Only shown once the config has loaded. */}
        {draft ? (
          <div className="flex items-center justify-end gap-2 border-t border-slate-100 dark:border-slate-800 bg-white dark:bg-slate-800 px-5 py-3">
            {saved ? <span className="mr-auto text-xs font-medium text-emerald-600 dark:text-emerald-400">Saved ✓</span> : null}
            <button
              type="button"
              onClick={onClose}
              className="rounded border border-slate-300 dark:border-slate-600 px-3 py-1.5 text-sm text-slate-600 dark:text-slate-300 hover:bg-slate-50 dark:hover:bg-slate-700"
            >
              Close
            </button>
            <button
              type="button"
              onClick={onSave}
              disabled={saving}
              className="rounded bg-slate-900 px-3 py-1.5 text-sm font-medium text-white hover:bg-slate-700 disabled:opacity-50 dark:bg-slate-100 dark:text-slate-900 dark:hover:bg-white"
            >
              {saving ? "Saving…" : "Save"}
            </button>
          </div>
        ) : null}
      </div>
    </div>
  );
}
