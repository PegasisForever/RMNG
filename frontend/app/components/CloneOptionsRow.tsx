// The clone dialog's headless toggle: a single checkbox that needs no whole line
// to itself.
import { TEXT_2 } from "~/desk/control/ui";

/** A checkbox, ticked in the one accent. */
const CHECK = "size-3.5 accent-emerald-600";

export function CloneOptionsRow({
  headless,
  onHeadlessChange,
  runStartupScript,
  onRunStartupScriptChange,
  rebuild,
  onRebuildChange,
}: {
  /** No desktop: the screen shows a tmux tab view instead of a video stream. */
  headless: boolean;
  onHeadlessChange: (headless: boolean) => void;
  /** Run the preset's startup script as the clone user. On unless unchecked. */
  runStartupScript: boolean;
  onRunStartupScriptChange: (run: boolean) => void;
  /** Rebuild image: force a fresh build with a fresh base pull. Off unless checked. */
  rebuild: boolean;
  onRebuildChange: (rebuild: boolean) => void;
}) {
  return (
    <div className={`mt-3 flex flex-wrap items-center gap-x-5 gap-y-2 text-xs font-medium ${TEXT_2}`}>
      <label className="flex cursor-pointer items-center gap-2">
        <input
          type="checkbox"
          checked={headless}
          onChange={(e) => onHeadlessChange(e.target.checked)}
          className={CHECK}
        />
        Headless (no desktop)
      </label>
      <label
        className="flex cursor-pointer items-center gap-2"
        data-tip="Runs the preset's startup script as the clone user when the clone is created"
      >
        <input
          type="checkbox"
          checked={runStartupScript}
          onChange={(e) => onRunStartupScriptChange(e.target.checked)}
          className={CHECK}
        />
        Run startup script
      </label>
      <label
        className="flex cursor-pointer items-center gap-2"
        data-tip="Pulls the latest base image and rebuilds the preset image before the clone is created"
      >
        <input
          type="checkbox"
          checked={rebuild}
          onChange={(e) => onRebuildChange(e.target.checked)}
          className={CHECK}
        />
        Rebuild image
      </label>
    </div>
  );
}
