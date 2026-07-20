# CLIProxyAPI group-proxy rotation redesign

**Date:** 2026-07-19
**Status:** Planned — pending implementation sign-off
**Supersedes:** `2026-07-15-central-inference-proxy-plan.md` (and the two rotation designs it
superseded). Disregard those entirely.
**Scope:** Replace RMNG's in-house account rotation + clone-local credential injection with one
[CLIProxyAPI](https://github.com/router-for-me/CLIProxyAPI) instance **per account group**. The
control-server becomes a request router and process supervisor; CLIProxyAPI owns OAuth refresh and
intra-group account selection. Covers Claude Code, Codex, and OpenCode clients.

---

## Context

Today the control-server owns the whole token lifecycle: it harvests OAuth pairs off signed-in
clones (`import_clone_account`), refreshes them (`refresh_account`/`fresh_access_token`),
scores/rotates accounts (`claude.rs` / `codex.rs`, ~lines 650–1352), and pushes the current access
token into each clone's `~/.claude/.credentials.json` / `~/.codex/auth.json` over `docker exec`
(`apply_clone_token` / `push_stale_tokens`). Clones then call `api.anthropic.com` / `chatgpt.com`
directly.

Two problems motivate the change:

1. RMNG's rotation is **per-account** and cannot express a **per-model** limit. Anthropic's Fable
   model has its own weekly sub-limit; when Fable is exhausted on account A the account is still
   fine for Opus/Sonnet, but RMNG can only mark the whole account.
2. Maintaining bespoke scoring, refresh, and token-push machinery is a large surface. CLIProxyAPI
   already does multi-account load-balancing, per-(account, model) quota failover, OAuth refresh,
   and cross-protocol translation.

**Outcome:** a group is one CLIProxyAPI process; a clone points its agents at a constant
control-server URL; the control-server routes each request to the clone's group instance;
CLIProxyAPI picks the account (sticky per clone, fails over per model) and refreshes tokens. RMNG
keeps only usage display (reading tokens from each instance's auth-dir) and process supervision.

---

## Confirmed decisions (the contract)

1. **Group = provider-agnostic account pool = one CLIProxyAPI (v7) process.** A pool may hold
   Anthropic (Claude) **and** OpenAI (Codex/GPT) accounts. One OS process per group, loopback-only,
   own port, own `auth-dir`, own `config.yaml`. The control-server supervises them (spawn / backoff
   / restart), modeled on `ssh.rs` / `smb.rs`, but **dynamic** (spawn/kill as groups are
   created/deleted).
2. **A clone binds exactly one group.** All of its agents (Claude Code, Codex, OpenCode) route to
   that one group's instance. Selection collapses to **{group, none}**. (One shared pool per clone
   is sufficient — confirmed.)
3. **Control-server is the router.** Each clone's agents point at a constant control-server URL. The
   router identifies the clone from a per-clone bearer key, maps clone → group → instance, and
   reverse-proxies (streaming) to `127.0.0.1:<port>`. **Changing a clone's group is a map update —
   no clone-side change.**
4. **CLIProxyAPI owns intra-group selection.** `routing.session-affinity: true` pins a clone to one
   account; per-(account, model) quota + in-request cross-account failover satisfies the Fable case
   automatically (see "Fable failover" below). RMNG deletes all its scoring/rotation code.
5. **CLIProxyAPI owns OAuth refresh.** RMNG deletes refresh + token-store code. RMNG only *reads*
   the current access token from each instance's `auth-dir` to poll usage.
6. **Onboarding = re-login via CLIProxyAPI OAuth.** Accounts enter a group by completing
   CLIProxyAPI's management OAuth flow against that group's instance; the credential lands in that
   instance's `auth-dir`. RMNG no longer harvests tokens from clones.
7. **An account may live in several groups.** The operator logs it in once per group, so each
   instance holds an independent token set for that email, refreshed independently — no single-use
   refresh-token clash.
8. **Usage panel is by group.** For each group, list the accounts authenticated into it with 5h /
   7d / Fable bars. Fable stays display-only.
9. **Per-agent model visibility is soft** (personal dev tool; no hard enforcement):
   - **Claude Code** — picker shows Claude **and** GPT models (gateway model discovery from the
     instance's `/v1/models`). It sends the chosen `model` verbatim; the instance routes by model.
   - **Codex / OpenCode** — their generated configs list **GPT models only**, so Claude models never
     appear in their pickers.
10. **Migration is in place, no recreate.** The reconciler rewrites each clone's `/etc/environment`
    + agent configs and deletes the dead provider credential files; the operator restarts the agent.
    Containers are never recreated.

---

## Fable failover — why it works with zero custom code

Verified against CLIProxyAPI v7 source:

- Quota/cooldown state is keyed by **(auth_id, model)** (`Auth.ModelStates`, `MarkResult` →
  `ensureModelState`). A Fable 429 marks only A's Fable model state; the account-level flag flips
  only when *every* model is down (`updateAggregatedAvailability`). So A stays usable for
  Opus/Sonnet.
- The request loop (`executeMixedOnce`) retries the **same** request against another untried
  account that serves that model (`max-retry-credentials: 0` = try all).
- The session-affinity cache key is **`provider::session::model`** (`SessionAffinitySelector.Pick`),
  so only the Fable binding moves to B; Opus/Sonnet keep hitting A. A quota error never drops the
  account's other session bindings.

**Required knobs (baked into every generated `config.yaml`):** `routing.session-affinity: true`
(off by default), `max-retry-credentials` unset/0, cooling left enabled, and the router injects
`X-Session-ID: <clone-id>` on every forwarded request. Both A and B must carry Fable (same
subscription tier → they do).

---

## Architecture / request flow

```
Claude Code / Codex / OpenCode  (in a clone)
    │  ANTHROPIC_BASE_URL / provider base_url = http://rmng-control:9000/cc
    │  Authorization: Bearer <per-clone key>
    ▼
control-server ROUTER  (axum, web port 9000, path prefix /cc/*)
    │  key → host_id → host.group → instance port      (loopback lookup)
    │  strip /cc, inject instance inbound key + X-Session-ID: <host_id>, stream both ways
    ▼
CLIProxyAPI instance for that group   (127.0.0.1:<port>, own auth-dir)
    │  route by requested model → provider → account
    │  session-affinity sticky per (session, model); per-model quota failover; owns refresh
    │  translate protocol (e.g. Claude-in → Codex-out) as needed
    ▼
api.anthropic.com  /  chatgpt.com
```

One inbound prefix `/cc/*` serves every agent (they differ only in the suffix they append —
`/v1/messages`, `/v1/responses`, `/v1/chat/completions`, `/v1/models`). All point at the same clone
instance; the router forwards the suffix verbatim.

---

## Components

### 1. Group config model — `crates/wire/src/config.rs`

- Replace the two per-provider lists (`AppConfig.clone_groups`, `AppConfig.codex_groups`) with a
  single provider-agnostic **`groups: Vec<Group>`**, where `Group { name: String }`. Membership is
  **not** a stored email list — it is derived from the instance's `auth-dir` contents. Update
  `AppConfigRedacted` + `AppConfig::redacted()` to mirror `groups`.
- `merge_update` (`control-server/src/config.rs`) keeps replacing the array wholesale; on a
  `groups` change, `config_put` calls `cliproxy::apply_now(&app)` to spawn/tear-down instances
  (mirror the existing `ssh::apply_now` call).
- **Ports are internal** — do not put them on the TS-exported struct. The supervisor owns a stable
  `group → port` allocation persisted to a server-only `data/cliproxy-ports.json` (0600, atomic
  write like `ClaudeStore::save`), allocated from a base (e.g. 9100). Instances bind `127.0.0.1`
  only — no Docker `EXPOSE` change.

### 2. Generated per-instance `config.yaml`

Written by the supervisor to `data/cliproxy/<group>/config.yaml` (0600 — holds secrets):

```yaml
host: "127.0.0.1"            # loopback only
port: <allocated>
auth-dir: <data>/cliproxy/<group>/auth
api-keys: ["<inbound key>"]  # shared secret: router → instance
routing:
  strategy: fill-first
  session-affinity: true
  session-affinity-ttl: "6h"
quota-exceeded:
  switch-project: true
remote-management:
  secret-key: "<random>"     # lets RMNG drive OAuth + list/delete auth-files
  allow-remote: false
  disable-control-panel: true
# max-retry-credentials omitted (0 = try all accounts on quota)
```

*(To-confirm: exact v7 key names for the loopback bind and the inbound key list — `api-keys` vs the
provider-specific `claude-api-key`/`codex-api-key`. See Risks.)*

### 3. Go sidecar binary — `cliproxy-sidecar/`

New Go module at repo root (`go.mod` pinning `github.com/router-for-me/CLIProxyAPI/v7`). `main.go`
embeds the SDK and runs exactly one Service from a config path:

```go
svc, _ := cliproxy.NewBuilder().WithConfigPath(cfgPath).Build()
// ctx cancelled on SIGTERM/SIGINT; svc.Run(ctx); defer svc.Shutdown
```

Flag `--config <path>`. Optional `WithHooks(OnAfterStart)` to log readiness for the Rust log drain.
One process = one group.

**Dockerfile:** add a Go build stage parallel to the existing bun/rust stages
(`FROM golang:1.23 AS go-build` → `go build -o /out/cliproxy-sidecar .`), then
`COPY --from=go-build /out/cliproxy-sidecar /usr/local/bin/`. Runtime stage already has
`ca-certificates`. Update the stage-list header comment.

### 4. Supervisor + management client — new module `crates/control-server/src/cliproxy.rs`

- `App` gains `cliproxy: Arc<CliProxyManager>`. Manager holds `Mutex<HashMap<String, Instance>>`
  (group → running instance: child handle, port, config path, auth-dir), the persisted port map,
  and the **RouterKeys** reverse map `token → host_id`.
- Disk: `data/cliproxy/<group>/{config.yaml, auth/}`.
- `run(app)` (spawned early in `main.rs` alongside `ssh::run`): every `RECONCILE_INTERVAL`, diff
  desired groups (from config) vs running; spawn missing, kill removed, restart crashed with the
  `ssh.rs` capped-backoff formula. Per-instance task drains stdout/stderr (`target: "cliproxy"`) and
  waits on the child — same shape as `run_sshd`. `spawn_instance` uses
  `Command::new("/usr/local/bin/cliproxy-sidecar").args(["--config", path])`.
- `apply_now(app)` — immediate spawn/tear-down from `config_put`.
- **Management client** (reqwest, reuse `app.http`): `GET /v0/management/{anthropic-auth-url,
  codex-auth-url,get-auth-status}`, `GET/DELETE /v0/management/auth-files`, dialed at
  `127.0.0.1:<port>` with header `X-Management-Key: <secret-key>`. Backs onboarding (§7) + account
  deletion + the usage poller's auth-file enumeration.

### 5. The router — new axum routes in `crates/control-server/src/web.rs`

Reuse the existing web listener (port 9000) with a path prefix — clones already reach
`http://{control_host}:9000` (`provision.rs` / `docker.rs control_host`), so **zero new
networking**, and dev-mode gateway-IP resolution works unchanged. Add before the SPA fallback:

```
.route("/cc/*rest", any(router_proxy))
```

`router_proxy`:
1. Read `Authorization: Bearer <token>` → `RouterKeys` → `host_id` (unknown → 401).
2. `host_id` → `host.group` (none/absent → 409 "clone has no group").
3. `group` → instance port via `app.cliproxy` (missing/booting → 503; the agent retries).
4. Forward to `http://127.0.0.1:<port>/<rest>` (+ query): copy method/body/headers except
   hop-by-hop and `Authorization`; set the instance inbound key; inject `X-Session-ID: <host_id>`.
   Stream request and response bodies (no buffering; preserve `text/event-stream`) — same shape as
   the existing `chat.rs` / MCP proxies. Log `target: "router"`.

One handler covers all agents (Anthropic, Responses, Chat Completions, `/v1/models`) since they hit
the same instance.

### 6. Clone provisioning + agent configs — `provision.rs`, `jobs.rs`, `clone_reconcile.rs`

Per-clone key: mint a random bearer at clone-create, store in server-only `data/clone-router-keys.json`
(0600) + the in-memory `token → host_id` map. **Never** put it on `Host` (that serializes into
`state.json` and streams to the browser). Delete on host delete.

Env / config written in place (constant for the clone's life — only the key is per-clone):

- **`control_env_vars`** (shared): `ANTHROPIC_BASE_URL=http://{control}:{web}/cc`,
  `CLAUDE_CODE_ENABLE_GATEWAY_MODEL_DISCOVERY=1`.
- **Per-clone** (via a new `router_env_vars(app, host_id)` extended into the `jobs.rs` create env
  and the `clone_reconcile.rs` per-clone resync): `ANTHROPIC_AUTH_TOKEN=<key>` and
  `RMNG_PROXY_KEY=<key>` (referenced by the Codex/OpenCode provider configs).
- **Codex** (`clone_reconcile.rs codex_config_toml`, already managed): add
  `[model_providers.rmng]` (`base_url = ".../cc/v1"`, `wire_api = "responses"`,
  `env_key = "RMNG_PROXY_KEY"`, WebSockets disabled), set it as the active provider, list GPT
  models only.
- **OpenCode**: generate its managed provider config (OpenAI-compatible, `baseURL = ".../cc/v1"`,
  apiKey from `RMNG_PROXY_KEY`), models = GPT only, no Anthropic provider.
- **Claude Code**: gateway discovery is enough; optionally set a default `model`.
- Clones no longer need `~/.claude/.credentials.json` or `~/.codex/auth.json`. Remove the
  token-apply/clear arms in `jobs.rs run_clone`; the `clone_ops::run_clone_op` transport stays
  (used elsewhere).

### 7. Onboarding — new endpoints in `web.rs`

Thin proxies to a group instance's management API via `app.cliproxy`:

- `POST /api/groups` / `DELETE /api/groups/:name` — create/delete group (spawns/kills its instance
  through `apply_now`).
- `POST /api/groups/:name/accounts/login/start` → management `anthropic-auth-url` /
  `codex-auth-url`, returns `{ loginUrl, state }`; the operator completes OAuth in a browser tab.
- `GET /api/groups/:name/accounts/login/status?state=…` → management `get-auth-status`; poll until
  the credential lands in the instance's `auth-dir`.
- `POST /api/groups/:name/accounts/:id/delete` → management `DELETE /auth-files`, then trigger a
  usage poll.

Because each `login/start` runs against a specific instance's `auth-dir`, the same email in two
groups gets independent token sets (decision 7). Remove the old
`import_clone_account` / `check_clone_auth` / swap / rotate endpoints.

### 8. Usage poller (by group) — `claude.rs` / `codex.rs`

Keep `fetch_usage` + parsing (`to_window`, `fable_window`, `window_of`, spend, reset credits).
Change only the **token source** and **published shape**:

- Enumerate instances from `app.cliproxy`; for each, read the `auth-dir/*.json` credential files
  and parse `{ email, access_token[, account_id] }` (the *current* token — **no refresh**). Call the
  same upstream usage endpoints (`api.anthropic.com/api/oauth/usage` + `anthropic-beta` header;
  `chatgpt.com/backend-api/wham/usage` + `ChatGPT-Account-Id`).
- Publish `ControlState.usage_groups: Vec<GroupUsage>` where
  `GroupUsage { name, accounts: Vec<ClaudeUsage> }`. Keep the `last_good` / `stale` caching keyed by
  `(group, account_id)`. An expired token → 401 → surface as `stale`/`error` (CLIProxyAPI refreshes
  it on the next proxied request).
- **Codex reset-credit / auto-reset**: RMNG can still read the token from the `auth-dir` and POST
  the consume endpoint. Decision pending (Risks) — recommend keeping it, re-plumbed to read from
  `auth-dir` rather than the deleted store.

### 9. Wire types + frontend

- **`Host`** (`crates/wire/src/control.rs`): remove `claude_account_email`, `codex_account_email`,
  `claude_group`, `codex_group`, `claude_selection`, `codex_selection`; add
  **`group: Option<String>`** (None = no inference).
- **New `GroupUsage`** (TS-exported); `ControlState.usage_groups` replaces `claude_accounts`.
  `ClaudeUsage.id` becomes group-scoped-unique (`<group>|<email>`), since one email can appear in
  multiple groups.
- **`CloneGroup` → `Group { name }`** in config wire; regenerate `frontend/app/lib/wire/*.ts` via
  the ts-rs export test.
- Frontend (`frontend/app/`): `lib/types.ts` mirror; selection UI in `SidebarHost.tsx`,
  `CloneModal.tsx`, `AccountGroupSelect.tsx`, `ChangeAccountModal.tsx` collapse `auto|group|specific|none`
  → **group | none**; `ClaudeAccountsPanel.tsx` reorganizes **by group** (group header, then the
  existing Row/Bar rendering); `SettingsPanel.tsx` group editor becomes name-only + a per-group
  "Add account" button driving the OAuth login modal + a live authed-account list with delete;
  `lib/api.ts` swaps import/swap/rotate for `createGroup` / `startGroupLogin` / `groupLoginStatus` /
  `deleteGroupAccount`; update `stories/fixtures.ts` + stories.

### 10. Migration (in place, no recreate)

The reconciler already rewrites `/etc/environment` (`clone_reconcile.rs reconcile_once`) and
`~/.codex/config.toml` (`codex_parity_entries`) per running clone. Migration adds:

1. Mint + persist each existing clone's per-clone router key.
2. Write the new env + Codex/OpenCode configs (automatic via reconcile).
3. Delete the now-dead `~/.claude/.credentials.json` / `~/.codex/auth.json`.
4. Operator restarts the clone's agent (env is read at process start) — or `docker restart <clone>`.
   **No container recreate.**

Host-field migration: old `claude_group`/`codex_group` names seed the new `group` binding where
present; clones on `auto`/pinned become `group: None` until reassigned. Because all tokens are
re-established by re-login, operators must, post-upgrade: create groups → log accounts in → bind
clones. Clone **containers are preserved** throughout.

---

## Deletion inventory

- **`claude.rs` / `codex.rs`:** OAuth refresh (`refresh_account`, `fresh_access_token`,
  `set_expiry_from_access`, client-id/token-url consts, `REFRESH_LEAD_MS`); the token stores
  (`StoredClaudeAccount`/`StoredCodexAccount`, `ClaudeStore`/`CodexStore`, the on-disk
  `claude-accounts.json`/`codex-accounts.json`); import (`import_clone_account`, `check_clone_auth`);
  the entire scoring/rotation block (`AUTO`/`NONE` sentinels, `normalize_selection`,
  `score_accounts`, `best_scored`, `resolve_clone_account`, `resolve_assignment`, `is_exhausted`,
  `assign_rotation`, `saturated_rank`, `keep_saturated_current`, `assign_saturated_rotation`,
  `pick_group_account`, `rotate_pool`, `auto_pool_clones`, `rotate_once`, `run_rotator`); token push
  (`credentials_json`/`auth_json`, `apply_clone_token`, `clear_clone_token`, `push_account_to_clone`,
  `push_stale_tokens`). Keep `fetch_usage` + all usage parsing.
- **Guest scripts:** `crates/control-server/scripts/claude-import.sh`, `codex-import.sh`.
- **Callers:** `jobs.rs` assignment block; `web.rs` claude/codex import/swap/rotate handlers +
  routes; `main.rs` `run_rotator` spawns; `app.rs` store fields (→ usage cache + `cliproxy`).
- **Tests:** all rotation/scoring/token-push tests; keep usage-parse tests, add router-resolution,
  `render_config_yaml`, and `auth-dir` parser tests.

---

## Spike findings — confirmed against v7.2.91 (2026-07-19)

Pinned by building `cmd/server` (go 1.26.5) and probing a live loopback instance:

- **Config keys** all accepted: `host: "127.0.0.1"` (loopback), `port`, `auth-dir`, `api-keys` (inbound
  client auth), `remote-management.{allow-remote, secret-key, disable-control-panel}`,
  `routing.{strategy, session-affinity, session-affinity-ttl}`, `quota-exceeded.switch-project`,
  `max-retry-credentials: 0`. Accounts are **not** in config — they are OAuth files in `auth-dir`.
- **`secret-key` is auto-bcrypt-hashed in place on startup** (the instance rewrites its own
  `config.yaml`). RMNG must persist the *plaintext* management secret + inbound key in its own store
  (`data/cliproxy-instances.json`) and send the plaintext as `X-Management-Key` — never re-derive from
  the mutated file.
- **Auth-dir credential JSON** (files named `claude-<email>.json` / `codex-<email>.json`):
  Claude `{ id_token, access_token, refresh_token, last_refresh, email, type:"claude", expired }`;
  Codex adds `account_id`, `type:"codex"`. The usage poller reads `access_token` (+ `account_id` for
  Codex) directly — same fields RMNG parses today.
- **OAuth onboarding contract** (all under `/v0/management`, `X-Management-Key`):
  `GET /anthropic-auth-url` (or `/codex-auth-url`) → `{status:"ok", url, state}` and spawns a 5-min
  waiter; operator opens `url`, logs in; Claude redirects to `http://localhost:54545/callback?...`
  (Codex → `localhost:1455/auth/callback`). Because the instance is headless, RMNG takes the pasted
  redirect URL (or code+state) and `POST /oauth-callback {provider, redirect_url}` (it parses
  code/state out); then polls `GET /get-auth-status?state=…` until complete.
- **`/v1/models`** returns an OpenAI-style `{data:[...],object:"list"}` catalog — the surface Claude
  Code gateway discovery reads.
- **SDK builder API** (for the sidecar): `cliproxy.NewBuilder().WithConfig/WithConfigPath/WithHooks(...).Build()`
  → `*Service`; `Service.Run(ctx) error`, `Service.Shutdown(ctx) error`;
  `Hooks{OnBeforeStart(*config.Config), OnAfterStart(*Service)}`; `Service.RegisterUsagePlugin(usage.Plugin)`.

## Risks / to-confirm — remaining (need operator's real Claude + ChatGPT logins)

- **v7 config key names**: loopback bind key; inbound key (`api-keys` vs `claude-api-key` /
  `codex-api-key`); exact `routing.*` / `quota-exceeded.*` spellings.
- **Management OAuth shapes**: request params + response bodies of `anthropic-auth-url` /
  `codex-auth-url` / `get-auth-status`; the state/poll contract.
- **`auth-dir` JSON shapes** for Claude and Codex (fields for access token, email, account_id) — the
  usage poller parser depends on these.
- **Codex CLI custom-provider config**: exact `[model_providers.*]` keys (`wire_api`, `env_key`, WS
  disable) accepted by the current Codex client.
- **OpenCode config format**: provider block + how to pin the model list (GPT only), and whether it
  auto-discovers `/v1/models` (if so, keep it off / config-listed).
- **Claude Code gateway discovery**: confirm CLIProxyAPI's `/v1/models` payload matches Claude
  Code's `llm-gateway-protocol` discovery format so GPT models actually populate the picker.
- **Prompt caching through the proxy**: `cache_control` + `anthropic-beta` survive router forwarding
  byte-for-byte, and session affinity keeps a clone on one account long enough for cache hits.
- **v7 SDK builder surface** (`NewBuilder`/`WithConfigPath`/`Build`/`Run`/`Shutdown`) — pin a
  working version in `go.mod`.
- **Codex reset-credit / auto-reset**: keep (re-plumbed to read the token from `auth-dir`) or drop.
- **Startup ordering**: instances up before traffic — the 503+retry posture makes this non-fatal;
  spawn the supervisor early.

---

## Verification

- **Build:** `cargo build -p control-server` (after deletions); `cd cliproxy-sidecar && go build`;
  `docker build .` (exercises the Go stage); ts-rs export test → `cd frontend && bun run build`.
- **Unit:** keep usage-parse tests; add router token→host→port resolution, `render_config_yaml`
  snapshot, and `auth-dir` parser tests.
- **End-to-end:**
  1. Create a group in Settings → a `cliproxy-sidecar` runs on its port; `config.yaml` + `auth/`
     exist under `data/cliproxy/<group>/`.
  2. Run OAuth login → complete in browser → credential appears in `auth-dir`; account shows under
     the group in the usage panel (5h/7d/Fable).
  3. Clone bound to the group → `/etc/environment` has `ANTHROPIC_BASE_URL=.../cc` +
     `ANTHROPIC_AUTH_TOKEN`, and **no** `~/.claude/.credentials.json`.
  4. Claude Code request in the clone → `target: "router"` logs show key→group→port + `X-Session-ID`;
     response streams back. Pick a GPT model in Claude Code → routed to a GPT account (translated).
  5. Codex + OpenCode pickers show GPT models only.
  6. Change the clone's group in the UI → next request routes to the new instance, **no restart**.
  7. Fable exhaustion on account A → Fable requests fail over to B while Opus/Sonnet stay on A.
  8. Migration: an existing clone gets the new env/configs + a restart and works — **container not
     recreated**.
