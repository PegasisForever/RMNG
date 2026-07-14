//! Claude accounts — usage tracking + clone assignment/swap.
//!
//! Single-token model: each account is just its short-lived OAuth pair (access +
//! single-use refresh token) in the 0600 secret store `claude-accounts.json`. The
//! control server owns the whole refresh lifecycle — nothing that can refresh ever
//! leaves it. A clone is authed by writing **only the current access token** into
//! its `~/.claude/.credentials.json` (empty refresh token, far-future expiry, so
//! Claude Code just uses whatever we last installed; see [`apply_clone_token`]).
//! Whenever a refresh rotates an account's access token, [`push_stale_tokens`]
//! fans the new token out to every clone assigned to that account. The poller
//! publishes a token-free `ClaudeUsage` view onto `ControlState.claudeAccounts`.
//! Clones select an account via `"auto"` (rotated across all imported accounts by
//! [`rotate_once`]), a named group, or a pinned email.
//!
//! **Importing an account** ([`check_clone_auth`] / [`import_clone_account`]) harvests
//! the OAuth pair from a clone that's already signed in to Claude Code via `claude.ai`:
//! we read `claude auth status` to confirm the login + identity, read the pair straight
//! off the clone's `~/.claude/.credentials.json`, then **delete that file from the
//! clone** so its Claude Code can never rotate (and thus invalidate) the refresh token
//! the server now owns. All clone commands run over `docker exec` (via
//! [`crate::provision::run_clone_op`]), replacing the retired Proxmox `pct exec` path — the
//! clone is addressed by its container name (== host id). (Codex accounts are out of scope
//! here — TODO if needed.)

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use wire::{ClaudeSpend, ClaudeUsage, ClaudeUsageWindow, CloneGroup, Host};

use crate::app::App;
use crate::clone_ops::{extract_json, now_ms, rand_u64, shuffle, snippet};

const USAGE_URL: &str = "https://api.anthropic.com/api/oauth/usage";
const OAUTH_TOKEN_URL: &str = "https://platform.claude.com/v1/oauth/token";
const OAUTH_CLIENT_ID: &str = "9d1c250a-e61b-44d9-88ed-5944d1962f5e";
const OAUTH_BETA_HEADER: &str = "oauth-2025-04-20";
const USER_AGENT: &str = "claude-swap/1.0";
/// Refresh an access token this far before its expiry. Clones *run* on these
/// tokens, so the lead must comfortably exceed the worst-case gap between polls
/// (poll_secs default 600s, 429 backoff up to 30 min).
const REFRESH_LEAD_MS: i64 = 60 * 60 * 1000;
const FETCH_TIMEOUT: Duration = Duration::from_secs(10);
const STAGGER: Duration = Duration::from_millis(400);

// scoring knobs (clone-accounts.server.ts)
const SESSION_HEADROOM_PCT: f64 = 20.0;
const SEVEN_DAY_CAP_PCT: f64 = 95.0;
const RESET_STICKY_MARGIN_SECS: i64 = 15 * 60;
const UTIL_STICKY_MARGIN_PCT: f64 = 5.0;
/// How often group-bound clones are checked against their group's eligible accounts.
/// Sticky: a pass moves a clone only if its account fell out of eligibility — an
/// account switch always cold-starts the clone's Anthropic prompt cache, so staying
/// put is cheaper than perfect spread.
const ROTATE_SECS: u64 = 600;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StoredClaudeAccount {
    pub id: String,
    pub email: String,
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
}

#[derive(Default, Serialize, Deserialize)]
struct AccountsFile {
    #[serde(default)]
    accounts: Vec<StoredClaudeAccount>,
}

/// Server-only Claude state: the secret account store + last-good usage cache.
pub struct ClaudeStore {
    accounts: Mutex<Vec<StoredClaudeAccount>>,
    last_good: Mutex<HashMap<String, ClaudeUsage>>,
    path: PathBuf,
    polling: Mutex<bool>,
    /// Serializes OAuth refreshes: refresh tokens are single-use, so two
    /// concurrent refreshes of one account would invalidate each other.
    refresh_gate: tokio::sync::Mutex<()>,
    /// host id → the access token last pushed to it successfully. In-memory on
    /// purpose: an empty map after a restart makes the first reconcile pass
    /// re-push every assigned clone.
    pushed: Mutex<HashMap<String, String>>,
}

impl ClaudeStore {
    pub fn load(data_dir: &str) -> Self {
        let path = Path::new(data_dir).join("claude-accounts.json");
        let accounts = std::fs::read_to_string(&path)
            .ok()
            .and_then(|s| serde_json::from_str::<AccountsFile>(&s).ok())
            .map(|f| f.accounts)
            .unwrap_or_default();
        Self {
            accounts: Mutex::new(accounts),
            last_good: Mutex::new(HashMap::new()),
            path,
            polling: Mutex::new(false),
            refresh_gate: tokio::sync::Mutex::new(()),
            pushed: Mutex::new(HashMap::new()),
        }
    }

    fn save(&self, accounts: &[StoredClaudeAccount]) -> Result<()> {
        if let Some(d) = self.path.parent() {
            std::fs::create_dir_all(d).ok();
        }
        let tmp = self
            .path
            .with_extension(format!("tmp.{}", std::process::id()));
        let body = serde_json::to_string_pretty(&AccountsFile {
            accounts: accounts.to_vec(),
        })? + "\n";
        std::fs::write(&tmp, body)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600)).ok();
        }
        std::fs::rename(&tmp, &self.path)?;
        Ok(())
    }

    fn snapshot(&self) -> Vec<StoredClaudeAccount> {
        self.accounts.lock().unwrap().clone()
    }

    fn get_by_email(&self, email: &str) -> Option<StoredClaudeAccount> {
        self.accounts
            .lock()
            .unwrap()
            .iter()
            .find(|a| a.email == email)
            .cloned()
    }

    /// Emails of every imported account (the assignable universe).
    fn emails(&self) -> Vec<String> {
        self.accounts
            .lock()
            .unwrap()
            .iter()
            .map(|a| a.email.clone())
            .collect()
    }

    /// Upsert `acct` (by id) and persist the store.
    fn update_account(&self, acct: &StoredClaudeAccount) -> Result<()> {
        let mut accounts = self.accounts.lock().unwrap();
        match accounts.iter_mut().find(|a| a.id == acct.id) {
            Some(existing) => *existing = acct.clone(),
            None => accounts.push(acct.clone()),
        }
        self.save(&accounts)
    }

    /// Drop `host_id`'s pushed-token record so the next reconcile pass re-pushes
    /// (or, for an unassigned host, simply forgets it).
    pub fn forget_pushed(&self, host_id: &str) {
        self.pushed.lock().unwrap().remove(host_id);
    }

    /// Remove every imported account matching `email` and persist. Returns whether any
    /// were present (false ⇒ nothing to delete). The token is gone from disk immediately;
    /// re-adding it requires a fresh import from a signed-in clone.
    fn delete(&self, email: &str) -> Result<bool> {
        let mut accounts = self.accounts.lock().unwrap();
        let before = accounts.len();
        accounts.retain(|a| a.email != email);
        if accounts.len() == before {
            return Ok(false);
        }
        self.save(&accounts)?;
        Ok(true)
    }
}

// --- import from a signed-in clone ----------------------------------------

/// Parsed `claude auth status` output. Clean JSON when Claude Code is signed in;
/// `loggedIn` is false (or the parse fails) otherwise.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AuthStatus {
    #[serde(default)]
    pub logged_in: bool,
    #[serde(default)]
    pub auth_method: Option<String>,
    #[serde(default)]
    pub email: Option<String>,
    #[serde(default)]
    pub org_id: Option<String>,
    #[serde(default)]
    pub org_name: Option<String>,
    #[serde(default)]
    pub subscription_type: Option<String>,
}

/// The on-disk `~/.claude/.credentials.json` shape (Claude Code's OAuth store).
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ClaudeCreds {
    #[serde(default)]
    claude_ai_oauth: Option<ClaudeOauth>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ClaudeOauth {
    #[serde(default)]
    access_token: Option<String>,
    #[serde(default)]
    refresh_token: Option<String>,
    #[serde(default)]
    expires_at: Option<i64>,
    #[serde(default)]
    scopes: Option<Vec<String>>,
}

/// What [`import_clone_account`] returns to the caller / UI.
pub struct ImportResult {
    pub email: String,
    /// Whether the clone's credentials file was successfully removed.
    pub cleared: bool,
}

/// Confirm clone `host` is signed in to Claude Code via **claude.ai** (not an API
/// key) and return its account identity. Used both to validate up front (so the UI
/// can show the account before the operator mints a token) and inside import.
pub async fn check_clone_auth(app: &App, host: &Host) -> Result<AuthStatus> {
    if !host.managed {
        bail!(
            "host '{}' is not a managed clone; only clones can be imported",
            host.id
        );
    }
    let raw = crate::provision::run_clone_op(app, &host.id, "status", &[]).await?;
    let status: AuthStatus = serde_json::from_str(extract_json(&raw)).map_err(|_| {
        anyhow::anyhow!(
            "couldn't read `claude auth status` on '{}' — is Claude Code installed and the clone running? (got: {})",
            host.id,
            extract_json(&raw).chars().take(140).collect::<String>()
        )
    })?;
    if !status.logged_in {
        bail!("'{}' is not signed in to Claude Code", host.id);
    }
    match status.auth_method.as_deref() {
        Some("claude.ai") => Ok(status),
        other => bail!(
            "'{}' is signed in via '{}', but import needs a claude.ai subscription login (not an API key)",
            host.id,
            other.unwrap_or("unknown"),
        ),
    }
}

/// Import a Claude account from a signed-in clone: read the OAuth pair (access +
/// refresh token) off the clone's credentials file, upsert it into the secret
/// store (by id), then **delete that file from the clone** so it can't rotate /
/// invalidate the refresh token the server now owns.
pub async fn import_clone_account(app: &App, host: &Host) -> Result<ImportResult> {
    if !host.managed {
        bail!(
            "host '{}' is not a managed clone; only clones can be imported",
            host.id
        );
    }

    // 1. Confirm the login + learn the account identity (email / org).
    let status = check_clone_auth(app, host).await?;
    let email = status
        .email
        .clone()
        .context("`claude auth status` returned no email")?;
    let org_uuid = status.org_id.clone().unwrap_or_default();

    // 2. Read the OAuth pair straight off the clone's disk.
    let raw = crate::provision::run_clone_op(app, &host.id, "read", &[])
        .await
        .with_context(|| format!("reading '{}' Claude credentials", host.id))?;
    let oauth = serde_json::from_str::<ClaudeCreds>(extract_json(&raw))
        .ok()
        .and_then(|c| c.claude_ai_oauth)
        .context("the clone's credentials file has no claudeAiOauth block")?;
    let (Some(access), Some(refresh)) = (oauth.access_token, oauth.refresh_token) else {
        bail!("the clone's credentials file is missing its access/refresh tokens");
    };

    // 3. Upsert into the 0600 secret store (by id).
    let id = format!("{email}|{org_uuid}");
    let stored = StoredClaudeAccount {
        id: id.clone(),
        email: email.clone(),
        org_uuid,
        org_name: status.org_name.clone().unwrap_or_default(),
        active: false,
        access_token: access,
        refresh_token: refresh,
        expires_at: oauth.expires_at.unwrap_or(0),
        scopes: oauth.scopes.unwrap_or_default(),
    };
    {
        let mut accts = app.claude.accounts.lock().unwrap();
        let mut by_id: HashMap<String, StoredClaudeAccount> =
            accts.drain(..).map(|a| (a.id.clone(), a)).collect();
        by_id.insert(stored.id.clone(), stored);
        let mut next: Vec<_> = by_id.into_values().collect();
        next.sort_by(|a, b| a.email.cmp(&b.email));
        app.claude.save(&next)?;
        *accts = next;
    }

    // 4. Clear the clone's credentials so its Claude Code can't rotate the refresh
    //    token we just took ownership of. Best-effort: the account is already stored.
    //    Forget the clone's pushed record too — if it has an assigned account, the
    //    next reconcile pass restores that token over the file we just deleted.
    let cleared = match crate::provision::run_clone_op(app, &host.id, "clear", &[]).await {
        Ok(_) => true,
        Err(e) => {
            tracing::warn!("import: clearing '{}' credentials failed: {e}", host.id);
            false
        }
    };
    app.claude.forget_pushed(&host.id);

    tracing::info!(
        "imported Claude account {email} from '{}' (cleared={cleared})",
        host.id
    );
    Ok(ImportResult { email, cleared })
}

// --- token refresh + usage fetch ------------------------------------------

fn is_expired(expires_at: i64) -> bool {
    now_ms() + REFRESH_LEAD_MS >= expires_at
}

#[derive(Deserialize)]
struct RefreshResp {
    access_token: String,
    expires_in: i64,
    #[serde(default)]
    refresh_token: Option<String>,
    #[serde(default)]
    scope: Option<String>,
}

/// Refresh `acct`'s access token unconditionally (rotates the single-use refresh
/// token). Mutates `acct` in place; the caller persists.
async fn refresh_account(http: &reqwest::Client, acct: &mut StoredClaudeAccount) -> Result<()> {
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
        .await?;
    let status = resp.status();
    if !status.is_success() {
        let text = resp.text().await.unwrap_or_default();
        bail!("refresh {}{}", status.as_u16(), snippet(&text));
    }
    let data: RefreshResp = resp.json().await?;
    acct.access_token = data.access_token;
    acct.expires_at = now_ms() + data.expires_in * 1000;
    if let Some(r) = data.refresh_token {
        acct.refresh_token = r;
    }
    if let Some(s) = data.scope {
        acct.scopes = s.split(' ').map(str::to_string).collect();
    }
    Ok(())
}

/// `email`'s current access token, refreshed (and persisted) first if within
/// [`REFRESH_LEAD_MS`] of expiry. Returns `(token, rotated)`. All refreshes run
/// under the store's refresh gate, so concurrent callers can't burn the same
/// single-use refresh token; the account is re-read under the gate so a refresh
/// another caller just finished is observed instead of repeated.
pub async fn fresh_access_token(app: &App, email: &str) -> Result<(String, bool)> {
    let _gate = app.claude.refresh_gate.lock().await;
    let mut acct = app
        .claude
        .get_by_email(email)
        .with_context(|| format!("no imported Claude account for '{email}'"))?;
    if !is_expired(acct.expires_at) {
        return Ok((acct.access_token, false));
    }
    refresh_account(&app.http, &mut acct).await?;
    app.claude.update_account(&acct)?;
    Ok((acct.access_token, true))
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
#[derive(Deserialize)]
struct RawUsage {
    #[serde(default)]
    five_hour: Option<RawWindow>,
    #[serde(default)]
    seven_day: Option<RawWindow>,
    #[serde(default)]
    extra_usage: Option<RawExtra>,
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

fn to_usage(acct: &StoredClaudeAccount, raw: RawUsage) -> ClaudeUsage {
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
        spend: None,
        reset_credits: None,
    }
}

/// Refresh-if-needed + fetch usage for every account; publish a token-free view.
/// Keeps last-good (marked `stale`) on per-account failure. Returns true on a 429.
pub async fn poll_once(app: &App) -> Result<bool> {
    {
        let mut p = app.claude.polling.lock().unwrap();
        if *p {
            return Ok(false);
        }
        *p = true;
    }
    let result = poll_inner(app).await;
    *app.claude.polling.lock().unwrap() = false;
    result
}

async fn poll_inner(app: &App) -> Result<bool> {
    let accts = app.claude.snapshot();
    if accts.is_empty() {
        crate::clone_ops::replace_provider_views(app, wire::Provider::Claude, Vec::new(), None);
        return Ok(false);
    }

    let mut any429 = false;
    let mut views = Vec::with_capacity(accts.len());

    for (i, acct) in accts.iter().enumerate() {
        if i > 0 {
            tokio::time::sleep(STAGGER).await;
        }
        let outcome = async {
            let (token, _) = fresh_access_token(app, &acct.email).await?;
            let raw = fetch_usage(&app.http, &token).await?;
            Ok::<_, anyhow::Error>(to_usage(acct, raw))
        }
        .await;
        match outcome {
            Ok(u) => {
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
                let prev = app.claude.last_good.lock().unwrap().get(&acct.id).cloned();
                views.push(match prev {
                    Some(mut p) => {
                        p.stale = Some(true);
                        p
                    }
                    None => {
                        let mut b = claude_base(acct);
                        b.error = Some(msg);
                        b
                    }
                });
            }
        }
    }

    // Every imported account can run a clone (the server owns its token lifecycle).
    for v in &mut views {
        v.assignable = Some(true);
    }
    let cfg = app.config();
    crate::clone_ops::replace_provider_views(
        app,
        wire::Provider::Claude,
        views,
        cfg.claude.pinned_email.as_deref(),
    );

    // Fan out any tokens this poll rotated (and retry earlier failed pushes).
    push_stale_tokens(app).await;

    Ok(any429)
}

// --- scoring + assignment (clone-accounts.server.ts) ----------------------

const AUTO: &str = "auto";
/// Selection sentinel: install no token at all (leave the clone tokenless).
const NONE: &str = "none";

/// Canonicalize a raw account-selection string into its stored form: `"auto"`,
/// `"none"`, `"group:<name>"`, or an account email. Missing/blank → `"auto"`.
pub fn normalize_selection(requested: Option<&str>) -> String {
    let want = requested.unwrap_or("").trim();
    if want.is_empty() {
        AUTO.to_string()
    } else {
        want.to_string()
    }
}

struct Scored {
    email: String,
    score: f64,
    eligible: bool,
}

fn clamp01(n: f64) -> f64 {
    n.clamp(0.0, 1.0)
}

fn score_accounts(app: &App) -> Vec<Scored> {
    let st = app.store.get();
    let usage: HashMap<&str, &ClaudeUsage> = st
        .claude_accounts
        .iter()
        .filter(|u| u.provider != Some(wire::Provider::Codex))
        .map(|u| (u.email.as_str(), u))
        .collect();
    let mut clones: HashMap<&str, u32> = HashMap::new();
    for h in &st.hosts {
        if let Some(e) = &h.claude_account_email {
            *clones.entry(e.as_str()).or_insert(0) += 1;
        }
    }
    app.claude
        .emails()
        .into_iter()
        .map(|email| {
            let u = usage.get(email.as_str());
            let five = u
                .and_then(|u| u.five_hour.as_ref())
                .map(|w| w.pct)
                .unwrap_or(0.0);
            let seven = u
                .and_then(|u| u.seven_day.as_ref())
                .map(|w| w.pct)
                .unwrap_or(0.0);
            let headroom = clamp01((100.0 - five) / 100.0);
            // reset-soon term omitted (ISO reset parsing TODO) → 0.
            let n = *clones.get(email.as_str()).unwrap_or(&0) as f64;
            let score = headroom - 0.5 * n;
            let eligible = (100.0 - five >= SESSION_HEADROOM_PCT) && seven < SEVEN_DAY_CAP_PCT;
            Scored {
                email,
                score,
                eligible,
            }
        })
        .collect()
}

fn best_scored(app: &App) -> Option<String> {
    let scored = score_accounts(app);
    if scored.is_empty() {
        return None;
    }
    let mut pool: Vec<&Scored> = scored.iter().filter(|s| s.eligible).collect();
    if pool.is_empty() {
        let members: Vec<String> = scored.iter().map(|s| s.email.clone()).collect();
        return best_saturated_email(&rotation_candidates(app, &members), &clone_counts(app));
    }
    pool.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    pool.first().map(|s| s.email.clone())
}

/// Resolve a clone request's account selection to a concrete account email.
pub fn resolve_clone_account(app: &App, requested: Option<&str>) -> Option<String> {
    let emails = app.claude.emails();
    if emails.is_empty() {
        return None;
    }
    let want = requested.unwrap_or("").trim();
    if !want.is_empty() && want != AUTO {
        if let Some(hit) = emails.iter().find(|e| e.as_str() == want) {
            return Some(hit.clone());
        }
        tracing::warn!("clone account '{want}' not imported; using recommended");
    }
    best_scored(app)
}

// --- groups: selection + rotation -----------------------------------------

/// What a clone is bound to (accounts by email). `Group` carries the initial pick to
/// apply right away; `AutoPending` records explicit auto intent before an imported
/// account exists; `None` means the operator explicitly opted out of a token.
pub enum Assignment {
    Account(String),
    Group { name: String, initial: String },
    AutoPending,
    None,
}

/// Resolve a selection string to an [`Assignment`]: `none` → no token, `group:<name>` →
/// a group (with an initial account picked from it), else an email / `auto` → a single
/// account. Explicit `auto` without imported accounts is kept as pending auto; outer
/// `None` means no usable concrete assignment and no explicit pending-auto intent.
///
/// `current` is the clone's account right now (for a swap); when resolving a group it
/// makes the pick sticky — a clone moving from a pinned account into a group that
/// already contains that account keeps it, rather than cold-starting its prompt cache.
/// Pass `None` when there's no incumbent (a fresh clone at create time).
pub fn resolve_assignment(
    app: &App,
    requested: Option<&str>,
    current: Option<&str>,
) -> Option<Assignment> {
    let want = requested.unwrap_or("").trim();
    if want.eq_ignore_ascii_case(NONE) {
        return Some(Assignment::None);
    }
    if let Some(name) = want.strip_prefix("group:") {
        let name = name.trim();
        let initial = pick_group_account(app, name, current)?;
        return Some(Assignment::Group {
            name: name.to_string(),
            initial,
        });
    }
    match resolve_clone_account(app, requested) {
        Some(account) => Some(Assignment::Account(account)),
        None if requested.is_some() && (want.is_empty() || want.eq_ignore_ascii_case(AUTO)) => {
            Some(Assignment::AutoPending)
        }
        None => None,
    }
}

/// How many clones each account email is currently assigned to.
fn clone_counts(app: &App) -> HashMap<String, u32> {
    let mut m = HashMap::new();
    for h in &app.store.get().hosts {
        if let Some(e) = &h.claude_account_email {
            *m.entry(e.clone()).or_insert(0) += 1;
        }
    }
    m
}

/// The 5h utilization for `email` from the latest usage view (0 if unknown).
fn five_hour_pct(app: &App, email: &str) -> f64 {
    app.store
        .get()
        .claude_accounts
        .iter()
        .filter(|u| u.provider != Some(wire::Provider::Codex))
        .find(|u| u.email == email)
        .and_then(|u| u.five_hour.as_ref())
        .map(|w| w.pct)
        .unwrap_or(0.0)
}

/// The 7d utilization for `email` from the latest usage view (0 if unknown).
fn seven_day_pct(app: &App, email: &str) -> f64 {
    app.store
        .get()
        .claude_accounts
        .iter()
        .filter(|u| u.provider != Some(wire::Provider::Codex))
        .find(|u| u.email == email)
        .and_then(|u| u.seven_day.as_ref())
        .map(|w| w.pct)
        .unwrap_or(0.0)
}

#[derive(Debug, Clone)]
struct RotationCandidate {
    email: String,
    five_pct: f64,
    seven_pct: f64,
    five_reset: Option<i64>,
    seven_reset: Option<i64>,
}

fn parse_rfc3339_utc_secs(s: &str) -> Option<i64> {
    if s.len() != 20
        || s.get(4..5)? != "-"
        || s.get(7..8)? != "-"
        || s.get(10..11)? != "T"
        || s.get(13..14)? != ":"
        || s.get(16..17)? != ":"
        || s.get(19..20)? != "Z"
    {
        return None;
    }
    let year: i32 = s.get(0..4)?.parse().ok()?;
    let month: u32 = s.get(5..7)?.parse().ok()?;
    let day: u32 = s.get(8..10)?.parse().ok()?;
    let hour: u32 = s.get(11..13)?.parse().ok()?;
    let minute: u32 = s.get(14..16)?.parse().ok()?;
    let second: u32 = s.get(17..19)?.parse().ok()?;
    if !(1..=12).contains(&month)
        || !(1..=31).contains(&day)
        || hour > 23
        || minute > 59
        || second > 59
    {
        return None;
    }
    let days = days_from_civil(year, month, day);
    Some(days * 86_400 + i64::from(hour) * 3_600 + i64::from(minute) * 60 + i64::from(second))
}

fn days_from_civil(year: i32, month: u32, day: u32) -> i64 {
    let y = year - i32::from(month <= 2);
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = month as i32 + if month > 2 { -3 } else { 9 };
    let doy = (153 * mp + 2) / 5 + day as i32 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    i64::from(era * 146_097 + doe - 719_468)
}

fn rotation_candidates(app: &App, members: &[String]) -> Vec<RotationCandidate> {
    let known = app.claude.emails();
    let st = app.store.get();
    members
        .iter()
        .filter(|email| known.iter().any(|k| &k == email))
        .map(|email| {
            let usage = st
                .claude_accounts
                .iter()
                .filter(|u| u.provider != Some(wire::Provider::Codex))
                .find(|u| u.email == *email);
            let five = usage.and_then(|u| u.five_hour.as_ref());
            let seven = usage.and_then(|u| u.seven_day.as_ref());
            RotationCandidate {
                email: email.clone(),
                five_pct: five.map(|w| w.pct).unwrap_or(0.0),
                seven_pct: seven.map(|w| w.pct).unwrap_or(0.0),
                five_reset: five
                    .and_then(|w| w.resets_at.as_deref())
                    .and_then(parse_rfc3339_utc_secs),
                seven_reset: seven
                    .and_then(|w| w.resets_at.as_deref())
                    .and_then(parse_rfc3339_utc_secs),
            }
        })
        .collect()
}

/// Whether an account is out of usable headroom: 5h over the session cap or 7d at the
/// weekly cap. Pure decision (see [`exhausted`] for the store-backed wrapper).
fn is_exhausted(five: f64, seven: f64) -> bool {
    (100.0 - five) < SESSION_HEADROOM_PCT || seven >= SEVEN_DAY_CAP_PCT
}

/// [`is_exhausted`] against `email`'s latest usage view.
fn exhausted(app: &App, email: &str) -> bool {
    is_exhausted(five_hour_pct(app, email), seven_day_pct(app, email))
}

/// Imported accounts among `members` that aren't exhausted — the usable rotation
/// targets for a pool. (Non-imported members have no token, so they're dropped.)
fn eligible_members(app: &App, members: &[String]) -> Vec<String> {
    let known = app.claude.emails();
    members
        .iter()
        .filter(|email| known.iter().any(|k| &k == email))
        .filter(|email| !exhausted(app, email))
        .cloned()
        .collect()
}

/// Group members that are imported accounts and not exhausted. Missing usage counts as
/// eligible (0% util).
fn eligible_group_accounts(app: &App, group: &CloneGroup) -> Vec<String> {
    eligible_members(app, &group.accounts)
}

/// Pick one account from group `group_name` for a new assignment. Stickiness first: if
/// the clone's `current` account is an eligible member of the group, keep it — switching
/// accounts cold-starts the clone's Anthropic prompt cache, so a clone moving from a
/// pinned account into a group that already contains it shouldn't be rebalanced off it
/// (mirrors the rotator's keep-if-eligible rule in [`assign_rotation`]). Otherwise: among
/// eligible members (or any member if none are eligible), fewest assigned clones first,
/// then lowest 5h usage, random tiebreak. `None` if the group is empty / has no imported
/// members.
fn pick_group_account(app: &App, group_name: &str, current: Option<&str>) -> Option<String> {
    let cfg = app.config();
    let group = cfg.clone_groups.iter().find(|g| g.name == group_name)?;
    let counts = clone_counts(app);
    let mut pool = eligible_group_accounts(app, group);
    if let Some(cur) = current {
        if pool.iter().any(|e| e == cur) {
            return Some(cur.to_string());
        }
    }
    if pool.is_empty() {
        // All over the cap → still need a valid token; fall back to any imported member.
        return best_saturated_email(&rotation_candidates(app, &group.accounts), &counts);
    }
    shuffle(&mut pool); // randomize ties
    pool.into_iter().min_by_key(|email| {
        let load = *counts.get(email).unwrap_or(&0);
        let pct = five_hour_pct(app, email).round() as u32;
        (load, pct)
    })
}

/// Sticky assignment of `clones` to `eligible` account emails (5h utilization in
/// `usage`), returning `(clone, email)` pairs. A clone whose current account is
/// still eligible **keeps it** — switching cold-starts the clone's Anthropic prompt
/// cache, so a clone is never moved just to even out spread. Only clones without an
/// eligible account (over the 5h cap, removed from the group, or unassigned) are
/// placed: fewest assigned clones first (keepers counted), then lowest 5h usage,
/// random tiebreak.
fn assign_rotation(
    clones: &[Host],
    eligible: &[String],
    usage: &HashMap<String, f64>,
) -> Vec<(Host, String)> {
    let mut used: HashMap<String, u32> = HashMap::new();
    let mut out: Vec<(Host, String)> = Vec::with_capacity(clones.len());
    let mut homeless: Vec<Host> = Vec::new();
    for c in clones {
        match &c.claude_account_email {
            Some(e) if eligible.contains(e) => {
                *used.entry(e.clone()).or_insert(0) += 1;
                out.push((c.clone(), e.clone()));
            }
            _ => homeless.push(c.clone()),
        }
    }
    shuffle(&mut homeless);
    for host in homeless {
        let pick = eligible
            .iter()
            .min_by_key(|email| {
                let load = *used.get(*email).unwrap_or(&0);
                let pct = usage.get(*email).copied().unwrap_or(0.0).round() as u32;
                (load, pct, rand_u64() as u32)
            })
            .expect("eligible is non-empty")
            .clone();
        *used.entry(pick.clone()).or_insert(0) += 1;
        out.push((host, pick));
    }
    out
}

fn pct_key(pct: f64) -> u32 {
    if !pct.is_finite() {
        return 0;
    }
    (pct.max(0.0) * 100.0).round() as u32
}

fn saturated_rank(
    candidate: &RotationCandidate,
    load: u32,
) -> (u8, i64, u32, u8, i64, u32, u32, u32) {
    let (five_missing, five_reset) = match candidate.five_reset {
        Some(reset) => (0, reset),
        None => (1, i64::MAX),
    };
    let (seven_missing, seven_reset) = match candidate.seven_reset {
        Some(reset) => (0, reset),
        None => (1, i64::MAX),
    };
    (
        five_missing,
        five_reset,
        pct_key(candidate.five_pct),
        seven_missing,
        seven_reset,
        pct_key(candidate.seven_pct),
        load,
        rand_u64() as u32,
    )
}

fn best_saturated_candidate<'a>(
    candidates: &'a [RotationCandidate],
    used: &HashMap<String, u32>,
) -> Option<&'a RotationCandidate> {
    candidates.iter().min_by_key(|candidate| {
        saturated_rank(candidate, *used.get(&candidate.email).unwrap_or(&0))
    })
}

fn best_saturated_email(
    candidates: &[RotationCandidate],
    used: &HashMap<String, u32>,
) -> Option<String> {
    best_saturated_candidate(candidates, used).map(|candidate| candidate.email.clone())
}

fn keep_saturated_current(current: &RotationCandidate, best: &RotationCandidate) -> bool {
    if current.email == best.email {
        return true;
    }
    match (current.five_reset, best.five_reset) {
        (Some(current_reset), Some(best_reset)) => {
            current_reset <= best_reset + RESET_STICKY_MARGIN_SECS
        }
        (None, None) => current.five_pct <= best.five_pct + UTIL_STICKY_MARGIN_PCT,
        _ => false,
    }
}

fn assign_saturated_rotation(
    clones: &[Host],
    candidates: &[RotationCandidate],
) -> Vec<(Host, String)> {
    if candidates.is_empty() {
        return Vec::new();
    }

    let mut used: HashMap<String, u32> = HashMap::new();
    let mut out: Vec<(Host, String)> = Vec::with_capacity(clones.len());
    let mut homeless: Vec<Host> = Vec::new();

    for clone in clones {
        let current = clone.claude_account_email.as_ref().and_then(|email| {
            candidates
                .iter()
                .find(|candidate| candidate.email == *email)
        });
        let best = best_saturated_candidate(candidates, &used).expect("candidates is non-empty");
        if let Some(current) = current {
            if keep_saturated_current(current, best) {
                *used.entry(current.email.clone()).or_insert(0) += 1;
                out.push((clone.clone(), current.email.clone()));
                continue;
            }
        }
        homeless.push(clone.clone());
    }

    shuffle(&mut homeless);
    for host in homeless {
        let pick = best_saturated_candidate(candidates, &used)
            .expect("candidates is non-empty")
            .email
            .clone();
        *used.entry(pick.clone()).or_insert(0) += 1;
        out.push((host, pick));
    }

    out
}

/// Rotate one pool of clones over candidate account emails `members`. Drops members
/// that aren't imported. When at least one account is under the hard limits, clones
/// stick to eligible accounts exactly as before. When every imported candidate is over
/// a limit, the saturated fallback picks the account closest to reset or least used.
async fn rotate_pool(app: &App, label: &str, members: &[String], clones: &[Host]) {
    let candidates = rotation_candidates(app, members);
    if candidates.is_empty() {
        tracing::info!(
            "rotate: pool '{label}' has no imported account; leaving {} clone(s)",
            clones.len()
        );
        return;
    }

    let eligible: Vec<String> = candidates
        .iter()
        .filter(|candidate| !is_exhausted(candidate.five_pct, candidate.seven_pct))
        .map(|candidate| candidate.email.clone())
        .collect();
    let assignments = if eligible.is_empty() {
        tracing::info!(
            "rotate: pool '{label}' has no under-cap account; using saturated fallback for {} clone(s)",
            clones.len()
        );
        assign_saturated_rotation(clones, &candidates)
    } else {
        let usage: HashMap<String, f64> = candidates
            .iter()
            .filter(|candidate| eligible.contains(&candidate.email))
            .map(|candidate| (candidate.email.clone(), candidate.five_pct))
            .collect();
        assign_rotation(clones, &eligible, &usage)
    };

    for (host, email) in assignments {
        if host.claude_account_email.as_deref() == Some(email.as_str()) {
            continue; // unchanged (sticky keep) → no rewrite
        }
        match push_account_to_clone(app, &host.id, &email).await {
            Ok(()) => {
                tracing::info!(
                    "rotate[{label}]: {} {} -> {}",
                    host.id,
                    host.claude_account_email.as_deref().unwrap_or("none"),
                    email
                );
                let id = host.id.clone();
                app.store.mutate(|s| {
                    if let Some(h) = s.hosts.iter_mut().find(|h| h.id == id) {
                        h.claude_account_email = Some(email);
                    }
                });
            }
            Err(e) => tracing::warn!(
                "rotate[{label}]: applying {email} to {} failed: {e}",
                host.id
            ),
        }
        tokio::time::sleep(STAGGER).await; // gentle on the daemon
    }
}

/// Managed clones bound to the implicit "auto" pool: `claude_selection == "auto"` and
/// not in a named group. Legacy hosts with `claude_selection == None` are treated as
/// pinned (never rotated).
fn auto_pool_clones(hosts: &[Host]) -> Vec<Host> {
    hosts
        .iter()
        .filter(|h| {
            h.managed && h.claude_group.is_none() && h.claude_selection.as_deref() == Some(AUTO)
        })
        .cloned()
        .collect()
}

/// One rotation pass over every named group plus the implicit "auto" pool (all imported
/// accounts, recomputed live). Sticky: a clone moves only when its account exhausts or
/// leaves its pool. See [`rotate_pool`] / [`assign_rotation`].
pub async fn rotate_once(app: &App) {
    let cfg = app.config();
    let hosts = app.store.get().hosts;
    // Named groups.
    let mut by_group: HashMap<String, Vec<Host>> = HashMap::new();
    for h in &hosts {
        if let (Some(g), true) = (&h.claude_group, h.managed) {
            by_group.entry(g.clone()).or_default().push(h.clone());
        }
    }
    for (gname, clones) in by_group {
        let Some(group) = cfg.clone_groups.iter().find(|g| g.name == gname) else {
            continue; // group deleted → leave its clones on their current account
        };
        rotate_pool(app, &gname, &group.accounts, &clones).await;
    }
    // "auto" == a live group of all imported accounts.
    let auto = auto_pool_clones(&hosts);
    if !auto.is_empty() {
        rotate_pool(app, "auto", &app.claude.emails(), &auto).await;
    }
}

/// Delete an imported Claude account by email, then heal the fleet.
///
/// Refuses (Err) if any clone is **pinned** to it (`claude_selection == email`) — a pin
/// is an explicit operator choice, so it must be reassigned first (swap it to another
/// account, a group, or `auto`). Clones running the account via `auto`/a group need no
/// pre-work: once the token is gone a [`rotate_once`] pass moves them onto a surviving
/// account (its assign step treats a no-longer-imported account as ineligible). Any clone
/// that still can't be placed — its whole pool was this one account — is left cleanly
/// unassigned (`claude_account_email = None`) rather than pointing at a deleted account.
///
/// Returns the ids of clones that were moved off (or unassigned from) the account.
pub async fn delete_account(app: &App, email: &str) -> Result<Vec<String>> {
    let pinned: Vec<String> = app
        .store
        .get()
        .hosts
        .iter()
        .filter(|h| h.claude_selection.as_deref() == Some(email))
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
    if !app.claude.delete(email)? {
        bail!("no imported Claude account '{email}'");
    }

    // Clones currently running the (now-deleted) account. Drop their pushed-token records so
    // the reassignment pushes the replacement token, then reassign auto/group pools now
    // rather than waiting up to `ROTATE_SECS` for the background rotator.
    let on_it: Vec<String> = app
        .store
        .get()
        .hosts
        .iter()
        .filter(|h| h.claude_account_email.as_deref() == Some(email))
        .map(|h| h.id.clone())
        .collect();
    for id in &on_it {
        app.claude.forget_pushed(id);
    }
    rotate_once(app).await;

    // Degenerate case (the account was a pool's only imported member): rotate couldn't
    // place those clones, so clear the dangling reference instead of leaving them pointed
    // at a deleted account.
    app.store.mutate(|s| {
        for h in &mut s.hosts {
            if h.claude_account_email.as_deref() == Some(email) {
                h.claude_account_email = None;
            }
        }
    });
    Ok(on_it)
}

/// Self-scheduling 10-minute group-rotation loop.
pub async fn run_rotator(app: App) {
    // Let the usage poller publish 5h numbers before the first rotation.
    tokio::time::sleep(Duration::from_secs(30)).await;
    loop {
        rotate_once(&app).await;
        tokio::time::sleep(Duration::from_secs(ROTATE_SECS)).await;
    }
}

/// The `~/.claude/.credentials.json` body that runs Claude Code under `token` (the
/// account's current short-lived access token). The refresh token is left **empty**
/// and the expiry far-future so the clone's Claude Code never tries to rotate or
/// abandon the token — it just uses whatever the server last installed; the server
/// pushes a replacement on every refresh ([`push_stale_tokens`]).
fn credentials_json(token: &str) -> String {
    format!(
        r#"{{"claudeAiOauth":{{"accessToken":"{token}","refreshToken":"","expiresAt":4102444800000,"scopes":["user:inference","user:profile"],"subscriptionType":"max"}}}}"#
    )
}

/// Install an access token into clone `host_id`'s `~/.claude/.credentials.json` over
/// `docker exec` (via [`crate::provision::run_clone_op`], fish-proof). Hot-swaps a running
/// clone with **no** agent-wrapper restart — Claude Code re-reads the file at request time.
/// Best-effort; errors are returned to log. Low-level: callers that target an assigned host
/// should go through [`push_account_to_clone`] / [`push_stale_tokens`] so the push is recorded.
pub async fn apply_clone_token(app: &App, host_id: &str, token: &str) -> Result<()> {
    let token = token.trim();
    if !token.starts_with("sk-ant-") {
        bail!("refusing to apply a non-`sk-ant-` token");
    }
    let b64 = crate::provision::b64_encode(credentials_json(token).as_bytes());
    let out = crate::provision::run_clone_op(app, host_id, "apply", &[&b64]).await?;
    if out.contains("OK") {
        Ok(())
    } else {
        bail!("token apply produced unexpected output: {}", out.trim());
    }
}

/// Remove clone `host_id`'s `~/.claude/.credentials.json` over `docker exec`, leaving it
/// with no Claude token. Used when a clone's account is set to "none" (unassigned) —
/// callers should also [`ClaudeStore::forget_pushed`] the host.
pub async fn clear_clone_token(app: &App, host_id: &str) -> Result<()> {
    let out = crate::provision::run_clone_op(app, host_id, "clear", &[]).await?;
    if out.contains("CLEARED") {
        Ok(())
    } else {
        bail!("token clear produced unexpected output: {}", out.trim());
    }
}

/// Refresh-if-needed and install `email`'s access token into clone `host_id` (== its
/// container name), recording the push so the reconcile pass doesn't repeat it. If the
/// refresh rotated the token, fan it out to the account's other clones in the background.
pub async fn push_account_to_clone(app: &App, host_id: &str, email: &str) -> Result<()> {
    let (token, rotated) = fresh_access_token(app, email).await?;
    apply_clone_token(app, host_id, &token).await?;
    app.claude
        .pushed
        .lock()
        .unwrap()
        .insert(host_id.to_string(), token);
    if rotated {
        let app = app.clone();
        tokio::spawn(async move { push_stale_tokens(&app).await });
    }
    Ok(())
}

/// Reconcile pass: every clone assigned an account gets that account's current access
/// token, unless the last successful push already delivered exactly that token. Runs
/// at the end of every poll (where refreshes happen, so a rotation is pushed in the
/// same pass) and after out-of-band rotations; a failed push (clone stopped /
/// unreachable) stays stale and is retried next pass. The pushed map is in-memory, so
/// the first pass after a server restart re-pushes every assigned clone.
pub async fn push_stale_tokens(app: &App) {
    let mut first = true;
    for host in app.store.get().hosts {
        let Some(email) = host.claude_account_email.as_deref() else {
            continue;
        };
        if !host.managed {
            continue;
        }
        let Some(acct) = app.claude.get_by_email(email) else {
            continue;
        };
        let stale = app.claude.pushed.lock().unwrap().get(&host.id) != Some(&acct.access_token);
        if !stale {
            continue;
        }
        if !first {
            tokio::time::sleep(STAGGER).await; // gentle on the daemon
        }
        first = false;
        match apply_clone_token(app, &host.id, &acct.access_token).await {
            Ok(()) => {
                app.claude
                    .pushed
                    .lock()
                    .unwrap()
                    .insert(host.id.clone(), acct.access_token);
                tracing::info!("pushed fresh token ({email}) to {}", host.id);
            }
            Err(e) => {
                tracing::warn!(
                    "pushing token ({email}) to {} failed (retried next pass): {e}",
                    host.id
                )
            }
        }
    }
}

/// Self-scheduling poll loop with 429 backoff.
pub async fn run_poller(app: App) {
    const MAX_BACKOFF: Duration = Duration::from_secs(30 * 60);
    let mut backoff: u32 = 0;
    loop {
        let any429 = match poll_once(&app).await {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!("claude usage poll failed: {e}");
                false
            }
        };
        let base = Duration::from_secs(app.config().claude.poll_secs.max(15));
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
                "claude usage rate-limited (429); next poll in {}s",
                delay.as_secs()
            );
        }
        tokio::time::sleep(delay).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // The exact shapes Claude Code v2 emits — `claude auth status` (camelCase JSON)
    // and `~/.claude/.credentials.json` (camelCase, nested under `claudeAiOauth`).
    const AUTH_STATUS: &str = r#"{
        "loggedIn": true, "authMethod": "claude.ai", "apiProvider": "firstParty",
        "email": "a@b.com", "orgId": "org-uuid", "orgName": "A's Org",
        "subscriptionType": "max"
    }"#;
    const CREDS: &str = r#"{"claudeAiOauth":{"accessToken":"sk-ant-oat01-AAA",
        "refreshToken":"sk-ant-ort01-BBB","expiresAt":1782865752191,
        "scopes":["user:inference","user:profile"],"subscriptionType":"max"}}"#;

    #[test]
    fn parses_auth_status() {
        let s: AuthStatus = serde_json::from_str(extract_json(AUTH_STATUS)).unwrap();
        assert!(s.logged_in);
        assert_eq!(s.auth_method.as_deref(), Some("claude.ai"));
        assert_eq!(s.email.as_deref(), Some("a@b.com"));
        assert_eq!(s.org_id.as_deref(), Some("org-uuid"));
    }

    #[test]
    fn parses_credentials_camelcase() {
        // Regression: `claudeAiOauth` (camelCase) must map onto `claude_ai_oauth`.
        let oauth = serde_json::from_str::<ClaudeCreds>(CREDS)
            .unwrap()
            .claude_ai_oauth
            .unwrap();
        assert_eq!(oauth.access_token.as_deref(), Some("sk-ant-oat01-AAA"));
        assert_eq!(oauth.refresh_token.as_deref(), Some("sk-ant-ort01-BBB"));
        assert_eq!(oauth.expires_at, Some(1782865752191));
        assert_eq!(oauth.scopes.unwrap().len(), 2);
    }

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
            org_uuid: "o".into(),
            org_name: String::new(),
            active: false,
            access_token: String::new(),
            refresh_token: String::new(),
            expires_at: 0,
            scopes: vec![],
        };
        let u = to_usage(&acct, raw);
        assert_eq!(u.five_hour.unwrap().pct, 7.0);
        assert_eq!(u.seven_day.unwrap().pct, 2.0);
        assert!(u.spend.is_none()); // extra usage disabled → no spend line
    }

    #[test]
    fn extract_json_strips_login_shell_noise() {
        // A login shell may wrap the JSON in MOTD/profile chatter on either side.
        let noisy = format!("Welcome to Ubuntu\n{AUTH_STATUS}\nLast login: today");
        let s: AuthStatus = serde_json::from_str(extract_json(&noisy)).unwrap();
        assert!(s.logged_in);
        // No JSON at all → falls back to the trimmed input (which then fails to parse).
        assert_eq!(
            extract_json("  claude: command not found  "),
            "claude: command not found"
        );
    }

    // --- groups: rotation assignment ---------------------------------------

    fn acct(email: &str) -> String {
        email.to_string()
    }
    fn clone_host(id: &str, cur: Option<&str>) -> Host {
        Host {
            id: id.into(),
            managed: true,
            claude_account_email: cur.map(str::to_string),
            ..Default::default()
        }
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

    #[test]
    fn assignment_rule_a_only_group_accounts() {
        // Every clone is assigned an account from the eligible set, never outside it.
        let eligible = [acct("a@x"), acct("b@x")];
        let clones = [clone_host("c1", Some("z@outside")), clone_host("c2", None)];
        for (_h, picked) in assign_rotation(&clones, &eligible, &HashMap::new()) {
            assert!(eligible.contains(&picked), "{picked} not in group");
        }
    }

    #[test]
    fn assignment_rule_b_distinct_when_enough_accounts() {
        // |eligible| >= |unassigned clones| ⇒ they land on distinct accounts (run
        // repeatedly: randomized, but the load term forces distinctness here).
        let eligible = [acct("a@x"), acct("b@x"), acct("c@x")];
        let clones = [
            clone_host("c1", None),
            clone_host("c2", None),
            clone_host("c3", None),
        ];
        for _ in 0..50 {
            let got = assign_rotation(&clones, &eligible, &HashMap::new());
            let mut emails: Vec<_> = got.iter().map(|(_, e)| e.clone()).collect();
            emails.sort();
            emails.dedup();
            assert_eq!(
                emails.len(),
                3,
                "expected 3 distinct accounts, got {emails:?}"
            );
        }
    }

    #[test]
    fn assignment_rule_c_sticks_to_an_eligible_account() {
        // One clone on A, two eligible {A,B} ⇒ always stays on A: a switch would
        // cold-start the clone's prompt cache for zero gain.
        let eligible = [acct("a@x"), acct("b@x")];
        let clones = [clone_host("c1", Some("a@x"))];
        for _ in 0..50 {
            let got = assign_rotation(&clones, &eligible, &HashMap::new());
            assert_eq!(got[0].1, "a@x");
        }
    }

    #[test]
    fn assignment_moves_only_ineligible_and_avoids_keepers() {
        // c1 keeps its eligible account A; c2 (account dropped from the group) must
        // move, and lands on B — the keeper on A counts toward A's load.
        let eligible = [acct("a@x"), acct("b@x")];
        let clones = [
            clone_host("c1", Some("a@x")),
            clone_host("c2", Some("z@gone")),
        ];
        for _ in 0..50 {
            let got = assign_rotation(&clones, &eligible, &HashMap::new());
            let by_id: HashMap<_, _> = got.iter().map(|(h, e)| (h.id.clone(), e.clone())).collect();
            assert_eq!(by_id["c1"], "a@x");
            assert_eq!(by_id["c2"], "b@x");
        }
    }

    #[test]
    fn assignment_prefers_less_used_account_on_load_tie() {
        // A fresh clone with two equally-loaded accounts picks the lower 5h usage.
        let eligible = [acct("hot@x"), acct("cold@x")];
        let clones = [clone_host("c1", None)];
        let usage = HashMap::from([(acct("hot@x"), 72.0), (acct("cold@x"), 5.0)]);
        for _ in 0..50 {
            let got = assign_rotation(&clones, &eligible, &usage);
            assert_eq!(got[0].1, "cold@x");
        }
    }

    #[test]
    fn assignment_degrades_with_single_eligible() {
        // Only one usable account ⇒ all clones get it even though spread can't hold.
        let eligible = [acct("only@x")];
        let clones = [
            clone_host("c1", Some("only@x")),
            clone_host("c2", Some("old@x")),
        ];
        let got = assign_rotation(&clones, &eligible, &HashMap::new());
        assert!(got.iter().all(|(_, e)| e == "only@x"));
    }

    fn stored(email: &str) -> StoredClaudeAccount {
        StoredClaudeAccount {
            id: email.into(),
            email: email.into(),
            org_uuid: String::new(),
            org_name: String::new(),
            active: true,
            access_token: "sk-ant-oat01-x".into(),
            refresh_token: String::new(),
            // Far-future so `fresh_access_token` never attempts a (network) refresh in tests.
            expires_at: 4_102_444_800_000,
            scopes: Vec::new(),
        }
    }

    /// An App over a temp data dir with a `team` group of the given imported accounts.
    fn app_with_group(members: &[&str]) -> App {
        use std::sync::Arc;
        let dir = std::env::temp_dir().join(format!(
            "rmng-claude-sticky-{}-{}",
            std::process::id(),
            rand_u64()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let store = Arc::new(crate::state::StateStore::load(dir.join("state.json")).unwrap());
        let cfg = wire::AppConfig {
            data_dir: dir.to_string_lossy().into_owned(),
            clone_groups: vec![CloneGroup {
                name: "team".into(),
                accounts: members.iter().map(|s| s.to_string()).collect(),
            }],
            ..Default::default()
        };
        let app = App::new(store, cfg);
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
            match resolve_assignment(&app, Some("group:team"), Some("b@x")) {
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
            match resolve_assignment(&app, Some("group:team"), current) {
                Some(Assignment::Group { initial, .. }) => {
                    assert!(matches!(initial.as_str(), "a@x" | "b@x"), "picked non-member {initial}");
                }
                _ => panic!("expected a group assignment"),
            }
        }
    }

    fn auto_clone(id: &str, account: &str) -> Host {
        Host {
            id: id.into(),
            managed: true,
            claude_account_email: Some(account.into()),
            claude_selection: Some(AUTO.into()),
            ..Default::default()
        }
    }

    #[test]
    fn store_delete_removes_only_the_named_account() {
        let app = app_with_group(&["a@x", "b@x"]);
        assert!(app.claude.delete("a@x").unwrap(), "present → removed");
        assert!(!app.claude.emails().contains(&"a@x".to_string()));
        assert!(app.claude.emails().contains(&"b@x".to_string()));
        assert!(!app.claude.delete("a@x").unwrap(), "already gone → false");
    }

    #[tokio::test]
    async fn delete_account_refuses_when_a_clone_is_pinned() {
        let app = app_with_group(&["a@x", "b@x"]);
        app.store.mutate(|s| {
            s.hosts.push(Host {
                id: "c1".into(),
                managed: true,
                claude_account_email: Some("a@x".into()),
                claude_selection: Some("a@x".into()), // pinned
                ..Default::default()
            })
        });
        let err = delete_account(&app, "a@x").await.unwrap_err();
        assert!(err.to_string().contains("pinned"), "message: {err}");
        // Aborted before touching the store — the account is untouched.
        assert!(app.claude.emails().contains(&"a@x".to_string()));
    }

    #[tokio::test]
    async fn delete_account_removes_token_and_detaches_unpinned_clones() {
        let app = app_with_group(&["a@x", "b@x"]);
        app.store.mutate(|s| s.hosts.push(auto_clone("c1", "a@x")));
        let moved = delete_account(&app, "a@x").await.unwrap();
        assert_eq!(moved, vec!["c1".to_string()]);
        assert!(!app.claude.emails().contains(&"a@x".to_string()), "token deleted");
        // The clone no longer points at the deleted account. (The reassignment push fails
        // without a live clone/daemon in the test, so the dangling ref is cleared to None.)
        let c1 = app.store.get().hosts.into_iter().find(|h| h.id == "c1").unwrap();
        assert_ne!(c1.claude_account_email.as_deref(), Some("a@x"));
    }

    fn rotation_candidate(
        email: &str,
        five_pct: f64,
        seven_pct: f64,
        five_reset: Option<i64>,
        seven_reset: Option<i64>,
    ) -> RotationCandidate {
        RotationCandidate {
            email: email.to_string(),
            five_pct,
            seven_pct,
            five_reset,
            seven_reset,
        }
    }

    #[test]
    fn saturated_assignment_prefers_soonest_5h_reset() {
        let candidates = [
            rotation_candidate("soon@x", 97.0, 96.0, Some(1_000), Some(10_000)),
            rotation_candidate("late@x", 90.0, 96.0, Some(2_000), Some(10_000)),
        ];
        let clones = [clone_host("c1", Some("late@x"))];

        let got = assign_saturated_rotation(&clones, &candidates);

        assert_eq!(got[0].1, "soon@x");
    }

    #[test]
    fn saturated_assignment_uses_lower_5h_when_resets_are_missing() {
        let candidates = [
            rotation_candidate("hot@x", 98.0, 96.0, None, Some(10_000)),
            rotation_candidate("cool@x", 90.0, 96.0, None, Some(10_000)),
        ];
        let clones = [clone_host("c1", Some("hot@x"))];

        let got = assign_saturated_rotation(&clones, &candidates);

        assert_eq!(got[0].1, "cool@x");
    }

    #[test]
    fn saturated_assignment_keeps_current_within_reset_margin() {
        let candidates = [
            rotation_candidate("current@x", 98.0, 96.0, Some(1_800), Some(10_000)),
            rotation_candidate("best@x", 99.0, 96.0, Some(1_000), Some(10_000)),
        ];
        let clones = [clone_host("c1", Some("current@x"))];

        let got = assign_saturated_rotation(&clones, &candidates);

        assert_eq!(got[0].1, "current@x");
    }

    #[test]
    fn saturated_assignment_moves_missing_reset_current_to_known_reset() {
        let candidates = [
            rotation_candidate("unknown@x", 90.0, 96.0, None, Some(10_000)),
            rotation_candidate("known@x", 94.0, 96.0, Some(1_000), Some(10_000)),
        ];
        let clones = [clone_host("c1", Some("unknown@x"))];

        let got = assign_saturated_rotation(&clones, &candidates);

        assert_eq!(got[0].1, "known@x");
    }

    #[test]
    fn exhaustion_threshold_is_80_5h_or_95_7d() {
        assert!(!is_exhausted(80.0, 0.0), "exactly 80% 5h is still eligible");
        assert!(is_exhausted(80.1, 0.0), "just over 80% 5h is exhausted");
        assert!(!is_exhausted(0.0, 94.9), "under the 7d cap is eligible");
        assert!(is_exhausted(0.0, 95.0), "hitting the 7d cap is exhausted");
        assert!(!is_exhausted(79.9, 94.9), "both under caps is eligible");
    }

    // --- "auto" pool ---------------------------------------------------------

    fn host_sel(id: &str, managed: bool, group: Option<&str>, sel: Option<&str>) -> Host {
        Host {
            id: id.into(),
            managed,
            claude_group: group.map(str::to_string),
            claude_selection: sel.map(str::to_string),
            ..Default::default()
        }
    }

    #[test]
    fn auto_pool_is_only_managed_ungrouped_auto_clones() {
        let hosts = vec![
            host_sel("auto1", true, None, Some("auto")),        // in
            host_sel("pinned", true, None, Some("me@x")),       // out: pinned to an email
            host_sel("legacy", true, None, None),               // out: legacy None == pinned
            host_sel("grouped", true, Some("g"), Some("auto")), // out: named group handles it
            host_sel("stopped", false, None, Some("auto")),     // out: unmanaged
        ];
        let picked: Vec<String> = auto_pool_clones(&hosts).into_iter().map(|h| h.id).collect();
        assert_eq!(picked, vec!["auto1"]);
    }
}
