import { describe, expect, test } from "bun:test";

import { makeUsage } from "~/components/__fixtures__/accounts";

import { checkSignIn } from "./signInCheck";

const win = { pct: 10, resetsAt: null };

describe("checkSignIn", () => {
  test("not yet in the list: not working, no problem to name", () => {
    expect(checkSignIn([], "claude", "new@x.com", null)).toEqual({ works: false, oldGone: true, problem: null });
  });

  test("in the list before its first poll (no numbers): not working yet", () => {
    expect(checkSignIn([makeUsage({ email: "new@x.com" })], "claude", "new@x.com", null).works).toBe(false);
  });

  test("fresh numbers, no error: working", () => {
    const list = [makeUsage({ email: "New@x.com", fiveHour: win })];
    expect(checkSignIn(list, "claude", "new@x.com", null).works).toBe(true);
  });

  test("the same account repaired: its old dead row does not count until the poll clears it", () => {
    const dead = makeUsage({ email: "a@x.com", sevenDay: win, assignable: false, stale: true, error: "refresh token rejected" });
    expect(checkSignIn([dead], "claude", "a@x.com", "a@x.com")).toEqual({ works: false, oldGone: true, problem: "refresh token rejected" });
    const fixed = makeUsage({ email: "a@x.com", sevenDay: win });
    expect(checkSignIn([fixed], "claude", "a@x.com", "a@x.com")).toEqual({ works: true, oldGone: true, problem: null });
  });

  test("a replacement waits for the old account to leave the list", () => {
    const fresh = makeUsage({ email: "new@x.com", fiveHour: win });
    const old = makeUsage({ email: "old@x.com", assignable: false });
    expect(checkSignIn([fresh, old], "claude", "new@x.com", "old@x.com")).toMatchObject({ works: true, oldGone: false });
    expect(checkSignIn([fresh], "claude", "new@x.com", "old@x.com")).toMatchObject({ works: true, oldGone: true });
  });

  test("the other provider's account of the same email is not this one", () => {
    const codex = makeUsage({ email: "a@x.com", provider: "codex", sevenDay: win });
    expect(checkSignIn([codex], "claude", "a@x.com", null).works).toBe(false);
  });
});
