import { describe, expect, test } from "bun:test";

import { type RecognitionResult, TakeText, joinPiece, replaces, textToKeysyms } from "./dictation";

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

const sure = (transcript: string): RecognitionResult => ({
  isFinal: true,
  0: { transcript },
});
const unsure = (transcript: string): RecognitionResult => ({
  isFinal: false,
  0: { transcript },
});

/** The text on the clone after each event's edit. */
function typedAfter(take: TakeText, events: RecognitionResult[][]): string[] {
  let text: string[] = [];
  return events.map((results) => {
    const { erase, type } = take.update(results);
    text = [...text.slice(0, text.length - erase), ...type];
    return text.join("");
  });
}

describe("TakeText as the spec says (Safari, desktop Chrome)", () => {
  test("each sure result is typed once, with a space between", () => {
    const take = new TakeText(false);
    expect(
      typedAfter(take, [
        [unsure("hel")],
        [sure("hello")],
        [sure("hello"), unsure("wor")],
        [sure("hello"), sure(" world")],
      ]),
    ).toEqual(["", "hello", "hello", "hello world"]);
  });

  test("the same words said two times are typed two times", () => {
    expect(typedAfter(new TakeText(false), [[sure("yes")], [sure("yes"), sure("yes")]])).toEqual(["yes", "yes yes"]);
  });

  test("no Backspace goes, even when a sure result changes", () => {
    const take = new TakeText(false);
    take.update([sure("hello")]);
    expect(take.update([sure("help")])).toEqual({ erase: 0, type: " help" });
  });
});

describe("TakeText on Chrome for Android", () => {
  test("one result that grows at index 0 is typed once", () => {
    expect(
      typedAfter(new TakeText(true), [
        [sure("can")],
        [sure("can you")],
        [sure("can you see")],
        [sure("can you see this")],
      ]),
    ).toEqual(["can", "can you", "can you see", "can you see this"]);
  });

  test("a new result that grows the one before, at each new index, is typed once", () => {
    expect(
      typedAfter(new TakeText(true), [
        [sure("can you")],
        [sure("can you"), sure("can you see")],
        [sure("can you"), sure("can you see"), sure("can you see this")],
      ]),
    ).toEqual(["can you", "can you see", "can you see this"]);
  });

  test("the same results again in each event change nothing", () => {
    const take = new TakeText(true);
    take.update([sure("hello world")]);
    expect(take.update([sure("hello world")])).toEqual({ erase: 0, type: "" });
    expect(take.update([sure("hello world"), sure("hello world")])).toEqual({
      erase: 0,
      type: "",
    });
  });

  test("a change of case is fixed, and is no new utterance", () => {
    const take = new TakeText(true);
    take.update([sure("hello world")]);
    expect(take.update([sure("hello world"), sure("Hello world.")])).toEqual({
      erase: 11,
      type: "Hello world.",
    });
  });

  test("a changed last word goes back and is typed again", () => {
    const take = new TakeText(true);
    take.update([sure("why exactly is")]);
    expect(take.update([sure("why exactly is"), sure("why exactly it's")])).toEqual({ erase: 1, type: "t's" });
  });

  test("a second utterance comes after the first", () => {
    expect(
      typedAfter(new TakeText(true), [
        [sure("hello world")],
        [sure("hello world"), sure("how")],
        [sure("hello world"), sure("how are you")],
      ]),
    ).toEqual(["hello world", "hello world how", "hello world how are you"]);
  });

  test("when the list begins again, what was typed stays", () => {
    expect(typedAfter(new TakeText(true), [[sure("hello world")], [sure("how are")], [sure("how are you")]])).toEqual([
      "hello world",
      "hello world how are",
      "hello world how are you",
    ]);
  });

  test("an earlier utterance does not change", () => {
    const take = new TakeText(true);
    take.update([sure("hello world"), sure("how are")]);
    expect(take.update([sure("yellow world"), sure("how are")])).toEqual({
      erase: 0,
      type: " yellow world how are",
    });
  });

  test("words not sure yet are not typed", () => {
    expect(typedAfter(new TakeText(true), [[unsure("can you")], [sure("can you"), unsure("can you see")]])).toEqual([
      "",
      "can you",
    ]);
  });
});

describe("replaces", () => {
  test("the same, grown, or with a changed end", () => {
    expect(replaces("yes", "Yes.")).toBe(true);
    expect(replaces("no", "no no")).toBe(true);
    expect(replaces("why exactly is", "why exactly it's")).toBe(true);
  });

  test("another utterance", () => {
    expect(replaces("the cat", "the dog")).toBe(false);
    expect(replaces("I went home", "I like it")).toBe(false);
    expect(replaces("hello world", "how are you")).toBe(false);
    expect(replaces("can you see this", "can you")).toBe(false);
  });
});
