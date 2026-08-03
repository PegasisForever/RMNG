// A compact BlockNote editor that yields **markdown**, used for the description of a ticket
// that does not exist yet: the clone dialog's new-ticket tab and the board's New ticket dialog.
// Same engine as the per-clone notes (`NotesEditorView`), so pasting and image upload behave
// identically, and the same metrics as `TicketDescription`, so a body reads the same size
// before and after the issue is opened. This one is a controlled-ish field, though: it reports
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
  // Theme, not data. It is the one dependency the pure-UI rules keep out of props: a global
  // the preview supplies for every story at once, through the `.dark` class its theme toolbar
  // already sets. `useColorScheme` reads that class as well as the OS media query, so the
  // toolbar rethemes this editor along with everything around it, and there is nothing for a
  // container to inject that the decorator does not already control.
  const scheme = useColorScheme();
  const editor = useCreateBlockNote({
    uploadFile,
    // `blocksToMarkdownLossy` is async in BlockNote 0.51 despite the sync-looking name in
    // some overloads; await it either way.
    placeholders: placeholder ? { emptyDocument: placeholder } : undefined,
  });

  return (
    // `ticket-description` is the metrics every ticket body is edited at: 14px text, headings
    // capped under it, and none of BlockNote's own 54px side padding (see app.css). It is set
    // here rather than asked of each caller because every body this editor holds is a ticket's,
    // and the one already in Linear is edited at the same size in `TicketDescription`. The
    // caller's own class still rides along, for whatever else it wants to say about the box.
    <div className={`ticket-description ${className ?? ""}`}>
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
