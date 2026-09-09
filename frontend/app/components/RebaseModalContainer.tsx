// Rebase dialog, network half. Target preset plus a rebuild flag, nothing else: the
// home dataset, the id, and the clone's own preset bindings all stay (rebase swaps
// the image only).
//
// Two things live here and nowhere below: the config read that supplies the presets,
// and the operation the POST returns. The dialog stays open on that operation and
// closes only when it settles, which is why the op list is a prop rather than
// something the View could ever have. The markup is RebaseModalView.
import { useCallback, useEffect, useState } from "react";

import { RebaseModalView } from "~/components/RebaseModalView";
import { getConfig } from "~/lib/api";
import { opPhase } from "~/lib/cloneDraft";
import type { Operation } from "~/lib/types";
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
  // The started rebase operation: its id once the POST returns, plus a local error.
  const [opId, setOpId] = useState<string | null>(null);
  const [starting, setStarting] = useState(false);
  const [error, setError] = useState<string | null>(null);

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

  const valid = preset !== "" && (presets.length === 0 || presets.some((p) => p.name === preset));

  // --- operation tracking (same rules as the template dialog) ---------------------------
  // Finished ops are PRUNED from state a few seconds after they land, so an op that
  // disappears having previously been seen counts as done. A sticky failed flag keeps
  // the error message from being closed out from under when the failed op is pruned.
  const op = opId ? operations.find((o) => o.id === opId) : undefined;
  const [opSeen, setOpSeen] = useState(false);
  const [failed, setFailed] = useState(false);
  useEffect(() => {
    if (op) setOpSeen(true);
    if (op?.status === "error") {
      setFailed(true);
      setError(op.message || "the rebase failed");
    }
  }, [op]);
  useEffect(() => {
    if (!opId) return;
    if (opPhase(op, opSeen, failed) === "done") onClose();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [opId, op, opSeen, failed]);

  const busy = starting || (!!opId && !failed);

  const submit = useCallback(() => {
    if (!valid || busy) return;
    // Clear the previous attempt so a retry after a failure tracks the NEW op, not the old
    // failed one (which is still in `operations` for another minute before it's pruned).
    setError(null);
    setOpId(null);
    setOpSeen(false);
    setFailed(false);
    setStarting(true);
    onRebase(preset, rebuild)
      .then((started) => setOpId(started.id))
      .catch((e: Error) => setError(e.message))
      .finally(() => setStarting(false));
  }, [valid, busy, preset, rebuild, onRebase]);

  return (
    <RebaseModalView
      cloneId={cloneId}
      presets={presets}
      preset={preset}
      onPresetChange={setPreset}
      rebuild={rebuild}
      onRebuildChange={setRebuild}
      valid={valid}
      busy={busy}
      error={error}
      operation={op ?? null}
      onSubmit={submit}
      onClose={onClose}
    />
  );
}
