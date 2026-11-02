// The first-run wizard's markup: a full-page centered card with a step indicator, one step in
// the body, and a Back/Next footer. It renders from props alone — no config PUT — so every
// state it can be in is a story. SetupWizardContainer owns the PUT and hands the result down.
//
// There is no clone's screen under it, so no glass (`~/desk/GLASS.md`): the flat content on a
// solid ground, as in the control panel's own window.
//
// NOT a dismissable modal: there is no Escape, no overlay click and no ✕, because the app
// does not exist behind it. The route renders this INSTEAD of the app while `setupComplete`
// is false.
//
// The form is one editable model (`SetupDraft`) plus a single `onDraftChange`, rather than a
// value/onChange pair per field. What is NOT in the draft is everything the server decides:
// whether a save is in flight and whether the environment probe passed.
import { Check } from "lucide-react";
import { useState, type ReactNode } from "react";

import { SetupEnvironmentStep } from "~/components/SetupEnvironmentStep";
import { SetupReviewStep } from "~/components/SetupReviewStep";
import { SetupServerStep } from "~/components/SetupServerStep";
import { BUTTON, DONE_BUTTON, ErrorLine, LINE, R_PIECE, TEXT, TEXT_3 } from "~/desk/control/ui";
import { SETUP_STEPS, type SetupDraft } from "~/lib/setupDraft";

export interface SetupWizardViewProps {
  /** Which step is showing, indexing `SETUP_STEPS`. */
  step: number;
  /** The whole form, as one editable model. */
  draft: SetupDraft;
  /** Write one field back. The container holds the draft; this is how a keystroke reaches it. */
  onDraftChange: <K extends keyof SetupDraft>(
    key: K,
    value: SetupDraft[K],
  ) => void;

  /** The environment preflight, as a slot: the probe behind it is a fetch, so the container
   *  decides what mounts there. */
  envChecklist: ReactNode;

  /** The last failed save, in the wizard's own banner. */
  error: string | null;
  /** A config PUT is in flight. Locks the footer. */
  saving: boolean;
  /** Next refuses to advance: a failing required check. */
  nextDisabled: boolean;
  onNext: () => void;
  onBack: () => void;
  /** Latch setup and hand the route back to the app. */
  onFinish: () => void;
}

export function SetupWizardView({
  step,
  draft,
  onDraftChange,
  envChecklist,
  error,
  saving,
  nextDisabled,
  onNext,
  onBack,
  onFinish,
}: SetupWizardViewProps) {
  return (
    <div className="flex min-h-screen items-center justify-center bg-neutral-100 p-4 dark:bg-neutral-950">
      <div className={`flex max-h-[92vh] w-full max-w-2xl flex-col overflow-hidden rounded-2xl border bg-white dark:bg-neutral-900 ${LINE}`}>
        {/* Header + step indicator. */}
        <div className={`shrink-0 border-b px-6 pb-4 pt-5 ${LINE}`}>
          <h1 className={`text-lg font-semibold ${TEXT}`}>
            Set up rmng
          </h1>
          <p className={`mt-0.5 text-xs ${TEXT_3}`}>
            First-run configuration — a few settings are baked in for good, so
            choose carefully.
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
                          ? "bg-emerald-500/15 text-emerald-700 dark:bg-emerald-400/15 dark:text-emerald-300"
                          : `bg-black/5 dark:bg-white/10 ${TEXT_3}`
                    }`}
                  >
                    {i < step ? <Check className="size-4" /> : i + 1}
                  </span>
                  <span
                    className={`hidden text-xs font-medium sm:inline ${
                      i === step ? TEXT : TEXT_3
                    }`}
                  >
                    {label}
                  </span>
                </div>
                {i < SETUP_STEPS.length - 1 ? (
                  <div className="h-px flex-1 bg-black/10 dark:bg-white/10" />
                ) : null}
              </div>
            ))}
          </div>
        </div>

        {/* Body. */}
        <div className="min-h-0 flex-1 overflow-y-auto px-6 py-5">
          {error ? (
            <div className={`mb-4 overflow-hidden ${R_PIECE}`}>
              <ErrorLine message={error} />
            </div>
          ) : null}

          {/* Step 1: Environment. */}
          {step === 0 ? (
            <SetupEnvironmentStep envChecklist={envChecklist} />
          ) : null}

          {/* Step 2: Server. */}
          {step === 1 ? (
            <SetupServerStep draft={draft} onDraftChange={onDraftChange} />
          ) : null}

          {/* Step 3: Finish. Clone images build on demand from each preset's Dockerfile,
              so there is no template to download. */}
          {step === 2 ? <SetupReviewStep draft={draft} /> : null}
        </div>

        {/* Footer: Back / Next / Finish. */}
        <div className={`flex shrink-0 items-center justify-between gap-2 border-t px-6 py-3 ${LINE}`}>
          <button
            type="button"
            onClick={onBack}
            disabled={step === 0 || saving}
            className={`${BUTTON} px-3 py-1.5 text-sm disabled:opacity-40`}
          >
            Back
          </button>
          {step < SETUP_STEPS.length - 1 ? (
            <button
              type="button"
              onClick={onNext}
              disabled={nextDisabled}
              className={`${DONE_BUTTON} px-4 py-1.5 text-sm font-medium`}
            >
              {saving ? "Saving…" : "Next"}
            </button>
          ) : (
            <button
              type="button"
              onClick={onFinish}
              disabled={saving}
              className={`${DONE_BUTTON} px-4 py-1.5 text-sm font-medium`}
            >
              {saving ? "Finishing…" : "Finish setup"}
            </button>
          )}
        </div>
      </div>
    </div>
  );
}
