// One component per settings category: the sections the rail files under Board, Agents,
// Presets, Claude, Codex, Clones and Server. Each is a pure composition of sections that
// already have their own stories, so a pane has no state and no variants of its own beyond
// what the panel hands it, and `SettingsPanelView`'s per-category stories are what show them.
//
// A pane takes the whole panel prop bag rather than a hand-picked slice. Seven prop interfaces
// restating the same forty fields would be the only other way to say it, and every one of them
// would have to be updated in step with the panel's.
import { useState } from "react";

import { BoardColumnsEditor } from "~/components/BoardColumnsEditor";
import { ImagesSection } from "~/components/ImagesSection";
import { SettingsAccountList } from "~/components/SettingsAccountList";
import { SettingsAdvancedSection } from "~/components/SettingsAdvancedSection";
import { SettingsDockerSection } from "~/components/SettingsDockerSection";
import { Field, Section, settingsInput } from "~/components/SettingsFields";
import { SettingsGroupsEditor } from "~/components/SettingsGroupsEditor";
import { SettingsLayoutPresets } from "~/components/SettingsLayoutPresets";
import type { SettingsPanelViewProps } from "~/components/SettingsPanelView";
import { SettingsPresetList } from "~/components/SettingsPresetList";
import { SettingsProviderFields } from "~/components/SettingsProviderFields";
import { SettingsServerSection } from "~/components/SettingsServerSection";
import { SettingsSshSection } from "~/components/SettingsSshSection";
import type { ClaudeUsage } from "~/lib/types";
import type { SettingsDraft } from "~/lib/settingsDraft";
import type { ChromaMode } from "~/lib/wire/ChromaMode";

/** What every pane takes: the panel's own props, narrowed to the loaded case (a pane only
 *  renders once the config is in), plus the two account lists in the operator's order. */
export type SettingsPaneProps = SettingsPanelViewProps & {
  draft: SettingsDraft;
  rows: { claude: ClaudeUsage[]; codex: ClaudeUsage[] };
};

/** Board: the dashboard's swim lanes and the monitor arrangements the viewer switches between.
 *  Both apply the moment they are saved. */
export function BoardPane({
  draft,
  onDraftChange,
  boardColumns,
  boardColumnCounts,
  onAddBoardColumn,
  onRenameBoardColumn,
  onSetBoardColumnArchive,
  onDeleteBoardColumn,
  onReorderBoardColumns,
}: SettingsPaneProps) {
  return (
    <>
      {/* Board columns. Clones move between them by drag on the board itself; the columns
          are made and ordered here. */}
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

      <Section
        title="Layout presets"
        effect="immediate"
        hint="Named monitor arrangements. Switch the active preset from the sidebar. Running clones reconfigure live without closing apps."
      >
        <SettingsLayoutPresets
          presets={draft.layoutPresets}
          onChange={(presets) => onDraftChange("layoutPresets", presets)}
        />
      </Section>
    </>
  );
}

/** Agents: the two prompt layers every clone is built with. The global one is every agent's
 *  native rules file; the second is the node-agent's alone. */
export function AgentsPane({ draft, onDraftChange }: SettingsPaneProps) {
  return (
    <>
      {/* Layer a: the shared operating memory EVERY agent reads as its native global rules
          (CLAUDE.md / AGENTS.md). Kept in sync into existing clones by the reconciler. */}
      <Section
        title="Global agent prompt (all presets)"
        effect="immediate"
        hint="General engineering guidance written to every agent's native rules file (Claude CLAUDE.md, Codex AGENTS.md, and read by the node-agent). Applies to all presets; edits sync into existing clones. Keep desktop/Cursor ticket procedure OUT of this: that belongs in the node-agent prompt below (the inner Cursor Claude Code reads this file and would recurse)."
      >
        <textarea
          value={draft.globalPrompt}
          onChange={(e) => onDraftChange("globalPrompt", e.target.value)}
          spellCheck={false}
          rows={12}
          className="w-full rounded border border-slate-300 dark:border-slate-600 px-2 py-1 font-mono text-xs focus:border-slate-400 dark:focus:border-slate-500 focus:outline-none dark:bg-slate-800 dark:text-slate-100"
        />
      </Section>

      {/* Layer b: the desktop agent's operating notes / ticket procedure, appended to its
          system prompt at clone time. Node-agent only. */}
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
    </>
  );
}

/** Presets: Linear identity (key + the ticket-id prefixes that auto-select it) plus the env
 *  vars a clone is created with. */
export function PresetsPane({ draft, onDraftChange, accounts }: SettingsPaneProps) {
  return (
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
  );
}

/** Claude: the provider's own polling and pin, the imported accounts clones draw from, and the
 *  named pools built out of them. */
export function ClaudePane({
  draft,
  onDraftChange,
  rows,
  onReorderAccounts,
  onDeleteAccount,
  onImportAccount,
}: SettingsPaneProps) {
  return (
    <>
      <Section title="Claude">
        <SettingsProviderFields
          pollSecs={draft.claude.pollSecs}
          pinnedEmail={draft.claude.pinnedEmail}
          onPollSecsChange={(v) => onDraftChange("claude", { ...draft.claude, pollSecs: v })}
          onPinnedEmailChange={(v) => onDraftChange("claude", { ...draft.claude, pinnedEmail: v })}
        />
      </Section>

      {/* There is no in-browser login: the control-server harvests an account's tokens off a
          clone that is already signed in, hence "Import account" rather than "Add". */}
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
    </>
  );
}

/** Codex: the twin of the Claude pane. The two providers' pools are independent, and a clone
 *  binds one of each, so neither list is a filter of the other. */
export function CodexPane({
  draft,
  onDraftChange,
  rows,
  onReorderAccounts,
  onDeleteCodexAccount,
}: SettingsPaneProps) {
  return (
    <>
      <Section title="Codex">
        <SettingsProviderFields
          pollSecs={draft.codex.pollSecs}
          pinnedEmail={draft.codex.pinnedEmail}
          onPollSecsChange={(v) => onDraftChange("codex", { ...draft.codex, pollSecs: v })}
          onPinnedEmailChange={(v) => onDraftChange("codex", { ...draft.codex, pinnedEmail: v })}
          codexToggles={{
            usagePolling: draft.codex.usagePolling,
            autoReset: draft.codex.autoReset,
            onUsagePollingChange: (v) => onDraftChange("codex", { ...draft.codex, usagePolling: v }),
            onAutoResetChange: (v) => onDraftChange("codex", { ...draft.codex, autoReset: v }),
          }}
        />
      </Section>

      {/* Importing is provider-picked inside the same modal the Claude list opens, so this
          list has no entry point of its own. */}
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
    </>
  );
}

/** Clones: what a new clone is cut from. The Docker settings every clone is created with, and
 *  the images they are created from. */
export function ClonesPane({
  draft,
  onDraftChange,
  setupComplete,
  testMessage,
  onTestDocker,
  images,
  imagesLoading,
  pullBusy,
  now,
  onPullLatestImage,
  onPullOtherImage,
  onDeleteImage,
}: SettingsPaneProps) {
  return (
    <>
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

      {/* The pull prompt is prefilled from the Template reference field above. */}
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
    </>
  );
}

/** Server: the control-server itself. Its own version, the settings it reads at startup, and
 *  the keys it installs on every clone. Most of this needs a restart to take. */
export function ServerPane({
  draft,
  onDraftChange,
  serverStatus,
  serverMessage,
  updateOperation,
  updateDisabled,
  onCheckUpdate,
  onUpdateServer,
  onRestartServer,
}: SettingsPaneProps) {
  // Whether the ports-and-directories block is expanded. Ephemeral: it is not part of the
  // form, nothing outside this pane reads it, and it resets every time the pane is left.
  const [advanced, setAdvanced] = useState(false);

  return (
    <>
      <Section
        title="Control-server"
        effect="restart"
        hint="Update to the latest published image, or restart to apply changed startup settings."
      >
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

      {/* Chroma subsampling for the port-1 viewer stream, server-wide, chosen at launch. */}
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

      {/* Public keys installed on the bastion + every clone, so "Copy SSH command" (per-clone)
          and `rmng ssh <clone>` work with no laptop-side config. Keys apply live; the bastion
          port itself is fixed at startup and shown for reference. */}
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

      {/* Ports + dirs. These need a full control-server restart. */}
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
    </>
  );
}
