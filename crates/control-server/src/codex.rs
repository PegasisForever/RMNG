//! Codex (OpenAI/ChatGPT) accounts — the sibling of `claude.rs`. Same server-owned
//! single-token model: the server holds each account's OAuth pair in the 0600 store
//! `codex-accounts.json`, refreshes access tokens itself (expiry decoded from the
//! access-token JWT — the Codex OAuth response has no `expires_in`), injects only the
//! short-lived access + id token + account_id into a clone's `~/.codex/auth.json` with an
//! empty refresh token, and re-pushes on every rotation. Importing harvests the OAuth
//! triple from a clone already signed in to Codex via ChatGPT, then clears the clone's
//! auth.json so its CLI can never rotate the refresh token the server now owns.
//!
//! The store and the refresh lifecycle are NOT here, and they no longer come from `claude.rs`
//! either. Both sides share one module ([`crate::account`]); this one is the adapter: the
//! account struct, the refresh POST, the usage/auto-reset specifics, and token delivery. What
//! used to make this file depend on Claude's internals — `RefreshRecord`, `token_alive`,
//! `grant_rejected`, even `PUSH_CONCURRENCY` — is neutral vocabulary there now.

use std::time::Duration;

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use wire::{ClaudeUsage, ClaudeUsageWindow};

use crate::account::{
    AccountKind, FETCH_TIMEOUT, PUSH_CONCURRENCY, ROTATE_SECS, RefreshFailure, RefreshRecord,
    Store, account_usable, fingerprint, refresh_status_is_fatal,
};
use crate::app::App;
use crate::clone_ops::{now_ms, rand_u64, snippet};

const USAGE_URL: &str = "https://chatgpt.com/backend-api/wham/usage";
const CONSUME_URL: &str = "https://chatgpt.com/backend-api/wham/rate-limit-reset-credits/consume";
const OAUTH_TOKEN_URL: &str = "https://auth.openai.com/oauth/token";
const OAUTH_CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";
/// Auto-reset only fires when every account's 7d window is at least this far from
/// resetting (spec: "more than 24h from the next 7d reset").
const RESET_MIN_HEADROOM_SECS: i64 = 24 * 3600;

/// Merge the `openai-codex` provider entry into a pi `auth.json` body, preserving the
/// operator's other providers. A missing, corrupt, or non-object current file seeds
/// from the fragment alone — same rule the old guest-side jq merge used.
fn merge_pi_auth(current: Option<&[u8]>, fragment: &serde_json::Value) -> serde_json::Value {
    let mut cur = current
        .and_then(|b| serde_json::from_slice::<serde_json::Value>(b).ok())
        .and_then(|v| v.as_object().cloned())
        .unwrap_or_default();
    if let Some(entry) = fragment.get("openai-codex") {
        cur.insert("openai-codex".to_string(), entry.clone());
    }
    serde_json::Value::Object(cur)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StoredCodexAccount {
    /// `codex:<account_id>`.
    pub id: String,
    pub email: String,
    pub account_id: String,
    #[serde(default)]
    pub plan: String,
    #[serde(default)]
    pub active: bool,
    pub access_token: String,
    #[serde(default)]
    pub id_token: String,
    pub refresh_token: String,
    #[serde(default)]
    pub expires_at: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_refresh: Option<RefreshRecord>,
}

/// The Codex side of the shared account store: one 0600 file of [`StoredCodexAccount`],
/// the refresh lifecycle in [`crate::account`].
pub(crate) type CodexStore = Store<StoredCodexAccount>;

impl AccountKind for StoredCodexAccount {
    const FILE: &'static str = "codex-accounts.json";
    const LABEL: &'static str = "Codex";
    /// A test elsewhere in the crate points this side's store at a disposable file.
    const PATH_ENV: Option<&'static str> = Some("RMNG_CODEX_ACCOUNTS_FILE");

    fn id(&self) -> &str {
        &self.id
    }
    fn email(&self) -> &str {
        &self.email
    }
    fn expires_at(&self) -> i64 {
        self.expires_at
    }
    fn refresh_token(&self) -> &str {
        &self.refresh_token
    }
    fn last_refresh(&self) -> Option<&RefreshRecord> {
        self.last_refresh.as_ref()
    }
    fn set_last_refresh(&mut self, r: RefreshRecord) {
        self.last_refresh = Some(r);
    }
    fn store(app: &App) -> &Store<Self> {
        &app.codex
    }

    /// The refresh itself. Returns the fingerprint of the token the reply carried, empty when
    /// it carried none.
    ///
    /// The OAuth response carries no `expires_in`, so expiry is decoded from the new access
    /// token's JWT ([`set_expiry_from_access`]) — the one real difference from Claude's POST.
    async fn refresh(http: &reqwest::Client, acct: &mut Self) -> Result<String, RefreshFailure> {
        let before = fingerprint(&acct.refresh_token);
        let resp = http
            .post(OAUTH_TOKEN_URL)
            .timeout(FETCH_TIMEOUT)
            .header("Content-Type", "application/json")
            .json(&serde_json::json!({
                "grant_type": "refresh_token",
                "refresh_token": acct.refresh_token,
                "client_id": OAUTH_CLIENT_ID,
            }))
            .send()
            .await
            .with_context(|| {
                format!(
                    "refresh {} (rt {before}) never got a reply, so the token may be spent",
                    acct.email
                )
            })?;
        let status = resp.status();
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            return Err(RefreshFailure {
                error: anyhow::anyhow!(
                    "refresh {} (rt {before}){}",
                    status.as_u16(),
                    snippet(&text)
                ),
                rejected: refresh_status_is_fatal(status.as_u16()),
            });
        }
        let data: RefreshResp = resp.json().await.with_context(|| {
            format!(
                "refresh {} (rt {before}) was accepted but its reply could not be read, \
                 so the token is spent and its replacement is lost",
                acct.email
            )
        })?;
        if let Some(a) = data.access_token {
            acct.access_token = a;
        }
        if let Some(i) = data.id_token {
            acct.id_token = i;
        }
        // Same single-use rule as Claude: a reply with no replacement leaves the store holding
        // a spent token, and the account dies at the next refresh rather than at this one.
        let after = match data.refresh_token {
            Some(r) => {
                let after = fingerprint(&r);
                acct.refresh_token = r;
                tracing::info!("refreshed codex {}: rt {before} -> {after}", acct.email);
                after
            }
            None => {
                tracing::error!(
                    "refreshed codex {}: the reply carried NO refresh_token, so the store keeps \
                     the one it just spent (rt {before}). This account fails its next refresh.",
                    acct.email
                );
                String::new()
            }
        };
        set_expiry_from_access(acct);
        Ok(after)
    }
}

// --- the account store ----------------------------------------------------

/// Drop an imported account from the secret store, with none of [`delete_account`]'s healing.
/// Exists so a test elsewhere in the crate can stage a delete landing mid-poll.
#[cfg(test)]
pub(crate) fn test_delete(app: &App, email: &str) {
    app.codex.delete(email).unwrap();
}

/// Replaces by **email**, not by `id`, for the reason spelled out on [`Store::upsert`]: every
/// caller looks an account up by email and takes the first match, so a second record under the
/// same email is unreachable and answers for the one that is reachable.
pub fn upsert_account(app: &App, stored: StoredCodexAccount) -> Result<()> {
    app.codex.upsert(stored)
}

// --- token refresh + push -------------------------------------------------

/// Set `acct.expires_at` from its access-token JWT `exp` claim; if the token isn't a
/// decodable JWT, fall back to a conservative 55-minute lifetime so the account still
/// refreshes before the CLI's 5-minute trigger.
fn set_expiry_from_access(acct: &mut StoredCodexAccount) {
    acct.expires_at = crate::clone_ops::jwt_exp_ms(&acct.access_token)
        .unwrap_or_else(|| now_ms() + 55 * 60 * 1000);
}

#[derive(Deserialize)]
struct RefreshResp {
    #[serde(default)]
    id_token: Option<String>,
    #[serde(default)]
    access_token: Option<String>,
    #[serde(default)]
    refresh_token: Option<String>,
}

/// `email`'s current account, refreshed (and persisted) first if within its refresh lead of
/// expiry. Returns `(account, rotated)`.
///
/// Runs in its own task with the refresh gate inside it, so a disconnected HTTP client cannot
/// abandon a rotation half-done. [`crate::account::refresh_and_persist`] carries the reasoning,
/// and this side has the same two cancellable callers in `/api/codex/{refresh,swap}` plus the
/// stuck detector. Spawning here rather than there keeps the spawned future concrete, which is
/// what lets the lifecycle itself stay generic.
pub async fn fresh_access_token(app: &App, email: &str) -> Result<(StoredCodexAccount, bool)> {
    let app = app.clone();
    let email = email.to_string();
    tokio::spawn(async move {
        crate::account::refresh_and_persist::<StoredCodexAccount>(&app, &email).await
    })
    .await
    .context("the codex refresh task did not finish")?
}

/// The `~/.codex/auth.json` body that runs codex under `acct`'s current tokens. The
/// refresh token is emptied and `last_refresh` set to now so the clone's CLI never tries
/// to rotate or abandon the server-owned token (see the module + PROTOCOL docs).
fn auth_json(acct: &StoredCodexAccount) -> String {
    let last_refresh = crate::docker::epoch_to_rfc3339(now_ms() / 1000);
    format!(
        r#"{{"OPENAI_API_KEY":null,"tokens":{{"id_token":"{id}","access_token":"{access}","refresh_token":"","account_id":"{acct_id}"}},"last_refresh":"{last_refresh}"}}"#,
        id = acct.id_token,
        access = acct.access_token,
        acct_id = acct.account_id,
    )
}

/// The `~/.pi/agent/auth.json` body that logs a stock `pi` in as `acct`.
///
/// pi keys credentials by provider id and reads only its own file, so it never sees the
/// Codex CLI's `~/.codex/auth.json`. Writing this second copy is what makes a `pi` the
/// operator installs themselves start authenticated instead of at a `/login` prompt.
///
/// `expires` is year 2100 for the same reason the refresh token is empty: pi refreshes any
/// OAuth credential within five minutes of expiry, and that call could only fail. The server
/// owns rotation and re-pushes both files roughly two hours before the real expiry, which is
/// about nine days out. Same trick as `claude::credentials_json`.
///
/// The clone's own agent-wrapper does NOT read this file. It bridges `~/.codex/auth.json`
/// directly through its own CredentialStore, so the assistant keeps working even when this
/// copy is stale or absent.
fn pi_auth_json(acct: &StoredCodexAccount) -> String {
    format!(
        r#"{{"openai-codex":{{"type":"oauth","access":"{access}","refresh":"","expires":4102444800000,"accountId":"{acct_id}"}}}}"#,
        access = acct.access_token,
        acct_id = acct.account_id,
    )
}

/// Install `acct`'s tokens into clone `host_id`'s `~/.codex/auth.json` and merge them into
/// `~/.pi/agent/auth.json` — direct filesystem writes into the clone's live home, no guest shell.
/// Sanity-checks the access token is a JWT (`eyJ…`). Best-effort hot-swap; codex and pi both
/// re-read their auth file per call.
pub async fn apply_clone_token(_app: &App, host_id: &str, acct: &StoredCodexAccount) -> Result<()> {
    if !acct.access_token.starts_with("eyJ") {
        bail!("refusing to apply a non-JWT codex access token");
    }
    // `~/.codex/auth.json` is server-owned wholesale: overwrite, never merge.
    crate::home_overlay::write_clone_home(
        host_id,
        ".codex/auth.json",
        auth_json(acct).as_bytes(),
        0o600,
    )
    .with_context(|| format!("{host_id}: writing Codex auth"))?;
    // pi's file belongs to the operator (their other providers live in it): merge only
    // the `openai-codex` key, never overwrite. Upload only on change.
    let fragment: serde_json::Value = serde_json::from_str(&pi_auth_json(acct))
        .with_context(|| format!("{host_id}: pi auth fragment is not JSON"))?;
    let current = crate::home_overlay::read_clone_home(host_id, ".pi/agent/auth.json")
        .with_context(|| format!("{host_id}: reading pi auth"))?;
    let merged = merge_pi_auth(current.as_deref(), &fragment);
    let current_value = current
        .as_deref()
        .and_then(|b| serde_json::from_slice::<serde_json::Value>(b).ok());
    if current_value.as_ref() != Some(&merged) {
        crate::home_overlay::write_clone_home(
            host_id,
            ".pi/agent/auth.json",
            merged.to_string().as_bytes(),
            0o600,
        )
        .with_context(|| format!("{host_id}: writing pi auth"))?;
    }
    Ok(())
}

/// Remove clone `host_id`'s `~/.codex/auth.json`, leaving it with no Codex token, and
/// drop only the `openai-codex` key from its pi auth file — the operator's other
/// providers stay. Removes the pi file itself when no keys remain.
pub async fn clear_clone_token(_app: &App, host_id: &str) -> Result<()> {
    crate::home_overlay::remove_clone_home(host_id, ".codex/auth.json")
        .with_context(|| format!("{host_id}: clearing Codex auth"))?;
    if let Some(bytes) = crate::home_overlay::read_clone_home(host_id, ".pi/agent/auth.json")
        .with_context(|| format!("{host_id}: reading pi auth for clear"))?
    {
        if let Ok(serde_json::Value::Object(mut map)) =
            serde_json::from_slice::<serde_json::Value>(&bytes)
        {
            if map.remove("openai-codex").is_some() {
                if map.is_empty() {
                    crate::home_overlay::remove_clone_home(host_id, ".pi/agent/auth.json")
                        .with_context(|| format!("{host_id}: removing emptied pi auth"))?;
                } else {
                    crate::home_overlay::write_clone_home(
                        host_id,
                        ".pi/agent/auth.json",
                        serde_json::Value::Object(map).to_string().as_bytes(),
                        0o600,
                    )
                    .with_context(|| format!("{host_id}: writing cleared pi auth"))?;
                }
            }
        }
        // A missing, corrupt, or non-object pi file is none of ours to repair: the codex
        // file above is already gone, which is what de-authenticates the clone.
    }
    Ok(())
}

/// Refresh-if-needed and install `email`'s tokens into clone `host_id`, recording the
/// push. If the refresh rotated the token, fan it out to the account's other clones.
pub async fn push_account_to_clone(app: &App, host_id: &str, email: &str) -> Result<()> {
    let (acct, rotated) = fresh_access_token(app, email).await?;
    let applied = apply_clone_token(app, host_id, &acct).await;
    if applied.is_ok() {
        app.codex
            .pushed
            .lock()
            .unwrap()
            .insert(host_id.to_string(), acct.access_token.clone());
    }
    // Fan out even when this clone's own push failed — see `claude::push_account_to_clone`
    // for why. The refresh has already invalidated the previous token for every clone on
    // this account, and the one that triggered it is often a stopped clone that could never
    // have taken the new one.
    if rotated {
        let app = app.clone();
        let email = email.to_string();
        tokio::spawn(async move { push_stale_tokens_for(&app, Some(&email)).await });
    }
    applied
}

/// Fleet-wide reconcile pass: see [`push_stale_tokens_for`]. Runs at the end of every
/// poll to retry failed pushes and catch out-of-band reassignments.
pub async fn push_stale_tokens(app: &App) {
    push_stale_tokens_for(app, None).await;
}

/// Give every clone assigned a Codex account that account's current access token, unless
/// the last successful push already delivered it. With `only` set, visit just that
/// account's clones. Mirrors `claude::push_stale_tokens_for` (which carries the reasoning
/// on scope), reading `RmngClone.codex_account_email`.
/// See `claude::push_stale_tokens_for` for why this runs wide rather than one at a time: a
/// refresh kills the previous token outright, so every clone still holding it is broken
/// until this arrives.
pub async fn push_stale_tokens_for(app: &App, only: Option<&str>) {
    let started = std::time::Instant::now();
    let mut targets = Vec::new();
    let mut skipped_fresh = 0usize;

    for host in app.store.get().hosts {
        let Some(email) = host.codex_account_email.as_deref() else {
            continue;
        };
        // Archived clones cannot take a push; see `claude::push_stale_tokens_for`.
        if only.is_some_and(|want| want != email) || !host.managed || host.archived {
            continue;
        }
        let Some(acct) = app.codex.get_by_email(email) else {
            tracing::warn!(
                "clone {} is bound to codex account {email}, which is not imported; leaving its token alone",
                host.id
            );
            continue;
        };
        if app.codex.pushed.lock().unwrap().get(&host.id) == Some(&acct.access_token) {
            skipped_fresh += 1;
            continue;
        }
        targets.push((host.id.clone(), email.to_string(), acct));
    }

    if targets.is_empty() {
        tracing::debug!("codex token push: nothing to do ({skipped_fresh} already current)");
        return;
    }
    tracing::info!(
        "codex token push: {} clone(s) to update ({skipped_fresh} already current)",
        targets.len()
    );

    let (mut ok, mut failed, mut unreachable) = (0usize, 0usize, 0usize);
    for chunk in targets.chunks(PUSH_CONCURRENCY) {
        let results = futures::future::join_all(chunk.iter().map(|(id, email, acct)| async move {
            // Plain home write now: works stopped or running, skips only deleted clones.
            if !crate::home_overlay::clone_home_present(id) {
                return (id, email, acct, None);
            }
            (
                id,
                email,
                acct,
                Some(apply_clone_token(app, id, acct).await),
            )
        }))
        .await;

        for (id, email, acct, outcome) in results {
            match outcome {
                None => {
                    unreachable += 1;
                    tracing::debug!("skipping codex token push to {id}: no live home");
                }
                Some(Ok(())) => {
                    ok += 1;
                    app.codex
                        .pushed
                        .lock()
                        .unwrap()
                        .insert(id.clone(), acct.access_token.clone());
                    tracing::info!("pushed fresh codex token ({email}) to {id}");
                }
                Some(Err(e)) => {
                    failed += 1;
                    tracing::warn!(
                        "pushing codex token ({email}) to {id} failed (retried next pass): {e}"
                    );
                }
            }
        }
    }

    tracing::info!(
        "codex token push done in {:?}: {ok} pushed, {failed} failed, {unreachable} without live home",
        started.elapsed()
    );
}

// --- usage fetch + mapping -------------------------------------------------

#[derive(Deserialize)]
struct RawRateWindow {
    #[serde(default)]
    used_percent: Option<f64>,
    #[serde(default)]
    limit_window_seconds: Option<i64>,
    /// Epoch SECONDS when the window resets (the ChatGPT usage API returns a number here,
    /// unlike Claude's ISO string) — converted to an ISO timestamp in [`window_of`].
    #[serde(default)]
    reset_at: Option<i64>,
}
#[derive(Deserialize)]
struct RawRateLimit {
    #[serde(default)]
    primary_window: Option<RawRateWindow>,
    #[serde(default)]
    secondary_window: Option<RawRateWindow>,
}
#[derive(Deserialize)]
struct RawResetCredits {
    #[serde(default)]
    available_count: Option<i64>,
}
#[derive(Deserialize)]
struct RawUsage {
    #[serde(default)]
    plan_type: Option<String>,
    #[serde(default)]
    rate_limit: Option<RawRateLimit>,
    #[serde(default)]
    rate_limit_reset_credits: Option<RawResetCredits>,
}

async fn fetch_usage(http: &reqwest::Client, token: &str, account_id: &str) -> Result<RawUsage> {
    let resp = http
        .get(USAGE_URL)
        .timeout(FETCH_TIMEOUT)
        .header("Authorization", format!("Bearer {token}"))
        .header("ChatGPT-Account-Id", account_id)
        .send()
        .await?;
    let status = resp.status();
    if !status.is_success() {
        let text = resp.text().await.unwrap_or_default();
        bail!("usage {}{}", status.as_u16(), snippet(&text));
    }
    Ok(resp.json().await?)
}

#[derive(Debug, PartialEq, Eq)]
enum ConsumeOutcome {
    Reset,
    NothingToReset,
    NoCredit,
    AlreadyRedeemed,
    Unknown(String),
}

fn parse_consume_outcome(body: &str) -> ConsumeOutcome {
    let code = serde_json::from_str::<serde_json::Value>(body)
        .ok()
        .and_then(|v| v.get("code").and_then(|c| c.as_str()).map(str::to_string))
        .unwrap_or_default();
    match code.as_str() {
        "reset" => ConsumeOutcome::Reset,
        "nothingToReset" => ConsumeOutcome::NothingToReset,
        "noCredit" => ConsumeOutcome::NoCredit,
        "alreadyRedeemed" => ConsumeOutcome::AlreadyRedeemed,
        other => ConsumeOutcome::Unknown(other.to_string()),
    }
}

/// A 32-hex-char idempotency key (no `uuid` dep; `rand_u64` from `clone_ops`).
fn new_request_id() -> String {
    format!("{:016x}{:016x}", rand_u64(), rand_u64())
}

/// POST one reset-credit consume. Mirrors `fetch_usage` headers/timeout/error style.
async fn consume_reset(
    http: &reqwest::Client,
    token: &str,
    account_id: &str,
    request_id: &str,
) -> Result<ConsumeOutcome> {
    let resp = http
        .post(CONSUME_URL)
        .timeout(FETCH_TIMEOUT)
        .header("Authorization", format!("Bearer {token}"))
        .header("ChatGPT-Account-Id", account_id)
        .json(&serde_json::json!({ "redeem_request_id": request_id }))
        .send()
        .await?;
    let status = resp.status();
    let text = resp.text().await.unwrap_or_default();
    if !status.is_success() {
        bail!("consume {}{}", status.as_u16(), snippet(&text));
    }
    Ok(parse_consume_outcome(&text))
}

/// A rolling window whose `limit_window_seconds` is nearer 5h (18000s) than a week
/// (604800s) maps to the 5h bar, else the weekly bar — never by field order.
fn window_of(w: RawRateWindow) -> Option<(bool, ClaudeUsageWindow)> {
    let secs = w.limit_window_seconds?;
    let is_five = (secs - 18_000).abs() <= (secs - 604_800).abs();
    Some((
        is_five,
        ClaudeUsageWindow {
            pct: w.used_percent.unwrap_or(0.0).round(),
            // ISO string for the frontend (ClaudeUsageWindow.resetsAt → Date.parse).
            resets_at: w.reset_at.map(crate::docker::epoch_to_rfc3339),
        },
    ))
}

fn to_usage(acct: &StoredCodexAccount, raw: RawUsage) -> ClaudeUsage {
    let mut five_hour = None;
    let mut seven_day = None;
    if let Some(rl) = raw.rate_limit {
        for w in [rl.primary_window, rl.secondary_window]
            .into_iter()
            .flatten()
        {
            if let Some((is_five, win)) = window_of(w) {
                if is_five {
                    five_hour = Some(win);
                } else {
                    seven_day = Some(win);
                }
            }
        }
    }
    let _ = raw.plan_type; // plan is stored on the account, not the usage view
    let reset_credits = raw
        .rate_limit_reset_credits
        .as_ref()
        .and_then(|c| c.available_count);
    ClaudeUsage {
        id: acct.id.clone(),
        email: acct.email.clone(),
        provider: Some(wire::Provider::Codex),
        active: acct.active,
        assignable: None,
        error: None,
        stale: None,
        last_updated: now_ms(),
        five_hour,
        seven_day,
        fable: None, // Fable is a Claude-model limit; never present for Codex.
        spend: None,
        reset_credits,
    }
}

fn codex_base(acct: &StoredCodexAccount) -> ClaudeUsage {
    ClaudeUsage {
        id: acct.id.clone(),
        email: acct.email.clone(),
        provider: Some(wire::Provider::Codex),
        active: acct.active,
        assignable: None,
        error: None,
        stale: None,
        last_updated: now_ms(),
        five_hour: None,
        seven_day: None,
        fable: None,
        spend: None,
        reset_credits: None,
    }
}

/// Per-account inputs the fleet gate needs, extracted from a fresh raw usage fetch
/// (epoch-seconds based, so the gate never round-trips the display ISO string).
struct FleetFacts {
    account_id: String,
    seven_pct: f64,
    seven_reset_at: i64,
    reset_credits: i64,
}

/// Extract gate facts from a raw usage response. `None` if the weekly window or its
/// reset epoch is missing — such an account can't be confirmed, so the gate won't fire.
///
/// Deliberately uses the raw (unrounded) weekly `used_percent` for the gate decision,
/// whereas the display path rounds — so the gate can fire at raw 95.4% while the badge
/// still shows 95%.
fn gate_facts(account_id: &str, raw: &RawUsage) -> Option<FleetFacts> {
    let rl = raw.rate_limit.as_ref()?;
    // Weekly window = the one whose length is nearer a week than 5h (never by field order).
    let seven = [rl.primary_window.as_ref(), rl.secondary_window.as_ref()]
        .into_iter()
        .flatten()
        .find(|w| {
            let s = w.limit_window_seconds.unwrap_or(0);
            (s - 604_800).abs() <= (s - 18_000).abs()
        })?;
    Some(FleetFacts {
        account_id: account_id.to_string(),
        seven_pct: seven.used_percent.unwrap_or(0.0),
        seven_reset_at: seven.reset_at?,
        reset_credits: raw
            .rate_limit_reset_credits
            .as_ref()
            .and_then(|c| c.available_count)
            .unwrap_or(0),
    })
}

/// The fleet gate. Returns the account id to spend one reset on, or `None`.
fn choose_reset_target(
    facts: &[FleetFacts],
    account_count: usize,
    marks: &[wire::CodexResetMark],
    now_secs: i64,
    enabled: bool,
) -> Option<String> {
    if !enabled || account_count == 0 || facts.len() != account_count {
        return None; // off, no accounts, or incomplete fresh data → never fire.
    }
    let all_capped = facts
        .iter()
        .all(|f| f.seven_pct > crate::pool::SEVEN_DAY_CAP_PCT);
    let none_soon = facts
        .iter()
        .all(|f| f.seven_reset_at - now_secs >= RESET_MIN_HEADROOM_SECS);
    if !all_capped || !none_soon {
        return None;
    }
    let mut eligible: Vec<&FleetFacts> = facts
        .iter()
        .filter(|f| {
            f.reset_credits > 0
                && !marks
                    .iter()
                    .any(|m| m.account_id == f.account_id && m.window_resets_at == f.seven_reset_at)
        })
        .collect();
    // Most credits first; tie-break by soonest reset.
    eligible.sort_by(|a, b| {
        b.reset_credits
            .cmp(&a.reset_credits)
            .then(a.seven_reset_at.cmp(&b.seven_reset_at))
    });
    eligible.first().map(|f| f.account_id.clone())
}

/// Drop marks whose 7d window has already elapsed (account is now in a new window).
fn prune_marks(marks: &mut Vec<wire::CodexResetMark>, now_secs: i64) {
    marks.retain(|m| m.window_resets_at > now_secs);
}

pub async fn rotate_once(app: &App) {
    crate::pool::rotate_once::<crate::pool::CodexPool>(app).await
}

/// Delete an imported Codex account by email, then heal the fleet — the Codex twin of
/// [`crate::claude::delete_account`], including its ordering: everything visible is settled
/// before this returns (token gone, row out of the published state, no clone pointing at
/// it), and the re-placement runs in the background. Refuses if any clone is pinned to it.
/// Returns the ids of clones that were on the account.
pub async fn delete_account(app: &App, email: &str) -> Result<Vec<String>> {
    let pinned: Vec<String> = app
        .store
        .get()
        .hosts
        .iter()
        .filter(|h| h.codex_selection.as_deref() == Some(email))
        .map(|h| h.id.clone())
        .collect();
    if !pinned.is_empty() {
        bail!(
            "{n} clone(s) are pinned to {email}: {ids}. Reassign them (swap to another \
             account, a group, or auto) before deleting the account.",
            n = pinned.len(),
            ids = pinned.join(", "),
        );
    }
    let account_id = app.codex.get_by_email(email).map(|a| a.id);
    if !app.codex.delete(email)? {
        bail!("no imported Codex account '{email}'");
    }
    if let Some(id) = &account_id {
        app.codex.last_good.lock().unwrap().remove(id);
    }

    let on_it: Vec<String> = app
        .store
        .get()
        .hosts
        .iter()
        .filter(|h| h.codex_account_email.as_deref() == Some(email))
        .map(|h| h.id.clone())
        .collect();
    for id in &on_it {
        app.codex.forget_pushed(id);
    }

    app.store.mutate(|s| {
        s.claude_accounts
            .retain(|u| u.provider != Some(wire::Provider::Codex) || u.email != email);
        for h in &mut s.hosts {
            if h.codex_account_email.as_deref() == Some(email) {
                h.codex_account_email = None;
            }
        }
    });

    let bg = app.clone();
    tokio::spawn(async move { rotate_once(&bg).await });
    Ok(on_it)
}

/// Move both of a clone's Codex bindings from `old` to `new`, fleet-wide, in one mutation.
/// The Codex twin of `crate::claude::repoint_clones` — see there for why both bindings move
/// and why this is separate from the config write.
fn repoint_clones(app: &App, old: &str, new: &str) -> Vec<String> {
    crate::pool::repoint_clones::<crate::pool::CodexPool>(app, old, new)
}

/// Hand everything `old_email` holds to `new_email`, then delete it — the Codex twin of
/// [`crate::claude::replace_account`]. Same contract: pools and both bindings move, a
/// sign-in as the same account is a no-op, and the token delivery is backgrounded.
pub async fn replace_account(app: &App, old_email: &str, new_email: &str) -> Result<Vec<String>> {
    if old_email == new_email {
        return Ok(Vec::new());
    }
    if app.codex.get_by_email(old_email).is_none() {
        bail!("no imported Codex account '{old_email}' to replace");
    }
    if app.codex.get_by_email(new_email).is_none() {
        bail!("'{new_email}' is not an imported Codex account");
    }

    let mut cfg = app.config();
    let joined = crate::pool::swap_pool_member(&mut cfg.groups, old_email, new_email);
    crate::config::save(&cfg).context("saving the replacement's pool membership")?;
    *app.cfg.write().unwrap() = cfg;

    let moved = repoint_clones(app, old_email, new_email);
    delete_account(app, old_email).await?;
    tracing::info!(
        "replaced Codex account {old_email} with {new_email}: {} clone(s), pool(s) {}",
        moved.len(),
        if joined.is_empty() {
            "none".to_string()
        } else {
            joined.join(", ")
        },
    );

    let bg = app.clone();
    let email = new_email.to_string();
    tokio::spawn(async move { push_stale_tokens_for(&bg, Some(&email)).await });
    Ok(moved)
}

pub async fn run_rotator(app: App) {
    tokio::time::sleep(Duration::from_secs(30)).await;
    loop {
        rotate_once(&app).await;
        tokio::time::sleep(Duration::from_secs(ROTATE_SECS)).await;
    }
}

// --- poller ----------------------------------------------------------------

pub async fn poll_once(app: &App) -> Result<bool> {
    // Guarded, not set-and-clear. Same reason as the Claude poller: this is awaited inside
    // HTTP handlers, and a dropped handler future skips the line after the await, leaving
    // the flag set forever. See [`crate::clone_ops::PollGuard`].
    let Some(_guard) = crate::clone_ops::try_poll(&app.codex.polling) else {
        return Ok(false);
    };
    poll_inner(app).await
}

async fn poll_inner(app: &App) -> Result<bool> {
    let accts = app.codex.snapshot();
    let cfg = app.config();
    if accts.is_empty() {
        crate::clone_ops::replace_provider_views(app, wire::Provider::Codex, Vec::new());
        return Ok(false);
    }

    let mut any429 = false;
    let mut views = Vec::with_capacity(accts.len());
    let mut fleet: Vec<FleetFacts> = Vec::with_capacity(accts.len());

    for (i, acct) in accts.iter().enumerate() {
        if i > 0 {
            tokio::time::sleep(crate::pool::STAGGER).await;
        }
        let outcome = async {
            let (fresh, rotated) = fresh_access_token(app, &acct.email).await?;
            if rotated {
                // Deliver before the usage fetch: this account's clones are holding the
                // token the refresh above just replaced.
                push_stale_tokens_for(app, Some(&acct.email)).await;
            }
            let raw = fetch_usage(&app.http, &fresh.access_token, &fresh.account_id).await?;
            let facts = gate_facts(&acct.id, &raw); // borrow before `raw` moves into to_usage
            Ok::<_, anyhow::Error>((to_usage(acct, raw), facts))
        }
        .await;
        match outcome {
            Ok((mut u, facts)) => {
                // The refresh above ran on this account's own token, so the token works.
                u.assignable = Some(true);
                app.codex
                    .last_good
                    .lock()
                    .unwrap()
                    .insert(acct.id.clone(), u.clone());
                views.push(u);
                if let Some(f) = facts {
                    fleet.push(f);
                }
            }
            Err(e) => {
                let msg = e.to_string();
                if msg.contains("429") {
                    any429 = true;
                }
                // Re-read the account: a refresh that succeeded earlier in this same pass
                // moved `expires_at`, and the snapshot taken at the top has not. It is also
                // where the rejection this pass may have just recorded lives.
                let alive = app
                    .codex
                    .get_by_email(&acct.email)
                    .is_some_and(|a| account_usable(&a, now_ms()));
                let prev = app.codex.last_good.lock().unwrap().get(&acct.id).cloned();
                views.push(match prev {
                    Some(mut p) => {
                        p.stale = Some(true);
                        // Carry the reason, not just the fact. Without it a dead refresh
                        // token and a momentary 429 both read as "these numbers are old".
                        p.error = Some(msg);
                        p.assignable = Some(alive);
                        p
                    }
                    None => {
                        let mut b = codex_base(acct);
                        b.error = Some(msg);
                        b.assignable = Some(alive);
                        b
                    }
                });
            }
        }
    }

    // --- fleet auto-reset gate ---------------------------------------------
    let now_secs = now_ms() / 1000;
    let marks = app.store.get().codex_reset_marks;
    if let Some(target_id) =
        choose_reset_target(&fleet, accts.len(), &marks, now_secs, cfg.codex.auto_reset)
    {
        if let (Some(target_facts), Some(acct)) = (
            fleet.iter().find(|f| f.account_id == target_id),
            accts.iter().find(|a| a.id == target_id),
        ) {
            let window = target_facts.seven_reset_at;
            let req_id = new_request_id();
            match fresh_access_token(app, &acct.email).await {
                Ok((fresh, _)) => {
                    // Reserve the cooldown mark now that the refresh succeeded, still
                    // BEFORE the POST (no outcome can double-spend).
                    app.store.mutate(|s| {
                        s.codex_reset_marks.retain(|m| m.account_id != target_id);
                        s.codex_reset_marks.push(wire::CodexResetMark {
                            account_id: target_id.clone(),
                            window_resets_at: window,
                            consumed_at: now_ms(),
                            redeem_request_id: req_id.clone(),
                        });
                        prune_marks(&mut s.codex_reset_marks, now_secs);
                    });
                    match consume_reset(&app.http, &fresh.access_token, &fresh.account_id, &req_id)
                        .await
                    {
                        Ok(ConsumeOutcome::Reset) => {
                            tracing::info!(
                                "codex auto-reset consumed for {} (7d was {:.0}%); re-polling",
                                acct.email,
                                target_facts.seven_pct
                            );
                            // Best-effort immediate re-poll of just this account.
                            if let Ok(raw2) =
                                fetch_usage(&app.http, &fresh.access_token, &fresh.account_id).await
                            {
                                let u2 = to_usage(acct, raw2);
                                tracing::info!(
                                    "codex auto-reset after: {} 7d={:?} credits={:?}",
                                    acct.email,
                                    u2.seven_day.as_ref().map(|w| w.pct),
                                    u2.reset_credits
                                );
                                app.codex
                                    .last_good
                                    .lock()
                                    .unwrap()
                                    .insert(acct.id.clone(), u2.clone());
                                if let Some(v) = views.iter_mut().find(|v| v.id == acct.id) {
                                    *v = u2;
                                    v.assignable = Some(true);
                                }
                            }
                        }
                        Ok(other) => tracing::warn!(
                            "codex auto-reset for {}: {:?} (mark kept, no retry this window)",
                            acct.email,
                            other
                        ),
                        Err(e) => tracing::warn!(
                            "codex auto-reset consume for {} failed: {e} (mark kept)",
                            acct.email
                        ),
                    }
                }
                Err(e) => tracing::warn!(
                    "codex auto-reset: token refresh for {} failed: {e} (no mark reserved, retrying next poll)",
                    acct.email
                ),
            }
        }
    }

    crate::clone_ops::replace_provider_views(app, wire::Provider::Codex, views);

    push_stale_tokens(app).await;

    Ok(any429)
}

pub async fn run_poller(app: App) {
    const MAX_BACKOFF: Duration = Duration::from_secs(30 * 60);
    let mut backoff: u32 = 0;
    loop {
        let any429 = match poll_once(&app).await {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!("codex usage poll failed: {e}");
                false
            }
        };
        let base = Duration::from_secs(wire::CODEX_POLL_SECS.max(15));
        let delay = if any429 {
            backoff = (backoff + 1).min(8);
            let escalate = backoff.saturating_sub(2);
            (base * 2u32.pow(escalate)).min(MAX_BACKOFF)
        } else {
            backoff = 0;
            base
        };
        if any429 {
            tracing::warn!(
                "codex usage rate-limited (429); next poll in {}s",
                delay.as_secs()
            );
        }
        tokio::time::sleep(delay).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    use crate::pool::{
        CodexPool, RotationCandidate, assign_rotation, assign_saturated_rotation, auto_pool_clones,
        is_exhausted,
    };
    use base64::Engine;
    use base64::engine::general_purpose::STANDARD as B64;
    use wire::RmngClone;

    /// Parity with `claude::jittered_lead_never_drops_below_the_floor`: the offset only
    /// ever adds lead, and it is derived from the email so a batch of accounts imported
    /// together stops coming due in the same second.

    fn jwt_with(payload: &str) -> String {
        let b64 = B64.encode(payload.as_bytes());
        let url = b64
            .trim_end_matches('=')
            .replace('+', "-")
            .replace('/', "_");
        format!("eyJhbGciOiJub25lIn0.{url}.sig")
    }

    fn sample_account() -> StoredCodexAccount {
        StoredCodexAccount {
            id: "codex:acc-1".into(),
            email: "z@openai.com".into(),
            account_id: "acc-1".into(),
            plan: "plus".into(),
            active: false,
            access_token: "eyJaccess".into(),
            id_token: "eyJid".into(),
            refresh_token: "rt-1".into(),
            expires_at: 0,
            last_refresh: None,
        }
    }

    /// The Codex twin of `claude::a_rejected_grant_is_never_posted_to_the_provider_again`.
    /// Both providers rotate a single-use refresh token, so both stop asking once the
    /// provider has rejected one.
    #[tokio::test]
    async fn a_rejected_codex_grant_is_never_posted_to_the_provider_again() {
        use std::sync::Arc;
        let dir = std::env::temp_dir().join(format!(
            "rmng-codex-rejected-{}-{}",
            std::process::id(),
            crate::clone_ops::rand_u64()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let store = Arc::new(crate::state::StateStore::load(dir.join("state.json")).unwrap());
        let app = App::new(store, wire::AppConfig::default(), &dir.to_string_lossy());
        let mut acct = sample_account();
        acct.expires_at = 0; // long expired, so a refresh is due
        acct.last_refresh = Some(RefreshRecord {
            at: 1,
            ok: false,
            rt_before: "beef".into(),
            rt_after: String::new(),
            error: Some("refresh 400: invalid_grant".into()),
            rejected: true,
        });
        app.codex.update_account(&acct).unwrap();

        let err = fresh_access_token(&app, &acct.email)
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains("signed in again"), "message: {err}");
        let after = app.codex.get_by_email(&acct.email).unwrap();
        assert_eq!(after.last_refresh.unwrap().rt_before, "beef");
    }

    #[test]
    fn store_upsert_roundtrip() {
        let dir = std::env::temp_dir().join(format!("rmng-codex-store-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let store = CodexStore::load(dir.to_str().unwrap());
        store.update_account(&sample_account()).unwrap();
        // Second store loading the same file sees the account.
        let reloaded = CodexStore::load(dir.to_str().unwrap());
        assert_eq!(reloaded.emails(), vec!["z@openai.com".to_string()]);
        assert_eq!(
            reloaded.get_by_email("z@openai.com").unwrap().account_id,
            "acc-1"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn injected_auth_json_shape() {
        let j = auth_json(&sample_account());
        let v: serde_json::Value = serde_json::from_str(&j).unwrap();
        assert!(v["OPENAI_API_KEY"].is_null());
        assert_eq!(v["tokens"]["access_token"], "eyJaccess");
        assert_eq!(v["tokens"]["id_token"], "eyJid");
        assert_eq!(v["tokens"]["account_id"], "acc-1");
        // Refresh token is emptied — the clone can never rotate the server-owned token.
        assert_eq!(v["tokens"]["refresh_token"], "");
        // last_refresh is a present RFC3339 string (defeats the CLI's 8-day fallback).
        assert!(v["last_refresh"].as_str().is_some_and(|s| s.contains('T')));
    }

    /// pi keys credentials by provider id and never reads ~/.codex, so this second file is
    /// the only thing that logs a stock `pi` in.
    #[test]
    fn injected_pi_auth_json_shape() {
        let j = pi_auth_json(&sample_account());
        let v: serde_json::Value = serde_json::from_str(&j).unwrap();
        let c = &v["openai-codex"];
        assert_eq!(c["type"], "oauth");
        assert_eq!(c["access"], "eyJaccess");
        assert_eq!(c["accountId"], "acc-1");
        // Empty for the same reason as the codex file: the server owns rotation.
        assert_eq!(c["refresh"], "");
        // Year 2100. A real expiry would make pi attempt a refresh that can only fail.
        assert_eq!(c["expires"], 4102444800000i64);
    }

    /// Merging the pushed fragment must never drop the operator's other providers —
    /// the live bug this replaces (guest-side overwrite wiped them).
    #[test]
    fn pi_auth_merge_keeps_other_providers() {
        let fragment: serde_json::Value =
            serde_json::from_str(&pi_auth_json(&sample_account())).unwrap();
        let current = serde_json::json!({
            "anthropic": {"type": "oauth", "access": "KEEP"},
            "openai-codex": {"type": "oauth", "access": "OLD"}
        });
        let merged = merge_pi_auth(Some(current.to_string().as_bytes()), &fragment);
        assert_eq!(merged["anthropic"]["access"], "KEEP");
        assert_eq!(merged["openai-codex"]["access"], "eyJaccess");
    }

    #[test]
    fn pi_auth_merge_seeds_missing_or_corrupt_files() {
        let fragment: serde_json::Value =
            serde_json::from_str(&pi_auth_json(&sample_account())).unwrap();
        let seeded = merge_pi_auth(None, &fragment);
        assert_eq!(seeded["openai-codex"]["access"], "eyJaccess");
        let corrupt = merge_pi_auth(Some(b"{broken".as_slice()), &fragment);
        assert_eq!(corrupt, seeded);
    }

    /// Both files carry the same access token, so a clone can never run codex under one
    /// account while pi runs under another.
    #[test]
    fn both_auth_files_carry_the_same_token() {
        let acct = sample_account();
        let codex: serde_json::Value = serde_json::from_str(&auth_json(&acct)).unwrap();
        let pi: serde_json::Value = serde_json::from_str(&pi_auth_json(&acct)).unwrap();
        assert_eq!(
            codex["tokens"]["access_token"],
            pi["openai-codex"]["access"]
        );
        assert_eq!(
            codex["tokens"]["account_id"],
            pi["openai-codex"]["accountId"]
        );
    }

    #[test]
    fn usage_maps_by_window_seconds_not_order() {
        // Real chatgpt.com/backend-api/wham/usage shape: `used_percent` is a bare number,
        // `reset_at` is epoch SECONDS (not an ISO string), and there are sibling fields we
        // ignore (`allowed`, `reset_after_seconds`, `additional_rate_limits`). primary=5h,
        // secondary=weekly.
        let body = r#"{"plan_type":"pro","rate_limit":{"allowed":true,
            "primary_window":{"used_percent":12,"limit_window_seconds":18000,"reset_after_seconds":2434,"reset_at":1609459200},
            "secondary_window":{"used_percent":3,"limit_window_seconds":604800,"reset_at":1612137600}
        },"additional_rate_limits":[]}"#;
        let u = to_usage(&sample_account(), serde_json::from_str(body).unwrap());
        assert_eq!(u.five_hour.as_ref().unwrap().pct, 12.0);
        assert_eq!(u.seven_day.as_ref().unwrap().pct, 3.0);
        // Epoch seconds are converted to an ISO string so the frontend's Date.parse works.
        assert_eq!(
            u.five_hour.as_ref().unwrap().resets_at.as_deref(),
            Some("2021-01-01T00:00:00Z")
        );
        assert_eq!(
            u.seven_day.as_ref().unwrap().resets_at.as_deref(),
            Some("2021-02-01T00:00:00Z")
        );
        assert_eq!(u.provider, Some(wire::Provider::Codex));
        assert!(u.spend.is_none());
        // Swapped field order: still classified by limit_window_seconds. `reset_at` absent
        // here → resets_at is None (window still maps).
        let swapped = r#"{"rate_limit":{
            "primary_window":{"used_percent":3,"limit_window_seconds":604800},
            "secondary_window":{"used_percent":12,"limit_window_seconds":18000}
        }}"#;
        let u2 = to_usage(&sample_account(), serde_json::from_str(swapped).unwrap());
        assert_eq!(u2.five_hour.as_ref().unwrap().pct, 12.0);
        assert_eq!(u2.seven_day.as_ref().unwrap().pct, 3.0);
        assert!(u2.five_hour.as_ref().unwrap().resets_at.is_none());
    }

    #[test]
    fn expiry_decoded_from_access_jwt() {
        // apply_expiry_from_jwt sets expires_at from the access token's exp claim.
        let mut acct = sample_account();
        acct.access_token = jwt_with(r#"{"exp":2000000000}"#);
        set_expiry_from_access(&mut acct);
        assert_eq!(acct.expires_at, 2_000_000_000_000);
    }

    fn clone_host(id: &str, cur: Option<&str>) -> RmngClone {
        RmngClone {
            id: id.into(),
            managed: true,
            codex_account_email: cur.map(str::to_string),
            ..Default::default()
        }
    }

    #[test]
    fn assignment_uses_codex_account_field() {
        // Sticky keep: a clone on an eligible account stays; a homeless clone lands in-set.
        let eligible = ["a@o".to_string(), "b@o".to_string()];
        let clones = [
            clone_host("c1", Some("a@o")),
            clone_host("c2", Some("z@gone")),
        ];
        for _ in 0..50 {
            let got = assign_rotation::<CodexPool>(&clones, &eligible, &HashMap::new());
            let by_id: HashMap<_, _> = got.iter().map(|(h, e)| (h.id.clone(), e.clone())).collect();
            assert_eq!(by_id["c1"], "a@o");
            assert_eq!(by_id["c2"], "b@o");
        }
    }

    fn codex_rotation_candidate(
        email: &str,
        seven_pct: f64,
        seven_reset: Option<i64>,
    ) -> RotationCandidate {
        RotationCandidate {
            email: email.to_string(),
            five_pct: 0.0,
            seven_pct,
            five_reset: None,
            seven_reset,
        }
    }

    #[test]
    fn codex_exhaustion_threshold_is_95_7d() {
        assert!(!is_exhausted::<CodexPool>(0.0, 94.9));
        assert!(is_exhausted::<CodexPool>(0.0, 95.0));
    }

    #[test]
    fn saturated_prefers_soonest_7d_reset_when_all_weekly_capped() {
        // Everyone is weekly-capped → soonest weekly reset wins. This pins the unified
        // ranking to the old Codex order (Codex has no 5h window, so the class key is
        // constant and the reset decides, exactly as before the merge).
        let candidates = [
            codex_rotation_candidate("soon@o", 97.0, Some(500_000)),
            codex_rotation_candidate("late@o", 96.0, Some(600_000)),
        ];
        let clones = [clone_host("c1", Some("late@o"))];

        let got = assign_saturated_rotation::<CodexPool>(&clones, &candidates);

        assert_eq!(got[0].1, "soon@o");
    }

    #[test]
    fn saturated_keeps_current_when_its_reset_is_close_to_best() {
        // c1 sits on soon@o, whose reset is within the sticky margin of best's — churning
        // it onto late@o would buy nothing.
        let candidates = [
            codex_rotation_candidate("soon@o", 97.0, Some(500_000)),
            codex_rotation_candidate("late@o", 96.0, Some(500_100)),
        ];
        let clones = [clone_host("c1", Some("soon@o"))];

        let got = assign_saturated_rotation::<CodexPool>(&clones, &candidates);

        assert_eq!(got[0].1, "soon@o");
    }

    #[test]
    fn auto_pool_is_only_managed_ungrouped_auto_clones() {
        let hosts = vec![
            host_sel("auto1", true, None, Some("auto")),        // in
            host_sel("pinned", true, None, Some("me@o")),       // out: pinned to an email
            host_sel("legacy", true, None, None),               // out: legacy None == pinned
            host_sel("grouped", true, Some("g"), Some("auto")), // out: named group handles it
            host_sel("stopped", false, None, Some("auto")),     // out: unmanaged
        ];
        let picked: Vec<String> = auto_pool_clones::<CodexPool>(&hosts)
            .into_iter()
            .map(|h| h.id)
            .collect();
        assert_eq!(picked, vec!["auto1"]);
    }

    fn host_sel(id: &str, managed: bool, group: Option<&str>, sel: Option<&str>) -> RmngClone {
        RmngClone {
            id: id.into(),
            managed,
            codex_group: group.map(str::to_string),
            codex_selection: sel.map(str::to_string),
            ..Default::default()
        }
    }

    #[test]
    fn to_usage_reads_reset_credits() {
        let raw: RawUsage = serde_json::from_str(
            r#"{"plan_type":"pro","rate_limit":{"secondary_window":{"used_percent":96,"limit_window_seconds":604800,"reset_at":1783392770}},"rate_limit_reset_credits":{"available_count":4}}"#,
        )
        .unwrap();
        let u = to_usage(&sample_account(), raw);
        assert_eq!(u.reset_credits, Some(4));
        assert_eq!(u.seven_day.unwrap().pct, 96.0);
        // Absent credits read as none, not zero.
        let bare: RawUsage = serde_json::from_str(r#"{"rate_limit":{}}"#).unwrap();
        assert_eq!(to_usage(&sample_account(), bare).reset_credits, None);
    }

    fn facts(id: &str, pct: f64, reset_at: i64, credits: i64) -> FleetFacts {
        FleetFacts {
            account_id: id.into(),
            seven_pct: pct,
            seven_reset_at: reset_at,
            reset_credits: credits,
        }
    }
    const DAY: i64 = 24 * 3600;

    #[test]
    fn gate_fires_picks_max_credits_when_all_capped_and_far() {
        let now = 1_000_000;
        let f = vec![
            facts("codex:a", 96.0, now + 2 * DAY, 1),
            facts("codex:b", 99.0, now + 3 * DAY, 4),
        ];
        assert_eq!(
            choose_reset_target(&f, 2, &[], now, true),
            Some("codex:b".into())
        );
    }

    #[test]
    fn gate_blocked_when_setting_off() {
        let now = 1_000_000;
        let f = vec![facts("codex:a", 99.0, now + 2 * DAY, 4)];
        assert_eq!(choose_reset_target(&f, 1, &[], now, false), None);
    }

    #[test]
    fn gate_blocked_when_any_account_below_cap() {
        let now = 1_000_000;
        let f = vec![
            facts("codex:a", 96.0, now + 2 * DAY, 4),
            facts("codex:b", 90.0, now + 2 * DAY, 4),
        ];
        assert_eq!(choose_reset_target(&f, 2, &[], now, true), None);
    }

    #[test]
    fn gate_boundary_pct_exactly_95_does_not_fire() {
        // seven_pct == SEVEN_DAY_CAP_PCT is not > the cap, so it doesn't count as capped.
        let now = 1_000_000;
        let f = vec![
            facts("codex:a", 95.0, now + 2 * DAY, 4),
            facts("codex:b", 95.0, now + 2 * DAY, 4),
        ];
        assert_eq!(choose_reset_target(&f, 2, &[], now, true), None);
    }

    #[test]
    fn gate_blocked_when_any_resets_within_24h() {
        let now = 1_000_000;
        let f = vec![
            facts("codex:a", 99.0, now + 2 * DAY, 4),
            facts("codex:b", 99.0, now + 3600, 4),
        ];
        assert_eq!(choose_reset_target(&f, 2, &[], now, true), None);
    }

    #[test]
    fn gate_boundary_headroom_exactly_24h_fires() {
        // seven_reset_at - now == RESET_MIN_HEADROOM_SECS satisfies `>=`, so the gate fires.
        let now = 1_000_000;
        let f = vec![
            facts("codex:a", 99.0, now + DAY, 4),
            facts("codex:b", 99.0, now + DAY, 2),
        ];
        assert_eq!(
            choose_reset_target(&f, 2, &[], now, true),
            Some("codex:a".into())
        );
    }

    #[test]
    fn gate_blocked_when_facts_incomplete() {
        // Only 1 of 2 accounts reported fresh usage → never fire on partial data.
        let now = 1_000_000;
        let f = vec![facts("codex:a", 99.0, now + 2 * DAY, 4)];
        assert_eq!(choose_reset_target(&f, 2, &[], now, true), None);
    }

    #[test]
    fn gate_skips_accounts_out_of_credit_or_on_cooldown() {
        let now = 1_000_000;
        let f = vec![
            facts("codex:a", 99.0, now + 2 * DAY, 0), // no credit
            facts("codex:b", 99.0, now + 2 * DAY, 2), // on cooldown this window
        ];
        let marks = vec![wire::CodexResetMark {
            account_id: "codex:b".into(),
            window_resets_at: now + 2 * DAY,
            consumed_at: 0,
            redeem_request_id: "x".into(),
        }];
        assert_eq!(choose_reset_target(&f, 2, &marks, now, true), None);
    }

    #[test]
    fn cooldown_clears_when_window_rolls() {
        let now = 1_000_000;
        let f = vec![facts("codex:b", 99.0, now + 9 * DAY, 2)]; // new window resets_at
        let marks = vec![wire::CodexResetMark {
            account_id: "codex:b".into(),
            window_resets_at: now + 2 * DAY, // stale window
            consumed_at: 0,
            redeem_request_id: "x".into(),
        }];
        assert_eq!(
            choose_reset_target(&f, 1, &marks, now, true),
            Some("codex:b".into())
        );
    }

    #[test]
    fn prune_drops_elapsed_windows() {
        let now = 1_000_000;
        let mut marks = vec![
            wire::CodexResetMark {
                account_id: "a".into(),
                window_resets_at: now - 10,
                consumed_at: 0,
                redeem_request_id: "x".into(),
            },
            wire::CodexResetMark {
                account_id: "b".into(),
                window_resets_at: now + 10,
                consumed_at: 0,
                redeem_request_id: "y".into(),
            },
        ];
        prune_marks(&mut marks, now);
        assert_eq!(marks.len(), 1);
        assert_eq!(marks[0].account_id, "b");
    }

    #[test]
    fn prune_boundary_window_equals_now_is_dropped() {
        // retain keeps `> now_secs`, so a window that resets exactly now is elapsed.
        let now = 1_000_000;
        let mut marks = vec![wire::CodexResetMark {
            account_id: "a".into(),
            window_resets_at: now,
            consumed_at: 0,
            redeem_request_id: "x".into(),
        }];
        prune_marks(&mut marks, now);
        assert!(marks.is_empty());
    }

    #[test]
    fn gate_facts_extracts_weekly_window_and_credits() {
        let raw: RawUsage = serde_json::from_str(
            r#"{"rate_limit":{"primary_window":{"used_percent":10,"limit_window_seconds":18000,"reset_at":111},"secondary_window":{"used_percent":97,"limit_window_seconds":604800,"reset_at":222}},"rate_limit_reset_credits":{"available_count":3}}"#,
        ).unwrap();
        let ff = gate_facts("codex:a", &raw).unwrap();
        assert_eq!(ff.seven_pct, 97.0);
        assert_eq!(ff.seven_reset_at, 222);
        assert_eq!(ff.reset_credits, 3);
    }

    #[test]
    fn parse_consume_outcomes() {
        assert_eq!(
            parse_consume_outcome(r#"{"code":"reset","windows_reset":2}"#),
            ConsumeOutcome::Reset
        );
        assert_eq!(
            parse_consume_outcome(r#"{"code":"noCredit"}"#),
            ConsumeOutcome::NoCredit
        );
        assert_eq!(
            parse_consume_outcome(r#"{"code":"alreadyRedeemed"}"#),
            ConsumeOutcome::AlreadyRedeemed
        );
        assert_eq!(
            parse_consume_outcome(r#"{"code":"nothingToReset"}"#),
            ConsumeOutcome::NothingToReset
        );
        assert_eq!(
            parse_consume_outcome(r#"{"code":"wat"}"#),
            ConsumeOutcome::Unknown("wat".into())
        );
        assert_eq!(
            parse_consume_outcome("not json"),
            ConsumeOutcome::Unknown(String::new())
        );
    }
}
