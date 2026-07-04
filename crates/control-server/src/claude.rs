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
//! publishes a token-free `ClaudeUsage` view onto `ControlState.claudeAccounts`,
//! and (when enabled) auto-swaps a clone whose account is exhausted.
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
        let tmp = self.path.with_extension(format!("tmp.{}", std::process::id()));
        let body = serde_json::to_string_pretty(&AccountsFile { accounts: accounts.to_vec() })? + "\n";
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
        self.accounts.lock().unwrap().iter().find(|a| a.email == email).cloned()
    }

    /// Emails of every imported account (the assignable universe).
    fn emails(&self) -> Vec<String> {
        self.accounts.lock().unwrap().iter().map(|a| a.email.clone()).collect()
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
        bail!("host '{}' is not a managed clone; only clones can be imported", host.id);
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
        bail!("host '{}' is not a managed clone; only clones can be imported", host.id);
    }

    // 1. Confirm the login + learn the account identity (email / org).
    let status = check_clone_auth(app, host).await?;
    let email = status.email.clone().context("`claude auth status` returned no email")?;
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

    tracing::info!("imported Claude account {email} from '{}' (cleared={cleared})", host.id);
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
    w.map(|w| ClaudeUsageWindow { pct: w.utilization.unwrap_or(0.0).round(), resets_at: w.resets_at })
}

fn to_usage(acct: &StoredClaudeAccount, raw: RawUsage) -> ClaudeUsage {
    let spend = raw.extra_usage.filter(|e| e.is_enabled).map(|e| ClaudeSpend {
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
                app.claude.last_good.lock().unwrap().insert(acct.id.clone(), u.clone());
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

    if cfg.claude.auto_swap_on_exhaustion {
        auto_swap_exhausted(app).await;
    }
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
    if want.is_empty() { AUTO.to_string() } else { want.to_string() }
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
            let five = u.and_then(|u| u.five_hour.as_ref()).map(|w| w.pct).unwrap_or(0.0);
            let seven = u.and_then(|u| u.seven_day.as_ref()).map(|w| w.pct).unwrap_or(0.0);
            let headroom = clamp01((100.0 - five) / 100.0);
            // reset-soon term omitted (ISO reset parsing TODO) → 0.
            let n = *clones.get(email.as_str()).unwrap_or(&0) as f64;
            let score = headroom - 0.5 * n;
            let eligible = (100.0 - five >= SESSION_HEADROOM_PCT) && seven < SEVEN_DAY_CAP_PCT;
            Scored { email, score, eligible }
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
        pool = scored.iter().collect();
    }
    pool.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));
    pool.first().map(|s| s.email.clone())
}

/// The recommended account (email) for a new clone (or None if none imported).
pub fn recommend(app: &App) -> Option<String> {
    best_scored(app)
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
/// apply right away; `None` means the operator explicitly opted out of a token (leave
/// the clone tokenless).
pub enum Assignment {
    Account(String),
    Group { name: String, initial: String },
    None,
}

/// Resolve a selection string to an [`Assignment`]: `none` → no token, `group:<name>` →
/// a group (with an initial account picked from it), else an email / `auto` → a single
/// account. Outer `None` if nothing usable is configured for an account/group pick.
pub fn resolve_assignment(app: &App, requested: Option<&str>) -> Option<Assignment> {
    let want = requested.unwrap_or("").trim();
    if want.eq_ignore_ascii_case(NONE) {
        return Some(Assignment::None);
    }
    if let Some(name) = want.strip_prefix("group:") {
        let name = name.trim();
        let initial = pick_group_account(app, name)?;
        return Some(Assignment::Group { name: name.to_string(), initial });
    }
    resolve_clone_account(app, requested).map(Assignment::Account)
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

/// Pick one account from group `group_name` for a new assignment: among eligible
/// members (or any member if none are eligible), fewest assigned clones first, then
/// lowest 5h usage, random tiebreak. `None` if the group is empty / has no imported
/// members.
fn pick_group_account(app: &App, group_name: &str) -> Option<String> {
    let cfg = app.config();
    let group = cfg.clone_groups.iter().find(|g| g.name == group_name)?;
    let counts = clone_counts(app);
    let mut pool = eligible_group_accounts(app, group);
    if pool.is_empty() {
        // All over the cap → still need a valid token; fall back to any imported member.
        let known = app.claude.emails();
        pool = group.accounts.iter().filter(|e| known.iter().any(|k| &k == e)).cloned().collect();
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

/// Rotate one pool of clones over candidate account emails `members`. Drops members
/// that aren't imported or are exhausted, sticky-assigns clones to the survivors
/// ([`assign_rotation`]), and pushes any moves (no agent-wrapper restart). `label` names
/// the pool in logs. Leaves clones untouched when no member is eligible.
async fn rotate_pool(app: &App, label: &str, members: &[String], clones: &[Host]) {
    let eligible = eligible_members(app, members);
    if eligible.is_empty() {
        tracing::info!("rotate: pool '{label}' has no eligible account; leaving {} clone(s)", clones.len());
        return;
    }
    let usage: HashMap<String, f64> =
        eligible.iter().map(|e| (e.clone(), five_hour_pct(app, e))).collect();
    for (host, email) in assign_rotation(clones, &eligible, &usage) {
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
            Err(e) => tracing::warn!("rotate[{label}]: applying {email} to {} failed: {e}", host.id),
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
    app.claude.pushed.lock().unwrap().insert(host_id.to_string(), token);
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
        let Some(email) = host.claude_account_email.as_deref() else { continue };
        if !host.managed {
            continue;
        }
        let Some(acct) = app.claude.get_by_email(email) else { continue };
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
                app.claude.pushed.lock().unwrap().insert(host.id.clone(), acct.access_token);
                tracing::info!("pushed fresh token ({email}) to {}", host.id);
            }
            Err(e) => {
                tracing::warn!("pushing token ({email}) to {} failed (retried next pass): {e}", host.id)
            }
        }
    }
}

/// When a clone's assigned account is exhausted, hot-swap it to the best alternative.
async fn auto_swap_exhausted(app: &App) {
    let st = app.store.get();
    let usage: HashMap<String, &ClaudeUsage> = st
        .claude_accounts
        .iter()
        .filter(|u| u.provider != Some(wire::Provider::Codex))
        .map(|u| (u.email.clone(), u))
        .collect();
    let exhausted = |email: &str| -> bool {
        usage.get(email).is_some_and(|u| {
            let five = u.five_hour.as_ref().map(|w| w.pct).unwrap_or(0.0);
            let seven = u.seven_day.as_ref().map(|w| w.pct).unwrap_or(0.0);
            (100.0 - five) < SESSION_HEADROOM_PCT || seven >= SEVEN_DAY_CAP_PCT
        })
    };
    for host in &st.hosts {
        // Group-bound clones are handled by the rotator, not exhaustion-swap.
        if host.claude_group.is_some() {
            continue;
        }
        if !host.managed {
            continue;
        }
        let Some(cur) = &host.claude_account_email else { continue };
        if !exhausted(cur) {
            continue;
        }
        let Some(next) = best_scored(app) else { continue };
        if &next == cur || exhausted(&next) {
            continue; // no better option
        }
        match push_account_to_clone(app, &host.id, &next).await {
            Ok(()) => {
                tracing::info!("auto-swapped {} from {cur} to {next}", host.id);
                let id = host.id.clone();
                app.store.mutate(|s| {
                    if let Some(h) = s.hosts.iter_mut().find(|h| h.id == id) {
                        h.claude_account_email = Some(next);
                    }
                });
            }
            Err(e) => tracing::warn!("auto-swap of {} failed: {e}", host.id),
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
            tracing::warn!("claude usage rate-limited (429); next poll in {}s", delay.as_secs());
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
        let oauth = serde_json::from_str::<ClaudeCreds>(CREDS).unwrap().claude_ai_oauth.unwrap();
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
            id: "a@b|o".into(), email: "a@b".into(), org_uuid: "o".into(),
            org_name: String::new(), active: false, access_token: String::new(),
            refresh_token: String::new(), expires_at: 0, scopes: vec![],
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
        assert_eq!(extract_json("  claude: command not found  "), "claude: command not found");
    }

    // --- groups: rotation assignment ---------------------------------------

    fn acct(email: &str) -> String {
        email.to_string()
    }
    fn clone_host(id: &str, cur: Option<&str>) -> Host {
        Host { id: id.into(), managed: true, claude_account_email: cur.map(str::to_string), ..Default::default() }
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
        let clones = [clone_host("c1", None), clone_host("c2", None), clone_host("c3", None)];
        for _ in 0..50 {
            let got = assign_rotation(&clones, &eligible, &HashMap::new());
            let mut emails: Vec<_> = got.iter().map(|(_, e)| e.clone()).collect();
            emails.sort();
            emails.dedup();
            assert_eq!(emails.len(), 3, "expected 3 distinct accounts, got {emails:?}");
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
        let clones = [clone_host("c1", Some("a@x")), clone_host("c2", Some("z@gone"))];
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
        let clones = [clone_host("c1", Some("only@x")), clone_host("c2", Some("old@x"))];
        let got = assign_rotation(&clones, &eligible, &HashMap::new());
        assert!(got.iter().all(|(_, e)| e == "only@x"));
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
