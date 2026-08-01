// Step 3 of the first-run wizard: pull the clone template off Docker Hub.
//
// The only step whose progress comes from the server rather than from the form. The pull runs
// as an operation and the wizard follows it over /events, so the bar, the button label, the
// locked input and the Skip link are all functions of that operation's status. They arrive
// here as plain booleans, which is what makes a mid-pull wizard a story.
import { OperationProgress } from "~/components/OperationProgress";
import { Field, settingsInput } from "~/components/SettingsFields";
import type { Operation } from "~/lib/types";

export function SetupTemplateStep({
  templateReference,
  placeholder,
  operation,
  pullTarget,
  pulling,
  pullRunning,
  pullDone,
  canPull,
  onTemplateReferenceChange,
  onPull,
  onSkip,
}: {
  /** The reference to pull, as typed. Blank falls back to the placeholder below. */
  templateReference: string;
  /** The configured `docker.templateReference`, which is what a blank field pulls. */
  placeholder: string;
  /** The pull this step started, once it shows up in the live op list. */
  operation: Operation | null;
  /** The reference the pull was started for, which is the op's target. */
  pullTarget: string | null;
  /** The POST that starts the pull is in flight (before the op exists). */
  pulling: boolean;
  /** The pull operation is running. */
  pullRunning: boolean;
  /** The pull operation finished. The reference is locked in from here on. */
  pullDone: boolean;
  /** The Download button may fire. */
  canPull: boolean;
  onTemplateReferenceChange: (value: string) => void;
  onPull: () => void;
  /** Move on without a template. It can be pulled later from the Images panel. */
  onSkip: () => void;
}) {
  return (
    <div className="space-y-4">
      <p className="text-sm text-slate-600 dark:text-slate-300">
        Pull the pre-built clone template (Ubuntu 26.04, the base our patched GNOME is
        built for) from Docker Hub. The pulled image keeps its own{" "}
        <code>repo:tag</code> as the clone source. You can skip this and pull it later
        from the Images panel.
      </p>
      <Field label="Template reference">
        <input
          value={templateReference}
          onChange={(e) => onTemplateReferenceChange(e.target.value)}
          placeholder={placeholder}
          spellCheck={false}
          disabled={pullRunning || pullDone}
          className={`${settingsInput} disabled:bg-slate-50 dark:disabled:bg-slate-900 disabled:text-slate-400 dark:disabled:text-slate-500`}
        />
        <span className="mt-0.5 block text-xs text-slate-400 dark:text-slate-500">
          Docker Hub <code>repo:tag</code> the template is pulled from — clones are
          created from this exact reference.
        </span>
      </Field>

      {operation ? <OperationProgress op={operation} /> : null}
      {pullDone ? (
        <p className="text-xs font-medium text-emerald-600 dark:text-emerald-400">
          ✓ Template “{pullTarget}” pulled.
        </p>
      ) : null}

      <div className="flex items-center gap-3">
        <button
          type="button"
          onClick={onPull}
          disabled={!canPull}
          className="rounded bg-emerald-600 px-4 py-1.5 text-sm font-medium text-white hover:bg-emerald-700 disabled:opacity-40"
        >
          {pulling || pullRunning ? "Pulling…" : "Download template"}
        </button>
        {!pullRunning && !pullDone ? (
          <button
            type="button"
            onClick={onSkip}
            className="text-xs font-medium text-slate-500 dark:text-slate-400 underline-offset-2 hover:text-slate-700 dark:hover:text-slate-200 hover:underline"
          >
            Skip for now
          </button>
        ) : null}
      </div>
    </div>
  );
}
