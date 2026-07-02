// First-run setup wizard. Replaces the dashboard while `!setupComplete` — a
// full-page centered card (NOT a dismissable modal: no Escape/overlay-click
// close, no ✕). Each step persists via `putConfig` on Next; a failed PUT blocks
// the advance and surfaces the standard red banner. Storage/bridge/dataDir are
// freely editable here because the server only latches the one-time fields once
// `setupComplete` flips (via the Finish step's `putConfig({ setupComplete: true })`).
import { useState } from "react";

import { MonitorsEditor, type Mon } from "~/components/MonitorsEditor";
import { OperationProgress } from "~/components/OperationProgress";
import { bootstrapTemplate, putConfig, testConfig } from "~/lib/api";
import type { AppConfigRedacted } from "~/lib/wire/AppConfigRedacted";
import type { ChromaMode } from "~/lib/wire/ChromaMode";
import type { ControlState } from "~/lib/types";

const input =
  "w-full rounded border border-slate-300 px-2 py-1 text-sm focus:border-slate-400 focus:outline-none";

/** Template resource defaults — mirror NewTemplateModal / what the template is built with. */
const DEFAULT_RESOURCES = { cores: 16, memoryMb: 32768, diskGb: 128 };

/** Mirrors the server's `is_dns_label` (same rule NewTemplateModal enforces). */
function isDnsLabel(s: string): boolean {
  return s.length <= 63 && /^[a-z0-9]([a-z0-9-]*[a-z0-9])?$/.test(s);
}

const STEPS = ["Proxmox", "Server", "Template", "Finish"] as const;

function Field({ label, children }: { label: string; children: React.ReactNode }) {
  return (
    <label className="block">
      <span className="mb-0.5 block text-xs font-medium text-slate-500">{label}</span>
      {children}
    </label>
  );
}

/** The amber "cannot be changed after setup" callout for one-time fields. */
function OneTimeWarning({ children }: { children: React.ReactNode }) {
  return (
    <div className="rounded border border-amber-300 bg-amber-50 px-3 py-2 text-xs text-amber-800">
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

  // --- Step 1: Proxmox ---
  const [proxmoxSsh, setProxmoxSsh] = useState("");
  const [proxmoxSshSet, setProxmoxSshSet] = useState(initialConfig.proxmoxSshSet);
  const [storage, setStorage] = useState(initialConfig.proxmoxStorage);
  const [bridge, setBridge] = useState(initialConfig.proxmoxBridge);
  const [testMsg, setTestMsg] = useState<string | null>(null);
  const [testing, setTesting] = useState(false);

  // --- Step 2: Server ---
  const [dataDir, setDataDir] = useState(initialConfig.dataDir);
  const [cloneSocket, setCloneSocket] = useState(initialConfig.cloneSocket);
  const [hostnamePrefix, setHostnamePrefix] = useState(initialConfig.proxmoxHostnamePrefix);
  const [monitors, setMonitors] = useState<Mon[]>(
    initialConfig.monitors.length
      ? initialConfig.monitors.map((m) => ({ ...m }))
      : [{ width: 1920, height: 1080, x: 0, y: 0, primary: true }],
  );
  const [chroma, setChroma] = useState<ChromaMode>(initialConfig.chroma);
  const [detectorInferenceUrl, setDetectorInferenceUrl] = useState(
    initialConfig.detectorInferenceUrl,
  );
  const [portsOpen, setPortsOpen] = useState(false);
  const [listen, setListen] = useState({ ...initialConfig.listen });
  const [agentPort, setAgentPort] = useState(initialConfig.agentPort);

  // --- Step 3: Template ---
  const [tplHostname, setTplHostname] = useState("");
  const [resources, setResources] = useState(DEFAULT_RESOURCES);
  const [provisioning, setProvisioning] = useState(false);
  const [provisionTarget, setProvisionTarget] = useState<string | null>(null);

  const monitorsPatch = () =>
    monitors.map((m) => ({
      width: Math.max(1, m.width),
      height: Math.max(1, m.height),
      x: Math.max(0, m.x),
      y: Math.max(0, m.y),
      primary: m.primary,
    }));

  // The bootstrap operation is created with kind "clone" and target === hostname
  // (verified in control-server jobs.rs `start_bootstrap` → make_op(Clone, …)).
  const tplOp = provisionTarget
    ? state.operations.find((o) => o.kind === "clone" && o.target === provisionTarget)
    : undefined;
  const tplRunning = tplOp?.status === "running";
  const tplDone = tplOp?.status === "done";

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

  async function runTest() {
    setTesting(true);
    setTestMsg("testing…");
    setError(null);
    // Save the SSH target first so the server tests what's on screen.
    const ok = await persist({
      proxmox: { ssh: proxmoxSsh, storage, bridge },
    });
    if (!ok) {
      setTestMsg(null);
      setTesting(false);
      return;
    }
    if (proxmoxSsh.trim()) setProxmoxSshSet(true);
    setProxmoxSsh("");
    try {
      const r = await testConfig("proxmox");
      setTestMsg(`${r.ok ? "✓" : "✗"} ${r.message}`);
    } catch (e) {
      setTestMsg(`✗ ${(e as Error).message}`);
    } finally {
      setTesting(false);
    }
  }

  async function next() {
    if (saving) return;
    if (step === 0) {
      if (!(await persist({ proxmox: { ssh: proxmoxSsh, storage, bridge } }))) return;
      if (proxmoxSsh.trim()) setProxmoxSshSet(true);
      setProxmoxSsh("");
    } else if (step === 1) {
      const ok = await persist({
        dataDir,
        cloneSocket,
        proxmox: { hostnamePrefix },
        monitors: monitorsPatch(),
        chroma,
        detectorInferenceUrl,
        listen,
        agentPort,
      });
      if (!ok) return;
    } else if (step === 2) {
      // Nothing to persist here — provisioning happens via bootstrapTemplate.
    }
    setStep((s) => Math.min(STEPS.length - 1, s + 1));
    setError(null);
  }

  function back() {
    if (saving) return;
    setError(null);
    setStep((s) => Math.max(0, s - 1));
  }

  async function provision() {
    const name = tplHostname.trim();
    if (!isDnsLabel(name) || provisioning || tplRunning) return;
    setProvisioning(true);
    setError(null);
    try {
      await bootstrapTemplate(name, resources);
      setProvisionTarget(name);
    } catch (e) {
      setError((e as Error).message);
    } finally {
      setProvisioning(false);
    }
  }

  async function finish() {
    if (saving) return;
    if (!(await persist({ setupComplete: true }))) return;
    onDone();
  }

  const setRes = (k: keyof typeof DEFAULT_RESOURCES, v: number) =>
    setResources((r) => ({ ...r, [k]: v }));

  const tplName = tplHostname.trim();
  const tplLabelOk = tplName.length === 0 || isDnsLabel(tplName);
  const canProvision = isDnsLabel(tplName) && !provisioning && !tplRunning && !tplDone;
  // On the Template step, a running provision blocks Next (mid-provision).
  const nextDisabled = saving || (step === 2 && tplRunning);

  return (
    <div className="flex min-h-screen items-center justify-center bg-slate-50 p-4">
      <div className="flex max-h-[92vh] w-full max-w-2xl flex-col overflow-hidden rounded-xl border border-slate-200 bg-white shadow-xl">
        {/* Header + step indicator. */}
        <div className="shrink-0 border-b border-slate-100 px-6 pb-4 pt-5">
          <h1 className="text-lg font-semibold text-slate-900">Set up rmng</h1>
          <p className="mt-0.5 text-xs text-slate-400">
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
                          ? "bg-emerald-100 text-emerald-700"
                          : "bg-slate-100 text-slate-400"
                    }`}
                  >
                    {i < step ? "✓" : i + 1}
                  </span>
                  <span
                    className={`hidden text-xs font-medium sm:inline ${
                      i === step ? "text-slate-800" : "text-slate-400"
                    }`}
                  >
                    {label}
                  </span>
                </div>
                {i < STEPS.length - 1 ? (
                  <div className="h-px flex-1 bg-slate-200" />
                ) : null}
              </div>
            ))}
          </div>
        </div>

        {/* Body. */}
        <div className="min-h-0 flex-1 overflow-y-auto px-6 py-5">
          {error ? (
            <div className="mb-4 rounded border border-red-200 bg-red-50 px-3 py-2 text-xs text-red-700">
              {error}
            </div>
          ) : null}

          {/* Step 1: Proxmox. */}
          {step === 0 ? (
            <div className="space-y-4">
              <p className="text-sm text-slate-600">
                Connect to your Proxmox host. rmng SSHes in to provision and manage containers.
              </p>
              <div className="flex items-end gap-2">
                <div className="flex-1">
                  <Field label="SSH target (e.g. root@10.0.0.100)">
                    <div className="flex items-center gap-2">
                      <input
                        type="password"
                        value={proxmoxSsh}
                        placeholder={
                          proxmoxSshSet ? "•••••••• (set — leave blank to keep)" : "root@…"
                        }
                        onChange={(e) => setProxmoxSsh(e.target.value)}
                        spellCheck={false}
                        className={input}
                      />
                      <span
                        className={`shrink-0 rounded px-1.5 py-0.5 text-[10px] font-semibold ${
                          proxmoxSshSet
                            ? "bg-emerald-100 text-emerald-700"
                            : "bg-slate-100 text-slate-400"
                        }`}
                      >
                        {proxmoxSshSet ? "set" : "unset"}
                      </span>
                    </div>
                  </Field>
                </div>
                <button
                  type="button"
                  onClick={runTest}
                  disabled={testing || saving}
                  className="rounded border border-slate-300 px-2.5 py-1.5 text-xs text-slate-600 hover:bg-slate-50 disabled:opacity-50"
                >
                  Test connection
                </button>
              </div>
              {testMsg ? <p className="text-xs text-slate-500">{testMsg}</p> : null}

              <OneTimeWarning>
                Storage pool + bridge are baked into every container's disk and NIC at provision
                time — they <strong>cannot be changed after setup</strong>.
              </OneTimeWarning>
              <div className="grid grid-cols-2 gap-3">
                <Field label="Storage pool">
                  <input
                    value={storage}
                    onChange={(e) => setStorage(e.target.value)}
                    placeholder="local-lvm"
                    spellCheck={false}
                    className={input}
                  />
                </Field>
                <Field label="Bridge">
                  <input
                    value={bridge}
                    onChange={(e) => setBridge(e.target.value)}
                    placeholder="vmbr0"
                    spellCheck={false}
                    className={input}
                  />
                </Field>
              </div>
            </div>
          ) : null}

          {/* Step 2: Server. */}
          {step === 1 ? (
            <div className="space-y-4">
              <p className="text-sm text-slate-600">
                Server-side layout and defaults for the fleet.
              </p>
              <OneTimeWarning>
                The data directory is baked into the on-disk layout at first-run setup — it{" "}
                <strong>cannot be changed after setup</strong>.
              </OneTimeWarning>
              <Field label="Data dir">
                <input
                  value={dataDir}
                  onChange={(e) => setDataDir(e.target.value)}
                  spellCheck={false}
                  className={input}
                />
              </Field>

              <OneTimeWarning>
                The clone media socket is baked into the template at provision time — it{" "}
                <strong>cannot be changed after setup</strong>. Changing it here requires restarting
                the control-server before provisioning the template.
              </OneTimeWarning>
              <Field label="Clone media socket">
                <input
                  value={cloneSocket}
                  onChange={(e) => setCloneSocket(e.target.value)}
                  placeholder="/srv/rmng-sock/clones.sock"
                  spellCheck={false}
                  className={input}
                />
              </Field>

              <Field label="Clone hostname prefix">
                <input
                  value={hostnamePrefix}
                  onChange={(e) => setHostnamePrefix(e.target.value)}
                  placeholder="pega-"
                  spellCheck={false}
                  className={input}
                />
                <span className="mt-0.5 block text-xs text-slate-400">
                  Prepended to derived clone hostnames — e.g.{" "}
                  <code>{hostnamePrefix || "pega-"}</code>dev-123.
                </span>
              </Field>

              <div>
                <span className="mb-1 block text-xs font-medium text-slate-500">Monitors</span>
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
              <div className="border-t border-slate-100 pt-3">
                <button
                  type="button"
                  onClick={() => setPortsOpen((o) => !o)}
                  className="text-xs font-medium text-slate-500 hover:text-slate-700"
                >
                  {portsOpen ? "▾ Hide" : "▸ Show"} ports
                </button>
                {portsOpen ? (
                  <div className="mt-2 grid grid-cols-2 gap-3">
                    {(["web", "video", "cloneMcp", "globalMcp", "daemonMcp"] as const).map((k) => (
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

          {/* Step 3: Template. */}
          {step === 2 ? (
            <div className="space-y-4">
              <p className="text-sm text-slate-600">
                Provision your first template container (Ubuntu 26.04, the base our patched GNOME
                is built for). Clones are made from it. You can skip this and do it later.
              </p>
              <Field label="Hostname">
                <input
                  value={tplHostname}
                  onChange={(e) => setTplHostname(e.target.value)}
                  placeholder="e.g. rmng-template"
                  spellCheck={false}
                  disabled={tplRunning || tplDone}
                  className={`${input} disabled:bg-slate-50 disabled:text-slate-400`}
                />
                {!tplLabelOk ? (
                  <span className="mt-1 block text-[11px] text-red-600">
                    lowercase letters, digits and hyphens only (no leading/trailing hyphen, ≤63
                    chars)
                  </span>
                ) : null}
              </Field>
              <div className="grid grid-cols-3 gap-3">
                {(
                  [
                    { key: "cores", label: "Cores", min: 1 },
                    { key: "memoryMb", label: "Memory (MB)", min: 1024 },
                    { key: "diskGb", label: "Disk (GB)", min: 8 },
                  ] as const
                ).map((f) => (
                  <Field key={f.key} label={f.label}>
                    <input
                      type="number"
                      min={f.min}
                      value={resources[f.key]}
                      onChange={(e) => setRes(f.key, Number(e.target.value) || 0)}
                      disabled={tplRunning || tplDone}
                      className={`${input} disabled:bg-slate-50 disabled:text-slate-400`}
                    />
                  </Field>
                ))}
              </div>

              {tplOp ? <OperationProgress op={tplOp} /> : null}
              {tplDone ? (
                <p className="text-xs font-medium text-emerald-600">
                  ✓ Template “{provisionTarget}” provisioned.
                </p>
              ) : null}

              <div className="flex items-center gap-3">
                <button
                  type="button"
                  onClick={provision}
                  disabled={!canProvision}
                  className="rounded bg-emerald-600 px-4 py-1.5 text-sm font-medium text-white hover:bg-emerald-700 disabled:opacity-40"
                >
                  {provisioning || tplRunning ? "Provisioning…" : "Provision"}
                </button>
                {!tplRunning && !tplDone ? (
                  <button
                    type="button"
                    onClick={next}
                    className="text-xs font-medium text-slate-500 underline-offset-2 hover:text-slate-700 hover:underline"
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
              <p className="text-sm text-slate-600">
                Review your configuration, then finish setup. The one-time settings latch when you
                click Finish.
              </p>
              <dl className="divide-y divide-slate-100 rounded border border-slate-200 text-sm">
                {(
                  [
                    ["Proxmox SSH", proxmoxSshSet ? "set" : "not set"],
                    ["Storage pool", storage || "—"],
                    ["Bridge", bridge || "—"],
                    ["Data dir", dataDir || "—"],
                    ["Clone media socket", cloneSocket || "—"],
                    ["Clone hostname prefix", hostnamePrefix || "(none)"],
                    ["Monitors", `${monitors.length} monitor(s)`],
                    ["Chroma", chroma],
                    ["Detector URL", detectorInferenceUrl || "(none)"],
                    [
                      "Template",
                      tplDone ? `${provisionTarget} ✓` : "not provisioned (add one later)",
                    ],
                  ] as const
                ).map(([k, v]) => (
                  <div key={k} className="flex justify-between gap-3 px-3 py-2">
                    <dt className="text-slate-500">{k}</dt>
                    <dd className="text-right font-medium text-slate-800">{v}</dd>
                  </div>
                ))}
              </dl>
            </div>
          ) : null}
        </div>

        {/* Footer: Back / Next / Finish. */}
        <div className="flex shrink-0 items-center justify-between gap-2 border-t border-slate-100 bg-white px-6 py-3">
          <button
            type="button"
            onClick={back}
            disabled={step === 0 || saving}
            className="rounded border border-slate-300 px-3 py-1.5 text-sm text-slate-600 hover:bg-slate-50 disabled:opacity-40"
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
