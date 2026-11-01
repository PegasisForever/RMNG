// The settings form's categories, and one component per category: the sections the rail files
// under Board, Presets, LLM and Server. Each is a pure composition of sections that already
// have their own stories, so a pane has no state and no variants of its own beyond what the
// panel hands it.
//
// Each pane declares only the fields it renders. `SettingsPane` assembles each pane's props
// from the panel's, so adding a field used by one pane touches that pane's interface and one
// assembly branch — not all four panes.
//
// The categories are data rather than markup, so the rail, the panes and the stories all read
// one list.
import { Bot, Boxes, LayoutGrid, Server } from "lucide-react";
import type { LucideIcon } from "lucide-react";

import { BoardColumnsEditor } from "~/components/BoardColumnsEditor";
import { SettingsDockerSection } from "~/components/SettingsDockerSection";
import { Field, Section, settingsInput } from "~/components/SettingsFields";
import { SettingsGroupsEditor } from "~/components/SettingsGroupsEditor";
import type { BoardColumn } from "~/lib/board";
import { SettingsPresetList } from "~/components/SettingsPresetList";
import { SettingsServerSection } from "~/components/SettingsServerSection";
import { SettingsSshSection } from "~/components/SettingsSshSection";
import { SettingsStuckSection } from "~/components/SettingsStuckSection";
import type { ClaudeUsage, Operation } from "~/lib/types";
import type { SettingsDraft } from "~/lib/settingsDraft";
import { type AcctOrder, orderedAccounts } from "~/lib/accountOrder";
import type { UpdateStatus } from "~/lib/wire/UpdateStatus";

export type SettingsCategory = "board" | "presets" | "llm" | "server";

interface SettingsCategoryInfo {
  id: SettingsCategory;
  label: string;
  icon: LucideIcon;
}

/** Every category, in rail order. Roughly outside-in: what the operator arranges daily first,
 *  what the server was installed with last. */
export const SETTINGS_CATEGORIES: SettingsCategoryInfo[] = [
  { id: "board", label: "Board", icon: LayoutGrid },
  { id: "presets", label: "Presets", icon: Boxes },
  { id: "llm", label: "LLM", icon: Bot },
  { id: "server", label: "Server", icon: Server },
];

/** Write one field of the settings form back. Same shape as the panel's writer; kept here so each pane seam declares its own use. */
type OnDraftChange = <K extends keyof SettingsDraft>(
  key: K,
  value: SettingsDraft[K],
) => void;

/** The form slice every pane needs: the loaded draft plus its single writer. A pane only renders once the config is in, so the draft is non-null here. */
interface PaneDraftProps {
  draft: SettingsDraft;
  onDraftChange: OnDraftChange;
}

/** Board: the board's swim lanes, plus their handlers. */
interface BoardPaneProps extends PaneDraftProps {
  boardColumns: BoardColumn[];
  boardColumnCounts: Record<string, number>;
  onAddBoardColumn: (title: string) => void;
  onRenameBoardColumn: (columnId: string, title: string) => void;
  onSetBoardColumnArchive: (columnId: string, archive: boolean) => void;
  onDeleteBoardColumn: (columnId: string) => void;
  onReorderBoardColumns: (columnIds: string[]) => void;
}

/** Presets: only the form, plus the forkable clones the default-source picker offers. */
interface PresetsPaneProps extends PaneDraftProps {
  forkSources: string[];
}

/** LLM: the form, the ordered account rows, the import entry point, and the judge test. */
interface LlmPaneProps extends PaneDraftProps {
  rows: { claude: ClaudeUsage[]; codex: ClaudeUsage[] };
  onImportAccount: (provider?: "claude" | "codex", group?: string) => void;
  onTestJudge: () => void;
  judgeTestMessage: string | null;
}

/** Server: the form, the control-server's own version and update state. */
interface ServerPaneProps extends PaneDraftProps {
  serverStatus: UpdateStatus | null;
  serverMessage: string | null;
  updateOperation: Operation | null;
  updateDisabled: boolean;
  onCheckUpdate: () => void;
  onUpdateServer: () => void;
  onRestartServer: () => void;
}

/** Board: the board's swim lanes. Applies the moment it is saved. */
function BoardPane({
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
    // Board columns. Clones move between them by drag on the board itself; the columns are
    // made and ordered here.
    <Section
      title="Board columns"
      hint="The board's columns, left to right. Drag to reorder. Tick archive to make a column stop and keep the clones dropped into it, and start them again when they are dragged out. A clone in no column shows up in the first one."
    >
      <BoardColumnsEditor
        columns={boardColumns}
        counts={boardColumnCounts}
        onAddColumn={onAddBoardColumn}
        onRenameColumn={onRenameBoardColumn}
        onSetArchive={onSetBoardColumnArchive}
        onDeleteColumn={onDeleteBoardColumn}
        onReorderColumns={onReorderBoardColumns}
      />
    </Section>
  );
}

/** Presets: the assistant, the two prompt layers, then Linear identity (key +
 *  the ticket-id prefixes that auto-select it) plus the env vars a clone is created with. */
function PresetsPane({
  draft,
  onDraftChange,
  forkSources,
}: PresetsPaneProps) {
  return (
    <>
      {/* The chat panel's assistant: where it is, and where it finds this server. */}
      <Section
        title="Assistant"
        hint="The chat panel talks to an outside assistant: a pi-web server. Each clone gets one chat there, started by its first message. The assistant controls the clone with the rmng CLI, so it needs this server's address as it can reach it (not a Tailscale name it cannot resolve)."
      >
        <Field label="Assistant URL (pi-web)">
          <input
            value={draft.assistant.url}
            onChange={(e) => onDraftChange("assistant", { ...draft.assistant, url: e.target.value })}
            placeholder="http://10.0.0.12:9999"
            spellCheck={false}
            className={settingsInput}
          />
        </Field>
        <Field label="This server's address, as the assistant reaches it">
          <input
            value={draft.assistant.serverUrl}
            onChange={(e) =>
              onDraftChange("assistant", { ...draft.assistant, serverUrl: e.target.value })
            }
            placeholder="http://10.0.0.129:9000"
            spellCheck={false}
            className={settingsInput}
          />
        </Field>
      </Section>

      {/* Layer a: the shared operating memory EVERY agent reads as its native global rules
          (CLAUDE.md / AGENTS.md). Kept in sync into existing clones by the reconciler. */}
      <Section
        title="Global agent prompt (all presets)"
        hint="General engineering guidance written to every agent's native rules file inside the clone (Claude CLAUDE.md, Codex AGENTS.md, pi AGENTS.md, a Cursor rule). Applies to all presets; edits sync into existing clones. Keep the desktop/Cursor ticket procedure OUT of this: that belongs in the assistant playbook below (the inner Cursor Claude Code reads this file and would recurse)."
      >
        <textarea
          value={draft.globalPrompt}
          onChange={(e) => onDraftChange("globalPrompt", e.target.value)}
          spellCheck={false}
          rows={12}
          className="w-full rounded border border-slate-300 dark:border-slate-600 px-2 py-1 font-mono text-xs focus:border-slate-400 dark:focus:border-slate-500 focus:outline-none dark:bg-slate-800 dark:text-slate-100"
        />
      </Section>

      {/* Layer b: the person's ticket procedure, sent in the first message of each new
          assistant chat. */}
      <Section
        title="Assistant playbook (all presets)"
        hint="Your ticket procedure for the assistant: what to do with a ticket, and app quirks. How to drive a desktop is the assistant's own knowledge, so leave it out. Sent, with the preset's append, in the first message of each new assistant chat. Not given to Claude/Codex. A chat that already started keeps what it was sent."
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
 *  ("sign in again") lives in the Usage tab. */
function LlmPane({
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
      <Section title="Groups">
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

      <SettingsStuckSection
        provider={draft.judge.provider}
        geminiKey={draft.judge.geminiKey}
        onProviderChange={(v) =>
          onDraftChange("judge", { ...draft.judge, provider: v })
        }
        onGeminiKeyChange={(v) =>
          onDraftChange("judge", { ...draft.judge, geminiKey: v })
        }
        onTest={onTestJudge}
        testMessage={judgeTestMessage}
      />
    </>
  );
}

/** Server: the control-server itself. Its own version, the Docker settings for new clones,
 *  and the keys it installs on every clone. */
function ServerPane({
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
  return (
    <>
      <Section title="Control-server" hint="Update to the latest published image, or restart the server.">
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

/** The settings form and everything it can do, as `useSettingsPanel` hands it over. */
export interface SettingsPanelProps {
  /** The whole form, as one editable model. Null while the config is still in flight, which
   *  is what draws the loading state. */
  draft: SettingsDraft | null;
  /** Write one field back. The container holds the draft; this is how a keystroke reaches it. */
  onDraftChange: OnDraftChange;

  /** Which category the rail is on. Controlled, so every pane is reachable as a story. */
  category: SettingsCategory;
  onCategoryChange: (category: SettingsCategory) => void;

  /** Live per-account usage (from `ControlState.claudeAccounts`). Despite the field name it
   *  carries BOTH providers' rows tagged by `provider`; the Claude and Codex panes split it
   *  by that tag. The preset pickers take the list unordered, the way a dropdown of options
   *  rather than a list of rows wants it. */
  accounts: ClaudeUsage[];
  /** The operator's own cosmetic ordering of those rows, per provider. It orders the
   *  group tree's account source. */
  accountOrder: AcctOrder;
  /** Open the import modal, optionally on one provider tab with a group preselected
   *  (the settings group tree's per-group import buttons). Accounts are never OAuth'd
   *  in the browser — the control-server harvests the tokens off a clone that's
   *  already signed in. */
  onImportAccount: (provider?: "claude" | "codex", group?: string) => void;
  /** Forkable clone ids, newest first, for the preset default-source picker. */
  forkSources: string[];

  /** The last failed load or save, in the panel's own banner. */
  error: string | null;
  saving: boolean;
  /** The post-save confirmation, which the container clears after a beat. */
  saved: boolean;
  onSave: () => void;

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

  /** A second slot rather than one shared string: the two tests live in different
   *  categories, and one verdict overwriting the other reads as the wrong answer. */
  judgeTestMessage: string | null;
  onTestJudge: () => void;

  /** The board's columns, left to right. */
  boardColumns: BoardColumn[];
  /** Clones per column id, so a delete can say what it displaces. */
  boardColumnCounts: Record<string, number>;
  onAddBoardColumn: (title: string) => void;
  onRenameBoardColumn: (columnId: string, title: string) => void;
  onSetBoardColumnArchive: (columnId: string, archive: boolean) => void;
  onDeleteBoardColumn: (columnId: string) => void;
  onReorderBoardColumns: (columnIds: string[]) => void;
}

/** One category's pane of sections. Each pane declares only the fields it renders; this
 *  switch assembles each pane's props from the panel's. Adding a category is a branch here
 *  and a line in `SETTINGS_CATEGORIES`. */
export function SettingsPane(props: Omit<SettingsPanelProps, "draft"> & { draft: SettingsDraft }) {
  const { draft, onDraftChange } = props;
  switch (props.category) {
    case "board":
      return (
        <BoardPane
          draft={draft}
          onDraftChange={onDraftChange}
          boardColumns={props.boardColumns}
          boardColumnCounts={props.boardColumnCounts}
          onAddBoardColumn={props.onAddBoardColumn}
          onRenameBoardColumn={props.onRenameBoardColumn}
          onSetBoardColumnArchive={props.onSetBoardColumnArchive}
          onDeleteBoardColumn={props.onDeleteBoardColumn}
          onReorderBoardColumns={props.onReorderBoardColumns}
        />
      );
    case "presets":
      return <PresetsPane draft={draft} onDraftChange={onDraftChange} forkSources={props.forkSources} />;
    case "llm":
      return (
        <LlmPane
          draft={draft}
          onDraftChange={onDraftChange}
          rows={orderedAccounts(props.accounts, props.accountOrder)}
          onImportAccount={props.onImportAccount}
          onTestJudge={props.onTestJudge}
          judgeTestMessage={props.judgeTestMessage}
        />
      );
    case "server":
      return (
        <ServerPane
          draft={draft}
          onDraftChange={onDraftChange}
          serverStatus={props.serverStatus}
          serverMessage={props.serverMessage}
          updateOperation={props.updateOperation}
          updateDisabled={props.updateDisabled}
          onCheckUpdate={props.onCheckUpdate}
          onUpdateServer={props.onUpdateServer}
          onRestartServer={props.onRestartServer}
        />
      );
  }
}
