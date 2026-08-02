// The upload's two decisions: what Linear's answer means, and which headers the relay has to
// replay. Both were established by watching the bucket refuse, so they are pinned here.
import { expect, test } from "bun:test";

import { putHeaders, uploadTargetFromResponse } from "./upload";

function answer(overrides: Record<string, unknown> = {}): unknown {
  return {
    fileUpload: {
      success: true,
      uploadFile: {
        uploadUrl: "https://storage.googleapis.com/linear-uploads/abc?X-Goog-Expires=60",
        assetUrl: "https://uploads.linear.app/abc/shot.png",
        headers: [{ key: "x-goog-content-length-range", value: "0,2048" }],
        ...overrides,
      },
    },
  };
}

test("a successful fileUpload yields the URL to PUT to, the asset URL, and the signed headers", () => {
  expect(uploadTargetFromResponse(answer())).toEqual({
    uploadUrl: "https://storage.googleapis.com/linear-uploads/abc?X-Goog-Expires=60",
    assetUrl: "https://uploads.linear.app/abc/shot.png",
    headers: [{ key: "x-goog-content-length-range", value: "0,2048" }],
  });
});

test("a refusal, or an answer missing either URL, throws rather than uploading nowhere", () => {
  expect(() => uploadTargetFromResponse({ fileUpload: { success: false } })).toThrow();
  expect(() => uploadTargetFromResponse(null)).toThrow();
  expect(() => uploadTargetFromResponse(answer({ uploadUrl: null }))).toThrow(/uploadUrl/);
  expect(() => uploadTargetFromResponse(answer({ assetUrl: "" }))).toThrow(/assetUrl/);
});

test("a header entry that is not two strings is dropped rather than sent as undefined", () => {
  const headers = [{ key: "x-goog-content-length-range", value: "0,9" }, { key: "bad" }, null];

  expect(uploadTargetFromResponse(answer({ headers })).headers).toEqual([
    { key: "x-goog-content-length-range", value: "0,9" },
  ]);
  expect(uploadTargetFromResponse(answer({ headers: null })).headers).toEqual([]);
});

// The declared content type is signed but is not in Linear's own `headers[]`: it is an
// argument to the mutation. Substituting `application/octet-stream` is a 403.
test("the declared content type leads, and Linear's own headers follow verbatim", () => {
  expect(putHeaders("image/png", [{ key: "x-goog-content-length-range", value: "0,7" }])).toEqual([
    { key: "content-type", value: "image/png" },
    { key: "x-goog-content-length-range", value: "0,7" },
  ]);
});

test("a content type Linear also listed is sent once, not twice", () => {
  const signed = [
    { key: "Content-Type", value: "application/octet-stream" },
    { key: "x-goog-content-length-range", value: "0,7" },
  ];

  expect(putHeaders("image/png", signed)).toEqual([
    { key: "content-type", value: "image/png" },
    { key: "x-goog-content-length-range", value: "0,7" },
  ]);
});

test("a nameless header is dropped, and no signed headers leave the content type alone", () => {
  expect(putHeaders("image/gif", [{ key: "   ", value: "x" }])).toEqual([
    { key: "content-type", value: "image/gif" },
  ]);
  expect(putHeaders("image/gif", [])).toEqual([{ key: "content-type", value: "image/gif" }]);
});
