import { test } from "node:test";
import assert from "node:assert/strict";
import { withRmngSubclones } from "../../src/rmng/extension.ts";
import { createSubagentParamsSchema } from "../../src/extension/schemas.ts";

test("subclone registration preserves upstream action validation and dispatch", async () => {
  const handlers = new Map<string, Function>();
  let registered: any;
  const pi = {
    on(event: string, handler: Function) { handlers.set(event, handler); },
    registerTool(tool: any) { registered = tool; },
  };
  const parameters = createSubagentParamsSchema();
  const result = { content: [{ type: "text", text: "Local status works." }], details: {} };
  let calls = 0;
  withRmngSubclones(pi as any).registerTool({
    name: "subagent", description: "Delegate tasks.", parameters,
    execute: async (_id: any, args: any) => {
      assert.deepEqual(args, { action: "status" });
      calls++;
      return result;
    },
  } as any);
  try {
    assert.deepEqual(registered.parameters.properties.action, parameters.properties.action);
    assert.ok(registered.parameters.properties.isolation.enum.includes("subclone"));
    assert.equal(await registered.execute("call", { action: "status" }, undefined, undefined, {}), result);
    assert.equal(calls, 1);
  } finally { handlers.get("session_shutdown")!(); }
});
