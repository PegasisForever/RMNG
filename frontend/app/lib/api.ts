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

/** Clone payload: an existing ticket link/id, a new ticket to create (in team
 *  `team`, using the chosen preset's Linear key), or a plain no-ticket clone
 *  (just a container title + an optional first agent message).
 *  The ticket modes also accept optional host-agent + Claude Code overrides.
 *  `claudeAccount` (all modes) picks the Claude account to run under — an email,
 *  "auto" (the default when omitted), "group:<name>", or "none" (install no token).
 *  `preset` picks the clone preset (env vars + Linear key): omitted/"auto" means
 *  auto-select by ticket labels (ticket mode); create/plain require a name. */
export type ClonePayload = (
  | ((
      | { ticket: string }
      | { create: { team: string; title: string; description: string } }
    ) & { agentInstructions?: string; claudeInstructions?: string })
  | { plain: { title: string; message: string } }
) & { claudeAccount?: string; preset?: string };

export const activate = (id: string | null) =>
  postJson("/api/activate", { id });
export const reorder = (order: string[]) => postJson("/api/reorder", { order });
/** Start a clone from a source image (`image` = a canonical reference from
 *  `listImages`, e.g. `rmng/template:my-base`). Progress streams over /events. */
export const cloneHost = (image: string, payload: ClonePayload) =>
  postJson("/api/clone", { image, ...payload });
export const deleteHost = (id: string) => postJson("/api/delete", { id });
/** Replace a host's port-forward rules. New rules omit `id` (server derives it as
 *  `f<localPort>`). 400 on a local-port conflict (validated server-side); the UI
 *  refreshes from the next `/events` frame. */
export const putForwards = (
  hostId: string,
  forwards: Array<{ id?: string; remotePort: number; localPort: number; enabled: boolean; label?: string }>,
) => putJson(`/api/hosts/${encodeURIComponent(hostId)}/forwards`, { forwards });

// --- images (clone-source templates) ---------------------------------------

/** The clone-source images (`rmng.image=1`); each carries the host ids of live
 *  clones running on it (`inUseBy`). Powers the sidebar Images section + the
 *  clone dialog's image picker. */
export const listImages = () => getJson("/api/images") as Promise<ImageInfo[]>;
/** Pull the clone template from a registry (`reference`, e.g. `pegasis0/rmng-template:latest`)
 *  and retag it locally as `rmng/template:<name>`. Omitted/blank `reference` falls back
 *  server-side to `docker.templateReference`. Returns the driving Operation (kind `pull`);
 *  progress streams over /events. */
export const pullTemplate = (name: string, reference?: string) =>
  postJson("/api/images/pull", { name, reference });
/** Commit a running clone to a new clone-source image `rmng/template:<name>`.
 *  Returns the driving Operation (kind `commit`); progress streams over /events. */
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
export const checkClaudeImport = (host: string) =>
  postJson("/api/claude/import/check", { host }) as Promise<{
    email: string;
    orgName: string | null;
    subscriptionType: string | null;
  }>;
/** Import a Claude account from a signed-in clone: the server harvests the clone's
 *  OAuth pair (and owns its refresh lifecycle), then clears the clone's credentials file. */
export const importClaudeAccount = (host: string) =>
  postJson("/api/claude/import", { host }) as Promise<{ email: string; cleared: boolean }>;
/** The account the clone dialog should pre-select (scored by usage + load). */
export const recommendedClaudeAccount = () =>
  getJson("/api/claude/recommended") as Promise<{ email: string | null }>;
/** Change a clone's Claude account/group. `account` is "auto", "none", an email, or
 *  "group:<name>". `account` in the reply is null when set to "none". */
export const swapClaudeAccount = (host: string, account: string) =>
  postJson("/api/claude/swap", { host, account }) as Promise<{
    ok: boolean;
    account: string | null;
    group: string | null;
    selection: string;
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
/** Apply the saved monitor layout to all running clones (rewrites RMNG_MONITORS +
 *  restarts each clone's GNOME session + daemon). Restarts the clones' desktops. */
export const applyMonitors = () =>
  postJson("/api/monitors/apply", {}) as Promise<{ ok: boolean; applied: string[]; errors: string[] }>;
