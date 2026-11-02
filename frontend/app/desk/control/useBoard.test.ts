// The board's lists, driven through the real step function the control panel uses: a change
// shows at once and goes out, a frame with a new server copy replaces it, a clone being made
// is filed in the frame that first has it.
import { expect, test } from "bun:test";

import type { BoardColumn } from "~/lib/board";
import type { Clone } from "~/lib/types";

import {
  type BoardEvent,
  type BoardFrame,
  boardFrom,
  type BoardState,
  boardStore,
  type BoardWrite,
  shown,
  stepBoard,
} from "./useBoard";

const clone = (id: string, archived = false): Clone => ({ id, host: "10.99.0.1", managed: true, archived });

const COLUMNS: BoardColumn[] = [
  { id: "todo", title: "Todo", cloneIds: ["a", "b"], archive: false },
  { id: "done", title: "Done", cloneIds: [], archive: true },
];
const CLONES = [clone("a"), clone("b")];

const frame = (over: Partial<BoardFrame> = {}): BoardFrame => ({
  clones: CLONES,
  columns: COLUMNS,
  ticketOrder: ["we-1"],
  muted: [],
  ...over,
});

/** The state after `events`, and every write they sent, in order. */
function run(events: BoardEvent[], from: BoardState = boardFrom(frame())): { s: BoardState; sent: BoardWrite[] } {
  const sent: BoardWrite[] = [];
  const s = events.reduce((s, e) => {
    const step = stepBoard(s, e);
    sent.push(...step.send);
    return step.state;
  }, from);
  return { s, sent };
}

const ids = (columns: BoardColumn[], id: string) => columns.find((c) => c.id === id)?.cloneIds;

test("no stored columns draws the defaults", () => {
  const s = boardFrom(frame({ columns: [] }));
  expect(shown(s.columns).map((c) => c.id)).toEqual(["clones", "archived"]);
});

test("a change shows at once and goes to the server", () => {
  const { s, sent } = run([{ type: "renameColumn", columnId: "todo", title: "Next" }]);
  expect(shown(s.columns)[0].title).toBe("Next");
  expect(sent).toEqual([{ to: "columns", value: shown(s.columns) }]);
  // The server's copy is still the old one
  expect(s.columns.server[0].title).toBe("Todo");
});

test("a frame with a new server copy replaces the local one", () => {
  const { s } = run([
    { type: "toggleMute", cloneId: "a" },
    { type: "frame", ...frame({ muted: ["b"] }) },
  ]);
  expect(shown(s.muted)).toEqual(["b"]);
  expect(s.muted.local).toBeNull();
});

test("a frame whose lists did not change keeps the local change, and the same state", () => {
  const { s } = run([{ type: "toggleMute", cloneId: "a" }]);
  expect(shown(s.muted)).toEqual(["a"]);
  // A new clone list (every frame has one) is not a new server copy of the lists
  const step = stepBoard(s, { type: "frame", ...frame({ clones: [...CLONES] }) });
  expect(step.state).toBe(s);
  expect(step.send).toEqual([]);
});

test("a drop into an archive column moves the card and archives the clone", () => {
  const { s, sent } = run([{ type: "moveClone", clones: CLONES, cloneId: "a", columnId: "done" }]);
  expect(ids(shown(s.columns), "done")).toEqual(["a"]);
  expect(sent).toEqual([
    { to: "columns", value: shown(s.columns) },
    { to: "archive", cloneId: "a", archived: true, before: COLUMNS },
  ]);
});

test("a drop out of an archive column restores the clone; a move between plain columns calls nothing else", () => {
  const archived = [clone("a"), clone("b"), clone("c", true)];
  const from = boardFrom(frame({ clones: archived, columns: [...COLUMNS.slice(0, 1), { ...COLUMNS[1], cloneIds: ["c"] }] }));
  const out = run([{ type: "moveClone", clones: archived, cloneId: "c", columnId: "todo", at: 0 }], from);
  expect(ids(shown(out.s.columns), "todo")).toEqual(["c", "a", "b"]);
  expect(out.sent[1]).toMatchObject({ to: "archive", cloneId: "c", archived: false });

  const plain = run([{ type: "moveInColumn", clones: CLONES, cloneId: "b", columnId: "todo", move: "up" }]);
  expect(ids(shown(plain.s.columns), "todo")).toEqual(["b", "a"]);
  expect(plain.sent.map((w) => w.to)).toEqual(["columns"]);
});

test("a clone being made is filed into its column in the frame that first has it", () => {
  const filing = run([{ type: "file", cloneId: "n", columnId: "todo", clones: CLONES }]);
  // Not made yet: nothing moves
  expect(filing.sent).toEqual([]);
  const made = [...CLONES, clone("n")];
  const { s, sent } = run([{ type: "frame", ...frame({ clones: made }) }], filing.s);
  expect(ids(shown(s.columns), "todo")).toEqual(["n", "a", "b"]);
  expect(sent).toEqual([{ to: "columns", value: shown(s.columns) }]);
  expect(s.filing).toBeNull();
  // Once only
  expect(run([{ type: "frame", ...frame({ clones: [...made] }) }], s).sent).toEqual([]);
});

test("a clone that is already there when it is filed is filed at once", () => {
  const made = [...CLONES, clone("n")];
  const { s, sent } = run([{ type: "file", cloneId: "n", columnId: "todo", clones: made }]);
  expect(ids(shown(s.columns), "todo")?.[0]).toBe("n");
  expect(sent).toHaveLength(1);
});

test("a ticket drawn for the first time takes its place in the order, once", () => {
  const { s, sent } = run([{ type: "adoptTickets", drawn: [{ id: "WE-9" }, { id: "WE-1" }] }]);
  expect(shown(s.ticketOrder)).toEqual(["we-9", "we-1"]);
  expect(sent).toEqual([{ to: "ticketOrder", value: ["we-9", "we-1"], adopted: true }]);
  // The column asks on every render: nothing new is the same state back
  const again = stepBoard(s, { type: "adoptTickets", drawn: [{ id: "WE-9" }, { id: "WE-1" }] });
  expect(again.state).toBe(s);
  expect(again.send).toEqual([]);
});

// --- refused: the server's copy comes back ---------------------------------------------

/** Each kind of change, and the list it shows on. */
const CHANGES: [BoardEvent, (s: BoardState) => unknown][] = [
  [{ type: "moveInColumn", clones: CLONES, cloneId: "b", columnId: "todo", move: "up" }, (s) => shown(s.columns)],
  [{ type: "orderTickets", ids: ["we-2", "we-1"] }, (s) => shown(s.ticketOrder)],
  [{ type: "toggleMute", cloneId: "a" }, (s) => shown(s.muted)],
];

test("a refused write puts the server's copy back, which no frame would do", () => {
  for (const [change, list] of CHANGES) {
    const start = boardFrom(frame());
    const { s, sent } = run([change], start);
    expect(list(s)).not.toEqual(list(start));
    const back = run([{ type: "refused", write: sent[0] }], s).s;
    expect(list(back)).toEqual(list(start));
  }
});

test("a refused write leaves a later change alone", () => {
  const first = run([{ type: "toggleMute", cloneId: "a" }]);
  const second = run([{ type: "toggleMute", cloneId: "b" }], first.s);
  // The first is refused while the second is on its way: the second still shows
  const s = run([{ type: "refused", write: first.sent[0] }], second.s).s;
  expect(shown(s.muted)).toEqual(["a", "b"]);
});

test("a refused archive puts the columns back, on the server too", () => {
  const drop = run([{ type: "moveClone", clones: CLONES, cloneId: "a", columnId: "done" }]);
  const { s, sent } = run([{ type: "refused", write: drop.sent[1] }], drop.s);
  expect(shown(s.columns)).toEqual(COLUMNS);
  expect(sent).toEqual([{ to: "columns", value: COLUMNS }]);
});

test("a refused adoption is not sent again on the next render, but a new ticket tries again", () => {
  const drawn = [{ id: "WE-9" }, { id: "WE-1" }];
  const adopt = run([{ type: "adoptTickets", drawn }]);
  const s = run([{ type: "refused", write: adopt.sent[0] }], adopt.s).s;
  expect(shown(s.ticketOrder)).toEqual(["we-1"]);
  expect(stepBoard(s, { type: "adoptTickets", drawn }).send).toEqual([]);
  expect(stepBoard(s, { type: "adoptTickets", drawn: [{ id: "WE-7" }, ...drawn] }).send).toHaveLength(1);
});

test("the store sends through the server calls it is given, and rolls back what they refuse", async () => {
  const calls: string[] = [];
  const errors: (string | null)[] = [];
  const refuse = (what: string) => (v: unknown) => {
    calls.push(`${what} ${JSON.stringify(v)}`);
    return Promise.reject(new Error(`no ${what}`));
  };
  const store = boardStore(
    frame(),
    {
      putColumns: refuse("columns"),
      putTicketOrder: refuse("order"),
      putMuted: refuse("muted"),
      archive: refuse("archive"),
      unarchive: refuse("unarchive"),
    },
    (e) => errors.push(e),
  );
  let renders = 0;
  store.subscribe(() => (renders += 1));

  store.dispatch({ type: "toggleMute", cloneId: "a" });
  expect(shown(store.get().muted)).toEqual(["a"]);
  await Bun.sleep(0);
  expect(calls).toEqual(['muted ["a"]']);
  expect(errors).toEqual(["no muted"]);
  expect(shown(store.get().muted)).toEqual([]);
  expect(renders).toBe(2);
});
