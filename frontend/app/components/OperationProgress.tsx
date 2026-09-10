import {
  CheckCircle2,
  ChevronDown,
  CircleAlert,
  LoaderCircle,
} from "lucide-react";
import { useId, useState } from "react";

import { CardFrame } from "~/components/BoardCard";
import type { Operation } from "~/lib/types";

const STATUS_COLOR: Record<Operation["status"], string> = {
  running: "bg-sky-500",
  done: "bg-emerald-500",
  error: "bg-red-500",
};

const STATUS_ICON = {
  running: LoaderCircle,
  done: CheckCircle2,
  error: CircleAlert,
};

const STATUS_LABEL: Record<Operation["status"], string> = {
  running: "Running",
  done: "Complete",
  error: "Failed",
};

const VERB: Record<Operation["kind"], string> = {
  clone: "Cloning",
  delete: "Deleting",
  archive: "Archiving",
  unarchive: "Restoring",
  pull: "Pulling",
  commit: "Committing",
  update: "Updating",
};

export function OperationProgress({ op }: { op: Operation }) {
  const [open, setOpen] = useState(false);
  const logId = useId();
  const titleId = useId();
  const verb = VERB[op.kind];
  const StatusIcon = STATUS_ICON[op.status];
  const pct = Number.isFinite(op.pct) ? Math.min(100, Math.max(0, op.pct)) : 0;

  return (
    <CardFrame>
      <article aria-labelledby={titleId} className="px-2 py-1.5">
        <div className="flex items-center justify-between gap-2 text-[10px] font-medium">
          <span className="flex min-w-0 items-center gap-1.5 text-slate-500 dark:text-slate-400">
            <StatusIcon
              aria-hidden="true"
              className={`size-3 shrink-0 ${op.status === "running" ? "motion-safe:animate-spin text-sky-500" : ""} ${op.status === "done" ? "text-emerald-500" : ""} ${op.status === "error" ? "text-red-500" : ""}`}
            />
            {verb}
            <span className="sr-only"> — {STATUS_LABEL[op.status]}</span>
          </span>
          <button
            type="button"
            onClick={() => setOpen((v) => !v)}
            aria-expanded={open}
            aria-controls={logId}
            aria-label={`${open ? "Hide" : "Show"} log for ${op.target}`}
            className="flex shrink-0 items-center gap-1 rounded px-1 py-0.5 text-slate-500 hover:bg-slate-100 hover:text-slate-700 focus-visible:outline-2 focus-visible:outline-sky-500 dark:text-slate-400 dark:hover:bg-slate-800 dark:hover:text-slate-200"
          >
            Log
            <ChevronDown
              aria-hidden="true"
              className={`size-3 transition-transform ${open ? "rotate-180" : ""}`}
            />
          </button>
        </div>
        <h3
          id={titleId}
          className="break-words text-sm font-medium leading-snug text-slate-800 dark:text-slate-100"
        >
          {op.target}
        </h3>
        {op.source ? (
          <p
            title={`From ${op.source}`}
            className="mt-0.5 truncate text-[10px] text-slate-500 dark:text-slate-400"
          >
            From {op.source}
          </p>
        ) : null}
        <div className="mb-1 mt-1 flex items-center gap-2 text-[10px]">
          <span
            title={op.message}
            className="min-w-0 flex-1 truncate text-slate-500 dark:text-slate-400"
          >
            {op.message}
          </span>
          <span className="shrink-0 font-medium tabular-nums text-slate-600 dark:text-slate-300">
            {Math.round(pct)}%
          </span>
        </div>
        <div
          role="progressbar"
          aria-label={`${verb} ${op.target}`}
          aria-valuemin={0}
          aria-valuemax={100}
          aria-valuenow={pct}
          aria-valuetext={`${STATUS_LABEL[op.status]}: ${op.message} (${Math.round(pct)}%)`}
          className="h-1 w-full overflow-hidden rounded-sm bg-slate-100 dark:bg-slate-800"
        >
          <div
            className={`h-full ${STATUS_COLOR[op.status]} transition-all`}
            style={{ width: `${pct}%` }}
          />
        </div>
        <pre
          id={logId}
          hidden={!open}
          className="mt-2 max-h-48 select-text overflow-auto rounded bg-slate-50 p-2 text-[11px] leading-relaxed text-slate-600 dark:bg-slate-950 dark:text-slate-300"
        >
          {op.log.join("\n") || "(no output yet)"}
        </pre>
      </article>
    </CardFrame>
  );
}
