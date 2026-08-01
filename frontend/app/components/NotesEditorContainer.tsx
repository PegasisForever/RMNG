// Per-clone notes: the network half. Loads the clone's document from /api/notes/:id, autosaves
// it back (debounced), and hands the editor the /api/upload call for pasted images. Every one
// of those is a thing a story cannot have, which is why they live here and nothing below
// knows about any of them. The markup is NotesEditorView, which takes the document and the
// callbacks as props and is the half Storybook renders.
//
// Client-only (BlockNote/ProseMirror touch the DOM), so it is lazy-imported behind a mount
// gate in _index.tsx and MobileDashboard.tsx, and never runs during SSR.
import type { PartialBlock } from "@blocknote/core";
import { useEffect, useRef, useState } from "react";

import { NotesEditorView } from "~/components/NotesEditorView";
import { uploadFile } from "~/lib/api";

const SAVE_DEBOUNCE_MS = 600;

/** Debounced autosave, flushed on unmount (clone switch) so a pending edit is sent before
 *  the editor goes.
 *
 *  Send is all it is. A flush drops `pending` before the request goes out and never looks at
 *  the answer: `res.ok` is not checked, so a 500 resolves and vanishes, and the `.catch`
 *  below swallows a network failure the same way. Nothing retries and nothing tells the
 *  operator, so an edit can be lost with the pane still reading as saved. */
function SavingEditor({
  cloneId,
  initialContent,
}: {
  cloneId: string;
  initialContent: PartialBlock[] | undefined;
}) {
  const pending = useRef<unknown[] | null>(null);
  const timer = useRef<ReturnType<typeof setTimeout> | null>(null);

  const flush = () => {
    if (timer.current) {
      clearTimeout(timer.current);
      timer.current = null;
    }
    if (pending.current === null) return;
    const blocks = pending.current;
    pending.current = null;
    fetch(`/api/notes/${cloneId}`, {
      method: "PUT",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ blocks }),
      keepalive: true,
      // Failure is dropped on purpose here, and by omission above: see the note on
      // SavingEditor. Do not read this as "the save succeeded".
    }).catch(() => {});
  };

  useEffect(() => () => flush(), []); // eslint-disable-line react-hooks/exhaustive-deps

  return (
    <NotesEditorView
      initialContent={initialContent}
      uploadFile={uploadFile}
      onChange={(blocks) => {
        pending.current = blocks;
        if (timer.current) clearTimeout(timer.current);
        timer.current = setTimeout(flush, SAVE_DEBOUNCE_MS);
      }}
    />
  );
}

export default function NotesEditorContainer({ cloneId }: { cloneId: string }) {
  const [initial, setInitial] = useState<"loading" | PartialBlock[] | undefined>(
    "loading",
  );

  useEffect(() => {
    let cancelled = false;
    setInitial("loading");
    fetch(`/api/notes/${cloneId}`)
      .then((r) => r.json())
      .then((d: { blocks?: unknown }) => {
        if (cancelled) return;
        setInitial(Array.isArray(d.blocks) ? (d.blocks as PartialBlock[]) : undefined);
      })
      .catch(() => {
        if (!cancelled) setInitial(undefined);
      });
    return () => {
      cancelled = true;
    };
  }, [cloneId]);

  if (initial === "loading") {
    return <div className="p-6 text-sm text-slate-400 dark:text-slate-500">Loading…</div>;
  }
  return <SavingEditor key={cloneId} cloneId={cloneId} initialContent={initial} />;
}
