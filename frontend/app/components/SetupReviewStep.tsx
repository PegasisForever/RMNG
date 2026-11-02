// Final step of the first-run wizard: what is about to be latched, on one screen.
//
// Every row here was already saved by the step that owns it. The list exists as a last
// place to notice a typo before Finish latches setup. Clone images need no row: they
// build on demand from each preset's Dockerfile once setup is done.
import { DIVIDE, LINE, R_PIECE, TEXT, TEXT_2 } from "~/desk/control/ui";
import type { SetupDraft } from "~/lib/setupDraft";

export function SetupReviewStep({
  draft,
}: {
  /** The wizard's model, as the steps left it. */
  draft: SetupDraft;
}) {
  return (
    <div className="space-y-4">
      <p className={`text-sm ${TEXT_2}`}>
        Review your configuration, then finish setup. Setup latches and the{" "}
        <code>rmng</code> network is ensured when you click Finish.
      </p>
      <dl className={`divide-y border text-sm ${R_PIECE} ${DIVIDE} ${LINE}`}>
        {(
          [
            ["Clone hostname prefix", draft.hostnamePrefix || "(none)"],
            ["CPU limit per clone", `${draft.cloneCpus} cores`],
            ["Memory limit per clone", `${draft.cloneMemoryMb} MB`],
          ] as const
        ).map(([k, v]) => (
          <div key={k} className="flex justify-between gap-3 px-3 py-2">
            <dt className={TEXT_2}>{k}</dt>
            <dd className={`text-right font-medium ${TEXT}`}>
              {v}
            </dd>
          </div>
        ))}
      </dl>
    </div>
  );
}
