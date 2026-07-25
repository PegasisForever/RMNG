// Per-clone BlockNote editor. Client-only (BlockNote/ProseMirror touch the DOM),
// so this module is lazy-imported behind a mount gate in _index.tsx and never
// runs during SSR. Loads the clone's document from /api/notes/:id, autosaves
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

import { useColorScheme } from "~/lib/useColorScheme";

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
  cloneId,
  initialContent,
}: {
  cloneId: string;
  initialContent: PartialBlock[] | undefined;
}) {
  // Follow the OS light/dark setting (BlockNote themes via a JS prop, not CSS).
  const scheme = useColorScheme();
  const editor = useCreateBlockNote({
    initialContent: initialContent && initialContent.length ? initialContent : undefined,
    uploadFile,
    // No links, period. `enablePasteRules: false` stops pasted URLs from being
    // turned into links (also disables markdown-on-paste like **bold**; typing it
    // still works). The plugins removed below cover typed URLs, paste-onto-selection
    // and clicks on existing links. We keep the link mark in the schema so notes
    // that already contain links still load; app.css renders them as plain text.
    _tiptapOptions: {
      enablePasteRules: false,
      editorProps: {
        // The link mark's `a[href]` parse rule would re-create links from pasted
        // HTML (browser/VSCode copies) — strip anchors up front, keeping their text.
        transformPastedHTML: (html: string) => {
          const doc = new DOMParser().parseFromString(html, "text/html");
          for (const a of doc.body.querySelectorAll("a")) {
            a.replaceWith(...a.childNodes);
          }
          return doc.body.innerHTML;
        },
      },
    },
  });

  // Drop the ProseMirror plugins that auto-convert typed URLs into links
  // (`autolink`), turn a selection into a link when a URL is pasted over it
  // (`handlePasteLink`), and open legacy links on click (`handleClickLink`).
  // Runs after the BlockNoteView child has mounted the editor view.
  useEffect(() => {
    editor._tiptapEditor.unregisterPlugin([
      "autolink",
      "handlePasteLink",
      "handleClickLink",
    ]);
  }, [editor]);

  // Debounced autosave; flushed immediately when the editor unmounts (clone
  // switch) so nothing is lost between clones.
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
    <BlockNoteView
      editor={editor}
      theme={scheme}
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
  return <Editor key={cloneId} cloneId={cloneId} initialContent={initial} />;
}
