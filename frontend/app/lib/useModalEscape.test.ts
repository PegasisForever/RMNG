// The stacking rule is the whole point of `useModalEscape`: with modals owning plain
// `window` listeners, one Escape closed every open modal at once. These cases drive the
// real `ownsEscape` against the real module-level stack.
import { beforeEach, describe, expect, test } from "bun:test";

import {
  __popModal,
  __pushModal,
  __resetModalEscapeStack,
  ownsEscape,
} from "./useModalEscape";

describe("modal Escape stacking", () => {
  beforeEach(() => __resetModalEscapeStack());

  test("only the topmost modal owns Escape", () => {
    const dialog = Symbol("dialog");
    const dropdown = Symbol("dropdown");
    __pushModal(dialog);
    __pushModal(dropdown); // a dropdown opens inside the dialog

    expect(ownsEscape(dropdown, true)).toBe(true);
    expect(ownsEscape(dialog, true)).toBe(false);
  });

  test("the one underneath takes over once the top unmounts", () => {
    const dialog = Symbol("dialog");
    const dropdown = Symbol("dropdown");
    __pushModal(dialog);
    __pushModal(dropdown);
    __popModal(dropdown);

    expect(ownsEscape(dialog, true)).toBe(true);
  });

  test("a disabled top modal swallows Escape instead of passing it down", () => {
    const under = Symbol("under");
    const busyClone = Symbol("busy-clone");
    __pushModal(under);
    __pushModal(busyClone);

    // The clone dialog mid-operation: it must not close...
    expect(ownsEscape(busyClone, false)).toBe(false);
    // ...and the dialog beneath must not close either.
    expect(ownsEscape(under, true)).toBe(false);
  });
});
