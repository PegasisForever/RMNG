import { expect, test } from "bun:test";

import {
  CLIPBOARD_PIECE_BYTES,
  RateMeter,
  annexbNals,
  annexbToAvcc,
  avcDescription,
  clipboardPieces,
  codecOf,
  encodeClipboardPiece,
  parseAudioHeader,
  parseClipboardPiece,
  parseVideoHeader,
  sameBytes,
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

// An IDR access unit as the encoder writes it: AUD (3-byte start code), SPS for constrained
// baseline (66, flags 0xc0) level 4.0 (40), PPS, then the IDR slice, each with a 4-byte start code.
const IDR = new Uint8Array([
  0, 0, 1, 0x09, 0xf0, 0, 0, 0, 1, 0x67, 66, 0xc0, 40, 0x8c, 0, 0, 0, 1, 0x68, 0xce, 0x3c, 0x80,
  0, 0, 0, 1, 0x65, 0x88, 0x84,
]);

test("the NAL units of an access unit, without start codes or the zero in front of one", () => {
  expect(annexbNals(IDR).map((n) => [...n])).toEqual([
    [0x09, 0xf0],
    [0x67, 66, 0xc0, 40, 0x8c],
    [0x68, 0xce, 0x3c, 0x80],
    [0x65, 0x88, 0x84],
  ]);
  expect(annexbNals(new Uint8Array([]))).toEqual([]);
});

test("the avcC description holds the SPS and PPS, and names the codec", () => {
  const d = avcDescription(IDR)!;
  expect([...d]).toEqual([
    1, 66, 0xc0, 40, 0xff, 0xe1, 0, 5, 0x67, 66, 0xc0, 40, 0x8c, 1, 0, 4, 0x68, 0xce, 0x3c, 0x80,
  ]);
  expect(codecOf(d)).toBe("avc1.42c028");
  // A delta frame has no SPS.
  expect(avcDescription(new Uint8Array([0, 0, 0, 1, 0x41, 0x9a]))).toBeNull();
});

test("an access unit as length-prefixed NAL units keeps only the slices", () => {
  expect([...annexbToAvcc(IDR)]).toEqual([0, 0, 0, 3, 0x65, 0x88, 0x84]);
  const delta = new Uint8Array([0, 0, 0, 1, 0x09, 0xf0, 0, 0, 0, 1, 0x41, 0x9a, 0, 0, 1, 0x41, 7]);
  expect([...annexbToAvcc(delta)]).toEqual([0, 0, 0, 2, 0x41, 0x9a, 0, 0, 0, 2, 0x41, 7]);
});

test("byte arrays compare by content", () => {
  expect(sameBytes(new Uint8Array([1, 2]), new Uint8Array([1, 2]))).toBe(true);
  expect(sameBytes(new Uint8Array([1, 2]), new Uint8Array([1, 3]))).toBe(false);
  expect(sameBytes(new Uint8Array([1]), null)).toBe(false);
  expect(sameBytes(null, null)).toBe(true);
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

test("the audio header reads the bytes the server writes", () => {
  // The same bytes as `wire::browser::tests::audio_message_layout`.
  const bytes = new Uint8Array([3, 1, 2, 3, 4, 5, 6, 7, 8, 9, 9]);
  expect(parseAudioHeader(bytes.buffer)).toEqual({ stream: 0x01020304, seq: 0x05060708 });
});
