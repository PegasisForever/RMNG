// Settings panel, impure half. Everything the overlay is not allowed to do lives here: the
// config read that seeds the form, the save that sends it back, the Docker probe, the
// control-server's version check and its two self-directed actions, the shared account-order
// store, the five confirms that guard a destructive click, and the one prompt that asks which
// image to pull.
//
// The server calls themselves arrive as props rather than being imported, which is how the
// setup wizard can reuse this panel against a different pair of endpoints. The markup is
// SettingsPanelView.
import { useEffect, useState } from "react";

import type { SettingsCategory } from "~/components/SettingsNav";
import { SettingsPanelView } from "~/components/SettingsPanelView";
import { useAccountOrder } from "~/lib/accountOrder";
import type { BoardColumn } from "~/lib/board";
import {
  settingsDraftFrom,
  settingsPatch,
  type SettingsDraft,
} from "~/lib/settingsDraft";
import type { ClaudeUsage, Operation } from "~/lib/types";
import type { AppConfigRedacted } from "~/lib/wire/AppConfigRedacted";
import type { ConfigPutResponse } from "~/lib/wire/ConfigPutResponse";
import type { ImageInfo } from "~/lib/wire/ImageInfo";
import type { UpdateStatus } from "~/lib/wire/UpdateStatus";

export interface SettingsPanelContainerProps {
  /** Live per-account usage (from `ControlState.claudeAccounts`). Despite the field name
   *  it carries BOTH providers' rows tagged by `provider`; the two account/group sections
   *  below split it by that tag. Used here only for the emails. */
  accounts: ClaudeUsage[];
  onClose: () => void;
  // --- injected server calls (no API logic lives in this component, so it's
  //     renderable in isolation — e.g. Storybook — with mocked data) ---
  /** Read the current redacted config (each preset's Linear key comes back verbatim). */
  getConfig: () => Promise<AppConfigRedacted>;
  /** Persist a partial config patch; returns the merged config + a restart-required flag. */
  putConfig: (patch: unknown) => Promise<ConfigPutResponse & { networkWarning?: string }>;
  /** Validate a setting (e.g. `"docker"` — re-runs the Docker self-setup probe). */
  testConfig: (what: string) => Promise<{ ok: boolean; message: string }>;
  /** Read the control-server's own version + update-available status. */
  getUpdateStatus: () => Promise<UpdateStatus>;
  /** Pull the latest control-server image and swap the running container onto it. Returns
   *  the driving Operation, whose progress renders inline below the button. */
  updateServer: () => Promise<Operation>;
  /** Live operations from the SSE state — used to follow the update op started above.
   *  Note the server restarts itself mid-update, so the op stream *will* cut out; the
   *  reconnected stream may or may not still carry it. */
  operations: Operation[];
  /** Restart the control-server container in place (applies changed startup settings). */
  restartServer: () => Promise<{ ok: boolean }>;
  // --- clone-source images (moved here from the sidebar) ---
  images: ImageInfo[];
  imagesLoading: boolean;
  /** True while a template-pull op is running (disables the pull action). */
  pullBusy: boolean;
  onPullTemplate: (reference: string) => void;
  onDeleteImage: (reference: string) => void;
  /** Delete an imported Claude account by email (removes its stored token; reassigns clones). */
  onDeleteAccount: (email: string) => void;
  /** Delete an imported Codex account by email. */
  onDeleteCodexAccount: (email: string) => void;
  /** Open the import-from-a-clone modal. Accounts are never OAuth'd in the browser — the
   *  control-server harvests the tokens off a clone that's already signed in. */
  onImportAccount: () => void;
  // --- board columns ---
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

export function SettingsPanelContainer({
  accounts,
  onClose,
  getConfig,
  putConfig,
  testConfig,
  getUpdateStatus,
  updateServer,
  operations,
  restartServer,
  images,
  imagesLoading,
  pullBusy,
  onPullTemplate,
  onDeleteImage,
  onDeleteAccount,
  onDeleteCodexAccount,
  onImportAccount,
  boardColumns,
  boardColumnCounts,
  onAddBoardColumn,
  onRenameBoardColumn,
  onSetBoardColumnArchive,
  onDeleteBoardColumn,
  onReorderBoardColumns,
}: SettingsPanelContainerProps) {
  // Which category the rail is on. Resets to the top every time the panel opens, which is the
  // right default: the panel is closed by the operator, and the next open is a new errand.
  const [category, setCategory] = useState<SettingsCategory>("board");
  // The loaded config, kept alongside the form for the one thing the form does not carry:
  // whether first-run setup has finished, which decides both the subnet field and the patch.
  const [config, setConfig] = useState<AppConfigRedacted | null>(null);
  const [draft, setDraft] = useState<SettingsDraft | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [saving, setSaving] = useState(false);
  const [saved, setSaved] = useState(false);
  const [testMsg, setTestMsg] = useState<string | null>(null);
  // True after a save that touched a restart-required setting (ports / cloneSocket /
  // staticDir / chroma) — surfaces a persistent banner until a later save clears it.
  const [restartRequired, setRestartRequired] = useState(false);
  // Control-server's own version + update-available status (fetched on open; re-checked
  // on demand via the "Check for updates" button).
  const [serverStatus, setServerStatus] = useState<UpdateStatus | null>(null);
  const [serverMsg, setServerMsg] = useState<string | null>(null);
  // The in-flight self-update op, followed through the SSE frames so its progress renders
  // inline here rather than only in the sidebar. The server restarts itself partway through,
  // so the stream drops and the op may not come back — `updateOp` simply goes undefined and
  // the last message stands. Kept until the panel closes; there's nothing to close here.
  const [updateOpId, setUpdateOpId] = useState<string | null>(null);
  const updateOp = updateOpId ? operations.find((o) => o.id === updateOpId) : undefined;
  // The image rows' clock. Captured once: an image is days old, and nobody keeps this panel
  // open long enough for "6d ago" to turn into "7d".
  const [now] = useState(() => Date.now());
  // Shared cosmetic ordering for the two account lists (drag to reorder). The rail's usage
  // panel reads the same store, so a reorder here reflects there live — persistence and
  // notification both happen in the store. Bucketed per provider because that is the
  // granularity the operator can drag at: the two lists are separate, and the two pools are
  // independent.
  const { acctOrder, setAcctOrder } = useAccountOrder();

  function load(c: AppConfigRedacted) {
    setConfig(c);
    setDraft(settingsDraftFrom(c));
  }

  // Seed the form from the server ONCE, when the panel opens. This must NOT depend on
  // `onClose` (a fresh inline arrow on every parent render): the dashboard re-renders
  // every few seconds on each `stats` SSE frame, and re-running this would re-seed the
  // form from the server and wipe the user's in-progress edits.
  useEffect(() => {
    getConfig().then(load).catch((e: Error) => setError(e.message));
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  useEffect(() => {
    getUpdateStatus().then(setServerStatus).catch((e) => setServerMsg(`✗ ${(e as Error).message}`));
  }, [getUpdateStatus]);

  function updateDraft<K extends keyof SettingsDraft>(key: K, value: SettingsDraft[K]) {
    setDraft((d) => (d ? { ...d, [key]: value } : d));
  }

  async function checkUpdate() {
    setServerMsg("checking…");
    try {
      const s = await getUpdateStatus();
      setServerStatus(s);
      setServerMsg(s.error ? `⚠ ${s.error}` : s.available ? "update available" : "up to date");
    } catch (e) {
      setServerMsg(`✗ ${(e as Error).message}`);
    }
  }

  async function doUpdate() {
    if (!confirm("Update the control-server now?\n\nIt will pull the latest image and restart itself. The UI will briefly disconnect and reconnect; running clones are unaffected.")) return;
    setServerMsg("updating… the server will restart shortly");
    try {
      setUpdateOpId((await updateServer()).id);
    } catch (e) {
      setServerMsg(`✗ ${(e as Error).message}`);
    }
  }

  async function doRestart() {
    if (!confirm("Restart the control-server now to apply the changed settings?\n\nThe UI will briefly disconnect and reconnect; running clones are unaffected.")) return;
    setServerMsg("restarting… reconnecting shortly");
    try {
      await restartServer();
    } catch (e) {
      setServerMsg(`✗ ${(e as Error).message}`);
    }
  }

  /** Which reference "+ Pull template" pulls. Prefilled with the configured template, which
   *  is the one the operator almost always means. */
  function promptPullImage() {
    if (pullBusy) return;
    const rawRef = window.prompt(
      "Template reference to pull (Docker Hub repo:tag)",
      draft?.templateReference ?? "",
    );
    if (rawRef == null) return;
    const reference = rawRef.trim();
    if (!reference) {
      alert("Enter a template reference.");
      return;
    }
    onPullTemplate(reference);
  }

  /** One-click refresh: re-pull the configured template reference. Re-pulling the same
   *  `repo:tag` moves the local tag onto the freshly pulled image — that IS the refresh, and
   *  it costs several gigabytes, so it is asked about first. */
  function confirmPullLatest() {
    if (pullBusy) return;
    const templateRef = draft?.templateReference ?? "";
    if (!confirm(`Pull the latest template (${templateRef || "configured reference"})?`)) return;
    onPullTemplate(templateRef);
  }

  /** Deleting an image takes it out of the Docker daemon, so it is confirmed first. */
  function confirmDeleteImage(reference: string) {
    if (
      confirm(`Delete image ${reference}?\n\nThis removes the image from the Docker daemon.`)
    ) {
      onDeleteImage(reference);
    }
  }

  /** Deleting an account removes its stored token, so it is confirmed before it is asked for. */
  function confirmDelete(email: string, remove: (email: string) => void) {
    if (
      window.confirm(
        `Delete ${email}?\n\nThis removes its stored token (re-adding needs a fresh import). Clones running it are reassigned to another account; a clone pinned to it must be reassigned first.`,
      )
    ) {
      remove(email);
    }
  }

  async function save() {
    if (!draft) return;
    setSaving(true);
    setError(null);
    setSaved(false);
    try {
      const res = await putConfig(settingsPatch(draft, !!config?.setupComplete));
      load(res.config); // re-seed from the server's redacted view; clears write-only inputs
      setRestartRequired(res.restartRequired); // shows/clears the restart banner
      setSaved(true);
      setTimeout(() => setSaved(false), 2500);
    } catch (e) {
      setError((e as Error).message);
    } finally {
      setSaving(false);
    }
  }

  async function runTest() {
    setTestMsg("testing…");
    try {
      const r = await testConfig("docker");
      setTestMsg(`${r.ok ? "✓" : "✗"} ${r.message}`);
    } catch (e) {
      setTestMsg(`✗ ${(e as Error).message}`);
    }
  }

  return (
    <SettingsPanelView
      draft={draft}
      onDraftChange={updateDraft}
      category={category}
      onCategoryChange={setCategory}
      accounts={accounts}
      accountOrder={acctOrder}
      onReorderAccounts={(provider, ids) =>
        setAcctOrder((prev) => ({ ...prev, [provider]: ids }))
      }
      onDeleteAccount={(email) => confirmDelete(email, onDeleteAccount)}
      onDeleteCodexAccount={(email) => confirmDelete(email, onDeleteCodexAccount)}
      onImportAccount={onImportAccount}
      setupComplete={!!config?.setupComplete}
      error={error}
      restartRequired={restartRequired}
      saving={saving}
      saved={saved}
      onSave={save}
      onClose={onClose}
      serverStatus={serverStatus}
      serverMessage={serverMsg}
      updateOperation={updateOp ?? null}
      updateDisabled={!serverStatus?.available || !!updateOpId}
      onCheckUpdate={checkUpdate}
      onUpdateServer={doUpdate}
      onRestartServer={doRestart}
      testMessage={testMsg}
      onTestDocker={runTest}
      images={images}
      imagesLoading={imagesLoading}
      pullBusy={pullBusy}
      now={now}
      onPullLatestImage={confirmPullLatest}
      onPullOtherImage={promptPullImage}
      onDeleteImage={confirmDeleteImage}
      boardColumns={boardColumns}
      boardColumnCounts={boardColumnCounts}
      onAddBoardColumn={onAddBoardColumn}
      onRenameBoardColumn={onRenameBoardColumn}
      onSetBoardColumnArchive={onSetBoardColumnArchive}
      onDeleteBoardColumn={onDeleteBoardColumn}
      onReorderBoardColumns={onReorderBoardColumns}
    />
  );
}
