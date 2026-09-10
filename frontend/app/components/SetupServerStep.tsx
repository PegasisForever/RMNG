// Step 2 of the first-run wizard: the fleet's server-side defaults. Clone naming, the
// per-clone resource limits, the monitor arrangement every clone boots with, and the
// viewer's chroma mode. (Ports and directories used to be editable here; they are
// hardcoded now, so this step edits everything the wizard still owns.)
//
// It takes the wizard's whole model plus one updater, the same pair `SetupWizardView` holds
// and `SetupReviewStep` reads, rather than a value/setter couple per field.
import { MonitorsEditor } from "~/components/MonitorsEditor";
import { Field, settingsInput } from "~/components/SettingsFields";
import type { SetupDraft } from "~/lib/setupDraft";
import type { ChromaMode } from "~/lib/wire/ChromaMode";

export function SetupServerStep({
  draft,
  onDraftChange,
}: {
  /** The whole wizard form. The monitors in it are the one arrangement the wizard edits:
   *  there is no preset picker here, because which named preset this becomes is decided by
   *  the config, not by the operator. */
  draft: SetupDraft;
  /** Write one field back. The container holds the draft; this is how a keystroke reaches it. */
  onDraftChange: <K extends keyof SetupDraft>(key: K, value: SetupDraft[K]) => void;
}) {
  return (
    <div className="space-y-4">
      <p className="text-sm text-slate-600 dark:text-slate-300">
        Server-side layout and defaults for the fleet.
      </p>

      <Field label="Clone hostname prefix">
        <input
          value={draft.hostnamePrefix}
          onChange={(e) => onDraftChange("hostnamePrefix", e.target.value)}
          placeholder="pega-"
          spellCheck={false}
          className={settingsInput}
        />
        <span className="mt-0.5 block text-xs text-slate-400 dark:text-slate-500">
          Prepended to derived clone hostnames — e.g.{" "}
          <code>{draft.hostnamePrefix || "pega-"}</code>dev-123.
        </span>
      </Field>

      <div className="grid grid-cols-2 gap-3">
        <Field label="CPU limit per clone (cores)">
          <input
            type="number"
            min={1}
            value={draft.cloneCpus}
            onChange={(e) => onDraftChange("cloneCpus", Number(e.target.value) || 0)}
            className={settingsInput}
          />
        </Field>
        <Field label="Memory limit per clone (MB)">
          <input
            type="number"
            min={1024}
            value={draft.cloneMemoryMb}
            onChange={(e) => onDraftChange("cloneMemoryMb", Number(e.target.value) || 0)}
            className={settingsInput}
          />
        </Field>
      </div>

      <div>
        <span className="mb-1 block text-xs font-medium text-slate-500 dark:text-slate-400">Monitors</span>
        <MonitorsEditor
          monitors={draft.monitors}
          onChange={(monitors) => onDraftChange("monitors", monitors)}
        />
      </div>

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

    </div>
  );
}
