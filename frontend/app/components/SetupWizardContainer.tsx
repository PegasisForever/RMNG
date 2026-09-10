// First-run setup wizard, impure half. Everything the card is not allowed to do lives here:
// the per-step config PUT. The markup is SetupWizardView.
//
// The route renders this INSTEAD of the dashboard while `!setupComplete`. Each step persists
// via `putConfig` on Next; a failed PUT blocks the advance and surfaces the standard red
// banner. The Finish step's `putConfig({ setupComplete: true })` latches setup and also
// ensures the `rmng` bridge network.
import { useCallback, useState } from "react";

import { EnvChecklistContainer } from "~/components/EnvChecklistContainer";
import { SetupWizardView } from "~/components/SetupWizardView";
import { putConfig } from "~/lib/api";
import {
  nextDisabled,
  serverPatch,
  SETUP_STEPS,
  setupDraftFrom,
  type SetupDraft,
} from "~/lib/setupDraft";
import type { AppConfigRedacted } from "~/lib/wire/AppConfigRedacted";

export function SetupWizardContainer({
  initialConfig,
  onDone,
}: {
  /** The config as the server sent it at mount. Seeds the form and the tracked config below.
   *  Read it nowhere else, because the route fetches it once and never refetches. */
  initialConfig: AppConfigRedacted;
  /** Called after setup latches; the parent refetches config and swaps to the dashboard. */
  onDone: () => void;
}) {
  const [step, setStep] = useState(0);
  const [error, setError] = useState<string | null>(null);
  const [saving, setSaving] = useState(false);
  const [draft, setDraft] = useState<SetupDraft>(() =>
    setupDraftFrom(initialConfig),
  );

  // The config as the server last confirmed it, which is what every step's patch reads for the
  // fields the form does not carry: the layout preset the edited arrangement belongs to.
  //
  // It has to be state rather than the `initialConfig` prop. The route fetches config exactly
  // once and never refetches, while the wizard writes to it on every Next. Reading the prop
  // after a save answers with a value the operator has already replaced. Every successful PUT
  // answers with the post-merge redacted config (`ConfigPutResponse.config`), so the response
  // body is the authority here. No extra GET, and a value the server normalized on the way in
  // shows up as the server holds it.
  const [config, setConfig] = useState<AppConfigRedacted>(initialConfig);

  // Whether every REQUIRED environment check passes. Reported up by the checklist's own
  // container, because the answer is a function of a response only that half has.
  const [envOk, setEnvOk] = useState(false);
  const onEnvChange = useCallback((ok: boolean) => setEnvOk(ok), []);

  function updateDraft<K extends keyof SetupDraft>(
    key: K,
    value: SetupDraft[K],
  ) {
    setDraft((d) => ({ ...d, [key]: value }));
  }

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
      // Step 1 sends nothing now (the clone subnet is hardcoded on the server); it only
      // gates on the environment checks.
    } else if (step === 1) {
      if (!(await persist(serverPatch(draft, config)))) return;
    }
    setStep((s) => Math.min(SETUP_STEPS.length - 1, s + 1));
    setError(null);
  }

  function back() {
    if (saving) return;
    setError(null);
    setStep((s) => Math.max(0, s - 1));
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
      error={error}
      saving={saving}
      nextDisabled={nextDisabled({ step, saving, envOk })}
      onNext={next}
      onBack={back}
      onFinish={finish}
    />
  );
}
