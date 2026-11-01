import { describe, expect, test } from "bun:test";

import { joinPiece, textToKeysyms } from "./dictation";

describe("textToKeysyms", () => {
  test("ASCII and Latin-1 are their own keysyms, a new line is Return, the rest Unicode keysyms", () => {
    expect(textToKeysyms("Hi, é\n")).toEqual([0x48, 0x69, 0x2c, 0x20, 0xe9, 0xff0d]);
    expect(textToKeysyms("€")).toEqual([0x1000000 + 0x20ac]);
    expect(textToKeysyms("😀")).toEqual([0x1000000 + 0x1f600]);
  });
});

describe("joinPiece", () => {
  test("a space goes between two pieces of speech", () => {
    expect(joinPiece("", "hello")).toBe("hello");
    expect(joinPiece("hello", "world")).toBe(" world");
  });

  test("no second space, and none before punctuation", () => {
    expect(joinPiece("hello ", "world")).toBe("world");
    expect(joinPiece("hello", " world")).toBe(" world");
    expect(joinPiece("hello", ", world")).toBe(", world");
  });
});
