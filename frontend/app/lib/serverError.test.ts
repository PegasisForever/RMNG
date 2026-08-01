// A banner reads whatever body the failed request came back with. These pin the two shapes the
// server sends (plain text and `{"error"}` JSON) and the four a body can arrive in that cannot
// reach the operator raw: nothing, several lines, more than fits, and a proxy's HTML page.
import { expect, test } from "bun:test";

import { SERVER_ERROR_MAX_CHARS, serverErrorText } from "./serverError";

test("the server's own words are what the banner says", () => {
  expect(serverErrorText("clone 'pega-we-142' is archived; unarchive it first", "chat failed")).toBe(
    "clone 'pega-we-142' is archived; unarchive it first",
  );
});

test("a body with nothing readable in it falls back", () => {
  expect(serverErrorText("", "chat failed")).toBe("chat failed");
  expect(serverErrorText("   \n\t ", "stop failed")).toBe("stop failed");
});

test("a multi-line body collapses to one line", () => {
  expect(serverErrorText("  agent prompt HTTP 502\n\n  connection reset  ", "chat failed")).toBe(
    "agent prompt HTTP 502 connection reset",
  );
});

test("a body past the cap is cut and marked", () => {
  const long = "x".repeat(SERVER_ERROR_MAX_CHARS + 50);
  const out = serverErrorText(long, "chat failed");
  expect(out).toHaveLength(SERVER_ERROR_MAX_CHARS + 1);
  expect(out.endsWith("…")).toBe(true);
  expect(serverErrorText("x".repeat(SERVER_ERROR_MAX_CHARS), "chat failed")).toHaveLength(
    SERVER_ERROR_MAX_CHARS,
  );
});

test("the cut lands between code points, never inside one", () => {
  const out = serverErrorText("a".repeat(SERVER_ERROR_MAX_CHARS - 1) + "🙂🙂", "chat failed");
  expect(out).toBe("a".repeat(SERVER_ERROR_MAX_CHARS - 1) + "🙂…");
});

// --- the JSON shape, as `err_json` writes it -------------------------------

test("a JSON error body reads as its error field, not as its markup", () => {
  expect(
    serverErrorText(
      '{"error":"account pega@example.com is pinned to clone pega-we-142"}',
      "Bad Request",
    ),
  ).toBe("account pega@example.com is pinned to clone pega-we-142");
});

test("a JSON error field is collapsed and capped like any other body", () => {
  expect(serverErrorText('{"error":"docker daemon\\n  connection refused"}', "Bad Gateway")).toBe(
    "docker daemon connection refused",
  );
  const long = JSON.stringify({ error: "y".repeat(SERVER_ERROR_MAX_CHARS + 10) });
  const out = serverErrorText(long, "Bad Request");
  expect(out).toHaveLength(SERVER_ERROR_MAX_CHARS + 1);
  expect(out.endsWith("…")).toBe(true);
});

test("a JSON body with no usable error field is shown as it arrived", () => {
  // Absent, not a string, blank, and not an object at all. None of these carries a sentence, so
  // the body itself is the most the operator can be told without inventing one.
  expect(serverErrorText('{"ok":false}', "Bad Request")).toBe('{"ok":false}');
  expect(serverErrorText('{"error":42}', "Bad Request")).toBe('{"error":42}');
  expect(serverErrorText('{"error":null}', "Bad Request")).toBe('{"error":null}');
  expect(serverErrorText('{"error":"   "}', "Bad Request")).toBe('{"error":" "}');
  expect(serverErrorText('["error"]', "Bad Request")).toBe('["error"]');
});

test("a plain-text body that is not JSON is never mistaken for one", () => {
  expect(serverErrorText("reference is required", "Bad Request")).toBe("reference is required");
  expect(serverErrorText("unknown clone '<none>'", "Bad Request")).toBe("unknown clone '<none>'");
});

// --- a body from something that is not the control server ------------------

test("a proxy's HTML page falls back to the status line", () => {
  const nginx =
    "<html>\r\n<head><title>502 Bad Gateway</title></head>\r\n<body>\r\n" +
    "<center><h1>502 Bad Gateway</h1></center>\r\n<hr><center>nginx/1.24.0</center>\r\n" +
    "</body>\r\n</html>\r\n";
  expect(serverErrorText(nginx, "Bad Gateway")).toBe("Bad Gateway");
  expect(
    serverErrorText('<!DOCTYPE html>\n<html lang="en"><body>503</body></html>', "Service Unavailable"),
  ).toBe("Service Unavailable");
  expect(
    serverErrorText('  \n<!doctype html public "-//IETF//DTD HTML 2.0//EN">', "Bad Gateway"),
  ).toBe("Bad Gateway");
});

test("a sentence that merely starts with a bracket is not an HTML page", () => {
  expect(serverErrorText("<none> is not a valid image reference", "Bad Request")).toBe(
    "<none> is not a valid image reference",
  );
});
