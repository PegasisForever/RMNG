// The banner reads the chat routes' plain-text body. These pin the three shapes that body can
// arrive in and cannot reach the operator raw: nothing, several lines, and more than fits.
import { expect, test } from "bun:test";

import { CHAT_ERROR_MAX_CHARS, chatErrorText } from "./chatError";

test("the server's own words are what the banner says", () => {
  expect(chatErrorText("clone 'pega-we-142' is archived; unarchive it first", "chat failed")).toBe(
    "clone 'pega-we-142' is archived; unarchive it first",
  );
});

test("a body with nothing readable in it falls back", () => {
  expect(chatErrorText("", "chat failed")).toBe("chat failed");
  expect(chatErrorText("   \n\t ", "stop failed")).toBe("stop failed");
});

test("a multi-line body collapses to one line", () => {
  expect(chatErrorText("  agent prompt HTTP 502\n\n  connection reset  ", "chat failed")).toBe(
    "agent prompt HTTP 502 connection reset",
  );
});

test("a body past the cap is cut and marked", () => {
  const long = "x".repeat(CHAT_ERROR_MAX_CHARS + 50);
  const out = chatErrorText(long, "chat failed");
  expect(out).toHaveLength(CHAT_ERROR_MAX_CHARS + 1);
  expect(out.endsWith("…")).toBe(true);
  expect(chatErrorText("x".repeat(CHAT_ERROR_MAX_CHARS), "chat failed")).toHaveLength(
    CHAT_ERROR_MAX_CHARS,
  );
});

test("the cut lands between code points, never inside one", () => {
  const out = chatErrorText("a".repeat(CHAT_ERROR_MAX_CHARS - 1) + "🙂🙂", "chat failed");
  expect(out).toBe("a".repeat(CHAT_ERROR_MAX_CHARS - 1) + "🙂…");
});
