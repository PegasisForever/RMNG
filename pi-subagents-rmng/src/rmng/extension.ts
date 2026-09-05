import { randomBytes } from "node:crypto";
import { existsSync, readFileSync, readdirSync } from "node:fs";
import { join, resolve } from "node:path";
import { setTimeout as delay } from "node:timers/promises";
import type { ExtensionAPI } from "@earendil-works/pi-coding-agent";
import { registerExternalRun, updateExternalRun } from "../api/external-runs.ts";
import { registerBackgroundWorkProvider } from "../api/background-work.ts";
import { resolveCurrentSessionId } from "../shared/session-identity.ts";
import { isActive, validateCwd, validateLaunch, toolResult, resultText, type RemoteRecord, type Snapshot, type ToolResult } from "./protocol.ts";
import { provision, request, save, self, rmng } from "./fleet.ts";

const ROOT = join(process.env.HOME!, ".pi/rmng-subclones");
const CONTROLS = new Set(["status", "debug.run", "steer", "interrupt", "stop", "resume", "close"]);

export function withRmngSubclones(pi: ExtensionAPI): ExtensionAPI {
  const records = new Map<string, RemoteRecord>();
  const polling = new Set<string>();
  let currentSession = "";
  let stopped = false;
  const savedSession = (record: RemoteRecord): string => JSON.parse(readFileSync(join(ROOT, record.id + ".json"), "utf8")).sessionId;
  const persist = (record: RemoteRecord) => save(join(ROOT, record.id + ".json"), record);
  const display = (record: RemoteRecord, snapshot?: Snapshot) => {
    const state = isActive(record.state) ? "running" : record.state === "complete" ? "completed" : ["failed", "unreachable"].includes(record.state) ? "failed" : "stopped";
    const update = { state, updatedAt: Date.now(), preview: (snapshot ? resultText(snapshot) : record.error || record.state).slice(0, 4000) } as const;
    try { updateExternalRun(record.sessionId, record.id, update); }
    catch {
      registerExternalRun({ id: record.id, sessionId: record.sessionId, source: "rmng", label: record.agent + " on " + record.id,
        startedAt: record.startedAt, ...update });
    }
  };
  const inspect = async (record: RemoteRecord): Promise<Snapshot> => {
    const previousNativeId = record.nativeId;
    const snapshot: Snapshot = await request(record, "/snapshot");
    if (record.nativeId !== previousNativeId && snapshot.nativeId !== record.nativeId) return inspect(record);
    record.sessionId = savedSession(record);
    record.state = snapshot.state;
    record.nativeId = snapshot.nativeId;
    delete record.error;
    await persist(record);
    await save(join(ROOT, record.id + ".result.json"), snapshot);
    display(record, snapshot);
    return snapshot;
  };
  const watch = async (record: RemoteRecord) => {
    if (polling.has(record.id)) return;
    polling.add(record.id);
    let failures = 0;
    try {
      while (!stopped && record.sessionId === currentSession && savedSession(record) === currentSession && isActive(record.state)) {
        try {
          const snapshot = await inspect(record);
          failures = 0;
          if (!stopped && record.sessionId === currentSession && !isActive(snapshot.state) && !record.notified) {
            record.notified = true;
            await persist(record);
            pi.sendMessage({ customType: "rmng-subagent", content: toolResult(record, snapshot).content.map((part) => part.text || "").join("\n"),
              display: true, details: { clone: record.id, state: snapshot.state } }, { triggerTurn: true, deliverAs: "followUp" });
          }
        } catch (error) {
          record.error = "Cannot reach subclone: " + String(error);
          if (++failures >= 3) {
            record.state = "unreachable";
            if (!stopped && record.sessionId === currentSession) {
              pi.sendMessage({ customType: "rmng-subagent", content: toolResult(record).content[0]!.text!,
                display: true, details: { clone: record.id, state: record.state } }, { triggerTurn: true, deliverAs: "followUp" });
            }
          }
          display(record);
          await persist(record);
        }
        if (isActive(record.state)) await delay(3000);
      }
    } finally {
      record.sessionId = savedSession(record);
      polling.delete(record.id);
      if (!stopped && record.sessionId === currentSession && savedSession(record) === currentSession && isActive(record.state)) void watch(record);
    }
  };
  const unregister = registerBackgroundWorkProvider({
    name: "rmng-subclones",
    listActiveWork: () => [...records.values()].filter((record) => isActive(record.state)).map(({ id, sessionId }) => ({ id, sessionId })),
  });
  pi.on("session_start", async (_event, ctx) => {
    currentSession = resolveCurrentSessionId(ctx.sessionManager);
    stopped = false;
    if (!existsSync(ROOT)) return;
    for (const file of readdirSync(ROOT).filter((name) => /^rmng-subagent-[a-f0-9]+\.json$/.test(name))) {
      try {
        const record: RemoteRecord = JSON.parse(readFileSync(join(ROOT, file), "utf8"));
        if (record.state === "creating" && record.sessionId === currentSession) {
          record.state = "failed";
          record.error = "Provisioning stopped before the parent saved a ready host. Inspect the retained clone.";
          await persist(record);
        }
        records.set(record.id, record);
        if (record.sessionId === currentSession) {
          display(record);
          if (isActive(record.state)) void watch(record);
        }
      } catch (error) { ctx.ui.notify("Cannot restore subclone record: " + String(error), "error"); }
    }
  });
  pi.on("session_shutdown", () => { stopped = true; unregister(); });

  return new Proxy(pi, { get(target, prop) {
    if (prop !== "registerTool") return Reflect.get(target, prop);
    return (original: any) => {
      if (original.name !== "subagent") { target.registerTool(original); return; }
      const properties = original.parameters.properties;
      const tool = {
        ...original,
        description: original.description + "\nInside RMNG, use isolation: 'subclone' with {agent, task} for a fresh headless clone seeded from cwd. " +
          "Each call creates one clone. Use its returned rmng-subagent id with status, steer, interrupt, stop, resume, or close. " +
          "Resume keeps the same clone and files. Close deletes that clone. Save wanted changes first. " +
          "Subclone context is fresh. Include needed context in task. Normal calls keep upstream behavior.",
        parameters: { ...original.parameters, properties: { ...properties,
          isolation: { type: "string", enum: ["none", "worktree", "subclone"], description: "Use subclone for one child in a fresh RMNG clone." },
        } },
        async execute(id: string, params: Record<string, any>, signal: AbortSignal | undefined, onUpdate: any, ctx: any) {
          const requested = params.id ?? params.runId;
          if (params.isolation !== "subclone" && !(typeof requested === "string" && requested.startsWith("rmng-subagent-"))) {
            return original.execute(id, params, signal, onUpdate, ctx);
          }
          const sessionId = resolveCurrentSessionId(ctx.sessionManager);
          const record = typeof requested === "string" ? records.get(requested) : undefined;
          try {
            if (record) {
              if (!CONTROLS.has(params.action)) throw new Error("Use status, steer, interrupt, stop, resume, or close for a subclone.");
              if (params.action === "close") {
                await rmng(["clone", "rm", record.id, "-y", "--wait", "--json"], undefined, 660_000);
                record.state = "closed";
                await persist(record);
                display(record);
                return toolResult(record);
              }
              if (record.state === "closed") throw new Error("This subclone is closed.");
              if (params.action === "status" && !params.view) {
                const snapshot = await inspect(record);
                if (isActive(record.state)) void watch(record);
                return toolResult(record, snapshot);
              }
              const result: ToolResult = await request(record, "/execute", params);
              if (params.action === "resume" && !result.isError) {
                record.sessionId = sessionId;
                record.state = "running";
                record.nativeId = result.details?.asyncId ?? record.nativeId;
                record.notified = false;
                await persist(record);
                display(record);
                void watch(record);
                return toolResult(record);
              }
              return { ...result, content: [{ type: "text", text: record.id }, ...result.content],
                details: { mode: "management", results: [], runId: record.id, rmng: { clone: record.id, native: result.details } } };
            }
            if (typeof requested === "string" && requested.startsWith("rmng-subagent-")) {
              throw new Error("No local control record exists for this subclone.");
            }
            if (params.isolation !== "subclone") return original.execute(id, params, signal, onUpdate, ctx);
            validateLaunch(params);
            const parent = await self();
            const cwd = validateCwd(resolve(ctx.cwd, params.cwd || "."), process.env.HOME!);
            if (!existsSync(cwd)) throw new Error("The requested project directory does not exist.");
            const created: RemoteRecord = {
              id: "rmng-subagent-" + randomBytes(6).toString("hex"), parent: parent.id, sessionId, cwd,
              agent: params.agent, startedAt: Date.now(), state: "creating",
            };
            records.set(created.id, created);
            await persist(created);
            display(created);
            onUpdate?.(toolResult(created));
            try {
              await provision(created, parent, {
                model: ctx.model ? ctx.model.provider + "/" + ctx.model.id : undefined,
                thinking: pi.getThinkingLevel(),
              }, signal);
              const { isolation: _isolation, cwd: _cwd, async: _async, ...remoteParams } = params;
              const started: ToolResult = await request(created, "/execute", remoteParams);
              if (started.isError) throw new Error(started.content.map((part) => part.text || "").join("\n"));
              created.state = "running";
              created.nativeId = started.details?.asyncId;
              await persist(created);
              display(created);
              if (params.async !== false) {
                void watch(created);
                return toolResult(created);
              }
              while (true) {
                if (signal?.aborted) {
                  await request(created, "/execute", { action: "stop" });
                  return toolResult(created, await inspect(created));
                }
                const snapshot = await inspect(created);
                onUpdate?.(toolResult(created, snapshot));
                if (!isActive(snapshot.state)) return toolResult(created, snapshot);
                await delay(1500);
              }
            } catch (error) {
              created.state = created.nativeId ? "unreachable" : "failed";
              created.error = String(error);
              await persist(created);
              display(created);
              return toolResult(created);
            }
          } catch (error) {
            return { content: [{ type: "text", text: String(error) }], details: { mode: "management", results: [] }, isError: true };
          }
        },
      };
      target.registerTool(tool);
    };
  } });
}
