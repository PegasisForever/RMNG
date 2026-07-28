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

  test("a lone modal owns Escape", () => {
    const only = Symbol("only");
    __pushModal(only);
    expect(ownsEscape(only, true)).toBe(true);
  });

  test("only the topmost modal owns Escape", () => {
    const settings = Symbol("settings");
    const login = Symbol("login");
    __pushModal(settings);
    __pushModal(login); // group-login opens on top of the settings panel

    expect(ownsEscape(login, true)).toBe(true);
    expect(ownsEscape(settings, true)).toBe(false);
  });

  test("the one underneath takes over once the top unmounts", () => {
    const settings = Symbol("settings");
    const login = Symbol("login");
    __pushModal(settings);
    __pushModal(login);
    __popModal(login);

    expect(ownsEscape(settings, true)).toBe(true);
  });

  test("a disabled top modal swallows Escape instead of passing it down", () => {
    const under = Symbol("under");
    const busyClone = Symbol("busy-clone");
    __pushModal(under);
    __pushModal(busyClone);

    // CloneModal mid-operation: it must not close...
    expect(ownsEscape(busyClone, false)).toBe(false);
    // ...and the dialog beneath must not close either.
    expect(ownsEscape(under, true)).toBe(false);
  });

  test("an unmounted modal owns nothing", () => {
    const gone = Symbol("gone");
    __pushModal(gone);
    __popModal(gone);
    expect(ownsEscape(gone, true)).toBe(false);
  });
});
