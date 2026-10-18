import { expect, test } from "bun:test";

import {
  CLIPBOARD_PIECE_BYTES,
  RateMeter,
  clipboardPieces,
  codecFromAnnexB,
  encodeClipboardPiece,
  parseClipboardPiece,
  parseVideoHeader,
} from "./protocol";

test("the video header reads the bytes the server writes", () => {
  // The same bytes as `wire::browser::tests::video_header_layout`.
  const bytes = new Uint8Array([0, 1, 1, 2, 3, 4, 5, 6, 7, 8]);
  const h = parseVideoHeader(bytes.buffer);
  expect(h.key).toBe(true);
  expect(h.monitorId).toBe(0x01020304);
  expect(h.seq).toBe(0x05060708);
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

test("a clipboard piece matches the server's binary layout, both ways", () => {
  // The same bytes as `wire::browser::tests::clipboard_piece_layout`.
  const bytes = new Uint8Array([
    2, 0, 1, 2, 3, 4, 5, 6, 7, 0, 9, ...new TextEncoder().encode("text/html<b>"),
  ]);
  const d = parseClipboardPiece(bytes.buffer);
  expect(d.serial).toBe(0x0001020304050607);
  expect(d.mimeType).toBe("text/html");
  expect(new TextDecoder().decode(d.bytes)).toBe("<b>");
  expect(d.last).toBe(false);
  expect(new Uint8Array(encodeClipboardPiece(d, false))).toEqual(bytes);
  expect(parseClipboardPiece(encodeClipboardPiece(d, true)).last).toBe(true);
});

test("a clipboard reply goes in pieces, the last one marked; an empty one in one piece", () => {
  const bytes = new Uint8Array(CLIPBOARD_PIECE_BYTES * 2 + 5).fill(7);
  const pieces = clipboardPieces({ serial: 7, mimeType: "image/png", bytes }).map(parseClipboardPiece);
  expect(pieces.map((p) => [p.bytes.length, p.last])).toEqual([
    [CLIPBOARD_PIECE_BYTES, false],
    [CLIPBOARD_PIECE_BYTES, false],
    [5, true],
  ]);
  const empty = clipboardPieces({ serial: 7, mimeType: "image/png", bytes: new Uint8Array(0) });
  expect(empty.map(parseClipboardPiece)).toEqual([
    { serial: 7, mimeType: "image/png", bytes: new Uint8Array(0), last: true },
  ]);
});

test("the rate meter gives the rate of the last complete period", () => {
  const m = new RateMeter(500);
  m.add(1000, 0);
  m.add(1000, 250);
  expect(m.rate).toBe(0);
  m.add(3000, 500);
  expect(m.rate).toBe(10);
  m.add(100, 600);
  expect(m.rate).toBe(10);
  m.add(400, 1000);
  expect(m.rate).toBe(1);
});
