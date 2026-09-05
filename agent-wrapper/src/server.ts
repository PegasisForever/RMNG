// Small HTTP wrapper around the pi coding agent, run inside each RDP container
// (one process per host) on :4096.
//
// It holds ONE long-lived pi AgentSession, created lazily on the first prompt and kept alive
// for the process lifetime. The desktop operating notes + the per-host "implement a ticket"
// procedure ride the system-prompt append: the Settings-editable copy the control-server
// injects at clone creation if present, else the baked-in default (see instructions.ts /
// agent-instructions.md).
//
// The agent runs on gpt-5.6-luna through pi's `openai-codex` provider, authenticated by the
// Codex token the control-server pushes into ~/.codex/auth.json (see auth.ts). Desktop and
// Linear reach the model as tools through pi-mcp-adapter (see mcp.ts).
//
//   POST /prompt { text }   queue a user turn. 202 immediately; 409 if a turn is
//                           already running. Reply + progress arrive on /events.
//   GET  /events            SSE: { busy } snapshot, then { activity } lines, then
//                           { reply, solicited } / { error } per turn.
//   POST /abort             interrupt the current turn (session stays alive).
//
// Every reply is solicited. pi has no background bash and no task notifications, so the
// autonomous `{ reply, solicited:false }` frame the Claude Agent SDK could produce never
// fires. The control-server still reads `busy` and `activity` for fleet working/idle state.
//
// Session state is in memory only: a CoW clone boots a fresh wrapper and starts a brand-new
// conversation.
import { readFileSync } from "node:fs";

import {
  createAgentSession,
  DefaultResourceLoader,
  ModelRuntime,
  SessionManager,
  type AgentSession,
  type AgentSessionEvent,
} from "@earendil-works/pi-coding-agent";
import type { Model } from "@earendil-works/pi-ai";
import { registerBunOAuthFlows } from "@earendil-works/pi-ai/bun-oauth";
import { createMcpAdapter } from "pi-mcp-adapter";

import { CodexFileCredentialStore } from "./auth";
import { mcpConfigFromDescriptor, type McpAdapterConfig } from "./mcp";
import { resolveSystemAppend } from "./instructions";
import { serviceTierExtension } from "./serviceTier";

import { CONFIG } from "./config";

// The system-prompt append for THIS host's session agent: the control-server-injected,
// Settings-editable playbook (operating notes + ticket procedure) if present, else the
// baked-in default. Read once at startup — a fresh clone boots a fresh wrapper.
const SYSTEM_APPEND = resolveSystemAppend(CONFIG.instructionsPath);

// pi loads each provider's OAuth flow through a variable import specifier so bundlers cannot
// follow it. That breaks in a `bun build --compile` binary, where the resolver has no such
// file to find and every request dies with "Cannot find module ./openai-codex.js". This
// registers the statically bundled flows instead. Must run before the first auth resolve.
registerBunOAuthFlows();

const ACTIVITY_MAX = 200;

// ---- session state (one per process) --------------------------------------
let session: AgentSession | null = null;
let starting: Promise<AgentSession> | null = null;
let turnActive = false;
let lastReply = "";
let lastError = "";
let interruptRequested = false;

// ---- SSE fan-out -----------------------------------------------------------
type Sub = (frame: string) => void;
const subs = new Set<Sub>();

function emit(obj: Record<string, unknown>): void {
  const frame = `data: ${JSON.stringify(obj)}\n\n`;
  for (const fn of subs) {
    try {
      fn(frame);
    } catch {
      // dead subscriber; its stream cancel() cleans it up
    }
  }
}

function emitActivity(text: string): void {
  const oneLine = text.replace(/\s+/g, " ").trim();
  if (!oneLine) return;
  emit({ activity: oneLine.length > ACTIVITY_MAX ? oneLine.slice(0, ACTIVITY_MAX - 1) + "…" : oneLine });
}

// ---- MCP -------------------------------------------------------------------
// Prefer the control-server's MCP descriptor (single source of truth, ~/.config/rmng/mcp.json,
// already headless-filtered). Fall back to the built-in `desktop`+`linear` set when the file is
// missing/unreadable (e.g. a clone created before this control-server, or a bare dev run).
function mcpConfig(): McpAdapterConfig {
  try {
    const parsed = JSON.parse(readFileSync(CONFIG.mcpConfigPath, "utf8"));
    if (Array.isArray(parsed)) return mcpConfigFromDescriptor(parsed);
  } catch {
    // fall through to the built-in set
  }
  return mcpConfigBuiltin();
}

// The historical hardcoded set, kept as a fallback when no descriptor is present.
function mcpConfigBuiltin(): McpAdapterConfig {
  const entries = [];
  // The desktop-control MCP is served by the clone-daemon over HTTP (localhost), sharing its live
  // Mutter session. Skipped on headless clones — there is no daemon / :9004 there.
  if (!CONFIG.headless) {
    entries.push({ name: "desktop", url: CONFIG.daemonMcpUrl, alwaysLoad: true });
  }
  // The clone's preset Linear identity (LINEAR_API_KEY, injected at clone creation).
  entries.push({ name: "linear", url: "https://mcp.linear.app/mcp", bearerEnv: "LINEAR_API_KEY" });
  return mcpConfigFromDescriptor(entries);
}

// ---- the persistent session ------------------------------------------------
/** Resolve the configured model, accepting either `provider/id` or a bare id. */
async function pickModel(runtime: ModelRuntime): Promise<Model<any>> {
  const [provider, id] = CONFIG.model.includes("/") ? CONFIG.model.split("/", 2) : [undefined, CONFIG.model];
  const available = await runtime.getAvailable();
  const match = available.find((m) => m.id === id && (!provider || m.provider === provider));
  if (!match) {
    const seen = available.map((m) => `${m.provider}/${m.id}`).join(", ") || "none";
    throw new Error(`model ${CONFIG.model} is not available (have: ${seen})`);
  }
  return match;
}

async function startSession(): Promise<AgentSession> {
  const runtime = await ModelRuntime.create({
    credentials: new CodexFileCredentialStore(CONFIG.codexAuthPath),
    // The credential store is the only auth source, and the model ships in pi's built-in
    // catalog, so neither auth.json nor models.json is consulted.
    authPath: undefined,
    modelsPath: null,
  });
  const resourceLoader = new DefaultResourceLoader({
    cwd: process.env.HOME ?? process.cwd(),
    agentDir: CONFIG.agentDir,
    extensionFactories: [
      { name: "rmng-mcp", factory: createMcpAdapter({ config: mcpConfig() }) },
      { name: "rmng-service-tier", factory: serviceTierExtension },
    ],
    ...(SYSTEM_APPEND ? { appendSystemPrompt: [SYSTEM_APPEND] } : {}),
  });
  // A caller-supplied loader starts empty. createAgentSession only reloads the one it
  // builds itself, so without this the extensions never load and the agent has no MCP tools.
  await resourceLoader.reload();
  const { session: created, extensionsResult } = await createAgentSession({
    cwd: process.env.HOME ?? process.cwd(),
    agentDir: CONFIG.agentDir,
    modelRuntime: runtime,
    model: await pickModel(runtime),
    thinkingLevel: CONFIG.thinkingLevel,
    sessionManager: SessionManager.inMemory(),
    resourceLoader,
  });
  // The desktop tools reach the model only through the MCP adapter extension, so a silent
  // load failure would leave the agent blind with no other symptom.
  for (const e of extensionsResult.errors) {
    console.error(`extension failed: ${e.path}: ${e.error}`);
  }
  console.log(
    `extensions: ${extensionsResult.extensions.map((e) => e.path).join(", ") || "none"} | ` +
      `tools: ${created.agent.state.tools.map((t) => t.name).join(", ") || "none"}`,
  );
  created.subscribe(onSessionEvent);
  return created;
}

async function ensureSession(): Promise<AgentSession> {
  if (session) return session;
  if (!starting) {
    starting = startSession()
      .then((s) => {
        session = s;
        return s;
      })
      .finally(() => {
        starting = null;
      });
  }
  return starting;
}

/**
 * Concatenated text blocks of an assistant message, ignoring thinking and tool calls.
 * Returns "" for any other role: `message_end` also fires for the user message that opened
 * the turn, and treating that as a reply would echo the operator's own prompt back at them.
 */
function assistantText(message: unknown): string {
  const m = message as { role?: string; content?: unknown };
  if (m?.role !== "assistant" || !Array.isArray(m.content)) return "";
  return m.content
    .filter((b): b is { type: "text"; text: string } => (b as { type?: string })?.type === "text")
    .map((b) => b.text)
    .join("")
    .trim();
}

function onSessionEvent(event: AgentSessionEvent): void {
  switch (event.type) {
    case "tool_execution_start":
      emitActivity(`⚙ ${event.toolName}`);
      break;
    case "message_end": {
      const text = assistantText(event.message);
      if (text) {
        lastReply = text;
        emitActivity(text);
      }
      break;
    }
    case "agent_end": {
      // pi surfaces a stream failure as an errorMessage on the final assistant message
      // rather than throwing out of prompt().
      const last = event.messages[event.messages.length - 1] as { errorMessage?: string } | undefined;
      if (last?.errorMessage) lastError = last.errorMessage;
      break;
    }
    case "agent_settled":
      finishTurn();
      break;
    default:
      break;
  }
}

function finishTurn(): void {
  if (!turnActive) return;
  const wasInterrupted = interruptRequested;
  turnActive = false;
  interruptRequested = false;
  if (wasInterrupted) {
    emit({ reply: "⏹ Stopped.", solicited: true });
  } else if (lastReply) {
    emit({ reply: lastReply, solicited: true });
  } else if (lastError) {
    emit({ error: lastError });
  } else {
    emit({ reply: "(no response)", solicited: true });
  }
  emit({ busy: false });
}

// ---- SSE response ----------------------------------------------------------
const HEARTBEAT_MS = 20_000;

function sseResponse(req: Request): Response {
  const enc = new TextEncoder();
  const stream = new ReadableStream<Uint8Array>({
    start(controller) {
      let closed = false;
      const send = (s: string) => {
        if (closed) return;
        try {
          controller.enqueue(enc.encode(s));
        } catch {
          cleanup();
        }
      };
      send(`data: ${JSON.stringify({ busy: turnActive })}\n\n`);
      subs.add(send);
      const ping = setInterval(() => send(": ping\n\n"), HEARTBEAT_MS);

      function cleanup() {
        if (closed) return;
        closed = true;
        clearInterval(ping);
        subs.delete(send);
        req.signal.removeEventListener("abort", cleanup);
        try {
          controller.close();
        } catch {
          // already closed
        }
      }
      req.signal.addEventListener("abort", cleanup);
    },
  });
  return new Response(stream, {
    headers: {
      "Content-Type": "text/event-stream",
      "Cache-Control": "no-cache, no-transform",
      Connection: "keep-alive",
      "X-Accel-Buffering": "no",
    },
  });
}

// ---- HTTP server -----------------------------------------------------------
const server = Bun.serve({
  port: CONFIG.port,
  hostname: "0.0.0.0",
  idleTimeout: 0,
  async fetch(req): Promise<Response> {
    const { pathname } = new URL(req.url);
    const { method } = req;

    if (method === "GET" && pathname === "/health") return new Response("ok");

    if (method === "GET" && pathname === "/events") return sseResponse(req);

    if (method === "POST" && pathname === "/abort") {
      if (turnActive) interruptRequested = true;
      try {
        await session?.abort();
      } catch {
        // best-effort
      }
      return Response.json({ ok: true });
    }

    if (method === "POST" && pathname === "/prompt") {
      if (turnActive) return Response.json({ error: "busy" }, { status: 409 });
      let body: unknown;
      try {
        body = await req.json();
      } catch {
        return Response.json({ error: "invalid json" }, { status: 400 });
      }
      const text = typeof (body as { text?: unknown })?.text === "string" ? (body as { text: string }).text.trim() : "";
      if (!text) return Response.json({ error: "body must be { text }" }, { status: 400 });

      let active: AgentSession;
      try {
        active = await ensureSession();
      } catch (e) {
        const msg = e instanceof Error ? e.message : String(e);
        console.error(`session start failed: ${msg}`);
        return Response.json({ error: msg }, { status: 503 });
      }

      turnActive = true;
      lastReply = "";
      lastError = "";
      interruptRequested = false;
      emit({ busy: true });
      // Fire and forget: the reply rides /events, and the control-server has its own
      // idle and turn-length watchdogs.
      void active.prompt(text).catch((e: unknown) => {
        lastError = e instanceof Error ? e.message : String(e);
        finishTurn();
      });
      return Response.json({ ok: true }, { status: 202 });
    }

    return new Response("not found", { status: 404 });
  },
});

console.log(
  `agent-wrapper listening on http://0.0.0.0:${server.port} ` +
    `(model ${CONFIG.model}, thinking ${CONFIG.thinkingLevel})`,
);
