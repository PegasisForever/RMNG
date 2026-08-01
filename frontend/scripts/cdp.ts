/**
 * A tiny Chrome DevTools Protocol client, enough to drive headless Chrome from a Bun
 * script. Bun ships a native WebSocket, so this needs no dependency.
 *
 * Each tab gets its own socket. One socket carrying every tab's console traffic is a
 * single point of failure: when it drops, every in-flight request waits forever and the
 * run wedges with no error. Per-tab sockets keep one dead tab from taking the run with
 * it, and every request carries a deadline so a lost reply fails loudly.
 */

import { spawn, type ChildProcess } from "node:child_process";
import { accessSync, constants, mkdtempSync, rmSync, statSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

type Json = Record<string, unknown>;

interface Pending {
  resolve: (value: Json) => void;
  reject: (reason: Error) => void;
  timer: ReturnType<typeof setTimeout>;
}

const REQUEST_TIMEOUT_MS = 30_000;

/** One WebSocket to one DevTools endpoint, either the browser or a single page. */
export class Cdp {
  private socket: WebSocket;
  private nextId = 1;
  private pending = new Map<number, Pending>();
  private listeners = new Set<(method: string, params: Json) => void>();
  private dead: Error | null = null;

  private constructor(socket: WebSocket) {
    this.socket = socket;
    socket.addEventListener("message", (event) => {
      const raw = typeof event.data === "string" ? event.data : "";
      if (!raw) return;
      const msg = JSON.parse(raw) as {
        id?: number;
        method?: string;
        params?: Json;
        result?: Json;
        error?: { message?: string };
      };
      if (typeof msg.id === "number") {
        const slot = this.pending.get(msg.id);
        if (!slot) return;
        this.pending.delete(msg.id);
        clearTimeout(slot.timer);
        if (msg.error) slot.reject(new Error(msg.error.message ?? "CDP error"));
        else slot.resolve(msg.result ?? {});
        return;
      }
      if (typeof msg.method === "string") {
        for (const fn of this.listeners) fn(msg.method, msg.params ?? {});
      }
    });
    socket.addEventListener("close", () => this.fail(new Error("CDP socket closed")));
    socket.addEventListener("error", () => this.fail(new Error("CDP socket errored")));
  }

  /** Mark the connection unusable and settle everything still waiting on it. */
  private fail(reason: Error): void {
    if (this.dead) return;
    this.dead = reason;
    for (const slot of this.pending.values()) {
      clearTimeout(slot.timer);
      slot.reject(reason);
    }
    this.pending.clear();
  }

  static async connect(url: string): Promise<Cdp> {
    const socket = new WebSocket(url);
    await new Promise<void>((resolve, reject) => {
      const timer = setTimeout(() => reject(new Error(`CDP connect timed out: ${url}`)), 20_000);
      socket.addEventListener(
        "open",
        () => {
          clearTimeout(timer);
          resolve();
        },
        { once: true },
      );
      socket.addEventListener(
        "error",
        () => {
          clearTimeout(timer);
          reject(new Error(`CDP connect failed: ${url}`));
        },
        { once: true },
      );
    });
    return new Cdp(socket);
  }

  send(method: string, params: Json = {}): Promise<Json> {
    if (this.dead) return Promise.reject(this.dead);
    const id = this.nextId++;
    return new Promise<Json>((resolve, reject) => {
      const timer = setTimeout(() => {
        this.pending.delete(id);
        reject(new Error(`CDP request timed out after ${REQUEST_TIMEOUT_MS}ms: ${method}`));
      }, REQUEST_TIMEOUT_MS);
      this.pending.set(id, { resolve, reject, timer });
      try {
        this.socket.send(JSON.stringify({ id, method, params }));
      } catch (err) {
        clearTimeout(timer);
        this.pending.delete(id);
        reject(err instanceof Error ? err : new Error(String(err)));
      }
    });
  }

  /** Subscribe to every protocol event on this connection. Returns an unsubscribe. */
  on(fn: (method: string, params: Json) => void): () => void {
    this.listeners.add(fn);
    return () => {
      this.listeners.delete(fn);
    };
  }

  close(): void {
    this.fail(new Error("CDP connection closed locally"));
    try {
      this.socket.close();
    } catch {
      // Closing an already-dead socket is not a failure worth reporting.
    }
  }
}

/** A single tab, with its own connection. */
export interface Page {
  cdp: Cdp;
  close(): Promise<void>;
}

export interface Browser {
  newPage(): Promise<Page>;
  kill(): void;
}

const CHROME_CANDIDATES = [
  "/usr/bin/google-chrome",
  "/usr/bin/google-chrome-stable",
  "/usr/bin/chromium",
  "/usr/bin/chromium-browser",
];

/** The first candidate that is a file this process can execute.
 *
 *  `Bun.file(path).size` cannot answer this: it reports 0 for a path that does not exist, so
 *  every candidate looks present and the first one always wins. `accessSync` with `X_OK` is
 *  the question actually being asked, and `statSync` rejects a directory that happens to sit
 *  at the path. Both follow symlinks, which is what `/usr/bin/google-chrome` usually is.
 *
 *  `candidates` is a parameter so a test can hand it a list that resolves to nothing. */
export function findChrome(
  candidates: readonly (string | undefined)[] = [process.env.CHROME_PATH, ...CHROME_CANDIDATES],
): string {
  const tried = candidates.filter((p): p is string => !!p);
  for (const path of tried) {
    try {
      accessSync(path, constants.X_OK);
      if (statSync(path).isFile()) return path;
    } catch {
      // Absent, not executable, or not a file. The next candidate gets its turn.
    }
  }
  throw new Error(
    `No usable Chrome binary. Set CHROME_PATH to an executable. Tried: ${tried.join(", ") || "(nothing)"}`,
  );
}

/** Launch headless Chrome and wait for its DevTools endpoint to answer. */
export async function launch(): Promise<Browser> {
  const profile = mkdtempSync(join(tmpdir(), "sb-sweep-"));
  const port = 9222 + Math.floor(Math.random() * 2000);
  const child: ChildProcess = spawn(
    findChrome(),
    [
      "--headless=new",
      `--remote-debugging-port=${port}`,
      `--user-data-dir=${profile}`,
      "--no-first-run",
      "--no-default-browser-check",
      "--disable-gpu",
      "--disable-dev-shm-usage",
      "--disable-extensions",
      "--hide-scrollbars",
      "--window-size=1600,1000",
      "about:blank",
    ],
    { stdio: "ignore" },
  );

  const deadline = Date.now() + 30_000;
  let ready = false;
  while (Date.now() < deadline) {
    try {
      const res = await fetch(`http://127.0.0.1:${port}/json/version`);
      if (res.ok) {
        ready = true;
        break;
      }
    } catch {
      // Chrome has not bound the port yet.
    }
    await new Promise((r) => setTimeout(r, 200));
  }
  if (!ready) {
    child.kill("SIGKILL");
    throw new Error("Chrome did not expose a DevTools endpoint within 30s");
  }

  return {
    async newPage(): Promise<Page> {
      const res = await fetch(`http://127.0.0.1:${port}/json/new?about:blank`, { method: "PUT" });
      if (!res.ok) throw new Error(`Could not open a tab: ${res.status}`);
      const target = (await res.json()) as { id: string; webSocketDebuggerUrl: string };
      const cdp = await Cdp.connect(target.webSocketDebuggerUrl);
      return {
        cdp,
        async close() {
          cdp.close();
          await fetch(`http://127.0.0.1:${port}/json/close/${target.id}`).catch(() => undefined);
        },
      };
    },
    kill() {
      child.kill("SIGKILL");
      try {
        rmSync(profile, { recursive: true, force: true });
      } catch {
        // A leftover profile directory in the OS temp dir is harmless.
      }
    },
  };
}
