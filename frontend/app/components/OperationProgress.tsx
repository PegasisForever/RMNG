import { useState } from "react";

import type { Operation } from "~/lib/types";

const STATUS_COLOR: Record<Operation["status"], string> = {
  running: "bg-sky-500",
  done: "bg-emerald-500",
  error: "bg-red-500",
};

export function OperationProgress({ op }: { op: Operation }) {
  const [open, setOpen] = useState(false);
  const verb = op.kind === "clone" ? "Cloning" : "Deleting";
  return (
    <div className="rounded-xl border border-slate-200 bg-white p-3 shadow-sm">
      <div className="flex items-center justify-between gap-3 text-sm">
        <div className="min-w-0">
          <span className="font-medium text-slate-900">
            {verb} {op.target}
          </span>
          {op.source ? (
            <span className="text-slate-400"> from {op.source}</span>
          ) : null}
          <div className="truncate text-xs text-slate-500">{op.message}</div>
        </div>
        <button
          type="button"
          onClick={() => setOpen((v) => !v)}
          className="shrink-0 rounded px-2 py-1 text-xs text-slate-500 hover:bg-slate-100"
        >
          {open ? "hide log" : "log"}
        </button>
      </div>
      <div className="mt-2 h-1.5 w-full overflow-hidden rounded bg-slate-100">
        <div
          className={`h-full ${STATUS_COLOR[op.status]} transition-all`}
          style={{ width: `${Math.max(2, op.pct)}%` }}
        />
      </div>
      {open ? (
        <pre className="mt-2 max-h-48 overflow-auto rounded bg-slate-50 p-2 text-[11px] leading-relaxed text-slate-600">
          {op.log.join("\n") || "(no output yet)"}
        </pre>
      ) : null}
    </div>
  );
}
