// Step 2 of the first-run wizard: the fleet's server-side defaults. Clone naming and the
// per-clone resource limits. (Monitors are each browser's own: the app's Edit layout.)
//
// It takes the wizard's whole model plus one updater, the same pair `SetupWizardView` holds
// and `SetupReviewStep` reads, rather than a value/setter couple per field.
import { Field, settingsInput } from "~/components/SettingsFields";
import { TEXT_2, TEXT_3 } from "~/desk/control/ui";
import type { SetupDraft } from "~/lib/setupDraft";

export function SetupServerStep({
  draft,
  onDraftChange,
}: {
  /** The whole wizard form. */
  draft: SetupDraft;
  /** Write one field back. The container holds the draft; this is how a keystroke reaches it. */
  onDraftChange: <K extends keyof SetupDraft>(
    key: K,
    value: SetupDraft[K],
  ) => void;
}) {
  return (
    <div className="space-y-4">
      <p className={`text-sm ${TEXT_2}`}>
        Server-side defaults for the fleet.
      </p>

      <Field label="Clone hostname prefix">
        <input
          value={draft.hostnamePrefix}
          onChange={(e) => onDraftChange("hostnamePrefix", e.target.value)}
          placeholder="pega-"
          spellCheck={false}
          className={settingsInput}
        />
        <span className={`mt-0.5 block text-xs ${TEXT_3}`}>
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
            onChange={(e) =>
              onDraftChange("cloneCpus", Number(e.target.value) || 0)
            }
            className={settingsInput}
          />
        </Field>
        <Field label="Memory limit per clone (MB)">
          <input
            type="number"
            min={1024}
            value={draft.cloneMemoryMb}
            onChange={(e) =>
              onDraftChange("cloneMemoryMb", Number(e.target.value) || 0)
            }
            className={settingsInput}
          />
        </Field>
      </div>

    </div>
  );
}
