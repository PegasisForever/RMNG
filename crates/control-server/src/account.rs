//! One account store and one refresh lifecycle, shared by both providers.
//!
//! `claude.rs` and `codex.rs` used to carry this twice — the same store struct with the same
//! eleven methods, the same FNV-1a phase hash, the same expiry test, the same three-ending
//! refresh record. Stripped of doc comments the two stores differed by about one line, and the
//! duplication had a direction: Codex imported its refresh vocabulary from Claude
//! (`crate::claude::RefreshRecord`, `token_alive`, `grant_rejected`, even `PUSH_CONCURRENCY`),
//! so the sibling depended on Claude's internals rather than on anything neutral. Codex's own
//! comment said its hash was "kept identical to Claude's so refresh phases are stable across
//! restarts" — a contract kept by hand, which is the kind that stops being kept.
//!
//! So the whole lifecycle moved here, parameterized by a small [`AccountKind`] adapter each
//! side implements. What stays per side: the account struct, the refresh POST (the one
//! genuinely provider-specific step), OAuth import, usage polling, and token delivery.
//!
//! This is the same shape [`crate::pool`] already uses for rotation, and the same reason.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use wire::ClaudeUsage;

use crate::app::App;
use crate::clone_ops::now_ms;

/// Refresh an access token this far before its expiry. Clones *run* on these
/// tokens, so the lead must comfortably exceed the worst-case gap between polls
/// (poll_secs default 600s, 429 backoff up to 30 min).
///
/// Anthropic issues 8-hour access tokens (`expires_in: 28800`), so a 2-hour lead
/// refreshes every 6 hours and leaves four back-to-back 30-minute backoffs of room.
/// Codex issues much shorter ones and decodes its expiry from the access-token JWT, but the
/// lead is the same number for the same reason: it is the poll gap it has to survive.
pub(crate) const REFRESH_LEAD_MS: i64 = 2 * 60 * 60 * 1000;
/// Width of the per-account offset added on top of [`REFRESH_LEAD_MS`].
///
/// Accounts imported in one sitting expire in the same second, so without an offset a
/// single refresh event rewrites every clone's credentials at once — the widest possible
/// window in which clones run on a token the server has already replaced. The offset only
/// ever *adds* lead, so no account is refreshed later than the 2-hour floor, and one cycle
/// spreads the expiries for good: a refreshed token expires 8 hours after its own refresh.
pub(crate) const REFRESH_SPREAD_MS: i64 = 90 * 60 * 1000;
/// How long any one provider call gets before it is abandoned.
pub(crate) const FETCH_TIMEOUT: Duration = Duration::from_secs(10);
/// How often group-bound clones are checked against their group's eligible accounts.
/// Sticky: a pass moves a clone only if its account fell out of eligibility — an
/// account switch always cold-starts the clone's prompt cache, so staying put is cheaper
/// than perfect spread.
pub(crate) const ROTATE_SECS: u64 = 600;
/// How many clones are pushed at once. A push is one home write, which costs seconds of
/// waiting and almost no CPU, so the useful width is set by how many the daemon will
/// happily carry rather than by cores. The old serial pass took a measured ~11s per clone,
/// which on a forty-clone fleet left the last one running a dead token for minutes.
pub(crate) const PUSH_CONCURRENCY: usize = 8;

/// What this account's last refresh attempt did.
///
/// It lives in the store because the log does not survive the question being asked. Docker
/// keeps a container's log with the container, a server update replaces the container, and
/// the line that says whether a refresh rotated the token or silently handed back the one
/// it had just spent is gone before anyone notices the account is dead. That line six
/// hours before an `invalid_grant` is the whole diagnosis, so it is written next to the
/// token instead of only to stdout.
///
/// Fingerprints only ([`fingerprint`]), never a token.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RefreshRecord {
    /// When the attempt finished, epoch ms.
    pub at: i64,
    pub ok: bool,
    /// The refresh token the attempt was made with.
    pub rt_before: String,
    /// The refresh token the reply carried. Empty on failure, and empty on the one success
    /// that dooms the account: a reply with no `refresh_token` leaves the store holding a
    /// token it has already spent, and the next refresh six hours later is rejected.
    #[serde(default)]
    pub rt_after: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// The provider REJECTED the grant (400/401), so no later attempt with this refresh
    /// token can succeed. Distinct from every other failure — a timeout, a 429, a 5xx —
    /// which leaves a perfectly good chain and must be retried, not acted on.
    ///
    /// This is what separates "dead" from "not answering" without waiting for the clock.
    /// See [`grant_rejected`].
    #[serde(default)]
    pub rejected: bool,
}

/// A refresh that did not produce a token, and whether the provider said so permanently.
/// Both providers rotate a single-use refresh token, so both have the same three endings.
pub(crate) struct RefreshFailure {
    pub error: anyhow::Error,
    /// The token endpoint answered 400 or 401. The grant is spent or revoked, and every
    /// later attempt with it fails the same way.
    pub rejected: bool,
}

impl RefreshFailure {
    /// A failure that says nothing about the grant: a timeout, a 429, a 5xx, an unreadable
    /// reply. The chain may still be fine, so the account keeps its place.
    fn transient(error: anyhow::Error) -> Self {
        Self {
            error,
            rejected: false,
        }
    }
}

impl From<anyhow::Error> for RefreshFailure {
    fn from(error: anyhow::Error) -> Self {
        Self::transient(error)
    }
}

/// Whether `status` from a token endpoint means the refresh token itself is dead.
///
/// 400 is what Anthropic returns with `{"error": "invalid_grant"}` for a spent or revoked
/// token, and 401 for a rejected client credential. Everything else (429, 5xx, a proxy's
/// 502) is the provider having a bad moment, and an account must not be evicted for one.
pub(crate) fn refresh_status_is_fatal(status: u16) -> bool {
    status == 400 || status == 401
}

/// Whether the token the store holds for an account still works.
///
/// `expires_at` moves forward only when a refresh succeeds, so this is the one test that
/// separates "a poll pass failed" from "this account can no longer run a clone". A 429 or
/// a dropped connection on the usage fetch leaves a perfectly good token behind and must
/// not evict an account. A refresh chain the provider has rejected cannot mint another
/// token, so the account goes dark the moment its last one expires, and comes back by
/// itself the moment a refresh succeeds.
///
/// The account keeps its full refresh lead (2 hours plus its own offset) of grace, which
/// is four to five poll passes of failure before anything moves.
pub(crate) fn token_alive(expires_at: i64, now: i64) -> bool {
    now < expires_at
}

/// Whether the account's refresh chain is known dead: the last attempt was rejected by the
/// provider. A success (including a fresh sign-in, which clears the record entirely) undoes
/// it, so an account that comes back needs no separate repair.
pub(crate) fn grant_rejected(last_refresh: Option<&RefreshRecord>) -> bool {
    last_refresh.is_some_and(|r| r.rejected)
}

/// FNV-1a over `s`. Hand-rolled because an account's refresh phase has to survive a
/// server restart, and `DefaultHasher` is explicitly not stable across Rust releases.
///
/// One copy, not one per provider: the two used to be identical by hand, with a comment on
/// the Codex copy saying so.
pub(crate) fn stable_hash(s: &str) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in s.as_bytes() {
        h ^= u64::from(*b);
        h = h.wrapping_mul(0x100_0000_01b3);
    }
    h
}

/// A short, non-reversible handle on a token, safe to write to a log.
///
/// A token value must never be logged, but without some handle on it there is no way to
/// tell "the stored refresh token rotated" apart from "the store still holds the one we
/// already spent". Those two look identical from outside and fail six hours apart.
/// [`stable_hash`] is already here and costs no dependency.
pub(crate) fn fingerprint(token: &str) -> String {
    format!("{:016x}", stable_hash(token))
}

/// How long before expiry `email`'s token is refreshed: the shared floor plus that
/// account's own offset within [`REFRESH_SPREAD_MS`].
pub(crate) fn refresh_lead_ms(email: &str) -> i64 {
    REFRESH_LEAD_MS + (stable_hash(email) % REFRESH_SPREAD_MS as u64) as i64
}

pub(crate) fn is_expired(email: &str, expires_at: i64) -> bool {
    now_ms() + refresh_lead_ms(email) >= expires_at
}

/// Whether this account can be handed to a clone: it holds a token that has not expired
/// AND its refresh chain has not been rejected.
///
/// Expiry alone was the old test, and it left a revoked account in the rotation for its
/// whole refresh lead (two hours plus its own offset, up to three and a half). During that
/// window every clone on it got 401s from the provider while the server reported it healthy.
pub(crate) fn account_usable<A: AccountKind>(acct: &A, now: i64) -> bool {
    token_alive(acct.expires_at(), now) && !grant_rejected(acct.last_refresh())
}

/// The per-provider bits the shared lifecycle cannot know: which file it lives in, what to
/// call it, how to read the four fields the lifecycle reasons about, and how to post a
/// refresh. Implemented once per side (`StoredClaudeAccount` / `StoredCodexAccount`);
/// everything else in this module is generic over it.
pub(crate) trait AccountKind:
    Clone + Serialize + DeserializeOwned + Send + Sync + 'static
{
    /// The 0600 secret store's file name inside `data_dir`.
    const FILE: &'static str;
    /// How this provider is named in a log line or an operator-facing error.
    const LABEL: &'static str;
    /// An environment variable that overrides the store path outright, when this side has
    /// one. Tests point the Codex store somewhere disposable this way.
    const PATH_ENV: Option<&'static str> = None;

    fn id(&self) -> &str;
    fn email(&self) -> &str;
    fn expires_at(&self) -> i64;
    /// The token the next refresh will be made with. The lifecycle fingerprints it BEFORE
    /// the POST, because the failure that matters most — the provider accepted and spent it
    /// but handed back no replacement — is invisible without the before/after pair.
    fn refresh_token(&self) -> &str;
    fn last_refresh(&self) -> Option<&RefreshRecord>;
    fn set_last_refresh(&mut self, r: RefreshRecord);

    /// This provider's store on the running server. The seam that lets one refresh
    /// lifecycle serve both sides without either knowing about the other.
    fn store(app: &App) -> &Store<Self>;

    /// The one genuinely provider-specific step: the refresh POST.
    ///
    /// Mutates `acct` in place (access token, expiry, and whatever else the reply carries)
    /// and returns the fingerprint of the refresh token the reply carried — empty when it
    /// carried none, which is the success that dooms the account. The caller records the
    /// attempt and persists; an implementation must leave the stored tokens untouched on
    /// every failing path, so a failure writes back the record and nothing else.
    async fn refresh(http: &reqwest::Client, acct: &mut Self) -> Result<String, RefreshFailure>;
}

/// The on-disk shape of a provider's secret store. One file, one array, camelCase members
/// supplied by the account struct itself.
#[derive(Serialize, Deserialize)]
struct AccountsFile<A> {
    // Named rather than `#[serde(default)]`: the bare form makes serde's derive demand
    // `A: Default`, which no account struct has any reason to implement.
    #[serde(default = "Vec::new")]
    accounts: Vec<A>,
}

/// Server-only state for one provider: the 0600 secret account store + last-good usage cache.
pub(crate) struct Store<A: AccountKind> {
    pub(crate) accounts: Mutex<Vec<A>>,
    pub(crate) last_good: Mutex<HashMap<String, ClaudeUsage>>,
    pub(crate) path: PathBuf,
    pub(crate) polling: Mutex<bool>,
    /// Serializes OAuth refreshes: refresh tokens are single-use, so two
    /// concurrent refreshes of one account would invalidate each other.
    pub(crate) refresh_gate: tokio::sync::Mutex<()>,
    /// host id → what was last pushed to it successfully. In-memory on
    /// purpose: an empty map after a restart makes the first reconcile pass
    /// re-push every assigned clone.
    pub(crate) pushed: Mutex<HashMap<String, String>>,
}

impl<A: AccountKind> Store<A> {
    pub fn load(data_dir: &str) -> Self {
        let path = A::PATH_ENV
            .and_then(|var| std::env::var(var).ok())
            .map(PathBuf::from)
            .unwrap_or_else(|| Path::new(data_dir).join(A::FILE));
        let accounts = std::fs::read_to_string(&path)
            .ok()
            .and_then(|s| serde_json::from_str::<AccountsFile<A>>(&s).ok())
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

    /// Replace the whole store file with `accounts`, `0600`, via a temp file + rename so a
    /// crash mid-write cannot leave a truncated token store behind.
    pub(crate) fn save(&self, accounts: &[A]) -> Result<()> {
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

    pub(crate) fn snapshot(&self) -> Vec<A> {
        self.accounts.lock().unwrap().clone()
    }

    pub(crate) fn get_by_email(&self, email: &str) -> Option<A> {
        self.accounts
            .lock()
            .unwrap()
            .iter()
            .find(|a| a.email() == email)
            .cloned()
    }

    /// Emails of every imported account. Membership, not usability: an account whose
    /// refresh chain is dead is still imported. Use [`Self::usable_emails`] to pick one
    /// for a clone.
    pub(crate) fn emails(&self) -> Vec<String> {
        self.accounts
            .lock()
            .unwrap()
            .iter()
            .map(|a| a.email().to_string())
            .collect()
    }

    /// Emails whose stored token still works: the accounts a clone may be handed. See
    /// [`account_usable`].
    pub(crate) fn usable_emails(&self) -> Vec<String> {
        let now = now_ms();
        self.accounts
            .lock()
            .unwrap()
            .iter()
            .filter(|a| account_usable(*a, now))
            .map(|a| a.email().to_string())
            .collect()
    }

    /// Upsert `acct` (by id) and persist the store.
    pub(crate) fn update_account(&self, acct: &A) -> Result<()> {
        let mut accounts = self.accounts.lock().unwrap();
        match accounts.iter_mut().find(|a| a.id() == acct.id()) {
            Some(existing) => *existing = acct.clone(),
            None => accounts.push(acct.clone()),
        }
        self.save(&accounts)
    }

    /// Write one account into the 0600 store, replacing whatever shared its **email**.
    ///
    /// Accounts arrive one way: signing in to the provider at this server ([`crate::oauth`]).
    /// Reading credentials back out of a signed-in clone was the other, and it is gone: it
    /// needed a clone standing, the CLI installed in it, and a second login.
    ///
    /// The email is the identity, not `id`. Everything downstream names an account by email
    /// ([`Store::get_by_email`], the clone's account binding, pool membership, the delete
    /// path) and takes the FIRST record with that email. Claude's `id` carries the org uuid,
    /// so a sign-in under a different org (or one recorded before the org uuid was captured,
    /// leaving `id` as `email|`) used to land a SECOND record beside the first instead of
    /// over it. The stale copy then answered every lookup for the fresh one, so its rejected
    /// grant made a just-signed-in account unusable and no clone could get the new token.
    /// Two records for one email are not distinguishable by any caller, so the store never
    /// holds them.
    pub(crate) fn upsert(&self, stored: A) -> Result<()> {
        let mut accts = self.accounts.lock().unwrap();
        accts.retain(|a| a.id() != stored.id() && a.email() != stored.email());
        accts.push(stored);
        accts.sort_by(|a, b| a.email().cmp(b.email()));
        self.save(&accts)
    }

    /// Drop `host_id`'s pushed-token record so the next reconcile pass re-pushes
    /// (or, for an unassigned host, simply forgets it).
    pub fn forget_pushed(&self, host_id: &str) {
        self.pushed.lock().unwrap().remove(host_id);
    }

    /// Remove every imported account matching `email` and persist. Returns whether any
    /// were present (false ⇒ nothing to delete). The token is gone from disk immediately;
    /// re-adding it requires a fresh sign-in.
    pub(crate) fn delete(&self, email: &str) -> Result<bool> {
        let mut accounts = self.accounts.lock().unwrap();
        let before = accounts.len();
        accounts.retain(|a| a.email() != email);
        if accounts.len() == before {
            return Ok(false);
        }
        self.save(&accounts)?;
        Ok(true)
    }
}

/// Refresh `acct`'s access token unconditionally (rotates the single-use refresh
/// token). Mutates `acct` in place; the caller persists.
///
/// Every exit is logged AND written to `acct.last_refresh`, because a refresh has three
/// distinct bad endings that all look the same later:
///   - the request never reached the provider, so the token is untouched;
///   - the provider rejected it, so the token was already dead;
///   - the provider accepted and spent it, but the reply was unreadable or carried no
///     replacement, so the store now holds a spent token and the account dies at the next
///     refresh with `invalid_grant`.
///
/// The record is the same evidence as the log line, in a place a container replace cannot
/// take with it. See [`RefreshRecord`].
pub(crate) async fn refresh_account<A: AccountKind>(
    http: &reqwest::Client,
    acct: &mut A,
) -> Result<()> {
    let before = fingerprint(acct.refresh_token());
    let out = A::refresh(http, acct).await;
    acct.set_last_refresh(RefreshRecord {
        at: now_ms(),
        ok: out.is_ok(),
        rt_before: before,
        rt_after: match &out {
            Ok(after) => after.clone(),
            Err(_) => String::new(),
        },
        error: out.as_ref().err().map(|e| format!("{:#}", e.error)),
        rejected: out.as_ref().err().is_some_and(|e| e.rejected),
    });
    out.map(|_| ()).map_err(|e| e.error)
}

/// `email`'s current account, refreshed (and persisted) first if within
/// [`refresh_lead_ms`] of expiry. Returns `(account, rotated)`.
///
/// Each side wraps this in its own `fresh_access_token`, which runs it in its own task so a
/// caller that goes away cannot abandon a refresh half-done. Most callers are HTTP handlers
/// (`/api/claude/refresh`, `/api/claude/swap` and their Codex twins), axum drops a handler's
/// future the moment its client disconnects, and the refresh pass they start runs for ten
/// seconds or more across a fleet's accounts. A drop between the POST and the store write
/// leaves the provider holding a rotated pair and the store holding the token it just spent,
/// with no record and no log line, and the account dies at its next refresh. The gate is
/// taken inside this function for the same reason: released early, it would let a second
/// caller refresh an account mid-rotation.
pub(crate) async fn refresh_and_persist<A: AccountKind>(
    app: &App,
    email: &str,
) -> Result<(A, bool)> {
    let store = A::store(app);
    let _gate = store.refresh_gate.lock().await;
    let mut acct = store
        .get_by_email(email)
        .with_context(|| format!("no imported {} account for '{email}'", A::LABEL))?;
    if !is_expired(acct.email(), acct.expires_at()) {
        return Ok((acct, false));
    }
    // A rejected grant cannot mint anything, so posting it again only tells the provider that
    // this address keeps presenting a dead refresh token. Nothing here can repair it: the
    // repair is a sign-in, and that clears the record ([`Store::upsert`] stores the account
    // with `last_refresh: None`), so the next poll picks the account straight back up.
    //
    // Left unguarded this ran every poll forever. Measured on CT 101 on 2026-09-04: two
    // rejected accounts retried every 10 minutes for 38 hours, about 460 rejected calls.
    if let Some(rec) = acct.last_refresh().filter(|r| r.rejected) {
        let why = rec.error.as_deref().unwrap_or("no reason recorded");
        let label = A::LABEL;
        bail!(
            "{email}'s {label} refresh token was rejected by the provider, so no refresh is \
             attempted until it is signed in again: {why}"
        );
    }
    if let Err(e) = refresh_account(&app.http, &mut acct).await {
        // Persist the attempt even though it failed. `refresh_account` leaves the tokens
        // untouched on every failing path, so this writes back the record and nothing
        // else, and it is the only way the account's own file says why it stopped working.
        if let Err(w) = store.update_account(&acct) {
            tracing::warn!(
                "recording {}'s failed {} refresh: {w:#}",
                acct.email(),
                A::LABEL
            );
        }
        return Err(e);
    }
    // A failed write here is fatal to the account, not cosmetic: memory now holds the new
    // token, the process runs on it for hours, and the next restart reads the spent one
    // back off disk. Name it in the error so it is not mistaken for a network failure.
    store.update_account(&acct).with_context(|| {
        format!(
            "persisting {}'s refreshed token failed, so the rotation exists only in memory",
            acct.email()
        )
    })?;
    Ok((acct, true))
}
