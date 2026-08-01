// The first-run wizard's markup: a full-page centered card with a step indicator, one step in
// the body, and a Back/Next footer. It renders from props alone — no config PUT, no pull, no
// operation stream — so every state it can be in is a story. SetupWizardContainer owns all
// three of those and hands the results down.
//
// NOT a dismissable modal: there is no Escape, no overlay click and no ✕, because the
// dashboard does not exist behind it. The route renders this INSTEAD of the dashboard while
// `setupComplete` is false.
//
// The form is one editable model (`SetupDraft`) plus a single `onDraftChange`, rather than a
// value/onChange pair per field. What is NOT in the draft is everything the server decides:
// whether a save is in flight, whether the environment probe passed, and how far along the
// template pull is.
import { Check } from "lucide-react";
import { useState, type ReactNode } from "react";

import { SetupEnvironmentStep } from "~/components/SetupEnvironmentStep";
import { SetupReviewStep } from "~/components/SetupReviewStep";
import { SetupServerStep } from "~/components/SetupServerStep";
import { SetupTemplateStep } from "~/components/SetupTemplateStep";
import { SETUP_STEPS, type SetupDraft } from "~/lib/setupDraft";
import type { Operation } from "~/lib/types";

export interface SetupWizardViewProps {
  /** Which step is showing, indexing `SETUP_STEPS`. */
  step: number;
  /** The whole form, as one editable model. */
  draft: SetupDraft;
  /** Write one field back. The container holds the draft; this is how a keystroke reaches it. */
  onDraftChange: <K extends keyof SetupDraft>(key: K, value: SetupDraft[K]) => void;

  /** The environment preflight, as a slot: the probe behind it is a fetch, so the container
   *  decides what mounts there. */
  envChecklist: ReactNode;

  /** The configured `docker.templateReference`, which is what a blank template field pulls. */
  templatePlaceholder: string;
  /** The reference step 3 saves and pulls, already resolved against the placeholder. The
   *  review step names it, so a skipped download still shows what clones will be built from. */
  savedTemplateReference: string;
  /** The template pull this wizard started, once it shows up in the live op list. */
  pullOperation: Operation | null;
  /** The reference the pull was started for. */
  pullTarget: string | null;
  /** The POST that starts the pull is in flight. */
  pulling: boolean;
  /** The pull operation is running / has finished. */
  pullRunning: boolean;
  pullDone: boolean;
  /** The Download button may fire. */
  canPull: boolean;
  onPull: () => void;

  /** The last failed save or pull, in the wizard's own banner. */
  error: string | null;
  /** A config PUT is in flight. Locks the footer. */
  saving: boolean;
  /** Next refuses to advance: a failing required check, an invalid subnet, or a running pull. */
  nextDisabled: boolean;
  onNext: () => void;
  onBack: () => void;
  /** Latch setup and hand the route back to the dashboard. */
  onFinish: () => void;
}

export function SetupWizardView({
  step,
  draft,
  onDraftChange,
  envChecklist,
  templatePlaceholder,
  savedTemplateReference,
  pullOperation,
  pullTarget,
  pulling,
  pullRunning,
  pullDone,
  canPull,
  onPull,
  error,
  saving,
  nextDisabled,
  onNext,
  onBack,
  onFinish,
}: SetupWizardViewProps) {
  // Whether the ports block on the Server step is expanded. Ephemeral: it is not part of the
  // form, nothing outside this card reads it, and it resets every time the wizard mounts.
  const [portsOpen, setPortsOpen] = useState(false);

  return (
    <div className="flex min-h-screen items-center justify-center bg-slate-50 dark:bg-slate-900 p-4">
      <div className="flex max-h-[92vh] w-full max-w-2xl flex-col overflow-hidden rounded-xl border border-slate-200 dark:border-slate-700 bg-white dark:bg-slate-800 shadow-xl">
        {/* Header + step indicator. */}
        <div className="shrink-0 border-b border-slate-100 dark:border-slate-800 px-6 pb-4 pt-5">
          <h1 className="text-lg font-semibold text-slate-900 dark:text-slate-100">Set up rmng</h1>
          <p className="mt-0.5 text-xs text-slate-400 dark:text-slate-500">
            First-run configuration — a few settings are baked in for good, so choose carefully.
          </p>
          <div className="mt-4 flex items-center gap-2">
            {SETUP_STEPS.map((label, i) => (
              <div key={label} className="flex flex-1 items-center gap-2">
                <div className="flex items-center gap-2">
                  <span
                    className={`flex h-6 w-6 shrink-0 items-center justify-center rounded-full text-xs font-semibold ${
                      i === step
                        ? "bg-emerald-600 text-white"
                        : i < step
                          ? "bg-emerald-100 dark:bg-emerald-900/40 text-emerald-700 dark:text-emerald-400"
                          : "bg-slate-100 dark:bg-slate-800 text-slate-400 dark:text-slate-500"
                    }`}
                  >
                    {i < step ? <Check className="size-4" /> : i + 1}
                  </span>
                  <span
                    className={`hidden text-xs font-medium sm:inline ${
                      i === step ? "text-slate-800 dark:text-slate-100" : "text-slate-400 dark:text-slate-500"
                    }`}
                  >
                    {label}
                  </span>
                </div>
                {i < SETUP_STEPS.length - 1 ? (
                  <div className="h-px flex-1 bg-slate-200 dark:bg-slate-700" />
                ) : null}
              </div>
            ))}
          </div>
        </div>

        {/* Body. */}
        <div className="min-h-0 flex-1 overflow-y-auto px-6 py-5">
          {error ? (
            <div className="mb-4 rounded border border-red-200 dark:border-red-900 bg-red-50 dark:bg-red-950/40 px-3 py-2 text-xs text-red-700 dark:text-red-400">
              {error}
            </div>
          ) : null}

          {/* Step 1: Environment. */}
          {step === 0 ? (
            <SetupEnvironmentStep
              subnet={draft.subnet}
              envChecklist={envChecklist}
              onSubnetChange={(subnet) => onDraftChange("subnet", subnet)}
            />
          ) : null}

          {/* Step 2: Server. */}
          {step === 1 ? (
            <SetupServerStep
              draft={draft}
              onDraftChange={onDraftChange}
              portsOpen={portsOpen}
              onPortsOpenChange={setPortsOpen}
            />
          ) : null}

          {/* Step 3: Download template. */}
          {step === 2 ? (
            <SetupTemplateStep
              templateReference={draft.templateReference}
              placeholder={templatePlaceholder}
              operation={pullOperation}
              pullTarget={pullTarget}
              pulling={pulling}
              pullRunning={pullRunning}
              pullDone={pullDone}
              canPull={canPull}
              onTemplateReferenceChange={(v) => onDraftChange("templateReference", v)}
              onPull={onPull}
              onSkip={onNext}
            />
          ) : null}

          {/* Step 4: Finish. */}
          {step === 3 ? (
            <SetupReviewStep
              draft={draft}
              savedTemplateReference={savedTemplateReference}
              pullTarget={pullTarget}
              pullDone={pullDone}
            />
          ) : null}
        </div>

        {/* Footer: Back / Next / Finish. */}
        <div className="flex shrink-0 items-center justify-between gap-2 border-t border-slate-100 dark:border-slate-800 bg-white dark:bg-slate-800 px-6 py-3">
          <button
            type="button"
            onClick={onBack}
            disabled={step === 0 || saving}
            className="rounded border border-slate-300 dark:border-slate-600 px-3 py-1.5 text-sm text-slate-600 dark:text-slate-300 hover:bg-slate-50 dark:hover:bg-slate-800 disabled:opacity-40"
          >
            Back
          </button>
          {step < SETUP_STEPS.length - 1 ? (
            <button
              type="button"
              onClick={onNext}
              disabled={nextDisabled}
              className="rounded bg-emerald-600 px-4 py-1.5 text-sm font-medium text-white hover:bg-emerald-700 disabled:opacity-40"
            >
              {saving ? "Saving…" : "Next"}
            </button>
          ) : (
            <button
              type="button"
              onClick={onFinish}
              disabled={saving}
              className="rounded bg-emerald-600 px-4 py-1.5 text-sm font-medium text-white hover:bg-emerald-700 disabled:opacity-40"
            >
              {saving ? "Finishing…" : "Finish setup"}
            </button>
          )}
        </div>
      </div>
    </div>
  );
}
