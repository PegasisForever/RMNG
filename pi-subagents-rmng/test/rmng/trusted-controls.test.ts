import { test } from "node:test";
import assert from "node:assert/strict";
import { mkdtemp, mkdir, writeFile, readFile, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { createServer } from "node:http";
import { execFile } from "node:child_process";
import { promisify } from "node:util";
import { fileURLToPath } from "node:url";

const root = fileURLToPath(new URL("../../", import.meta.url));

test("another session can control and close a recorded clone without authentication", async () => {
  const home = await mkdtemp(join(tmpdir(), "rmng-trusted-"));
  const id = "rmng-subagent-ab12cd";
  const calls: Array<{ route?: string; authorization?: string; body: string }> = [];
  const server = createServer(async (req, res) => {
    let body = "";
    for await (const part of req) body += part;
    calls.push({ route: req.url, authorization: req.headers.authorization, body });
    const value = req.url === "/execute"
      ? { content: [], details: { asyncId: "resumed-run" } }
      : { state: "complete", nativeId: "resumed-run", result: { results: [{ output: "TRUSTED_CONTROL_OK" }] } };
    res.writeHead(200, { "content-type": "application/json" }).end(JSON.stringify(value));
  });
  await new Promise<void>((resolve) => server.listen(0, "127.0.0.1", resolve));
  try {
    const address = server.address() as { port: number };
    const records = join(home, ".pi/rmng-subclones");
    await mkdir(records, { recursive: true });
    await mkdir(join(home, "bin"));
    await writeFile(join(home, "bin/rmng"), '#!/bin/sh\nprintf "%s\\n" "$@" > "$HOME/rmng-args"\n', { mode: 0o755 });
    await writeFile(join(records, id + ".json"), JSON.stringify({
      id, parent: "another-clone", sessionId: "original-session", cwd: home + "/project", agent: "worker",
      startedAt: Date.now(), state: "complete", address: "http://127.0.0.1:" + address.port,
    }));
    const code = `
      import assert from "node:assert/strict";
      import { withRmngSubclones } from "./src/rmng/extension.ts";
      import { createSubagentParamsSchema } from "./src/extension/schemas.ts";
      const handlers = new Map();
      const notices = [];
      let tool;
      const pi = {
        on(event, handler) { handlers.set(event, handler); },
        registerTool(value) { tool = value; },
        sendMessage(value) { notices.push(value); },
      };
      const wrapped = withRmngSubclones(pi);
      wrapped.registerTool({ name: "subagent", description: "Delegate tasks.", parameters: createSubagentParamsSchema(),
        execute() { throw new Error("Unexpected local dispatch."); } });
      const ctx = { sessionManager: { getSessionFile() { return "requesting-session"; } },
        ui: { notify(error) { throw new Error(error); } } };
      await handlers.get("session_start")({}, ctx);
      assert.equal(notices.length, 0);
      const call = (action) => tool.execute("test", { action, id: ${JSON.stringify(id)}, message: "Continue." }, undefined, undefined, ctx);
      assert.equal((await call("status")).isError, undefined);
      assert.equal((await call("resume")).isError, undefined);
      for (let attempt = 0; notices.length === 0 && attempt < 100; attempt++) await new Promise(resolve => setTimeout(resolve, 20));
      assert.equal(notices.length, 1);
      assert.match(notices[0].content, /TRUSTED_CONTROL_OK/);
      assert.equal((await call("close")).isError, undefined);
      handlers.get("session_shutdown")();
    `;
    await promisify(execFile)(process.execPath, ["--experimental-strip-types", "--input-type=module", "-e", code], {
      cwd: root, timeout: 20_000,
      env: { ...process.env, HOME: home, PATH: join(home, "bin") + ":" + process.env.PATH, RMNG_CONTROL_URL: "http://unused" },
    });
    assert.ok(calls.some((call) => call.route === "/execute"));
    assert.ok(calls.every((call) => call.authorization === undefined));
    const record = JSON.parse(await readFile(join(records, id + ".json"), "utf8"));
    assert.equal(record.sessionId, "requesting-session");
    assert.equal(record.state, "closed");
    assert.equal("token" in record, false);
    assert.deepEqual((await readFile(join(home, "rmng-args"), "utf8")).trim().split("\n"),
      ["clone", "rm", id, "-y", "--wait", "--json"]);
  } finally {
    server.closeAllConnections();
    await new Promise<void>((resolve) => server.close(() => resolve()));
    await rm(home, { recursive: true, force: true });
  }
});
