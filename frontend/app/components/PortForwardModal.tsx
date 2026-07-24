// Configure a clone's local port forwards (remote clone port → 127.0.0.1:<local> on the
// machine running the native viewer). Mirrors the change-account modal shell. Live
// status (listening / error / offline) is merged from the `forwards` SSE event by rule id.
import { useState } from "react";

import type { Clone } from "~/lib/types";
import type { ForwardRuntime } from "~/lib/wire/ForwardRuntime";

type Row = { id?: string; remotePort: string; localPort: string; enabled: boolean };

function toRows(clone: Clone): Row[] {
  return (clone.forwards ?? []).map((f) => ({
    id: f.id,
    remotePort: String(f.remotePort),
    localPort: String(f.localPort),
    enabled: f.enabled,
  }));
}

function statusFor(runtime: ForwardRuntime[], id?: string): ForwardRuntime | undefined {
  return id ? runtime.find((r) => r.id === id) : undefined;
}

export function PortForwardModal({
  clone,
  runtime,
  busy,
  error,
  onClose,
  onSubmit,
}: {
  clone: Clone;
  runtime: ForwardRuntime[];
  busy: boolean;
  error: string | null;
  onClose: () => void;
  onSubmit: (
    forwards: Array<{ id?: string; remotePort: number; localPort: number; enabled: boolean; label?: string }>,
  ) => void;
}) {
  const [rows, setRows] = useState<Row[]>(() => toRows(clone));

  const update = (i: number, patch: Partial<Row>) =>
    setRows((rs) => rs.map((r, j) => (j === i ? { ...r, ...patch } : r)));
  const remove = (i: number) => setRows((rs) => rs.filter((_, j) => j !== i));
  const add = () =>
    setRows((rs) => [...rs, { remotePort: "", localPort: "", enabled: true }]);

  const submit = () => {
    const forwards = rows.map((r) => ({
      id: r.id,
      remotePort: Number(r.remotePort),
      localPort: Number(r.localPort),
      enabled: r.enabled,
    }));
    onSubmit(forwards);
  };

  // All errors surfaced at the top: the save/validation error (if any) plus every rule
  // whose live runtime state is "error", labelled by its port mapping so it's traceable
  // now that the per-row status column is gone.
  const runtimeErrors = rows.flatMap((r) => {
    const rt = statusFor(runtime, r.id);
    return rt?.state === "error"
      ? [{ id: r.id!, text: `${r.remotePort || "?"}→${r.localPort || "?"}: ${rt.error ?? "failed"}` }]
      : [];
  });

  return (
    <div className="fixed inset-0 z-50 flex items-center justify-center bg-slate-900/30 p-4" onClick={onClose}>
      <div
        className="w-full max-w-lg rounded-xl border border-slate-200 bg-white p-5 shadow-xl dark:border-slate-700 dark:bg-slate-800"
        onClick={(e) => e.stopPropagation()}
        onKeyDown={(e) => {
          if (e.key === "Escape") onClose();
        }}
      >
        <h3 className="text-sm font-semibold text-slate-900 dark:text-slate-100">
          Port forwards · <span className="text-emerald-700 dark:text-emerald-400">{clone.displayName ?? clone.id}</span>
        </h3>
        <p className="mt-1 text-xs text-slate-500 dark:text-slate-400">
          Expose a port inside this clone at <code>127.0.0.1:&lt;local&gt;</code> on the machine running the viewer.
        </p>

        {error || runtimeErrors.length > 0 ? (
          <div className="mt-3 space-y-1">
            {error ? <p className="text-xs text-red-600 dark:text-red-400">{error}</p> : null}
            {runtimeErrors.map((e) => (
              <p key={e.id} className="text-xs text-red-600 dark:text-red-400">
                {e.text}
              </p>
            ))}
          </div>
        ) : null}

        <div className="mt-4 space-y-2">
          <div className="grid grid-cols-[1fr_1fr_2rem_2rem] gap-2 text-[11px] font-medium uppercase tracking-wide text-slate-400">
            <span>Remote</span>
            <span>Local</span>
            <span className="text-center">On</span>
            <span></span>
          </div>
          {rows.map((r, i) => (
            <div key={i} className="grid grid-cols-[1fr_1fr_2rem_2rem] items-center gap-2">
              <input
                inputMode="numeric"
                value={r.remotePort}
                onChange={(e) => update(i, { remotePort: e.target.value, id: undefined })}
                placeholder="3000"
                className="min-w-0 rounded-md border border-slate-300 px-2 py-1 text-sm dark:border-slate-600 dark:bg-slate-900 dark:text-slate-100"
              />
              <input
                inputMode="numeric"
                value={r.localPort}
                onChange={(e) => update(i, { localPort: e.target.value, id: undefined })}
                placeholder="8080"
                className="min-w-0 rounded-md border border-slate-300 px-2 py-1 text-sm dark:border-slate-600 dark:bg-slate-900 dark:text-slate-100"
              />
              <input
                type="checkbox"
                checked={r.enabled}
                onChange={(e) => update(i, { enabled: e.target.checked })}
                className="size-4 justify-self-center"
              />
              <button
                type="button"
                onClick={() => remove(i)}
                className="cursor-pointer justify-self-center rounded px-1.5 py-1 text-xs text-red-600 hover:bg-red-50 dark:text-red-400 dark:hover:bg-red-950/40"
              >
                ✕
              </button>
            </div>
          ))}
          <button
            type="button"
            onClick={add}
            className="cursor-pointer text-xs text-emerald-700 hover:underline dark:text-emerald-400"
          >
            + Add forward
          </button>
        </div>

        <div className="mt-5 flex justify-end gap-2">
          <button
            type="button"
            onClick={onClose}
            className="rounded-md px-3 py-1.5 text-sm text-slate-600 hover:bg-slate-100 dark:text-slate-300 dark:hover:bg-slate-800"
          >
            Cancel
          </button>
          <button
            type="button"
            disabled={busy}
            onClick={submit}
            className="rounded-md bg-emerald-600 px-3 py-1.5 text-sm font-medium text-white hover:bg-emerald-500 disabled:opacity-50"
          >
            {busy ? "Saving…" : "Save"}
          </button>
        </div>
      </div>
    </div>
  );
}
