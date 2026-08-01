// Step 4 of the first-run wizard: what is about to be latched, on one screen.
//
// Every row here was already saved by the step that owns it. The list exists because the
// subnet stops being editable the moment Finish is clicked, and this is the last place to
// notice a typo in it.
import type { SetupDraft } from "~/lib/setupDraft";

export function SetupReviewStep({
  draft,
  savedTemplateReference,
  pullTarget,
  pullDone,
}: {
  /** The wizard's model, as the steps left it. */
  draft: SetupDraft;
  /** The reference step 3 saved, resolved: what was typed, else the configured default a blank
   *  field falls back to. Empty only when neither exists. */
  savedTemplateReference: string;
  /** The reference the template pull was started for. */
  pullTarget: string | null;
  /** The pull finished, so there is a template to name here. */
  pullDone: boolean;
}) {
  // The template row. A finished pull names the image that landed. Skipping still SAVED the
  // reference, because "Skip for now" skips the download and not the setting, so the skip path
  // names the saved value too. Clones are created from it either way, and this screen is the
  // last place to catch a wrong one. With nothing saved there is nothing to name.
  const template = pullDone
    ? `${pullTarget} ✓`
    : savedTemplateReference
      ? `${savedTemplateReference} (not pulled, pull one later)`
      : "not pulled (pull one later)";

  return (
    <div className="space-y-4">
      <p className="text-sm text-slate-600 dark:text-slate-300">
        Review your configuration, then finish setup. The one-time subnet latches and the{" "}
        <code>rmng</code> network is ensured when you click Finish.
      </p>
      <dl className="divide-y divide-slate-100 dark:divide-slate-800 rounded border border-slate-200 dark:border-slate-700 text-sm">
        {(
          [
            ["Clone network subnet", draft.subnet || "—"],
            ["Clone hostname prefix", draft.hostnamePrefix || "(none)"],
            ["CPU limit per clone", `${draft.cloneCpus} cores`],
            ["Memory limit per clone", `${draft.cloneMemoryMb} MB`],
            ["Monitors", `${draft.monitors.length} monitor(s)`],
            ["Chroma", draft.chroma],
            ["Template image", template],
          ] as const
        ).map(([k, v]) => (
          <div key={k} className="flex justify-between gap-3 px-3 py-2">
            <dt className="text-slate-500 dark:text-slate-400">{k}</dt>
            <dd className="text-right font-medium text-slate-800 dark:text-slate-100">{v}</dd>
          </div>
        ))}
      </dl>
    </div>
  );
}
