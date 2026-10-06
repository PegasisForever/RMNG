// The assistant thread and its queue, so the chat pane renders without the per-clone SSE stream.
//
// `chatNow` and `chatLocale` are the clock and the locale the view is given, so the
// scheduled-message labels ("Today 15:00") come out the same on every load and on every
// machine. Without the locale the same story reads "Today 03:00 PM" under en-US and
// "Today 15:00" under en-GB.

import { serverErrorText } from "~/lib/serverError";
import type { ChatMessage } from "~/lib/types";
import type { ScheduledMessage } from "~/lib/wire/ScheduledMessage";

export const chatNow = new Date(2026, 6, 27, 14, 5).getTime();

/** A 24-hour English locale, so the labels read "Today 15:00" and "Tue 10:05" the way the
 *  code's own examples say they do. Built from local wall-clock parts, `chatNow` also renders
 *  the same under every TZ: the constructor and the formatter read the same local frame. */
export const chatLocale = "en-GB";

export function makeChatMessage(overrides: Partial<ChatMessage> = {}): ChatMessage {
  return {
    id: "m1",
    role: "user",
    text: "Open the sidebar mockup in Figma.",
    ts: chatNow - 60_000,
    ...overrides,
  };
}

export const chatMessages: ChatMessage[] = [
  makeChatMessage({
    id: "m1",
    text: "Open the sidebar mockup in Figma and compare it against the running dashboard.",
    ts: chatNow - 9 * 60_000,
  }),
  makeChatMessage({
    id: "m2",
    role: "assistant",
    // The assistant answers in markdown; the pane renders it.
    text:
      "Both are open side by side. Two differences:\n\n" +
      "- The metric row is **4px tighter** in the mockup.\n" +
      "- The token counts sit on the second line, not the first.\n\n" +
      "The dashboard styles are in `frontend/app/components/CloneCard.tsx`.",
    ts: chatNow - 8 * 60_000,
  }),
  makeChatMessage({
    id: "m3",
    text: "Make the running dashboard match, then take a screenshot.",
    ts: chatNow - 2 * 60_000,
  }),
];

/** The assistant's current tool line, shown under the working bubble while a turn is in flight. */
export const chatActivity = "⚙ bash: rmng --server http://10.0.0.129:9000 desktop pega-we-142 screenshot";

/** What an HTTP failure of the SEND route puts in the banner: the server's own sentence,
 *  verbatim. All four chat routes answer an error as a plain-text body (axum
 *  `(StatusCode, String)`), and the container reads the body as text, so this is what a clone
 *  archived in another tab looks like when a send lands on it.
 *
 *  One banner carries every failure the pane can have, and the other three routes fill it the
 *  same way. A send also reports "unknown clone 'pega-we-142'", a stop reports only this
 *  archived line, a schedule adds "scheduled time must be in the future", and a cancel reports
 *  "invalid clone id 'pega-we-142'" (a 404 there means the message already went, which is not
 *  worth a banner). Each route has a fallback behind it for a body that arrives empty: "chat
 *  failed", "stop failed", "schedule failed", "cancel failed". Two more strings never come from
 *  a route at all. A fetch that does not reach the server surfaces the browser's rejection
 *  ("Failed to fetch"), and a delivery time in the past is refused locally with "Pick a time in
 *  the future." */
export const chatError = "clone 'pega-we-142' is archived; unarchive it first";

/** A failure whose body runs past what the banner can hold, as the container hands it over:
 *  through `serverErrorText`, so the story shows the real cut rather than a hand-shortened
 *  string. None of the four routes writes a sentence this long, but the body is whatever
 *  answered the request, and the banner has no height limit of its own. */
export const chatErrorLong = serverErrorText(
  "502 Bad Gateway from the reverse proxy in front of the control-server: upstream " +
    "http://127.0.0.1:9000/api/chat/pega-we-142 closed the connection before sending a " +
    "response header (connection reset by peer); the proxy retried 3 times over 12s and " +
    "gave up. Check the control-server's own logs for the reason it went away.",
  "chat failed",
);

/** Unsent composer text, for the states where the box is not empty: a send that failed and
 *  put the text back, or a message waiting on a delivery time. */
export const chatDraft = "Roll the sub-clone activity up to the parent card.";

export function makeScheduledMessage(overrides: Partial<ScheduledMessage> = {}): ScheduledMessage {
  return {
    id: "s1",
    at: BigInt(chatNow + 55 * 60_000),
    createdAt: BigInt(chatNow - 3 * 60_000),
    text: "Re-run the visual diff once the build lands.",
    ...overrides,
  };
}

/** Queued for later delivery, soonest first. */
export const scheduledMessages: ScheduledMessage[] = [
  makeScheduledMessage(),
  makeScheduledMessage({
    id: "s2",
    at: BigInt(chatNow + 20 * 3600_000),
    text: "Summarize what changed in the sidebar and post it in the notes.",
  }),
];
