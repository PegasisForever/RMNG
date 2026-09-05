import { test } from "node:test";
import assert from "node:assert/strict";
import { validateCwd, validateLaunch, isActive, resultText, toolResult } from "../../src/rmng/protocol.ts";

test("subclone launch rejects combined isolation and implicit context sharing", () => {
  assert.doesNotThrow(() => validateLaunch({ agent: "worker", task: "Read the file." }));
  for (const extra of [{ worktree: true }, { context: "fork" }, { workflowScript: "return 1" }, { share: true }, { action: "resume" }]) {
    assert.throws(() => validateLaunch({ agent: "worker", ...extra }));
  }
  assert.throws(() => validateLaunch({ task: "Missing agent." }));
});

test("project paths cannot seed the home or escape into host files", () => {
  for (const path of ["/home/rmng", "/home/rmngx/project", "/home/rmng/../root", "/home/rmng/.pi/agent"]) {
    assert.throws(() => validateCwd(path, "/home/rmng"));
  }
  assert.equal(validateCwd("/home/rmng/project", "/home/rmng"), "/home/rmng/project");
  assert.equal(validateCwd("/home/rmng/.local/project", "/home/rmng"), "/home/rmng/.local/project");
});

test("results preserve child failures", () => {
  const record = { id: "rmng-subagent-123", parent: "parent", sessionId: "session", cwd: "/home/rmng/project", agent: "worker", startedAt: 1, state: "failed" };
  const snapshot = { state: "failed", result: { results: [{ error: "The worker failed." }] } };
  const result = toolResult(record, snapshot);
  assert.equal(result.isError, true);
  assert.equal(resultText(snapshot), "The worker failed.");
  assert.ok(result.content[0]?.text?.includes(record.id));
  assert.equal(isActive("paused"), false);
  assert.equal(isActive("running"), true);
  assert.equal(isActive("unreachable"), false);
  assert.equal(toolResult({ ...record, state: "unreachable" }).isError, true);
});
