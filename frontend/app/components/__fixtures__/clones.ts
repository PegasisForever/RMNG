// The fleet every clone-facing story draws from. Each named clone covers one distinct visual
// state, so a story picks the state it wants instead of hand-building a row.
//
// `makeClone` carries only the fields every clone has. Account, source, ticket and container
// fields are deliberately left out of the base: an unmanaged clone binds none of them, and a
// base that handed them out would put the unmanaged and tokenless states out of reach.

import type { Clone } from "~/lib/types";

/** A bare clone row: addressable, unmanaged, idle. Every other clone fixture layers on it. */
export function makeClone(overrides: Partial<Clone> = {}): Clone {
  return {
    id: "pega-clone-1",
    host: "10.99.0.10",
    port: 3389,
    username: "pega",
    password: "",
    monitorState: "idle",
    ...overrides,
  };
}

/** A managed clone actively working, pinned to one Claude account, on a ticket. */
export function makeCloneWorking(overrides: Partial<Clone> = {}): Clone {
  return makeClone({
    id: "pega-we-142",
    host: "10.99.0.11",
    managed: true,
    source: "pegasis0/rmng-template:latest",
    claudeAccountEmail: "alex@example.com",
    claudeSelection: "alex@example.com",
    linearWorkspace: "we",
    linearTicket: "WE-142",
    linearLabel: "frontend",
    displayName: "Normalize sidebar CPU to % of allowance",
    monitorState: "working",
    ...overrides,
  });
}

/** Idle, balanced within the "pooled" Claude pool, with an unread dot (dropped from working). */
export function makeCloneIdle(overrides: Partial<Clone> = {}): Clone {
  return makeClone({
    id: "pega-dev-88",
    host: "10.99.0.12",
    managed: true,
    source: "pegasis0/rmng-template:latest",
    claudeAccountEmail: "sam@example.com",
    claudeGroup: "pooled",
    claudeSelection: "group:pooled",
    linearWorkspace: "dev",
    linearTicket: "DEV-88",
    displayName: "Wire up the pull-template wizard",
    monitorState: "idle",
    unread: true,
    ...overrides,
  });
}

/** Offline (wrapper unreachable), Claude on auto: the server picked the account. */
export function makeCloneOffline(overrides: Partial<Clone> = {}): Clone {
  return makeClone({
    id: "pega-hh-7",
    host: "10.99.0.13",
    managed: true,
    claudeAccountEmail: "alex@example.com",
    claudeSelection: "auto",
    displayName: "Database migration spike",
    monitorState: "idle",
    ...overrides,
  });
}

/** A managed scratch box deliberately left tokenless (no account installed). */
export function makeCloneNoToken(overrides: Partial<Clone> = {}): Clone {
  return makeClone({
    id: "scratch-box",
    host: "10.99.0.20",
    managed: true,
    claudeSelection: "none",
    monitorState: "idle",
    ...overrides,
  });
}

/** A plain unmanaged row (no container): only deletable, no commit/account actions.
 *  Carries no account fields at all, because an unmanaged clone never binds one. */
export function makeCloneUnmanaged(overrides: Partial<Clone> = {}): Clone {
  return makeClone({
    id: "legacy-desktop",
    host: "192.168.1.50",
    username: "admin",
    monitorState: "idle",
    ...overrides,
  });
}

/** A managed clone holding BOTH providers: a pinned Claude account and a pooled Codex one.
 *  Exercises the two-line sidebar layout (one binding line per provider, CPU on the first /
 *  MEM on the second). */
export function makeCloneDualProvider(overrides: Partial<Clone> = {}): Clone {
  return makeClone({
    id: "pega-dual-9",
    host: "10.99.0.14",
    managed: true,
    source: "pegasis0/rmng-template:latest",
    claudeAccountEmail: "alex@example.com",
    claudeSelection: "alex@example.com",
    codexAccountEmail: "alex@openai.com",
    codexGroup: "team",
    codexSelection: "group:team",
    linearWorkspace: "we",
    linearTicket: "WE-207",
    displayName: "Port the encoder path to the new VA surface pool",
    monitorState: "working",
    ...overrides,
  });
}

export const cloneWorking: Clone = makeCloneWorking();
export const cloneIdle: Clone = makeCloneIdle();
export const cloneOffline: Clone = makeCloneOffline();
export const cloneNoToken: Clone = makeCloneNoToken();
export const cloneUnmanaged: Clone = makeCloneUnmanaged();
export const cloneDualProvider: Clone = makeCloneDualProvider();

/** A sub clone: a managed clone spawned by `cloneWorking`, shown indented under it in the
 *  sidebar (collapsed by default). Cosmetic one-level nesting via `parent`. */
export function makeCloneSubClone(overrides: Partial<Clone> = {}): Clone {
  return makeClone({
    id: "pega-we-142-helper",
    host: "10.99.0.31",
    managed: true,
    source: "pegasis0/rmng-template:latest",
    claudeAccountEmail: "sam@example.com",
    claudeGroup: "pooled",
    claudeSelection: "group:pooled",
    parent: cloneWorking.id,
    displayName: "helper: run the e2e suite",
    monitorState: "working",
    ...overrides,
  });
}

export const cloneSubClone: Clone = makeCloneSubClone();

export const hosts: Clone[] = [
  cloneWorking,
  cloneSubClone,
  cloneIdle,
  cloneOffline,
  cloneNoToken,
  cloneUnmanaged,
  cloneDualProvider,
];

export const cloneIds: string[] = hosts.map((h) => h.id);
