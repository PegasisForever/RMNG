// A compact BlockNote editor that yields **markdown**, used for the clone dialog's
// new-ticket description. Same engine as the per-clone notes (`NotesEditorView`), so pasting
// and image upload behave identically. This one is a controlled-ish field, though: it reports
// markdown up on every change instead of autosaving to a document.
//
// No network of its own: every edit leaves through `onChange` and image uploads go through
// the injected `uploadFile`. MarkdownEditorContainer supplies the /api/upload implementation,
// and a story supplies a stub.
//
// BlockNote/ProseMirror touch the DOM, so this module is browser-only. MarkdownEditorContainer
// is the lazy-import target (it carries the /api/upload call in the same chunk), which is why
// this module has a named export only: a default export here would invite
// `lazy(() => import(".../MarkdownEditorView"))`, and that mounts an editor with no upload.
import "@blocknote/core/fonts/inter.css";
import "@blocknote/mantine/style.css";

import { BlockNoteView } from "@blocknote/mantine";
import { useCreateBlockNote } from "@blocknote/react";

import { useColorScheme } from "~/lib/useColorScheme";

export interface MarkdownEditorViewProps {
  /** Fires with the document serialized to markdown after every edit. */
  onChange: (markdown: string) => void;
  /** Store a pasted or dropped image and answer with its URL. The URL lands in the markdown
   *  as `![](<url>)`, so whatever this returns is what the ticket body will point at. */
  uploadFile: (file: File) => Promise<string>;
  placeholder?: string;
  className?: string;
}

export function MarkdownEditorView({
  onChange,
  uploadFile,
  placeholder,
  className,
}: MarkdownEditorViewProps) {
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
