//! Claude accounts — usage tracking + clone assignment/swap.
//!
//! Single-token model: each account is just its short-lived OAuth pair (access +
//! single-use refresh token) in the 0600 secret store `claude-accounts.json`. The
//! control server owns the whole refresh lifecycle — nothing that can refresh ever
//! leaves it. A clone is authed by writing **only the current access token** into
//! its `~/.claude/.credentials.json` (empty refresh token, far-future expiry, so
//! Claude Code just uses whatever we last installed; see [`apply_clone_token`]).
//! Whenever a refresh rotates an account's access token,
//! [`crate::pool::push_stale_tokens_for`] fans the new token out to every clone assigned to
//! that account. The poller
//! publishes a token-free `ClaudeUsage` view onto `ControlState.claudeAccounts`.
//! Clones select an account via `"auto"` (rotated across all imported accounts by
//! [`rotate_once`]), a named group, or a pinned email.
//!
//! **Importing an account** happens at this server, by signing in to Anthropic through
//! [`crate::oauth`]. Harvesting the pair out of a clone that was already signed in is gone:
//! it needed a clone standing, Claude Code installed in it, and a second login for an
//! account the provider will hand over directly.
//!
//! Each clone gets a short-lived access token in its `~/.claude/.credentials.json` with an
//! empty refresh token, so its Claude Code can never rotate the pair this server owns. It
//! gets the account's identity with it ([`identity_json`]), because Claude Code names its
//! account to Anthropic on every request and a token swap alone leaves it naming the
//! previous one. Those writes go straight into the clone's live home (merged view),
//! addressing the clone by container name.
//!
//! The store and the refresh lifecycle are NOT here. They are one module shared with Codex
//! ([`crate::account`]), because the two sides had the same store struct with the same eleven
//! methods and Codex imported Claude's refresh vocabulary to get them. Neither are rotation,
//! token delivery, the delete/replace lifecycle, the poll and rotate loops or the swap: those
//! are [`crate::pool`], for the same reason. What stays on this side is the adapter: the
//! account struct, the refresh POST, the OAuth/usage specifics, and the two files a Claude
//! token is written into.

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use wire::{ClaudeSpend, ClaudeUsage, ClaudeUsageWindow};

use crate::account::{
    AccountKind, FETCH_TIMEOUT, RefreshFailure, RefreshRecord, Store, account_usable, fingerprint,
    refresh_status_is_fatal,
};
use crate::app::App;
use crate::clone_ops::{now_ms, snippet};
use crate::pool::{ClaudePool, push_stale_tokens, push_stale_tokens_for};

const USAGE_URL: &str = "https://api.anthropic.com/api/oauth/usage";
/// Who an access token belongs to. Same endpoint the sign-in uses (`crate::oauth`), read
/// here only to backfill an account uuid the sign-in did not record.
const PROFILE_URL: &str = "https://api.anthropic.com/api/oauth/profile";
const OAUTH_TOKEN_URL: &str = "https://platform.claude.com/v1/oauth/token";
const OAUTH_CLIENT_ID: &str = "9d1c250a-e61b-44d9-88ed-5944d1962f5e";
const OAUTH_BETA_HEADER: &str = "oauth-2025-04-20";
const USER_AGENT: &str = "claude-swap/1.0";

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StoredClaudeAccount {
    pub id: String,
    pub email: String,
    /// The account's own uuid at Anthropic, which is NOT the organization's.
    ///
    /// Claude Code names the signed-in account by this uuid in every request it makes
    /// ([`identity_json`]), so a clone cannot be told who it is without it. Accounts
    /// imported before this field existed carry an empty string until the poller fills it
    /// in from the profile endpoint.
    #[serde(default)]
    pub account_uuid: String,
    #[serde(default)]
    pub org_uuid: String,
    #[serde(default)]
    pub org_name: String,
    #[serde(default)]
    pub active: bool,
    pub access_token: String,
    pub refresh_token: String,
    #[serde(default)]
    pub expires_at: i64,
    #[serde(default)]
    pub scopes: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_refresh: Option<RefreshRecord>,
}

/// The Claude side of the shared account store: one 0600 file of [`StoredClaudeAccount`],
/// the refresh lifecycle in [`crate::account`].
pub(crate) type ClaudeStore = Store<StoredClaudeAccount>;

impl AccountKind for StoredClaudeAccount {
    const FILE: &'static str = "claude-accounts.json";
    const LABEL: &'static str = "Claude";

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
        &app.claude
    }

    /// The refresh itself. Returns the fingerprint of the token the reply carried, empty when
    /// it carried none. `before` is recomputed here rather than passed in so the trait's one
    /// provider-specific step stays a plain POST; it names the same token the record will,
    /// because nothing has mutated `acct` yet.
    async fn refresh(http: &reqwest::Client, acct: &mut Self) -> Result<String, RefreshFailure> {
        let before = fingerprint(&acct.refresh_token);
        let resp = http
            .post(OAUTH_TOKEN_URL)
            .timeout(FETCH_TIMEOUT)
            .header("Content-Type", "application/json")
            .header("User-Agent", USER_AGENT)
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
        acct.access_token = data.access_token;
        acct.expires_at = now_ms() + data.expires_in * 1000;
        let after = match data.refresh_token {
            Some(r) => {
                let after = fingerprint(&r);
                acct.refresh_token = r;
                tracing::info!(
                    "refreshed {}: rt {before} -> {after}, access token valid {}s",
                    acct.email,
                    data.expires_in
                );
                after
            }
            None => {
                tracing::error!(
                    "refreshed {}: the reply carried NO refresh_token, so the store keeps the \
                     one it just spent (rt {before}). This account fails its next refresh.",
                    acct.email
                );
                String::new()
            }
        };
        if let Some(s) = data.scope {
            acct.scopes = s.split(' ').map(str::to_string).collect();
        }
        Ok(after)
    }
}

// --- the account store ----------------------------------------------------

/// Write one account into the 0600 store, replacing whatever shared its **email**.
///
/// Accounts arrive one way: signing in to Anthropic at this server ([`crate::oauth`]).
/// Reading credentials back out of a signed-in clone was the other, and it is gone: it
/// needed a clone standing, Claude Code installed in it, and a second login.
///
/// The replace-by-email rule and the incident behind it live on [`Store::upsert`].
pub fn upsert_account(app: &App, stored: StoredClaudeAccount) -> Result<()> {
    app.claude.upsert(stored)
}

// --- token refresh + usage fetch ------------------------------------------

#[derive(Deserialize)]
struct RefreshResp {
    access_token: String,
    expires_in: i64,
    #[serde(default)]
    refresh_token: Option<String>,
    #[serde(default)]
    scope: Option<String>,
}

/// `email`'s current account, refreshed (and persisted) first if within
/// [`crate::account::refresh_lead_ms`] of expiry. Returns `(account, rotated)`.
///
/// The work runs in its own task, so a caller that goes away cannot abandon a refresh
/// half-done — see [`crate::account::refresh_and_persist`], which carries the reasoning and
/// the gate. Spawning here rather than there keeps the spawned future concrete, which is what
/// lets the lifecycle itself stay generic.
pub async fn fresh_access_token(app: &App, email: &str) -> Result<(StoredClaudeAccount, bool)> {
    let app = app.clone();
    let email = email.to_string();
    tokio::spawn(async move {
        crate::account::refresh_and_persist::<StoredClaudeAccount>(&app, &email).await
    })
    .await
    .context("the refresh task did not finish")?
}

/// Ask Anthropic which account a token belongs to, for the `accountUuid` a sign-in before
/// 2026-09-04 never captured. One call per account, once, and only for the accounts that
/// still lack it: [`identity_json`] cannot name the account without it, so those clones
/// keep declaring whoever they were bound to last.
async fn fetch_account_uuid(http: &reqwest::Client, token: &str) -> Result<String> {
    #[derive(Deserialize)]
    struct Profile {
        #[serde(default)]
        account: ProfileAccount,
    }
    #[derive(Default, Deserialize)]
    struct ProfileAccount {
        #[serde(default)]
        uuid: String,
    }
    let resp = http
        .get(PROFILE_URL)
        .timeout(FETCH_TIMEOUT)
        .header("Authorization", format!("Bearer {token}"))
        .header("Accept", "application/json")
        .header("User-Agent", USER_AGENT)
        .send()
        .await?;
    let status = resp.status();
    if !status.is_success() {
        let text = resp.text().await.unwrap_or_default();
        bail!("profile {}{}", status.as_u16(), snippet(&text));
    }
    let profile: Profile = resp.json().await?;
    if profile.account.uuid.is_empty() {
        bail!("the profile carried no account uuid");
    }
    Ok(profile.account.uuid)
}

/// Fill in `email`'s missing `account_uuid` and persist. Runs under the refresh gate so the
/// read-modify-write cannot land on top of a refresh that rotated the tokens meanwhile.
async fn backfill_account_uuid(app: &App, email: &str, token: &str) {
    let uuid = match fetch_account_uuid(&app.http, token).await {
        Ok(u) => u,
        Err(e) => {
            tracing::debug!("looking up {email}'s account uuid failed, retrying next poll: {e:#}");
            return;
        }
    };
    let _gate = app.claude.refresh_gate.lock().await;
    let Some(mut acct) = app.claude.get_by_email(email) else {
        return;
    };
    if acct.account_uuid == uuid {
        return;
    }
    acct.account_uuid = uuid;
    match app.claude.update_account(&acct) {
        Ok(()) => tracing::info!("recorded {email}'s account uuid, so its clones can name it"),
        Err(e) => tracing::warn!("persisting {email}'s account uuid failed: {e:#}"),
    }
}

// The usage API returns explicit `null` for numeric fields that don't apply (e.g.
// an account with extra-usage disabled). `#[serde(default)]` only covers a *missing*
// key, not a present `null`, so every nullable number is `Option<_>` here.
#[derive(Deserialize)]
struct RawWindow {
    #[serde(default)]
    utilization: Option<f64>,
    #[serde(default)]
    resets_at: Option<String>,
}
#[derive(Deserialize)]
struct RawExtra {
    #[serde(default)]
    is_enabled: bool,
    #[serde(default)]
    used_credits: Option<i64>,
    #[serde(default)]
    monthly_limit: Option<i64>,
    #[serde(default)]
    utilization: Option<f64>,
    #[serde(default)]
    currency: Option<String>,
    #[serde(default)]
    resets_at: Option<String>,
}
/// The model a scoped limit applies to, e.g. `{ "display_name": "Fable" }`.
#[derive(Deserialize)]
struct RawLimitModel {
    #[serde(default)]
    display_name: Option<String>,
}
#[derive(Deserialize)]
struct RawLimitScope {
    #[serde(default)]
    model: Option<RawLimitModel>,
}
/// One entry of the `limits` array. The Fable weekly cap only appears here (as a
/// `weekly_scoped` limit whose `scope.model.display_name` is "Fable") — there is no
/// top-level `fable` field, so we read it out of this list.
#[derive(Deserialize)]
struct RawLimit {
    #[serde(default)]
    percent: Option<f64>,
    #[serde(default)]
    resets_at: Option<String>,
    #[serde(default)]
    scope: Option<RawLimitScope>,
}
#[derive(Deserialize)]
struct RawUsage {
    #[serde(default)]
    five_hour: Option<RawWindow>,
    #[serde(default)]
    seven_day: Option<RawWindow>,
    #[serde(default)]
    extra_usage: Option<RawExtra>,
    #[serde(default)]
    limits: Vec<RawLimit>,
}

async fn fetch_usage(http: &reqwest::Client, token: &str) -> Result<RawUsage> {
    let resp = http
        .get(USAGE_URL)
        .timeout(FETCH_TIMEOUT)
        .header("Authorization", format!("Bearer {token}"))
        .header("anthropic-beta", OAUTH_BETA_HEADER)
        .header("User-Agent", USER_AGENT)
        .send()
        .await?;
    let status = resp.status();
    if !status.is_success() {
        let text = resp.text().await.unwrap_or_default();
        bail!("usage {}{}", status.as_u16(), snippet(&text));
    }
    Ok(resp.json().await?)
}

fn to_window(w: Option<RawWindow>) -> Option<ClaudeUsageWindow> {
    w.map(|w| ClaudeUsageWindow {
        pct: w.utilization.unwrap_or(0.0).round(),
        resets_at: w.resets_at,
    })
}

/// Pull the Fable model-scoped weekly limit out of the `limits` array, matching on the
/// scope's model display name (robust to a version suffix like "Fable 5"). `None` when
/// the account has no Fable-scoped limit.
fn fable_window(limits: &[RawLimit]) -> Option<ClaudeUsageWindow> {
    limits
        .iter()
        .find(|l| {
            l.scope
                .as_ref()
                .and_then(|s| s.model.as_ref())
                .and_then(|m| m.display_name.as_deref())
                .is_some_and(|name| name.to_ascii_lowercase().contains("fable"))
        })
        .map(|l| ClaudeUsageWindow {
            pct: l.percent.unwrap_or(0.0).round(),
            resets_at: l.resets_at.clone(),
        })
}

fn to_usage(acct: &StoredClaudeAccount, raw: RawUsage) -> ClaudeUsage {
    let fable = fable_window(&raw.limits);
    let spend = raw
        .extra_usage
        .filter(|e| e.is_enabled)
        .map(|e| ClaudeSpend {
            used_cents: e.used_credits.unwrap_or(0),
            limit_cents: e.monthly_limit,
            pct: e.utilization.unwrap_or(0.0).round(),
            currency: e.currency.unwrap_or_else(|| "USD".into()),
            resets_at: e.resets_at,
        });
    ClaudeUsage {
        id: acct.id.clone(),
        email: acct.email.clone(),
        provider: Some(wire::Provider::Claude),
        active: acct.active,
        assignable: None,
        error: None,
        stale: None,
        last_updated: now_ms(),
        five_hour: to_window(raw.five_hour),
        seven_day: to_window(raw.seven_day),
        fable,
        spend,
        reset_credits: None,
    }
}

fn claude_base(acct: &StoredClaudeAccount) -> ClaudeUsage {
    ClaudeUsage {
        id: acct.id.clone(),
        email: acct.email.clone(),
        provider: Some(wire::Provider::Claude),
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

/// Refresh-if-needed + fetch usage for every account; publish a token-free view.
/// Keeps last-good (marked `stale`) on per-account failure. Returns true on a 429.
pub async fn poll_once(app: &App) -> Result<bool> {
    // The guard, not a flag set here and cleared after the await. This function is awaited
    // inside HTTP handlers, and a handler's future is dropped when its client disconnects,
    // which skips every line after the await. See [`crate::clone_ops::PollGuard`].
    let Some(_guard) = crate::clone_ops::try_poll(&app.claude.polling) else {
        return Ok(false);
    };
    poll_inner(app).await
}

async fn poll_inner(app: &App) -> Result<bool> {
    let accts = app.claude.snapshot();
    if accts.is_empty() {
        crate::clone_ops::replace_provider_views(app, wire::Provider::Claude, Vec::new());
        return Ok(false);
    }

    let mut any429 = false;
    let mut views = Vec::with_capacity(accts.len());

    for (i, acct) in accts.iter().enumerate() {
        if i > 0 {
            tokio::time::sleep(crate::pool::STAGGER).await;
        }
        let outcome = async {
            let (fresh, rotated) = fresh_access_token(app, &acct.email).await?;
            let token = fresh.access_token;
            if rotated {
                // Before the usage fetch, not after the whole pass: this account's clones
                // are holding the token the refresh above just replaced, and the fetch can
                // burn 10s (or a 429) per remaining account before the pass ends.
                push_stale_tokens_for::<ClaudePool>(app, Some(&acct.email)).await;
            }
            let raw = fetch_usage(&app.http, &token).await?;
            // After the usage fetch, so a profile lookup that fails never costs the numbers.
            // Once it lands the account is complete and this never runs again.
            if fresh.account_uuid.is_empty() {
                backfill_account_uuid(app, &acct.email, &token).await;
            }
            Ok::<_, anyhow::Error>(to_usage(acct, raw))
        }
        .await;
        match outcome {
            Ok(mut u) => {
                // The fetch above ran on this account's own token, so the token works.
                u.assignable = Some(true);
                app.claude
                    .last_good
                    .lock()
                    .unwrap()
                    .insert(acct.id.clone(), u.clone());
                views.push(u);
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
                    .claude
                    .get_by_email(&acct.email)
                    .is_some_and(|a| account_usable(&a, now_ms()));
                let prev = app.claude.last_good.lock().unwrap().get(&acct.id).cloned();
                views.push(match prev {
                    Some(mut p) => {
                        p.stale = Some(true);
                        // Carry the reason, not just the fact. Without it a dead refresh
                        // token and a momentary 429 both read as "these numbers are old",
                        // and the one that never recovers goes unnoticed for days.
                        p.error = Some(msg);
                        p.assignable = Some(alive);
                        p
                    }
                    None => {
                        let mut b = claude_base(acct);
                        b.error = Some(msg);
                        b.assignable = Some(alive);
                        b
                    }
                });
            }
        }
    }

    crate::clone_ops::replace_provider_views(app, wire::Provider::Claude, views);

    // Rotations were fanned out per account as they happened; this sweep only retries
    // pushes that failed and catches clones reassigned during the pass.
    push_stale_tokens::<ClaudePool>(app).await;

    Ok(any429)
}

/// One rotation pass over every named group plus the implicit "auto" pool (all imported
/// accounts, recomputed live). Sticky: a clone moves only when its account exhausts or
/// leaves its pool. The pass itself lives in [`crate::pool`]; this entry point keeps the
/// call sites (swap, delete, refresh) on the provider they mean.
pub async fn rotate_once(app: &App) {
    crate::pool::rotate_once::<ClaudePool>(app).await
}

/// Self-scheduling rotation loop for the Claude side. The loop is
/// [`crate::pool::run_rotator`]; this entry point is what `main.rs` spawns.
pub async fn run_rotator(app: App) {
    crate::pool::run_rotator::<ClaudePool>(app).await
}

/// The `~/.claude/.credentials.json` body that runs Claude Code under `token` (the
/// account's current short-lived access token). The refresh token is left **empty**
/// and the expiry far-future so the clone's Claude Code never tries to rotate or
/// abandon the token — it just uses whatever the server last installed; the server
/// pushes a replacement on every refresh ([`crate::pool::push_stale_tokens`]).
fn credentials_json(token: &str) -> String {
    format!(
        r#"{{"claudeAiOauth":{{"accessToken":"{token}","refreshToken":"","expiresAt":4102444800000,"scopes":["user:inference","user:profile"],"subscriptionType":"max"}}}}"#
    )
}

/// A stable 64-hex machine identity for `acct`, different for every account.
///
/// Anthropic sees this value with every request Claude Code makes, so one value shared by
/// the whole fleet reads as a single machine running dozens of sessions across a dozen
/// accounts. Measured on 2026-09-04: all 24 readable clone homes on CT 105 and CT 106 held
/// the identical `userID`, the one baked into the template image. Deriving it from the
/// account instead makes one account look like one machine, which is what
/// [CLIProxyAPI](https://github.com/router-for-me/CLIProxyAPI) settled on as well (its
/// device pool canonicalizes to exactly one device per credential).
///
/// `field` separates the two identifiers Claude Code keeps, so `userID` and `machineID`
/// never collide.
fn device_id(acct: &StoredClaudeAccount, field: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(b"rmng-claude-identity/v1|");
    h.update(field.as_bytes());
    h.update(b"|");
    // The uuid, not the email: an account re-signed-in under the same address keeps its
    // machine rather than appearing to move to a new one.
    h.update(acct.account_uuid.as_bytes());
    format!("{:x}", h.finalize())
}

/// What clone `host_id` should say about itself once it runs `acct`'s token: the account
/// identity Claude Code sends to Anthropic, plus this account's machine identity.
///
/// `None` when the account uuid is unknown, because a clone that names the wrong account is
/// exactly the problem this fixes. The poller backfills the uuid within one pass
/// ([`backfill_account_uuid`]), and until then the clone keeps whatever it had.
///
/// The guest merges these three keys into `~/.claude.json` and leaves its other 50 alone.
/// Inside `oauthAccount` it keeps the sibling fields (billing type, rate-limit tier) when
/// the account is unchanged and drops them when it is not, since none of them describe the
/// new account.
fn identity_json(acct: &StoredClaudeAccount) -> Option<String> {
    if acct.account_uuid.is_empty() {
        return None;
    }
    let mut account = serde_json::Map::new();
    account.insert("accountUuid".into(), acct.account_uuid.clone().into());
    account.insert("emailAddress".into(), acct.email.clone().into());
    // Only when we have one. An account can reach the store with no organization recorded,
    // and naming an empty one would write that emptiness over a clone's correct value.
    // Claude Code refills what it is not told.
    if !acct.org_uuid.is_empty() {
        account.insert("organizationUuid".into(), acct.org_uuid.clone().into());
        account.insert("organizationName".into(), acct.org_name.clone().into());
    }
    let body = serde_json::json!({
        "userID": device_id(acct, "user"),
        "machineID": device_id(acct, "machine"),
        "oauthAccount": account,
    });
    Some(body.to_string())
}

/// Outcome of merging an account identity into a clone's `~/.claude.json` body.
/// Pure Rust port of the old guest-side python merge: that file belongs to Claude Code
/// (project history lives in it), so only the three identity keys are touched and a
/// file that does not parse is left alone — losing history to repair identity is the
/// worse trade.
enum IdentityMerge {
    /// Already names this account: no upload.
    Current,
    /// Merged body to upload.
    Updated(String),
    /// Left untouched, with the reason (warn, do not fail the push).
    Skipped(String),
}

fn merge_claude_identity(current: Option<&[u8]>, patch: &serde_json::Value) -> IdentityMerge {
    let mut cur = serde_json::Map::new();
    if let Some(bytes) = current {
        let raw = String::from_utf8_lossy(bytes);
        if !raw.trim().is_empty() {
            match serde_json::from_str::<serde_json::Value>(&raw) {
                Ok(serde_json::Value::Object(map)) => cur = map,
                _ => {
                    return IdentityMerge::Skipped(
                        "~/.claude.json is not a JSON object".to_string(),
                    );
                }
            }
        }
    }
    let Some(want) = patch.get("oauthAccount").and_then(|v| v.as_object()) else {
        return IdentityMerge::Skipped("identity patch has no oauthAccount".to_string());
    };
    let same_account = cur
        .get("oauthAccount")
        .and_then(|v| v.as_object())
        .and_then(|have| have.get("accountUuid"))
        == want.get("accountUuid");
    let block = if same_account {
        // The same account: billing, seat and rate-limit siblings still describe it.
        let mut block = cur
            .get("oauthAccount")
            .and_then(|v| v.as_object())
            .cloned()
            .unwrap_or_default();
        let before = block.clone();
        for (k, v) in want {
            block.insert(k.clone(), v.clone());
        }
        if block != before {
            // Something owned moved: the rest of the block profiles the old state.
            // Claude Code refills it on the next account lookup.
            block.remove("profileFetchedAt");
        }
        block
    } else {
        // A different account: nothing the old block said carries over.
        want.clone()
    };
    // `want` is only cloned above; no borrow survives into the inserts below.
    cur.insert(
        "userID".to_string(),
        patch
            .get("userID")
            .cloned()
            .unwrap_or(serde_json::Value::Null),
    );
    cur.insert(
        "machineID".to_string(),
        patch
            .get("machineID")
            .cloned()
            .unwrap_or(serde_json::Value::Null),
    );
    cur.insert("oauthAccount".to_string(), serde_json::Value::Object(block));
    let merged = serde_json::Value::Object(cur);
    let before = match current {
        Some(bytes) => serde_json::from_slice::<serde_json::Value>(bytes).ok(),
        None => None,
    };
    if before.as_ref() == Some(&merged) {
        IdentityMerge::Current
    } else {
        IdentityMerge::Updated(merged.to_string())
    }
}

/// This side's [`crate::pool::push_key_of`]: the access token that was installed, plus the
/// identity that was installed with it — the whole `~/.claude.json` patch, since every key
/// in it is delivered. A clone whose identity changed is stale even when its token did not.
pub(crate) fn push_key(acct: &StoredClaudeAccount) -> String {
    crate::pool::push_key_of(
        &acct.access_token,
        identity_json(acct).as_deref().unwrap_or(""),
    )
}

/// Install `acct`'s access token AND its identity into clone `host_id` — direct file
/// writes straight into the clone's live home, no guest shell and no daemon roundtrip. Hot-swaps
/// a running clone with **no** agent-wrapper restart, because Claude Code re-reads both
/// files at request time.
/// Best-effort; errors are returned to log. Low-level: callers that target an assigned host
/// should go through [`crate::pool::push_account_to_clone`] / [`crate::pool::push_stale_tokens`]
/// so the push is recorded.
///
/// The token alone used to be the whole push, which left `~/.claude.json` naming whoever the
/// clone ran before. Measured on CT 105 on 2026-09-04: 11 of 14 readable clones declared an
/// account that was not the one their token belonged to. See [`identity_json`].
pub async fn apply_clone_token(
    _app: &App,
    host_id: &str,
    acct: &StoredClaudeAccount,
) -> Result<()> {
    let token = acct.access_token.trim();
    if !token.starts_with("sk-ant-") {
        bail!("refusing to apply a non-`sk-ant-` token");
    }
    // The credentials file is server-owned wholesale: overwrite, never merge.
    crate::home_overlay::write_clone_home(
        host_id,
        ".claude/.credentials.json",
        credentials_json(token).as_bytes(),
        0o600,
    )
    .with_context(|| format!("{host_id}: writing Claude credentials"))?;
    // The identity is a separate outcome from the token. A clone that took the token and
    // refused the identity still works, so this is a warning rather than a failed push.
    if let Some(patch_str) = identity_json(acct) {
        let patch: serde_json::Value = serde_json::from_str(&patch_str)
            .with_context(|| format!("{host_id}: identity patch is not JSON"))?;
        let current = crate::home_overlay::read_clone_home(host_id, ".claude.json")
            .with_context(|| format!("{host_id}: reading ~/.claude.json"))?;
        match merge_claude_identity(current.as_deref(), &patch) {
            IdentityMerge::Current => {}
            IdentityMerge::Updated(body) => {
                crate::home_overlay::write_clone_home(
                    host_id,
                    ".claude.json",
                    body.as_bytes(),
                    0o600,
                )
                .with_context(|| format!("{host_id}: writing ~/.claude.json identity"))?;
            }
            IdentityMerge::Skipped(reason) => {
                tracing::warn!(
                    "{host_id} kept {}'s token but not its identity: {}",
                    acct.email,
                    reason
                );
            }
        }
    }
    Ok(())
}

/// Remove clone `host_id`'s `~/.claude/.credentials.json`, leaving it
/// with no Claude token. Used when a clone's account is set to "none" (unassigned) —
/// callers should also [`Store::forget_pushed`] the host.
pub async fn clear_clone_token(_app: &App, host_id: &str) -> Result<()> {
    crate::home_overlay::remove_clone_home(host_id, ".claude/.credentials.json")
}

/// Self-scheduling usage-poll loop for the Claude side. The loop is
/// [`crate::pool::run_poller`]; this entry point is what `main.rs` spawns, and it names the
/// poll interval this side answers to.
pub async fn run_poller(app: App) {
    crate::pool::run_poller::<ClaudePool>(app, wire::CLAUDE_POLL_SECS).await
}

#[cfg(test)]
mod tests {
    use super::*;

    // The refresh-phase rules these tests pin now live in `crate::account`; both providers
    // assert them, and a later pass consolidates the pair.
    use crate::account::{
        REFRESH_LEAD_MS, REFRESH_SPREAD_MS, is_expired, refresh_lead_ms, token_alive,
    };
    // What is left here is what needs a Claude account to assert: the store, the identity a
    // clone declares, and Claude's own scoring constants. The provider-neutral rotation
    // rules are asserted once, in `crate::pool`.
    use crate::pool::{
        Assignment, delete_account, eligible_members, is_exhausted, pick_group_account,
        replace_account, repoint_clones, resolve_assignment,
    };
    use wire::{CloneGroup, RmngClone};

    // The exact shapes Claude Code v2 emits — `claude auth status` (camelCase JSON)
    // and `~/.claude/.credentials.json` (camelCase, nested under `claudeAiOauth`).

    // The bug this guard exists for: re-importing a clone the server already pushed a token
    // to stored a blank refresh token beside a year-2100 expiry, so the account was never
    // refreshed again and handed out a dead access token until somebody noticed the 401s.

    #[test]
    fn parses_usage_with_null_extra_fields() {
        // The real /oauth/usage response: windows carry numbers, but `extra_usage`
        // (disabled here) comes back with explicit null numerics. Must still decode.
        let body = r#"{
            "five_hour": {"utilization": 7.0, "resets_at": "2026-06-30T19:10:00Z"},
            "seven_day": {"utilization": 2.0, "resets_at": "2026-07-05T10:00:00Z"},
            "extra_usage": {"is_enabled": false, "monthly_limit": null,
                            "used_credits": null, "utilization": null}
        }"#;
        let raw: RawUsage = serde_json::from_str(body).unwrap();
        let acct = StoredClaudeAccount {
            id: "a@b|o".into(),
            email: "a@b".into(),
            account_uuid: "acct-uuid".into(),
            org_uuid: "o".into(),
            org_name: String::new(),
            active: false,
            access_token: String::new(),
            refresh_token: String::new(),
            expires_at: 0,
            scopes: vec![],
            last_refresh: None,
        };
        let u = to_usage(&acct, raw);
        assert_eq!(u.five_hour.unwrap().pct, 7.0);
        assert_eq!(u.seven_day.unwrap().pct, 2.0);
        assert!(u.fable.is_none()); // no `limits` array → no fable window
        assert!(u.spend.is_none()); // extra usage disabled → no spend line
    }

    #[test]
    fn parses_fable_from_scoped_limits() {
        // The real /oauth/usage response carries the model-scoped Fable weekly cap only
        // inside `limits` (as a `weekly_scoped` entry) — never as a top-level field. Its
        // `percent` is a bare integer and `resets_at` an offset timestamp with fractional
        // seconds. The unscoped `weekly_all` entry must NOT be mistaken for it.
        let body = r#"{
            "five_hour": {"utilization": 23.0, "resets_at": "2026-07-20T01:20:00.469592+00:00"},
            "seven_day": {"utilization": 61.0, "resets_at": "2026-07-24T22:00:00.469612+00:00"},
            "limits": [
                {"kind": "weekly_all", "percent": 61, "resets_at": "2026-07-24T22:00:00.469612+00:00", "scope": null},
                {"kind": "weekly_scoped", "percent": 8, "resets_at": "2026-07-24T22:00:00.469890+00:00",
                 "scope": {"model": {"id": null, "display_name": "Fable"}, "surface": null}}
            ]
        }"#;
        let raw: RawUsage = serde_json::from_str(body).unwrap();
        let acct = StoredClaudeAccount {
            id: "a@b|o".into(),
            email: "a@b".into(),
            account_uuid: "acct-uuid".into(),
            org_uuid: "o".into(),
            org_name: String::new(),
            active: false,
            access_token: String::new(),
            refresh_token: String::new(),
            expires_at: 0,
            scopes: vec![],
            last_refresh: None,
        };
        let fable = to_usage(&acct, raw).fable.expect("fable window present");
        assert_eq!(fable.pct, 8.0);
        assert_eq!(
            fable.resets_at.as_deref(),
            Some("2026-07-24T22:00:00.469890+00:00")
        );
    }

    #[test]
    fn credentials_json_uses_access_token_with_empty_refresh() {
        let j = credentials_json("sk-ant-oat01-XYZ");
        assert!(j.contains(r#""accessToken":"sk-ant-oat01-XYZ""#));
        assert!(j.contains(r#""refreshToken":"""#));
        // parses as the same shape Claude Code writes
        let v: serde_json::Value = serde_json::from_str(&j).unwrap();
        assert_eq!(v["claudeAiOauth"]["accessToken"], "sk-ant-oat01-XYZ");
        assert_eq!(v["claudeAiOauth"]["refreshToken"], "");
    }

    /// Ten emails standing in for a batch of accounts imported in one sitting.
    const FLEET: [&str; 10] = [
        "a@one.test",
        "b@one.test",
        "c@one.test",
        "d@two.test",
        "e@two.test",
        "f@two.test",
        "g@three.test",
        "h@three.test",
        "i@three.test",
        "j@three.test",
    ];

    /// The offset may only ever ADD lead. An account refreshed later than the floor
    /// would lose the safety margin the floor exists to guarantee.
    #[test]
    fn jittered_lead_never_drops_below_the_floor() {
        for email in FLEET {
            let lead = refresh_lead_ms(email);
            assert!(
                lead >= REFRESH_LEAD_MS,
                "{email}: lead {lead} is under the floor"
            );
            assert!(
                lead < REFRESH_LEAD_MS + REFRESH_SPREAD_MS,
                "{email}: lead {lead} exceeds floor + spread"
            );
        }
    }

    /// A restart must not re-phase an account, or its expiry would drift each time.
    #[test]
    fn jittered_lead_is_stable_for_one_email() {
        for email in FLEET {
            assert_eq!(refresh_lead_ms(email), refresh_lead_ms(email));
        }
        // ...and is actually derived from the email, not a constant.
        assert_ne!(refresh_lead_ms("a@one.test"), refresh_lead_ms("b@one.test"));
    }

    /// The point of the offset: accounts that expire in the same second must not come
    /// due in the same second. Ten emails over six 15-minute buckets.
    #[test]
    fn jitter_spreads_a_batch_of_accounts_across_the_window() {
        let buckets: std::collections::HashSet<i64> = FLEET
            .iter()
            .map(|e| refresh_lead_ms(e) / (15 * 60 * 1000))
            .collect();
        assert!(
            buckets.len() >= 4,
            "10 accounts landed in only {} of 6 buckets",
            buckets.len()
        );
    }

    #[test]
    fn expiry_check_uses_the_accounts_own_lead() {
        let email = "a@one.test";
        let lead = refresh_lead_ms(email);
        let now = now_ms();
        // Inside this account's lead ⇒ due for refresh.
        assert!(is_expired(email, now + lead - 60_000));
        // Outside it ⇒ not yet, even though it is inside another account's longer lead.
        assert!(!is_expired(email, now + lead + 5 * 60_000));
    }

    fn stored(email: &str) -> StoredClaudeAccount {
        StoredClaudeAccount {
            id: email.into(),
            email: email.into(),
            account_uuid: format!("uuid-of-{email}"),
            org_uuid: String::new(),
            org_name: String::new(),
            active: true,
            access_token: "sk-ant-oat01-x".into(),
            refresh_token: String::new(),
            // Far-future so `fresh_access_token` never attempts a (network) refresh in tests.
            expires_at: 4_102_444_800_000,
            scopes: Vec::new(),
            last_refresh: None,
        }
    }

    /// An App over a temp data dir with a `team` group of the given imported accounts.
    fn app_with_group(members: &[&str]) -> App {
        use std::sync::Arc;
        let dir = std::env::temp_dir().join(format!(
            "rmng-claude-sticky-{}-{}",
            std::process::id(),
            crate::clone_ops::rand_u64()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let store = Arc::new(crate::state::StateStore::load(dir.join("state.json")).unwrap());
        let cfg = wire::AppConfig {
            groups: vec![CloneGroup {
                name: "team".into(),
                accounts: members.iter().map(|s| s.to_string()).collect(),
            }],
            ..Default::default()
        };
        let app = App::new(store, cfg, &dir.to_string_lossy());
        for m in members {
            app.claude.update_account(&stored(m)).unwrap();
        }
        app
    }

    #[test]
    fn group_swap_keeps_current_account_when_it_is_an_eligible_member() {
        // Switching a clone pinned to b@x into group:team must KEEP b@x — it's already a
        // member, so rebalancing to a@x would cold-start the prompt cache for nothing.
        // Repeated: a non-sticky pick would randomize across {a@x, b@x} and flake.
        let app = app_with_group(&["a@x", "b@x"]);
        for _ in 0..25 {
            match resolve_assignment::<ClaudePool>(&app, Some("group:team"), Some("b@x"), None) {
                Some(Assignment::Group { name, initial }) => {
                    assert_eq!(name, "team");
                    assert_eq!(initial, "b@x", "must keep the current member on group swap");
                }
                _ => panic!("expected a group assignment"),
            }
        }
    }

    #[test]
    fn group_swap_picks_a_member_when_current_is_outside_or_absent() {
        // A current account not in the group (or no incumbent) → a real group member.
        let app = app_with_group(&["a@x", "b@x"]);
        for current in [Some("z@outside"), None] {
            match resolve_assignment::<ClaudePool>(&app, Some("group:team"), current, None) {
                Some(Assignment::Group { initial, .. }) => {
                    assert!(
                        matches!(initial.as_str(), "a@x" | "b@x"),
                        "picked non-member {initial}"
                    );
                }
                _ => panic!("expected a group assignment"),
            }
        }
    }

    fn auto_clone(id: &str, account: &str) -> RmngClone {
        RmngClone {
            id: id.into(),
            managed: true,
            claude_account_email: Some(account.into()),
            claude_selection: Some(crate::pool::AUTO.into()),
            ..Default::default()
        }
    }

    /// Measured on CT 105 on 2026-09-03: three emails held two records each, the dead one
    /// first, so `get_by_email` handed every caller the rejected grant of an account the
    /// operator had just signed in. A sign-in must land ON the old record, whatever its id.
    #[test]
    fn a_second_sign_in_replaces_the_record_even_when_the_id_changed() {
        let app = app_with_group(&["a@x"]);
        let mut again = stored("a@x");
        // What the org uuid moving (or arriving for the first time) does to the id.
        again.id = "a@x|org-uuid".into();
        again.access_token = "sk-ant-oat01-new".into();
        upsert_account(&app, again).unwrap();

        let held: Vec<_> = app
            .claude
            .snapshot()
            .into_iter()
            .filter(|a| a.email == "a@x")
            .collect();
        assert_eq!(
            held.len(),
            1,
            "one email, one record: {:?}",
            held.iter().map(|a| &a.id).collect::<Vec<_>>()
        );
        assert_eq!(
            app.claude.get_by_email("a@x").unwrap().access_token,
            "sk-ant-oat01-new"
        );
    }

    #[test]
    fn store_delete_removes_only_the_named_account() {
        let app = app_with_group(&["a@x", "b@x"]);
        assert!(app.claude.delete("a@x").unwrap(), "present → removed");
        assert!(!app.claude.emails().contains(&"a@x".to_string()));
        assert!(app.claude.emails().contains(&"b@x".to_string()));
        assert!(!app.claude.delete("a@x").unwrap(), "already gone → false");
    }

    /// The provider has already said this grant is dead, so the only thing another POST can
    /// do is tell Anthropic that this address keeps presenting a rejected refresh token. The
    /// account comes back through a sign-in, which clears the record.
    #[tokio::test]
    async fn a_rejected_grant_is_never_posted_to_the_provider_again() {
        let app = app_with_group(&["a@x"]);
        let mut acct = app.claude.get_by_email("a@x").unwrap();
        acct.expires_at = 0; // long expired, so a refresh is due
        acct.last_refresh = Some(RefreshRecord {
            at: 1,
            ok: false,
            rt_before: "beef".into(),
            rt_after: String::new(),
            error: Some("refresh 400: invalid_grant".into()),
            rejected: true,
        });
        app.claude.update_account(&acct).unwrap();

        let err = fresh_access_token(&app, "a@x")
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains("signed in again"), "message: {err}");
        assert!(
            err.contains("invalid_grant"),
            "the reason travels with it: {err}"
        );
        // No HTTP call happened, so the record is exactly the one the test wrote.
        let after = app.claude.get_by_email("a@x").unwrap();
        assert_eq!(after.last_refresh.unwrap().rt_before, "beef");
    }

    /// A clone declares its account to Anthropic on every request. Handing it a token
    /// without the matching identity is what left 11 of 14 clones on CT 105 naming an
    /// account they were not running.
    #[test]
    fn the_identity_names_the_account_whose_token_is_installed() {
        let acct = stored("a@x");
        let body: serde_json::Value =
            serde_json::from_str(&identity_json(&acct).expect("uuid present")).unwrap();
        assert_eq!(body["oauthAccount"]["emailAddress"], "a@x");
        assert_eq!(body["oauthAccount"]["accountUuid"], "uuid-of-a@x");
        assert_eq!(body["userID"].as_str().unwrap().len(), 64);
        assert_ne!(
            body["userID"], body["machineID"],
            "two identifiers, not one"
        );
    }

    #[test]
    fn identity_merge_seeds_a_missing_file() {
        let acct = stored("a@x");
        let patch: serde_json::Value =
            serde_json::from_str(&identity_json(&acct).unwrap()).unwrap();
        match merge_claude_identity(None, &patch) {
            IdentityMerge::Updated(body) => {
                let v: serde_json::Value = serde_json::from_str(&body).unwrap();
                assert_eq!(v["oauthAccount"]["emailAddress"], "a@x");
                assert_eq!(v["userID"], patch["userID"]);
            }
            other => panic!("expected Updated, got {}", merge_name(&other)),
        }
    }

    #[test]
    fn identity_merge_keeps_siblings_for_the_same_account() {
        let acct = stored("a@x");
        let patch: serde_json::Value =
            serde_json::from_str(&identity_json(&acct).unwrap()).unwrap();
        let current = serde_json::json!({
            "userID": "old-user", "machineID": "old-machine",
            "oauthAccount": { "accountUuid": "uuid-of-a@x", "emailAddress": "a@x",
                               "rateLimitTier": "max20", "profileFetchedAt": 123 },
            "projects": {"/x": {}}
        });
        match merge_claude_identity(Some(current.to_string().as_bytes()), &patch) {
            IdentityMerge::Updated(body) => {
                let v: serde_json::Value = serde_json::from_str(&body).unwrap();
                // Same account: the sibling survives, identity refreshes, history untouched.
                assert_eq!(v["oauthAccount"]["rateLimitTier"], "max20");
                assert_eq!(v["userID"], patch["userID"]);
                assert_eq!(v["projects"], serde_json::json!({"/x": {}}));
            }
            other => panic!("expected Updated, got {}", merge_name(&other)),
        }
    }

    #[test]
    fn identity_merge_replaces_the_block_for_a_different_account() {
        let acct = stored("a@x");
        let patch: serde_json::Value =
            serde_json::from_str(&identity_json(&acct).unwrap()).unwrap();
        let current = serde_json::json!({
            "oauthAccount": { "accountUuid": "other", "emailAddress": "b@y",
                               "rateLimitTier": "max20" }
        });
        match merge_claude_identity(Some(current.to_string().as_bytes()), &patch) {
            IdentityMerge::Updated(body) => {
                let v: serde_json::Value = serde_json::from_str(&body).unwrap();
                assert_eq!(v["oauthAccount"]["accountUuid"], "uuid-of-a@x");
                assert!(v["oauthAccount"].get("rateLimitTier").is_none());
            }
            other => panic!("expected Updated, got {}", merge_name(&other)),
        }
    }

    #[test]
    fn identity_merge_leaves_unparseable_files_alone() {
        let acct = stored("a@x");
        let patch: serde_json::Value =
            serde_json::from_str(&identity_json(&acct).unwrap()).unwrap();
        match merge_claude_identity(Some(b"{broken".as_slice()), &patch) {
            IdentityMerge::Skipped(_) => {}
            other => panic!("expected Skipped, got {}", merge_name(&other)),
        }
    }

    #[test]
    fn identity_merge_reports_current_when_nothing_changes() {
        let acct = stored("a@x");
        let patch: serde_json::Value =
            serde_json::from_str(&identity_json(&acct).unwrap()).unwrap();
        let mut current = serde_json::Map::new();
        current.insert("userID".into(), patch["userID"].clone());
        current.insert("machineID".into(), patch["machineID"].clone());
        current.insert("oauthAccount".into(), patch["oauthAccount"].clone());
        let raw = serde_json::Value::Object(current).to_string();
        match merge_claude_identity(Some(raw.as_bytes()), &patch) {
            IdentityMerge::Current => {}
            other => panic!("expected Current, got {}", merge_name(&other)),
        }
    }

    fn merge_name(m: &IdentityMerge) -> &'static str {
        match m {
            IdentityMerge::Current => "Current",
            IdentityMerge::Updated(_) => "Updated",
            IdentityMerge::Skipped(_) => "Skipped",
        }
    }

    /// would write that emptiness over whatever the clone already had, so those two keys are
    /// left out instead and the clone keeps its own.
    #[test]
    fn an_account_with_no_organization_names_only_itself() {
        let mut acct = stored("a@x");
        acct.org_uuid = String::new();
        acct.org_name = String::new();
        let body: serde_json::Value =
            serde_json::from_str(&identity_json(&acct).expect("uuid present")).unwrap();
        let account = body["oauthAccount"].as_object().unwrap();
        assert_eq!(account["accountUuid"], "uuid-of-a@x");
        assert!(
            !account.contains_key("organizationUuid"),
            "no empty org: {account:?}"
        );
        assert!(!account.contains_key("organizationName"));
    }

    #[test]
    fn one_account_is_one_machine_and_two_accounts_are_two() {
        let (a, b) = (stored("a@x"), stored("b@x"));
        assert_ne!(device_id(&a, "user"), device_id(&b, "user"));
        // Stable across calls, or every push would rewrite the clone's config.
        assert_eq!(device_id(&a, "user"), device_id(&stored("a@x"), "user"));
    }

    /// A rebind can hand a clone an account whose token was already pushed somewhere else.
    /// Comparing tokens alone called that clone current while it still named its old
    /// account. The key's shape is `crate::pool`'s; what this pins is that Claude puts its
    /// account identity in it.
    #[test]
    fn a_clone_is_stale_when_its_identity_changed_even_if_the_token_did_not() {
        let a = stored("a@x");
        let mut b = stored("b@x");
        b.access_token = a.access_token.clone();
        assert_ne!(push_key(&a), push_key(&b));
    }

    #[tokio::test]
    async fn delete_account_refuses_when_a_clone_is_pinned() {
        let app = app_with_group(&["a@x", "b@x"]);
        app.store.mutate(|s| {
            s.hosts.push(RmngClone {
                id: "c1".into(),
                managed: true,
                claude_account_email: Some("a@x".into()),
                claude_selection: Some("a@x".into()), // pinned
                ..Default::default()
            })
        });
        let err = delete_account::<ClaudePool>(&app, "a@x").await.unwrap_err();
        assert!(err.to_string().contains("pinned"), "message: {err}");
        // Aborted before touching the store — the account is untouched.
        assert!(app.claude.emails().contains(&"a@x".to_string()));
    }

    #[tokio::test]
    async fn delete_account_removes_token_and_detaches_unpinned_clones() {
        let app = app_with_group(&["a@x", "b@x"]);
        app.store.mutate(|s| s.hosts.push(auto_clone("c1", "a@x")));
        let moved = delete_account::<ClaudePool>(&app, "a@x").await.unwrap();
        assert_eq!(moved, vec!["c1".to_string()]);
        assert!(
            !app.claude.emails().contains(&"a@x".to_string()),
            "token deleted"
        );
        // The clone no longer points at the deleted account. The re-placement runs in the
        // background, so what this call guarantees is only the detach.
        let c1 = app
            .store
            .get()
            .hosts
            .into_iter()
            .find(|h| h.id == "c1")
            .unwrap();
        assert_ne!(c1.claude_account_email.as_deref(), Some("a@x"));
    }

    /// The delete is what the operator watches, so everything it decides has to be visible by
    /// the time it returns. The row used to leave `claude_accounts` only when the NEXT usage
    /// poll rebuilt that list — a walk of every remaining account at a 400ms stagger with a
    /// 10s timeout each — so a deleted account sat on screen looking like a failed delete.
    #[tokio::test]
    async fn delete_account_publishes_the_removal_before_it_returns() {
        let app = app_with_group(&["a@x", "b@x"]);
        app.store.mutate(|s| {
            s.claude_accounts = vec![
                usage_row("a@x", wire::Provider::Claude),
                usage_row("b@x", wire::Provider::Claude),
                // Same email under the other provider: a separate account that must survive.
                usage_row("a@x", wire::Provider::Codex),
            ];
        });
        delete_account::<ClaudePool>(&app, "a@x").await.unwrap();

        let rows = app.store.get().claude_accounts;
        assert!(
            !rows
                .iter()
                .any(|u| u.email == "a@x" && u.provider != Some(wire::Provider::Codex)),
            "the deleted Claude row is still published: {rows:?}"
        );
        assert!(
            rows.iter().any(|u| u.email == "b@x"),
            "an untouched account was dropped"
        );
        assert!(
            rows.iter()
                .any(|u| u.email == "a@x" && u.provider == Some(wire::Provider::Codex)),
            "the Codex account sharing the email was dropped with it"
        );
    }

    fn usage_row(email: &str, provider: wire::Provider) -> ClaudeUsage {
        ClaudeUsage {
            id: format!("{email}|{provider:?}"),
            email: email.into(),
            provider: Some(provider),
            active: false,
            assignable: Some(true),
            error: None,
            stale: None,
            last_updated: 0,
            five_hour: None,
            seven_day: None,
            fable: None,
            spend: None,
            reset_credits: None,
        }
    }

    /// A refresh chain Anthropic has rejected cannot mint another token, so the account is
    /// dead the moment it is told so — not when its last access token happens to run out,
    /// which is up to `REFRESH_LEAD_MS` plus the account's own offset later. Every clone on
    /// it is getting 401s throughout that window.
    #[test]
    fn a_rejected_grant_takes_an_account_out_while_its_token_is_still_in_date() {
        let app = app_with_group(&["live@x", "revoked@x"]);
        let mut acct = app.claude.get_by_email("revoked@x").unwrap();
        assert!(
            token_alive(acct.expires_at, now_ms()),
            "the fixture token is in date"
        );
        acct.last_refresh = Some(RefreshRecord {
            at: now_ms(),
            ok: false,
            rt_before: "aaaa".into(),
            rt_after: String::new(),
            error: Some(r#"refresh 400 (rt aaaa): {"error": "invalid_grant"}"#.into()),
            rejected: true,
        });
        app.claude.update_account(&acct).unwrap();

        assert_eq!(app.claude.usable_emails(), vec!["live@x".to_string()]);
        assert_eq!(
            pick_group_account::<ClaudePool>(&app, "team", Some("revoked@x")).unwrap(),
            "live@x"
        );
    }

    /// The other half of the same rule. A 429, a timeout or a 5xx says nothing about the
    /// grant, and evicting on one would rotate the whole fleet off a healthy account every
    /// time Anthropic had a bad minute.
    #[test]
    fn a_transient_refresh_failure_leaves_the_account_in_the_rotation() {
        let app = app_with_group(&["live@x", "flaky@x"]);
        let mut acct = app.claude.get_by_email("flaky@x").unwrap();
        acct.last_refresh = Some(RefreshRecord {
            at: now_ms(),
            ok: false,
            rt_before: "aaaa".into(),
            rt_after: String::new(),
            error: Some("refresh 429 (rt aaaa)".into()),
            rejected: false,
        });
        app.claude.update_account(&acct).unwrap();

        let mut usable = app.claude.usable_emails();
        usable.sort();
        assert_eq!(usable, vec!["flaky@x".to_string(), "live@x".to_string()]);
    }

    #[test]
    fn only_a_rejection_from_the_provider_is_fatal() {
        assert!(
            refresh_status_is_fatal(400),
            "invalid_grant arrives as a 400"
        );
        assert!(refresh_status_is_fatal(401));
        for retryable in [429, 500, 502, 503] {
            assert!(
                !refresh_status_is_fatal(retryable),
                "{retryable} must be retryable"
            );
        }
    }

    /// The bug this whole pass exists for. An archived clone's container is stopped, so the
    /// token push into it can never succeed — and the binding used to be written only on a
    /// successful push. Measured on CT 105 as twelve clones frozen on three `invalid_grant`
    /// accounts, the rotator picking a correct replacement and discarding it every ten
    /// minutes for as long as the log went back.
    #[tokio::test]
    async fn an_archived_clone_is_rebound_off_a_dead_account_without_a_push() {
        let app = app_with_group(&["live@x", "dead@x"]);
        kill_token(&app, "dead@x");
        app.store.mutate(|s| {
            s.hosts.push(RmngClone {
                id: "archived-1".into(),
                managed: true,
                archived: true,
                claude_account_email: Some("dead@x".into()),
                claude_selection: Some("group:team".into()),
                claude_group: Some("team".into()),
                ..Default::default()
            })
        });

        // No docker in the test, so a pass that still tried to push would fail and leave the
        // binding where it was. Reaching `live@x` IS the assertion that it no longer tries.
        rotate_once(&app).await;

        let host = app
            .store
            .get()
            .hosts
            .into_iter()
            .find(|h| h.id == "archived-1")
            .unwrap();
        assert_eq!(host.claude_account_email.as_deref(), Some("live@x"));
    }

    /// A replacement takes over the two things an operator would otherwise have to rebuild
    /// from memory: which clones named the old account, and which ran it.
    #[tokio::test]
    async fn replacing_an_account_moves_its_pins_and_its_clones_then_deletes_it() {
        let app = app_with_group(&["old@x", "new@x"]);
        app.store.mutate(|s| {
            s.hosts.push(RmngClone {
                id: "pinned".into(),
                managed: true,
                archived: true, // no docker in the test; the binding is what is under test
                claude_account_email: Some("old@x".into()),
                claude_selection: Some("old@x".into()),
                ..Default::default()
            });
            s.hosts.push(RmngClone {
                id: "pooled".into(),
                managed: true,
                archived: true,
                claude_account_email: Some("old@x".into()),
                claude_selection: Some("group:team".into()),
                claude_group: Some("team".into()),
                ..Default::default()
            });
        });

        // The two halves `replace_account` composes, minus its config write: that one goes
        // through `crate::config::save`, which writes a fixed relative path and would leave a
        // `config.json` behind in whatever directory the suite ran in. The pool half is
        // covered by `a_replacement_inherits_every_pool_the_old_account_sat_in`.
        let moved = repoint_clones::<ClaudePool>(&app, "old@x", "new@x");
        assert_eq!(moved, vec!["pinned".to_string(), "pooled".to_string()]);
        delete_account::<ClaudePool>(&app, "old@x").await.unwrap();

        let hosts = app.store.get().hosts;
        let pinned = hosts.iter().find(|h| h.id == "pinned").unwrap();
        assert_eq!(
            pinned.claude_selection.as_deref(),
            Some("new@x"),
            "the pin follows"
        );
        assert_eq!(pinned.claude_account_email.as_deref(), Some("new@x"));
        let pooled = hosts.iter().find(|h| h.id == "pooled").unwrap();
        assert_eq!(pooled.claude_account_email.as_deref(), Some("new@x"));

        // The old account is gone, including from the published list, and a pin no longer
        // blocks that: moving it is part of the same operation.
        assert!(!app.claude.emails().contains(&"old@x".to_string()));
    }

    /// Signing in as the SAME account is a token repair, which `upsert_account` has already
    /// done by the time this is reached. Treating it as a replacement would delete the
    /// account that was just fixed.
    #[tokio::test]
    async fn replacing_an_account_with_itself_does_nothing() {
        let app = app_with_group(&["a@x", "b@x"]);
        assert!(
            replace_account::<ClaudePool>(&app, "a@x", "a@x")
                .await
                .unwrap()
                .is_empty()
        );
        assert!(app.claude.emails().contains(&"a@x".to_string()));
    }

    /// Mark `email`'s last access token as long expired, which is what a refresh chain
    /// Anthropic has rejected leaves behind: nothing can move `expires_at` forward again.
    fn kill_token(app: &App, email: &str) {
        let mut acct = app.claude.get_by_email(email).unwrap();
        acct.expires_at = now_ms() - 60 * 60 * 1000;
        app.claude.update_account(&acct).unwrap();
    }

    #[test]
    fn an_expired_token_leaves_the_rotation_but_stays_imported() {
        let app = app_with_group(&["live@x", "dead@x"]);
        kill_token(&app, "dead@x");

        assert_eq!(app.claude.usable_emails(), vec!["live@x".to_string()]);
        let members = vec!["live@x".to_string(), "dead@x".to_string()];
        assert_eq!(
            eligible_members::<ClaudePool>(&app, &members),
            vec!["live@x".to_string()]
        );
        // Still imported. Deleting it is the operator's call, and a clone pinned to it by
        // name still resolves, so the pin reports a real error instead of silently moving.
        assert!(app.claude.emails().contains(&"dead@x".to_string()));
    }

    #[test]
    fn a_clone_holding_a_dead_account_is_moved_off_it() {
        // Stickiness normally keeps a clone on its current account, because switching
        // cold-starts the prompt cache. A dead token beats stickiness: the clone is
        // already broken, so there is no cache worth protecting.
        let app = app_with_group(&["live@x", "dead@x"]);
        kill_token(&app, "dead@x");

        assert_eq!(
            pick_group_account::<ClaudePool>(&app, "team", Some("dead@x")).unwrap(),
            "live@x"
        );
    }

    #[test]
    fn a_token_that_has_not_expired_survives_a_failing_poll() {
        // The whole point of keying on expiry rather than on the last error: a 429 or a
        // dropped usage fetch leaves a working token, and evicting on it would rotate the
        // fleet over a blip.
        let now = 1_000_000_000_000;
        assert!(token_alive(now + 1, now));
        assert!(
            !token_alive(now, now),
            "expired to the millisecond is expired"
        );
    }

    /// Claude's own gate: the five-hour session window as well as the shared weekly cap.
    /// (The weekly cap itself is asserted for both sides in `crate::pool`.)
    #[test]
    fn exhaustion_threshold_is_80_5h_or_95_7d() {
        assert!(
            !is_exhausted::<ClaudePool>(80.0, 0.0),
            "exactly 80% 5h is still eligible"
        );
        assert!(
            is_exhausted::<ClaudePool>(80.1, 0.0),
            "just over 80% 5h is exhausted"
        );
        assert!(
            !is_exhausted::<ClaudePool>(0.0, 94.9),
            "under the 7d cap is eligible"
        );
        assert!(
            is_exhausted::<ClaudePool>(0.0, 95.0),
            "hitting the 7d cap is exhausted"
        );
        assert!(
            !is_exhausted::<ClaudePool>(79.9, 94.9),
            "both under caps is eligible"
        );
    }
}
