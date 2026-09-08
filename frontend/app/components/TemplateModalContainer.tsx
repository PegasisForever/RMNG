// Template-create dialog, network half. Title plus preset, nothing else: always
// headed, always the preset's default accounts, always the preset's Dockerfile (built
// lazily at create). Submit files `POST /api/clone` in plain mode (title + preset).
//
// Gen-2 rule: this creates from a preset Dockerfile onto a fresh empty home dataset.
// It never forks (the New clone dialog does that).
//
// Two things live here and nowhere below: the config read that supplies the presets,
// and the operation the POST returns. The dialog stays open on that operation and
// closes only when it settles, which is why the op list is a prop rather than
// something the View could ever have. The markup is TemplateModalView.
import { useCallback, useEffect, useState } from "react";

import { TemplateModalView } from "~/components/TemplateModalView";
import { getConfig, type ClonePayload } from "~/lib/api";
import { opPhase } from "~/lib/cloneDraft";
import type { Operation } from "~/lib/types";
import type { PresetRedacted } from "~/lib/wire/PresetRedacted";

/** First FROM line of a Dockerfile, for the read-only base display. */
function fromLine(dockerfile: string): string | null {
  for (const line of dockerfile.split("\n")) {
    const m = line.trim().match(/^FROM\s+(\S+)/i);
    if (m) return m[1];
  }
  return null;
}

export function TemplateModalContainer({
  operations,
  onClose,
  onClone,
}: {
  /** Live operations from the SSE state — the started clone op is tracked through these. */
  operations: Operation[];
  onClose: () => void;
  /** Starts the clone and resolves with the driving Operation. The dialog stays open,
   *  showing its progress, until the operation settles. */
  onClone: (payload: ClonePayload) => Promise<Operation>;
}) {
  const [title, setTitle] = useState("");
  const [presets, setPresets] = useState<PresetRedacted[]>([]);
  const [preset, setPreset] = useState("");
  // The started clone operation: its id once the POST returns, plus a local error.
  const [opId, setOpId] = useState<string | null>(null);
  const [starting, setStarting] = useState(false);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    getConfig()
      .then((c) => {
        setPresets(c.presets);
        setPreset((p) => p || c.presets[0]?.name || "");
      })
      .catch(() => {
        // Config unreachable — just no preset options.
      });
  }, []);

  const valid =
    title.trim().length > 0 && (presets.length === 0 || preset !== "");

  // --- operation tracking ---------------------------------------------------------------
  // Once started, follow the op through the SSE frames and close only when it settles.
  // Finished ops are PRUNED from state a few seconds after they land, so an op that
  // disappears having previously been seen counts as done — the same rule the CLI's waiter
  // uses, and the reason a slow SSE frame can't strand the dialog open forever.
  const op = opId ? operations.find((o) => o.id === opId) : undefined;
  const [opSeen, setOpSeen] = useState(false);
  // Sticky: an op that errored has SETTLED. Without this the vanish-means-done rule above
  // would fire when the failed op is pruned (60s later) and close the dialog out from under
  // the error message.
  const [failed, setFailed] = useState(false);
  useEffect(() => {
    if (op) setOpSeen(true);
    if (op?.status === "error") {
      setFailed(true);
      setError(op.message || "the clone failed");
    }
  }, [op]);
  useEffect(() => {
    if (!opId) return;
    if (opPhase(op, opSeen, failed) === "done") onClose();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [opId, op, opSeen, failed]);

  const busy = starting || (!!opId && !failed);
  const picked = presets.find((p) => p.name === preset);
  const presetImage = picked ? fromLine(picked.dockerfile) : null;

  const submit = useCallback(() => {
    if (!valid || busy) return;
    // Clear the previous attempt so a retry after a failure tracks the NEW op, not the old
    // failed one (which is still in `operations` for another minute before it's pruned).
    setError(null);
    setOpId(null);
    setOpSeen(false);
    setFailed(false);
    setStarting(true);
    const payload: ClonePayload = {
      plain: { title: title.trim(), message: "" },
      ...(preset ? { preset } : {}),
    };
    onClone(payload)
      .then((started) => setOpId(started.id))
      .catch((e: Error) => setError(e.message))
      .finally(() => setStarting(false));
  }, [valid, busy, title, preset, onClone]);

  return (
    <TemplateModalView
      title={title}
      onTitleChange={setTitle}
      presets={presets}
      preset={preset}
      onPresetChange={setPreset}
      presetImage={presetImage}
      valid={valid}
      busy={busy}
      error={error}
      operation={op ?? null}
      onSubmit={submit}
      onClose={onClose}
    />
  );
}
