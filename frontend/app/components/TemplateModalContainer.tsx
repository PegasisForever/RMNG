// Template-create dialog, network half. Title plus preset, nothing else: always
// headed, always the preset's default accounts, always the last-used (else first)
// template image. Submit files `POST /api/clone` in plain mode (title + preset).
//
// Gen-2 rule: this creates from a TEMPLATE image onto a fresh empty home dataset.
// It never forks (the New clone dialog does that).
//
// Two things live here and nowhere below: the config read that supplies the presets,
// and the operation the POST returns. The dialog stays open on that operation and
// closes only when it settles, which is why the op list is a prop rather than
// something the View could ever have. The markup is TemplateModalView.
import { useCallback, useEffect, useState } from "react";

import { TemplateModalView } from "~/components/TemplateModalView";
import { getConfig, type ClonePayload } from "~/lib/api";
import {
  lastCloneImage,
  preferredCloneImage,
} from "~/lib/lastCloneImage";
import { opPhase } from "~/lib/cloneDraft";
import type { Operation } from "~/lib/types";
import type { ImageInfo } from "~/lib/wire/ImageInfo";
import type { PresetRedacted } from "~/lib/wire/PresetRedacted";

export function TemplateModalContainer({
  images,
  operations,
  onClose,
  onClone,
}: {
  /** Clone-source images to pick from (from `listImages`). */
  images: ImageInfo[];
  /** Live operations from the SSE state — the started clone op is tracked through these. */
  operations: Operation[];
  onClose: () => void;
  /** Starts the clone and resolves with the driving Operation. The dialog stays open,
   *  showing its progress, until the operation settles. */
  onClone: (image: string, payload: ClonePayload) => Promise<Operation>;
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

  // Base image: last one actually cloned from, else the first listed. No picker: the
  // dialog is two fields, and the base rarely changes.
  const image =
    preferredCloneImage(images, lastCloneImage()) ?? images[0]?.reference ?? null;
  const valid =
    image !== null &&
    title.trim().length > 0 &&
    (presets.length === 0 || preset !== "");

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
  const presetImage = presets.find((p) => p.name === preset)?.image ?? null;

  const submit = useCallback(() => {
    if (!valid || busy || !image) return;
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
    onClone(image, payload)
      .then((started) => setOpId(started.id))
      .catch((e: Error) => setError(e.message))
      .finally(() => setStarting(false));
  }, [valid, busy, image, title, preset, onClone]);

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
