import { expect, test } from "bun:test";

import { makeClone, makeCloneSubClone, makeCloneWorking } from "~/components/__fixtures__/clones";

import { emptyForkDraft, forkParent, forkRequest, forkValid, withoutLetter } from "./forkDraft";

const ticketClone = makeCloneWorking({
  id: "pega-we-142a",
  linearTicketUrl: "https://linear.app/we/issue/WE-142",
  displayName: "Normalize sidebar CPU (a)",
});
const plainClone = makeClone({ id: "pega-scratch", managed: true, displayName: "scratch" });

test("a clone with a ticket forks onto the same ticket, its title without the letter", () => {
  const req = forkRequest(ticketClone, emptyForkDraft(ticketClone));
  expect(req.source).toBe("pega-we-142a");
  expect(req.linear).toEqual({
    workspace: "we",
    ticket: "WE-142",
    ticketUrl: "https://linear.app/we/issue/WE-142",
    branch: undefined,
    displayName: "Normalize sidebar CPU",
    label: "frontend",
  });
  expect(forkValid(ticketClone, emptyForkDraft(ticketClone))).toBe(true);
});

test("a clone with no ticket needs a name, which the fork takes", () => {
  const d = emptyForkDraft(plainClone);
  expect(forkValid(plainClone, d)).toBe(false);
  expect(forkValid(plainClone, { ...d, name: "   " })).toBe(false);
  const named = { ...d, name: " scratch two " };
  expect(forkValid(plainClone, named)).toBe(true);
  expect(forkRequest(plainClone, named).linear).toEqual({ displayName: "scratch two" });
});

test("the fork is a sub clone by default; of a sub clone, it goes under the same parent", () => {
  expect(forkRequest(ticketClone, emptyForkDraft(ticketClone)).parent).toBe("pega-we-142a");
  const sub = makeCloneSubClone();
  expect(sub.parent).toBeTruthy();
  expect(forkParent(sub)).toBe(sub.parent!);
  const top = { ...emptyForkDraft(ticketClone), sub: false };
  expect(forkRequest(ticketClone, top).parent).toBeUndefined();
});

test("a message starts the agent; no message starts nothing", () => {
  const d = emptyForkDraft(ticketClone);
  expect(forkRequest(ticketClone, d)).toMatchObject({ kickoff: false, firstMessage: undefined });
  expect(forkRequest(ticketClone, { ...d, message: "  try the other fix " })).toMatchObject({
    kickoff: true,
    firstMessage: "try the other fix",
  });
});

test("the pool is sent only when changed from the source's", () => {
  const pooled = makeCloneWorking({ group: "pooled" });
  const d = emptyForkDraft(pooled);
  expect(d.group).toBe("pooled");
  expect(forkRequest(pooled, d).group).toBeUndefined();
  expect(forkRequest(pooled, { ...d, group: "none" }).group).toBe("none");
  expect(emptyForkDraft(ticketClone).group).toBe("none");
});

test("only the letter the server adds comes off a title", () => {
  expect(withoutLetter("Fix login (a)")).toBe("Fix login");
  expect(withoutLetter("Fix login (beta)")).toBe("Fix login (beta)");
  expect(withoutLetter("Fix login")).toBe("Fix login");
});
