import type { AppConfigRedacted } from "~/lib/wire/AppConfigRedacted";
import type { ConfigPutResponse } from "~/lib/wire/ConfigPutResponse";
import type { ImageInfo } from "~/lib/wire/ImageInfo";
import type { Operation } from "~/lib/wire/Operation";
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
 *  `team`, using the chosen preset's Linear key), or a plain no-ticket clone
 *  (just a container title + an optional first agent message).
 *  The ticket modes also accept optional clone-agent + Claude Code overrides.
 *  `group` (all modes) binds the clone to an account pool (a CLIProxyAPI instance) —
 *  a group name, or null/omitted for no inference binding.
 *  `preset` picks the clone preset (env vars + Linear key): omitted/"auto" means
 *  auto-select by ticket-id prefix (ticket mode); create/plain require a name. */
export type ClonePayload = (
  | ((
      | { ticket: string }
      | { create: { team: string; title: string; description: string } }
    ) & { agentInstructions?: string; claudeInstructions?: string })
  | { plain: { title: string; message: string } }
) & { group?: string | null; preset?: string; headless?: boolean };

export const activate = (id: string | null) =>
  postJson("/api/activate", { id });
export const reorder = (order: string[]) => postJson("/api/reorder", { order });
/** Start a clone from a source image (`image` = a canonical reference from
 *  `listImages`, e.g. `pegasis0/rmng-template:latest`). Progress streams over /events. */
export const duplicateClone = (image: string, payload: ClonePayload) =>
  postJson("/api/clone", { image, ...payload });
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

// --- account groups (CLIProxyAPI pools) ------------------------------------
// A group is a provider-agnostic account pool = one CLIProxyAPI instance. Accounts
// enter a group by completing that instance's OAuth login (start → complete → poll).
// Usage per group is display-only and streams in `ControlState.usageGroups`.

/** OAuth provider for a group login. */
export type LoginProvider = "anthropic" | "codex" | "antigravity";

/** Create an account group (spawns its CLIProxyAPI instance). Returns the redacted config. */
export const createGroup = (name: string) =>
  postJson("/api/groups", { name }) as Promise<AppConfigRedacted>;
/** Delete an account group (stops its instance; the on-disk auth-dir is left in place). */
export const deleteGroup = (name: string) =>
  delJson(`/api/groups/${encodeURIComponent(name)}`) as Promise<AppConfigRedacted>;
/** Begin an OAuth login into a group's instance. Returns the URL the operator opens
 *  (it redirects to a `localhost` callback on the operator's machine) plus the `state`
 *  token used to poll/complete. */
export const startGroupLogin = (group: string, provider: LoginProvider) =>
  postJson(`/api/groups/${encodeURIComponent(group)}/accounts/login/start`, { provider }) as Promise<{
    status?: string;
    url: string;
    state: string;
  }>;
/** Finish an OAuth login by handing back the pasted redirect URL (or an explicit
 *  code+state). The credential lands in the group instance's auth-dir. */
export const completeGroupLogin = (
  group: string,
  body: { provider: LoginProvider; redirectUrl?: string; code?: string; state?: string },
) =>
  postJson(`/api/groups/${encodeURIComponent(group)}/accounts/login/complete`, body) as Promise<{
    status?: string;
  } & Record<string, unknown>>;
/** Poll an in-flight login. The control server normalizes CLIProxyAPI's `get-auth-status`
 *  to a small stable shape: `pending` while the instance exchanges the code, `done` once the
 *  credential lands in the group's auth-dir, `error` (with a message) on a failed/expired
 *  session. */
export const groupLoginStatus = (group: string, state: string) =>
  getJson(
    `/api/groups/${encodeURIComponent(group)}/accounts/login/status?state=${encodeURIComponent(state)}`,
  ) as Promise<{ state: "pending" | "done" | "error"; error?: string }>;
/** Remove an authenticated account from a group by its auth-dir credential file name
 *  (`claude-<email>.json` / `codex-<email>.json` / `antigravity-<email>.json`). */
export const deleteGroupAccount = (group: string, file: string) =>
  postJson(`/api/groups/${encodeURIComponent(group)}/accounts/delete`, { file });

/** Trigger an immediate server-side usage poll (the manual refresh button, and auto-fired
 *  after an account is added). The refreshed `usageGroups` arrive over SSE within ~a second. */
export const refreshUsage = (): Promise<void> =>
  postJson("/api/usage/refresh", {}).then(() => undefined);

/** Bind a clone to an account group (or clear it with `null`). Replaces the old
 *  per-provider account swap — one group backs all of a clone's agents. */
export const setCloneGroup = (cloneId: string, group: string | null) =>
  postJson(`/api/hosts/${encodeURIComponent(cloneId)}/group`, { group }) as Promise<{
    ok: boolean;
    group: string | null;
  }>;

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
