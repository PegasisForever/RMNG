// The ticket column is an inbox of work not started, so the filter has one job: never show
// a ticket somebody already has a clone for.
import { expect, test } from "bun:test";

import {
  branchNameOf,
  cloneForTicket,
  cloneTickets,
  findTicket,
  openTickets,
  orderTickets,
  ticketDragId,
  ticketIdFromDrag,
  type LinearTicket,
} from "./tickets";
import type { Clone } from "~/lib/types";

function ticket(id: string, state: LinearTicket["state"] = "todo"): LinearTicket {
  return { id, title: id, url: `https://linear.app/x/issue/${id}`, state, labels: [], children: [] };
}

function clone(id: string, linearTicket?: string, archived = false): Clone {
  return {
    id,
    host: "10.0.0.1",
    port: 3389,
    username: "pega",
    password: "",
    managed: true,
    ...(linearTicket ? { linearTicket } : {}),
    ...(archived ? { archived } : {}),
  };
}

test("open tickets with no clone are kept, in the given order", () => {
  const tickets = [ticket("WE-1"), ticket("WE-2", "in_progress")];

  expect(openTickets(tickets, [clone("c1")]).map((t) => t.id)).toEqual(["WE-1", "WE-2"]);
});

test("a ticket with a clone is dropped", () => {
  const tickets = [ticket("WE-1"), ticket("WE-2")];

  expect(openTickets(tickets, [clone("c1", "WE-2")]).map((t) => t.id)).toEqual(["WE-1"]);
});

test("an archived clone still counts as claimed", () => {
  const tickets = [ticket("WE-1")];

  expect(openTickets(tickets, [clone("c1", "WE-1", true)])).toEqual([]);
});

test("the ticket id match ignores case", () => {
  const tickets = [ticket("WE-1")];

  expect(openTickets(tickets, [clone("c1", "we-1")])).toEqual([]);
});

test("states past in-progress are dropped whether or not a clone exists", () => {
  const done = { ...ticket("WE-9"), state: "done" as unknown as LinearTicket["state"] };

  expect(openTickets([done], [])).toEqual([]);
});

test("a repeated id is kept once, at its first position", () => {
  const merged = [ticket("WE-1"), ticket("DEV-5"), ticket("WE-1"), ticket("DEV-9")];

  expect(openTickets(merged, []).map((t) => t.id)).toEqual(["WE-1", "DEV-5", "DEV-9"]);
});

test("a drag id round-trips, and a clone's own id is not mistaken for one", () => {
  expect(ticketIdFromDrag(ticketDragId("WE-142"))).toBe("WE-142");
  expect(ticketIdFromDrag("pega-we-142")).toBeNull();
});

// --- the operator's own order ------------------------------------------------

const ids = (list: LinearTicket[]) => list.map((t) => t.id);

test("the stored order wins over Linear's", () => {
  const fromLinear = [ticket("WE-1"), ticket("WE-2"), ticket("WE-3")];

  expect(ids(orderTickets(fromLinear, ["WE-3", "WE-1", "WE-2"]))).toEqual([
    "WE-3",
    "WE-1",
    "WE-2",
  ]);
});

test("a ticket the order has never seen goes to the top", () => {
  const fromLinear = [ticket("WE-1"), ticket("WE-9"), ticket("WE-2")];

  expect(ids(orderTickets(fromLinear, ["WE-1", "WE-2"]))).toEqual(["WE-9", "WE-1", "WE-2"]);
});

test("several new ones keep Linear's order among themselves, above the placed ones", () => {
  const fromLinear = [ticket("WE-8"), ticket("WE-1"), ticket("WE-9")];

  expect(ids(orderTickets(fromLinear, ["WE-1"]))).toEqual(["WE-8", "WE-9", "WE-1"]);
});

test("an id in the order that no longer exists is ignored", () => {
  const fromLinear = [ticket("WE-2")];

  expect(ids(orderTickets(fromLinear, ["WE-1", "WE-2"]))).toEqual(["WE-2"]);
});

test("no stored order at all leaves Linear's order alone", () => {
  const fromLinear = [ticket("WE-2"), ticket("WE-1")];

  expect(ids(orderTickets(fromLinear, []))).toEqual(["WE-2", "WE-1"]);
});

// `PUT /api/tickets/order` lowercases every id before it stores one, so what comes back over
// `/events` never matches a Linear identifier's own case. This is the test that makes that
// safe to do.
test("a stored order in a different case still places the tickets", () => {
  const fromLinear = [ticket("WE-1"), ticket("WE-2"), ticket("WE-3")];

  expect(ids(orderTickets(fromLinear, ["we-3", "we-1", "we-2"]))).toEqual([
    "WE-3",
    "WE-1",
    "WE-2",
  ]);
});

// --- the branch name ----------------------------------------------------------

test("Linear's own branch name is used verbatim when it sent one", () => {
  const t = { ...ticket("WE-1"), branchName: "alex/we-1-fix-the-encoder" };

  expect(branchNameOf(t)).toBe("alex/we-1-fix-the-encoder");
});

test("without one, the id and a slug of the title stand in", () => {
  const t = { ...ticket("WE-301"), title: "Encoder drops frames" };

  expect(branchNameOf(t)).toBe("we-301-encoder-drops-frames");
});

test("punctuation never reaches the branch", () => {
  const t = { ...ticket("DEV-7"), title: "Retry on 429 (don't drop the window!)" };

  expect(branchNameOf(t)).toBe("dev-7-retry-on-429-don-t-drop-the-window");
});

test("a long title is cut on a word boundary", () => {
  const t = {
    ...ticket("WE-9"),
    title: "Normalize the sidebar CPU reading to a percentage of the account allowance",
  };
  const name = branchNameOf(t);

  expect(name.startsWith("we-9-normalize-the-sidebar-cpu-reading-to-a")).toBe(true);
  expect(name.endsWith("-")).toBe(false);
});

test("a title with nothing usable in it leaves the id alone", () => {
  const t = { ...ticket("WE-4"), title: "!!!" };

  expect(branchNameOf(t)).toBe("we-4");
});

test("a referenced issue in the column resolves to that ticket", () => {
  const tickets = [ticket("WE-1"), ticket("WE-2")];

  expect(findTicket("we-2", tickets)?.id).toBe("WE-2");
  expect(findTicket("WE-9", tickets)).toBeNull();
});

test("a referenced issue somebody cloned resolves to that clone", () => {
  const clones = [clone("mercury", "WE-2"), clone("venus")];

  expect(cloneForTicket("we-2", clones)?.id).toBe("mercury");
  expect(cloneForTicket("WE-1", clones)).toBeNull();
});

// --- a clone's own ticket, live ----------------------------------------------
//
// A clone stores its ticket's title and link when it is made and never hears about them
// again, so everything below is about the join that replaces them at render.

test("a clone's live title and link come off the ticket that answered", () => {
  const t = ticket("WE-2");
  t.title = "Renamed in Linear";

  const live = cloneTickets([clone("mercury", "WE-2")], [t]);

  expect(live.mercury).toEqual({ title: "Renamed in Linear", url: t.url });
});

test("the identifier match ignores case, the way every other one does", () => {
  const live = cloneTickets([clone("mercury", "we-2")], [ticket("WE-2")]);

  expect(live.mercury?.title).toBe("WE-2");
});

test("a clone with no ticket, and one Linear did not answer for, are both absent", () => {
  const clones = [clone("mercury", "WE-9"), clone("venus")];

  const live = cloneTickets(clones, [ticket("WE-2")]);

  expect(live).toEqual({});
});
