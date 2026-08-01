// The agent thread and its queue, so the chat pane renders without the per-clone SSE stream.
//
// `chatNow` and `chatLocale` are the clock and the locale the view is given, so the
// scheduled-message labels ("Today 15:00") come out the same on every load and on every
// machine. Without the locale the same story reads "Today 03:00 PM" under en-US and
// "Today 15:00" under en-GB.

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
    text: "Both are open side by side. The metric row is 4px tighter in the mockup and the token counts sit on the second line rather than the first.",
    ts: chatNow - 8 * 60_000,
  }),
  makeChatMessage({
    id: "m3",
    text: "Make the running dashboard match, then take a screenshot.",
    ts: chatNow - 2 * 60_000,
  }),
];

/** The agent's current tool line, shown under the working bubble while a turn is in flight. */
export const chatActivity = "Bash(bun run build)";

/** What a failed send puts in the banner, all of it. The chat routes answer an error
 *  as a plain-text body, but the send path parses that body as JSON and reads `.error` off the
 *  result, so the parse always fails and the fallback string is all the operator ever sees.
 *  The server's own words ("clone 'pega-we-142' is archived") are discarded on the way. */
export const chatError = "chat failed";

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
