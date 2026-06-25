import { useEffect, useState } from "react";

import { applyMonitors, getConfig, putConfig, testConfig } from "~/lib/api";
import type { AppConfigRedacted } from "~/lib/wire/AppConfigRedacted";

type Mon = { width: number; height: number; x: number; y: number; primary: boolean };

const PRESETS: { label: string; width: number; height: number }[] = [
  { label: "1080p", width: 1920, height: 1080 },
  { label: "1440p", width: 2560, height: 1440 },
  { label: "4K", width: 3840, height: 2160 },
];

const input =
  "w-full rounded border border-slate-300 px-2 py-1 text-sm focus:border-slate-400 focus:outline-none";
const numInput =
  "w-24 rounded border border-slate-300 px-2 py-1 text-sm focus:border-slate-400 focus:outline-none";

function Section({
  title,
  hint,
  children,
}: {
  title: string;
  hint?: string;
  children: React.ReactNode;
}) {
  return (
    <section className="border-t border-slate-100 pt-4">
      <h3 className="text-sm font-semibold text-slate-800">{title}</h3>
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

/** Graphical preview of the monitor arrangement (scaled to fit). Primary is highlighted. */
function LayoutPreview({ monitors }: { monitors: Mon[] }) {
  if (monitors.length === 0) return null;
  const minX = Math.min(...monitors.map((m) => m.x));
  const minY = Math.min(...monitors.map((m) => m.y));
  const maxX = Math.max(...monitors.map((m) => m.x + m.width));
  const maxY = Math.max(...monitors.map((m) => m.y + m.height));
  const W = Math.max(1, maxX - minX);
  const H = Math.max(1, maxY - minY);
  const BOX_W = 380;
  const BOX_H = 150;
  const PAD = 10;
  const scale = Math.min((BOX_W - 2 * PAD) / W, (BOX_H - 2 * PAD) / H);
  const offX = (BOX_W - W * scale) / 2;
  const offY = (BOX_H - H * scale) / 2;
  return (
    <svg
      viewBox={`0 0 ${BOX_W} ${BOX_H}`}
      className="mb-3 h-[150px] w-full rounded border border-slate-200 bg-slate-50"
      role="img"
      aria-label="monitor layout preview"
    >
      {monitors.map((m, i) => {
        const x = offX + (m.x - minX) * scale;
        const y = offY + (m.y - minY) * scale;
        const w = m.width * scale;
        const h = m.height * scale;
        return (
          <g key={i}>
            <rect
              x={x}
              y={y}
              width={Math.max(2, w)}
              height={Math.max(2, h)}
              rx={3}
              strokeWidth={1.5}
              className={m.primary ? "fill-emerald-100 stroke-emerald-500" : "fill-white stroke-slate-400"}
            />
            <text x={x + w / 2} y={y + h / 2 - 3} textAnchor="middle" className="fill-slate-600 text-[11px] font-semibold">
              {i}
              {m.primary ? " ★" : ""}
            </text>
            <text x={x + w / 2} y={y + h / 2 + 10} textAnchor="middle" className="fill-slate-400 text-[9px]">
              {m.width}×{m.height}
            </text>
          </g>
        );
      })}
    </svg>
  );
}

export function SettingsPanel({ onClose }: { onClose: () => void }) {
  const [cfg, setCfg] = useState<AppConfigRedacted | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [saving, setSaving] = useState(false);
  const [saved, setSaved] = useState(false);
  const [advanced, setAdvanced] = useState(false);
  const [testMsg, setTestMsg] = useState<string | null>(null);
  const [applying, setApplying] = useState(false);
  const [applyMsg, setApplyMsg] = useState<string | null>(null);

  // Editable form state. Secrets (proxmoxSsh, linear) start blank = "unchanged".
  const [monitors, setMonitors] = useState<Mon[]>([]);
  const [template, setTemplate] = useState({ baseImage: "", cores: 4, memoryMb: 8192, diskGb: 40 });
  const [proxmoxSsh, setProxmoxSsh] = useState("");
  const [hostnamePrefix, setHostnamePrefix] = useState("");
  const [linear, setLinear] = useState({ we: "", dev: "", hh: "", per: "" });
  const [claude, setClaude] = useState({
    pollSecs: 600,
    pinnedEmail: "",
    templateHostId: "",
    swapDataSubpath: "",
    autoSwapOnExhaustion: false,
  });
  const [listen, setListen] = useState({ web: 9000, video: 9001, cloneMcp: 9002, globalMcp: 9003, daemonMcp: 9004 });
  const [agentPort, setAgentPort] = useState(4096);
  const [dataDir, setDataDir] = useState("");
  const [staticDir, setStaticDir] = useState("");

  function load(c: AppConfigRedacted) {
    setCfg(c);
    setMonitors(
      c.monitors.length
        ? c.monitors.map((m) => ({ ...m }))
        : [{ width: 1920, height: 1080, x: 0, y: 0, primary: true }],
    );
    setTemplate({ ...c.template });
    setHostnamePrefix(c.proxmoxHostnamePrefix);
    setClaude({ ...c.claude, pinnedEmail: c.claude.pinnedEmail ?? "" });
    setListen({ ...c.listen });
    setAgentPort(c.agentPort);
    setDataDir(c.dataDir);
    setStaticDir(c.staticDir);
  }

  useEffect(() => {
    getConfig().then(load).catch((e: Error) => setError(e.message));
    const onKey = (e: KeyboardEvent) => e.key === "Escape" && onClose();
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [onClose]);

  const setMon = (i: number, k: "width" | "height" | "x" | "y", v: number) =>
    setMonitors((ms) => ms.map((m, j) => (j === i ? { ...m, [k]: v } : m)));
  const setPrimary = (i: number) =>
    setMonitors((ms) => ms.map((m, j) => ({ ...m, primary: j === i })));
  // New monitor: append at the right edge of the current layout (primary if it's the first).
  const addMon = (p: { width: number; height: number }) =>
    setMonitors((ms) => {
      const x = ms.reduce((mx, m) => Math.max(mx, m.x + m.width), 0);
      return [...ms, { width: p.width, height: p.height, x, y: 0, primary: ms.length === 0 }];
    });
  const rmMon = (i: number) =>
    setMonitors((ms) => {
      const next = ms.filter((_, j) => j !== i);
      if (next.length && !next.some((m) => m.primary)) next[0] = { ...next[0], primary: true };
      return next;
    });

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
        template,
        proxmox: { ssh: proxmoxSsh, hostnamePrefix },
        linear,
        claude: { ...claude, pinnedEmail: claude.pinnedEmail || null },
        listen,
        agentPort,
        dataDir,
        staticDir,
      };
      const next = await putConfig(patch);
      load(next); // re-seed from the server's redacted view; clears write-only inputs
      setProxmoxSsh("");
      setLinear({ we: "", dev: "", hh: "", per: "" });
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
      const r = await testConfig("proxmox");
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

        {!cfg ? (
          <p className="py-8 text-center text-sm text-slate-400">Loading…</p>
        ) : (
          <div className="space-y-4">
            {/* Monitors — size, position + primary per monitor, with a live preview. */}
            <Section
              title="Monitors"
              hint="Set each monitor's size, position (x,y in the unified desktop) and which is primary. Applies to newly provisioned clones; restart an existing clone's daemon to pick up changes."
            >
              <LayoutPreview monitors={monitors} />
              <div className="space-y-1.5">
                <div className="flex items-center gap-2 px-0.5 text-[10px] font-medium uppercase tracking-wide text-slate-400">
                  <span className="w-5">#</span>
                  <span className="w-[4.5rem]">width</span>
                  <span className="w-[4.5rem]">height</span>
                  <span className="w-[4.5rem]">x</span>
                  <span className="w-[4.5rem]">y</span>
                  <span>primary</span>
                </div>
                {monitors.map((m, i) => (
                  <div key={i} className="flex items-center gap-2">
                    <span className="w-5 text-xs text-slate-400">{i}</span>
                    {(["width", "height", "x", "y"] as const).map((k) => (
                      <input
                        key={k}
                        type="number"
                        value={m[k]}
                        min={k === "width" || k === "height" ? 1 : 0}
                        onChange={(e) => setMon(i, k, Number(e.target.value) || 0)}
                        className="w-[4.5rem] rounded border border-slate-300 px-1.5 py-1 text-sm focus:border-slate-400 focus:outline-none"
                      />
                    ))}
                    <input
                      type="radio"
                      name="primaryMonitor"
                      checked={m.primary}
                      onChange={() => setPrimary(i)}
                      title="set as primary"
                      className="ml-1 accent-emerald-600"
                    />
                    <button
                      type="button"
                      onClick={() => rmMon(i)}
                      disabled={monitors.length <= 1}
                      className="ml-auto rounded px-2 py-1 text-xs text-slate-500 hover:bg-slate-100 disabled:opacity-40"
                    >
                      Remove
                    </button>
                  </div>
                ))}
              </div>
              <div className="mt-2 flex flex-wrap gap-1.5">
                {PRESETS.map((p) => (
                  <button
                    key={p.label}
                    type="button"
                    onClick={() => addMon(p)}
                    className="rounded border border-slate-300 px-2 py-1 text-xs text-slate-600 hover:bg-slate-50"
                  >
                    + {p.label} ({p.width}×{p.height})
                  </button>
                ))}
              </div>
              {/* Apply to running clones (saves + restarts each clone's desktop). */}
              <div className="mt-3 flex items-center gap-3 border-t border-slate-100 pt-3">
                <button
                  type="button"
                  onClick={applyNow}
                  disabled={applying}
                  className="rounded bg-sky-600 px-3 py-1.5 text-sm font-medium text-white hover:bg-sky-500 disabled:opacity-50"
                >
                  {applying ? "Applying…" : "Apply to running clones"}
                </button>
                <span className="text-xs text-slate-500">
                  {applyMsg ?? "Saves the layout, then restarts each running clone's desktop to apply it."}
                </span>
              </div>
            </Section>

            {/* Clone container (template). */}
            <Section title="Clone container" hint="Resources + base image for newly bootstrapped clones.">
              <div className="grid grid-cols-2 gap-3">
                <div className="col-span-2">
                  <Field label="Base image (vztmpl)">
                    <input
                      value={template.baseImage}
                      onChange={(e) => setTemplate({ ...template, baseImage: e.target.value })}
                      className={input}
                    />
                  </Field>
                </div>
                <Field label="Cores">
                  <input
                    type="number"
                    value={template.cores}
                    onChange={(e) => setTemplate({ ...template, cores: Number(e.target.value) || 0 })}
                    className={input}
                  />
                </Field>
                <Field label="Memory (MB)">
                  <input
                    type="number"
                    value={template.memoryMb}
                    onChange={(e) => setTemplate({ ...template, memoryMb: Number(e.target.value) || 0 })}
                    className={input}
                  />
                </Field>
                <Field label="Disk (GB)">
                  <input
                    type="number"
                    value={template.diskGb}
                    onChange={(e) => setTemplate({ ...template, diskGb: Number(e.target.value) || 0 })}
                    className={input}
                  />
                </Field>
              </div>
            </Section>

            {/* Proxmox. */}
            <Section title="Proxmox">
              <div className="space-y-3">
                <div className="flex items-end gap-2">
                  <div className="flex-1">
                    <Secret
                      label="SSH target (e.g. root@10.0.0.100)"
                      set={cfg.proxmoxSshSet}
                      value={proxmoxSsh}
                      onChange={setProxmoxSsh}
                    />
                  </div>
                  <button
                    type="button"
                    onClick={runTest}
                    className="rounded border border-slate-300 px-2.5 py-1.5 text-xs text-slate-600 hover:bg-slate-50"
                  >
                    Test
                  </button>
                </div>
                {testMsg ? <p className="text-xs text-slate-500">{testMsg}</p> : null}
                <div>
                  <Field label="Clone hostname prefix">
                    <input
                      value={hostnamePrefix}
                      onChange={(e) => setHostnamePrefix(e.target.value)}
                      placeholder="pega-"
                      className={input}
                    />
                  </Field>
                  <p className="mt-0.5 text-xs text-slate-400">
                    Prepended to derived clone hostnames — e.g. <code>{hostnamePrefix || "pega-"}</code>dev-123 /{" "}
                    <code>{hostnamePrefix || "pega-"}</code>my-task. Lowercased + sanitized to a DNS label; blank keeps
                    the current value.
                  </p>
                </div>
              </div>
            </Section>

            {/* Linear (write-only keys). */}
            <Section title="Linear API keys" hint="Per-workspace keys. Leave blank to keep the stored key.">
              <div className="grid grid-cols-2 gap-3">
                {(["we", "dev", "hh", "per"] as const).map((k) => (
                  <Secret
                    key={k}
                    label={k.toUpperCase()}
                    set={cfg.linearKeysSet[k]}
                    value={linear[k]}
                    onChange={(v) => setLinear({ ...linear, [k]: v })}
                  />
                ))}
              </div>
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
                <Field label="Template host id">
                  <input
                    value={claude.templateHostId}
                    onChange={(e) => setClaude({ ...claude, templateHostId: e.target.value })}
                    className={input}
                  />
                </Field>
                <Field label="claude-swap data subpath">
                  <input
                    value={claude.swapDataSubpath}
                    onChange={(e) => setClaude({ ...claude, swapDataSubpath: e.target.value })}
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

            {/* Clone accounts (read-only; managed via Import). */}
            <Section
              title="Clone Claude accounts"
              hint="Imported from claude-swap (use Import in the sidebar). Tokens are write-only and never shown."
            >
              {cfg.cloneAccounts.length === 0 ? (
                <p className="text-xs text-slate-400">None imported.</p>
              ) : (
                <ul className="space-y-1 text-xs">
                  {cfg.cloneAccounts.map((a) => (
                    <li key={a.email} className="flex items-center gap-2">
                      <span className="text-slate-600">{a.email}</span>
                      <span
                        className={`rounded px-1.5 py-0.5 text-[10px] font-semibold ${
                          a.longLivedTokenSet ? "bg-emerald-100 text-emerald-700" : "bg-slate-100 text-slate-400"
                        }`}
                      >
                        token {a.longLivedTokenSet ? "set" : "unset"}
                      </span>
                    </li>
                  ))}
                </ul>
              )}
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
                  {(["web", "video", "cloneMcp", "globalMcp", "daemonMcp"] as const).map((k) => (
                    <Field key={k} label={`Port: ${k}`}>
                      <input
                        type="number"
                        value={listen[k]}
                        onChange={(e) => setListen({ ...listen, [k]: Number(e.target.value) || 0 })}
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
                  <Field label="Data dir">
                    <input value={dataDir} onChange={(e) => setDataDir(e.target.value)} className={input} />
                  </Field>
                  <div className="col-span-2">
                    <Field label="Static (frontend) dir">
                      <input value={staticDir} onChange={(e) => setStaticDir(e.target.value)} className={input} />
                    </Field>
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
