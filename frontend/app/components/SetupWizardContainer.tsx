// First-run setup wizard, impure half. Everything the card is not allowed to do lives here:
// the per-step config PUT, the template pull, the operation the pull returns, and the step
// number those two gate. The markup is SetupWizardView.
//
// The route renders this INSTEAD of the dashboard while `!setupComplete`. Each step persists
// via `putConfig` on Next; a failed PUT blocks the advance and surfaces the standard red
// banner. The one-time fields (subnet) stay editable here because the server only latches them
// once `setupComplete` flips (via the Finish step's `putConfig({ setupComplete: true })`,
// which also ensures the `rmng` bridge network).
import { useCallback, useState } from "react";

import { EnvChecklistContainer } from "~/components/EnvChecklistContainer";
import { SetupWizardView } from "~/components/SetupWizardView";
import { pullTemplate, putConfig } from "~/lib/api";
import {
  canPull,
  findPullOperation,
  nextDisabled,
  pullReference,
  serverPatch,
  SETUP_STEPS,
  setupDraftFrom,
  subnetOk,
  subnetPatch,
  templateFallback,
  templatePatch,
  type SetupDraft,
} from "~/lib/setupDraft";
import type { Operation } from "~/lib/types";
import type { AppConfigRedacted } from "~/lib/wire/AppConfigRedacted";

export function SetupWizardContainer({
  operations,
  initialConfig,
  onDone,
}: {
  /** Live operations from the SSE state — the started template pull is tracked through these. */
  operations: Operation[];
  /** The config as the server sent it at mount. Seeds the form and the tracked config below.
   *  Read it nowhere else, because the route fetches it once and never refetches. */
  initialConfig: AppConfigRedacted;
  /** Called after setup latches; the parent refetches config and swaps to the dashboard. */
  onDone: () => void;
}) {
  const [step, setStep] = useState(0);
  const [error, setError] = useState<string | null>(null);
  const [saving, setSaving] = useState(false);
  const [draft, setDraft] = useState<SetupDraft>(() => setupDraftFrom(initialConfig));

  // The config as the server last confirmed it, which is what every step's patch reads for the
  // fields the form does not carry: the layout preset the edited arrangement belongs to, and
  // the template reference a blank field falls back to.
  //
  // It has to be state rather than the `initialConfig` prop. The route fetches config exactly
  // once and never refetches, while the wizard writes to it on every Next. Reading the prop
  // after a save answers with a value the operator has already replaced, and step 3 is where
  // that costs data: the placeholder would name the OLD reference, and clearing the field
  // would then save that old reference back over the one just stored. Every successful PUT
  // answers with the post-merge redacted config (`ConfigPutResponse.config`), so the response
  // body is the authority here. No extra GET, and a value the server normalized on the way in
  // shows up as the server holds it.
  const [config, setConfig] = useState<AppConfigRedacted>(initialConfig);

  // Whether every REQUIRED environment check passes. Reported up by the checklist's own
  // container, because the answer is a function of a response only that half has.
  const [envOk, setEnvOk] = useState(false);
  const onEnvChange = useCallback((ok: boolean) => setEnvOk(ok), []);

  // The pull: whether the POST is in flight, and the reference it was started for.
  const [pulling, setPulling] = useState(false);
  const [pullTarget, setPullTarget] = useState<string | null>(null);

  function updateDraft<K extends keyof SetupDraft>(key: K, value: SetupDraft[K]) {
    setDraft((d) => ({ ...d, [key]: value }));
  }

  const pullOperation = findPullOperation(operations, pullTarget);
  const pullRunning = pullOperation?.status === "running";
  const pullDone = pullOperation?.status === "done";

  // The one reference step 3 deals in: what Download pulls, what Next saves, and what the
  // review step names. Resolved once here so those three cannot answer differently.
  const savedTemplateReference = pullReference(draft, config);

  /** Persist this step's fields; resolves true on success, false (banner shown) on failure. */
  async function persist(patch: unknown): Promise<boolean> {
    setSaving(true);
    setError(null);
    try {
      setConfig((await putConfig(patch)).config);
      return true;
    } catch (e) {
      setError((e as Error).message);
      return false;
    } finally {
      setSaving(false);
    }
  }

  async function next() {
    if (saving) return;
    if (step === 0) {
      if (!subnetOk(draft.subnet)) {
        setError("Enter a valid IPv4 CIDR subnet (/16–/24), e.g. 10.99.0.0/24.");
        return;
      }
      if (!(await persist(subnetPatch(draft)))) return;
    } else if (step === 1) {
      if (!(await persist(serverPatch(draft, config)))) return;
    } else if (step === 2) {
      // Leaving the template step saves its reference, whether it was pulled or skipped: the
      // pull is one use of the value, and `POST /api/images/pull` reads the saved one on every
      // later use. Skip goes through here too, because the View wires it to onNext.
      // A null patch is the empty case (blank field, no configured reference): nothing to save.
      const patch = templatePatch(draft, config);
      if (patch && !(await persist(patch))) return;
    }
    setStep((s) => Math.min(SETUP_STEPS.length - 1, s + 1));
    setError(null);
  }

  function back() {
    if (saving) return;
    setError(null);
    setStep((s) => Math.max(0, s - 1));
  }

  async function pull() {
    // The exact reference the server will pull (blank ⇒ configured default), so `pullTarget`
    // matches the op's target and we can track it over /events.
    const reference = savedTemplateReference;
    if (!reference || pulling || pullRunning) return;
    setPulling(true);
    setError(null);
    try {
      await pullTemplate(reference);
      setPullTarget(reference);
    } catch (e) {
      setError((e as Error).message);
    } finally {
      setPulling(false);
    }
  }

  async function finish() {
    if (saving) return;
    setSaving(true);
    setError(null);
    try {
      const res = await putConfig({ setupComplete: true });
      // Non-fatal: setup is already latched server-side. Surface the network warning
      // (the operator may need to `docker network rm rmng`) but don't leave the wizard —
      // the `rmng` network is also created lazily on the first clone. Clicking Finish
      // again is idempotent (setupComplete already true → no re-check) and proceeds.
      if (res.networkWarning) {
        setError(
          `Setup saved, but the rmng network could not be ensured: ${res.networkWarning}. ` +
            "It will be created on the first clone. Click Finish again to continue.",
        );
        return;
      }
      onDone();
    } catch (e) {
      setError((e as Error).message);
    } finally {
      setSaving(false);
    }
  }

  return (
    <SetupWizardView
      step={step}
      draft={draft}
      onDraftChange={updateDraft}
      envChecklist={<EnvChecklistContainer onChange={onEnvChange} />}
      templatePlaceholder={templateFallback(config)}
      savedTemplateReference={savedTemplateReference}
      pullOperation={pullOperation ?? null}
      pullTarget={pullTarget}
      pulling={pulling}
      pullRunning={pullRunning}
      pullDone={pullDone}
      canPull={canPull({
        templateReference: draft.templateReference,
        pulling,
        pullRunning,
        pullDone,
      })}
      onPull={pull}
      error={error}
      saving={saving}
      nextDisabled={nextDisabled({
        step,
        saving,
        envOk,
        subnetOk: subnetOk(draft.subnet),
        pullRunning,
      })}
      onNext={next}
      onBack={back}
      onFinish={finish}
    />
  );
}
