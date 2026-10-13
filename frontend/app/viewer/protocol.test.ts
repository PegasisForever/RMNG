import { expect, test } from "bun:test";

import { codecFromAnnexB, encodeClipboardData, parseClipboardData, parseVideoHeader } from "./protocol";

test("the video header reads the bytes the server writes", () => {
  // The same bytes as `wire::browser::tests::video_header_layout`.
  const bytes = new Uint8Array([
    0, 1, 1, 2, 3, 4, 5, 6, 7, 8, 0x11, 0x12, 0x13, 0x14, 0x15, 0x16, 0x17, 0x18,
  ]);
  const h = parseVideoHeader(bytes.buffer);
  expect(h.key).toBe(true);
  expect(h.monitorId).toBe(0x01020304);
  expect(h.seq).toBe(0x05060708);
  expect(h.serverUs).toBe(Number(0x1112131415161718n));
  bytes[1] = 0;
  expect(parseVideoHeader(bytes.buffer).key).toBe(false);
});

test("the codec string comes from the SPS, after the access unit delimiter", () => {
  // AUD, then an SPS for constrained baseline (66, flags 0xc0) level 4.0 (40), then an IDR slice.
  const au = new Uint8Array([
    0, 0, 0, 1, 0x09, 0xf0, 0, 0, 0, 1, 0x67, 66, 0xc0, 40, 0x8c, 0, 0, 1, 0x65, 0x88,
  ]);
  expect(codecFromAnnexB(au)).toBe("avc1.42c028");
});

test("a frame without an SPS has no codec string", () => {
  expect(codecFromAnnexB(new Uint8Array([0, 0, 0, 1, 0x41, 0x9a, 0, 0, 1, 0x67, 66, 0, 40]))).toBeNull();
  expect(codecFromAnnexB(new Uint8Array([]))).toBeNull();
});

test("clipboard data matches the server's binary layout, both ways", () => {
  // The same bytes as `wire::browser::tests::clipboard_data_layout`.
  const bytes = new Uint8Array([
    1, 0, 1, 2, 3, 4, 5, 6, 7, 0, 9, ...new TextEncoder().encode("text/html<b>"),
  ]);
  const d = parseClipboardData(bytes.buffer);
  expect(d.serial).toBe(0x0001020304050607);
  expect(d.mimeType).toBe("text/html");
  expect(new TextDecoder().decode(d.bytes)).toBe("<b>");
  expect(new Uint8Array(encodeClipboardData(d))).toEqual(bytes);
  const empty = parseClipboardData(
    encodeClipboardData({ serial: 7, mimeType: "image/png", bytes: new Uint8Array(0) }),
  );
  expect(empty).toEqual({ serial: 7, mimeType: "image/png", bytes: new Uint8Array(0) });
});
