// Small HTTP wrapper around the Claude Agent SDK, run inside each RDP container
// (one process per host) on :4096 — the slot opencode used to occupy.
//
// It holds ONE long-lived streaming-input session (created lazily on the first
// prompt and kept alive for the process lifetime). Streaming input is what makes
// the monitoring loop work: the agent can start a background command (Bash
// run_in_background), end its turn, and be re-engaged AUTOMATICALLY when that
// command exits (a `task_notification` arrives in the still-open stream) — no new
// user message needed. The desktop operating notes + the per-host "implement a ticket"
// procedure are injected as the system-prompt append below — the Settings-editable copy
// the control-server injects at clone creation if present, else the baked-in default
// (see instructions.ts / agent-instructions.md). Separately, the clone's shared
// ~/.claude/CLAUDE.md (general engineering guidance, deployed by provision-clone.sh) is
// read as user memory via settingSources — see buildOptions.
//
// Turns are either:
//   • solicited   — answering a POST /prompt; the reply rides /events as
//                   { reply, solicited:true } and the control-server persists it.
//   • autonomous  — triggered by a background task finishing (monitoring); its
//                   reply rides as { reply, solicited:false }.
//
//   POST /prompt { text }   queue a user turn. 202 immediately; 409 if a turn is
//                           already running. Reply + progress arrive on /events.
//   GET  /events            SSE: { busy } snapshot, then { activity } lines, then
//                           { reply, solicited } / { error } per turn.
//   POST /abort             interrupt the current turn (session stays alive).
//   GET  /status            { busy, monitoring, sessionId }.
//
// Session id is in memory only: a CoW clone boots a fresh wrapper and starts a
// brand-new conversation. Auth = the container's logged-in `claude` subscription.
import { query, type McpServerConfig, type Options, type Query, type SDKUserMessage } from "@anthropic-ai/claude-agent-sdk";
import { hostname } from "node:os";

import { resolveSystemAppend } from "./instructions";

import { CONFIG } from "./config";

// The system-prompt append for THIS host's session agent: the control-server-injected,
// Settings-editable playbook (operating notes + ticket procedure) if present, else the
// baked-in default. Read once at startup — a fresh clone boots a fresh wrapper. It is NOT
// placed in ~/.claude/CLAUDE.md: the inner Cursor Claude Code reads that file and would
// recursively try to open Cursor. See instructions.ts + agent-wrapper/README.md.
const SYSTEM_APPEND = resolveSystemAppend(CONFIG.instructionsPath);

const ACTIVITY_MAX = 200;
// Safety: if an autonomous turn is marked active but never produces a result
// (shouldn't happen), don't wedge /prompt behind a 409 forever.
const STUCK_TURN_MS = 180_000;

// ---- a push-able async iterable of user messages (the session's input) -----
class Inbox implements AsyncIterable<SDKUserMessage> {
  private queue: SDKUserMessage[] = [];
  private resolvers: ((r: IteratorResult<SDKUserMessage>) => void)[] = [];
  private closed = false;

  push(text: string): void {
    const m: SDKUserMessage = {
      type: "user",
      parent_tool_use_id: null,
      message: { role: "user", content: text },
    };
    const r = this.resolvers.shift();
    if (r) r({ value: m, done: false });
    else this.queue.push(m);
  }
  close(): void {
    this.closed = true;
    let r: ((x: IteratorResult<SDKUserMessage>) => void) | undefined;
    while ((r = this.resolvers.shift())) r({ value: undefined as never, done: true });
  }
  [Symbol.asyncIterator](): AsyncIterator<SDKUserMessage> {
    return {
      next: () => {
        if (this.queue.length) return Promise.resolve({ value: this.queue.shift()!, done: false });
        if (this.closed) return Promise.resolve({ value: undefined as never, done: true });
        return new Promise((res) => this.resolvers.push(res));
      },
    };
  }
}

// ---- session state (one per process) --------------------------------------
let inbox: Inbox | null = null;
let session: Query | null = null;
let sessionId: string | null = null;
let turnActive = false; // a turn (solicited or autonomous) is running
let userTurnPending = false; // the active turn answers a POST /prompt
let turnStartedAt = 0;
let interruptRequested = false; // a /abort (Stop button) hit the active turn

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

/** `mcp__desktop__screenshot` → `desktop:screenshot`; `Bash` stays `Bash`. */
function toolLabel(name: string): string {
  const m = /^mcp__(.+?)__(.+)$/.exec(name);
  return m ? `${m[1]}:${m[2]}` : name;
}

// ---- MCP + query options ---------------------------------------------------
function mcpServers(): Record<string, McpServerConfig> {
  const servers: Record<string, McpServerConfig> = {
    // The desktop-control MCP is served by the clone-daemon over HTTP (localhost),
    // sharing its live Mutter session. Registered as "desktop" ("computer-use" is a
    // reserved MCP name); alwaysLoad keeps screenshot/click/… in context every turn.
    desktop: { type: "http", url: CONFIG.daemonMcpUrl, alwaysLoad: true },
    // The per-clone control-server MCP routes by the caller's self-reported clone id
    // (clone IPs are dynamic Docker IPAM — no source-IP mapping). hostname() == the
    // clone's container name == its host id.
    "control-server": {
      type: "http",
      url: CONFIG.controlMcpUrl,
      headers: { "x-rmng-clone": hostname() },
    },
  };

  // The clone's preset Linear identity (LINEAR_API_KEY, injected at clone creation).
  // Interactive `claude` gets the same server from ~/.claude.json (user scope).
  if (CONFIG.linearApiKey) {
    servers.linear = {
      type: "http",
      url: "https://mcp.linear.app/mcp",
      headers: { Authorization: `Bearer ${CONFIG.linearApiKey}` },
    };
  }
  return servers;
}

function buildOptions(): Options {
  return {
    model: CONFIG.model,
    executable: CONFIG.executable,
    // The clone's standalone Claude Code (native binary). Required because this
    // wrapper is a bun-compiled single-exec: the SDK can't resolve its own cli.js
    // from the bunfs, so without this it throws "Native CLI binary … not found".
    pathToClaudeCodeExecutable: CONFIG.claudeExecutable,
    // Adaptive thinking (model decides when/how much to think) at high effort —
    // set explicitly rather than relying on the CLI defaults.
    thinking: { type: "adaptive" },
    effort: "high",
    cwd: process.env.HOME ?? undefined,
    permissionMode: "bypassPermissions",
    allowDangerouslySkipPermissions: true,
    // Read ~/.claude from disk so the agent picks up the clone's shared user memory
    // (~/.claude/CLAUDE.md — general engineering guidance deployed by provision-clone.sh,
    // and also read by the inner Cursor Claude Code and any human `claude`). The
    // wrapper-only desktop notes + ticket procedure ride SYSTEM_APPEND above, NOT that
    // file. "user" also loads ~/.claude/settings.json (theme, etc.), but permissionMode/
    // mcpServers/model are all set programmatically here and override it. Auth is
    // independent — the container's ~/.claude/.credentials.json subscription.
    settingSources: ["user"],
    // Claude Code preset + the code-injected operating notes + per-host ticket procedure.
    systemPrompt: {
      type: "preset",
      preset: "claude_code",
      ...(SYSTEM_APPEND ? { append: SYSTEM_APPEND } : {}),
    },
    mcpServers: mcpServers(),
    stderr: (data: string) => process.stderr.write(`[claude-code] ${data}`),
  };
}

// ---- the persistent session ------------------------------------------------
function ensureSession(): void {
  if (session) return;
  inbox = new Inbox();
  session = query({ prompt: inbox, options: buildOptions() });
  void consume(session);
}

/** Read the session stream forever, dispatching every message. */
async function consume(q: Query): Promise<void> {
  try {
    for await (const msg of q) {
      switch (msg.type) {
        case "system":
          if (msg.subtype === "init") {
            sessionId = msg.session_id;
            const failed = msg.mcp_servers.filter((s) => s.status !== "connected");
            if (failed.length) {
              console.warn(`MCP not connected: ${failed.map((s) => `${s.name}(${s.status})`).join(", ")}`);
            }
          } else if (msg.subtype === "task_notification") {
            // A background command finished — the agent will re-engage on its own.
            turnActive = true;
            turnStartedAt = Date.now();
            emitActivity(`🔔 background task ${msg.status}`);
          } else if (msg.subtype === "task_started") {
            emitActivity(`▶ background task started`);
          }
          break;
        case "assistant":
          sessionId = msg.session_id;
          turnActive = true;
          for (const block of msg.message.content) {
            if (block.type === "text") {
              if (block.text.trim()) emitActivity(block.text);
            } else if (block.type === "tool_use") {
              emitActivity(`⚙ ${toolLabel(block.name)}`);
            }
          }
          break;
        case "result": {
          sessionId = msg.session_id;
          const wasUser = userTurnPending;
          const wasInterrupted = interruptRequested;
          userTurnPending = false;
          turnActive = false;
          interruptRequested = false;
          if (wasInterrupted) {
            // Stop button: emit a clean note instead of the raw interrupt diagnostic.
            emit({ reply: "⏹ Stopped.", solicited: wasUser });
            if (wasUser) emit({ busy: false });
            break;
          }
          const ok = msg.subtype === "success";
          const text = ok ? (msg.result || "").trim() : "";
          const errs = ok ? "" : (msg.errors && msg.errors.join("; ")) || msg.subtype;
          if (wasUser) {
            if (text) emit({ reply: text, solicited: true });
            else if (errs) emit({ error: errs });
            else emit({ reply: "(no response)", solicited: true });
            emit({ busy: false });
          } else if (text) {
            emit({ reply: text, solicited: false }); // autonomous (monitoring) message
          } else if (errs) {
            console.warn(`autonomous turn error: ${errs}`);
          }
          break;
        }
        default:
          break; // partial messages, tool progress, etc.
      }
    }
  } catch (e) {
    console.error(`session stream ended: ${e instanceof Error ? e.message : String(e)}`);
  } finally {
    // Session died — reset so the next /prompt starts a fresh one.
    if (userTurnPending) emit({ error: "agent session ended" });
    session = null;
    inbox = null;
    turnActive = false;
    userTurnPending = false;
    emit({ busy: false });
  }
}

// Clear a wedged autonomous turn flag (defensive; a real turn always resolves).
setInterval(() => {
  if (turnActive && !userTurnPending && Date.now() - turnStartedAt > STUCK_TURN_MS) {
    turnActive = false;
  }
}, 30_000);

// ---- monitoring detection (for the dashboard's monitorState) ---------------
function isMonitoring(): boolean {
  try {
    return Bun.spawnSync(["pgrep", "-f", "rmng-clone-daemon wait-for-stuck"]).exitCode === 0;
  } catch {
    return false;
  }
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
      send(`data: ${JSON.stringify({ busy: userTurnPending })}\n\n`);
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

    if (method === "GET" && pathname === "/status") {
      return Response.json({ busy: userTurnPending, monitoring: isMonitoring(), sessionId });
    }

    if (method === "GET" && pathname === "/events") return sseResponse(req);

    if (method === "POST" && pathname === "/abort") {
      if (turnActive) interruptRequested = true;
      try {
        await session?.interrupt();
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

      ensureSession();
      turnActive = true;
      userTurnPending = true;
      turnStartedAt = Date.now();
      emit({ busy: true });
      inbox!.push(text);
      return Response.json({ ok: true }, { status: 202 });
    }

    return new Response("not found", { status: 404 });
  },
});

console.log(
  `agent-wrapper listening on http://0.0.0.0:${server.port} ` +
    `(model ${CONFIG.model}, executable ${CONFIG.executable}, control-mcp ${CONFIG.controlMcpUrl})`,
);
