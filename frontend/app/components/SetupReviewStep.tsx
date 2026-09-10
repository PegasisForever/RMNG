// Final step of the first-run wizard: what is about to be latched, on one screen.
//
// Every row here was already saved by the step that owns it. The list exists as a last
// place to notice a typo before Finish latches setup. Clone images need no row: they
// build on demand from each preset's Dockerfile once setup is done.
import type { SetupDraft } from "~/lib/setupDraft";

export function SetupReviewStep({
  draft,
}: {
  /** The wizard's model, as the steps left it. */
  draft: SetupDraft;
}) {
  return (
    <div className="space-y-4">
      <p className="text-sm text-slate-600 dark:text-slate-300">
        Review your configuration, then finish setup. Setup latches and the{" "}
        <code>rmng</code> network is ensured when you click Finish.
      </p>
      <dl className="divide-y divide-slate-100 dark:divide-slate-800 rounded border border-slate-200 dark:border-slate-700 text-sm">
        {(
          [
            ["Clone hostname prefix", draft.hostnamePrefix || "(none)"],
            ["CPU limit per clone", `${draft.cloneCpus} cores`],
            ["Memory limit per clone", `${draft.cloneMemoryMb} MB`],
            ["Monitors", `${draft.monitors.length} monitor(s)`],
            ["Chroma", draft.chroma],
          ] as const
        ).map(([k, v]) => (
          <div key={k} className="flex justify-between gap-3 px-3 py-2">
            <dt className="text-slate-500 dark:text-slate-400">{k}</dt>
            <dd className="text-right font-medium text-slate-800 dark:text-slate-100">
              {v}
            </dd>
          </div>
        ))}
      </dl>
    </div>
  );
}
