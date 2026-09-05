import { isAbsolute, relative, resolve, sep } from "node:path";

export type ToolResult = {
  content: Array<{ type: string; text?: string; [key: string]: unknown }>;
  details?: Record<string, any>;
  isError?: boolean;
};

export interface RemoteRecord {
  id: string;
  parent: string;
  sessionId: string;
  cwd: string;
  agent: string;
  startedAt: number;
  state: string;
  address?: string;
  nativeId?: string;
  notified?: boolean;
  error?: string;
}

export interface Snapshot {
  state: string;
  nativeId?: string;
  progress?: Record<string, any>;
  result?: Record<string, any>;
  error?: string;
}

export function isActive(state: string): boolean {
  return ["creating", "queued", "running"].includes(state);
}

export function validateCwd(cwd: string, home: string): string {
  const absolute = resolve(cwd);
  const rel = relative(resolve(home), absolute);
  if (!rel || rel === ".." || rel.startsWith(".." + sep) || isAbsolute(rel)) {
    throw new Error("Subclone cwd must be a project directory below the clone user's home.");
  }
  if (rel === ".pi" || rel.startsWith(".pi" + sep)) {
    throw new Error("Subclone cwd cannot overwrite pi session and host state under .pi.");
  }
  return absolute;
}

export function validateLaunch(params: Record<string, any>): void {
  if (typeof params.agent !== "string" || !params.agent.trim()) {
    throw new Error("Subclone isolation requires one named agent per call.");
  }
  for (const key of ["action", "workflow", "workflowScript", "workflowScriptPath", "chain", "tasks", "parallel", "baseRef"]) {
    if (params[key] !== undefined) throw new Error("Subclone isolation does not accept " + key + ". Launch each child separately.");
  }
  if (params.worktree) throw new Error("Subclone isolation cannot also request a git worktree.");
  if (params.context === "fork") throw new Error("Subclone isolation requires fresh context. Include the needed context in the task.");
  if (params.share) throw new Error("Subclone isolation does not upload sessions.");
}

export function resultText(snapshot: Snapshot): string {
  if (snapshot.error) return snapshot.error;
  const children = snapshot.result?.results;
  if (Array.isArray(children)) {
    const text = children.map((child) => child.output || child.error || "").filter(Boolean).join("\n\n");
    if (text) return text;
  }
  return snapshot.result?.summary || snapshot.progress?.error || ("Subclone state: " + snapshot.state);
}

export function toolResult(record: RemoteRecord, snapshot?: Snapshot): ToolResult {
  const state = snapshot?.state ?? record.state;
  const text = snapshot ? resultText(snapshot) : record.error || ("Subclone state: " + state);
  const waiting = isActive(state) ? "\nThe child runs in the background. Finish this turn to await its completion message." : "";
  const files = state === "closed" ? "The subclone and its files were deleted." : "Files remain in " + record.cwd + " on " + record.id + ".";
  return {
    content: [{ type: "text", text: record.id + "\n" + text + waiting + "\n\n" + files }],
    details: {
      mode: "management", results: [], runId: record.id,
      rmng: { clone: record.id, state, cwd: record.cwd, nativeId: snapshot?.nativeId ?? record.nativeId },
    },
    ...(["failed", "rejected", "unreachable"].includes(state) ? { isError: true } : {}),
  };
}
