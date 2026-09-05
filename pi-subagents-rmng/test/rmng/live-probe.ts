import { mkdirSync, existsSync, readFileSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import { setTimeout as delay } from "node:timers/promises";
import assert from "node:assert/strict";
import { createAgentSession, DefaultResourceLoader, SessionManager, ModelRuntime, initTheme } from "@earendil-works/pi-coding-agent";
import register from "../../index.ts";
import { rmng } from "../../src/rmng/fleet.ts";

export async function probe() {
  const cwd = "/home/rmng/subclone-demo";
  initTheme();
  const runtime = await ModelRuntime.create();
  const model = (await runtime.getAvailable()).find((m: any) => m.provider === "openai-codex" && m.id === "gpt-5.6-luna");
  assert.ok(model);
  let context: any;
  let tool: any;
  const agentDir = join(process.env.HOME!, ".pi/agent");
  const loader = new DefaultResourceLoader({ cwd, agentDir, noExtensions: true, noContextFiles: true,
    extensionFactories: [{ name: "probe", factory: (pi: any) => {
      pi.on("session_start", (_event: any, ctx: any) => { context = ctx; });
      register(new Proxy(pi, { get(target, prop) {
        if (prop === "registerTool") return (definition: any) => {
          if (definition.name === "subagent") tool = definition;
          target.registerTool(definition);
        };
        if (prop === "sendMessage") return (message: any) => console.log("NOTICE " + JSON.stringify(message));
        return Reflect.get(target, prop);
      } }));
    } }],
  });
  await loader.reload();
  const probeDir = join(process.env.HOME!, ".pi/rmng-probe");
  mkdirSync(probeDir, { recursive: true });
  const existing = process.env.EXISTING_CLONE;
  const record = existing ? JSON.parse(readFileSync(join(process.env.HOME!, ".pi/rmng-subclones", existing + ".json"), "utf8")) : undefined;
  if (record) {
    const ready = JSON.parse(await rmng(["clone", "exec", existing!, "--", "cat", "/home/rmng/.pi/rmng-host/ready.json"]));
    const fleet = JSON.parse(await rmng(["clone", "ls", "--json"]));
    record.address = "http://" + fleet.clones.find((c: any) => c.id === existing).localIp + ":" + ready.port;
    writeFileSync(join(process.env.HOME!, ".pi/rmng-subclones", existing + ".json"), JSON.stringify(record));
  }
  const sessionManager = record ? SessionManager.open(record.sessionId) : SessionManager.create(cwd, probeDir);
  const { session, extensionsResult } = await createAgentSession({ cwd, agentDir, modelRuntime: runtime, model,
    thinkingLevel: "xhigh", sessionManager, resourceLoader: loader });
  assert.equal(extensionsResult.errors.length, 0);
  await session.bindExtensions({});
  writeFileSync(join(probeDir, "parent-session.txt"), sessionManager.getSessionFile()!);
  const call = async (params: any) => {
    console.log("CALL " + JSON.stringify(params));
    const result = await tool.execute("probe-" + Date.now(), params, new AbortController().signal,
      (update: any) => console.log("UPDATE " + JSON.stringify(update)), context);
    console.log("RESULT " + JSON.stringify(result));
    assert.ok(!result.isError, JSON.stringify(result));
    return result;
  };
  const first = existing ? await call({ action: "status", id: existing }) : await call({ agent: "worker", cwd, context: "fresh", async: false, isolation: "subclone",
    model: "openai-codex/gpt-5.6-luna:xhigh", output: false,
    task: "Read seed-marker.txt and marker-link. Run hostname. Write child-only.txt with the marker and hostname. Report both values. Do not change other files." });
  assert.match(first.content.map((p: any) => p.text || "").join("\n"), /uncommitted-seed-7319/);
  const id = first.details.rmng.clone;
  assert.ok(!existsSync(join(cwd, "child-only.txt")), "Child changed the parent tree.");
  assert.equal(readFileSync(join(cwd, "seed-marker.txt"), "utf8"), "uncommitted-seed-7319\n");
  writeFileSync(join(probeDir, "clone.txt"), id);
  await call({ action: "resume", id, message: "Read child-only.txt. Append the line FOLLOW_UP_OK to it. Report the full file. Do not change other files." });
  const deadline = Date.now() + 180_000;
  let completed = false;
  while (Date.now() < deadline) {
    await delay(2500);
    const status = await call({ action: "status", id });
    if (status.details.rmng.state === "complete") {
      assert.match(status.content.map((p: any) => p.text || "").join("\n"), /FOLLOW_UP_OK/);
      completed = true;
      break;
    }
  }
  assert.ok(completed, "Follow-up did not finish.");
  assert.ok(!existsSync(join(cwd, "child-only.txt")));
  if (process.env.CONTROL_TEST === "1") {
    await call({ action: "resume", id, message: "Run sleep 30 through bash. After it finishes, report WAIT_FINISHED. Do not edit files." });
    await delay(4000);
    await call({ action: "steer", id, message: "Report CONTROL_STEER_OK after the wait.", mode: "follow_up" });
    await call({ action: "stop", id });
    let stopped = false;
    for (let attempt = 0; attempt < 20; attempt++) {
      await delay(1000);
      const result = await call({ action: "status", id });
      if (["stopped", "paused", "cancelled", "aborted"].includes(result.details.rmng.state)) { stopped = true; break; }
    }
    assert.ok(stopped, "Stop did not settle the child.");
    console.log("CONTROL_PASS " + id);
  }
  console.log("PROBE_PASS " + id);
  process.exit(0);
}
