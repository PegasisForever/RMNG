// What an error banner says, given the body the control server sent.
//
// The server answers a failure in one of two shapes, and which one it uses depends on the
// handler. Most return axum `(StatusCode, String)`, a plain-text body: "unknown clone
// 'pega-we-142'", "clone 'pega-we-142' is archived; unarchive it first". The six account
// routes answer through `err_json`, which wraps the same sentence as `{"error": "..."}`.
// Reading either shape as the other throws away the one sentence the operator can act on, so
// the shape is settled here and no call site has to know which handler it just called.
//
// The body is not trusted to be presentable either. It can arrive empty, span several lines,
// run long enough to squeeze the page, or be an HTML error page from a proxy that never
// reached the server at all. Deciding what of it a banner can carry happens here too, so the
// containers stay lists of fetches.

/** Longest banner text, in characters. Both banners that show this are plain blocks in a
 *  fixed-height column with nothing else bounding their height: the chat pane's sits between
 *  the thread and the composer, and the shell's sits above the board. */
export const SERVER_ERROR_MAX_CHARS = 300;

/** The banner text for a failed request: the server's own words when it sent any, `fallback`
 *  when it sent nothing readable.
 *
 *  A JSON body carrying a non-blank string `error` reads as that field. Every other body reads
 *  as itself, which is what sends the plain-text routes through unchanged and still shows a
 *  JSON body whose `error` is missing, blank, or not a string.
 *
 *  Whitespace runs collapse to single spaces, so a multi-line body reads as one line rather
 *  than losing its breaks silently in the markup. Anything past `SERVER_ERROR_MAX_CHARS` is cut
 *  and marked with an ellipsis. The cut counts code points, so a body ending in an emoji cannot
 *  be split into half a surrogate pair. */
export function serverErrorText(body: string, fallback: string): string {
  if (isHtmlDocument(body)) return fallback;
  return present(jsonError(body) ?? body, fallback);
}

/** The `error` field of a JSON object body, when it holds one as a non-blank string. Null for
 *  every other body, which is what leaves a plain-text sentence to be read as itself. */
function jsonError(body: string): string | null {
  let parsed: unknown;
  try {
    parsed = JSON.parse(body);
  } catch {
    return null;
  }
  if (typeof parsed !== "object" || parsed === null) return null;
  const field = (parsed as { error?: unknown }).error;
  return typeof field === "string" && field.trim() !== "" ? field : null;
}

/** True when the body is an HTML document rather than anything the server meant as a message.
 *  A reverse proxy that cannot reach the control server answers with its own 502 page, and
 *  that markup is several hundred characters of nothing to act on, so the status line is the
 *  better answer.
 *
 *  Matched on the two prefixes a document actually opens with, so a message that merely
 *  contains a `<` ("unknown clone '<none>'") stays the server's own words. */
function isHtmlDocument(body: string): boolean {
  const head = body.trimStart().slice(0, 14).toLowerCase();
  return head.startsWith("<!doctype html") || head.startsWith("<html");
}

/** One line, capped, or `fallback` when nothing readable is left. */
function present(text: string, fallback: string): string {
  const line = text.replace(/\s+/g, " ").trim();
  if (!line) return fallback;
  const chars = Array.from(line);
  if (chars.length <= SERVER_ERROR_MAX_CHARS) return line;
  return `${chars.slice(0, SERVER_ERROR_MAX_CHARS).join("")}…`;
}
