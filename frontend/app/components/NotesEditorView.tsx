// The notes pane's editor, with no network of its own: the document comes in as
// `initialContent`, every edit leaves through `onChange`, and image uploads go through
// the injected `uploadFile`. NotesEditorContainer supplies the /api/notes and /api/upload
// implementations; a story supplies fixtures and a no-op.
//
// BlockNote/ProseMirror touch the DOM, so this module is browser-only. Import it lazily
// behind a mount gate (see _index.tsx) and never during SSR.
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
import { useEffect } from "react";

import { useColorScheme } from "~/lib/useColorScheme";

export interface NotesEditorViewProps {
  /** The document to open with. Empty or undefined starts a blank note. */
  initialContent: PartialBlock[] | undefined;
  /** Every edit, as the editor's current block list. Debouncing and persistence
   *  belong to the caller. */
  onChange: (blocks: unknown[]) => void;
  /** Store a pasted or dropped image and answer with its URL. */
  uploadFile: (file: File) => Promise<string>;
}

export function NotesEditorView({
  initialContent,
  onChange,
  uploadFile,
}: NotesEditorViewProps) {
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

  return (
    <BlockNoteView
      editor={editor}
      theme={scheme}
      // Replace the default formatting toolbar with one that omits the "create
      // link" button, so there's no way to add links manually (incl. Ctrl+K).
      formattingToolbar={false}
      onChange={() => onChange(editor.document)}
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

export default NotesEditorView;
