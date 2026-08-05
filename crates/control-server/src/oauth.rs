//! Import an agent account by signing in here, with no clone in the loop.
//!
//! This is the only way in. The one it replaced signed a clone in, then had this server
//! read the credentials off that clone's disk and take ownership, which needed a running
//! clone, a provider CLI inside it, and an operator willing to log in twice. This gets the
//! same OAuth pair directly.
//!
//! **Why it is a paste and not a redirect.** Both providers pin their redirect URI to a
//! port on `localhost`, and neither accepts another:
//!
//! | provider | redirect |
//! |---|---|
//! | Claude | `http://localhost:54545/callback` |
//! | Codex | `http://localhost:1455/auth/callback` |
//!
//! That `localhost` is the operator's own machine, never this server, so there is nothing
//! here for the browser to land on. The browser lands on a dead port instead, and the
//! address bar it lands on carries the authorization code. Pasting that back is the whole
//! transport. It costs one copy and needs no tunnel, no port on the operator's machine and
//! no inbound path to this server.
//!
//! Everything after the exchange is the existing store: the same `StoredClaudeAccount` and
//! `StoredCodexAccount` records, refreshed and rotated by the same code that has always
//! handled them. A caller cannot tell which way an account arrived, which is the point.
//!
//! The client ids and token endpoints below are the ones this server already refreshes
//! against ([`crate::claude`], [`crate::codex`]), quoted here so the flow reads in one
//! place. They belong to the provider CLIs, not to RMNG.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use serde::Deserialize;
use sha2::{Digest, Sha256};

use crate::app::App;
use crate::clone_ops::jwt_claims;

/// Claude's authorization endpoint, and the redirect it will not let us change.
const CLAUDE_AUTH_URL: &str = "https://claude.ai/oauth/authorize";
const CLAUDE_TOKEN_URL: &str = "https://platform.claude.com/v1/oauth/token";
const CLAUDE_PROFILE_URL: &str = "https://api.anthropic.com/api/oauth/profile";
const CLAUDE_CLIENT_ID: &str = "9d1c250a-e61b-44d9-88ed-5944d1962f5e";
const CLAUDE_REDIRECT: &str = "http://localhost:54545/callback";
const CLAUDE_SCOPE: &str =
    "user:profile user:inference user:sessions:claude_code user:mcp_servers user:file_upload";

const CODEX_AUTH_URL: &str = "https://auth.openai.com/oauth/authorize";
const CODEX_TOKEN_URL: &str = "https://auth.openai.com/oauth/token";
const CODEX_CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";
const CODEX_REDIRECT: &str = "http://localhost:1455/auth/callback";
const CODEX_SCOPE: &str = "openid email profile offline_access";

/// How long a started login stays completable. Long enough for a password manager, a
/// second factor and a moment of confusion; short enough that an abandoned verifier does
/// not sit in memory all day.
const PENDING_TTL: Duration = Duration::from_secs(15 * 60);

/// Bounded so a caller hammering `begin` cannot grow this without limit. Well past any
/// real number of logins in flight at once.
const MAX_PENDING: usize = 32;

const EXCHANGE_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Provider {
    Claude,
    Codex,
}

impl Provider {
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "claude" => Some(Self::Claude),
            "codex" => Some(Self::Codex),
            _ => None,
        }
    }

    fn redirect(self) -> &'static str {
        match self {
            Self::Claude => CLAUDE_REDIRECT,
            Self::Codex => CODEX_REDIRECT,
        }
    }
}

/// One login waiting for its code. Holds the PKCE verifier, which is the secret that makes
/// the pasted code usable by this server and nobody else.
struct Pending {
    provider: Provider,
    verifier: String,
    started_ms: i64,
}

/// Logins in flight, keyed by the `state` that will come back in the callback URL.
#[derive(Default)]
pub struct Logins {
    pending: Mutex<HashMap<String, Pending>>,
}

impl Logins {
    pub fn new() -> Self {
        Self::default()
    }
}

/// Random bytes from `/dev/urandom`, with no fall back to the clock.
///
/// The PKCE verifier is the only thing stopping a leaked authorization code from being
/// redeemed by whoever leaked it, so a predictable one is worse than a failed login.
fn rand_bytes(n: usize) -> Result<Vec<u8>> {
    use std::io::Read;
    let mut buf = vec![0u8; n];
    std::fs::File::open("/dev/urandom")
        .and_then(|mut f| f.read_exact(&mut buf))
        .context("reading /dev/urandom for an OAuth secret")?;
    Ok(buf)
}

/// base64url without padding, which is what both PKCE and JWT use.
fn b64url(bytes: &[u8]) -> String {
    use base64::Engine;
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

/// A PKCE pair: the verifier this server keeps, and the S256 challenge the provider gets.
fn pkce() -> Result<(String, String)> {
    let verifier = b64url(&rand_bytes(96)?);
    let challenge = b64url(&Sha256::digest(verifier.as_bytes()));
    Ok((verifier, challenge))
}

/// Percent-encode one query value. Hand-rolled against RFC 3986's unreserved set rather
/// than pulling in a URL crate for four call sites: everything outside it is escaped, so
/// the scope's spaces and the challenge's `-`/`_` both survive the round trip.
fn enc(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                out.push(b as char)
            }
            other => out.push_str(&format!("%{other:02X}")),
        }
    }
    out
}

/// Start a login: the URL to open, and the `state` that ties the paste back to it.
///
/// The verifier is held here until [`complete`] or the TTL, whichever comes first.
pub fn begin(app: &App, provider: Provider) -> Result<String> {
    let (verifier, challenge) = pkce()?;
    let state = b64url(&rand_bytes(24)?);

    let url = match provider {
        Provider::Claude => format!(
            "{CLAUDE_AUTH_URL}?code=true&client_id={}&response_type=code&redirect_uri={}\
             &scope={}&code_challenge={}&code_challenge_method=S256&state={}",
            enc(CLAUDE_CLIENT_ID),
            enc(CLAUDE_REDIRECT),
            enc(CLAUDE_SCOPE),
            enc(&challenge),
            enc(&state),
        ),
        Provider::Codex => format!(
            "{CODEX_AUTH_URL}?client_id={}&response_type=code&redirect_uri={}&scope={}\
             &state={}&code_challenge={}&code_challenge_method=S256&prompt=login\
             &id_token_add_organizations=true&codex_cli_simplified_flow=true",
            enc(CODEX_CLIENT_ID),
            enc(CODEX_REDIRECT),
            enc(CODEX_SCOPE),
            enc(&state),
            enc(&challenge),
        ),
    };

    let mut pending = app.logins.pending.lock().unwrap();
    let now = crate::clone_ops::now_ms();
    pending.retain(|_, p| now - p.started_ms < PENDING_TTL.as_millis() as i64);
    if pending.len() >= MAX_PENDING {
        bail!("too many sign-ins already waiting; finish or abandon one and try again");
    }
    pending.insert(state, Pending { provider, verifier, started_ms: now });
    Ok(url)
}

/// The `code` and `state` out of whatever the operator pasted.
///
/// Takes the whole callback URL, a bare query string, or just the pair, because all three
/// are things a person reasonably copies out of an address bar. Claude appends its own
/// state to the code with a `#`, which is split off here rather than sent to the provider.
pub fn parse_callback(pasted: &str) -> Result<(String, Option<String>)> {
    let trimmed = pasted.trim();
    if trimmed.is_empty() {
        bail!("nothing pasted");
    }
    let query = trimmed
        .split_once('?')
        .map(|(_, q)| q)
        .unwrap_or(trimmed)
        .split('#')
        .next()
        .unwrap_or_default();

    let mut code = String::new();
    let mut state = None;
    for pair in query.split('&') {
        match pair.split_once('=') {
            Some(("code", v)) => code = url_decode(v),
            Some(("state", v)) => state = Some(url_decode(v)),
            Some(("error", v)) => bail!("the provider refused the sign-in: {}", url_decode(v)),
            _ => {}
        }
    }
    // Claude's callback carries `code=<code>#<state>`. The fragment half is its state, and
    // the code is only the part before it.
    if let Some((head, tail)) = code.clone().split_once('#') {
        code = head.to_string();
        if state.is_none() && !tail.is_empty() {
            state = Some(tail.to_string());
        }
    }
    if code.is_empty() {
        bail!("that URL carries no `code=` parameter");
    }
    Ok((code, state))
}

/// Percent-decode, leaving anything malformed as it stands rather than failing: a code the
/// provider will reject is a better error than one this parser invents.
fn url_decode(s: &str) -> String {
    let bytes = s.replace('+', " ").into_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let Ok(v) = u8::from_str_radix(&String::from_utf8_lossy(&bytes[i + 1..i + 3]), 16) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[derive(Debug, Deserialize)]
struct TokenResp {
    access_token: String,
    #[serde(default)]
    refresh_token: String,
    #[serde(default)]
    id_token: String,
    #[serde(default)]
    expires_in: i64,
    #[serde(default)]
    scope: Option<String>,
}

/// Finish a login and store the account, optionally joining it to a pool. Returns the email.
///
/// The pool is part of the import because an account nobody put in one is an account no
/// clone will ever be handed by the rotator: it can still be pinned by name, but the pools
/// are how clones get accounts without anybody choosing.
pub async fn complete(
    app: &App,
    provider: Provider,
    pasted: &str,
    group: &str,
) -> Result<String> {
    let (code, state) = parse_callback(pasted)?;
    let state = state.context("that URL carries no `state` parameter")?;

    let (verifier, started) = {
        let mut pending = app.logins.pending.lock().unwrap();
        let entry = pending
            .remove(&state)
            .context("this sign-in has expired or was never started here; start it again")?;
        if entry.provider != provider {
            bail!("that callback belongs to a different provider's sign-in");
        }
        (entry.verifier, entry.started_ms)
    };
    if crate::clone_ops::now_ms() - started >= PENDING_TTL.as_millis() as i64 {
        bail!("this sign-in took too long; start it again");
    }

    let tokens = exchange(app, provider, &code, &verifier, &state).await?;
    if tokens.refresh_token.is_empty() {
        bail!("the provider returned no refresh token, so the account could not be kept");
    }
    let email = match provider {
        Provider::Claude => store_claude(app, tokens).await?,
        Provider::Codex => store_codex(app, tokens)?,
    };
    if !group.is_empty() {
        join_group(app, provider, &email, group)?;
    }
    Ok(email)
}

/// Add `email` to the named pool, leaving every other pool alone.
///
/// An unknown name is an error rather than a silently created pool: pools are config the
/// operator maintains, and inventing one here would put an account somewhere no clone is
/// bound to. Already being a member is not an error, and does not duplicate the entry.
fn join_group(app: &App, provider: Provider, email: &str, group: &str) -> Result<()> {
    let mut cfg = app.config();
    let pools = match provider {
        Provider::Claude => &mut cfg.clone_groups,
        Provider::Codex => &mut cfg.codex_groups,
    };
    add_to_pool(pools, email, group)?;
    crate::config::save(&cfg).context("saving the pool membership")?;
    *app.cfg.write().unwrap() = cfg;
    tracing::info!("added {email} to the {group} pool");
    Ok(())
}

/// Put `email` in the named pool. The decision, separated from reading and writing config so
/// it can be tested without a config file: `crate::config::save` writes a fixed relative
/// path, so a test that called it would drop a `config.json` in whatever directory it ran in.
fn add_to_pool(pools: &mut [wire::CloneGroup], email: &str, group: &str) -> Result<()> {
    let pool = pools
        .iter_mut()
        .find(|g| g.name == group)
        .with_context(|| format!("no pool named '{group}'"))?;
    if !pool.accounts.iter().any(|a| a == email) {
        pool.accounts.push(email.to_string());
    }
    Ok(())
}

/// Redeem the code. Claude takes JSON, Codex takes a form body, and each is what its own
/// CLI sends.
async fn exchange(
    app: &App,
    provider: Provider,
    code: &str,
    verifier: &str,
    state: &str,
) -> Result<TokenResp> {
    let redirect = provider.redirect();
    let req = match provider {
        Provider::Claude => app
            .http
            .post(CLAUDE_TOKEN_URL)
            .header("Content-Type", "application/json")
            .json(&serde_json::json!({
                "grant_type": "authorization_code",
                "code": code,
                "redirect_uri": redirect,
                "client_id": CLAUDE_CLIENT_ID,
                "code_verifier": verifier,
                "state": state,
            })),
        Provider::Codex => app.http.post(CODEX_TOKEN_URL).form(&[
            ("grant_type", "authorization_code"),
            ("code", code),
            ("redirect_uri", redirect),
            ("client_id", CODEX_CLIENT_ID),
            ("code_verifier", verifier),
        ]),
    };
    let resp = req
        .timeout(EXCHANGE_TIMEOUT)
        .send()
        .await
        .context("the token exchange never got a reply")?;
    let status = resp.status();
    let body = resp.text().await.unwrap_or_default();
    if !status.is_success() {
        bail!("the provider refused the code: {}{}", status.as_u16(), crate::clone_ops::snippet(&body));
    }
    serde_json::from_str::<TokenResp>(&body)
        .context("the provider's answer was not a token response")
}

/// Store a Claude account, asking the provider who it belongs to.
///
/// The email is not in the token response, and it is the account's identity everywhere in
/// this server, so the profile call is part of the import rather than an extra.
async fn store_claude(app: &App, tokens: TokenResp) -> Result<String> {
    #[derive(Deserialize)]
    struct Profile {
        #[serde(default)]
        account: ProfileAccount,
        #[serde(default)]
        organization: ProfileOrg,
    }
    #[derive(Default, Deserialize)]
    struct ProfileAccount {
        #[serde(default)]
        email: String,
    }
    #[derive(Default, Deserialize)]
    struct ProfileOrg {
        #[serde(default)]
        uuid: String,
        #[serde(default)]
        name: String,
    }

    let resp = app
        .http
        .get(CLAUDE_PROFILE_URL)
        .timeout(EXCHANGE_TIMEOUT)
        .header("Authorization", format!("Bearer {}", tokens.access_token))
        .header("Accept", "application/json")
        .send()
        .await
        .context("asking Anthropic whose account this is")?;
    let status = resp.status();
    let body = resp.text().await.unwrap_or_default();
    if !status.is_success() {
        bail!("the profile lookup failed: {}{}", status.as_u16(), crate::clone_ops::snippet(&body));
    }
    let profile: Profile =
        serde_json::from_str(&body).context("Anthropic's profile answer was not readable")?;
    if profile.account.email.is_empty() {
        bail!("Anthropic returned no email for this account");
    }

    let email = profile.account.email;
    let stored = crate::claude::StoredClaudeAccount {
        id: format!("{email}|{}", profile.organization.uuid),
        email: email.clone(),
        org_uuid: profile.organization.uuid,
        org_name: profile.organization.name,
        active: false,
        access_token: tokens.access_token,
        refresh_token: tokens.refresh_token,
        expires_at: crate::clone_ops::now_ms() + tokens.expires_in.max(0) * 1000,
        scopes: tokens
            .scope
            .unwrap_or_default()
            .split_whitespace()
            .map(str::to_string)
            .collect(),
        // A sign-in is not a refresh. The first one to run writes the record.
        last_refresh: None,
    };
    crate::claude::upsert_account(app, stored)?;
    tracing::info!("imported Claude account {email} by sign-in");
    Ok(email)
}

/// Store a Codex account. Its identity rides in the id token, the same claims the clone
/// import reads.
fn store_codex(app: &App, tokens: TokenResp) -> Result<String> {
    if tokens.id_token.is_empty() {
        bail!("the provider returned no id token, so the account has no identity");
    }
    let claims = jwt_claims(&tokens.id_token).context("the id token is not a decodable JWT")?;
    let email = claims
        .get("email")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .context("the id token carries no email claim")?
        .to_string();
    let auth = claims.get("https://api.openai.com/auth");
    let account_id = auth
        .and_then(|a| a.get("chatgpt_account_id"))
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .context("the id token carries no ChatGPT account id")?
        .to_string();
    let plan = auth
        .and_then(|a| a.get("chatgpt_plan_type"))
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string();

    let stored = crate::codex::StoredCodexAccount {
        id: format!("codex:{account_id}"),
        email: email.clone(),
        account_id,
        plan,
        active: false,
        access_token: tokens.access_token,
        id_token: tokens.id_token,
        refresh_token: tokens.refresh_token,
        expires_at: crate::clone_ops::now_ms() + tokens.expires_in.max(0) * 1000,
        // A sign-in is not a refresh. The first one to run writes the record.
        last_refresh: None,
    };
    crate::codex::upsert_account(app, stored)?;
    tracing::info!("imported Codex account {email} by sign-in");
    Ok(email)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_pkce_pair_is_the_verifier_and_its_sha256() {
        let (verifier, challenge) = pkce().unwrap();
        // 96 random bytes, base64url, no padding.
        assert_eq!(verifier.len(), 128);
        assert!(!verifier.contains('=') && !verifier.contains('+') && !verifier.contains('/'));
        assert_eq!(challenge, b64url(&Sha256::digest(verifier.as_bytes())));
        // Two calls never agree, or one leaked code would redeem another login.
        assert_ne!(pkce().unwrap().0, verifier);
    }

    #[test]
    fn a_query_value_is_escaped_outside_the_unreserved_set() {
        assert_eq!(enc("a b"), "a%20b");
        assert_eq!(enc("http://localhost:54545/callback"), "http%3A%2F%2Flocalhost%3A54545%2Fcallback");
        assert_eq!(enc("-._~AZaz09"), "-._~AZaz09");
    }

    #[test]
    fn the_callback_is_read_out_of_anything_a_person_would_copy() {
        let want = ("abc123".to_string(), Some("st8".to_string()));
        // The whole URL, which is what the address bar holds.
        assert_eq!(
            parse_callback("http://localhost:54545/callback?code=abc123&state=st8").unwrap(),
            want
        );
        // Just the query, and just the pair.
        assert_eq!(parse_callback("?code=abc123&state=st8").unwrap(), want);
        assert_eq!(parse_callback("code=abc123&state=st8").unwrap(), want);
        // Surrounding whitespace from the copy.
        assert_eq!(parse_callback("  code=abc123&state=st8\n").unwrap(), want);
    }

    #[test]
    fn claudes_state_rides_on_the_code_behind_a_hash() {
        // Measured shape: Claude appends its own state to the code rather than sending a
        // separate parameter. Splitting it off is what makes the exchange accept the code.
        let (code, state) = parse_callback("http://localhost:54545/callback?code=abc%23st8").unwrap();
        assert_eq!(code, "abc");
        assert_eq!(state.as_deref(), Some("st8"));
    }

    #[test]
    fn a_refusal_and_a_codeless_paste_each_say_what_is_wrong() {
        let err = parse_callback("?error=access_denied&state=s").unwrap_err().to_string();
        assert!(err.contains("access_denied"), "{err}");
        assert!(parse_callback("http://localhost:54545/callback").is_err());
        assert!(parse_callback("   ").is_err());
    }

    #[test]
    fn joining_a_pool_is_idempotent_and_refuses_a_pool_that_is_not_there() {
        let mut pools = vec![
            wire::CloneGroup { name: "Personal".into(), accounts: vec!["x@y.z".into()] },
            wire::CloneGroup { name: "Medi".into(), accounts: vec![] },
        ];
        add_to_pool(&mut pools, "a@b.c", "Personal").unwrap();
        add_to_pool(&mut pools, "a@b.c", "Personal").unwrap();
        assert_eq!(pools[0].accounts, vec!["x@y.z".to_string(), "a@b.c".to_string()]);
        assert!(pools[1].accounts.is_empty(), "no other pool is touched");

        // A name that is not a pool is a mistake worth reporting: creating it here would put
        // the account somewhere no clone is bound to.
        let err = add_to_pool(&mut pools, "a@b.c", "Nope").unwrap_err().to_string();
        assert!(err.contains("Nope"), "{err}");
    }

    #[test]
    fn a_percent_escape_survives_the_paste() {
        let (code, _) = parse_callback("?code=a%2Bb%2Fc&state=s").unwrap();
        assert_eq!(code, "a+b/c");
    }
}
