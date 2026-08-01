import type { AppConfigRedacted } from "~/lib/wire/AppConfigRedacted";
import type { BoardColumn } from "~/lib/wire/BoardColumn";
import type { ConfigPutResponse } from "~/lib/wire/ConfigPutResponse";
import type { ImageInfo } from "~/lib/wire/ImageInfo";
import type { LinearTicket } from "~/lib/wire/LinearTicket";
// The hand-maintained `Operation`, not the generated `wire/Operation`: ts-rs maps the
// Rust `u64` timestamps to `bigint`, but `JSON.parse` yields plain numbers, so the
// hand-maintained shape is the one these responses actually have at runtime.
import type { Operation } from "~/lib/types";
import type { SetupEnv } from "~/lib/wire/SetupEnv";
import type { UpdateStatus } from "~/lib/wire/UpdateStatus";

// Client-side API wrappers. Each POSTs JSON; the server mutates state and
// broadcasts, so the caller doesn't need the response beyond error handling —
// the UI updates when the SSE frame arrives.
async function postJson(url: string, body: unknown): Promise<unknown> {
  const res = await fetch(url, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify(body),
  });
  const data = (await res.json().catch(() => ({}))) as { error?: string };
  if (!res.ok) throw new Error(data.error ?? res.statusText);
  return data;
}

async function getJson(url: string): Promise<unknown> {
  const res = await fetch(url);
  const data = (await res.json().catch(() => ({}))) as { error?: string };
  if (!res.ok) throw new Error(data.error ?? res.statusText);
  return data;
}

async function delJson(url: string): Promise<unknown> {
  const res = await fetch(url, { method: "DELETE" });
  const data = (await res.json().catch(() => ({}))) as { error?: string };
  if (!res.ok) throw new Error(data.error ?? res.statusText);
  return data;
}

/** Clone payload: an existing ticket link/id, a new ticket to create (in team
 *  `team`, using the resolved preset's Linear key — `create.description` is markdown),
 *  or a plain no-ticket clone (just a container title + an optional first agent message).
 *  The ticket modes also accept optional clone-agent + Claude Code overrides.
 *  `group` (all modes) OVERRIDES the account pool the clone binds; omit it to let the
 *  server resolve it (preset default → first configured group). Every clone binds one.
 *  `preset` picks the clone preset (env vars + Linear key): omitted means auto-select by
 *  ticket-id prefix (ticket mode); create/plain send a resolved name.
 *  `parent` nests the new clone as a sub clone under that clone id. */
export type ClonePayload = (
  | ((
      | { ticket: string }
      | { create: { team: string; title: string; description: string } }
    ) & { agentInstructions?: string; claudeInstructions?: string })
  | { plain: { title: string; message: string } }
) & { group?: string; preset?: string; headless?: boolean; parent?: string };

export const activate = (id: string | null) =>
  postJson("/api/activate", { id });
/** Start a clone from a source image (`image` = a canonical reference from
 *  `listImages`, e.g. `pegasis0/rmng-template:latest`). Returns the driving Operation so
 *  the caller can follow it; progress streams over /events. */
export const duplicateClone = (image: string, payload: ClonePayload) =>
  postJson("/api/clone", { image, ...payload }).then(
    (r) => (r as { op: Operation }).op,
  );
export const deleteClone = (id: string) => postJson("/api/delete", { id });
/** Gracefully stop a managed clone while retaining its container and per-clone data. */
export const archiveClone = (id: string) =>
  postJson(`/api/hosts/${encodeURIComponent(id)}/archive`, {});
/** Restart a retained archived clone. */
export const unarchiveClone = (id: string) =>
  postJson(`/api/hosts/${encodeURIComponent(id)}/unarchive`, {});
/** Replace a clone's port-forward rules. New rules omit `id` (server derives it as
 *  `f<localPort>`). 400 on a local-port conflict (validated server-side); the UI
 *  refreshes from the next `/events` frame. */
export const putForwards = (
  cloneId: string,
  forwards: Array<{ id?: string; remotePort: number; localPort: number; enabled: boolean; label?: string }>,
) => putJson(`/api/hosts/${encodeURIComponent(cloneId)}/forwards`, { forwards });

/** Replace the board's columns wholesale. The client owns the layout rules and sends the
 *  settled list; the server just stores it and broadcasts the new state. */
export const putBoardColumns = (columns: BoardColumn[]) =>
  putJson("/api/board", { columns });

/** Write a title and/or description back to Linear. The browser holds no Linear key, so the
 *  server does the mutation and answers with the patched state. An omitted field is left
 *  alone; an empty description clears the body, which is a thing operators do on purpose. */
export const putTicket = (id: string, patch: { title?: string; description?: string }) =>
  putJson(`/api/tickets/${encodeURIComponent(id)}`, patch);

/** Open a new Linear issue. `team` is a team key (`WE`), which also picks the preset whose
 *  Linear key opens it. `priority` is Linear's scale: 1 urgent, 2 high, 3 medium, 4 low.
 *
 *  The server creates it as Todo and assigned to the key's owner, so the column can show it,
 *  and answers with the created ticket. The column itself updates from the `/events` frame. */
export const createTicket = (ticket: {
  team: string;
  title: string;
  description: string;
  priority?: number;
}) => postJson("/api/tickets", ticket) as Promise<LinearTicket>;

// --- images (clone-source templates) ---------------------------------------

/** The clone-source images (`rmng.image=1`); each carries the ids of the live
 *  clones running on it (`inUseBy`). Powers the sidebar Images section + the
 *  clone dialog's image picker. */
export const listImages = () => getJson("/api/images") as Promise<ImageInfo[]>;
/** Pull the clone template from a registry (`reference`, e.g. `pegasis0/rmng-template:latest`).
 *  The pulled image keeps its own `repo:tag` as the clone-source reference (no local retag).
 *  Omitted/blank `reference` falls back server-side to `docker.templateReference`. Returns the
 *  driving Operation (kind `pull`); progress streams over /events. */
export const pullTemplate = (reference?: string) =>
  postJson("/api/images/pull", { reference });
/** Commit a running clone to a new clone-source image `<name>:latest` (the name you give it
 *  is the full repo). Returns the driving Operation (kind `commit`); streams over /events. */
export const commitImage = (host: string, name: string) =>
  postJson("/api/images/commit", { host, name });
/** Remove a clone-source image by reference. 409 (with a "…in use by…" message)
 *  when a live clone or a running op still references it. */
export const deleteImage = (reference: string) =>
  postJson("/api/images/delete", { reference });
/** The environment preflight rows for the setup wizard's first step. */
export const getSetupEnv = () => getJson("/api/setup/env") as Promise<SetupEnv>;
/** The control-server's own version + whether Hub has a newer image (no pull). */
export const getUpdateStatus = () => getJson("/api/server/version") as Promise<UpdateStatus>;
/** Pull the latest control-server image and swap the running container onto it. Returns the
 *  driving Operation (kind `update`); the server restarts mid-op. */
export const updateServer = () => postJson("/api/server/update", {}) as Promise<Operation>;
/** Restart the control-server in place to apply changed startup settings. The UI briefly
 *  disconnects and reconnects. */
export const restartServer = () => postJson("/api/server/restart", {}) as Promise<{ ok: boolean }>;

/** Force an immediate Claude usage poll (refresh tokens + fetch 5h/7d). */
export const refreshClaudeUsage = () => postJson("/api/claude/refresh", {});
/** Confirm a clone is signed in to Claude Code via claude.ai; returns its identity. */
export const checkClaudeImport = (clone: string) =>
  postJson("/api/claude/import/check", { host: clone }) as Promise<{
    email: string;
    orgName: string | null;
    subscriptionType: string | null;
  }>;
/** Import a Claude account from a signed-in clone: the server harvests the clone's
 *  OAuth pair (and owns its refresh lifecycle), then clears the clone's credentials file. */
export const importClaudeAccount = (clone: string) =>
  postJson("/api/claude/import", { host: clone }) as Promise<{ email: string; cleared: boolean }>;
/** Change a clone's Claude account/group. `account` is "auto", "none", an email, or
 *  "group:<name>". `account` in the reply is null when set to "none". */
export const swapClaudeAccount = (clone: string, account: string) =>
  postJson("/api/claude/swap", { host: clone, account }) as Promise<{
    ok: boolean;
    account: string | null;
    group: string | null;
    selection: string;
  }>;

/** Delete an imported Claude account by email. Rejects (400) if a clone is pinned to it;
 *  auto/group clones are moved off first. `moved` lists the host ids that were reassigned. */
export const deleteClaudeAccount = (account: string) =>
  postJson("/api/claude/delete", { account }) as Promise<{ ok: boolean; moved: string[] }>;

export const refreshCodexUsage = () => postJson("/api/codex/refresh", {});

export const checkCodexImport = (clone: string) =>
  postJson("/api/codex/import/check", { host: clone }) as Promise<{
    email: string;
    plan: string | null;
    accountId: string;
  }>;

export const importCodexAccount = (clone: string) =>
  postJson("/api/codex/import", { host: clone }) as Promise<{ email: string; cleared: boolean }>;

export const swapCodexAccount = (clone: string, account: string) =>
  postJson("/api/codex/swap", { host: clone, account }) as Promise<{
    ok: boolean;
    account: string | null;
    group: string | null;
    selection: string;
  }>;

/** Delete an imported Codex account by email (the Codex twin of `deleteClaudeAccount`). */
export const deleteCodexAccount = (account: string) =>
  postJson("/api/codex/delete", { account }) as Promise<{ ok: boolean; moved: string[] }>;

// --- Settings / config (redacted read · partial write · validate) ----------
// Config errors come back as plain text (not the {error} JSON shape), so PUT
// reads the body as text on failure for a useful message.
async function putJson(url: string, body: unknown): Promise<unknown> {
  const res = await fetch(url, {
    method: "PUT",
    headers: { "content-type": "application/json" },
    body: JSON.stringify(body),
  });
  if (!res.ok) throw new Error((await res.text().catch(() => "")) || res.statusText);
  return res.json().catch(() => ({}));
}

/** Current config (secrets shown as set/unset booleans). */
export const getConfig = () => getJson("/api/config") as Promise<AppConfigRedacted>;
/** Merge a partial config update (empty-string secrets are left unchanged), persist,
 *  apply live. Returns the new redacted config plus whether a restart is required to
 *  apply restart-scoped settings (ports, cloneSocket, staticDir, chroma). When the
 *  patch flips `setupComplete` (wizard finish), the server also ensures the `rmng`
 *  network; a non-fatal failure rides along as `networkWarning`. */
export const putConfig = (patch: unknown) =>
  putJson("/api/config", patch) as Promise<
    ConfigPutResponse & { networkWarning?: string }
  >;
/** Validate a setting (e.g. `"docker"` — re-runs the Docker self-setup probe). */
export const testConfig = (what: string) =>
  postJson("/api/config/test", { what }) as Promise<{ ok: boolean; message: string }>;
/** Make `name` the active layout preset and live-apply it to all running clones. */
export const activateLayout = (name: string) =>
  postJson("/api/layout/activate", { name }) as Promise<{
    ok: boolean;
    applied: string[];
    errors: string[];
  }>;
