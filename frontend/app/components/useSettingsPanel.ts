// The settings panel, impure half. Everything the panel is not allowed to do lives here: the
// config read that seeds the form, the save that sends it back, the stuck-judge probe, the
// control-server's version check and its two self-directed actions, the shared account-order
// store, and the confirms that guard destructive clicks.
//
// The markup is the Settings tab (`~/desk/control/SettingsView.tsx`).
import { useEffect, useState } from "react";

import type { SettingsCategory, SettingsPanelProps } from "~/components/SettingsPanes";
import { useAccountOrder } from "~/lib/accountOrder";
import {
  getConfig,
  getUpdateStatus,
  putConfig,
  restartServer,
  testConfig,
  updateServer,
} from "~/lib/api";
import type { BoardColumn } from "~/lib/board";
import {
  settingsDraftFrom,
  settingsPatch,
  type SettingsDraft,
} from "~/lib/settingsDraft";
import type { ClaudeUsage, Operation } from "~/lib/types";
import { useOperation } from "~/lib/useOperation";
import type { AppConfigRedacted } from "~/lib/wire/AppConfigRedacted";
import type { UpdateStatus } from "~/lib/wire/UpdateStatus";

interface SettingsPanelOptions {
  /** Live per-account usage (from `ControlState.claudeAccounts`). Despite the field name
   *  it carries BOTH providers' rows tagged by `provider`. Handed through to the panes. */
  accounts: ClaudeUsage[];
  /** Live operations from the SSE state — used to follow the self-update op.
   *  Note the server restarts itself mid-update, so the op stream *will* cut out; the
   *  reconnected stream may or may not still carry it. */
  operations: Operation[];
  /** Open the import-from-a-clone modal. Accounts are never OAuth'd in the browser — the
   *  control-server harvests the tokens off a clone that's already signed in. Takes the
   *  provider tab + preselected group for the group tree's per-group import buttons. */
  onImportAccount: (provider?: "claude" | "codex", group?: string) => void;
  /** Forkable clone ids, newest first, for the preset default-source picker. */
  forkSources: string[];
  // --- board columns ---
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

/** Everything the settings form needs but cannot do itself. */
export function useSettingsPanel({
  accounts,
  operations,
  onImportAccount,
  forkSources,
  boardColumns,
  boardColumnCounts,
  onAddBoardColumn,
  onRenameBoardColumn,
  onSetBoardColumnArchive,
  onDeleteBoardColumn,
  onReorderBoardColumns,
}: SettingsPanelOptions): SettingsPanelProps {
  // Which category the rail is on. Starts at the top.
  const [category, setCategory] = useState<SettingsCategory>("board");
  const [draft, setDraft] = useState<SettingsDraft | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [saving, setSaving] = useState(false);
  const [saved, setSaved] = useState(false);
  const [judgeTestMsg, setJudgeTestMsg] = useState<string | null>(null);
  // Control-server's own version + update-available status (fetched on open; re-checked
  // on demand via the "Check for updates" button).
  const [serverStatus, setServerStatus] = useState<UpdateStatus | null>(null);
  const [serverMsg, setServerMsg] = useState<string | null>(null);
  // The in-flight self-update op, followed through the SSE frames so its progress renders
  // inline here rather than only in the jobs list. Same rules as the two dialogs that follow
  // an op — `~/lib/useOperation` — and the panel had none of them before: it held a bare op
  // id, so a FAILED update read exactly like a running one and left the Update button dead
  // for the life of the panel.
  //
  // The server restarts itself partway through a good update, so the stream drops and the op
  // may never come back. That is precisely the pruned-op rule: an op that was seen and then
  // vanished has settled, so the panel says so instead of waiting forever.
  const update = useOperation(operations, updateServer, {
    failureLabel: "the update failed",
  });
  // Shared cosmetic ordering for the two account lists (drag to reorder). The rail's usage
  // panel reads the same store, so a reorder here reflects there live — persistence and
  // notification both happen in the store. Bucketed per provider because that is the
  // granularity the operator can drag at: the two lists are separate, and the two pools are
  // independent.
  const { acctOrder, setAcctOrder } = useAccountOrder();

  function load(c: AppConfigRedacted) {
    setDraft(settingsDraftFrom(c));
  }

  // Seed the form from the server ONCE, when the panel mounts. Re-running this would re-seed
  // the form from the server and wipe the user's in-progress edits.
  useEffect(() => {
    getConfig()
      .then(load)
      .catch((e: Error) => setError(e.message));
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  useEffect(() => {
    getUpdateStatus()
      .then(setServerStatus)
      .catch((e) => setServerMsg(`✗ ${(e as Error).message}`));
  }, []);

  // The panel has ONE message slot for the check, the update and the restart, so the
  // update's verdict is written into it rather than derived: a later check has to be able to
  // speak over a finished update, which a derived message could never let it do.
  useEffect(() => {
    if (update.phase === "failed") setServerMsg(`✗ ${update.error}`);
    if (update.phase === "done") setServerMsg("updated — reconnecting shortly");
  }, [update.phase, update.error]);

  function updateDraft<K extends keyof SettingsDraft>(
    key: K,
    value: SettingsDraft[K],
  ) {
    setDraft((d) => (d ? { ...d, [key]: value } : d));
  }

  async function checkUpdate() {
    setServerMsg("checking…");
    try {
      const s = await getUpdateStatus();
      setServerStatus(s);
      setServerMsg(
        s.error
          ? `⚠ ${s.error}`
          : s.available
            ? "update available"
            : "up to date",
      );
    } catch (e) {
      setServerMsg(`✗ ${(e as Error).message}`);
    }
  }

  function doUpdate() {
    if (
      !confirm(
        "Update the control-server now?\n\nIt will pull the latest image and restart itself. The UI will briefly disconnect and reconnect; running clones are unaffected.",
      )
    )
      return;
    setServerMsg("updating… the server will restart shortly");
    update.run();
  }

  async function doRestart() {
    if (
      !confirm(
        "Restart the control-server now?\n\nThe UI will briefly disconnect and reconnect; running clones are unaffected.",
      )
    )
      return;
    setServerMsg("restarting… reconnecting shortly");
    try {
      await restartServer();
    } catch (e) {
      setServerMsg(`✗ ${(e as Error).message}`);
    }
  }

  async function save() {
    if (!draft) return;
    setSaving(true);
    setError(null);
    setSaved(false);
    try {
      const res = await putConfig(settingsPatch(draft));
      load(res.config); // re-seed from the server's redacted view; clears write-only inputs
      setSaved(true);
      setTimeout(() => setSaved(false), 2500);
    } catch (e) {
      setError((e as Error).message);
    } finally {
      setSaving(false);
    }
  }

  // Tests what is typed rather than what is stored, so the verdict is about the fields the
  // operator is looking at. For the judge that is the picked provider + the unsaved key.
  async function runJudgeTest() {
    setJudgeTestMsg("testing…");
    try {
      const r = await testConfig(
        "judge",
        draft?.judge.geminiKey || "",
        draft?.judge.provider || "",
      );
      setJudgeTestMsg(`${r.ok ? "✓" : "✗"} ${r.message}`);
    } catch (e) {
      setJudgeTestMsg(`✗ ${(e as Error).message}`);
    }
  }

  return {
    draft,
    onDraftChange: updateDraft,
    category,
    onCategoryChange: setCategory,
    accounts,
    accountOrder: acctOrder,
    onImportAccount,
    forkSources,
    error,
    saving,
    saved,
    onSave: save,
    serverStatus,
    serverMessage: serverMsg,
    updateOperation: update.op ?? null,
    updateDisabled: !serverStatus?.available || update.busy,
    onCheckUpdate: checkUpdate,
    onUpdateServer: doUpdate,
    onRestartServer: doRestart,
    judgeTestMessage: judgeTestMsg,
    onTestJudge: runJudgeTest,
    boardColumns,
    boardColumnCounts,
    onAddBoardColumn,
    onRenameBoardColumn,
    onSetBoardColumnArchive,
    onDeleteBoardColumn,
    onReorderBoardColumns,
  };
}
