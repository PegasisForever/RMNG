// The settings overlay's markup: a category rail beside one pane of sections, over a pinned
// footer. It renders from props alone — no config fetch, no save, no update check, no
// localStorage — so every state it can be in is a story. SettingsPanelContainer owns all of
// those and hands the results down.
//
// The form is one editable model (`SettingsDraft`) plus a single `onDraftChange`, rather than
// forty value/onChange pairs. What is NOT in the draft is everything the server decides:
// whether first-run setup has finished, what the last save said about needing a restart, the
// control-server's own version, and the result of the Docker probe.
//
// This file owns the panel's frame: the header, the two banners, the rail, and the footer.
// Which sections make up a category is `SettingsPanes`, and the rail's list is `SettingsNav`.
import { X } from "lucide-react";

import {
  SETTINGS_CATEGORIES,
  SettingsNav,
  type SettingsCategory,
} from "~/components/SettingsNav";
import {
  AgentsPane,
  BoardPane,
  ClaudePane,
  ClonesPane,
  CodexPane,
  PresetsPane,
  ServerPane,
  type SettingsPaneProps,
} from "~/components/SettingsPanes";
import type { AcctOrder } from "~/lib/accountOrder";
import type { BoardColumn } from "~/lib/board";
import { orderedAccounts, type SettingsDraft } from "~/lib/settingsDraft";
import type { ClaudeUsage, Operation } from "~/lib/types";
import { useModalEscape } from "~/lib/useModalEscape";
import type { ImageInfo } from "~/lib/wire/ImageInfo";
import type { UpdateStatus } from "~/lib/wire/UpdateStatus";

export interface SettingsPanelViewProps {
  /** The whole form, as one editable model. Null while the config is still in flight, which
   *  is what draws the loading state and hides the footer. */
  draft: SettingsDraft | null;
  /** Write one field back. The container holds the draft; this is how a keystroke reaches it. */
  onDraftChange: <K extends keyof SettingsDraft>(key: K, value: SettingsDraft[K]) => void;

  /** Which category the rail is on. Controlled, so every pane is reachable as a story. A
   *  category the rail is not offering (Board, on a page with no board) falls back to the
   *  first one, so the pane can never come out empty. */
  category: SettingsCategory;
  onCategoryChange: (category: SettingsCategory) => void;

  /** Live per-account usage (from `ControlState.claudeAccounts`). Despite the field name it
   *  carries BOTH providers' rows tagged by `provider`; the Claude and Codex panes split it
   *  by that tag. The preset pickers take the list unordered, the way a dropdown of options
   *  rather than a list of rows wants it. */
  accounts: ClaudeUsage[];
  /** The operator's own cosmetic ordering of those rows, per provider. It orders the two
   *  account lists and their group checkboxes. */
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

  /** The dashboard board's columns, left to right. Omit to drop the Board category entirely,
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

/** The pane each category draws. Keyed rather than switched, so adding a category is a line
 *  here and a line in `SETTINGS_CATEGORIES`. */
const PANES: Record<SettingsCategory, React.ComponentType<SettingsPaneProps>> = {
  board: BoardPane,
  agents: AgentsPane,
  presets: PresetsPane,
  claude: ClaudePane,
  codex: CodexPane,
  clones: ClonesPane,
  server: ServerPane,
};

export function SettingsPanelView(props: SettingsPanelViewProps) {
  const {
    draft,
    category,
    onCategoryChange,
    accounts,
    accountOrder,
    error,
    restartRequired,
    saving,
    saved,
    onSave,
    onClose,
    onRestartServer,
    boardColumns,
  } = props;

  // Escape closes. Stacked: the import modal opens ON TOP of this panel (z-60 over z-50),
  // and without the stack one Escape would close both — losing the panel as collateral for
  // dismissing the dialog above it. The stack is LIFO, so the import modal (mounted later)
  // owns Escape while it is up; this panel takes it back when the dialog unmounts.
  useModalEscape(onClose);

  // Board columns are optional, so the rail offers that category only when there are some.
  const categories = SETTINGS_CATEGORIES.filter((c) => c.id !== "board" || !!boardColumns);
  const active = categories.find((c) => c.id === category) ?? categories[0];
  const Pane = PANES[active.id];

  return (
    <div className="fixed inset-0 z-50 flex items-center justify-center bg-slate-900/30 p-4">
      {/* Backdrop is inert — clicking it must not close the panel, only the ✕/Cancel
          buttons and Escape (handled above) do. */}
      <div className="flex max-h-[90vh] h-[42rem] w-full max-w-4xl flex-col overflow-hidden rounded-xl border border-slate-200 dark:border-slate-700 bg-white dark:bg-slate-800 shadow-xl">
        {/* Header and banners sit outside the pane, so the rail starts under them and a save
            error stays on screen whichever category it came from. */}
        <div className="flex items-center justify-between px-5 pt-4 pb-2">
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
          <div className="mx-5 mb-2 rounded border border-red-200 dark:border-red-900 bg-red-50 dark:bg-red-950/40 px-3 py-2 text-xs text-red-700 dark:text-red-400">
            {error}
          </div>
        ) : null}

        {restartRequired ? (
          <div className="mx-5 mb-2 flex items-center gap-3 rounded border border-amber-300 dark:border-amber-900 bg-amber-50 dark:bg-amber-950/40 px-3 py-2 text-xs text-amber-800 dark:text-amber-400">
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
          <p className="flex-1 py-8 text-center text-sm text-slate-400 dark:text-slate-500">
            Loading…
          </p>
        ) : (
          // Rail beside pane on a desktop-width panel, rail above pane on a phone, where a
          // 11rem column would leave the fields too narrow to type in.
          <div className="flex min-h-0 flex-1 flex-col border-t border-slate-100 dark:border-slate-800 sm:flex-row">
            <SettingsNav
              categories={categories}
              active={active.id}
              onSelect={onCategoryChange}
            />
            {/* One pane scrolls, not the whole panel, so the rail stays put while a long
                section (the two prompts, the preset list) runs past the bottom edge. The
                first section drops its rule: it would sit right under the rail's top border
                and read as a doubled line. */}
            <div className="min-h-0 flex-1 space-y-4 overflow-y-auto p-5 [&>section:first-child]:border-t-0 [&>section:first-child]:pt-0">
              <Pane
                {...props}
                draft={draft}
                rows={orderedAccounts(accounts, accountOrder)}
              />
            </div>
          </div>
        )}

        {/* Footer — a flex sibling of the pane, so it's always pinned flush to the panel's
            bottom edge. Only shown once the config has loaded. */}
        {draft ? (
          <div className="flex items-center justify-end gap-2 border-t border-slate-100 dark:border-slate-800 bg-white dark:bg-slate-800 px-5 py-3">
            {saved ? (
              <span className="mr-auto text-xs font-medium text-emerald-600 dark:text-emerald-400">
                Saved ✓
              </span>
            ) : null}
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
