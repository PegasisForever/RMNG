// The jobs of dialogs closed before their job ended (`./useLeftJobs.ts`): a failure reaches the
// error line, a job that ends well says nothing. The op rules through the pure step; the
// hook's two kinds through a renderer (`@happy-dom/global-registrator`).
import { afterEach, describe, expect, test } from "bun:test";
import { GlobalRegistrator } from "@happy-dom/global-registrator";

import type { Operation } from "./types";

if (!GlobalRegistrator.isRegistered) GlobalRegistrator.register();
(globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
const { act } = await import("react");
const { createRoot } = await import("react-dom/client");
const { stepFollowed, useLeftJobs } = await import("./useLeftJobs");
type Leave = ReturnType<typeof useLeftJobs>;

const op = (status: Operation["status"], message = ""): Operation =>
  ({ id: "op1", kind: "clone", target: "pega-we-1", status, message, step: "", pct: 0, log: [] }) as unknown as Operation;

describe("stepFollowed", () => {
  const f = { id: "op1", label: "New clone", seen: false };

  test("a failed op is an error line, and is no longer followed", () => {
    expect(stepFollowed([f], [op("error", "no space left")])).toEqual({ followed: [], errors: ["New clone: no space left"] });
  });

  test("a failed op that says nothing still says it failed", () => {
    expect(stepFollowed([f], [op("error")]).errors).toEqual(["New clone: it failed"]);
  });

  test("a done op ends quietly", () => {
    expect(stepFollowed([f], [op("done")])).toEqual({ followed: [], errors: [] });
  });

  test("an op not in the list yet is still followed; one that was seen and is gone ended well", () => {
    expect(stepFollowed([f], [])).toEqual({ followed: [f], errors: [] });
    const seen = stepFollowed([f], [op("running")]).followed;
    expect(seen).toEqual([{ ...f, seen: true }]);
    expect(stepFollowed(seen, [])).toEqual({ followed: [], errors: [] });
  });
});

describe("useLeftJobs", () => {
  let root: ReturnType<typeof createRoot> | null = null;
  let leave: Leave | null = null;
  let errors: string[] = [];
  afterEach(() => {
    act(() => root?.unmount());
    root = null;
    errors = [];
  });

  function Probe({ operations }: { operations: Operation[] }) {
    leave = useLeftJobs(operations, (e) => errors.push(e));
    return null;
  }
  const show = (operations: Operation[]) => {
    root ??= createRoot(document.createElement("div"));
    act(() => root!.render(<Probe operations={operations} />));
  };
  const tick = () => act(() => new Promise((r) => setTimeout(r, 0)));

  test("a request that is refused after its dialog closed is an error line", async () => {
    show([]);
    leave!({ label: "New ticket", request: Promise.reject(new Error("Linear said no")) });
    await tick();
    expect(errors).toEqual(["New ticket: Linear said no"]);
  });

  test("a request that ends well says nothing", async () => {
    show([]);
    leave!({ label: "New ticket", request: Promise.resolve() });
    await tick();
    expect(errors).toEqual([]);
  });

  test("an operation that fails after its dialog closed is an error line, once", async () => {
    show([]);
    leave!({ label: "Rebase of pega-we-1", operation: Promise.resolve(op("running")) });
    await tick();
    show([op("running")]);
    show([op("error", "the image pull failed")]);
    show([op("error", "the image pull failed")]);
    expect(errors).toEqual(["Rebase of pega-we-1: the image pull failed"]);
  });

  test("a POST refused after its dialog closed is an error line", async () => {
    show([]);
    leave!({ label: "New clone", operation: Promise.reject(new Error("no free address")) });
    await tick();
    expect(errors).toEqual(["New clone: no free address"]);
  });
});
