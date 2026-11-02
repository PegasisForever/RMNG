// A clone's status as a small dot and a word, the same on the clone's card (`CloneCard`) and on
// its row: blue = recent token activity, grey = running but inactive, purple = stopped or gone.
// An unread working→not-working transition replaces the dot with a red one. The dots are flat,
// with no glow (`~/desk/GLASS.md`, Flat content).
import type { Clone } from "~/lib/types";

/** Each state's dot and word. */
export const STATUS_DOT: Record<NonNullable<Clone["monitorState"]>, { dot: string; label: string }> = {
  working: { dot: "bg-blue-500", label: "working" },
  idle: { dot: "bg-neutral-400 dark:bg-neutral-500", label: "not working" },
  offline: { dot: "bg-purple-500", label: "offline" },
};

/** No reading: a hollow ring. Every other dot is a reading; a filled dot in a fourth colour
 *  would read as a fourth thing the agent might be doing. */
export const NO_READING_DOT = "bg-transparent ring-2 ring-inset ring-amber-500 dark:ring-amber-400";
/** An archived clone's dot. */
export const ARCHIVED_DOT = "bg-neutral-300 dark:bg-neutral-600";
/** The dot of a clone that stopped working unseen. */
export const UNREAD_DOT = "bg-rose-500";

/** The word for a clone's current state. */
const NO_READING = { dot: NO_READING_DOT, label: "no reading" };

export function statusLabel(clone: Clone): string {
  if (clone.archived) return "archived";
  if (clone.activityUnknown) return NO_READING.label;
  return STATUS_DOT[clone.monitorState ?? "idle"].label;
}

/** The coloured dot, or the unread badge when the clone stopped working unseen. */
export function CloneStatusDot({ clone }: { clone: Clone }) {
  if (clone.unread && !clone.archived) {
    return (
      <span
        aria-label="stopped working, unread"
        data-tip="stopped working"
        className={`flex size-2.5 shrink-0 rounded-full ${UNREAD_DOT}`}
      />
    );
  }
  const status = clone.activityUnknown ? NO_READING : STATUS_DOT[clone.monitorState ?? "idle"];
  return (
    <span
      aria-label={status.label}
      data-tip={status.label}
      className={`flex size-2.5 shrink-0 rounded-full ${clone.archived ? ARCHIVED_DOT : status.dot}`}
    />
  );
}
