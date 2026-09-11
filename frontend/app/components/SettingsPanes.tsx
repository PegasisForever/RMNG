// One component per settings category: the sections the rail files under Board, Agents,
// Presets, Claude, Codex, Clones and Server. Each is a pure composition of sections that
// already have their own stories, so a pane has no state and no variants of its own beyond
// what the panel hands it, and `SettingsPanelView`'s per-category stories are what show them.
//
// Each pane declares only the fields it renders. `SettingsPanelView` assembles each pane's
// props from the panel's, so adding a field used by one pane touches that pane's interface
// and one assembly branch — not all five panes.

import { BoardColumnsEditor } from "~/components/BoardColumnsEditor";
import { SettingsDockerSection } from "~/components/SettingsDockerSection";
import { Field, Section, settingsInput } from "~/components/SettingsFields";
import { SettingsGroupsEditor } from "~/components/SettingsGroupsEditor";
import { SettingsLayoutPresets } from "~/components/SettingsLayoutPresets";
import type { BoardColumn } from "~/lib/board";
import { SettingsPresetList } from "~/components/SettingsPresetList";
import { SettingsServerSection } from "~/components/SettingsServerSection";
import { SettingsSshSection } from "~/components/SettingsSshSection";
import { SettingsStuckSection } from "~/components/SettingsStuckSection";
import type { ClaudeUsage, Operation } from "~/lib/types";
import type { SettingsDraft } from "~/lib/settingsDraft";
import type { ChromaMode } from "~/lib/wire/ChromaMode";
import type { UpdateStatus } from "~/lib/wire/UpdateStatus";

/** Write one field of the settings form back. Same shape as the panel's writer; kept here so each pane seam declares its own use. */
export type OnDraftChange = <K extends keyof SettingsDraft>(
  key: K,
  value: SettingsDraft[K],
) => void;

/** The form slice every pane needs: the loaded draft plus its single writer. A pane only renders once the config is in, so the draft is non-null here. */
export interface PaneDraftProps {
  draft: SettingsDraft;
  onDraftChange: OnDraftChange;
}

/** Board: the dashboard's swim lanes, plus their handlers. */
export interface BoardPaneProps extends PaneDraftProps {
  boardColumns?: BoardColumn[];
  boardColumnCounts?: Record<string, number>;
  onAddBoardColumn?: (title: string) => void;
  onRenameBoardColumn?: (columnId: string, title: string) => void;
  onSetBoardColumnArchive?: (columnId: string, archive: boolean) => void;
  onDeleteBoardColumn?: (columnId: string) => void;
  onReorderBoardColumns?: (columnIds: string[]) => void;
}

/** Layout: only the form. */
export type LayoutPaneProps = PaneDraftProps;

/** Presets: only the form, plus the forkable clones the default-source picker offers. */
export interface PresetsPaneProps extends PaneDraftProps {
  forkSources: string[];
}

/** LLM: the form, the ordered account rows, the import entry point, and the judge test. */
export interface LlmPaneProps extends PaneDraftProps {
  rows: { claude: ClaudeUsage[]; codex: ClaudeUsage[] };
  onImportAccount: (provider?: "claude" | "codex", group?: string) => void;
  onTestJudge: () => void;
  judgeTestMessage: string | null;
}

/** Server: the form, the control-server's own version and update state. */
export interface ServerPaneProps extends PaneDraftProps {
  serverStatus: UpdateStatus | null;
  serverMessage: string | null;
  updateOperation: Operation | null;
  updateDisabled: boolean;
  onCheckUpdate: () => void;
  onUpdateServer: () => void;
  onRestartServer: () => void;
}

/** Board: the dashboard's swim lanes. Applies the moment it is saved. */
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
}: BoardPaneProps) {
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
            onSetArchive={(id, archive) =>
              onSetBoardColumnArchive?.(id, archive)
            }
            onDeleteColumn={(id) => onDeleteBoardColumn?.(id)}
            onReorderColumns={(ids) => onReorderBoardColumns?.(ids)}
          />
        </Section>
      ) : null}
    </>
  );
}

/** Layout: the named monitor arrangements the viewer switches between. Applies the moment
 *  it is saved. */
export function LayoutPane({ draft, onDraftChange }: LayoutPaneProps) {
  return (
    <>
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

/** Presets: the two prompt layers every clone is built with, then Linear identity (key +
 *  the ticket-id prefixes that auto-select it) plus the env vars a clone is created with. */
export function PresetsPane({
  draft,
  onDraftChange,
  forkSources,
}: PresetsPaneProps) {
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

      <Section
        title="Presets"
        effect="immediate"
        hint="A preset = Linear API key + the ticket-id prefixes (Linear team keys, e.g. DEV) that auto-select it + env vars, written to the clone's session env at creation. The key is also injected as LINEAR_API_KEY (auths the clone's `linear` MCP). Cloning from a ticket auto-picks by the ticket's team prefix (DEV-196 → DEV); other clones require an explicit pick."
      >
        <SettingsPresetList
          presets={draft.presets}
          groups={draft.groups}
          forkSources={forkSources}
          onChange={(presets) => onDraftChange("presets", presets)}
        />
      </Section>
    </>
  );
}

/** LLM: the one shared pool list plus the Codex-wide reset-credit switch. A pool
 *  may mix Claude and Codex accounts; each side's rotator only sees its own members, and
 *  a clone binds at most one pool, which feeds both sides.
 *
 *  There are no per-provider account sections anymore: importing happens per-pool in the
 *  group tree (every import lands in a pool directly), deleting happens by dropping the
 *  last membership and saving (the server sweeps unclaimed accounts), and token health
 *  ("sign in again") lives in the dashboard Usage column. */
export function LlmPane({
  draft,
  onDraftChange,
  rows,
  onImportAccount,
  onTestJudge,
  judgeTestMessage,
}: LlmPaneProps) {
  return (
    <>
      {/* One pool list for both providers: members may mix Claude and Codex accounts.
          Each side rotates within its own members (Claude: 80% 5h or 95% 7d exhausts;
          Codex: 95% weekly), keeping its account otherwise to preserve prompt cache. */}
      <Section title="Groups" effect="immediate">
        <SettingsGroupsEditor
          groups={draft.groups}
          accounts={[...rows.claude, ...rows.codex]}
          noAccountsHint="Import some accounts first to add them to a group."
          onChange={(groups) => onDraftChange("groups", groups)}
          onImportAccount={(provider, group) =>
            onImportAccount(provider, group)
          }
        />
        <label className="mt-3 flex items-center gap-2 text-sm text-slate-600 dark:text-slate-300">
          <input
            type="checkbox"
            checked={draft.codex.autoReset}
            onChange={(e) =>
              onDraftChange("codex", {
                ...draft.codex,
                autoReset: e.target.checked,
              })
            }
          />
          Auto-use Codex reset credits (when every account is &gt;95% weekly and
          none reset within 24h, spend one banked reset to bring an account
          back)
        </label>
      </Section>

      {/* How RMNG tells a clone that is thinking from one that is waiting on you. The
          undecidable cases are settled by a GPT call on an imported Codex account, so
          this lives with the accounts, not with the Docker settings it used to sit under. */}
      <SettingsStuckSection
        model={draft.judge.codexModel}
        email={draft.judge.codexEmail}
        accounts={rows.codex.map((a) => a.email)}
        onModelChange={(v) =>
          onDraftChange("judge", { ...draft.judge, codexModel: v })
        }
        onEmailChange={(v) =>
          onDraftChange("judge", { ...draft.judge, codexEmail: v })
        }
        onTest={onTestJudge}
        testMessage={judgeTestMessage}
      />
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
}: ServerPaneProps) {
  // Whether the ports-and-directories block is expanded. Ephemeral: it is not part of the
  // form, nothing outside this pane reads it, and it resets every time the pane is left.

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
            onChange={(e) =>
              onDraftChange("chroma", e.target.value as ChromaMode)
            }
            className={settingsInput}
          >
            <option value="yuv420">4:2:0 (default)</option>
            <option value="yuv444">4:4:4 (AVC444, ≤1440p/monitor)</option>
          </select>
        </Field>
      </Section>

      {/* The Docker settings every new clone is created with. Clone images are gen-2 preset
          builds: each preset's Dockerfile builds into a hash tag on demand, so there is no image
          list to manage here. */}
      <Section title="Docker / Clones">
        <SettingsDockerSection
          hostnamePrefix={draft.hostnamePrefix}
          cloneCpus={draft.cloneCpus}
          cloneMemoryMb={draft.cloneMemoryMb}
          onHostnamePrefixChange={(v) => onDraftChange("hostnamePrefix", v)}
          onCloneCpusChange={(v) => onDraftChange("cloneCpus", v)}
          onCloneMemoryMbChange={(v) => onDraftChange("cloneMemoryMb", v)}
        />
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
          onAuthorizedKeysChange={(keys) =>
            onDraftChange("ssh", { ...draft.ssh, authorizedKeys: keys })
          }
        />
      </Section>
    </>
  );
}
