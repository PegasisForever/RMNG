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
use crate::clone_ops::{
    extract_json, jwt_claims, now_ms, rand_u64, run_clone_op, shuffle, snippet,
};

const USAGE_URL: &str = "https://chatgpt.com/backend-api/wham/usage";
const CONSUME_URL: &str = "https://chatgpt.com/backend-api/wham/rate-limit-reset-credits/consume";
const OAUTH_TOKEN_URL: &str = "https://auth.openai.com/oauth/token";
const OAUTH_CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";
/// Refresh an access token this far before its expiry (must exceed the worst-case
/// poll gap). Matches claude's lead.
const REFRESH_LEAD_MS: i64 = 60 * 60 * 1000;
const FETCH_TIMEOUT: Duration = Duration::from_secs(10);
const STAGGER: Duration = Duration::from_millis(400);

// scoring knobs — copied from claude.rs (identical semantics).
const SESSION_HEADROOM_PCT: f64 = 20.0;
const SEVEN_DAY_CAP_PCT: f64 = 95.0;
const RESET_STICKY_MARGIN_SECS: i64 = 15 * 60;
const UTIL_STICKY_MARGIN_PCT: f64 = 5.0;
const ROTATE_SECS: u64 = 600;
/// Auto-reset only fires when every account's 7d window is at least this far from
/// resetting (spec: "more than 24h from the next 7d reset").
const RESET_MIN_HEADROOM_SECS: i64 = 24 * 3600;

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

    fn snapshot(&self) -> Vec<StoredCodexAccount> {
        self.accounts.lock().unwrap().clone()
    }

    fn get_by_email(&self, email: &str) -> Option<StoredCodexAccount> {
        self.accounts
            .lock()
            .unwrap()
            .iter()
            .find(|a| a.email == email)
            .cloned()
    }

    fn emails(&self) -> Vec<String> {
        self.accounts
            .lock()
            .unwrap()
            .iter()
            .map(|a| a.email.clone())
            .collect()
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

    /// Remove every imported account matching `email` and persist. Returns whether any
    /// were present. The Codex twin of [`crate::claude::ClaudeStore::delete`].
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
    if file
        .openai_api_key
        .as_deref()
        .is_some_and(|k| !k.is_empty())
    {
        bail!("this clone is signed in to Codex with an API key, not a ChatGPT subscription");
    }
    let tokens = file
        .tokens
        .context("~/.codex/auth.json has no tokens block (not signed in?)")?;
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
    Ok(CodexAuth {
        email,
        plan,
        account_id,
        id_token,
        access_token,
        refresh_token,
    })
}

/// Confirm clone `host` is signed in to Codex via ChatGPT and return its identity +
/// tokens. Reads `~/.codex/auth.json` (codex has no clean JSON `login status`).
pub async fn check_clone_auth(app: &App, host: &Host) -> Result<CodexAuth> {
    if !host.managed {
        bail!(
            "host '{}' is not a managed clone; only clones can be imported",
            host.id
        );
    }
    let raw = run_clone_op(app, &host.id, IMPORT_SCRIPT, "status", &[]).await?;
    parse_codex_auth(&raw)
        .map_err(|e| anyhow::anyhow!("{e} — is codex installed and signed in on '{}'?", host.id))
}

/// Import a Codex account from a signed-in clone: harvest the OAuth triple, upsert into
/// the 0600 store (by id), then delete the clone's auth.json so its CLI can't rotate the
/// refresh token the server now owns.
pub async fn import_clone_account(app: &App, host: &Host) -> Result<ImportResult> {
    if !host.managed {
        bail!(
            "host '{}' is not a managed clone; only clones can be imported",
            host.id
        );
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
    tracing::info!(
        "imported Codex account {} from '{}' (cleared={cleared})",
        auth.email,
        host.id
    );
    Ok(ImportResult {
        email: auth.email,
        cleared,
    })
}

// --- token refresh + push -------------------------------------------------

fn is_expired(expires_at: i64) -> bool {
    now_ms() + REFRESH_LEAD_MS >= expires_at
}

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
        bail!(
            "codex token apply produced unexpected output: {}",
            out.trim()
        );
    }
}

/// Remove clone `host_id`'s `~/.codex/auth.json`, leaving it with no Codex token.
pub async fn clear_clone_token(app: &App, host_id: &str) -> Result<()> {
    let out = run_clone_op(app, host_id, IMPORT_SCRIPT, "clear", &[]).await?;
    if out.contains("CLEARED") {
        Ok(())
    } else {
        bail!(
            "codex token clear produced unexpected output: {}",
            out.trim()
        );
    }
}

/// Refresh-if-needed and install `email`'s tokens into clone `host_id`, recording the
/// push. If the refresh rotated the token, fan it out to the account's other clones.
pub async fn push_account_to_clone(app: &App, host_id: &str, email: &str) -> Result<()> {
    let (acct, rotated) = fresh_access_token(app, email).await?;
    apply_clone_token(app, host_id, &acct).await?;
    app.codex
        .pushed
        .lock()
        .unwrap()
        .insert(host_id.to_string(), acct.access_token.clone());
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
        let Some(email) = host.codex_account_email.as_deref() else {
            continue;
        };
        if !host.managed {
            continue;
        }
        let Some(acct) = app.codex.get_by_email(email) else {
            continue;
        };
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
                app.codex
                    .pushed
                    .lock()
                    .unwrap()
                    .insert(host.id.clone(), acct.access_token.clone());
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
    let all_capped = facts.iter().all(|f| f.seven_pct > SEVEN_DAY_CAP_PCT);
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

// --- scoring + assignment (mirrors claude.rs) -----------------------------

const AUTO: &str = "auto";
const NONE: &str = "none";

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
            let five = u
                .and_then(|u| u.five_hour.as_ref())
                .map(|w| w.pct)
                .unwrap_or(0.0);
            let seven = u
                .and_then(|u| u.seven_day.as_ref())
                .map(|w| w.pct)
                .unwrap_or(0.0);
            let headroom = clamp01((100.0 - five) / 100.0);
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
    AutoPending,
    None,
}

/// `current` is the clone's account now (for a swap); resolving a group makes the pick
/// sticky — a clone moving from a pinned account into a group that already contains it
/// keeps that account instead of cold-starting. Pass `None` for a fresh clone at create.
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

fn seven_day_pct(app: &App, email: &str) -> f64 {
    app.store
        .get()
        .claude_accounts
        .iter()
        .filter(|u| u.provider == Some(wire::Provider::Codex))
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
    let known = app.codex.emails();
    let st = app.store.get();
    members
        .iter()
        .filter(|email| known.iter().any(|k| &k == email))
        .map(|email| {
            let usage = st
                .claude_accounts
                .iter()
                .filter(|u| u.provider == Some(wire::Provider::Codex))
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

fn is_exhausted(five: f64, seven: f64) -> bool {
    (100.0 - five) < SESSION_HEADROOM_PCT || seven >= SEVEN_DAY_CAP_PCT
}

fn exhausted(app: &App, email: &str) -> bool {
    is_exhausted(five_hour_pct(app, email), seven_day_pct(app, email))
}

fn eligible_members(app: &App, members: &[String]) -> Vec<String> {
    let known = app.codex.emails();
    members
        .iter()
        .filter(|email| known.iter().any(|k| &k == email))
        .filter(|email| !exhausted(app, email))
        .cloned()
        .collect()
}

fn eligible_group_accounts(app: &App, group: &CloneGroup) -> Vec<String> {
    eligible_members(app, &group.accounts)
}

/// Stickiness first (see the Claude twin): if the clone's `current` account is an
/// eligible member of the group, keep it instead of rebalancing off it. Otherwise pick
/// the least-loaded / least-used eligible member.
fn pick_group_account(app: &App, group_name: &str, current: Option<&str>) -> Option<String> {
    let cfg = app.config();
    let group = cfg.codex_groups.iter().find(|g| g.name == group_name)?;
    let counts = clone_counts(app);
    let mut pool = eligible_group_accounts(app, group);
    if let Some(cur) = current {
        if pool.iter().any(|e| e == cur) {
            return Some(cur.to_string());
        }
    }
    if pool.is_empty() {
        return best_saturated_email(&rotation_candidates(app, &group.accounts), &counts);
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
        let current = clone.codex_account_email.as_ref().and_then(|email| {
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

async fn rotate_pool(app: &App, label: &str, members: &[String], clones: &[Host]) {
    let candidates = rotation_candidates(app, members);
    if candidates.is_empty() {
        tracing::info!(
            "codex rotate: pool '{label}' has no imported account; leaving {} clone(s)",
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
            "codex rotate: pool '{label}' has no under-cap account; using saturated fallback for {} clone(s)",
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
        if host.codex_account_email.as_deref() == Some(email.as_str()) {
            continue;
        }
        match push_account_to_clone(app, &host.id, &email).await {
            Ok(()) => {
                tracing::info!(
                    "codex rotate[{label}]: {} {} -> {}",
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
            Err(e) => tracing::warn!(
                "codex rotate[{label}]: applying {email} to {} failed: {e}",
                host.id
            ),
        }
        tokio::time::sleep(STAGGER).await;
    }
}

fn auto_pool_clones(hosts: &[Host]) -> Vec<Host> {
    hosts
        .iter()
        .filter(|h| {
            h.managed && h.codex_group.is_none() && h.codex_selection.as_deref() == Some(AUTO)
        })
        .cloned()
        .collect()
}

pub async fn rotate_once(app: &App) {
    let cfg = app.config();
    let hosts = app.store.get().hosts;
    let mut by_group: HashMap<String, Vec<Host>> = HashMap::new();
    for h in &hosts {
        if let (Some(g), true) = (&h.codex_group, h.managed) {
            by_group.entry(g.clone()).or_default().push(h.clone());
        }
    }
    for (gname, clones) in by_group {
        let Some(group) = cfg.codex_groups.iter().find(|g| g.name == gname) else {
            continue;
        };
        rotate_pool(app, &gname, &group.accounts, &clones).await;
    }
    let auto = auto_pool_clones(&hosts);
    if !auto.is_empty() {
        rotate_pool(app, "auto", &app.codex.emails(), &auto).await;
    }
}

/// Delete an imported Codex account by email, then heal the fleet — the Codex twin of
/// [`crate::claude::delete_account`]. Refuses if any clone is pinned to it; otherwise
/// removes the token and reassigns auto/group clones off it via [`rotate_once`], clearing
/// any dangling reference that couldn't be re-placed. Returns the ids of moved clones.
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
    if !app.codex.delete(email)? {
        bail!("no imported Codex account '{email}'");
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
    rotate_once(app).await;

    app.store.mutate(|s| {
        for h in &mut s.hosts {
            if h.codex_account_email.as_deref() == Some(email) {
                h.codex_account_email = None;
            }
        }
    });
    Ok(on_it)
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
    let mut fleet: Vec<FleetFacts> = Vec::with_capacity(accts.len());

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
                return Ok::<_, anyhow::Error>((b, None));
            }
            let raw = fetch_usage(&app.http, &fresh.access_token, &fresh.account_id).await?;
            let facts = gate_facts(&acct.id, &raw); // borrow before `raw` moves into to_usage
            Ok::<_, anyhow::Error>((to_usage(acct, raw), facts))
        }
        .await;
        match outcome {
            Ok((u, facts)) => {
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
                                let u2 = to_usage(&acct, raw2);
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

    crate::clone_ops::replace_provider_views(
        app,
        wire::Provider::Codex,
        views,
        cfg.codex.pinned_email.as_deref(),
    );

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

    fn jwt_with(payload: &str) -> String {
        let b64 = crate::provision::b64_encode(payload.as_bytes());
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

    fn clone_host(id: &str, cur: Option<&str>) -> Host {
        Host {
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
            let got = assign_rotation(&clones, &eligible, &HashMap::new());
            let by_id: HashMap<_, _> = got.iter().map(|(h, e)| (h.id.clone(), e.clone())).collect();
            assert_eq!(by_id["c1"], "a@o");
            assert_eq!(by_id["c2"], "b@o");
        }
    }

    #[test]
    fn assignment_distinct_when_enough_accounts() {
        let eligible = ["a@o".to_string(), "b@o".to_string(), "c@o".to_string()];
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
            assert_eq!(emails.len(), 3);
        }
    }

    fn codex_rotation_candidate(
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
    fn codex_saturated_assignment_prefers_soonest_5h_reset() {
        let candidates = [
            codex_rotation_candidate("soon@o", 97.0, 96.0, Some(1_000), Some(10_000)),
            codex_rotation_candidate("late@o", 90.0, 96.0, Some(2_000), Some(10_000)),
        ];
        let clones = [clone_host("c1", Some("late@o"))];

        let got = assign_saturated_rotation(&clones, &candidates);

        assert_eq!(got[0].1, "soon@o");
    }

    #[test]
    fn codex_saturated_assignment_uses_lower_5h_when_resets_are_missing() {
        let candidates = [
            codex_rotation_candidate("hot@o", 98.0, 96.0, None, Some(10_000)),
            codex_rotation_candidate("cool@o", 90.0, 96.0, None, Some(10_000)),
        ];
        let clones = [clone_host("c1", Some("hot@o"))];

        let got = assign_saturated_rotation(&clones, &candidates);

        assert_eq!(got[0].1, "cool@o");
    }

    #[test]
    fn codex_saturated_assignment_keeps_current_within_reset_margin() {
        let candidates = [
            codex_rotation_candidate("current@o", 98.0, 96.0, Some(1_800), Some(10_000)),
            codex_rotation_candidate("best@o", 99.0, 96.0, Some(1_000), Some(10_000)),
        ];
        let clones = [clone_host("c1", Some("current@o"))];

        let got = assign_saturated_rotation(&clones, &candidates);

        assert_eq!(got[0].1, "current@o");
    }

    #[test]
    fn codex_saturated_assignment_moves_missing_reset_current_to_known_reset() {
        let candidates = [
            codex_rotation_candidate("unknown@o", 90.0, 96.0, None, Some(10_000)),
            codex_rotation_candidate("known@o", 94.0, 96.0, Some(1_000), Some(10_000)),
        ];
        let clones = [clone_host("c1", Some("unknown@o"))];

        let got = assign_saturated_rotation(&clones, &candidates);

        assert_eq!(got[0].1, "known@o");
    }

    #[test]
    fn codex_exhaustion_threshold_is_80_5h_or_95_7d() {
        assert!(!is_exhausted(80.0, 0.0));
        assert!(is_exhausted(80.1, 0.0));
        assert!(!is_exhausted(0.0, 94.9));
        assert!(is_exhausted(0.0, 95.0));
    }

    fn host_sel(id: &str, managed: bool, group: Option<&str>, sel: Option<&str>) -> Host {
        Host {
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
    }

    #[test]
    fn to_usage_absent_reset_credits_is_none() {
        let raw: RawUsage = serde_json::from_str(r#"{"rate_limit":{}}"#).unwrap();
        assert_eq!(to_usage(&sample_account(), raw).reset_credits, None);
    }

    #[test]
    fn codex_auto_pool_is_only_managed_ungrouped_auto_clones() {
        let hosts = vec![
            host_sel("auto1", true, None, Some("auto")),
            host_sel("pinned", true, None, Some("me@x")),
            host_sel("legacy", true, None, None),
            host_sel("grouped", true, Some("g"), Some("auto")),
            host_sel("stopped", false, None, Some("auto")),
        ];
        let picked: Vec<String> = auto_pool_clones(&hosts).into_iter().map(|h| h.id).collect();
        assert_eq!(picked, vec!["auto1"]);
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

    #[test]
    fn request_id_is_nonempty_and_varies() {
        let a = new_request_id();
        let b = new_request_id();
        assert_eq!(a.len(), 32);
        assert_ne!(a, b);
    }
}
