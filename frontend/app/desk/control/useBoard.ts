// The board's three lists the server stores: the columns (which clone is in which, in what
// order), the ticket order and the muted clones. Each is shown here at once and sent to the
// server; the server's next frame is the copy that holds from then on.
//
// The rules, in one place:
//   1. A change shows at once (the local copy) and goes to the server in the same step.
//   2. A frame whose copy of a list is new replaces the local copy of that list.
//   3. A drop into an archive column archives the clone, a drop out of one restores it; if
//      that call is refused, the columns go back to what they were.
//   4. A clone being made into a column is filed there in the frame that first has it.
//   5. A ticket the column draws for the first time takes its place in the order
//      (`adoptTickets`).
//
// The rules are a pure step function (`stepBoard`): an event in, the next state and the
// writes to send out. `boardStore` sends them through the server calls it is given, so a
// test passes fakes; `useBoard` is a thin reading of the store.
import { useEffect, useRef, useState, useSyncExternalStore } from "react";

import type { BoardColumnsSettings } from "~/components/SettingsPanes";
import { archiveClone, putBoardColumns, putMutedClones, putTicketOrder, unarchiveClone } from "~/lib/api";
import {
  archivesOnDrop,
  type BoardColumn,
  columnIdOf,
  moveCard,
  newColumnId,
  removeColumn,
  resolveColumns,
  withDefaults,
} from "~/lib/board";
import { toggleMuted } from "~/lib/mute";
import { adoptTickets } from "~/lib/tickets";
import type { Clone } from "~/lib/types";

import { type Move, moved } from "./order";

/** The server calls the board makes. `SERVER` is the real set; a test passes fakes. */
export interface BoardServer {
  putColumns: (columns: BoardColumn[]) => Promise<unknown>;
  putTicketOrder: (ids: string[]) => Promise<unknown>;
  putMuted: (ids: string[]) => Promise<unknown>;
  archive: (cloneId: string) => Promise<unknown>;
  unarchive: (cloneId: string) => Promise<unknown>;
}

export const SERVER: BoardServer = {
  putColumns: putBoardColumns,
  putTicketOrder,
  putMuted: putMutedClones,
  archive: archiveClone,
  unarchive: unarchiveClone,
};

/** One list the server stores, as this window has it. */
export interface Synced<T> {
  /** The server's copy, from the last frame that changed it. */
  server: T;
  /** That frame's list as JSON: a frame changes the list only when this changes. */
  key: string;
  /** This window's change, shown until a frame brings a new server copy. */
  local: T | null;
}

/** What the board holds between frames. */
export interface BoardState {
  /** The columns, with the defaults when the server stores none (`withDefaults`). */
  columns: Synced<BoardColumn[]>;
  ticketOrder: Synced<string[]>;
  muted: Synced<string[]>;
  /** A clone being made, and the column it goes into once a frame has it. */
  filing: { cloneId: string; columnId: string } | null;
}

/** A frame of the server's state, the parts the board reads. */
export interface BoardFrame {
  clones: Clone[];
  columns: BoardColumn[];
  ticketOrder: string[];
  muted: string[];
}

/** One call to the server. */
export type BoardWrite =
  | { to: "columns"; value: BoardColumn[] }
  | { to: "ticketOrder"; value: string[] }
  | { to: "muted"; value: string[] }
  /** Archive (or restore) a clone that was dropped into (or out of) an archive column;
   *  `before`: the columns before the drop. */
  | { to: "archive"; cloneId: string; archived: boolean; before: BoardColumn[] };

export type BoardEvent =
  | ({ type: "frame" } & BoardFrame)
  | { type: "moveClone"; clones: Clone[]; cloneId: string; columnId: string; at?: number }
  | { type: "moveInColumn"; clones: Clone[]; cloneId: string; columnId: string; move: Move }
  | { type: "addColumn"; title: string }
  | { type: "renameColumn"; columnId: string; title: string }
  | { type: "setColumnArchive"; columnId: string; archive: boolean }
  | { type: "deleteColumn"; columnId: string }
  | { type: "reorderColumns"; columnIds: string[] }
  | { type: "orderTickets"; ids: string[] }
  /** The tickets the column draws now, in the order it draws them. */
  | { type: "adoptTickets"; drawn: { id: string }[] }
  | { type: "toggleMute"; cloneId: string }
  /** File the clone being made into `columnId` once it exists. */
  | { type: "file"; cloneId: string; columnId: string; clones: Clone[] };

export interface BoardStep {
  state: BoardState;
  /** The calls to make, in order. */
  send: BoardWrite[];
}

const synced = <T>(raw: T, value: T = raw): Synced<T> => ({ server: value, key: JSON.stringify(raw), local: null });

/** The board as the first frame has it. */
export function boardFrom(frame: BoardFrame): BoardState {
  return {
    columns: synced(frame.columns, withDefaults(frame.columns)),
    ticketOrder: synced(frame.ticketOrder),
    muted: synced(frame.muted),
    filing: null,
  };
}

/** What the list shows: this window's change, or the server's copy. */
export const shown = <T>(s: Synced<T>): T => s.local ?? s.server;

/** The server's copy, if this frame's is new; the same list back otherwise. */
function echo<T>(s: Synced<T>, raw: T, value: T = raw): Synced<T> {
  const key = JSON.stringify(raw);
  return key === s.key ? s : { server: value, key, local: null };
}

const local = <T>(s: Synced<T>, value: T): Synced<T> => ({ ...s, local: value });

/** The columns changed by the operator: shown now, sent. */
function setColumns(s: BoardState, value: BoardColumn[]): BoardStep {
  return { state: { ...s, columns: local(s.columns, value) }, send: [{ to: "columns", value }] };
}

/** The clone being made, filed into its column if `clones` has it. */
function fileIfThere(s: BoardState, clones: Clone[]): BoardStep {
  const f = s.filing;
  if (!f || !clones.some((c) => c.id === f.cloneId)) return { state: s, send: [] };
  const filed = { ...s, filing: null };
  const columns = shown(s.columns);
  if (columnIdOf(columns, f.cloneId) === f.columnId) return { state: filed, send: [] };
  return setColumns(filed, moveCard(columns, f.cloneId, f.columnId, 0));
}

const nothing = (s: BoardState): BoardStep => ({ state: s, send: [] });

export function stepBoard(s: BoardState, e: BoardEvent): BoardStep {
  const columns = shown(s.columns);
  switch (e.type) {
    case "frame": {
      const next = {
        ...s,
        columns: echo(s.columns, e.columns, withDefaults(e.columns)),
        ticketOrder: echo(s.ticketOrder, e.ticketOrder),
        muted: echo(s.muted, e.muted),
      };
      const changed = next.columns !== s.columns || next.ticketOrder !== s.ticketOrder || next.muted !== s.muted;
      return fileIfThere(changed ? next : s, e.clones);
    }
    case "moveClone": {
      const resolved = resolveColumns(columns, e.clones);
      const at = e.at ?? resolved.find((c) => c.id === e.columnId)?.cloneIds.length ?? 0;
      const step = setColumns(s, moveCard(resolved, e.cloneId, e.columnId, at));
      const clone = e.clones.find((c) => c.id === e.cloneId);
      const archived = archivesOnDrop(columns, e.columnId);
      if (!clone || archived === (clone.archived === true)) return step;
      return { ...step, send: [...step.send, { to: "archive", cloneId: e.cloneId, archived, before: columns }] };
    }
    case "moveInColumn": {
      const resolved = resolveColumns(columns, e.clones);
      const ids = resolved.find((c) => c.id === e.columnId)?.cloneIds ?? [];
      const next = moved(ids, e.cloneId, e.move);
      return setColumns(s, moveCard(resolved, e.cloneId, e.columnId, next.indexOf(e.cloneId)));
    }
    case "addColumn":
      return setColumns(s, [...columns, { id: newColumnId(e.title, columns), title: e.title, cloneIds: [], archive: false }]);
    case "renameColumn":
      return setColumns(s, columns.map((c) => (c.id === e.columnId ? { ...c, title: e.title } : c)));
    case "setColumnArchive":
      return setColumns(s, columns.map((c) => (c.id === e.columnId ? { ...c, archive: e.archive } : c)));
    case "deleteColumn":
      return setColumns(s, removeColumn(columns, e.columnId));
    case "reorderColumns":
      return setColumns(s, e.columnIds.flatMap((id) => columns.filter((c) => c.id === id)));
    case "orderTickets":
      return { state: { ...s, ticketOrder: local(s.ticketOrder, e.ids) }, send: [{ to: "ticketOrder", value: e.ids }] };
    case "adoptTickets": {
      // The same state back when there is nothing to adopt: the column asks on every render
      const adopted = adoptTickets(e.drawn, shown(s.ticketOrder));
      if (!adopted) return nothing(s);
      return { state: { ...s, ticketOrder: local(s.ticketOrder, adopted) }, send: [{ to: "ticketOrder", value: adopted }] };
    }
    case "toggleMute": {
      const value = toggleMuted(shown(s.muted), e.cloneId);
      return { state: { ...s, muted: local(s.muted, value) }, send: [{ to: "muted", value }] };
    }
    case "file":
      return fileIfThere({ ...s, filing: { cloneId: e.cloneId, columnId: e.columnId } }, e.clones);
  }
}

/** The board's state, the steps that change it and the calls they make. Each settled call
 *  reports to `report`: null when it went through, the server's words when it was refused. */
export interface BoardStore {
  get: () => BoardState;
  subscribe: (listener: () => void) => () => void;
  dispatch: (e: BoardEvent) => void;
}

export function boardStore(first: BoardFrame, server: BoardServer, report: (error: string | null) => void): BoardStore {
  let state = boardFrom(first);
  const listeners = new Set<() => void>();
  const call = (w: BoardWrite): Promise<unknown> => {
    switch (w.to) {
      case "columns":
        return server.putColumns(w.value);
      case "ticketOrder":
        return server.putTicketOrder(w.value);
      case "muted":
        return server.putMuted(w.value);
      case "archive":
        return w.archived ? server.archive(w.cloneId) : server.unarchive(w.cloneId);
    }
  };
  const dispatch = (e: BoardEvent) => {
    const step = stepBoard(state, e);
    if (step.state !== state) {
      state = step.state;
      for (const l of listeners) l();
    }
    for (const w of step.send) {
      call(w).then(
        () => report(null),
        (err: Error) => report(err.message),
      );
    }
  };
  return {
    get: () => state,
    subscribe: (l) => {
      listeners.add(l);
      return () => listeners.delete(l);
    },
    dispatch,
  };
}

/** What the control panel draws of the board, and what it can ask of it. */
export interface Board {
  /** The columns as stored, with the defaults when there are none. */
  columns: BoardColumn[];
  /** The columns as drawn: every live clone that is not a sub clone, in its column. */
  lanes: { id: string; cloneIds: string[] }[];
  ticketOrder: string[];
  mutedClones: string[];
  /** Into a column at `at` (the end without it); into an archive column archives the clone,
   *  out of one restores it. */
  moveClone: (cloneId: string, columnId: string, at?: number) => void;
  moveInColumn: (cloneId: string, columnId: string, move: Move) => void;
  renameColumn: (columnId: string, title: string) => void;
  orderTickets: (ids: string[]) => void;
  /** Give each ticket the column draws for the first time its place in the order. */
  adoptTickets: (drawn: { id: string }[]) => void;
  toggleMute: (cloneId: string) => void;
  /** File the clone being made into `columnId` in the frame that first has it. */
  fileNewClone: (cloneId: string, columnId: string) => void;
  /** The Settings tab's Board category. */
  settings: BoardColumnsSettings;
}

/** The board of the live state. `onError`: as `boardStore`'s `report`. */
export function useBoard(
  frame: BoardFrame,
  { onError, server = SERVER }: { onError: (error: string | null) => void; server?: BoardServer },
): Board {
  const report = useRef(onError);
  report.current = onError;
  const [store] = useState(() => boardStore(frame, server, (e) => report.current(e)));
  const s = useSyncExternalStore(store.subscribe, store.get, store.get);
  const { dispatch } = store;
  const { clones } = frame;

  // Keyed on the lists' JSON: a frame is a fresh object each time
  const columnsKey = JSON.stringify(frame.columns);
  const orderKey = JSON.stringify(frame.ticketOrder);
  const mutedKey = JSON.stringify(frame.muted);
  useEffect(() => {
    dispatch({ type: "frame", ...frame });
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [clones, columnsKey, orderKey, mutedKey]);

  const columns = shown(s.columns);
  const lanes = resolveColumns(columns, clones);
  return {
    columns,
    lanes: lanes.map((c) => ({ id: c.id, cloneIds: c.cloneIds })),
    ticketOrder: shown(s.ticketOrder),
    mutedClones: shown(s.muted),
    moveClone: (cloneId, columnId, at) => dispatch({ type: "moveClone", clones, cloneId, columnId, at }),
    moveInColumn: (cloneId, columnId, move) => dispatch({ type: "moveInColumn", clones, cloneId, columnId, move }),
    renameColumn: (columnId, title) => dispatch({ type: "renameColumn", columnId, title }),
    orderTickets: (ids) => dispatch({ type: "orderTickets", ids }),
    adoptTickets: (drawn) => dispatch({ type: "adoptTickets", drawn }),
    toggleMute: (cloneId) => dispatch({ type: "toggleMute", cloneId }),
    fileNewClone: (cloneId, columnId) => dispatch({ type: "file", cloneId, columnId, clones }),
    settings: {
      boardColumns: columns,
      boardColumnCounts: Object.fromEntries(lanes.map((c) => [c.id, c.cloneIds.length])),
      onAddBoardColumn: (title) => dispatch({ type: "addColumn", title }),
      onRenameBoardColumn: (columnId, title) => dispatch({ type: "renameColumn", columnId, title }),
      onSetBoardColumnArchive: (columnId, archive) => dispatch({ type: "setColumnArchive", columnId, archive }),
      onDeleteBoardColumn: (columnId) => dispatch({ type: "deleteColumn", columnId }),
      onReorderBoardColumns: (columnIds) => dispatch({ type: "reorderColumns", columnIds }),
    },
  };
}
