// First-run setup wizard. Replaces the dashboard while `!setupComplete` — a
// full-page centered card (NOT a dismissable modal: no Escape/overlay-click
// close, no ✕). Each step persists via `putConfig` on Next; a failed PUT blocks
// the advance and surfaces the standard red banner. The one-time fields (subnet)
// stay editable here because the server only latches them once `setupComplete`
// flips (via the Finish step's `putConfig({ setupComplete: true })`, which also
// ensures the `rmng` bridge network).
import { Check, ChevronDown, ChevronRight } from "lucide-react";
import { useCallback, useState } from "react";

import { EnvChecklist } from "~/components/EnvChecklist";
import { MonitorsEditor, type Mon } from "~/components/MonitorsEditor";
import { OperationProgress } from "~/components/OperationProgress";
import { pullTemplate, putConfig } from "~/lib/api";
import type { AppConfigRedacted } from "~/lib/wire/AppConfigRedacted";
import type { ChromaMode } from "~/lib/wire/ChromaMode";
import type { ControlState } from "~/lib/types";

const input =
  "w-full rounded border border-slate-300 dark:border-slate-600 px-2 py-1 text-sm focus:border-slate-400 dark:focus:border-slate-500 focus:outline-none dark:bg-slate-800 dark:text-slate-100";

/** Mirror of the server's `validate_docker_subnet`: an IPv4 CIDR with a /16–/24 prefix. */
function isValidSubnet(s: string): boolean {
  const [ip, prefix, ...rest] = s.split("/");
  if (rest.length > 0 || prefix === undefined) return false;
  const p = Number(prefix);
  if (!Number.isInteger(p) || p < 16 || p > 24) return false;
  const octets = ip.split(".");
  return (
    octets.length === 4 &&
    octets.every((o) => /^\d+$/.test(o) && Number(o) >= 0 && Number(o) <= 255)
  );
}

const STEPS = ["Environment", "Server", "Download template", "Finish"] as const;

function Field({ label, children }: { label: string; children: React.ReactNode }) {
  return (
    <label className="block">
      <span className="mb-0.5 block text-xs font-medium text-slate-500 dark:text-slate-400">{label}</span>
      {children}
    </label>
  );
}

/** The amber "cannot be changed after setup" callout for one-time fields. */
function OneTimeWarning({ children }: { children: React.ReactNode }) {
  return (
    <div className="rounded border border-amber-300 dark:border-amber-900 bg-amber-50 dark:bg-amber-950/40 px-3 py-2 text-xs text-amber-800 dark:text-amber-400">
      {children}
    </div>
  );
}

export function SetupWizard({
  state,
  initialConfig,
  onDone,
}: {
  state: ControlState;
  initialConfig: AppConfigRedacted;
  /** Called after setup latches; the parent refetches config and swaps to the dashboard. */
  onDone: () => void;
}) {
  const [step, setStep] = useState(0);
  const [error, setError] = useState<string | null>(null);
  const [saving, setSaving] = useState(false);

  // --- Step 1: Environment ---
  const [envOk, setEnvOk] = useState(false);
  const [subnet, setSubnet] = useState(initialConfig.docker.subnet);

  // --- Step 2: Server ---
  const [hostnamePrefix, setHostnamePrefix] = useState(initialConfig.docker.hostnamePrefix);
  const [cloneCpus, setCloneCpus] = useState(initialConfig.docker.cloneCpus);
  const [cloneMemoryMb, setCloneMemoryMb] = useState(initialConfig.docker.cloneMemoryMb);
  // Mirror the server's `effective_monitors()`: the active preset, else the first, else a
  // single-1080p default (the wizard only edits one layout, so there's no picker here).
  const activePreset =
    initialConfig.layoutPresets.find((p) => p.name === initialConfig.activeLayout) ??
    initialConfig.layoutPresets[0];
  const layoutName = activePreset?.name || "Default";
  const [monitors, setMonitors] = useState<Mon[]>(
    activePreset?.monitors.length
      ? activePreset.monitors.map((m) => ({ ...m }))
      : [{ width: 1920, height: 1080, x: 0, y: 0, primary: true }],
  );
  const [chroma, setChroma] = useState<ChromaMode>(initialConfig.chroma);
  const [detectorInferenceUrl, setDetectorInferenceUrl] = useState(
    initialConfig.detectorInferenceUrl,
  );
  const [portsOpen, setPortsOpen] = useState(false);
  const [listen, setListen] = useState({ ...initialConfig.listen });
  const [agentPort, setAgentPort] = useState(initialConfig.agentPort);

  // --- Step 3: Download template ---
  const [templateReference, setTemplateReference] = useState(
    initialConfig.docker.templateReference,
  );
  const [pulling, setPulling] = useState(false);
  const [pullTarget, setPullTarget] = useState<string | null>(null);

  const monitorsPatch = () =>
    monitors.map((m) => ({
      width: Math.max(1, m.width),
      height: Math.max(1, m.height),
      x: Math.max(0, m.x),
      y: Math.max(0, m.y),
      primary: m.primary,
    }));

  // Round-trip every existing preset — like SettingsPanel's save — instead of sending just
  // the one being edited. The server's `merge_update` replaces the whole `layoutPresets`
  // array wholesale (that's how SettingsPanel expresses deletes), so a single-element patch
  // here would silently delete every other named preset on a mature config.
  const layoutPresetsPatch = () => {
    const existing = initialConfig.layoutPresets;
    if (!existing.length) {
      return [{ name: layoutName || "Default", monitors: monitorsPatch() }];
    }
    const updated = existing.map((p) =>
      p.name === layoutName ? { ...p, monitors: monitorsPatch() } : p,
    );
    return existing.some((p) => p.name === layoutName)
      ? updated
      : [...updated, { name: layoutName, monitors: monitorsPatch() }];
  };

  // The pull op is kind "pull" with target === the pulled reference (jobs.rs start_pull →
  // make_op(Pull, reference, None)).
  const imgOp = pullTarget
    ? state.operations.find((o) => o.kind === "pull" && o.target === pullTarget)
    : undefined;
  const imgRunning = imgOp?.status === "running";
  const imgDone = imgOp?.status === "done";

  const onEnvChange = useCallback((ok: boolean) => setEnvOk(ok), []);

  /** Persist this step's fields; resolves true on success, false (banner shown) on failure. */
  async function persist(patch: Record<string, unknown>): Promise<boolean> {
    setSaving(true);
    setError(null);
    try {
      await putConfig(patch);
      return true;
    } catch (e) {
      setError((e as Error).message);
      return false;
    } finally {
      setSaving(false);
    }
  }

  const subnetOk = subnet.trim().length > 0 && isValidSubnet(subnet.trim());

  async function next() {
    if (saving) return;
    if (step === 0) {
      if (!subnetOk) {
        setError("Enter a valid IPv4 CIDR subnet (/16–/24), e.g. 10.99.0.0/24.");
        return;
      }
      if (!(await persist({ docker: { subnet: subnet.trim() } }))) return;
    } else if (step === 1) {
      const ok = await persist({
        docker: { hostnamePrefix, cloneCpus, cloneMemoryMb },
        layoutPresets: layoutPresetsPatch(),
        chroma,
        detectorInferenceUrl,
        listen,
        agentPort,
      });
      if (!ok) return;
    }
    // Step 2 (download template) has nothing to persist — the pull happens via pullTemplate.
    setStep((s) => Math.min(STEPS.length - 1, s + 1));
    setError(null);
  }

  function back() {
    if (saving) return;
    setError(null);
    setStep((s) => Math.max(0, s - 1));
  }

  async function pull() {
    // Resolve to the exact reference the server will pull (blank ⇒ configured default), so
    // `pullTarget` matches the op's target and we can track it over /events.
    const reference = templateReference.trim() || initialConfig.docker.templateReference;
    if (!reference || pulling || imgRunning) return;
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

  const canPull =
    templateReference.trim().length > 0 && !pulling && !imgRunning && !imgDone;
  // Env step blocks Next until required checks pass + a valid subnet; download-template
  // step blocks Next while a pull is running.
  const nextDisabled =
    saving ||
    (step === 0 && (!envOk || !subnetOk)) ||
    (step === 2 && imgRunning);

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
            {STEPS.map((label, i) => (
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
                {i < STEPS.length - 1 ? (
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
            <div className="space-y-4">
              <p className="text-sm text-slate-600 dark:text-slate-300">
                rmng drives your local Docker daemon over its unix socket. Confirm the environment
                is ready, then pick the private subnet for the clone network.
              </p>
              <EnvChecklist onChange={onEnvChange} />

              <OneTimeWarning>
                The clone network subnet is baked into the <code>rmng</code> bridge and every
                clone's static IP at first-run setup — it{" "}
                <strong>cannot be changed after setup</strong>.
              </OneTimeWarning>
              <Field label="Clone network subnet (IPv4 CIDR, /16–/24)">
                <input
                  value={subnet}
                  onChange={(e) => setSubnet(e.target.value)}
                  placeholder="10.99.0.0/24"
                  spellCheck={false}
                  className={input}
                />
                {subnet.trim() && !subnetOk ? (
                  <span className="mt-1 block text-[11px] text-red-600 dark:text-red-400">
                    must be an IPv4 CIDR with a /16–/24 prefix, e.g. 10.99.0.0/24
                  </span>
                ) : (
                  <span className="mt-0.5 block text-xs text-slate-400 dark:text-slate-500">
                    <code>.1</code> gateway, <code>.2</code> control-server, <code>.10+</code>{" "}
                    clone pool.
                  </span>
                )}
              </Field>
            </div>
          ) : null}

          {/* Step 2: Server. */}
          {step === 1 ? (
            <div className="space-y-4">
              <p className="text-sm text-slate-600 dark:text-slate-300">
                Server-side layout and defaults for the fleet.
              </p>

              <Field label="Clone hostname prefix">
                <input
                  value={hostnamePrefix}
                  onChange={(e) => setHostnamePrefix(e.target.value)}
                  placeholder="pega-"
                  spellCheck={false}
                  className={input}
                />
                <span className="mt-0.5 block text-xs text-slate-400 dark:text-slate-500">
                  Prepended to derived clone hostnames — e.g.{" "}
                  <code>{hostnamePrefix || "pega-"}</code>dev-123.
                </span>
              </Field>

              <div className="grid grid-cols-2 gap-3">
                <Field label="CPU limit per clone (cores)">
                  <input
                    type="number"
                    min={1}
                    value={cloneCpus}
                    onChange={(e) => setCloneCpus(Number(e.target.value) || 0)}
                    className={input}
                  />
                </Field>
                <Field label="Memory limit per clone (MB)">
                  <input
                    type="number"
                    min={1024}
                    value={cloneMemoryMb}
                    onChange={(e) => setCloneMemoryMb(Number(e.target.value) || 0)}
                    className={input}
                  />
                </Field>
              </div>

              <div>
                <span className="mb-1 block text-xs font-medium text-slate-500 dark:text-slate-400">Monitors</span>
                <MonitorsEditor monitors={monitors} onChange={setMonitors} />
              </div>

              <Field label="Chroma mode">
                <select
                  value={chroma}
                  onChange={(e) => setChroma(e.target.value as ChromaMode)}
                  className={input}
                >
                  <option value="yuv420">4:2:0 (default)</option>
                  <option value="yuv444">4:4:4 (AVC444, ≤1440p/monitor)</option>
                </select>
              </Field>

              <Field label="Detector inference URL">
                <input
                  value={detectorInferenceUrl}
                  onChange={(e) => setDetectorInferenceUrl(e.target.value)}
                  placeholder="http://…"
                  spellCheck={false}
                  className={input}
                />
              </Field>

              {/* Ports — collapsed by default. */}
              <div className="border-t border-slate-100 dark:border-slate-800 pt-3">
                <button
                  type="button"
                  onClick={() => setPortsOpen((o) => !o)}
                  className="flex items-center gap-1 text-xs font-medium text-slate-500 dark:text-slate-400 hover:text-slate-700 dark:hover:text-slate-200"
                >
                  {portsOpen ? <ChevronDown className="size-4" /> : <ChevronRight className="size-4" />}
                  {portsOpen ? "Hide" : "Show"} ports
                </button>
                {portsOpen ? (
                  <div className="mt-2 grid grid-cols-2 gap-3">
                    {(["web", "video", "cloneMcp", "daemonMcp"] as const).map((k) => (
                      <Field key={k} label={`Port: ${k}`}>
                        <input
                          type="number"
                          value={listen[k]}
                          onChange={(e) =>
                            setListen({ ...listen, [k]: Number(e.target.value) || 0 })
                          }
                          className={input}
                        />
                      </Field>
                    ))}
                    <Field label="Agent-wrapper port">
                      <input
                        type="number"
                        value={agentPort}
                        onChange={(e) => setAgentPort(Number(e.target.value) || 0)}
                        className={input}
                      />
                    </Field>
                  </div>
                ) : null}
              </div>
            </div>
          ) : null}

          {/* Step 3: Download template. */}
          {step === 2 ? (
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
                  onChange={(e) => setTemplateReference(e.target.value)}
                  placeholder={initialConfig.docker.templateReference}
                  spellCheck={false}
                  disabled={imgRunning || imgDone}
                  className={`${input} disabled:bg-slate-50 dark:disabled:bg-slate-900 disabled:text-slate-400 dark:disabled:text-slate-500`}
                />
                <span className="mt-0.5 block text-xs text-slate-400 dark:text-slate-500">
                  Docker Hub <code>repo:tag</code> the template is pulled from — clones are
                  created from this exact reference.
                </span>
              </Field>

              {imgOp ? <OperationProgress op={imgOp} /> : null}
              {imgDone ? (
                <p className="text-xs font-medium text-emerald-600 dark:text-emerald-400">
                  ✓ Template “{pullTarget}” pulled.
                </p>
              ) : null}

              <div className="flex items-center gap-3">
                <button
                  type="button"
                  onClick={pull}
                  disabled={!canPull}
                  className="rounded bg-emerald-600 px-4 py-1.5 text-sm font-medium text-white hover:bg-emerald-700 disabled:opacity-40"
                >
                  {pulling || imgRunning ? "Pulling…" : "Download template"}
                </button>
                {!imgRunning && !imgDone ? (
                  <button
                    type="button"
                    onClick={next}
                    className="text-xs font-medium text-slate-500 dark:text-slate-400 underline-offset-2 hover:text-slate-700 dark:hover:text-slate-200 hover:underline"
                  >
                    Skip for now
                  </button>
                ) : null}
              </div>
            </div>
          ) : null}

          {/* Step 4: Finish. */}
          {step === 3 ? (
            <div className="space-y-4">
              <p className="text-sm text-slate-600 dark:text-slate-300">
                Review your configuration, then finish setup. The one-time subnet latches and the{" "}
                <code>rmng</code> network is ensured when you click Finish.
              </p>
              <dl className="divide-y divide-slate-100 dark:divide-slate-800 rounded border border-slate-200 dark:border-slate-700 text-sm">
                {(
                  [
                    ["Clone network subnet", subnet || "—"],
                    ["Clone hostname prefix", hostnamePrefix || "(none)"],
                    ["CPU limit per clone", `${cloneCpus} cores`],
                    ["Memory limit per clone", `${cloneMemoryMb} MB`],
                    ["Monitors", `${monitors.length} monitor(s)`],
                    ["Chroma", chroma],
                    ["Detector URL", detectorInferenceUrl || "(none)"],
                    [
                      "Template image",
                      imgDone ? `${pullTarget} ✓` : "not pulled (pull one later)",
                    ],
                  ] as const
                ).map(([k, v]) => (
                  <div key={k} className="flex justify-between gap-3 px-3 py-2">
                    <dt className="text-slate-500 dark:text-slate-400">{k}</dt>
                    <dd className="text-right font-medium text-slate-800 dark:text-slate-100">{v}</dd>
                  </div>
                ))}
              </dl>
            </div>
          ) : null}
        </div>

        {/* Footer: Back / Next / Finish. */}
        <div className="flex shrink-0 items-center justify-between gap-2 border-t border-slate-100 dark:border-slate-800 bg-white dark:bg-slate-800 px-6 py-3">
          <button
            type="button"
            onClick={back}
            disabled={step === 0 || saving}
            className="rounded border border-slate-300 dark:border-slate-600 px-3 py-1.5 text-sm text-slate-600 dark:text-slate-300 hover:bg-slate-50 dark:hover:bg-slate-800 disabled:opacity-40"
          >
            Back
          </button>
          {step < STEPS.length - 1 ? (
            <button
              type="button"
              onClick={next}
              disabled={nextDisabled}
              className="rounded bg-emerald-600 px-4 py-1.5 text-sm font-medium text-white hover:bg-emerald-700 disabled:opacity-40"
            >
              {saving ? "Saving…" : "Next"}
            </button>
          ) : (
            <button
              type="button"
              onClick={finish}
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
