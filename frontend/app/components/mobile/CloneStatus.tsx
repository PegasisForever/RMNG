// The status vocabulary the phone shares with the desktop sidebar, so a clone reads the
// same on both: blue = recent token activity, gray = running but inactive, purple = stopped
// or gone. An unread working→not-working transition replaces the dot with a red badge,
// which on a phone row is the one thing worth a second colour.
import type { Clone } from "~/lib/types";

const STATUS: Record<NonNullable<Clone["monitorState"]>, { dot: string; label: string }> = {
  working: { dot: "bg-blue-500", label: "working" },
  idle: { dot: "bg-slate-400 dark:bg-slate-500", label: "not working" },
  offline: { dot: "bg-purple-500", label: "offline" },
};

/** The word for a clone's current state. */
export function statusLabel(clone: Clone): string {
  if (clone.archived) return "archived";
  return STATUS[clone.monitorState ?? "idle"].label;
}

/** The coloured dot, or the unread badge when the clone stopped working unseen. */
export function CloneStatusDot({ clone }: { clone: Clone }) {
  if (clone.unread && !clone.archived) {
    return (
      <span
        aria-label="stopped working, unread"
        title="stopped working"
        className="flex size-2.5 shrink-0 rounded-full bg-rose-500"
      />
    );
  }
  const status = STATUS[clone.monitorState ?? "idle"];
  return (
    <span
      aria-label={status.label}
      title={status.label}
      className={`flex size-2.5 shrink-0 rounded-full ${clone.archived ? "bg-slate-300 dark:bg-slate-600" : status.dot}`}
    />
  );
}
