import type { AppConfigRedacted } from "~/lib/wire/AppConfigRedacted";
import type { ConfigPutResponse } from "~/lib/wire/ConfigPutResponse";

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
export const cloneHost = (source: string, payload: ClonePayload) =>
  postJson("/api/clone", { source, ...payload });
export const deleteHost = (id: string) => postJson("/api/delete", { id });
/** Provision a brand-new template CT from the fixed Ubuntu 26.04 base image (the
 *  only base the patched GNOME is built for), with resources from the New-template
 *  modal. The new CT is registered as a clonable template. Progress streams over
 *  /events like a clone. (Bootstrap errors are plain text, so read the body as
 *  text on failure.) */
export async function bootstrapTemplate(
  hostname: string,
  resources: { cores: number; memoryMb: number; diskGb: number },
): Promise<{ id: string }> {
  const res = await fetch("/api/template/bootstrap", {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ hostname, ...resources }),
  });
  if (!res.ok) throw new Error((await res.text().catch(() => "")) || res.statusText);
  return res.json().catch(() => ({})) as Promise<{ id: string }>;
}
/** Hot-swap a clone's clone-daemon (+ agent-wrapper unless daemonOnly) binaries from the
 *  control-server's embedded copies, without reprovisioning. Restarts the unit(s). */
export const redeployClone = (id: string, daemonOnly = false) =>
  postJson("/api/clone/redeploy", { id, daemonOnly });

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
 *  apply restart-scoped settings (ports, cloneSocket, staticDir, chroma). */
export const putConfig = (patch: unknown) =>
  putJson("/api/config", patch) as Promise<ConfigPutResponse>;
/** Validate a setting (e.g. `proxmox` SSH reachability). */
export const testConfig = (what: string) =>
  postJson("/api/config/test", { what }) as Promise<{ ok: boolean; message: string }>;
/** Apply the saved monitor layout to all running clones (rewrites RMNG_MONITORS +
 *  restarts each clone's GNOME session + daemon). Restarts the clones' desktops. */
export const applyMonitors = () =>
  postJson("/api/monitors/apply", {}) as Promise<{ ok: boolean; applied: string[]; errors: string[] }>;
