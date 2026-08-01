// The clone dialog's new-ticket description field: the network half. It owns one call, the
// /api/upload that stores a pasted or dropped image, and hands the rest straight through to
// MarkdownEditorView. That one call is the whole reason the split exists. A `fetch` in the
// leaf is a `fetch` in every story of every screen the leaf appears on.
//
// Images land in the markdown as `![](/uploads/<name>)`. That URL is LAN-only, so the server
// re-hosts each one in Linear (`linear::rehost_markdown_images`) before creating the ticket.
//
// Client-only (BlockNote/ProseMirror touch the DOM), so CloneModal lazy-imports it and it
// never runs during SSR.
import { MarkdownEditorView } from "~/components/MarkdownEditorView";
import { uploadFile } from "~/lib/api";

export default function MarkdownEditorContainer({
  onChange,
  placeholder,
  className,
}: {
  /** Fires with the document serialized to markdown after every edit. */
  onChange: (markdown: string) => void;
  placeholder?: string;
  className?: string;
}) {
  return (
    <MarkdownEditorView
      onChange={onChange}
      uploadFile={uploadFile}
      placeholder={placeholder}
      className={className}
    />
  );
}
