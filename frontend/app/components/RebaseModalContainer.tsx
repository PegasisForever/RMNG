// Rebase dialog, network half. Target preset plus a rebuild flag, nothing else: the
// home dataset, the id, and the clone's own preset bindings all stay (rebase swaps
// the image only).
//
// Two things live here and nowhere below: the config read that supplies the presets,
// and the operation the POST returns. The dialog stays open on that operation and
// closes only when it settles, which is why the op list is a prop rather than
// something the View could ever have. Following that op is `~/lib/useOperation`, the
// same rules the clone dialog and the settings panel run on. The markup is
// RebaseModalView.
import { useEffect, useState } from "react";

import { RebaseModalView } from "~/components/RebaseModalView";
import { getConfig } from "~/lib/api";
import type { Operation } from "~/lib/types";
import { useOperation } from "~/lib/useOperation";
import type { PresetRedacted } from "~/lib/wire/PresetRedacted";

export function RebaseModalContainer({
  cloneId,
  currentPreset,
  operations,
  onClose,
  onRebase,
}: {
  /** The clone being rebased. */
  cloneId: string;
  /** The clone's own preset binding, preselected (rebase never changes it). */
  currentPreset: string | null;
  /** Live operations from the SSE state — the started rebase op is tracked through these. */
  operations: Operation[];
  onClose: () => void;
  /** Starts the rebase and resolves with the driving Operation. The dialog stays open,
   *  showing its progress, until the operation settles. */
  onRebase: (preset: string, rebuild: boolean) => Promise<Operation>;
}) {
  const [presets, setPresets] = useState<PresetRedacted[]>([]);
  const [preset, setPreset] = useState(currentPreset ?? "");
  const [rebuild, setRebuild] = useState(false);
  // The started rebase, from the POST to the frame that settles it. `start` is read afresh
  // on every run, so it carries the preset and the checkbox as they stand at the click.
  const rebase = useOperation(operations, () => onRebase(preset, rebuild), {
    failureLabel: "the rebase failed",
  });

  useEffect(() => {
    getConfig()
      .then((c) => {
        setPresets(c.presets);
        // Preselect the clone's own preset when it exists, else the first preset.
        setPreset((p) => p || c.presets[0]?.name || "");
      })
      .catch(() => {
        // Config unreachable — just no preset options.
      });
  }, []);

  const valid =
    preset !== "" &&
    (presets.length === 0 || presets.some((p) => p.name === preset));

  /** The Rebase button. Whether the form is ready is this module's question; whether an
   *  attempt is already running is the operation's, and `run` answers that one itself. */
  function submit() {
    if (valid) rebase.run();
  }

  return (
    <RebaseModalView
      cloneId={cloneId}
      presets={presets}
      preset={preset}
      onPresetChange={setPreset}
      rebuild={rebuild}
      onRebuildChange={setRebuild}
      valid={valid}
      busy={rebase.busy}
      error={rebase.error}
      operation={rebase.op ?? null}
      onSubmit={submit}
      // The settled operation is what closes the dialog: `open` goes false, the frame plays
      // its exit, and `onClose` unmounts once the frames have run. A FAILED rebase is not
      // settled for this purpose — the dialog stays up holding its error.
      open={!rebase.done}
      onClose={onClose}
    />
  );
}
