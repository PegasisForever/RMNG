# Central inference proxy implementation plan

> **SUPERSEDED (2026-07-19).** This plan (Rust-owned selection/refresh + one stateless
> direct-executor worker) is replaced by
> `2026-07-19-cliproxy-group-proxy-plan.md`, which moves refresh + account selection into
> full CLIProxyAPI instances (one per group). Kept for history only — do not implement.

**Date:** 2026-07-15
**Status:** Superseded by 2026-07-19-cliproxy-group-proxy-plan.md
**Scope:** RMNG's inference proxy, its patched CLIProxyAPI worker, and the clone configuration
required by Claude Code, Codex, and OpenCode V2.

This plan supersedes the clone-local credential injection and proactive rotation designs in:

- `2026-07-03-auto-account-rotation-design.md`
- `2026-07-07-reset-aware-auto-account-rotation-design.md`

The phases below are implementation checkpoints, not separately shipped MVPs. The feature ships
as one complete replacement after the final release gate passes.

## Goal

Move inference account selection from provider credentials installed in each clone to one central
RMNG proxy:

- RMNG remains the sole owner of Anthropic and OpenAI refresh tokens.
- Every clone receives one unique random RMNG inference credential, never a provider token.
- Rust owns clone authentication, profiles, account selection, OAuth refresh, usage polling,
  account health, and all durable RMNG state.
- A loopback-only Go worker built from a pinned, minimally patched CLIProxyAPI owns provider
  protocol execution and translation.
- Claude inference is available only through Claude Code-compatible clients.
- GPT inference is available through Claude Code, Codex, and OpenCode V2.
- Existing account import/delete, usage windows, spend, reset times, Codex reset credits, and
  auto-reset remain available.
- The old token push and proactive rotation implementation is removed completely at cutover.

## Simplified product decisions

### Model access

| Client | Claude inference | GPT inference |
|---|---|---|
| Claude Code | native | translated |
| RMNG Claude Agent SDK wrapper | native Claude only | not supported |
| Claude Code IDE extension | configured where straightforward, not separately certified | same |
| Codex | not exposed | native |
| Codex IDE extension | configured where straightforward, not separately certified | native |
| OpenCode V2 | not exposed | native OpenAI provider |

The Agent SDK wrapper preserves its existing Claude behavior and is not another translated GPT
surface. IDE extensions may inherit working CLI configuration, but extension-specific behavior is
not a release gate.

### Client versions

"Latest" means the exact Claude Code, Codex, and OpenCode V2 versions current when an RMNG release
candidate is built. Record those versions and support that tested set. RMNG does not continuously
track client updates between releases and does not add old-version compatibility branches.

### Conversation routing

Do not persist conversation bindings or a seven-day TTL. For a request with a stable conversation
ID, choose an account with rendezvous hashing over:

```text
clone ID + target provider + conversation ID + eligible account ID
```

This gives deterministic, near-even stickiness across control-server and worker restarts without a
binding database. The same conversation remains on the same account while the eligible account set
is unchanged. Adding, removing, disabling, exhausting, or recovering an account may move some
conversations.

If a supported client supplies no stable conversation ID, use a simple in-memory round-robin
cursor and make no stickiness guarantee.

### Failure behavior

Do not replay a failed inference request against another account.

- An auth, quota, or account-specific provider failure marks that account temporarily ineligible.
- The original failure is returned to the client.
- The user's next request or retry hashes over the remaining eligible accounts.
- Exact account pins remain fail-closed and never choose another account.
- Invalid request, context-limit, model, and translation failures do not change account health.
- A stream that fails after output starts is propagated and does not change routing mid-stream.

Ordinary token expiry is avoided by refreshing before requests. An unexpected 401 invalidates the
account/token state and is returned; RMNG does not replay the request after refreshing.

### Transport and translated feature floor

Support HTTP and SSE only. Configure Codex custom providers with WebSockets disabled. Responses
WebSockets and per-turn socket redial are out of scope.

The blocking Claude Code-to-GPT contract is:

- streaming text
- system/developer instructions
- multi-turn messages
- function tools and tool results
- ordinary usage and error reporting
- cancellation

CLIProxyAPI behavior for images, encrypted reasoning history, advanced reasoning controls, strict
structured output, unusual signatures, cache metadata, and uncommon parallel-tool fragmentation
is inherited best effort. Those features are not separately certified by RMNG and do not block
release.

## Design constraints

This remains a single-operator, secured-LAN application:

- no TLS between processes in the same container
- no separate gateway administrator, management UI, or metrics system
- no Redis, SQL database, replica coordination, rate limiter, or fairness scheduler
- no automatic CLIProxyAPI or client updates
- no provider request/response body logging
- no generalized graceful-shutdown framework
- normal `tracing` logs remain the diagnostic surface

Two local processes are acceptable. RMNG already supervises `smbd` and `sshd`; the inference
worker follows the same simple spawn, log, wait, and restart pattern.

## Architecture

### Rust control server

The existing Rust process remains the public proxy and owns:

- public Anthropic Messages and OpenAI Responses routes
- clone bearer authentication
- model visibility and defaults
- unified profiles
- clone policy: auto, profile, exact account, or none
- deterministic rendezvous routing
- in-memory fallback round-robin for requests without a conversation ID
- runtime account health after actual failures
- OAuth enrollment, refresh-token storage, and access-token refresh
- usage polling and Codex reset-credit behavior
- clone creation, reconciliation, migration, and deletion
- existing account/profile UI and APIs

Rust buffers inference request bodies under the existing 64 MiB web limit, parses only the model
and captured conversation field needed for routing, and forwards the original bytes to the worker.
There is no replay buffer or alternate-account retry.

### Direct-executor Go worker

Ship one `rmng-inference-worker` binary in the existing control-server image. It lives inside the
pinned CLIProxyAPI source tree so it can call the existing internal Claude and Codex executors
directly.

The worker does not instantiate CLIProxyAPI's full service, auth manager, selector, credential
store, watcher, scheduler, management API, auto-refresh loop, or generic public server.

For each request the worker:

1. Receives one current access token and provider metadata from Rust over loopback.
2. Constructs one request-scoped CLIProxyAPI `Auth`.
3. Sets source protocol, response protocol, target model, stream mode, and compaction mode.
4. Calls the Claude or Codex executor directly.
5. Streams native or translated caller-protocol bytes back to Rust.
6. Returns a small pre-output outcome classification when execution fails before a successful
   stream payload.

The worker imports CLIProxyAPI's built-in translator registrations and initializes only model
metadata needed by the Claude and Codex executors.

The worker owns:

- Anthropic subscription request construction and headers
- Codex subscription Responses request construction and headers
- Messages-to-Responses request translation for Claude Code using GPT
- Responses-to-Messages response translation
- native Responses normalization and compaction
- SSE parsing and fragmented tool-call reconstruction
- thinking/reasoning/signature compatibility inherited from CLIProxyAPI
- prompt-cache, tool-name, and tool-schema normalization
- provider error parsing

The worker does not own or persist:

- refresh tokens
- provider access tokens after a request
- clone credentials
- profiles or routing policy
- account health or cooldowns
- model selection policy
- cross-account retries

The worker may retain ordinary volatile CLIProxyAPI compatibility caches during its process
lifetime. Restarting it loses no durable RMNG account or routing state.

### Minimal downstream patch policy

Start from CLIProxyAPI tag `v7.2.79`, commit `b6ce0be`, retain its MIT notice, and record the pin
and RMNG patch set in `UPSTREAM.md` beside the worker.

Keep patches additive and narrow:

- add `cmd/rmng-inference-worker`
- expose a helper only when direct executor use lacks a required stable API
- add compact pre-output failure classification
- ensure RMNG internal headers and bodies never enter stock request/error logging paths
- add focused worker tests

Do not make RMNG-specific changes to:

- Claude or Codex upstream request behavior
- request/response translators
- thinking or signature conversion
- provider model compatibility
- WebSocket code

CLIProxyAPI is updated only as an explicit RMNG dependency change. There is no speculative
upstream-bump rehearsal in the initial release plan; run the focused worker and live protocol tests
when an actual bump is proposed.

## Public protocol surface

Use two protocol routes rather than client-named routes:

```text
POST /inference/anthropic/v1/messages
POST /inference/anthropic/v1/messages/count_tokens   # only if captured Claude Code requires it
GET  /inference/anthropic/v1/models                  # only if captured client requires it

POST /inference/openai/v1/responses
POST /inference/openai/v1/responses/compact          # only if captured Codex requires it
GET  /inference/openai/v1/models                     # only if captured client requires it
```

The Anthropic route permits configured Claude and GPT aliases. The Responses route permits GPT
only and rejects Claude before account selection. Codex and OpenCode share the Responses route.

Route names enforce supported protocol configuration, not executable identity. Every process in a
trusted clone shares one clone credential; hard per-executable authorization would require
separate credentials and is intentionally out of scope.

Rust serves any required `/models` response from one small static model table without selecting an
account or calling the worker. The same table defines:

- target provider and upstream model ID
- visibility on Anthropic or Responses routes
- the default Claude Code model
- the default Codex model
- the default OpenCode model
- any Claude-shaped alias required for Claude Code to select GPT

There is no dynamic model catalog or general model-alias subsystem.

## Internal worker contract

The worker binds to a fixed `127.0.0.1` address that is not published by Docker. No persistent
worker credential is needed inside the same privileged container.

Internal endpoints are limited to:

```text
GET  /_rmng/ready
POST /_rmng/messages
POST /_rmng/messages/count_tokens
POST /_rmng/responses
POST /_rmng/responses/compact
```

Readiness returns a small contract version and pinned upstream version. Rust logs them once at
startup; there is no health dashboard or version negotiation system.

Execution requests carry the original client body plus internal headers for:

- contract version
- target provider and model
- stable opaque account pseudonym
- current access token
- Codex account ID when applicable
- source and response protocol

Do not pass raw RMNG account IDs when an opaque stable pseudonym is sufficient. Strip every
client-supplied `X-RMNG-*` header before creating internal metadata. The worker removes all internal
headers before executor/upstream construction and does not install CLIProxyAPI's stock request or
deferred error-body logging middleware.

The worker outcome is intentionally small:

```text
success
request_error
account_auth
account_quota
account_transient
canceled
```

Rust already knows the selected account and whether client output started, so the worker does not
echo selected-auth identity or create a post-output control protocol. Native provider status,
headers, body, and retry timing remain available for the client and health decision.

For streams, the worker waits only for the first successful payload, a pre-output terminal error,
or an empty stream. After a successful payload is released, later EOF/error is ordinary stream
interruption and cannot trigger routing changes for that request.

### Worker supervision

Mirror existing RMNG child supervisors:

- spawn the worker with `kill_on_drop`
- drain stdout/stderr into an `inference_worker` log target
- check the small readiness contract after spawn
- return inference `503` when the loopback worker is unavailable
- restart after exit with a small capped delay
- do not add application-wide shutdown orchestration

## Clone authentication and profiles

Store clone gateway credentials in a server-only atomic `0600` JSON file. No conversation binding
state is persisted.

Keep the approved unified profile model:

```json
{
  "name": "personal",
  "anthropicAccounts": ["one@example.com", "two@example.com"],
  "openaiAccounts": ["three@example.com"]
}
```

Each clone has one policy per provider:

- `auto`: all imported accounts for that provider
- `profile:<name>`: that profile's members for the provider
- exact account: fail-closed pin
- `none`: provider disabled

Migrate same-named existing Claude/Codex groups into one profile with separate member lists.
Preserve unmatched groups as one-provider profiles. Migrate `group:<name>` selections to
`profile:<name>`. A previously recorded current account for auto/group routing must not become an
exact pin.

### Eligibility after actual failure

Do not use proactive usage thresholds. Auto/profile candidates begin with imported, enabled
profile members and are removed only after an actual request failure:

- auth failure: unavailable until a successful refresh/reconnect or account re-import
- quota failure: unavailable until the provider reset time when known, otherwise until a later
  successful usage/account check
- account-specific provider failure: short in-memory cooldown

These runtime health decisions do not need their own durable database. A control-server restart may
try an account again once. Exact pins never substitute another account even when known unhealthy.

## Client configuration

### Claude Code

Configure the Anthropic base URL and clone RMNG credential. Expose Claude and GPT aliases from the
static model table. The Claude Code IDE extension may inherit this configuration, but receives only
a launch/request smoke rather than separate certification.

### Codex

Configure one custom Responses provider with WebSockets disabled and the clone RMNG credential.
Expose GPT models only. The Codex IDE extension may inherit the same provider configuration.

### OpenCode V2

Generate only the native OpenAI provider configuration pointed at RMNG. Do not expose Anthropic or
Claude models.

RMNG does not continuously install or upgrade OpenCode. Put the tested OpenCode version in new
clone templates when convenient, or require a one-time manual install on existing clones. The
control server manages configuration, not the OpenCode package lifecycle.

### Agent SDK wrapper

Keep the existing wrapper on native Claude through the Claude Code configuration. GPT selection is
not supported through the wrapper.

### Managed files

Follow RMNG's existing overwrite/hash/stamp convention:

- use dedicated RMNG-owned files or complete managed sections
- render and replace those files/sections deterministically
- do not build general TOML/JSONC comment-preserving merge engines
- require a new client process, and when needed a new shell/IDE process, after migration

Account import remains clone-based. Import commands temporarily bypass gateway configuration,
allow official login, harvest OAuth credentials into Rust's account stores, and clear temporary
provider files from the clone.

## One-way migration and cutover

Do not ship parallel old/new inference modes or a runtime fallback to direct provider credentials.
Preflight the worker and account flows on CT 119 before cutover, then reconcile each clone with one
stamped operation:

1. Generate and persist its RMNG clone credential.
2. Write all managed client configurations.
3. Remove old managed Claude and Codex provider credential files after configuration writes
   succeed.
4. Restart the Agent SDK wrapper if its native-Claude configuration changed.
5. Write the reconciliation stamp.

If a file operation fails, do not stamp completion; log and retry on a later reconcile pass. Do not
perform a billable live inference verification transaction inside every clone migration.

After the migration path is proven, remove:

- `claude::run_rotator` and `codex::run_rotator`
- proactive headroom scoring and reset-aware fallback rotation
- clone token push caches and fan-out
- normal inference token apply/push paths
- manual Claude/Codex rotate endpoints
- clone creation logic that resolves auto/profile to an installed account

Keep Rust OAuth refresh, usage polling, last-good/stale usage, spend, reset timestamps, Codex reset
credits, and auto-reset.

## Test deployment

An isolated deployment already exists:

| Item | Value |
|---|---|
| Proxmox | `root@10.0.0.100` (`pegaswarm`) |
| CT | `119`, `rmng-gateway-test` |
| Address | `10.0.0.119/24` |
| Resources | 16 vCPU, 32 GiB RAM, 160 GiB thin disk |
| Docker | 29.6.1, `overlayfs` |
| RMNG URL | `http://10.0.0.119:9000` |
| Test clone | `gateway-smoke`, `10.119.0.3` |
| Snapshot | `baseline-current-rmng` |

Use CT 119 only for:

- latest-client endpoint/session/transport capture
- one migration rehearsal from the baseline snapshot
- one final release qualification

Do not redeploy CT 119 after every internal implementation checkpoint. Do not modify CTs 105 or
106. Testing proceeds without approval; ask the operator only for official Claude or OpenAI/Codex
login when real subscription accounts are needed.

## Phase 1: latest-client capture and worker proof

Update the test clients to the versions current for this RMNG release and record them. Capture only:

- methods and endpoints
- HTTP versus SSE behavior
- explicit stable conversation headers/fields
- `/models`, count-token, compaction, or search probes
- confirmation that configured Codex HTTP/SSE mode works with WebSockets disabled

Build the direct-executor worker and prove native Claude, native GPT Responses, and Claude
Code-to-GPT translation once.

**Blocking tests:**

- Reproducible worker build from the pinned source/patch set.
- Worker direct auth contains an access token but no refresh token.
- Worker has no auth store, selector, cooldown, auto-refresh, or alternate credential.
- Native Claude streaming text and one tool call/result.
- Native GPT Responses streaming text and one tool call/result.
- Claude Code-to-GPT streaming text and one multi-turn tool call/result.
- Cancellation reaches the upstream request.
- Token/body canaries are absent from worker logs and persistence.
- Latest Codex works with WebSockets disabled.

Do not add an endpoint or session extractor that the recorded clients do not use.

## Phase 2: core proxy, routing, and worker integration

Implement:

- worker build/package/supervision
- loopback contract and internal-header stripping
- clone credentials
- static model table and protocol model restrictions
- unified profiles and config migration
- captured explicit conversation IDs
- rendezvous routing and no-ID round-robin
- runtime account health after actual failures
- native Claude and GPT forwarding
- Claude Code-to-GPT worker path
- one small pre-output outcome adapter

**Automated tests:**

- Clone credentials are random, unique, and scoped to one clone.
- Worker is not reachable from clones or the LAN.
- Unknown/disallowed models fail before worker execution.
- Same conversation/account set routes deterministically across restarts.
- Distribution is acceptably even across many synthetic conversation IDs.
- Account/profile membership changes recalculate routing naturally.
- No-ID requests use in-memory round-robin without persistence.
- Exact and none policies behave correctly.
- Actual auth/quota/provider failures change eligibility for the next request.
- Failed requests are never replayed automatically.
- Invalid/context/translation errors do not change account health.
- Worker unavailable returns `503`, restarts, and leaves the RMNG web UI healthy.
- Internal headers, tokens, and bodies are absent from logs and downstream responses.
- Focused upstream executor/translator tests pass unchanged.
- `cargo test --workspace` and the normal frontend/Agent wrapper checks pass.

Use fake upstreams and captured fixtures for failure, fragmentation, quota, long-context, and
signature cases. Do not generate expensive or nondeterministic live failures.

## Phase 3: client configuration, migration, and old-code removal

Implement generated Claude Code, Codex, OpenCode V2, and wrapper configuration; unified profile
migration; stamped existing-clone migration; new-clone provisioning; and deletion of old token
push/rotation code.

**Automated tests:**

- Managed configuration rendering and modes.
- No provider refresh token in any generated clone file.
- OpenCode configuration contains GPT and no Claude provider/model.
- Codex configuration disables WebSockets.
- Wrapper remains on native Claude.
- Same-named groups merge into unified profiles without cross-provider member confusion.
- Old current-account fields do not become exact pins.
- Reconciliation is idempotent and retries unstamped failures.
- Account import still bypasses gateway config and clears temporary provider files.
- Clone/account deletion removes clone credentials and invalid policy references.
- Browser/redacted APIs expose no clone or provider credentials.
- Frontend policy/profile/default-model tests, generated wire types, type check, and build pass.

**One CT 119 migration rehearsal:**

1. Restore or clone the `baseline-current-rmng` snapshot.
2. Deploy the release candidate.
3. Confirm `gateway-smoke` receives managed configs and a unique RMNG credential.
4. Confirm provider credential files are removed after successful configuration writes.
5. Restart required client processes.
6. Confirm native Claude, native GPT, and translated GPT work.
7. Create another clone and confirm it has a different RMNG credential and no provider token.
8. Delete that clone and verify its RMNG credential is removed.

## Phase 4: final release gate

Run once on the final candidate.

**Live protocol gates:**

- Claude Code to Claude: streaming text plus one tool call/result.
- Codex to GPT: streaming text plus one tool call/result.
- OpenCode V2 to GPT: one native streaming request and no selectable Claude model.
- Claude Code to GPT: streaming text plus one multi-turn tool call/result.
- Agent SDK wrapper: one existing native-Claude chat turn.
- IDE extensions: at most one launch/request configuration smoke when straightforward; not a
  semantic compatibility matrix.

**Routing and failure gate:**

- Two conversations distribute across available accounts.
- Continuing one conversation stays deterministic.
- One controlled account failure is returned without replay.
- The next request routes away from the ineligible account for auto/profile policy.
- Exact pin returns the failure without substitution.
- Stream interruption after output does not create another request.

**Affected RMNG smoke:**

- Dashboard and SSE state load.
- Existing clone reconnects.
- One new clone can be created and deleted.
- Claude and Codex account import paths still work if changed.
- Usage refresh, spend, reset times, and reset credits remain visible.
- Standard Rust, worker, frontend, generated-type, and Agent wrapper checks pass.

Do not add a full manual SMB, SSH, media, desktop MCP, long-context, advanced reasoning, image,
structured-output, IDE, or upstream-bump matrix unless implementation changes those areas or the
recorded clients require them for their normal supported flow.

The release is complete when the four supported protocol paths pass, deterministic routing and
failure invalidation pass, existing clones migrate without provider credentials, the worker owns
no durable account state, and the old rotator/token-push paths are gone.
