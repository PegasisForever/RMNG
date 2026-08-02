// The selected ticket's description, editable, on the same BlockNote the clone notes use.
//
// Markdown in, markdown out. Linear stores the body as markdown, so this parses it into
// blocks on mount and serializes back on every edit — which is lossy in exactly the way
// BlockNote's markdown support is lossy, and no worse than pasting the text into Linear's
// own editor.
//
// Saving is debounced, like the notes editor: an edit that never settles should not send a
// mutation per keystroke to somebody else's API.
//
// Images cross a boundary in both directions here. A body from Linear carries
// `https://uploads.linear.app/…` URLs, which answer an unauthenticated GET with 401, so the
// editor is handed the same-origin proxy path instead. A body on its way back to Linear
// carries the original again, because Linear has to keep working for everyone else. The
// `markdown` prop and the `onSave` argument are therefore both in Linear's own form, and the
// proxy exists only inside this editor. See `~/lib/linear/assets`.
//
// Client-only (BlockNote/ProseMirror touch the DOM). Import it lazily behind a mount gate.
import "@blocknote/core/fonts/inter.css";
import "@blocknote/mantine/style.css";

import { BlockNoteView } from "@blocknote/mantine";
import { useCreateBlockNote } from "@blocknote/react";
import { useEffect, useRef, useState } from "react";

import { toLinearMarkdown, toProxyMarkdown } from "~/lib/linear/assets";
import { useColorScheme } from "~/lib/useColorScheme";

/** Long enough that a sentence being typed is one save, short enough that clicking away
 *  immediately after typing has already sent it. The clone notes use the same. */
const SAVE_DEBOUNCE_MS = 600;

export default function TicketDescription({
  markdown,
  onSave,
}: {
  /** The body as Linear stores it. Empty means the ticket has none yet. */
  markdown: string;
  /** Persist the new markdown. Called on the trailing edge of the debounce. */
  onSave: (markdown: string) => void;
}) {
  const scheme = useColorScheme();
  const editor = useCreateBlockNote();
  const timer = useRef<ReturnType<typeof setTimeout> | null>(null);
  // The editor mounts empty and the parse lands in an effect, so it is blank for one frame.
  // Holding it invisible until then keeps a bare block from flashing before the text.
  const [ready, setReady] = useState(false);

  // Load the ticket's body once per ticket. Reloading on every `markdown` change would fight
  // the operator: our own save round-trips back through the state and would then reset the
  // document under a cursor that has moved on.
  useEffect(() => {
    const blocks = editor.tryParseMarkdownToBlocks(toProxyMarkdown(markdown));
    editor.replaceBlocks(editor.document, blocks);
    setReady(true);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [editor]);

  // A pending save must not be lost when the panel closes or the ticket changes.
  useEffect(
    () => () => {
      if (timer.current) clearTimeout(timer.current);
    },
    [],
  );

  return (
    // `ticket-description` is what zeroes BlockNote's own 54px side padding (see app.css).
    // A negative margin would do it too, and that is the trap: it widens the element past
    // its container, which gives the panel a horizontal scrollbar and still leaves the body
    // a few pixels off the title above it. Removing the padding leaves both edges honest.
    <div className={`ticket-description ${ready ? "" : "opacity-0"}`}>
      <BlockNoteView
        editor={editor}
        theme={scheme}
        onChange={() => {
          if (timer.current) clearTimeout(timer.current);
          timer.current = setTimeout(() => {
            onSave(toLinearMarkdown(editor.blocksToMarkdownLossy(editor.document)));
          }, SAVE_DEBOUNCE_MS);
        }}
      />
    </div>
  );
}
