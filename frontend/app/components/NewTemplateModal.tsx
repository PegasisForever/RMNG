// Provision a brand-new template CT from zero. The base image is fixed (Ubuntu
// 26.04 — the patched gnome-shell deb is compiled against its GNOME only); the
// operator picks just a hostname + CT resources here.
import { useState } from "react";

/** CT resources sent with the bootstrap request. */
export type TemplateResources = { cores: number; memoryMb: number; diskGb: number };

/** What every template has actually been built with (proven on the current template). */
const DEFAULT_RESOURCES: TemplateResources = { cores: 16, memoryMb: 32768, diskGb: 128 };

/** Mirrors the server's `is_dns_label`: non-empty, ≤63 chars, lowercase letters /
 *  digits / hyphens, no leading or trailing hyphen. */
function isDnsLabel(s: string): boolean {
  return s.length <= 63 && /^[a-z0-9]([a-z0-9-]*[a-z0-9])?$/.test(s);
}

export function NewTemplateModal({
  busy,
  existing,
  onClose,
  onCreate,
}: {
  busy: boolean;
  /** Existing host ids, to flag a duplicate name before the server does. */
  existing: Set<string>;
  onClose: () => void;
  onCreate: (hostname: string, resources: TemplateResources) => void;
}) {
  const [hostname, setHostname] = useState("");
  const [resources, setResources] = useState(DEFAULT_RESOURCES);
  const trimmed = hostname.trim();
  const labelOk = isDnsLabel(trimmed);
  const duplicate = existing.has(trimmed);
  const resourcesOk = resources.cores >= 1 && resources.memoryMb >= 1024 && resources.diskGb >= 8;
  const valid = labelOk && !duplicate && resourcesOk;

  const setRes = (k: keyof TemplateResources, v: number) =>
    setResources((r) => ({ ...r, [k]: v }));

  function submit() {
    if (!valid || busy) return;
    onCreate(trimmed, resources);
  }

  return (
    <div
      className="fixed inset-0 z-50 flex items-center justify-center bg-slate-900/30 p-4"
      onClick={onClose}
    >
      <div
        className="w-full max-w-md rounded-xl border border-slate-200 bg-white p-5 shadow-xl"
        onClick={(e) => e.stopPropagation()}
        onKeyDown={(e) => {
          if (e.key === "Escape") onClose();
        }}
      >
        <h3 className="text-sm font-semibold text-slate-900">New template</h3>
        <p className="mt-1 text-xs text-slate-500">
          Provisions a fresh Ubuntu 26.04 container (the only base our patched GNOME is built
          for) with the resources below. The new container is registered as a clonable
          template.
        </p>

        <label className="mt-4 block text-xs font-medium text-slate-600">
          Hostname
          <input
            autoFocus
            value={hostname}
            onChange={(e) => setHostname(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === "Enter") submit();
            }}
            placeholder="e.g. rmng-template"
            spellCheck={false}
            className="mt-1 w-full rounded-md border border-slate-300 px-3 py-2 text-sm font-normal text-slate-900 placeholder:text-slate-400 focus:border-emerald-500 focus:outline-none"
          />
          {trimmed && !labelOk ? (
            <p className="mt-1 text-[11px] font-normal text-red-600">
              lowercase letters, digits and hyphens only (no leading/trailing hyphen, ≤63 chars)
            </p>
          ) : duplicate ? (
            <p className="mt-1 text-[11px] font-normal text-red-600">
              a host named “{trimmed}” already exists
            </p>
          ) : null}
        </label>

        <div className="mt-3 grid grid-cols-3 gap-3">
          {(
            [
              { key: "cores", label: "Cores", min: 1 },
              { key: "memoryMb", label: "Memory (MB)", min: 1024 },
              { key: "diskGb", label: "Disk (GB)", min: 8 },
            ] as const
          ).map((f) => (
            <label key={f.key} className="block text-xs font-medium text-slate-600">
              {f.label}
              <input
                type="number"
                min={f.min}
                value={resources[f.key]}
                onChange={(e) => setRes(f.key, Number(e.target.value) || 0)}
                onKeyDown={(e) => {
                  if (e.key === "Enter") submit();
                }}
                className="mt-1 w-full rounded-md border border-slate-300 px-3 py-2 text-sm font-normal text-slate-900 focus:border-emerald-500 focus:outline-none"
              />
            </label>
          ))}
        </div>
        {!resourcesOk ? (
          <p className="mt-1 text-[11px] text-red-600">
            need ≥1 core, ≥1024 MB memory and ≥8 GB disk
          </p>
        ) : null}

        <div className="mt-4 flex justify-end gap-2">
          <button
            type="button"
            onClick={onClose}
            className="rounded-md px-3 py-1.5 text-sm text-slate-600 hover:bg-slate-100"
          >
            Cancel
          </button>
          <button
            type="button"
            onClick={submit}
            disabled={!valid || busy}
            className="rounded-md bg-emerald-600 px-4 py-1.5 text-sm font-medium text-white hover:bg-emerald-700 disabled:opacity-40"
          >
            {busy ? "Provisioning…" : "Create template"}
          </button>
        </div>
      </div>
    </div>
  );
}
