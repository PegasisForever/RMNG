import { useEffect, useState } from "react";

import type { AppConfigRedacted } from "~/lib/wire/AppConfigRedacted";
import type { ChromaMode } from "~/lib/wire/ChromaMode";
import type { ConfigPutResponse } from "~/lib/wire/ConfigPutResponse";
import type { ImageInfo } from "~/lib/wire/ImageInfo";
import { ImagesSection } from "~/components/ImagesSection";
import { MonitorsEditor, type Mon } from "~/components/MonitorsEditor";

const input =
  "w-full rounded border border-slate-300 px-2 py-1 text-sm focus:border-slate-400 focus:outline-none";

/** When a changed setting takes effect. Placed on section headers / fields to set
 *  expectations: `immediate` applies on save, `restart` needs a control-server
 *  restart (ports, staticDir, chroma, docker socket), `one-time` is baked in at
 *  first-run setup and can't change afterwards (subnet; dataDir/cloneSocket are
 *  fixed by the container image). */
function EffectBadge({ effect }: { effect: "immediate" | "restart" | "one-time" }) {
  const style =
    effect === "immediate"
      ? "bg-emerald-100 text-emerald-700"
      : effect === "restart"
        ? "bg-amber-100 text-amber-700"
        : "bg-slate-100 text-slate-500";
  return (
    <span className={`shrink-0 rounded px-1.5 py-0.5 text-[10px] font-semibold ${style}`}>{effect}</span>
  );
}

function Section({
  title,
  hint,
  effect,
  children,
}: {
  title: string;
  hint?: string;
  effect?: "immediate" | "restart" | "one-time";
  children: React.ReactNode;
}) {
  return (
    <section className="border-t border-slate-100 pt-4">
      <div className="flex items-center gap-2">
        <h3 className="text-sm font-semibold text-slate-800">{title}</h3>
        {effect ? <EffectBadge effect={effect} /> : null}
      </div>
      {hint ? <p className="mb-2 mt-0.5 text-xs text-slate-400">{hint}</p> : <div className="mb-2" />}
      {children}
    </section>
  );
}

function Field({ label, children }: { label: string; children: React.ReactNode }) {
  return (
    <label className="block">
      <span className="mb-0.5 block text-xs font-medium text-slate-500">{label}</span>
      {children}
    </label>
  );
}

/** A write-only secret: blank input keeps the stored value; a "set"/"unset" badge
 *  reflects what the server currently holds. */
function Secret({
  label,
  set,
  value,
  onChange,
}: {
  label: string;
  set: boolean;
  value: string;
  onChange: (v: string) => void;
}) {
  return (
    <Field label={label}>
      <div className="flex items-center gap-2">
        <input
          type="password"
          value={value}
          placeholder={set ? "•••••••• (set — leave blank to keep)" : "not set"}
          onChange={(e) => onChange(e.target.value)}
          className={input}
        />
        <span
          className={`shrink-0 rounded px-1.5 py-0.5 text-[10px] font-semibold ${
            set ? "bg-emerald-100 text-emerald-700" : "bg-slate-100 text-slate-400"
          }`}
        >
          {set ? "set" : "unset"}
        </span>
      </div>
    </Field>
  );
}

export interface SettingsPanelProps {
  /** Emails of the imported Claude accounts (from live state) — the pool a group can draw from. */
  accountEmails: string[];
  onClose: () => void;
  // --- injected server calls (no API logic lives in this component, so it's
  //     renderable in isolation — e.g. Storybook — with mocked data) ---
  /** Read the current redacted config (secrets shown as set/unset booleans). */
  getConfig: () => Promise<AppConfigRedacted>;
  /** Persist a partial config patch; returns the merged config + a restart-required flag. */
  putConfig: (patch: unknown) => Promise<ConfigPutResponse & { networkWarning?: string }>;
  /** Validate a setting (e.g. `"docker"` — re-runs the Docker self-setup probe). */
  testConfig: (what: string) => Promise<{ ok: boolean; message: string }>;
  /** Apply the saved monitor layout to all running clones. */
  applyMonitors: () => Promise<{ ok: boolean; applied: string[]; errors: string[] }>;
  // --- clone-source images (moved here from the sidebar) ---
  images: ImageInfo[];
  imagesLoading: boolean;
  /** True while a template-pull op is running (disables the pull action). */
  pullBusy: boolean;
  onPullTemplate: (name: string, reference: string) => void;
  onDeleteImage: (reference: string) => void;
}

export function SettingsPanel({
  accountEmails,
  onClose,
  getConfig,
  putConfig,
  testConfig,
  applyMonitors,
  images,
  imagesLoading,
  pullBusy,
  onPullTemplate,
  onDeleteImage,
}: SettingsPanelProps) {
  const [cfg, setCfg] = useState<AppConfigRedacted | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [saving, setSaving] = useState(false);
  const [saved, setSaved] = useState(false);
  const [advanced, setAdvanced] = useState(false);
  const [testMsg, setTestMsg] = useState<string | null>(null);
  const [applying, setApplying] = useState(false);
  const [applyMsg, setApplyMsg] = useState<string | null>(null);
  // True after a save that touched a restart-required setting (ports / cloneSocket /
  // staticDir / chroma) — surfaces a persistent banner until a later save clears it.
  const [restartRequired, setRestartRequired] = useState(false);

  // Editable form state. Secrets (preset linearKey) start blank = "unchanged".
  const [monitors, setMonitors] = useState<Mon[]>([]);
  // Presets: labels edited as a comma-separated string; linearKey is write-only
  // (blank = keep stored), keySet mirrors whether the server holds one.
  const [presets, setPresets] = useState<
    {
      name: string;
      labels: string;
      linearKey: string;
      keySet: boolean;
      vars: { key: string; value: string }[];
    }[]
  >([]);
  const [claudeGroups, setClaudeGroups] = useState<{ name: string; accounts: string[] }[]>([]);
  const [hostnamePrefix, setHostnamePrefix] = useState("");
  const [templateReference, setTemplateReference] = useState("");
  const [subnet, setSubnet] = useState("");
  const [cloneCpus, setCloneCpus] = useState(16);
  const [cloneMemoryMb, setCloneMemoryMb] = useState(32768);
  const [claude, setClaude] = useState({
    pollSecs: 600,
    pinnedEmail: "",
    autoSwapOnExhaustion: false,
  });
  const [listen, setListen] = useState({ web: 9000, video: 9001, cloneMcp: 9002, globalMcp: 9003, daemonMcp: 9004 });
  const [agentPort, setAgentPort] = useState(4096);
  const [dataDir, setDataDir] = useState("");
  const [staticDir, setStaticDir] = useState("");
  const [cloneSocket, setCloneSocket] = useState("");
  const [chroma, setChroma] = useState<ChromaMode>("yuv420");
  const [detectorInferenceUrl, setDetectorInferenceUrl] = useState("");

  function load(c: AppConfigRedacted) {
    setCfg(c);
    setMonitors(
      c.monitors.length
        ? c.monitors.map((m) => ({ ...m }))
        : [{ width: 1920, height: 1080, x: 0, y: 0, primary: true }],
    );
    setHostnamePrefix(c.docker.hostnamePrefix);
    setTemplateReference(c.docker.templateReference);
    setSubnet(c.docker.subnet);
    setCloneCpus(c.docker.cloneCpus);
    setCloneMemoryMb(c.docker.cloneMemoryMb);
    setClaude({
      ...c.claude,
      pollSecs: Number(c.claude.pollSecs),
      pinnedEmail: c.claude.pinnedEmail ?? "",
    });
    setListen({ ...c.listen });
    setAgentPort(c.agentPort);
    setDataDir(c.dataDir);
    setStaticDir(c.staticDir);
    setCloneSocket(c.cloneSocket);
    setChroma(c.chroma);
    setDetectorInferenceUrl(c.detectorInferenceUrl);
    setPresets(
      c.presets.map((p) => ({
        name: p.name,
        labels: p.labels.join(", "),
        linearKey: "",
        keySet: p.linearKeySet,
        vars: p.vars.map((v) => ({ ...v })),
      })),
    );
    setClaudeGroups(c.cloneGroups.map((g) => ({ name: g.name, accounts: [...g.accounts] })));
  }

  useEffect(() => {
    getConfig().then(load).catch((e: Error) => setError(e.message));
    const onKey = (e: KeyboardEvent) => e.key === "Escape" && onClose();
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [onClose]);

  // Preset editors.
  const addPreset = () =>
    setPresets((ps) => [
      ...ps,
      { name: "", labels: "", linearKey: "", keySet: false, vars: [{ key: "", value: "" }] },
    ]);
  const rmPreset = (i: number) => setPresets((ps) => ps.filter((_, j) => j !== i));
  const setPresetField = (i: number, field: "name" | "labels" | "linearKey", v: string) =>
    setPresets((ps) => ps.map((p, j) => (j === i ? { ...p, [field]: v } : p)));
  const addVar = (i: number) =>
    setPresets((ps) => ps.map((p, j) => (j === i ? { ...p, vars: [...p.vars, { key: "", value: "" }] } : p)));
  const rmVar = (i: number, k: number) =>
    setPresets((ps) => ps.map((p, j) => (j === i ? { ...p, vars: p.vars.filter((_, m) => m !== k) } : p)));
  const setVar = (i: number, k: number, field: "key" | "value", v: string) =>
    setPresets((ps) =>
      ps.map((p, j) =>
        j === i ? { ...p, vars: p.vars.map((vv, m) => (m === k ? { ...vv, [field]: v } : vv)) } : p,
      ),
    );

  // Claude group editors (a group = a name + a set of member account emails).
  const addGroup = () => setClaudeGroups((gs) => [...gs, { name: "", accounts: [] }]);
  const rmGroup = (i: number) => setClaudeGroups((gs) => gs.filter((_, j) => j !== i));
  const setGroupName = (i: number, name: string) =>
    setClaudeGroups((gs) => gs.map((g, j) => (j === i ? { ...g, name } : g)));
  const toggleGroupAccount = (i: number, email: string) =>
    setClaudeGroups((gs) =>
      gs.map((g, j) =>
        j === i
          ? {
              ...g,
              accounts: g.accounts.includes(email)
                ? g.accounts.filter((e) => e !== email)
                : [...g.accounts, email],
            }
          : g,
      ),
    );

  async function save() {
    setSaving(true);
    setError(null);
    setSaved(false);
    try {
      const patch = {
        monitors: monitors.map((m) => ({
          width: Math.max(1, m.width),
          height: Math.max(1, m.height),
          x: Math.max(0, m.x),
          y: Math.max(0, m.y),
          primary: m.primary,
        })),
        // subnet is one-time: only sent (non-empty) pre-setup; after setup it's read-only,
        // and the server rejects a change anyway. Blank = unchanged.
        docker: {
          hostnamePrefix,
          templateReference,
          cloneCpus,
          cloneMemoryMb,
          ...(cfg?.setupComplete ? {} : { subnet }),
        },
        claude: { ...claude, pinnedEmail: claude.pinnedEmail || null },
        listen,
        agentPort,
        dataDir,
        staticDir,
        cloneSocket,
        chroma,
        detectorInferenceUrl,
        presets: presets
          .filter((p) => p.name.trim())
          .map((p) => ({
            name: p.name.trim(),
            labels: p.labels.split(",").map((s) => s.trim()).filter(Boolean),
            linearKey: p.linearKey, // "" = keep the stored key
            vars: p.vars.filter((v) => v.key.trim()).map((v) => ({ key: v.key.trim(), value: v.value })),
          })),
        cloneGroups: claudeGroups
          .filter((g) => g.name.trim())
          .map((g) => ({ name: g.name.trim(), accounts: [...new Set(g.accounts)] })),
      };
      const res = await putConfig(patch);
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

  // Persist the current monitor edits, then apply the layout to all running clones
  // (rewrites RMNG_MONITORS + restarts each clone's GNOME session + daemon).
  async function applyNow() {
    if (
      !confirm(
        "Apply this monitor layout to all running clones now?\n\nThis restarts each clone's headless GNOME session + daemon — open apps on the clone will close.",
      )
    )
      return;
    setApplying(true);
    setApplyMsg("applying…");
    try {
      await putConfig({
        monitors: monitors.map((m) => ({
          width: Math.max(1, m.width),
          height: Math.max(1, m.height),
          x: Math.max(0, m.x),
          y: Math.max(0, m.y),
          primary: m.primary,
        })),
      });
      const r = await applyMonitors();
      setApplyMsg(
        `✓ applied to ${r.applied.length} clone(s)${r.errors.length ? ` · ${r.errors.length} error(s): ${r.errors.join("; ")}` : ""}`,
      );
    } catch (e) {
      setApplyMsg(`✗ ${(e as Error).message}`);
    } finally {
      setApplying(false);
    }
  }

  return (
    <div
      className="fixed inset-0 z-50 flex items-center justify-center bg-slate-900/30 p-4"
      onClick={onClose}
    >
      <div
        className="max-h-[90vh] w-full max-w-2xl overflow-y-auto rounded-xl border border-slate-200 bg-white p-5 shadow-xl"
        onClick={(e) => e.stopPropagation()}
      >
        <div className="mb-2 flex items-center justify-between">
          <h2 className="text-base font-semibold text-slate-900">Settings</h2>
          <button
            type="button"
            onClick={onClose}
            className="rounded p-1 text-slate-400 hover:bg-slate-100 hover:text-slate-600"
            aria-label="Close"
          >
            ✕
          </button>
        </div>

        {error ? (
          <div className="mb-3 rounded border border-red-200 bg-red-50 px-3 py-2 text-xs text-red-700">
            {error}
          </div>
        ) : null}

        {restartRequired ? (
          <div className="mb-3 rounded border border-amber-300 bg-amber-50 px-3 py-2 text-xs text-amber-800">
            Run <code>docker restart rmng</code> to apply the changed port/socket/video settings.
          </div>
        ) : null}

        {!cfg ? (
          <p className="py-8 text-center text-sm text-slate-400">Loading…</p>
        ) : (
          <div className="space-y-4">
            {/* Monitors — size, position + primary per monitor, with a live preview. */}
            <Section
              title="Monitors"
              effect="immediate"
              hint="Set each monitor's size, position (x,y in the unified desktop) and which is primary. Applies to newly provisioned clones; restart an existing clone's daemon to pick up changes."
            >
              <MonitorsEditor
                monitors={monitors}
                onChange={setMonitors}
                onApply={applyNow}
                applying={applying}
                applyMsg={applyMsg}
              />
            </Section>

            {/* Presets — Linear identity (key + auto-select labels) + env vars, picked
                (or label-matched) at clone time. */}
            <Section
              title="Presets"
              effect="immediate"
              hint="A preset = Linear API key + the ticket labels that auto-select it + env vars, written to the clone's session env at creation. The key is also injected as LINEAR_API_KEY (auths the clone's `linear` MCP). Cloning from a ticket auto-picks by label; other clones require an explicit pick."
            >
              <div className="space-y-3">
                {presets.length === 0 ? <p className="text-xs text-slate-400">No presets.</p> : null}
                {presets.map((p, i) => (
                  <div key={i} className="rounded border border-slate-200 p-3">
                    <div className="flex items-center gap-2">
                      <input
                        value={p.name}
                        onChange={(e) => setPresetField(i, "name", e.target.value)}
                        placeholder="preset name"
                        className={input}
                      />
                      <button
                        type="button"
                        onClick={() => rmPreset(i)}
                        className="shrink-0 rounded px-2 py-1 text-xs text-slate-500 hover:bg-slate-100"
                      >
                        Remove
                      </button>
                    </div>
                    <div className="mt-2">
                      <input
                        value={p.labels}
                        onChange={(e) => setPresetField(i, "labels", e.target.value)}
                        placeholder="Linear ticket labels, comma-separated (auto-selects this preset)"
                        spellCheck={false}
                        className="w-full rounded border border-slate-300 px-2 py-1 text-xs focus:border-slate-400 focus:outline-none"
                      />
                    </div>
                    <div className="mt-1.5">
                      <Secret
                        label="Linear API key"
                        set={p.keySet}
                        value={p.linearKey}
                        onChange={(v) => setPresetField(i, "linearKey", v)}
                      />
                    </div>
                    <div className="mt-2 space-y-1.5">
                      {p.vars.map((v, k) => (
                        <div key={k} className="flex items-center gap-2">
                          <input
                            value={v.key}
                            onChange={(e) => setVar(i, k, "key", e.target.value)}
                            placeholder="KEY"
                            spellCheck={false}
                            className="w-2/5 rounded border border-slate-300 px-2 py-1 font-mono text-xs focus:border-slate-400 focus:outline-none"
                          />
                          <span className="text-slate-400">=</span>
                          <input
                            value={v.value}
                            onChange={(e) => setVar(i, k, "value", e.target.value)}
                            placeholder="value"
                            spellCheck={false}
                            className="flex-1 rounded border border-slate-300 px-2 py-1 font-mono text-xs focus:border-slate-400 focus:outline-none"
                          />
                          <button
                            type="button"
                            onClick={() => rmVar(i, k)}
                            title="remove variable"
                            className="shrink-0 rounded px-2 py-1 text-xs text-slate-400 hover:bg-slate-100"
                          >
                            ×
                          </button>
                        </div>
                      ))}
                    </div>
                    <button
                      type="button"
                      onClick={() => addVar(i)}
                      className="mt-2 rounded border border-slate-300 px-2 py-1 text-xs text-slate-600 hover:bg-slate-50"
                    >
                      + Add variable
                    </button>
                  </div>
                ))}
                <button
                  type="button"
                  onClick={addPreset}
                  className="rounded border border-slate-300 px-2 py-1 text-xs text-slate-600 hover:bg-slate-50"
                >
                  + Add preset
                </button>
              </div>
            </Section>

            {/* Docker / Clones. */}
            <Section title="Docker / Clones">
              <div className="space-y-3">
                <div className="flex items-center gap-2">
                  <button
                    type="button"
                    onClick={runTest}
                    className="rounded border border-slate-300 px-2.5 py-1.5 text-xs text-slate-600 hover:bg-slate-50"
                  >
                    Test Docker
                  </button>
                  {testMsg ? <p className="text-xs text-slate-500">{testMsg}</p> : null}
                </div>
                <div>
                  <div className="flex items-center gap-2">
                    <span className="text-xs font-medium text-slate-500">Clone hostname prefix</span>
                    <EffectBadge effect="immediate" />
                  </div>
                  <input
                    value={hostnamePrefix}
                    onChange={(e) => setHostnamePrefix(e.target.value)}
                    placeholder="pega-"
                    className={`mt-0.5 ${input}`}
                  />
                  <p className="mt-0.5 text-xs text-slate-400">
                    Prepended to derived clone hostnames — e.g. <code>{hostnamePrefix || "pega-"}</code>dev-123 /{" "}
                    <code>{hostnamePrefix || "pega-"}</code>my-task. Lowercased + sanitized to a DNS label; blank keeps
                    the current value.
                  </p>
                </div>
                <div>
                  <div className="flex items-center gap-2">
                    <span className="text-xs font-medium text-slate-500">Template reference</span>
                    <EffectBadge effect="immediate" />
                  </div>
                  <input
                    value={templateReference}
                    onChange={(e) => setTemplateReference(e.target.value)}
                    placeholder="pegasis0/rmng-template:latest"
                    spellCheck={false}
                    className={`mt-0.5 ${input}`}
                  />
                  <p className="mt-0.5 text-xs text-slate-400">
                    Registry <code>repo:tag</code> the wizard/Images panel pulls the clone template
                    from, then retags locally to <code>rmng/template:&lt;name&gt;</code>. Read fresh on
                    each pull.
                  </p>
                </div>
                {/* Subnet is baked into the rmng bridge + every clone's static IP at first-run
                    setup, so it's one-time: editable only during first-run setup. */}
                <div>
                  <div className="flex items-center gap-2">
                    <span className="text-xs font-medium text-slate-500">Clone network subnet</span>
                    <EffectBadge effect="one-time" />
                  </div>
                  <input
                    value={subnet}
                    onChange={(e) => setSubnet(e.target.value)}
                    disabled={cfg.setupComplete}
                    placeholder="10.99.0.0/24"
                    spellCheck={false}
                    className={`mt-0.5 ${input} disabled:bg-slate-50 disabled:text-slate-400`}
                  />
                  <p className="mt-0.5 text-xs text-slate-400">
                    {cfg.setupComplete
                      ? "Set during first-run setup — baked into the rmng network + clone IPs, cannot be changed."
                      : "IPv4 CIDR (/16–/24) for the rmng bridge — .1 gateway, .2 control-server, .10+ clone pool."}
                  </p>
                </div>
                <div className="grid grid-cols-2 gap-3">
                  <div>
                    <div className="flex items-center gap-2">
                      <span className="text-xs font-medium text-slate-500">CPU limit per clone (cores)</span>
                      <EffectBadge effect="immediate" />
                    </div>
                    <input
                      type="number"
                      min={1}
                      value={cloneCpus}
                      onChange={(e) => setCloneCpus(Number(e.target.value) || 0)}
                      className={`mt-0.5 ${input}`}
                    />
                  </div>
                  <div>
                    <div className="flex items-center gap-2">
                      <span className="text-xs font-medium text-slate-500">Memory limit per clone (MB)</span>
                      <EffectBadge effect="immediate" />
                    </div>
                    <input
                      type="number"
                      min={1024}
                      value={cloneMemoryMb}
                      onChange={(e) => setCloneMemoryMb(Number(e.target.value) || 0)}
                      className={`mt-0.5 ${input}`}
                    />
                  </div>
                </div>
                <p className="text-xs text-slate-400">
                  Limits apply to newly created clones (existing clones keep the limits they were
                  created with).
                </p>
              </div>
            </Section>

            {/* Images — clone-source templates (pull from a registry / delete). Moved
                here from the sidebar. Prefills the pull prompt from the Template
                reference field above. */}
            <Section
              title="Images"
              effect="immediate"
              hint="Clone-source images (rmng.image=1). Pull the template from a registry (retagged locally as rmng/template:<name>) or delete an unused one; a live clone running on an image blocks its delete."
            >
              <ImagesSection
                images={images}
                loading={imagesLoading}
                pullBusy={pullBusy}
                templateRef={templateReference}
                onPull={onPullTemplate}
                onDelete={onDeleteImage}
              />
            </Section>

            {/* Claude. */}
            <Section title="Claude">
              <div className="grid grid-cols-2 gap-3">
                <Field label="Usage poll interval (s)">
                  <input
                    type="number"
                    value={claude.pollSecs}
                    onChange={(e) => setClaude({ ...claude, pollSecs: Number(e.target.value) || 0 })}
                    className={input}
                  />
                </Field>
                <Field label="Pinned account email">
                  <input
                    value={claude.pinnedEmail}
                    onChange={(e) => setClaude({ ...claude, pinnedEmail: e.target.value })}
                    className={input}
                  />
                </Field>
                <label className="col-span-2 flex items-center gap-2 text-sm text-slate-600">
                  <input
                    type="checkbox"
                    checked={claude.autoSwapOnExhaustion}
                    onChange={(e) => setClaude({ ...claude, autoSwapOnExhaustion: e.target.checked })}
                  />
                  Auto-swap a clone to another account when usage is exhausted
                </label>
              </div>
            </Section>

            {/* Claude groups (named account pools; sticky — a clone moves only when its account exhausts). */}
            <Section
              title="Claude groups"
              hint="A pool of accounts. A clone bound to a group keeps its account (preserving its prompt cache) until that account passes 90% 5h usage, then moves to the least-used member."
            >
              <div className="space-y-3">
                {claudeGroups.length === 0 ? (
                  <p className="text-xs text-slate-400">No groups.</p>
                ) : null}
                {claudeGroups.map((g, i) => (
                  <div key={i} className="rounded border border-slate-200 p-3">
                    <div className="flex items-center gap-2">
                      <input
                        value={g.name}
                        onChange={(e) => setGroupName(i, e.target.value)}
                        placeholder="group name"
                        className={input}
                      />
                      <button
                        type="button"
                        onClick={() => rmGroup(i)}
                        className="shrink-0 rounded px-2 py-1 text-xs text-slate-500 hover:bg-slate-100"
                      >
                        Remove
                      </button>
                    </div>
                    {accountEmails.length === 0 ? (
                      <p className="mt-2 text-xs text-slate-400">
                        Import some accounts first to add them to a group.
                      </p>
                    ) : (
                      <div className="mt-2 flex flex-wrap gap-x-4 gap-y-1.5">
                        {accountEmails.map((email) => (
                          <label key={email} className="flex items-center gap-1.5 text-xs text-slate-600">
                            <input
                              type="checkbox"
                              checked={g.accounts.includes(email)}
                              onChange={() => toggleGroupAccount(i, email)}
                            />
                            {email}
                          </label>
                        ))}
                      </div>
                    )}
                  </div>
                ))}
                <button
                  type="button"
                  onClick={addGroup}
                  className="rounded border border-slate-300 px-2 py-1 text-xs text-slate-600 hover:bg-slate-50"
                >
                  + Add group
                </button>
              </div>
            </Section>

            {/* Video — chroma subsampling for the viewer stream (server-wide, chosen at launch). */}
            <Section
              title="Video"
              effect="restart"
              hint="Chroma subsampling for the port-1 viewer stream, server-wide. 4:4:4 recovers full chroma via AVC444 packing (a double-height stream reassembled on the GPU); keep monitors ≤1440p in that mode."
            >
              <Field label="Chroma mode">
                <select value={chroma} onChange={(e) => setChroma(e.target.value as ChromaMode)} className={input}>
                  <option value="yuv420">4:2:0 (default)</option>
                  <option value="yuv444">4:4:4 (AVC444, ≤1440p/monitor)</option>
                </select>
              </Field>
            </Section>

            {/* Detector — the window/element detector inference endpoint. */}
            <Section
              title="Detector"
              effect="immediate"
              hint="Base URL of the detector inference service the fleet queries for on-screen element detection."
            >
              <Field label="Inference URL">
                <input
                  value={detectorInferenceUrl}
                  onChange={(e) => setDetectorInferenceUrl(e.target.value)}
                  placeholder="http://…"
                  spellCheck={false}
                  className={input}
                />
              </Field>
            </Section>

            {/* Advanced (ports + dirs; need a full control-server restart). */}
            <Section title="Advanced">
              <button
                type="button"
                onClick={() => setAdvanced((a) => !a)}
                className="text-xs font-medium text-slate-500 hover:text-slate-700"
              >
                {advanced ? "▾ Hide" : "▸ Show"} ports + directories (restart the control-server to apply)
              </button>
              {advanced ? (
                <div className="mt-2 grid grid-cols-2 gap-3">
                  {/* web/video/cloneMcp/globalMcp are wired once at startup → restart-required.
                      daemonMcp applies live, but must match what clones bake in. */}
                  {(["web", "video", "cloneMcp", "globalMcp"] as const).map((k) => (
                    <div key={k}>
                      <div className="flex items-center gap-2">
                        <span className="text-xs font-medium text-slate-500">Port: {k}</span>
                        <EffectBadge effect="restart" />
                      </div>
                      <input
                        type="number"
                        value={listen[k]}
                        onChange={(e) => setListen({ ...listen, [k]: Number(e.target.value) || 0 })}
                        className={`mt-0.5 ${input}`}
                      />
                    </div>
                  ))}
                  <div>
                    <div className="flex items-center gap-2">
                      <span className="text-xs font-medium text-slate-500">Port: daemonMcp</span>
                      <EffectBadge effect="immediate" />
                    </div>
                    <input
                      type="number"
                      value={listen.daemonMcp}
                      onChange={(e) => setListen({ ...listen, daemonMcp: Number(e.target.value) || 0 })}
                      className={`mt-0.5 ${input}`}
                    />
                    <p className="mt-0.5 text-xs text-slate-400">must match what clones bake in: 9004</p>
                  </div>
                  <div>
                    <div className="flex items-center gap-2">
                      <span className="text-xs font-medium text-slate-500">Agent-wrapper port</span>
                      <EffectBadge effect="immediate" />
                    </div>
                    <input
                      type="number"
                      value={agentPort}
                      onChange={(e) => setAgentPort(Number(e.target.value) || 0)}
                      className={`mt-0.5 ${input}`}
                    />
                    <p className="mt-0.5 text-xs text-slate-400">must match what clones bake in: 4096</p>
                  </div>
                  {/* Data dir is the control-server's WORKDIR inside its container: fixed at
                      /data (the mounted volume). Shown read-only for reference. */}
                  <div>
                    <div className="flex items-center gap-2">
                      <span className="text-xs font-medium text-slate-500">Data dir</span>
                      <EffectBadge effect="one-time" />
                    </div>
                    <input
                      value={dataDir}
                      readOnly
                      disabled
                      className={`mt-0.5 ${input} disabled:bg-slate-50 disabled:text-slate-400`}
                    />
                    <p className="mt-0.5 text-xs text-slate-400">
                      fixed at <code>/data</code> in the container (the mounted volume)
                    </p>
                  </div>
                  {/* The shared unix socket clone-daemons connect to. Fixed by the container's
                      mounted sock volume; shown read-only. */}
                  <div>
                    <div className="flex items-center gap-2">
                      <span className="text-xs font-medium text-slate-500">Clone socket</span>
                      <EffectBadge effect="one-time" />
                    </div>
                    <input
                      value={cloneSocket}
                      readOnly
                      disabled
                      placeholder="/srv/rmng-sock/clones.sock"
                      spellCheck={false}
                      className={`mt-0.5 ${input} disabled:bg-slate-50 disabled:text-slate-400`}
                    />
                    <p className="mt-0.5 text-xs text-slate-400">
                      fixed by the container's shared sock volume
                    </p>
                  </div>
                  <div className="col-span-2">
                    <div className="flex items-center gap-2">
                      <span className="text-xs font-medium text-slate-500">Static (frontend) dir</span>
                      <EffectBadge effect="restart" />
                    </div>
                    <input
                      value={staticDir}
                      onChange={(e) => setStaticDir(e.target.value)}
                      spellCheck={false}
                      className={`mt-0.5 ${input}`}
                    />
                    <p className="mt-0.5 text-xs text-slate-400">empty = built-in (embedded) frontend</p>
                  </div>
                </div>
              ) : null}
            </Section>

            {/* Footer. */}
            <div className="sticky bottom-0 -mx-5 -mb-5 flex items-center justify-end gap-2 border-t border-slate-100 bg-white px-5 py-3">
              {saved ? <span className="mr-auto text-xs font-medium text-emerald-600">Saved ✓</span> : null}
              <button
                type="button"
                onClick={onClose}
                className="rounded border border-slate-300 px-3 py-1.5 text-sm text-slate-600 hover:bg-slate-50"
              >
                Close
              </button>
              <button
                type="button"
                onClick={save}
                disabled={saving}
                className="rounded bg-slate-900 px-3 py-1.5 text-sm font-medium text-white hover:bg-slate-700 disabled:opacity-50"
              >
                {saving ? "Saving…" : "Save"}
              </button>
            </div>
          </div>
        )}
      </div>
    </div>
  );
}
