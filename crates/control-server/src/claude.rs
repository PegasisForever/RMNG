//! Claude accounts — usage tracking + clone assignment/swap. Ports
//! `claude-accounts.server.ts` + `clone-accounts.server.ts`.
//!
//! Two-token model (per the rmng design): the **refresh** token (+ a cached
//! short-lived access token, in the 0600 secret store `claude-accounts.json`) is
//! used *only* to read 5h/7d usage; the **long-lived** token (config
//! `cloneAccounts`) runs Claude Code in a clone and is installed by writing the
//! clone's `~/.claude/.credentials.json` (see [`apply_clone_token`]). The poller
//! publishes a token-free `ClaudeUsage` view onto `ControlState.claudeAccounts`,
//! and (when enabled) auto-swaps a clone whose account is exhausted.
//! (Codex accounts are out of scope here — TODO if needed.)

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use wire::{ClaudeSpend, ClaudeUsage, ClaudeUsageWindow, CloneAccount, Host};

use crate::app::App;

const USAGE_URL: &str = "https://api.anthropic.com/api/oauth/usage";
const OAUTH_TOKEN_URL: &str = "https://platform.claude.com/v1/oauth/token";
const OAUTH_CLIENT_ID: &str = "9d1c250a-e61b-44d9-88ed-5944d1962f5e";
const OAUTH_BETA_HEADER: &str = "oauth-2025-04-20";
const USER_AGENT: &str = "claude-swap/1.0";
const EXPIRY_BUFFER_MS: i64 = 5 * 60 * 1000;
const FETCH_TIMEOUT: Duration = Duration::from_secs(10);
const STAGGER: Duration = Duration::from_millis(400);

// scoring knobs (clone-accounts.server.ts)
const SESSION_HEADROOM_PCT: f64 = 40.0;
const SEVEN_DAY_CAP_PCT: f64 = 95.0;
const APPLY_CREDENTIALS_SCRIPT: &str = include_str!("../scripts/apply-credentials.sh");

fn now_ms() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StoredClaudeAccount {
    pub id: String,
    pub num: u32,
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
}

impl ClaudeStore {
    pub fn load(data_dir: &str) -> Self {
        let path = std::env::var_os("KASM_CLAUDE_ACCOUNTS_FILE")
            .map(PathBuf::from)
            .unwrap_or_else(|| Path::new(data_dir).join("claude-accounts.json"));
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
}

fn b64_decode(s: &str) -> Option<Vec<u8>> {
    let mut table = [255u8; 256];
    for (i, c) in b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/".iter().enumerate() {
        table[*c as usize] = i as u8;
    }
    let bytes: Vec<u8> = s.bytes().filter(|&b| b != b'=' && !b.is_ascii_whitespace()).collect();
    let mut out = Vec::with_capacity(bytes.len() / 4 * 3);
    for chunk in bytes.chunks(4) {
        let mut n = 0u32;
        let mut bits = 0;
        for &c in chunk {
            let v = table[c as usize];
            if v == 255 {
                return None;
            }
            n = (n << 6) | v as u32;
            bits += 6;
        }
        n <<= 24 - bits;
        for i in 0..(bits / 8) {
            out.push((n >> (16 - i * 8)) as u8);
        }
    }
    Some(out)
}

// --- import from claude-swap ----------------------------------------------

#[derive(Deserialize)]
struct SwapSeqAccount {
    email: String,
    #[serde(default)]
    organization_uuid: Option<String>,
    #[serde(default)]
    organization_name: Option<String>,
}

#[derive(Deserialize)]
struct SwapSequence {
    #[serde(default)]
    active_account_number: Option<u32>,
    #[serde(default)]
    accounts: HashMap<String, SwapSeqAccount>,
}

#[derive(Deserialize)]
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

fn find_cred_file(cred_dir: &Path, num: u32) -> Option<PathBuf> {
    let prefix = format!(".creds-{num}-");
    std::fs::read_dir(cred_dir).ok()?.flatten().find_map(|e| {
        let name = e.file_name();
        let name = name.to_string_lossy();
        (name.starts_with(&prefix) && name.ends_with(".enc")).then(|| e.path())
    })
}

/// Import accounts (incl. OAuth tokens) from claude-swap's data dir. Upserts by id.
pub fn import_from_swap(app: &App) -> Result<usize> {
    let cfg = app.config();
    let base = Path::new(&cfg.data_dir)
        .join("hosts")
        .join(&cfg.claude.template_host_id)
        .join(&cfg.claude.swap_data_subpath);
    let seq: SwapSequence = serde_json::from_str(
        &std::fs::read_to_string(base.join("sequence.json"))
            .with_context(|| format!("reading {}", base.join("sequence.json").display()))?,
    )?;
    let cred_dir = base.join("credentials");

    let mut imported = Vec::new();
    for (num_str, meta) in &seq.accounts {
        let Ok(num) = num_str.parse::<u32>() else { continue };
        let Some(cred_path) = find_cred_file(&cred_dir, num) else {
            tracing::warn!("claude import: no creds file for {} (#{num})", meta.email);
            continue;
        };
        let Some(creds) = std::fs::read_to_string(&cred_path)
            .ok()
            .and_then(|b64| b64_decode(b64.trim()))
            .and_then(|raw| serde_json::from_slice::<ClaudeCreds>(&raw).ok())
            .and_then(|c| c.claude_ai_oauth)
        else {
            tracing::warn!("claude import: bad creds for {} (#{num})", meta.email);
            continue;
        };
        let (Some(access), Some(refresh)) = (creds.access_token, creds.refresh_token) else {
            tracing::warn!("claude import: missing tokens for {} (#{num})", meta.email);
            continue;
        };
        let org_uuid = meta.organization_uuid.clone().unwrap_or_default();
        imported.push(StoredClaudeAccount {
            id: format!("{}|{}", meta.email, org_uuid),
            num,
            email: meta.email.clone(),
            org_uuid,
            org_name: meta.organization_name.clone().unwrap_or_default(),
            active: seq.active_account_number == Some(num),
            access_token: access,
            refresh_token: refresh,
            expires_at: creds.expires_at.unwrap_or(0),
            scopes: creds.scopes.unwrap_or_default(),
        });
    }

    let n = imported.len();
    let mut accts = app.claude.accounts.lock().unwrap();
    let mut by_id: HashMap<String, StoredClaudeAccount> =
        accts.drain(..).map(|a| (a.id.clone(), a)).collect();
    let active_id = imported.iter().find(|a| a.active).map(|a| a.id.clone());
    for a in imported {
        by_id.insert(a.id.clone(), a);
    }
    let mut next: Vec<_> = by_id.into_values().collect();
    if let Some(active) = &active_id {
        for a in &mut next {
            a.active = &a.id == active;
        }
    }
    next.sort_by(|a, b| a.email.cmp(&b.email));
    app.claude.save(&next)?;
    *accts = next;
    Ok(n)
}

// --- token refresh + usage fetch ------------------------------------------

fn is_expired(expires_at: i64) -> bool {
    now_ms() + EXPIRY_BUFFER_MS >= expires_at
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

/// Refresh `acct`'s access token if near expiry (rotates the single-use refresh
/// token). Returns the fresh token; mutates `acct` in place (caller persists).
async fn ensure_fresh_token(http: &reqwest::Client, acct: &mut StoredClaudeAccount) -> Result<String> {
    if !is_expired(acct.expires_at) {
        return Ok(acct.access_token.clone());
    }
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
    acct.access_token = data.access_token.clone();
    acct.expires_at = now_ms() + data.expires_in * 1000;
    if let Some(r) = data.refresh_token {
        acct.refresh_token = r;
    }
    if let Some(s) = data.scope {
        acct.scopes = s.split(' ').map(str::to_string).collect();
    }
    Ok(data.access_token)
}

fn snippet(s: &str) -> String {
    if s.is_empty() { String::new() } else { format!(": {}", &s[..s.len().min(120)]) }
}

#[derive(Deserialize)]
struct RawWindow {
    #[serde(default)]
    utilization: f64,
    #[serde(default)]
    resets_at: Option<String>,
}
#[derive(Deserialize)]
struct RawExtra {
    #[serde(default)]
    is_enabled: bool,
    #[serde(default)]
    used_credits: i64,
    #[serde(default)]
    monthly_limit: Option<i64>,
    #[serde(default)]
    utilization: f64,
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
    w.map(|w| ClaudeUsageWindow { pct: w.utilization.round(), resets_at: w.resets_at })
}

fn to_usage(acct: &StoredClaudeAccount, raw: RawUsage) -> ClaudeUsage {
    let spend = raw.extra_usage.filter(|e| e.is_enabled).map(|e| ClaudeSpend {
        used_cents: e.used_credits,
        limit_cents: e.monthly_limit,
        pct: e.utilization.round(),
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
    let mut accts = app.claude.snapshot();
    if accts.is_empty() {
        app.store.mutate(|s| s.claude_accounts.clear());
        return Ok(false);
    }

    let mut any429 = false;
    let mut views = Vec::with_capacity(accts.len());
    let mut dirty = false;

    for (i, acct) in accts.iter_mut().enumerate() {
        if i > 0 {
            tokio::time::sleep(STAGGER).await;
        }
        let before = (acct.access_token.clone(), acct.expires_at);
        let outcome = async {
            let token = ensure_fresh_token(&app.http, acct).await?;
            let raw = fetch_usage(&app.http, &token).await?;
            Ok::<_, anyhow::Error>(to_usage(acct, raw))
        }
        .await;
        if (acct.access_token.clone(), acct.expires_at) != before {
            dirty = true; // token rotated
        }
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

    if dirty {
        // Persist rotated (single-use) tokens immediately.
        let mut store = app.claude.accounts.lock().unwrap();
        *store = accts.clone();
        let _ = app.claude.save(&store);
    }

    // Flag assignable accounts (those with a long-lived clone token in config).
    let cfg = app.config();
    let assignable: std::collections::HashSet<String> =
        cfg.clone_accounts.iter().map(|a| a.email.clone()).collect();
    for v in &mut views {
        if assignable.contains(&v.email) {
            v.assignable = Some(true);
        }
    }

    // Pinned email first, then alphabetical.
    let pinned = cfg.claude.pinned_email.clone();
    views.sort_by(|a, b| {
        let ap = Some(&a.email) == pinned.as_ref();
        let bp = Some(&b.email) == pinned.as_ref();
        if ap != bp {
            return if ap { std::cmp::Ordering::Less } else { std::cmp::Ordering::Greater };
        }
        a.email.cmp(&b.email)
    });
    app.store.mutate(|s| s.claude_accounts = views);

    if cfg.claude.auto_swap_on_exhaustion {
        auto_swap_exhausted(app).await;
    }
    Ok(any429)
}

// --- scoring + assignment (clone-accounts.server.ts) ----------------------

const AUTO: &str = "auto";

struct Scored {
    account: CloneAccount,
    score: f64,
    eligible: bool,
}

fn clamp01(n: f64) -> f64 {
    n.clamp(0.0, 1.0)
}

fn score_accounts(app: &App) -> Vec<Scored> {
    let st = app.store.get();
    let cfg = app.config();
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
    cfg.clone_accounts
        .iter()
        .map(|account| {
            let u = usage.get(account.email.as_str());
            let five = u.and_then(|u| u.five_hour.as_ref()).map(|w| w.pct).unwrap_or(0.0);
            let seven = u.and_then(|u| u.seven_day.as_ref()).map(|w| w.pct).unwrap_or(0.0);
            let headroom = clamp01((100.0 - five) / 100.0);
            // reset-soon term omitted (ISO reset parsing TODO) → 0.
            let n = *clones.get(account.email.as_str()).unwrap_or(&0) as f64;
            let score = headroom - 0.5 * n;
            let eligible = (100.0 - five >= SESSION_HEADROOM_PCT) && seven < SEVEN_DAY_CAP_PCT;
            Scored { account: account.clone(), score, eligible }
        })
        .collect()
}

fn best_scored(app: &App) -> Option<CloneAccount> {
    let scored = score_accounts(app);
    if scored.is_empty() {
        return None;
    }
    let mut pool: Vec<&Scored> = scored.iter().filter(|s| s.eligible).collect();
    if pool.is_empty() {
        pool = scored.iter().collect();
    }
    pool.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));
    pool.first().map(|s| s.account.clone())
}

/// The recommended account for a new clone (or None if none configured).
pub fn recommend(app: &App) -> Option<CloneAccount> {
    best_scored(app)
}

/// Resolve a clone request's account selection to a concrete account.
pub fn resolve_clone_account(app: &App, requested: Option<&str>) -> Option<CloneAccount> {
    let cfg = app.config();
    if cfg.clone_accounts.is_empty() {
        return None;
    }
    let want = requested.unwrap_or("").trim();
    if !want.is_empty() && want != AUTO {
        if let Some(hit) = cfg.clone_accounts.iter().find(|a| a.email == want) {
            return Some(hit.clone());
        }
        tracing::warn!("clone account '{want}' not configured; using recommended");
    }
    best_scored(app)
}

/// Install a long-lived token into a clone's `~/.claude/.credentials.json` over
/// SSH (hot-swaps a running clone). Best-effort; errors are returned to log.
pub async fn apply_clone_token(host: &Host, token: &str) -> Result<()> {
    let target = format!("{}@{}", host.username, host.host);
    let remote = format!("bash -s -- '{}'", token.replace('\'', r"'\''"));
    let mut child = tokio::process::Command::new("ssh")
        .args([
            "-o", "BatchMode=yes",
            "-o", "StrictHostKeyChecking=accept-new",
            "-o", "ConnectTimeout=15",
            &target, &remote,
        ])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()?;
    use tokio::io::AsyncWriteExt;
    child.stdin.take().unwrap().write_all(APPLY_CREDENTIALS_SCRIPT.as_bytes()).await?;
    let out = child.wait_with_output().await?;
    let stdout = String::from_utf8_lossy(&out.stdout);
    if out.status.success() && stdout.contains("OK") {
        Ok(())
    } else {
        let tail = String::from_utf8_lossy(&out.stderr);
        bail!("token apply failed (exit {:?}): {}", out.status.code(), tail.trim());
    }
}

/// When a clone's assigned account is exhausted, hot-swap it to the best alternative.
async fn auto_swap_exhausted(app: &App) {
    let st = app.store.get();
    let usage: HashMap<String, &ClaudeUsage> =
        st.claude_accounts.iter().map(|u| (u.email.clone(), u)).collect();
    let exhausted = |email: &str| -> bool {
        usage.get(email).is_some_and(|u| {
            let five = u.five_hour.as_ref().map(|w| w.pct).unwrap_or(0.0);
            let seven = u.seven_day.as_ref().map(|w| w.pct).unwrap_or(0.0);
            (100.0 - five) < SESSION_HEADROOM_PCT || seven >= SEVEN_DAY_CAP_PCT
        })
    };
    for host in &st.hosts {
        let Some(cur) = &host.claude_account_email else { continue };
        if !exhausted(cur) {
            continue;
        }
        let Some(next) = best_scored(app) else { continue };
        if &next.email == cur || exhausted(&next.email) {
            continue; // no better option
        }
        match apply_clone_token(host, &next.long_lived_token).await {
            Ok(()) => {
                tracing::info!("auto-swapped {} from {cur} to {}", host.id, next.email);
                let id = host.id.clone();
                let email = next.email.clone();
                app.store.mutate(|s| {
                    if let Some(h) = s.hosts.iter_mut().find(|h| h.id == id) {
                        h.claude_account_email = Some(email);
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
