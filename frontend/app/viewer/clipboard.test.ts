import { expect, test } from "bun:test";

import { localTypeFor, offerMimes, pickMimes } from "./clipboard";

test("a clone's offer: PNG, HTML and the best plain text, as Chrome types", () => {
  // What a Chromium app in the clone offers for a copied web page part with an image.
  const mimes = [
    "text/html",
    "TEXT",
    "STRING",
    "UTF8_STRING",
    "text/plain",
    "text/plain;charset=utf-8",
    "image/png",
    "image/jpeg",
  ];
  expect(pickMimes(mimes)).toEqual([
    { mime: "image/png", type: "image/png" },
    { mime: "text/html", type: "text/html" },
    { mime: "text/plain;charset=utf-8", type: "text/plain" },
  ]);
  // An X11 app with only the old names.
  expect(pickMimes(["TARGETS", "UTF8_STRING", "STRING"])).toEqual([
    { mime: "UTF8_STRING", type: "text/plain" },
  ]);
  // Nothing Chrome can write.
  expect(pickMimes(["image/jpeg", "application/x-gtk-text-buffer-rich-text"])).toEqual([]);
});

test("local content is offered under the names Linux apps ask for, and served back", () => {
  const mimes = offerMimes(["image/png", "text/html", "text/plain"]);
  expect(mimes).toEqual([
    "image/png",
    "text/html",
    "text/plain;charset=utf-8",
    "UTF8_STRING",
    "text/plain",
  ]);
  expect(mimes.map(localTypeFor)).toEqual([
    "image/png",
    "text/html",
    "text/plain",
    "text/plain",
    "text/plain",
  ]);
  expect(localTypeFor("image/jpeg")).toBeNull();
});
