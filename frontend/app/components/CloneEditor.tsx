// Per-clone notes container. Loads the clone's document from /api/notes/:id, autosaves
// (debounced) back to it, and uploads pasted/dropped images via /api/upload. The editor
// itself is NotesEditor; this module is the network half.
//
// Client-only (BlockNote/ProseMirror touch the DOM), so it is lazy-imported behind a
// mount gate in _index.tsx and never runs during SSR.
import type { PartialBlock } from "@blocknote/core";
import { useEffect, useRef, useState } from "react";

import { NotesEditor } from "~/components/NotesEditor";

async function uploadFile(file: File): Promise<string> {
  const fd = new FormData();
  fd.append("file", file);
  const res = await fetch("/api/upload", { method: "POST", body: fd });
  const data = (await res.json()) as { url?: string; error?: string };
  if (!res.ok || !data.url) throw new Error(data.error ?? "upload failed");
  return data.url;
}

const SAVE_DEBOUNCE_MS = 600;

/** Debounced autosave; flushed immediately when the editor unmounts (clone switch)
 *  so nothing is lost between clones. */
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
    }).catch(() => {});
  };

  useEffect(() => () => flush(), []); // eslint-disable-line react-hooks/exhaustive-deps

  return (
    <NotesEditor
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

export default function CloneEditor({ cloneId }: { cloneId: string }) {
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
