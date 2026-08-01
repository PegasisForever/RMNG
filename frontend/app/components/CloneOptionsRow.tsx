// The clone dialog's switch row: two independent toggles, neither of which needs a whole line
// to itself.
//
// The sub-clone toggle only exists when there is something to nest under. A clone can parent
// another only if it is managed and top-level, so the dialog is handed the candidate rather
// than working it out — with nothing selected, or with a sub clone selected, the row is one
// checkbox wide.
import type { Clone } from "~/lib/types";

export function CloneOptionsRow({
  headless,
  parentCandidate,
  asSubClone,
  onHeadlessChange,
  onAsSubCloneChange,
}: {
  /** No desktop: the viewer shows a tmux tab view instead of a video stream. */
  headless: boolean;
  /** The clone offered as a parent, or null when nothing can be one. */
  parentCandidate: Clone | null;
  asSubClone: boolean;
  onHeadlessChange: (headless: boolean) => void;
  onAsSubCloneChange: (asSubClone: boolean) => void;
}) {
  return (
    <div className="mt-3 flex flex-wrap items-center gap-x-5 gap-y-2 text-xs font-medium text-slate-500 dark:text-slate-400">
      <label className="flex cursor-pointer items-center gap-2">
        <input
          type="checkbox"
          checked={headless}
          onChange={(e) => onHeadlessChange(e.target.checked)}
          className="h-3.5 w-3.5 rounded border-slate-300 text-emerald-600 focus:ring-emerald-500 dark:border-slate-600"
        />
        Headless (no desktop)
      </label>

      {parentCandidate ? (
        <label className="flex min-w-0 cursor-pointer items-center gap-2">
          <input
            type="checkbox"
            checked={asSubClone}
            onChange={(e) => onAsSubCloneChange(e.target.checked)}
            className="h-3.5 w-3.5 shrink-0 rounded border-slate-300 text-emerald-600 focus:ring-emerald-500 dark:border-slate-600"
          />
          <span className="truncate">
            Sub clone of{" "}
            <span className="font-mono text-slate-700 dark:text-slate-200">
              {parentCandidate.displayName || parentCandidate.id}
            </span>
          </span>
        </label>
      ) : null}
    </div>
  );
}
