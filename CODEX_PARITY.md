# Codex account subsystem: server-owned tokens, full parity with Claude

Implementation plan — verified 2026-07-02 against the working tree (post Claude single-token
revamp + presets unification) and the openai/codex source. Not yet implemented.

## Context

RMNG's Claude accounts just moved to a single-token model: the server owns each account's OAuth pair (0600 `claude-accounts.json`), refreshes access tokens itself, injects only the short-lived token into clones, and re-pushes on every rotation. Codex (OpenAI's CLI) has **zero implementation** today — only stubs (`Provider::Codex` in wire, `codex:<id>` id-format doc, a ChatGPT logo + "exclude codex" filters in the frontend, and `codex` isn't even installed in clones). This change builds the Codex sibling subsystem with **full parity** (import-from-signed-in-clone, usage polling, assign at clone time, swap, recommended scoring, groups + rotation, auto-swap) and **independent coexistence** — a clone can hold a Claude account and a Codex account simultaneously.

## Verified Codex CLI facts (cited from github.com/openai/codex `main`)

- **Auth file** `~/.codex/auth.json` (`codex-rs/login/src/auth/storage.rs` `AuthDotJson`): `{ "OPENAI_API_KEY": null, "tokens": { "id_token": "<JWT>", "access_token": "<JWT>", "refresh_token": "<opaque>", "account_id": "<uuid>" }, "last_refresh": "<RFC3339>" }`. API calls use `access_token` as bearer; `account_id` (from `tokens.account_id`, falling back to the id_token claim `chatgpt_account_id`) goes in the `ChatGPT-Account-Id` header.
- **Refresh** (`codex-rs/login/src/auth/manager.rs`): POST `https://auth.openai.com/oauth/token`, `client_id = "app_EMoamEEZ73f0CkXaXp7hrann"`, `grant_type=refresh_token`. Response `{id_token?, access_token?, refresh_token?}` — **no `expires_in`**; expiry must be decoded from the access-token JWT `exp`. Refresh tokens are **single-use/rotating** (`refresh_token_reused` → Exhausted) — same hazard as Claude, so the refresh-gate + clone-can't-refresh model carries over verbatim.
- **CLI self-refresh triggers**: access-token `exp` within 5 min, OR `last_refresh` older than 8 days, OR on 401. **Injection trick**: write the fresh access+id token + account_id with `refresh_token: ""` and `last_refresh: now` — defeats the 8-day fallback; the 5-min window is never reached because the server re-pushes with a 60-min lead.
- **Usage**: GET `https://chatgpt.com/backend-api/wham/usage`, `Authorization: Bearer` + `ChatGPT-Account-Id`. Response `{plan_type, rate_limit: {primary_window, secondary_window: {used_percent, limit_window_seconds, reset_after_seconds, reset_at}}}` → map to the 5h/weekly bars **by `limit_window_seconds`** (~18000 vs ~604800), not by field order.
- **Import identity**: `codex login status` is stderr prose — read auth.json and decode the id_token JWT claims (`email`, `https://api.openai.com/auth`.chatgpt_plan_type / .chatgpt_account_id).
- **Install**: `CODEX_NON_INTERACTIVE=1 curl -fsSL https://chatgpt.com/codex/install.sh | sh` → standalone binary at `~/.local/bin/codex` (no node) — clean parallel to the Claude installer in `template/setup/30-user.sh:~140`.

**Unverified, hedged** (see Verification): exact access-token lifetime (decode a real token on staging; keep `REFRESH_LEAD_MS` < lifetime); whether the CLI tolerates `refresh_token: ""` at startup (fallback: dead sentinel token in the script — no Rust change); `/wham/usage` response drift (Option-tolerant serde + last-good-stale + a `codex.usage_polling` config flag that disables usage without touching refresh/push/assignment).

## Design

- **Separate `crates/control-server/src/codex.rs` mirroring claude.rs**, not a provider-generic rewrite (claude.rs just stabilized; the token/JWT/usage mechanics differ). Extract shared, side-effect-free helpers into a new `crates/control-server/src/clone_ops.rs`: `now_ms`, `sq`, `extract_json`, `snippet`, `rand_u64`, `shuffle`, a generalized `run_clone_op(ssh, ctid, user, script, op, extra)` (claude.rs keeps a thin wrapper passing its `IMPORT_SCRIPT`), new `jwt_exp_ms()` + `jwt_claims()` (hand-rolled base64url decode — matches orchestrate.rs's hand-rolled encode, no new dep), and `replace_provider_views(app, provider, views, pinned)`.
- **Poller clobber fix (required for coexistence)**: claude's `poll_inner` does `s.claude_accounts = views` (claude.rs:602) and `.clear()` (:543) — two pollers would erase each other. Both pollers switch to retain-other-provider + extend + re-sort via `replace_provider_views`. Also audit `five_hour_pct` (:760) and `auto_swap_exhausted` (:985) usage maps to filter `provider != Codex` (emails could collide across providers); codex.rs filters to `== Codex`.
- **Identity**: codex accounts keyed by ChatGPT **email** for assignment (parity with claude); wire id `codex:<account_id>` (matches control.rs doc). Store keeps email, account_id, plan, access/id/refresh tokens, `expires_at` (decoded from JWT).
- Same store shape as `ClaudeStore` (accounts + last_good + polling + `refresh_gate` + `pushed` map), same reconcile-push model (`push_stale_tokens` keyed off `Host.codex_account_email`), `REFRESH_LEAD_MS = 60 min`.

## Changes

### 1. Shared helpers — new `crates/control-server/src/clone_ops.rs`
As designed above; `mod clone_ops;` in main.rs; claude.rs drops its private copies (mechanical only — no behavior change).

### 2. New `crates/control-server/src/codex.rs` (~700 lines, mirrors claude.rs)
- Constants: `USAGE_URL = https://chatgpt.com/backend-api/wham/usage`, `OAUTH_TOKEN_URL = https://auth.openai.com/oauth/token`, `OAUTH_CLIENT_ID = app_EMoamEEZ73f0CkXaXp7hrann`, `REFRESH_LEAD_MS = 60min`, scoring knobs copied from claude, `IMPORT_SCRIPT = include_str!("../scripts/codex-import.sh")`.
- `StoredCodexAccount { id: "codex:<account_id>", email, account_id, plan, active, access_token, id_token, refresh_token, expires_at }` in 0600 `codex-accounts.json` (`RMNG_CODEX_ACCOUNTS_FILE` override); `CodexStore` mirrors `ClaudeStore`.
- `check_clone_auth` (reads auth.json via script `status`, requires `tokens` present + `OPENAI_API_KEY` null, decodes id_token for email/plan/account_id), `import_clone_account` (harvest triple → upsert → `clear` clone's auth.json → `forget_pushed`), `refresh_account` (no expires_in — `expires_at = jwt_exp_ms(access_token)`), `fresh_access_token(app, email) -> (StoredCodexAccount, bool)` under the gate (returns the whole account — apply needs id_token + account_id), `auth_json(acct)` (the injected file: real tokens, `refresh_token:""`, `last_refresh: now`), `apply_clone_token` (JWT `eyJ` prefix sanity check), `push_account_to_clone`, `push_stale_tokens`, full scoring/groups/rotator/auto-swap copies reading `cfg.codex_groups` + `Host.codex_*`, `poll_once`/`run_poller` with the merge fix and `codex.poll_secs`/`usage_polling` (flag off → skip fetch_usage but still refresh + push + publish base views with an explanatory `error`).
- Tests: copy claude's assign_rotation tests; auth_json shape (refresh empty, tokens present); jwt helpers on a sample JWT; provider-views merge preserves the other provider.

### 3. Wire types
- `crates/wire/src/control.rs` `Host`: add `codex_account_email`, `codex_group`, `codex_selection` (Option, `#[serde(default)]`, camelCase) after the claude fields; update the `assignable` doc ("every imported account of either provider"); camelCase roundtrip test.
- `crates/wire/src/config.rs`: new `CodexConfig { poll_secs: 600, pinned_email, auto_swap_on_exhaustion, usage_polling: bool (default true) }` (TS-exported like `ClaudeConfig`); `AppConfig` + `AppConfigRedacted` gain `codex` + `codex_groups: Vec<CloneGroup>` (reuse `CloneGroup`). Re-export `CodexConfig` in `crates/wire/src/lib.rs`.
- `crates/control-server/src/config.rs`: nothing — `codex_groups`/`codex` are non-secret, `deep_merge` handles them; add a wholesale-replace test mirroring `merge_replaces_clone_groups_wholesale`.

### 4. App/main + endpoints
- `crates/control-server/src/app.rs`: `pub codex: Arc<CodexStore>`; `main.rs`: spawn `codex::run_poller` + `codex::run_rotator` next to claude's.
- `crates/control-server/src/web.rs`: routes + handlers `/api/codex/{import/check,import,refresh,recommended,swap,rotate}` — codex copies of the claude handlers (import/check returns `{ok, email, plan, accountId}`); `clone()` parses `codexAccount` into `CloneSpec`.
- `crates/control-server/src/jobs.rs`: `CloneSpec.codex_account: Option<String>` (has `Default` — mcp.rs literal keeps `..Default::default()`); a parallel codex assignment block after the claude one in `run_clone` (both run independently).
- `crates/control-server/src/mcp.rs`: `codex_recommended` + `codex_swap` tools mirroring the claude ones.

### 5. Scripts + provisioning
- New `crates/control-server/scripts/codex-import.sh` (sibling of claude-import.sh): `status` (cat auth.json, never fails), `read`, `clear`, `apply <b64>` (writes full auth.json 0600).
- `template/setup/30-user.sh`: after the claude install (~:140), install codex via the standalone installer (`CODEX_NON_INTERACTIVE=1`, warn-only on failure) — this now lives in the clone **template** build, not an in-product provisioning script. Existing images/clones need a new template publish + pull (`scripts/publish-template.sh` then `POST /api/images/pull`) or manual install — call out in docs; the automatic hot-swap engine (`binswap`) only ever syncs `clone-daemon`/`agent-wrapper`, it does NOT install new CLIs or re-provision.

### 6. Frontend
- Regenerate ts-rs (`cargo test -p wire`): `Host.ts` (+codex fields), new `CodexConfig.ts`, `AppConfigRedacted.ts`. Add codex fields to the hand-written `Host` in `frontend/app/lib/types.ts`.
- `frontend/app/lib/api.ts`: `codexAccount?` on `ClonePayload`; `refreshCodexUsage`, `checkCodexImport`, `importCodexAccount`, `recommendedCodexAccount`, `swapCodexAccount`.
- `frontend/app/routes/_index.tsx`: split `claudeAccounts` / `codexAccounts` by provider (both `assignable`); feed CloneModal + ChangeAccountModal both lists; SettingsPanel gains `codexAccountEmails`.
- `frontend/app/components/ImportAccountModal.tsx`: provider toggle (Claude | Codex) switching check/import calls; codex shows email + plan.
- `frontend/app/components/CloneModal.tsx`: second `AccountGroupSelect` ("Codex account", shown when codex accounts/groups exist), own auto/recommended state, `codexAccount` in every payload branch. `AccountGroupSelect` is reused as-is (ids `codex:<id>` don't collide).
- `frontend/app/components/ChangeAccountModal.tsx`: two pickers (Claude / Codex) with independent values from `claudeSelection`/`codexSelection`; Apply calls whichever changed (`swapClaudeAccount` / `swapCodexAccount`); loads `cloneGroups` + `codexGroups`.
- `frontend/app/components/SettingsPanel.tsx`: "Codex" section (poll secs, pinned email, auto-swap, usage-polling toggle) + "Codex groups" editor bound to `codexGroups` fed by `codexAccountEmails`; `save()` patch adds both.

### 7. Docs
API.md (codex endpoint block), PROTOCOL.md (`<a id="codex-accounts">` sibling section: `codex-accounts.json`, auth.json injection semantics, `CodexConfig`/`codex_groups`), SCRIPTS.md (codex-import.sh row + section, provision note), wire README.

## Migration / compat
Greenfield: all new Host/config fields are `Option`/`#[serde(default)]` — old state.json/config.json parse unchanged. No data migration. Claude subsystem behavior unchanged except the poller merge fix (which is a no-op while only one provider has accounts).

## Verification
1. `cargo build --workspace` && `cargo test --workspace` (regenerates ts-rs; new tests per §2/§3).
2. `cd frontend && bun run typecheck`.
3. Manual on staging CT 106:
   a. Install codex in a clone (installer one-liner) → `codex login` (ChatGPT mode) → auth.json has `tokens` + null `OPENAI_API_KEY`.
   b. `POST /api/codex/import/check` → email/plan/accountId (JWT decode). `POST /api/codex/import` → account in `codex-accounts.json` (0600), clone's auth.json cleared, ChatGPT-logo row with usage in the panel.
   c. **Decode the stored access_token `exp`** → confirm lifetime > REFRESH_LEAD_MS (60 min); shrink the lead if not.
   d. Swap a clone onto the account → its `~/.codex/auth.json` written with `refresh_token:""`; run `codex` in the clone → successful API call (proves the CLI tolerates the empty refresh token; if not, switch the script to the sentinel fallback).
   e. Force `expires_at ≈ now` in `codex-accounts.json` + `POST /api/codex/refresh` → server refreshes (rotating the refresh token) and the clone's auth.json changes (push-on-refresh).
   f. Two accounts in a `codex_groups` group + two bound clones → `POST /api/codex/rotate` re-balances, both stay authed.
   g. Coexistence: one clone with both a Claude and a Codex account → both credential files present; both pollers publish without clobbering (`/events` shows both provider rows persisting across polls).
   h. If `/wham/usage` errors: set `codex.usage_polling=false` → accounts still listed, assignment/refresh/push unaffected.
