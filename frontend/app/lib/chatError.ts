// What the chat pane's error banner says, given the body the server sent.
//
// The four chat routes answer a failure as axum `(StatusCode, String)`, which is a plain-text
// body, never JSON: "unknown clone 'pega-we-142'", "clone 'pega-we-142' is archived; unarchive
// it first". Those words are the whole of what the operator can act on, so the banner shows
// them instead of a fixed per-route string.
//
// The body is not trusted to be presentable, though. It can arrive empty, span several lines,
// or run long enough to squeeze the thread and push the composer out of the pane, because the
// banner is a plain block with no cap of its own. Deciding what of it the banner can carry
// happens here, so the container stays a list of fetches.

/** Longest banner text, in characters. The banner shares a fixed-height column with the thread
 *  and the composer, and nothing else bounds its height. */
export const CHAT_ERROR_MAX_CHARS = 300;

/** The banner text for a failed chat request: the server's own words when it sent any,
 *  `fallback` when it sent nothing readable.
 *
 *  Whitespace runs collapse to single spaces, so a multi-line body reads as one line rather
 *  than losing its breaks silently in the markup. Anything past `CHAT_ERROR_MAX_CHARS` is cut
 *  and marked with an ellipsis. The cut counts code points, so a body ending in an emoji cannot
 *  be split into half a surrogate pair. */
export function chatErrorText(body: string, fallback: string): string {
  const text = body.replace(/\s+/g, " ").trim();
  if (!text) return fallback;
  const chars = Array.from(text);
  if (chars.length <= CHAT_ERROR_MAX_CHARS) return text;
  return `${chars.slice(0, CHAT_ERROR_MAX_CHARS).join("")}…`;
}
