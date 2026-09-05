import { createServer } from "node:http";
import { randomUUID } from "node:crypto";
import { readFileSync, existsSync } from "node:fs";
import { join } from "node:path";
import { createAgentSession, DefaultResourceLoader, SessionManager, ModelRuntime, initTheme } from "@earendil-works/pi-coding-agent";
import registerUpstream from "../extension/index.ts";
import { readResultOutput, readSessionBackedOutput } from "../runs/background/inspect-rpc.ts";
import { resolveCurrentSessionId } from "../shared/session-identity.ts";
import { isActive } from "./protocol.ts";
import { DIRS } from "../shared/types.ts";
import { save } from "./fleet.ts";
import type { Snapshot, ToolResult } from "./protocol.ts";

type Config = { cwd: string; remoteDir: string; model?: string; thinking?: any };

export async function startHost(config: Config): Promise<void> {
  initTheme();
  console.log("Loading model credentials.");
  const runtime = await ModelRuntime.create();
  const available = await runtime.getAvailable();
  console.log("Model credentials loaded.");
  const model = available.find((item: any) => item.provider + "/" + item.id === config.model) ?? available[0];
  if (!model) throw new Error("The subclone has no authenticated model.");
  let context: any;
  let tool: any;
  const statePath = join(config.remoteDir, "state.json");
  let state: { nativeId?: string; asyncDir?: string } = existsSync(statePath) ? JSON.parse(readFileSync(statePath, "utf8")) : {};
  const resourceLoader = new DefaultResourceLoader({
    cwd: config.cwd,
    agentDir: join(process.env.HOME!, ".pi/agent"),
    noExtensions: true,
    noSkills: true,
    noContextFiles: true,
    extensionFactories: [{ name: "rmng-upstream", factory: (pi: any) => {
      pi.on("session_start", (_event: any, ctx: any) => { context = ctx; });
      const proxy = new Proxy(pi, { get(target, prop) {
        if (prop === "registerTool") return (definition: any) => {
          if (definition.name === "subagent") tool = definition;
          target.registerTool(definition);
        };
        // The host runs tools directly and does not start an extra model conversation.
        if (prop === "sendMessage" || prop === "sendUserMessage") return () => {};
        return Reflect.get(target, prop);
      } });
      registerUpstream(proxy);
    } }],
  });
  console.log("Loading upstream resources.");
  await resourceLoader.reload();
  const pointer = join(config.remoteDir, "session.json");
  const sessionPath = existsSync(pointer) ? JSON.parse(readFileSync(pointer, "utf8")) as string : undefined;
  const sessionManager = sessionPath ? SessionManager.open(sessionPath) : SessionManager.create(config.cwd, config.remoteDir);
  await save(pointer, sessionManager.getSessionFile());
  console.log("Creating the control session.");
  const { session, extensionsResult } = await createAgentSession({
    cwd: config.cwd, modelRuntime: runtime, model, thinkingLevel: config.thinking,
    sessionManager, resourceLoader,
  });
  if (extensionsResult.errors.length) throw new Error(JSON.stringify(extensionsResult.errors));
  // Session initialization binds extension controls without requesting a model turn.
  await session.bindExtensions({});
  console.log("Control session ready.");
  if (!tool || !context) throw new Error("The upstream subagent tool did not initialize.");
  const execute = async (params: Record<string, any>): Promise<ToolResult> => {
    const request: Record<string, any> = { ...params, cwd: config.cwd };
    if (request.action) {
      if (!state.nativeId) throw new Error("No child exists in this subclone.");
      request.id = state.nativeId;
      delete request.runId;
    }
    const result = await tool.execute(randomUUID(), request, new AbortController().signal, undefined, context);
    if (result.details?.asyncId) {
      state = { nativeId: result.details.asyncId, asyncDir: result.details.asyncDir };
      await save(statePath, state);
    }
    return result;
  };
  const snapshot = async (): Promise<Snapshot> => {
    const current = { ...state };
    if (!current.nativeId || !current.asyncDir) return { state: "queued" };
    const snapshotPath = join(config.remoteDir, current.nativeId + ".result.json");
    if (existsSync(snapshotPath)) return JSON.parse(readFileSync(snapshotPath, "utf8"));
    // Upstream status reconciliation detects dead workers before files are read.
    await execute({ action: "status" });
    const status = JSON.parse(readFileSync(join(current.asyncDir, "status.json"), "utf8"));
    let output = readResultOutput(DIRS.results, resolveCurrentSessionId(sessionManager), current.nativeId,
      undefined, [config.remoteDir, config.cwd], undefined, Date.now, config.remoteDir);
    if (!isActive(status.state) && !output.output && !output.errorText && status.sessionFile) {
      output = readSessionBackedOutput(status.sessionFile, [config.remoteDir], config.remoteDir);
    }
    const snapshot: Snapshot = { state: status.state, nativeId: current.nativeId, progress: status,
      result: { results: [{ output: output.output, error: output.errorText }] } };
    if (!isActive(status.state) && (output.output || output.errorText || status.error)) await save(snapshotPath, snapshot);
    return snapshot;
  };
  const archiveTimer = setInterval(() => {
    void snapshot().catch((error) => console.error("Cannot save child output:", String(error)));
  }, 3000);
  archiveTimer.unref();
  let starting = false;
  const server = createServer(async (req, res) => {
    try {
      let value: unknown;
      if (req.method === "GET" && req.url === "/health") value = { ready: true };
      else if (req.method === "GET" && req.url === "/snapshot") value = await snapshot();
      else if (req.method === "POST" && req.url === "/execute") {
        let body = "";
        for await (const part of req) {
          body += part;
        }
        const params = JSON.parse(body);
        if (params.isolation === "subclone") throw new Error("The remote host cannot allocate another subclone.");
        if (!params.action && (state.nativeId || starting)) throw new Error("This subclone already has a child. Use resume.");
        starting = true;
        try { value = await execute({ ...params, async: true, context: "fresh", share: false }); }
        finally { starting = false; }
      } else {
        res.writeHead(404).end();
        return;
      }
      res.writeHead(200, { "content-type": "application/json" }).end(JSON.stringify(value));
    } catch (error) {
      res.writeHead(500, { "content-type": "application/json" }).end(JSON.stringify({ error: String(error) }));
    }
  });
  const readyPath = join(config.remoteDir, "ready.json");
  const port = existsSync(readyPath) ? JSON.parse(readFileSync(readyPath, "utf8")).port : 0;
  server.listen(port, "0.0.0.0", async () => {
    const address = server.address();
    await save(join(config.remoteDir, "ready.json"), { port: typeof address === "object" && address ? address.port : undefined });
  });
}
