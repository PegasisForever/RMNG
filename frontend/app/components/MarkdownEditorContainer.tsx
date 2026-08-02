// A markdown body's network half. It owns one call, the one that stores a pasted or dropped
// image, and hands the rest straight through to MarkdownEditorView. That one call is the
// whole reason the split exists. A `fetch` in the leaf is a `fetch` in every story of every
// screen the leaf appears on.
//
// Where the image goes depends on where the markdown goes, so the caller says which:
//
// - With a `linearKey`, into Linear, and the markdown gets an `assetUrl`. A body bound for a
//   Linear issue cannot carry `/uploads/<name>`, which resolves on this LAN and nowhere else.
// - Without one, into this server's own store as `![](/uploads/<name>)`. That is the clone
//   dialog's new-ticket field, whose body the server still re-hosts on its way to Linear.
//
// Client-only (BlockNote/ProseMirror touch the DOM), so every caller lazy-imports it and it
// never runs during SSR.
import { MarkdownEditorView } from "~/components/MarkdownEditorView";
import { uploadFile } from "~/lib/api";
import { uploadToLinear } from "~/lib/linear/upload";

export default function MarkdownEditorContainer({
  onChange,
  placeholder,
  className,
  linearKey,
}: {
  /** Fires with the document serialized to markdown after every edit. */
  onChange: (markdown: string) => void;
  placeholder?: string;
  className?: string;
  /** The Linear API key that stores pasted images, when this body is bound for a Linear
   *  issue. Blank or absent stores them here instead. */
  linearKey?: string;
}) {
  const key = (linearKey ?? "").trim();
  return (
    <MarkdownEditorView
      onChange={onChange}
      uploadFile={key === "" ? uploadFile : (file) => uploadToLinear(key, file)}
      placeholder={placeholder}
      className={className}
    />
  );
}
