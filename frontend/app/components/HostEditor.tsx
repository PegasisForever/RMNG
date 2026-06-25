// Per-host BlockNote editor. Client-only (BlockNote/ProseMirror touch the DOM),
// so this module is lazy-imported behind a mount gate in _index.tsx and never
// runs during SSR. Loads the host's document from /api/notes/:id, autosaves
// (debounced) back to it, and uploads pasted/dropped images via /api/upload.
import "@blocknote/core/fonts/inter.css";
import "@blocknote/mantine/style.css";

import type { PartialBlock } from "@blocknote/core";
import { BlockNoteView } from "@blocknote/mantine";
import {
  FormattingToolbar,
  FormattingToolbarController,
  getFormattingToolbarItems,
  useCreateBlockNote,
} from "@blocknote/react";
import { useEffect, useRef, useState } from "react";

async function uploadFile(file: File): Promise<string> {
  const fd = new FormData();
  fd.append("file", file);
  const res = await fetch("/api/upload", { method: "POST", body: fd });
  const data = (await res.json()) as { url?: string; error?: string };
  if (!res.ok || !data.url) throw new Error(data.error ?? "upload failed");
  return data.url;
}

const SAVE_DEBOUNCE_MS = 600;

function Editor({
  hostId,
  initialContent,
}: {
  hostId: string;
  initialContent: PartialBlock[] | undefined;
}) {
  const editor = useCreateBlockNote({
    initialContent: initialContent && initialContent.length ? initialContent : undefined,
    uploadFile,
    // No automatic links. `enablePasteRules: false` stops pasted URLs from being
    // turned into links (also disables markdown-on-paste like **bold**; typing it
    // still works). The `autolink`/`handlePasteLink` plugins (removed below) cover
    // typed URLs and paste-onto-selection. We keep the link mark in the schema so
    // notes that already contain links still load and render.
    _tiptapOptions: { enablePasteRules: false },
  });

  // Drop the ProseMirror plugins that auto-convert typed URLs into links and
  // turn a selection into a link when a URL is pasted over it. Runs after the
  // BlockNoteView child has mounted the editor view.
  useEffect(() => {
    editor._tiptapEditor.unregisterPlugin(["autolink", "handlePasteLink"]);
  }, [editor]);

  // Debounced autosave; flushed immediately when the editor unmounts (host
  // switch) so nothing is lost between hosts.
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
    fetch(`/api/notes/${hostId}`, {
      method: "PUT",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ blocks }),
      keepalive: true,
    }).catch(() => {});
  };

  useEffect(() => () => flush(), []); // eslint-disable-line react-hooks/exhaustive-deps

  return (
    <BlockNoteView
      editor={editor}
      theme="light"
      // Replace the default formatting toolbar with one that omits the "create
      // link" button, so there's no way to add links manually (incl. Ctrl+K).
      formattingToolbar={false}
      onChange={() => {
        pending.current = editor.document;
        if (timer.current) clearTimeout(timer.current);
        timer.current = setTimeout(flush, SAVE_DEBOUNCE_MS);
      }}
    >
      <FormattingToolbarController
        formattingToolbar={() => (
          <FormattingToolbar>
            {getFormattingToolbarItems().filter(
              (item) => item.key !== "createLinkButton",
            )}
          </FormattingToolbar>
        )}
      />
    </BlockNoteView>
  );
}

export default function HostEditor({ hostId }: { hostId: string }) {
  const [initial, setInitial] = useState<"loading" | PartialBlock[] | undefined>(
    "loading",
  );

  useEffect(() => {
    let cancelled = false;
    setInitial("loading");
    fetch(`/api/notes/${hostId}`)
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
  }, [hostId]);

  if (initial === "loading") {
    return <div className="p-6 text-sm text-slate-400">Loading…</div>;
  }
  return <Editor key={hostId} hostId={hostId} initialContent={initial} />;
}
