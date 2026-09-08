// The clone dialog's headless toggle: a single checkbox that needs no whole line
// to itself.

export function CloneOptionsRow({
  headless,
  onHeadlessChange,
}: {
  /** No desktop: the viewer shows a tmux tab view instead of a video stream. */
  headless: boolean;
  onHeadlessChange: (headless: boolean) => void;
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
    </div>
  );
}
