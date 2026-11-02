import {
  CheckCircle2,
  ChevronDown,
  CircleAlert,
  LoaderCircle,
} from "lucide-react";
import { useId, useState } from "react";

import { ICON_BUTTON, TEXT, TEXT_2 } from "~/desk/control/ui";
import type { Operation } from "~/lib/types";

/** The bar's fill. Busy is grey: the one accent, green, is for what is done
 *  (`~/desk/GLASS.md`, Color). */
const STATUS_COLOR: Record<Operation["status"], string> = {
  running: "bg-neutral-500 dark:bg-neutral-300",
  done: "bg-emerald-500",
  error: "bg-rose-500",
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

/** One present-participle label per operation kind. `Record<Operation["kind"], ...>` keeps
 *  this exhaustive against the generated kind list: a new server kind fails the type check
 *  here instead of rendering a blank verb. */
export const VERB: Record<Operation["kind"], string> = {
  clone: "Cloning",
  delete: "Deleting",
  archive: "Archiving",
  unarchive: "Restoring",
  pull: "Pulling",
  prebuild: "Building",
  update: "Updating",
};

/** The card's own frame, where it sits in a dialog (a running job): a card of `~/desk/GLASS.md`'s
 *  flat content, with no shadow. */
const FRAME = "rounded-lg border border-black/10 bg-white/70 dark:border-white/10 dark:bg-white/[0.06]";

export function OperationProgress({
  op,
  box,
}: {
  op: Operation;
  /** The card's frame and fill in place of its own (a dialog's): the control panel's card, as
   *  its accounts and clones wear it. */
  box?: string;
}) {
  const [open, setOpen] = useState(false);
  const logId = useId();
  const titleId = useId();
  const verb = VERB[op.kind];
  const StatusIcon = STATUS_ICON[op.status];
  const pct = Number.isFinite(op.pct) ? Math.min(100, Math.max(0, op.pct)) : 0;

  const card = (
    <article aria-labelledby={titleId} className={box ? "px-2.5 py-2" : "px-2 py-1.5"}>
      <div className="flex items-center justify-between gap-2 text-[10px] font-medium">
        <span className={`flex min-w-0 items-center gap-1.5 ${TEXT_2}`}>
          <StatusIcon
            aria-hidden="true"
            className={`size-3 shrink-0 ${op.status === "running" ? "motion-safe:animate-spin" : ""} ${op.status === "done" ? "text-emerald-600 dark:text-emerald-400" : ""} ${op.status === "error" ? "text-rose-600 dark:text-rose-400" : ""}`}
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
          className={`flex shrink-0 items-center gap-1 px-1 py-0.5 ${ICON_BUTTON}`}
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
        className={`break-words text-sm font-medium leading-snug ${TEXT}`}
      >
        {op.target}
      </h3>
      {op.source ? (
        <p
          data-tip={`From ${op.source}`}
          className={`mt-0.5 truncate text-[10px] ${TEXT_2}`}
        >
          From {op.source}
        </p>
      ) : null}
      <div className="mb-1 mt-1 flex items-center gap-2 text-[10px]">
        <span
          data-tip={op.message}
          className={`min-w-0 flex-1 truncate ${TEXT_2}`}
        >
          {op.message}
        </span>
        <span className={`shrink-0 font-medium tabular-nums ${TEXT}`}>
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
        // The track of the usage bars
        className="h-1 w-full overflow-hidden rounded-sm bg-black/10 dark:bg-white/15"
      >
        <div
          className={`h-full ${STATUS_COLOR[op.status]} transition-all`}
          style={{ width: `${pct}%` }}
        />
      </div>
      <pre
        id={logId}
        hidden={!open}
        // A hollow in the card, as a column is on the board
        className={`mt-2 max-h-48 select-text overflow-auto rounded-md border border-black/10 bg-black/[0.03] p-2 text-[11px] leading-relaxed dark:border-white/10 dark:bg-black/25 ${TEXT_2}`}
      >
        {op.log.join("\n") || "(no output yet)"}
      </pre>
    </article>
  );
  // Nothing in it is selectable but the log: its text is a label, not content anybody copies
  return <div className={`select-none overflow-hidden ${box ?? FRAME}`}>{card}</div>;
}
