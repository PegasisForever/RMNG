import { ChevronDown, ChevronRight, X } from "lucide-react";
import { useEffect, useState } from "react";

import type { AppConfigRedacted } from "~/lib/wire/AppConfigRedacted";
import type { ChromaMode } from "~/lib/wire/ChromaMode";
import type { ConfigPutResponse } from "~/lib/wire/ConfigPutResponse";
import type { ImageInfo } from "~/lib/wire/ImageInfo";
import type { UpdateStatus } from "~/lib/wire/UpdateStatus";
import { ImagesSection } from "~/components/ImagesSection";
import { MonitorsEditor, type Mon } from "~/components/MonitorsEditor";

const input =
  "w-full rounded border border-slate-300 dark:border-slate-600 px-2 py-1 text-sm focus:border-slate-400 dark:focus:border-slate-500 focus:outline-none dark:bg-slate-800 dark:text-slate-100";

/** When a changed setting takes effect. Placed on section headers / fields to set
 *  expectations: `immediate` applies on save, `restart` needs a control-server
 *  restart (ports, staticDir, chroma, docker socket), `one-time` is baked in at
 *  first-run setup and can't change afterwards (subnet; dataDir/cloneSocket are
 *  fixed by the container image). */
function EffectBadge({ effect }: { effect: "immediate" | "restart" | "one-time" }) {
  const style =
    effect === "immediate"
      ? "bg-emerald-100 dark:bg-emerald-900/40 text-emerald-700 dark:text-emerald-400"
      : effect === "restart"
        ? "bg-amber-100 dark:bg-amber-900/40 text-amber-700 dark:text-amber-400"
        : "bg-slate-100 dark:bg-slate-800 text-slate-500 dark:text-slate-400";
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
    <section className="border-t border-slate-100 dark:border-slate-800 pt-4">
      <div className="flex items-center gap-2">
        <h3 className="text-sm font-semibold text-slate-800 dark:text-slate-100">{title}</h3>
        {effect ? <EffectBadge effect={effect} /> : null}
      </div>
      {hint ? <p className="mb-2 mt-0.5 text-xs text-slate-400 dark:text-slate-500">{hint}</p> : <div className="mb-2" />}
      {children}
    </section>
  );
}

function Field({ label, children }: { label: string; children: React.ReactNode }) {
  return (
    <label className="block">
      <span className="mb-0.5 block text-xs font-medium text-slate-500 dark:text-slate-400">{label}</span>
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
            set ? "bg-emerald-100 dark:bg-emerald-900/40 text-emerald-700 dark:text-emerald-400" : "bg-slate-100 dark:bg-slate-800 text-slate-400 dark:text-slate-500"
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
  /** Emails of the imported Codex accounts (from live state) — the pool a Codex group can draw from. */
  codexAccountEmails: string[];
  onClose: () => void;
  // --- injected server calls (no API logic lives in this component, so it's
  //     renderable in isolation — e.g. Storybook — with mocked data) ---
  /** Read the current redacted config (secrets shown as set/unset booleans). */
  getConfig: () => Promise<AppConfigRedacted>;
  /** Persist a partial config patch; returns the merged config + a restart-required flag. */
  putConfig: (patch: unknown) => Promise<ConfigPutResponse & { networkWarning?: string }>;
  /** Validate a setting (e.g. `"docker"` — re-runs the Docker self-setup probe). */
  testConfig: (what: string) => Promise<{ ok: boolean; message: string }>;
  /** Read the control-server's own version + update-available status. */
  getUpdateStatus: () => Promise<UpdateStatus>;
  /** Pull the latest control-server image and swap the running container onto it. */
  updateServer: () => Promise<unknown>;
  /** Restart the control-server container in place (applies changed startup settings). */
  restartServer: () => Promise<{ ok: boolean }>;
  // --- clone-source images (moved here from the sidebar) ---
  images: ImageInfo[];
  imagesLoading: boolean;
  /** True while a template-pull op is running (disables the pull action). */
  pullBusy: boolean;
  onPullTemplate: (reference: string) => void;
  onDeleteImage: (reference: string) => void;
}

export function SettingsPanel({
  accountEmails,
  codexAccountEmails,
  onClose,
  getConfig,
  putConfig,
  testConfig,
  getUpdateStatus,
  updateServer,
  restartServer,
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
  // True after a save that touched a restart-required setting (ports / cloneSocket /
  // staticDir / chroma) — surfaces a persistent banner until a later save clears it.
  const [restartRequired, setRestartRequired] = useState(false);
  // Control-server's own version + update-available status (fetched on open; re-checked
  // on demand via the "Check for updates" button).
  const [serverStatus, setServerStatus] = useState<UpdateStatus | null>(null);
  const [serverMsg, setServerMsg] = useState<string | null>(null);

  useEffect(() => {
    getUpdateStatus().then(setServerStatus).catch((e) => setServerMsg(`✗ ${(e as Error).message}`));
  }, [getUpdateStatus]);

  async function checkUpdate() {
    setServerMsg("checking…");
    try {
      const s = await getUpdateStatus();
      setServerStatus(s);
      setServerMsg(s.error ? `⚠ ${s.error}` : s.available ? "update available" : "up to date");
    } catch (e) {
      setServerMsg(`✗ ${(e as Error).message}`);
    }
  }

  async function doUpdate() {
    if (!confirm("Update the control-server now?\n\nIt will pull the latest image and restart itself. The UI will briefly disconnect and reconnect; running clones are unaffected.")) return;
    setServerMsg("updating… the server will restart shortly");
    try {
      await updateServer();
    } catch (e) {
      setServerMsg(`✗ ${(e as Error).message}`);
    }
  }

  async function doRestart() {
    if (!confirm("Restart the control-server now to apply the changed settings?\n\nThe UI will briefly disconnect and reconnect; running clones are unaffected.")) return;
    setServerMsg("restarting… reconnecting shortly");
    try {
      await restartServer();
    } catch (e) {
      setServerMsg(`✗ ${(e as Error).message}`);
    }
  }

  // Editable form state. Secrets (preset linearKey) start blank = "unchanged".
  const [layoutPresets, setLayoutPresets] = useState<{ name: string; monitors: Mon[] }[]>([]);
  // Presets: labels edited as a comma-separated string; linearKey is write-only
  // (blank = keep stored), keySet mirrors whether the server holds one.
  const [presets, setPresets] = useState<
    {
      name: string;
      labels: string;
      linearKey: string;
      keySet: boolean;
      vars: { key: string; value: string }[];
      agentPlaybook: string;
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
  });
  const [codex, setCodex] = useState({
    pollSecs: 600,
    pinnedEmail: "",
    usagePolling: true,
  });
  const [codexGroups, setCodexGroups] = useState<{ name: string; accounts: string[] }[]>([]);
  const [listen, setListen] = useState({ web: 9000, video: 9001, cloneMcp: 9002, globalMcp: 9003, daemonMcp: 9004 });
  const [agentPort, setAgentPort] = useState(4096);
  const [dataDir, setDataDir] = useState("");
  const [staticDir, setStaticDir] = useState("");
  const [cloneSocket, setCloneSocket] = useState("");
  const [chroma, setChroma] = useState<ChromaMode>("yuv420");
  const [detectorInferenceUrl, setDetectorInferenceUrl] = useState("");
  const [agentPlaybook, setAgentPlaybook] = useState("");

  function load(c: AppConfigRedacted) {
    setCfg(c);
    setLayoutPresets(
      c.layoutPresets.length
        ? c.layoutPresets.map((p) => ({ name: p.name, monitors: p.monitors.map((m) => ({ ...m })) }))
        : [{ name: "Default", monitors: [{ width: 1920, height: 1080, x: 0, y: 0, primary: true }] }],
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
    setCodex({
      ...c.codex,
      pollSecs: Number(c.codex.pollSecs),
      pinnedEmail: c.codex.pinnedEmail ?? "",
    });
    setCodexGroups(c.codexGroups.map((g) => ({ name: g.name, accounts: [...g.accounts] })));
    setListen({ ...c.listen });
    setAgentPort(c.agentPort);
    setDataDir(c.dataDir);
    setStaticDir(c.staticDir);
    setCloneSocket(c.cloneSocket);
    setChroma(c.chroma);
    setDetectorInferenceUrl(c.detectorInferenceUrl);
    setAgentPlaybook(c.agentPlaybook);
    setPresets(
      c.presets.map((p) => ({
        name: p.name,
        labels: p.labels.join(", "),
        linearKey: "",
        keySet: p.linearKeySet,
        vars: p.vars.map((v) => ({ ...v })),
        agentPlaybook: p.agentPlaybook,
      })),
    );
    setClaudeGroups(c.cloneGroups.map((g) => ({ name: g.name, accounts: [...g.accounts] })));
  }

  // Seed the form from the server ONCE, when the panel opens. This must NOT depend on
  // `onClose` (a fresh inline arrow on every parent render): the dashboard re-renders
  // every few seconds on each `stats` SSE frame, and re-running this would re-seed the
  // form from the server and wipe the user's in-progress edits.
  useEffect(() => {
    getConfig().then(load).catch((e: Error) => setError(e.message));
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  // Escape closes — its own effect so it can track the latest `onClose` without
  // re-triggering the config fetch above.
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => e.key === "Escape" && onClose();
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [onClose]);

  // Preset editors.
  const addPreset = () =>
    setPresets((ps) => [
      ...ps,
      { name: "", labels: "", linearKey: "", keySet: false, vars: [{ key: "", value: "" }], agentPlaybook: "" },
    ]);
  const rmPreset = (i: number) => setPresets((ps) => ps.filter((_, j) => j !== i));
  const setPresetField = (i: number, field: "name" | "labels" | "linearKey" | "agentPlaybook", v: string) =>
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

  // Layout preset editors.
  const addLayoutPreset = () =>
    setLayoutPresets((ps) => [
      ...ps,
      { name: "", monitors: [{ width: 1920, height: 1080, x: 0, y: 0, primary: true }] },
    ]);
  const rmLayoutPreset = (i: number) => setLayoutPresets((ps) => ps.filter((_, j) => j !== i));
  const setLayoutPresetName = (i: number, name: string) =>
    setLayoutPresets((ps) => ps.map((p, j) => (j === i ? { ...p, name } : p)));
  const setLayoutPresetMonitors = (i: number, mons: Mon[]) =>
    setLayoutPresets((ps) => ps.map((p, j) => (j === i ? { ...p, monitors: mons } : p)));

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

  // Codex group editors.
  const addCodexGroup = () => setCodexGroups((gs) => [...gs, { name: "", accounts: [] }]);
  const rmCodexGroup = (i: number) => setCodexGroups((gs) => gs.filter((_, j) => j !== i));
  const setCodexGroupName = (i: number, name: string) =>
    setCodexGroups((gs) => gs.map((g, j) => (j === i ? { ...g, name } : g)));
  const toggleCodexGroupAccount = (i: number, email: string) =>
    setCodexGroups((gs) =>
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
        layoutPresets: layoutPresets
          .filter((p) => p.name.trim())
          .map((p) => ({
            name: p.name.trim(),
            monitors: p.monitors.map((m) => ({
              width: Math.max(1, m.width),
              height: Math.max(1, m.height),
              x: Math.max(0, m.x),
              y: Math.max(0, m.y),
              primary: m.primary,
            })),
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
        codex: { ...codex, pinnedEmail: codex.pinnedEmail || null },
        codexGroups: codexGroups
          .filter((g) => g.name.trim())
          .map((g) => ({ name: g.name.trim(), accounts: [...new Set(g.accounts)] })),
        listen,
        agentPort,
        dataDir,
        staticDir,
        cloneSocket,
        chroma,
        detectorInferenceUrl,
        agentPlaybook,
        presets: presets
          .filter((p) => p.name.trim())
          .map((p) => ({
            name: p.name.trim(),
            labels: p.labels.split(",").map((s) => s.trim()).filter(Boolean),
            linearKey: p.linearKey, // "" = keep the stored key
            vars: p.vars.filter((v) => v.key.trim()).map((v) => ({ key: v.key.trim(), value: v.value })),
            agentPlaybook: p.agentPlaybook,
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

  return (
    <div
      className="fixed inset-0 z-50 flex items-center justify-center bg-slate-900/30 p-4"
      onClick={onClose}
    >
      <div
        className="flex max-h-[90vh] w-full max-w-2xl flex-col overflow-hidden rounded-xl border border-slate-200 dark:border-slate-700 bg-white dark:bg-slate-800 shadow-xl"
        onClick={(e) => e.stopPropagation()}
      >
        {/* Scrollable body. The footer lives outside this so it stays flush to the
            panel's bottom edge instead of floating above the scroll container's padding. */}
        <div className="min-h-0 flex-1 overflow-y-auto p-5">
        <div className="mb-2 flex items-center justify-between">
          <h2 className="text-base font-semibold text-slate-900 dark:text-slate-100">Settings</h2>
          <button
            type="button"
            onClick={onClose}
            className="rounded p-1 text-slate-400 dark:text-slate-500 hover:bg-slate-100 dark:hover:bg-slate-800 hover:text-slate-600 dark:hover:text-slate-300"
            aria-label="Close"
          >
            <X className="size-4" />
          </button>
        </div>

        {error ? (
          <div className="mb-3 rounded border border-red-200 dark:border-red-900 bg-red-50 dark:bg-red-950/40 px-3 py-2 text-xs text-red-700 dark:text-red-400">
            {error}
          </div>
        ) : null}

        {restartRequired ? (
          <div className="mb-3 flex items-center gap-3 rounded border border-amber-300 dark:border-amber-900 bg-amber-50 dark:bg-amber-950/40 px-3 py-2 text-xs text-amber-800 dark:text-amber-400">
            <span>Changed port/socket/video settings need a restart to apply.</span>
            <button
              type="button"
              onClick={doRestart}
              className="rounded border border-amber-400 dark:border-amber-700 px-2 py-1 text-xs font-medium text-amber-800 dark:text-amber-300 hover:bg-amber-100 dark:hover:bg-amber-900/40"
            >
              Restart control-server
            </button>
          </div>
        ) : null}

        {!cfg ? (
          <p className="py-8 text-center text-sm text-slate-400 dark:text-slate-500">Loading…</p>
        ) : (
          <div className="space-y-4">
            {/* Layout presets — named monitor arrangements; switch the active one from
                the sidebar. Each preset uses the same editor as before. */}
            <Section
              title="Layout presets"
              effect="immediate"
              hint="Named monitor arrangements. Switch the active preset from the sidebar — running clones reconfigure live without closing apps."
            >
              <div className="space-y-3">
                {layoutPresets.length === 0 ? (
                  <p className="text-xs text-slate-400 dark:text-slate-500">No layout presets.</p>
                ) : null}
                {layoutPresets.map((p, i) => (
                  <div key={i} className="rounded border border-slate-200 p-3 dark:border-slate-700">
                    <div className="mb-2 flex items-center gap-2">
                      <input
                        className={input}
                        placeholder="preset name (e.g. Dual 1440p)"
                        value={p.name}
                        onChange={(e) => setLayoutPresetName(i, e.target.value)}
                      />
                      <button
                        type="button"
                        onClick={() => rmLayoutPreset(i)}
                        className="rounded px-2 py-1 text-xs text-slate-500 hover:bg-slate-100 dark:text-slate-400 dark:hover:bg-slate-800"
                      >
                        Remove
                      </button>
                    </div>
                    <MonitorsEditor
                      monitors={p.monitors}
                      onChange={(mons) => setLayoutPresetMonitors(i, mons)}
                    />
                  </div>
                ))}
                <button
                  type="button"
                  onClick={addLayoutPreset}
                  className="rounded border border-slate-300 px-2 py-1 text-xs text-slate-600 hover:bg-slate-50 dark:border-slate-600 dark:text-slate-300 dark:hover:bg-slate-800"
                >
                  + Add layout preset
                </button>
              </div>
            </Section>

            {/* Agent instructions — the desktop agent's operating notes / ticket
                procedure, injected as its system prompt at clone time. */}
            <Section
              title="Agent instructions"
              effect="immediate"
              hint="The desktop agent's operating notes + ticket procedure, injected as its system prompt. Applies to newly created clones (existing clones keep the instructions they were created with)."
            >
              <textarea
                value={agentPlaybook}
                onChange={(e) => setAgentPlaybook(e.target.value)}
                spellCheck={false}
                rows={16}
                className="w-full rounded border border-slate-300 dark:border-slate-600 px-2 py-1 font-mono text-xs focus:border-slate-400 dark:focus:border-slate-500 focus:outline-none dark:bg-slate-800 dark:text-slate-100"
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
                {presets.length === 0 ? <p className="text-xs text-slate-400 dark:text-slate-500">No presets.</p> : null}
                {presets.map((p, i) => (
                  <div key={i} className="rounded border border-slate-200 dark:border-slate-700 p-3">
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
                        className="shrink-0 rounded px-2 py-1 text-xs text-slate-500 dark:text-slate-400 hover:bg-slate-100 dark:hover:bg-slate-800"
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
                        className="w-full rounded border border-slate-300 dark:border-slate-600 px-2 py-1 text-xs focus:border-slate-400 dark:focus:border-slate-500 focus:outline-none dark:bg-slate-800 dark:text-slate-100 dark:placeholder:text-slate-500"
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
                            className="w-2/5 rounded border border-slate-300 dark:border-slate-600 px-2 py-1 font-mono text-xs focus:border-slate-400 dark:focus:border-slate-500 focus:outline-none dark:bg-slate-800 dark:text-slate-100 dark:placeholder:text-slate-500"
                          />
                          <span className="text-slate-400 dark:text-slate-500">=</span>
                          <input
                            value={v.value}
                            onChange={(e) => setVar(i, k, "value", e.target.value)}
                            placeholder="value"
                            spellCheck={false}
                            className="flex-1 rounded border border-slate-300 dark:border-slate-600 px-2 py-1 font-mono text-xs focus:border-slate-400 dark:focus:border-slate-500 focus:outline-none dark:bg-slate-800 dark:text-slate-100 dark:placeholder:text-slate-500"
                          />
                          <button
                            type="button"
                            onClick={() => rmVar(i, k)}
                            title="remove variable"
                            className="shrink-0 rounded px-2 py-1 text-xs text-slate-400 dark:text-slate-500 hover:bg-slate-100 dark:hover:bg-slate-800"
                          >
                            <X className="size-4" />
                          </button>
                        </div>
                      ))}
                    </div>
                    <button
                      type="button"
                      onClick={() => addVar(i)}
                      className="mt-2 rounded border border-slate-300 dark:border-slate-600 px-2 py-1 text-xs text-slate-600 dark:text-slate-300 hover:bg-slate-50 dark:hover:bg-slate-800"
                    >
                      + Add variable
                    </button>
                    <div className="mt-2">
                      <Field label="Extra agent instructions (appended after the global instructions for this preset)">
                        <textarea
                          value={p.agentPlaybook}
                          onChange={(e) => setPresetField(i, "agentPlaybook", e.target.value)}
                          spellCheck={false}
                          rows={4}
                          placeholder="(optional)"
                          className="w-full rounded border border-slate-300 dark:border-slate-600 px-2 py-1 font-mono text-xs focus:border-slate-400 dark:focus:border-slate-500 focus:outline-none dark:bg-slate-800 dark:text-slate-100 dark:placeholder:text-slate-500"
                        />
                      </Field>
                    </div>
                  </div>
                ))}
                <button
                  type="button"
                  onClick={addPreset}
                  className="rounded border border-slate-300 dark:border-slate-600 px-2 py-1 text-xs text-slate-600 dark:text-slate-300 hover:bg-slate-50 dark:hover:bg-slate-800"
                >
                  + Add preset
                </button>
              </div>
            </Section>

            {/* Control-server — its own image version + an on-demand update check. */}
            <Section title="Control-server" effect="restart" hint="Update to the latest published image, or restart to apply changed startup settings.">
              <div className="space-y-2">
                <div className="text-xs text-slate-500 dark:text-slate-400">
                  {serverStatus?.currentRevision ? (
                    <>Version <code>{serverStatus.currentRevision}</code>{serverStatus.currentCreated ? ` · ${serverStatus.currentCreated}` : ""}</>
                  ) : (
                    "dev build (unversioned image)"
                  )}
                </div>
                <div className="flex items-center gap-2">
                  <button
                    type="button"
                    onClick={checkUpdate}
                    className="rounded border border-slate-300 dark:border-slate-600 px-2.5 py-1.5 text-xs text-slate-600 dark:text-slate-300 hover:bg-slate-50 dark:hover:bg-slate-800"
                  >
                    Check for updates
                  </button>
                  <button
                    type="button"
                    onClick={doUpdate}
                    disabled={!serverStatus?.available}
                    className="rounded bg-slate-900 px-3 py-1.5 text-xs font-medium text-white hover:bg-slate-700 disabled:opacity-50 dark:bg-slate-100 dark:text-slate-900 dark:hover:bg-white"
                  >
                    Update
                  </button>
                  <button
                    type="button"
                    onClick={doRestart}
                    className="rounded border border-slate-300 dark:border-slate-600 px-2.5 py-1.5 text-xs text-slate-600 dark:text-slate-300 hover:bg-slate-50 dark:hover:bg-slate-800"
                  >
                    Restart
                  </button>
                  {serverStatus ? (
                    <span className={`rounded px-1.5 py-0.5 text-[10px] font-semibold ${serverStatus.available ? "bg-amber-100 dark:bg-amber-900/40 text-amber-700 dark:text-amber-400" : "bg-emerald-100 dark:bg-emerald-900/40 text-emerald-700 dark:text-emerald-400"}`}>
                      {serverStatus.available ? "update available" : "up to date"}
                    </span>
                  ) : null}
                  {serverMsg ? <p className="text-xs text-slate-500 dark:text-slate-400">{serverMsg}</p> : null}
                </div>
              </div>
            </Section>

            {/* Docker / Clones. */}
            <Section title="Docker / Clones">
              <div className="space-y-3">
                <div className="flex items-center gap-2">
                  <button
                    type="button"
                    onClick={runTest}
                    className="rounded border border-slate-300 dark:border-slate-600 px-2.5 py-1.5 text-xs text-slate-600 dark:text-slate-300 hover:bg-slate-50 dark:hover:bg-slate-800"
                  >
                    Test Docker
                  </button>
                  {testMsg ? <p className="text-xs text-slate-500 dark:text-slate-400">{testMsg}</p> : null}
                </div>
                <div>
                  <div className="flex items-center gap-2">
                    <span className="text-xs font-medium text-slate-500 dark:text-slate-400">Clone hostname prefix</span>
                    <EffectBadge effect="immediate" />
                  </div>
                  <input
                    value={hostnamePrefix}
                    onChange={(e) => setHostnamePrefix(e.target.value)}
                    placeholder="pega-"
                    className={`mt-0.5 ${input}`}
                  />
                  <p className="mt-0.5 text-xs text-slate-400 dark:text-slate-500">
                    Prepended to derived clone hostnames — e.g. <code>{hostnamePrefix || "pega-"}</code>dev-123 /{" "}
                    <code>{hostnamePrefix || "pega-"}</code>my-task. Lowercased + sanitized to a DNS label; blank keeps
                    the current value.
                  </p>
                </div>
                <div>
                  <div className="flex items-center gap-2">
                    <span className="text-xs font-medium text-slate-500 dark:text-slate-400">Template reference</span>
                    <EffectBadge effect="immediate" />
                  </div>
                  <input
                    value={templateReference}
                    onChange={(e) => setTemplateReference(e.target.value)}
                    placeholder="pegasis0/rmng-template:latest"
                    spellCheck={false}
                    className={`mt-0.5 ${input}`}
                  />
                  <p className="mt-0.5 text-xs text-slate-400 dark:text-slate-500">
                    Registry <code>repo:tag</code> the wizard/Images panel pulls the clone template
                    from. The pulled image keeps this reference and clones are created from it.
                    Read fresh on each pull.
                  </p>
                </div>
                {/* Subnet is baked into the rmng bridge + every clone's static IP at first-run
                    setup, so it's one-time: editable only during first-run setup. */}
                <div>
                  <div className="flex items-center gap-2">
                    <span className="text-xs font-medium text-slate-500 dark:text-slate-400">Clone network subnet</span>
                    <EffectBadge effect="one-time" />
                  </div>
                  <input
                    value={subnet}
                    onChange={(e) => setSubnet(e.target.value)}
                    disabled={cfg.setupComplete}
                    placeholder="10.99.0.0/24"
                    spellCheck={false}
                    className={`mt-0.5 ${input} disabled:bg-slate-50 dark:disabled:bg-slate-900 disabled:text-slate-400 dark:disabled:text-slate-500`}
                  />
                  <p className="mt-0.5 text-xs text-slate-400 dark:text-slate-500">
                    {cfg.setupComplete
                      ? "Set during first-run setup — baked into the rmng network + clone IPs, cannot be changed."
                      : "IPv4 CIDR (/16–/24) for the rmng bridge — .1 gateway, .2 control-server, .10+ clone pool."}
                  </p>
                </div>
                <div className="grid grid-cols-2 gap-3">
                  <div>
                    <div className="flex items-center gap-2">
                      <span className="text-xs font-medium text-slate-500 dark:text-slate-400">CPU limit per clone (cores)</span>
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
                      <span className="text-xs font-medium text-slate-500 dark:text-slate-400">Memory limit per clone (MB)</span>
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
                <p className="text-xs text-slate-400 dark:text-slate-500">
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
              hint="Clone-source images (rmng.image=1). Pull the template from a registry (it keeps its own repo:tag) or delete an unused one; a live clone running on an image blocks its delete."
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
              </div>
            </Section>

            {/* Claude groups (named account pools; sticky — a clone moves only when its account exhausts). */}
            <Section
              title="Claude groups"
              hint="A pool of accounts. A clone bound to a group keeps its account (preserving its prompt cache) until that account is exhausted (80% 5h or 95% 7d), then moves to the least-used member."
            >
              <div className="space-y-3">
                {claudeGroups.length === 0 ? (
                  <p className="text-xs text-slate-400 dark:text-slate-500">No groups.</p>
                ) : null}
                {claudeGroups.map((g, i) => (
                  <div key={i} className="rounded border border-slate-200 dark:border-slate-700 p-3">
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
                        className="shrink-0 rounded px-2 py-1 text-xs text-slate-500 dark:text-slate-400 hover:bg-slate-100 dark:hover:bg-slate-800"
                      >
                        Remove
                      </button>
                    </div>
                    {accountEmails.length === 0 ? (
                      <p className="mt-2 text-xs text-slate-400 dark:text-slate-500">
                        Import some accounts first to add them to a group.
                      </p>
                    ) : (
                      <div className="mt-2 flex flex-wrap gap-x-4 gap-y-1.5">
                        {accountEmails.map((email) => (
                          <label key={email} className="flex items-center gap-1.5 text-xs text-slate-600 dark:text-slate-300">
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
                  className="rounded border border-slate-300 dark:border-slate-600 px-2 py-1 text-xs text-slate-600 dark:text-slate-300 hover:bg-slate-50 dark:hover:bg-slate-800"
                >
                  + Add group
                </button>
              </div>
            </Section>

            {/* Codex. */}
            <Section title="Codex">
              <div className="grid grid-cols-2 gap-3">
                <Field label="Usage poll interval (s)">
                  <input
                    type="number"
                    value={codex.pollSecs}
                    onChange={(e) => setCodex({ ...codex, pollSecs: Number(e.target.value) || 0 })}
                    className={input}
                  />
                </Field>
                <Field label="Pinned account email">
                  <input
                    value={codex.pinnedEmail}
                    onChange={(e) => setCodex({ ...codex, pinnedEmail: e.target.value })}
                    className={input}
                  />
                </Field>
                <label className="col-span-2 flex items-center gap-2 text-sm text-slate-600">
                  <input
                    type="checkbox"
                    checked={codex.usagePolling}
                    onChange={(e) => setCodex({ ...codex, usagePolling: e.target.checked })}
                  />
                  Poll ChatGPT usage (uncheck if the usage endpoint drifts; refresh + push still run)
                </label>
              </div>
            </Section>

            {/* Codex groups (named account pools). */}
            <Section
              title="Codex groups"
              hint="A pool of Codex accounts. A clone bound to a group keeps its account until that account passes 90% usage, then moves to the least-used member."
            >
              <div className="space-y-3">
                {codexGroups.length === 0 ? (
                  <p className="text-xs text-slate-400">No groups.</p>
                ) : null}
                {codexGroups.map((g, i) => (
                  <div key={i} className="rounded border border-slate-200 p-3">
                    <div className="flex items-center gap-2">
                      <input
                        value={g.name}
                        onChange={(e) => setCodexGroupName(i, e.target.value)}
                        placeholder="group name"
                        className={input}
                      />
                      <button
                        type="button"
                        onClick={() => rmCodexGroup(i)}
                        className="shrink-0 rounded px-2 py-1 text-xs text-slate-500 hover:bg-slate-100"
                      >
                        Remove
                      </button>
                    </div>
                    {codexAccountEmails.length === 0 ? (
                      <p className="mt-2 text-xs text-slate-400">
                        Import some Codex accounts first to add them to a group.
                      </p>
                    ) : (
                      <div className="mt-2 flex flex-wrap gap-x-4 gap-y-1.5">
                        {codexAccountEmails.map((email) => (
                          <label key={email} className="flex items-center gap-1.5 text-xs text-slate-600">
                            <input
                              type="checkbox"
                              checked={g.accounts.includes(email)}
                              onChange={() => toggleCodexGroupAccount(i, email)}
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
                  onClick={addCodexGroup}
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
                className="flex items-center gap-1 text-xs font-medium text-slate-500 dark:text-slate-400 hover:text-slate-700 dark:hover:text-slate-200"
              >
                {advanced ? <ChevronDown className="size-4" /> : <ChevronRight className="size-4" />}
                {advanced ? "Hide" : "Show"} ports + directories (restart the control-server to apply)
              </button>
              {advanced ? (
                <div className="mt-2 grid grid-cols-2 gap-3">
                  {/* web/video/cloneMcp/globalMcp are wired once at startup → restart-required.
                      daemonMcp applies live, but must match what clones bake in. */}
                  {(["web", "video", "cloneMcp", "globalMcp"] as const).map((k) => (
                    <div key={k}>
                      <div className="flex items-center gap-2">
                        <span className="text-xs font-medium text-slate-500 dark:text-slate-400">Port: {k}</span>
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
                      <span className="text-xs font-medium text-slate-500 dark:text-slate-400">Port: daemonMcp</span>
                      <EffectBadge effect="immediate" />
                    </div>
                    <input
                      type="number"
                      value={listen.daemonMcp}
                      onChange={(e) => setListen({ ...listen, daemonMcp: Number(e.target.value) || 0 })}
                      className={`mt-0.5 ${input}`}
                    />
                    <p className="mt-0.5 text-xs text-slate-400 dark:text-slate-500">must match what clones bake in: 9004</p>
                  </div>
                  <div>
                    <div className="flex items-center gap-2">
                      <span className="text-xs font-medium text-slate-500 dark:text-slate-400">Agent-wrapper port</span>
                      <EffectBadge effect="immediate" />
                    </div>
                    <input
                      type="number"
                      value={agentPort}
                      onChange={(e) => setAgentPort(Number(e.target.value) || 0)}
                      className={`mt-0.5 ${input}`}
                    />
                    <p className="mt-0.5 text-xs text-slate-400 dark:text-slate-500">must match what clones bake in: 4096</p>
                  </div>
                  {/* Data dir is the control-server's WORKDIR inside its container: fixed at
                      /data (the mounted volume). Shown read-only for reference. */}
                  <div>
                    <div className="flex items-center gap-2">
                      <span className="text-xs font-medium text-slate-500 dark:text-slate-400">Data dir</span>
                      <EffectBadge effect="one-time" />
                    </div>
                    <input
                      value={dataDir}
                      readOnly
                      disabled
                      className={`mt-0.5 ${input} disabled:bg-slate-50 dark:disabled:bg-slate-900 disabled:text-slate-400 dark:disabled:text-slate-500`}
                    />
                    <p className="mt-0.5 text-xs text-slate-400 dark:text-slate-500">
                      fixed at <code>/data</code> in the container (the mounted volume)
                    </p>
                  </div>
                  {/* The shared unix socket clone-daemons connect to. Fixed by the container's
                      mounted sock volume; shown read-only. */}
                  <div>
                    <div className="flex items-center gap-2">
                      <span className="text-xs font-medium text-slate-500 dark:text-slate-400">Clone socket</span>
                      <EffectBadge effect="one-time" />
                    </div>
                    <input
                      value={cloneSocket}
                      readOnly
                      disabled
                      placeholder="/srv/rmng-sock/clones.sock"
                      spellCheck={false}
                      className={`mt-0.5 ${input} disabled:bg-slate-50 dark:disabled:bg-slate-900 disabled:text-slate-400 dark:disabled:text-slate-500`}
                    />
                    <p className="mt-0.5 text-xs text-slate-400 dark:text-slate-500">
                      fixed by the container's shared sock volume
                    </p>
                  </div>
                  <div className="col-span-2">
                    <div className="flex items-center gap-2">
                      <span className="text-xs font-medium text-slate-500 dark:text-slate-400">Static (frontend) dir</span>
                      <EffectBadge effect="restart" />
                    </div>
                    <input
                      value={staticDir}
                      onChange={(e) => setStaticDir(e.target.value)}
                      spellCheck={false}
                      className={`mt-0.5 ${input}`}
                    />
                    <p className="mt-0.5 text-xs text-slate-400 dark:text-slate-500">empty = built-in (embedded) frontend</p>
                  </div>
                </div>
              ) : null}
            </Section>
          </div>
        )}
        </div>

        {/* Footer — a flex sibling of the scroll body, so it's always pinned flush to
            the panel's bottom edge. Only shown once the config has loaded. */}
        {cfg ? (
          <div className="flex items-center justify-end gap-2 border-t border-slate-100 dark:border-slate-800 bg-white dark:bg-slate-800 px-5 py-3">
            {saved ? <span className="mr-auto text-xs font-medium text-emerald-600 dark:text-emerald-400">Saved ✓</span> : null}
            <button
              type="button"
              onClick={onClose}
              className="rounded border border-slate-300 dark:border-slate-600 px-3 py-1.5 text-sm text-slate-600 dark:text-slate-300 hover:bg-slate-50 dark:hover:bg-slate-700"
            >
              Close
            </button>
            <button
              type="button"
              onClick={save}
              disabled={saving}
              className="rounded bg-slate-900 px-3 py-1.5 text-sm font-medium text-white hover:bg-slate-700 disabled:opacity-50 dark:bg-slate-100 dark:text-slate-900 dark:hover:bg-white"
            >
              {saving ? "Saving…" : "Save"}
            </button>
          </div>
        ) : null}
      </div>
    </div>
  );
}
