//! Codex (OpenAI/ChatGPT) accounts — the sibling of `claude.rs`. Same server-owned
//! single-token model: the server holds each account's OAuth pair in the 0600 store
//! `codex-accounts.json`, refreshes access tokens itself (expiry decoded from the
//! access-token JWT — the Codex OAuth response has no `expires_in`), injects only the
//! short-lived access + id token + account_id into a clone's `~/.codex/auth.json` with an
//! empty refresh token, and re-pushes on every rotation. Importing harvests the OAuth
//! triple from a clone already signed in to Codex via ChatGPT, then clears the clone's
//! auth.json so its CLI can never rotate the refresh token the server now owns.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use wire::{ClaudeUsage, ClaudeUsageWindow, CloneGroup, Host};

use crate::app::App;
use crate::clone_ops::{extract_json, jwt_claims, now_ms, rand_u64, run_clone_op, shuffle, snippet};

const USAGE_URL: &str = "https://chatgpt.com/backend-api/wham/usage";
const OAUTH_TOKEN_URL: &str = "https://auth.openai.com/oauth/token";
const OAUTH_CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";
/// Refresh an access token this far before its expiry (must exceed the worst-case
/// poll gap). Matches claude's lead.
const REFRESH_LEAD_MS: i64 = 60 * 60 * 1000;
const FETCH_TIMEOUT: Duration = Duration::from_secs(10);
const STAGGER: Duration = Duration::from_millis(400);

// scoring knobs — copied from claude.rs (identical semantics).
const SESSION_HEADROOM_PCT: f64 = 40.0;
const SEVEN_DAY_CAP_PCT: f64 = 95.0;
const ROTATE_MAX_FIVE_HOUR_PCT: f64 = 90.0;
const ROTATE_SECS: u64 = 600;

const IMPORT_SCRIPT: &str = include_str!("../scripts/codex-import.sh");

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
}

#[derive(Default, Serialize, Deserialize)]
struct AccountsFile {
    #[serde(default)]
    accounts: Vec<StoredCodexAccount>,
}

pub struct CodexStore {
    accounts: Mutex<Vec<StoredCodexAccount>>,
    last_good: Mutex<HashMap<String, ClaudeUsage>>,
    path: PathBuf,
    polling: Mutex<bool>,
    refresh_gate: tokio::sync::Mutex<()>,
    pushed: Mutex<HashMap<String, String>>,
}

impl CodexStore {
    pub fn load(data_dir: &str) -> Self {
        let path = std::env::var("RMNG_CODEX_ACCOUNTS_FILE")
            .map(PathBuf::from)
            .unwrap_or_else(|_| Path::new(data_dir).join("codex-accounts.json"));
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

    fn save(&self, accounts: &[StoredCodexAccount]) -> Result<()> {
        if let Some(d) = self.path.parent() {
            std::fs::create_dir_all(d).ok();
        }
        let tmp = self.path.with_extension(format!("tmp.{}", std::process::id()));
        let body =
            serde_json::to_string_pretty(&AccountsFile { accounts: accounts.to_vec() })? + "\n";
        std::fs::write(&tmp, body)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600)).ok();
        }
        std::fs::rename(&tmp, &self.path)?;
        Ok(())
    }

    fn snapshot(&self) -> Vec<StoredCodexAccount> {
        self.accounts.lock().unwrap().clone()
    }

    fn get_by_email(&self, email: &str) -> Option<StoredCodexAccount> {
        self.accounts.lock().unwrap().iter().find(|a| a.email == email).cloned()
    }

    fn emails(&self) -> Vec<String> {
        self.accounts.lock().unwrap().iter().map(|a| a.email.clone()).collect()
    }

    fn update_account(&self, acct: &StoredCodexAccount) -> Result<()> {
        let mut accounts = self.accounts.lock().unwrap();
        match accounts.iter_mut().find(|a| a.id == acct.id) {
            Some(existing) => *existing = acct.clone(),
            None => accounts.push(acct.clone()),
        }
        self.save(&accounts)
    }

    pub fn forget_pushed(&self, host_id: &str) {
        self.pushed.lock().unwrap().remove(host_id);
    }
}

// --- import from a signed-in clone ----------------------------------------

/// The on-disk `~/.codex/auth.json` shape.
#[derive(Deserialize)]
struct CodexAuthFile {
    #[serde(rename = "OPENAI_API_KEY", default)]
    openai_api_key: Option<String>,
    #[serde(default)]
    tokens: Option<CodexTokens>,
}

#[derive(Deserialize)]
struct CodexTokens {
    #[serde(default)]
    id_token: Option<String>,
    #[serde(default)]
    access_token: Option<String>,
    #[serde(default)]
    refresh_token: Option<String>,
    #[serde(default)]
    account_id: Option<String>,
}

/// The identity + tokens harvested from a signed-in clone.
pub struct CodexAuth {
    pub email: String,
    pub plan: String,
    pub account_id: String,
    pub id_token: String,
    pub access_token: String,
    pub refresh_token: String,
}

pub struct ImportResult {
    pub email: String,
    pub cleared: bool,
}

/// Parse + validate a `~/.codex/auth.json` body into a [`CodexAuth`]. Requires a
/// ChatGPT login (`OPENAI_API_KEY` null/absent) with a full `tokens` block, and decodes
/// the id_token JWT for email / plan / account_id (account_id falls back to the JWT claim
/// when absent from `tokens`).
fn parse_codex_auth(raw: &str) -> Result<CodexAuth> {
    let file: CodexAuthFile = serde_json::from_str(extract_json(raw))
        .map_err(|_| anyhow::anyhow!("couldn't parse ~/.codex/auth.json"))?;
    if file.openai_api_key.as_deref().is_some_and(|k| !k.is_empty()) {
        bail!("this clone is signed in to Codex with an API key, not a ChatGPT subscription");
    }
    let tokens = file.tokens.context("~/.codex/auth.json has no tokens block (not signed in?)")?;
    let (Some(id_token), Some(access_token), Some(refresh_token)) =
        (tokens.id_token, tokens.access_token, tokens.refresh_token)
    else {
        bail!("~/.codex/auth.json is missing its id/access/refresh tokens");
    };
    let claims = jwt_claims(&id_token).context("codex id_token is not a decodable JWT")?;
    let auth_ns = claims.get("https://api.openai.com/auth");
    let email = claims
        .get("email")
        .and_then(|v| v.as_str())
        .context("codex id_token has no email claim")?
        .to_string();
    let plan = auth_ns
        .and_then(|a| a.get("chatgpt_plan_type"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let account_id = tokens
        .account_id
        .filter(|s| !s.is_empty())
        .or_else(|| {
            auth_ns
                .and_then(|a| a.get("chatgpt_account_id"))
                .and_then(|v| v.as_str())
                .map(str::to_string)
        })
        .context("codex auth has no account_id (in tokens or id_token claim)")?;
    Ok(CodexAuth { email, plan, account_id, id_token, access_token, refresh_token })
}

/// Confirm clone `host` is signed in to Codex via ChatGPT and return its identity +
/// tokens. Reads `~/.codex/auth.json` (codex has no clean JSON `login status`).
pub async fn check_clone_auth(app: &App, host: &Host) -> Result<CodexAuth> {
    if !host.managed {
        bail!("host '{}' is not a managed clone; only clones can be imported", host.id);
    }
    let raw = run_clone_op(app, &host.id, IMPORT_SCRIPT, "status", &[]).await?;
    parse_codex_auth(&raw).map_err(|e| {
        anyhow::anyhow!("{e} — is codex installed and signed in on '{}'?", host.id)
    })
}

/// Import a Codex account from a signed-in clone: harvest the OAuth triple, upsert into
/// the 0600 store (by id), then delete the clone's auth.json so its CLI can't rotate the
/// refresh token the server now owns.
pub async fn import_clone_account(app: &App, host: &Host) -> Result<ImportResult> {
    if !host.managed {
        bail!("host '{}' is not a managed clone; only clones can be imported", host.id);
    }
    let auth = check_clone_auth(app, host).await?;
    let stored = StoredCodexAccount {
        id: format!("codex:{}", auth.account_id),
        email: auth.email.clone(),
        account_id: auth.account_id,
        plan: auth.plan,
        active: false,
        access_token: auth.access_token,
        id_token: auth.id_token,
        refresh_token: auth.refresh_token,
        expires_at: 0,
    };
    {
        let mut accts = app.codex.accounts.lock().unwrap();
        let mut by_id: HashMap<String, StoredCodexAccount> =
            accts.drain(..).map(|a| (a.id.clone(), a)).collect();
        by_id.insert(stored.id.clone(), stored);
        let mut next: Vec<_> = by_id.into_values().collect();
        next.sort_by(|a, b| a.email.cmp(&b.email));
        app.codex.save(&next)?;
        *accts = next;
    }
    let cleared = match run_clone_op(app, &host.id, IMPORT_SCRIPT, "clear", &[]).await {
        Ok(_) => true,
        Err(e) => {
            tracing::warn!("codex import: clearing '{}' auth.json failed: {e}", host.id);
            false
        }
    };
    app.codex.forget_pushed(&host.id);
    tracing::info!("imported Codex account {} from '{}' (cleared={cleared})", auth.email, host.id);
    Ok(ImportResult { email: auth.email, cleared })
}

// --- token refresh + push -------------------------------------------------

fn is_expired(expires_at: i64) -> bool {
    now_ms() + REFRESH_LEAD_MS >= expires_at
}

/// Set `acct.expires_at` from its access-token JWT `exp` claim; if the token isn't a
/// decodable JWT, fall back to a conservative 55-minute lifetime so the account still
/// refreshes before the CLI's 5-minute trigger.
fn set_expiry_from_access(acct: &mut StoredCodexAccount) {
    acct.expires_at =
        crate::clone_ops::jwt_exp_ms(&acct.access_token).unwrap_or_else(|| now_ms() + 55 * 60 * 1000);
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

/// Refresh `acct`'s access token unconditionally (rotates the single-use refresh token).
/// The OAuth response carries no `expires_in`, so expiry is decoded from the new access
/// token's JWT. Mutates `acct` in place; the caller persists.
async fn refresh_account(http: &reqwest::Client, acct: &mut StoredCodexAccount) -> Result<()> {
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
        .await?;
    let status = resp.status();
    if !status.is_success() {
        let text = resp.text().await.unwrap_or_default();
        bail!("refresh {}{}", status.as_u16(), snippet(&text));
    }
    let data: RefreshResp = resp.json().await?;
    if let Some(a) = data.access_token {
        acct.access_token = a;
    }
    if let Some(i) = data.id_token {
        acct.id_token = i;
    }
    if let Some(r) = data.refresh_token {
        acct.refresh_token = r;
    }
    set_expiry_from_access(acct);
    Ok(())
}

/// `email`'s current account, refreshed (and persisted) first if within
/// [`REFRESH_LEAD_MS`] of expiry. Returns `(account, rotated)`. Runs under the store's
/// refresh gate so concurrent callers can't burn the same single-use refresh token.
pub async fn fresh_access_token(app: &App, email: &str) -> Result<(StoredCodexAccount, bool)> {
    let _gate = app.codex.refresh_gate.lock().await;
    let mut acct = app
        .codex
        .get_by_email(email)
        .with_context(|| format!("no imported Codex account for '{email}'"))?;
    if !is_expired(acct.expires_at) {
        return Ok((acct, false));
    }
    refresh_account(&app.http, &mut acct).await?;
    app.codex.update_account(&acct)?;
    Ok((acct, true))
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

/// Install `acct`'s tokens into clone `host_id`'s `~/.codex/auth.json`. Sanity-checks the
/// access token is a JWT (`eyJ…`). Best-effort hot-swap; codex re-reads auth.json per call.
pub async fn apply_clone_token(app: &App, host_id: &str, acct: &StoredCodexAccount) -> Result<()> {
    if !acct.access_token.starts_with("eyJ") {
        bail!("refusing to apply a non-JWT codex access token");
    }
    let b64 = crate::provision::b64_encode(auth_json(acct).as_bytes());
    let out = run_clone_op(app, host_id, IMPORT_SCRIPT, "apply", &[&b64]).await?;
    if out.contains("OK") {
        Ok(())
    } else {
        bail!("codex token apply produced unexpected output: {}", out.trim());
    }
}

/// Remove clone `host_id`'s `~/.codex/auth.json`, leaving it with no Codex token.
pub async fn clear_clone_token(app: &App, host_id: &str) -> Result<()> {
    let out = run_clone_op(app, host_id, IMPORT_SCRIPT, "clear", &[]).await?;
    if out.contains("CLEARED") {
        Ok(())
    } else {
        bail!("codex token clear produced unexpected output: {}", out.trim());
    }
}

/// Refresh-if-needed and install `email`'s tokens into clone `host_id`, recording the
/// push. If the refresh rotated the token, fan it out to the account's other clones.
pub async fn push_account_to_clone(app: &App, host_id: &str, email: &str) -> Result<()> {
    let (acct, rotated) = fresh_access_token(app, email).await?;
    apply_clone_token(app, host_id, &acct).await?;
    app.codex.pushed.lock().unwrap().insert(host_id.to_string(), acct.access_token.clone());
    if rotated {
        let app = app.clone();
        tokio::spawn(async move { push_stale_tokens(&app).await });
    }
    Ok(())
}

/// Reconcile pass: every clone assigned a Codex account gets that account's current
/// access token, unless the last successful push already delivered it. Mirrors
/// `claude::push_stale_tokens`, reading `Host.codex_account_email`.
pub async fn push_stale_tokens(app: &App) {
    let mut first = true;
    for host in app.store.get().hosts {
        let Some(email) = host.codex_account_email.as_deref() else { continue };
        if !host.managed {
            continue;
        }
        let Some(acct) = app.codex.get_by_email(email) else { continue };
        let stale = app.codex.pushed.lock().unwrap().get(&host.id) != Some(&acct.access_token);
        if !stale {
            continue;
        }
        if !first {
            tokio::time::sleep(STAGGER).await;
        }
        first = false;
        match apply_clone_token(app, &host.id, &acct).await {
            Ok(()) => {
                app.codex.pushed.lock().unwrap().insert(host.id.clone(), acct.access_token.clone());
                tracing::info!("pushed fresh codex token ({email}) to {}", host.id);
            }
            Err(e) => tracing::warn!(
                "pushing codex token ({email}) to {} failed (retried next pass): {e}",
                host.id
            ),
        }
    }
}

// --- usage fetch + mapping -------------------------------------------------

#[derive(Deserialize)]
struct RawRateWindow {
    #[serde(default)]
    used_percent: Option<f64>,
    #[serde(default)]
    limit_window_seconds: Option<i64>,
    #[serde(default)]
    reset_at: Option<String>,
}
#[derive(Deserialize)]
struct RawRateLimit {
    #[serde(default)]
    primary_window: Option<RawRateWindow>,
    #[serde(default)]
    secondary_window: Option<RawRateWindow>,
}
#[derive(Deserialize)]
struct RawUsage {
    #[serde(default)]
    plan_type: Option<String>,
    #[serde(default)]
    rate_limit: Option<RawRateLimit>,
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

/// A rolling window whose `limit_window_seconds` is nearer 5h (18000s) than a week
/// (604800s) maps to the 5h bar, else the weekly bar — never by field order.
fn window_of(w: RawRateWindow) -> Option<(bool, ClaudeUsageWindow)> {
    let secs = w.limit_window_seconds?;
    let is_five = (secs - 18_000).abs() <= (secs - 604_800).abs();
    Some((is_five, ClaudeUsageWindow { pct: w.used_percent.unwrap_or(0.0).round(), resets_at: w.reset_at }))
}

fn to_usage(acct: &StoredCodexAccount, raw: RawUsage) -> ClaudeUsage {
    let mut five_hour = None;
    let mut seven_day = None;
    if let Some(rl) = raw.rate_limit {
        for w in [rl.primary_window, rl.secondary_window].into_iter().flatten() {
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
        spend: None,
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
        spend: None,
    }
}

// --- scoring + assignment (mirrors claude.rs) -----------------------------

const AUTO: &str = "auto";
const NONE: &str = "none";

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
        .filter(|u| u.provider == Some(wire::Provider::Codex))
        .map(|u| (u.email.as_str(), u))
        .collect();
    let mut clones: HashMap<&str, u32> = HashMap::new();
    for h in &st.hosts {
        if let Some(e) = &h.codex_account_email {
            *clones.entry(e.as_str()).or_insert(0) += 1;
        }
    }
    app.codex
        .emails()
        .into_iter()
        .map(|email| {
            let u = usage.get(email.as_str());
            let five = u.and_then(|u| u.five_hour.as_ref()).map(|w| w.pct).unwrap_or(0.0);
            let seven = u.and_then(|u| u.seven_day.as_ref()).map(|w| w.pct).unwrap_or(0.0);
            let headroom = clamp01((100.0 - five) / 100.0);
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

pub fn recommend(app: &App) -> Option<String> {
    best_scored(app)
}

pub fn resolve_clone_account(app: &App, requested: Option<&str>) -> Option<String> {
    let emails = app.codex.emails();
    if emails.is_empty() {
        return None;
    }
    let want = requested.unwrap_or("").trim();
    if !want.is_empty() && want != AUTO {
        if let Some(hit) = emails.iter().find(|e| e.as_str() == want) {
            return Some(hit.clone());
        }
        tracing::warn!("codex account '{want}' not imported; using recommended");
    }
    best_scored(app)
}

pub enum Assignment {
    Account(String),
    Group { name: String, initial: String },
    None,
}

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

fn clone_counts(app: &App) -> HashMap<String, u32> {
    let mut m = HashMap::new();
    for h in &app.store.get().hosts {
        if let Some(e) = &h.codex_account_email {
            *m.entry(e.clone()).or_insert(0) += 1;
        }
    }
    m
}

fn five_hour_pct(app: &App, email: &str) -> f64 {
    app.store
        .get()
        .claude_accounts
        .iter()
        .filter(|u| u.provider == Some(wire::Provider::Codex))
        .find(|u| u.email == email)
        .and_then(|u| u.five_hour.as_ref())
        .map(|w| w.pct)
        .unwrap_or(0.0)
}

fn eligible_group_accounts(app: &App, group: &CloneGroup) -> Vec<String> {
    let known = app.codex.emails();
    group
        .accounts
        .iter()
        .filter(|email| known.iter().any(|k| &k == email))
        .filter(|email| five_hour_pct(app, email) <= ROTATE_MAX_FIVE_HOUR_PCT)
        .cloned()
        .collect()
}

fn pick_group_account(app: &App, group_name: &str) -> Option<String> {
    let cfg = app.config();
    let group = cfg.codex_groups.iter().find(|g| g.name == group_name)?;
    let counts = clone_counts(app);
    let mut pool = eligible_group_accounts(app, group);
    if pool.is_empty() {
        let known = app.codex.emails();
        pool = group.accounts.iter().filter(|e| known.iter().any(|k| &k == e)).cloned().collect();
    }
    shuffle(&mut pool);
    pool.into_iter().min_by_key(|email| {
        let load = *counts.get(email).unwrap_or(&0);
        let pct = five_hour_pct(app, email).round() as u32;
        (load, pct)
    })
}

fn assign_rotation(
    clones: &[Host],
    eligible: &[String],
    usage: &HashMap<String, f64>,
) -> Vec<(Host, String)> {
    let mut used: HashMap<String, u32> = HashMap::new();
    let mut out: Vec<(Host, String)> = Vec::with_capacity(clones.len());
    let mut homeless: Vec<Host> = Vec::new();
    for c in clones {
        match &c.codex_account_email {
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

pub async fn rotate_once(app: &App) {
    let cfg = app.config();
    if cfg.codex_groups.is_empty() {
        return;
    }
    let mut by_group: HashMap<String, Vec<Host>> = HashMap::new();
    for h in &app.store.get().hosts {
        if let (Some(g), true) = (&h.codex_group, h.managed) {
            by_group.entry(g.clone()).or_default().push(h.clone());
        }
    }
    for (gname, clones) in by_group {
        let Some(group) = cfg.codex_groups.iter().find(|g| g.name == gname) else {
            continue;
        };
        let eligible = eligible_group_accounts(app, group);
        if eligible.is_empty() {
            tracing::info!(
                "codex rotate: group '{gname}' has no account <= {ROTATE_MAX_FIVE_HOUR_PCT}% 5h; leaving {} clone(s)",
                clones.len()
            );
            continue;
        }
        let usage: HashMap<String, f64> =
            eligible.iter().map(|e| (e.clone(), five_hour_pct(app, e))).collect();
        for (host, email) in assign_rotation(&clones, &eligible, &usage) {
            if host.codex_account_email.as_deref() == Some(email.as_str()) {
                continue;
            }
            match push_account_to_clone(app, &host.id, &email).await {
                Ok(()) => {
                    tracing::info!(
                        "codex rotate: {} {} -> {}",
                        host.id,
                        host.codex_account_email.as_deref().unwrap_or("none"),
                        email
                    );
                    let id = host.id.clone();
                    app.store.mutate(|s| {
                        if let Some(h) = s.hosts.iter_mut().find(|h| h.id == id) {
                            h.codex_account_email = Some(email);
                        }
                    });
                }
                Err(e) => tracing::warn!("codex rotate: applying {email} to {} failed: {e}", host.id),
            }
            tokio::time::sleep(STAGGER).await;
        }
    }
}

pub async fn run_rotator(app: App) {
    tokio::time::sleep(Duration::from_secs(30)).await;
    loop {
        rotate_once(&app).await;
        tokio::time::sleep(Duration::from_secs(ROTATE_SECS)).await;
    }
}

async fn auto_swap_exhausted(app: &App) {
    let st = app.store.get();
    let usage: HashMap<String, &ClaudeUsage> = st
        .claude_accounts
        .iter()
        .filter(|u| u.provider == Some(wire::Provider::Codex))
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
        if host.codex_group.is_some() {
            continue;
        }
        if !host.managed {
            continue;
        }
        let Some(cur) = &host.codex_account_email else { continue };
        if !exhausted(cur) {
            continue;
        }
        let Some(next) = best_scored(app) else { continue };
        if &next == cur || exhausted(&next) {
            continue;
        }
        match push_account_to_clone(app, &host.id, &next).await {
            Ok(()) => {
                tracing::info!("codex auto-swapped {} from {cur} to {next}", host.id);
                let id = host.id.clone();
                app.store.mutate(|s| {
                    if let Some(h) = s.hosts.iter_mut().find(|h| h.id == id) {
                        h.codex_account_email = Some(next);
                    }
                });
            }
            Err(e) => tracing::warn!("codex auto-swap of {} failed: {e}", host.id),
        }
    }
}

// --- poller ----------------------------------------------------------------

pub async fn poll_once(app: &App) -> Result<bool> {
    {
        let mut p = app.codex.polling.lock().unwrap();
        if *p {
            return Ok(false);
        }
        *p = true;
    }
    let result = poll_inner(app).await;
    *app.codex.polling.lock().unwrap() = false;
    result
}

async fn poll_inner(app: &App) -> Result<bool> {
    let accts = app.codex.snapshot();
    let cfg = app.config();
    if accts.is_empty() {
        crate::clone_ops::replace_provider_views(app, wire::Provider::Codex, Vec::new(), None);
        return Ok(false);
    }

    let usage_polling = cfg.codex.usage_polling;
    let mut any429 = false;
    let mut views = Vec::with_capacity(accts.len());

    for (i, acct) in accts.iter().enumerate() {
        if i > 0 {
            tokio::time::sleep(STAGGER).await;
        }
        let outcome = async {
            let (fresh, _) = fresh_access_token(app, &acct.email).await?;
            if !usage_polling {
                // Refresh + push still happen; skip the usage fetch, publish a base view.
                let mut b = codex_base(acct);
                b.error = Some("usage polling disabled (codex.usagePolling=false)".into());
                return Ok::<_, anyhow::Error>(b);
            }
            let raw = fetch_usage(&app.http, &fresh.access_token, &fresh.account_id).await?;
            Ok::<_, anyhow::Error>(to_usage(acct, raw))
        }
        .await;
        match outcome {
            Ok(u) => {
                app.codex.last_good.lock().unwrap().insert(acct.id.clone(), u.clone());
                views.push(u);
            }
            Err(e) => {
                let msg = e.to_string();
                if msg.contains("429") {
                    any429 = true;
                }
                let prev = app.codex.last_good.lock().unwrap().get(&acct.id).cloned();
                views.push(match prev {
                    Some(mut p) => {
                        p.stale = Some(true);
                        p
                    }
                    None => {
                        let mut b = codex_base(acct);
                        b.error = Some(msg);
                        b
                    }
                });
            }
        }
    }

    for v in &mut views {
        v.assignable = Some(true);
    }
    crate::clone_ops::replace_provider_views(
        app,
        wire::Provider::Codex,
        views,
        cfg.codex.pinned_email.as_deref(),
    );

    push_stale_tokens(app).await;

    if cfg.codex.auto_swap_on_exhaustion {
        auto_swap_exhausted(app).await;
    }
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
        let base = Duration::from_secs(app.config().codex.poll_secs.max(15));
        let delay = if any429 {
            backoff = (backoff + 1).min(8);
            let escalate = backoff.saturating_sub(2);
            (base * 2u32.pow(escalate)).min(MAX_BACKOFF)
        } else {
            backoff = 0;
            base
        };
        if any429 {
            tracing::warn!("codex usage rate-limited (429); next poll in {}s", delay.as_secs());
        }
        tokio::time::sleep(delay).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn jwt_with(payload: &str) -> String {
        let b64 = crate::provision::b64_encode(payload.as_bytes());
        let url = b64.trim_end_matches('=').replace('+', "-").replace('/', "_");
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
        }
    }

    #[test]
    fn parses_codex_auth_identity() {
        let id_token = jwt_with(
            r#"{"email":"z@openai.com","exp":2000000000,"https://api.openai.com/auth":{"chatgpt_plan_type":"plus","chatgpt_account_id":"acc-1"}}"#,
        );
        let file = format!(
            r#"{{"OPENAI_API_KEY":null,"tokens":{{"id_token":"{id_token}","access_token":"eyJaccess","refresh_token":"rt-1","account_id":"acc-1"}},"last_refresh":"2026-07-01T00:00:00Z"}}"#
        );
        let auth = parse_codex_auth(&file).unwrap();
        assert_eq!(auth.email, "z@openai.com");
        assert_eq!(auth.plan, "plus");
        assert_eq!(auth.account_id, "acc-1");
        assert_eq!(auth.refresh_token, "rt-1");
        assert_eq!(auth.access_token, "eyJaccess");
    }

    #[test]
    fn rejects_api_key_login() {
        // A codex CLI signed in with an API key has OPENAI_API_KEY set — not importable.
        let file = r#"{"OPENAI_API_KEY":"sk-proj-xxx","tokens":null}"#;
        assert!(parse_codex_auth(file).is_err());
        // Missing tokens block is also an error.
        assert!(parse_codex_auth(r#"{"OPENAI_API_KEY":null}"#).is_err());
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
        assert_eq!(reloaded.get_by_email("z@openai.com").unwrap().account_id, "acc-1");
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

    #[test]
    fn usage_maps_by_window_seconds_not_order() {
        // primary=5h, secondary=weekly.
        let body = r#"{"plan_type":"plus","rate_limit":{
            "primary_window":{"used_percent":12.0,"limit_window_seconds":18000,"reset_at":"2026-07-03T00:00:00Z"},
            "secondary_window":{"used_percent":3.0,"limit_window_seconds":604800,"reset_at":"2026-07-10T00:00:00Z"}
        }}"#;
        let u = to_usage(&sample_account(), serde_json::from_str(body).unwrap());
        assert_eq!(u.five_hour.as_ref().unwrap().pct, 12.0);
        assert_eq!(u.seven_day.as_ref().unwrap().pct, 3.0);
        assert_eq!(u.provider, Some(wire::Provider::Codex));
        assert!(u.spend.is_none());
        // Swapped field order: still classified by limit_window_seconds.
        let swapped = r#"{"rate_limit":{
            "primary_window":{"used_percent":3.0,"limit_window_seconds":604800},
            "secondary_window":{"used_percent":12.0,"limit_window_seconds":18000}
        }}"#;
        let u2 = to_usage(&sample_account(), serde_json::from_str(swapped).unwrap());
        assert_eq!(u2.five_hour.as_ref().unwrap().pct, 12.0);
        assert_eq!(u2.seven_day.as_ref().unwrap().pct, 3.0);
    }

    #[test]
    fn expiry_decoded_from_access_jwt() {
        // apply_expiry_from_jwt sets expires_at from the access token's exp claim.
        let mut acct = sample_account();
        acct.access_token = jwt_with(r#"{"exp":2000000000}"#);
        set_expiry_from_access(&mut acct);
        assert_eq!(acct.expires_at, 2_000_000_000_000);
    }

    fn clone_host(id: &str, cur: Option<&str>) -> Host {
        Host { id: id.into(), managed: true, codex_account_email: cur.map(str::to_string), ..Default::default() }
    }

    #[test]
    fn assignment_uses_codex_account_field() {
        // Sticky keep: a clone on an eligible account stays; a homeless clone lands in-set.
        let eligible = ["a@o".to_string(), "b@o".to_string()];
        let clones = [clone_host("c1", Some("a@o")), clone_host("c2", Some("z@gone"))];
        for _ in 0..50 {
            let got = assign_rotation(&clones, &eligible, &HashMap::new());
            let by_id: HashMap<_, _> = got.iter().map(|(h, e)| (h.id.clone(), e.clone())).collect();
            assert_eq!(by_id["c1"], "a@o");
            assert_eq!(by_id["c2"], "b@o");
        }
    }

    #[test]
    fn assignment_distinct_when_enough_accounts() {
        let eligible = ["a@o".to_string(), "b@o".to_string(), "c@o".to_string()];
        let clones = [clone_host("c1", None), clone_host("c2", None), clone_host("c3", None)];
        for _ in 0..50 {
            let got = assign_rotation(&clones, &eligible, &HashMap::new());
            let mut emails: Vec<_> = got.iter().map(|(_, e)| e.clone()).collect();
            emails.sort();
            emails.dedup();
            assert_eq!(emails.len(), 3);
        }
    }
}
