// Guards the op-start handoff every op-driving dialog depends on: clone, fork,
// prebuild, and rebase handlers all answer `{ ok: true, op }`, and these readers
// resolve exactly that `op`. A bare-Operation answer would resolve `undefined` and
// crash the dialog's op tracking — which is what happened to rebase once, so this
// test pins the wrapped shape on both sides of each call.
import { afterEach, expect, mock, test } from "bun:test";

import {
  duplicateClone,
  forkClone,
  prebuildDockerfile,
  rebaseClone,
} from "./api";

const op = {
  id: "op-1",
  kind: "clone",
  target: "pega-x",
  status: "running",
  step: "queued",
  message: "",
  pct: 0,
  log: [],
};

function stubFetch(seen: { url: string; init?: RequestInit }[]) {
  // @ts-expect-error bun test stub for the browser fetch the api module calls.
  globalThis.fetch = mock(async (url: string, init?: RequestInit) => {
    seen.push({ url, init });
    return {
      ok: true,
      status: 200,
      statusText: "OK",
      text: async () => JSON.stringify({ ok: true, op }),
    };
  });
}

afterEach(() => {
  // @ts-expect-error restore the real fetch between cases.
  globalThis.fetch = undefined;
});

test("duplicateClone resolves the wrapped op", async () => {
  const seen: { url: string; init?: RequestInit }[] = [];
  stubFetch(seen);
  const got = await duplicateClone({
    plain: { title: "x", message: "" },
    runStartupScript: true,
  });
  expect(got.id).toBe("op-1");
  expect(seen[0].url).toBe("/api/clone");
});

test("forkClone resolves the wrapped op", async () => {
  const seen: { url: string; init?: RequestInit }[] = [];
  stubFetch(seen);
  const got = await forkClone("src-id", true, {
    preset: "webapp",
    runStartupScript: true,
  });
  expect(got.id).toBe("op-1");
  expect(seen[0].url).toBe("/api/fork");
});

test("prebuildDockerfile posts the editor text and resolves the wrapped op", async () => {
  const seen: { url: string; init?: RequestInit }[] = [];
  stubFetch(seen);
  const got = await prebuildDockerfile("FROM x:latest");
  expect(got.id).toBe("op-1");
  expect(seen[0].url).toBe("/api/images/prebuild");
  expect(JSON.parse(seen[0].init?.body as string)).toEqual({
    dockerfile: "FROM x:latest",
  });
});

test("rebaseClone posts preset plus rebuild and resolves the wrapped op", async () => {
  const seen: { url: string; init?: RequestInit }[] = [];
  stubFetch(seen);
  const got = await rebaseClone("pega-x", "webapp", true);
  expect(got.id).toBe("op-1");
  expect(seen[0].url).toBe("/api/hosts/pega-x/rebase");
  expect(JSON.parse(seen[0].init?.body as string)).toEqual({
    preset: "webapp",
    rebuild: true,
  });
});

test("forkClone sends rebuild only when checked", async () => {
  const seen: { url: string; init?: RequestInit }[] = [];
  stubFetch(seen);
  await forkClone("src-id", false, {
    preset: "webapp",
    runStartupScript: true,
    rebuild: true,
  });
  expect(JSON.parse(seen[0].init?.body as string).rebuild).toBe(true);

  const seen2: { url: string; init?: RequestInit }[] = [];
  stubFetch(seen2);
  await forkClone("src-id", false, {
    preset: "webapp",
    runStartupScript: true,
  });
  expect("rebuild" in JSON.parse(seen2[0].init?.body as string)).toBe(false);
});
