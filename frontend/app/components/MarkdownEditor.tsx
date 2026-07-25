// A compact BlockNote editor that yields **markdown**, used for the clone dialog's
// new-ticket description. Same engine as the per-clone notes (`CloneEditor`), so pasting
// and image upload behave identically — but this one is a controlled-ish field: it reports
// markdown up on every change instead of autosaving to a document.
//
// Client-only (BlockNote/ProseMirror touch the DOM). Import it lazily behind a mount gate.
//
// Images: pasted/dropped files go to `/api/upload` and land in the markdown as
// `![](/uploads/<name>)`. That URL is LAN-only, so the server re-hosts each one in Linear
// (`linear::rehost_markdown_images`) before creating the ticket.
import "@blocknote/core/fonts/inter.css";
import "@blocknote/mantine/style.css";

import { BlockNoteView } from "@blocknote/mantine";
import { useCreateBlockNote } from "@blocknote/react";

import { useColorScheme } from "~/lib/useColorScheme";

async function uploadFile(file: File): Promise<string> {
  const fd = new FormData();
  fd.append("file", file);
  const res = await fetch("/api/upload", { method: "POST", body: fd });
  const data = (await res.json()) as { url?: string; error?: string };
  if (!res.ok || !data.url) throw new Error(data.error ?? "upload failed");
  return data.url;
}

export default function MarkdownEditor({
  onChange,
  placeholder,
  className,
}: {
  /** Fires with the document serialized to markdown after every edit. */
  onChange: (markdown: string) => void;
  placeholder?: string;
  className?: string;
}) {
  const scheme = useColorScheme();
  const editor = useCreateBlockNote({
    uploadFile,
    // `blocksToMarkdownLossy` is async in BlockNote 0.51 despite the sync-looking name in
    // some overloads; await it either way.
    placeholders: placeholder ? { emptyDocument: placeholder } : undefined,
  });

  return (
    <div className={className}>
      <BlockNoteView
        editor={editor}
        theme={scheme}
        onChange={() => {
          Promise.resolve(editor.blocksToMarkdownLossy(editor.document))
            .then(onChange)
            .catch(() => {
              /* a transient serialization failure just skips this keystroke */
            });
        }}
      />
    </div>
  );
}
