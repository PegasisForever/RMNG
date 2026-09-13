//! One account pool shared by both providers: selection, rotation, and rebinding.
//!
//! `claude.rs` and `codex.rs` used to carry this logic twice — same signatures, bodies
//! differing only in a provider filter, a host field, and one usage window. Every
//! rotation fix shipped twice by hand, so the core moved here, parameterized by a small
//! [`PoolProvider`] adapter each side implements.
//!
//! Everything *around* that core was still written twice, and the copies had drifted — one
//! of the drifts handed Codex clones the wrong account. Token delivery, the delete/replace
//! lifecycle, the poll and rotate loops, the swap both HTTP routes perform: all of it lives
//! here now, generic over the same adapter. What stays per side is the adapter itself: the
//! account struct, the refresh POST, OAuth import, usage parsing, and the file writes that
//! put a token into a clone's home (different file formats, different identities).
//!
//! The one deliberate unification: [`RotationCandidate`] always carries both windows and
//! the saturated ranking is the Claude class-aware one. For Codex the five-hour window
//! is always empty, so every saturated Codex candidate is weekly-capped and the ranking
//! reduces exactly to the old Codex order — same behaviour, one code path.

use std::collections::HashMap;
use std::future::Future;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use wire::{ClaudeUsage, CloneGroup, RmngClone};

use crate::account::{AccountKind, PUSH_CONCURRENCY, ROTATE_SECS, Store, fingerprint};
use crate::app::App;
use crate::claude::StoredClaudeAccount;
use crate::clone_ops::{rand_u64, shuffle};
use crate::codex::StoredCodexAccount;

pub(crate) const AUTO: &str = "auto";
pub(crate) const SESSION_HEADROOM_PCT: f64 = 20.0;
pub(crate) const SEVEN_DAY_CAP_PCT: f64 = 95.0;
pub(crate) const RESET_STICKY_MARGIN_SECS: i64 = 15 * 60;
pub(crate) const UTIL_STICKY_MARGIN_PCT: f64 = 5.0;
pub(crate) const STAGGER: Duration = Duration::from_millis(400);

/// The per-provider bits the shared core cannot know: which account store is ours, which
/// host field we bind, how we score, and how we deliver a token. Implemented once per side
/// (`ClaudePool` / `CodexPool` below); everything else in this module is generic over it.
///
/// The delivery methods are declared `-> impl Future + Send` rather than as plain `async
/// fn`s because the flows built on them are spawned as background tasks (the delete's
/// re-placement pass, the fan-out after a refresh). A bare `async fn` in a trait promises no
/// `Send`, so a generic `tokio::spawn` of those flows could not be proven to compile.
pub(crate) trait PoolProvider: 'static {
    /// This side's stored account. The 0600 secret store and the whole refresh lifecycle
    /// come with it ([`crate::account`]), which is what lets the flows below read accounts
    /// and record pushes without knowing whose token they carry.
    type Account: AccountKind;

    /// Which published usage rows are ours, and which a delete may take out.
    ///
    /// Named rather than spelled as a filter at each site: the two hand-written filters this
    /// replaces had drifted into each other's negation (`provider == Some(Codex) || email
    /// != e` against `provider != Some(Codex) || email != e`). That reads as a typo and
    /// behaves as one the day a third provider arrives.
    const PROVIDER: wire::Provider;
    /// Whether this side reports a five-hour session window. Codex has only the weekly one,
    /// so its snapshots leave the 5h fields empty and every saturated Codex candidate ranks
    /// as weekly-capped (see the module header).
    const FIVE_HOUR_WINDOW: bool;
    /// This side's name in a log line: "claude token push", "codex usage poll failed".
    /// Operator-facing prose uses the capitalized [`AccountKind::LABEL`] instead, which is
    /// why both exist — "no imported claude account" would read as a bug.
    const NAME: &'static str;
    /// "account" vs "codex account" in op-log lines.
    const OP_LABEL: &'static str;
    /// "rotate" vs "codex rotate" in log lines.
    const LOG_LABEL: &'static str;
    /// "clone account" vs "codex account" in the unknown-pin warning.
    fn unknown_label() -> &'static str;

    /// This side's account store on the running server.
    fn store(app: &App) -> &Store<Self::Account> {
        <Self::Account as AccountKind>::store(app)
    }
    /// Emails holding a token that still works (rotation-eligible).
    fn usable_emails(app: &App) -> Vec<String> {
        Self::store(app).usable_emails()
    }
    /// Every imported email, usable or not (explicit pins resolve against these).
    fn imported_emails(app: &App) -> Vec<String> {
        Self::store(app).emails()
    }
    /// Drop the pushed-token record so the next delivery re-pushes.
    fn forget(app: &App, host_id: &str) {
        Self::store(app).forget_pushed(host_id);
    }
    /// Latest usage windows per email, for this provider's rows only.
    fn snapshots(app: &App) -> HashMap<String, UsageSnapshot> {
        let st = app.store.get();
        st.claude_accounts
            .iter()
            .filter(|u| row_provider(u) == Self::PROVIDER)
            .map(|u| {
                (
                    u.email.clone(),
                    snapshot_of(
                        Self::FIVE_HOUR_WINDOW
                            .then(|| {
                                u.five_hour
                                    .as_ref()
                                    .map(|w| (w.pct, w.resets_at.as_deref()))
                            })
                            .flatten(),
                        u.seven_day
                            .as_ref()
                            .map(|w| (w.pct, w.resets_at.as_deref())),
                    ),
                )
            })
            .collect()
    }

    /// The account installed on this clone right now, if any.
    fn host_email(h: &RmngClone) -> Option<&str>;
    /// Record the account installed on a host row — or, with `None`, that this side has
    /// none.
    ///
    /// Clearing is half of this interface, not an extra: a delete detaches every clone that
    /// ran the account, and a swap can resolve to no account at all. While this could only
    /// write `Some`, both of those tails had to stay outside this module and were written
    /// twice by hand — which is how the delete pair ended up disagreeing about which rows
    /// they owned.
    fn set_host_email(h: &mut RmngClone, email: Option<String>);
    /// The operator's selection for this side (`auto` or a pin email).
    fn selection(h: &RmngClone) -> Option<&str>;
    /// Rewrite the selection (account renames, swaps).
    fn set_selection(h: &mut RmngClone, sel: String);
    /// The pool this side's current pick came from (legacy per-side stickies).
    fn sticky(h: &RmngClone) -> Option<&str>;
    /// Record the pool a pick came from (`None` when it came from no pool).
    fn set_sticky(h: &mut RmngClone, pool: Option<String>);

    /// What was last delivered to a clone, as one comparable string. See [`push_key_of`]
    /// for what goes in it and the delivery bug that put the identity there.
    fn push_key(acct: &Self::Account) -> String;
    /// `email`'s account, refreshed and persisted first if it is within its refresh lead of
    /// expiry, plus whether that refresh rotated the token.
    fn fresh_access_token(
        app: &App,
        email: &str,
    ) -> impl Future<Output = Result<(Self::Account, bool)>> + Send;
    /// Write this side's token, and the identity that goes with it, into a clone's home.
    fn apply(
        app: &App,
        host_id: &str,
        acct: &Self::Account,
    ) -> impl Future<Output = Result<()>> + Send;
    /// Strip this side's credentials from a clone (pending-auto boots tokenless).
    fn clear(app: &App, host_id: &str) -> impl Future<Output = Result<()>> + Send;
    /// One usage poll over every imported account on this side; `true` when the provider
    /// rate-limited it.
    fn poll(app: &App) -> impl Future<Output = Result<bool>> + Send;

    /// Whether these windows leave no usable headroom.
    fn exhausted(five_pct: f64, seven_pct: f64) -> bool;
    /// (headroom score, eligible) for an account with these windows.
    fn score_weigh(five_pct: f64, seven_pct: f64) -> (f64, bool);
    /// The window spread-balancing compares (5h for Claude, 7d for Codex).
    fn spread_pct(five_pct: f64, seven_pct: f64) -> f64;
}

/// Latest usage windows for one imported account. Sides without a window leave it
/// empty (Codex has no five-hour window); unknown emails read as all-empty.
#[derive(Debug, Clone, Default)]
pub(crate) struct UsageSnapshot {
    pub five_pct: f64,
    pub seven_pct: f64,
    pub five_reset: Option<i64>,
    pub seven_reset: Option<i64>,
}

pub(crate) struct ClaudePool;
pub(crate) struct CodexPool;

fn snapshot_of(
    five: Option<(f64, Option<&str>)>,
    seven: Option<(f64, Option<&str>)>,
) -> UsageSnapshot {
    let (five_pct, five_reset) = five.unwrap_or((0.0, None));
    let (seven_pct, seven_reset) = seven.unwrap_or((0.0, None));
    UsageSnapshot {
        five_pct,
        seven_pct,
        five_reset: five_reset.and_then(parse_rfc3339_utc_secs),
        seven_reset: seven_reset.and_then(parse_rfc3339_utc_secs),
    }
}

impl PoolProvider for ClaudePool {
    type Account = StoredClaudeAccount;

    const PROVIDER: wire::Provider = wire::Provider::Claude;
    const FIVE_HOUR_WINDOW: bool = true;
    const NAME: &'static str = "claude";
    const OP_LABEL: &'static str = "account";
    const LOG_LABEL: &'static str = "rotate";
    fn unknown_label() -> &'static str {
        "clone account"
    }

    fn host_email(h: &RmngClone) -> Option<&str> {
        h.claude_account_email.as_deref()
    }
    fn set_host_email(h: &mut RmngClone, email: Option<String>) {
        h.claude_account_email = email;
    }
    fn selection(h: &RmngClone) -> Option<&str> {
        h.claude_selection.as_deref()
    }
    fn set_selection(h: &mut RmngClone, sel: String) {
        h.claude_selection = Some(sel);
    }
    fn sticky(h: &RmngClone) -> Option<&str> {
        h.claude_group.as_deref()
    }
    fn set_sticky(h: &mut RmngClone, pool: Option<String>) {
        h.claude_group = pool;
    }

    fn push_key(acct: &Self::Account) -> String {
        crate::claude::push_key(acct)
    }
    async fn fresh_access_token(app: &App, email: &str) -> Result<(Self::Account, bool)> {
        crate::claude::fresh_access_token(app, email).await
    }
    async fn apply(app: &App, host_id: &str, acct: &Self::Account) -> Result<()> {
        crate::claude::apply_clone_token(app, host_id, acct).await
    }
    async fn clear(app: &App, host_id: &str) -> Result<()> {
        crate::claude::clear_clone_token(app, host_id).await
    }
    async fn poll(app: &App) -> Result<bool> {
        crate::claude::poll_once(app).await
    }

    fn exhausted(five_pct: f64, seven_pct: f64) -> bool {
        (100.0 - five_pct) < SESSION_HEADROOM_PCT || seven_pct >= SEVEN_DAY_CAP_PCT
    }
    fn score_weigh(five_pct: f64, seven_pct: f64) -> (f64, bool) {
        let headroom = (100.0 - five_pct) / 100.0;
        let eligible = (100.0 - five_pct) >= SESSION_HEADROOM_PCT && seven_pct < SEVEN_DAY_CAP_PCT;
        (headroom, eligible)
    }
    fn spread_pct(five_pct: f64, _seven_pct: f64) -> f64 {
        five_pct
    }
}

impl PoolProvider for CodexPool {
    type Account = StoredCodexAccount;

    const PROVIDER: wire::Provider = wire::Provider::Codex;
    const FIVE_HOUR_WINDOW: bool = false;
    const NAME: &'static str = "codex";
    const OP_LABEL: &'static str = "codex account";
    const LOG_LABEL: &'static str = "codex rotate";
    fn unknown_label() -> &'static str {
        "codex account"
    }

    fn host_email(h: &RmngClone) -> Option<&str> {
        h.codex_account_email.as_deref()
    }
    fn set_host_email(h: &mut RmngClone, email: Option<String>) {
        h.codex_account_email = email;
    }
    fn selection(h: &RmngClone) -> Option<&str> {
        h.codex_selection.as_deref()
    }
    fn set_selection(h: &mut RmngClone, sel: String) {
        h.codex_selection = Some(sel);
    }
    fn sticky(h: &RmngClone) -> Option<&str> {
        h.codex_group.as_deref()
    }
    fn set_sticky(h: &mut RmngClone, pool: Option<String>) {
        h.codex_group = pool;
    }

    fn push_key(acct: &Self::Account) -> String {
        crate::codex::push_key(acct)
    }
    async fn fresh_access_token(app: &App, email: &str) -> Result<(Self::Account, bool)> {
        crate::codex::fresh_access_token(app, email).await
    }
    async fn apply(app: &App, host_id: &str, acct: &Self::Account) -> Result<()> {
        crate::codex::apply_clone_token(app, host_id, acct).await
    }
    async fn clear(app: &App, host_id: &str) -> Result<()> {
        crate::codex::clear_clone_token(app, host_id).await
    }
    async fn poll(app: &App) -> Result<bool> {
        crate::codex::poll_once(app).await
    }

    fn exhausted(_five_pct: f64, seven_pct: f64) -> bool {
        seven_pct >= SEVEN_DAY_CAP_PCT
    }
    fn score_weigh(_five_pct: f64, seven_pct: f64) -> (f64, bool) {
        ((100.0 - seven_pct) / 100.0, seven_pct < SEVEN_DAY_CAP_PCT)
    }
    fn spread_pct(_five_pct: f64, seven_pct: f64) -> f64 {
        seven_pct
    }
}

// --- time ------------------------------------------------------------------

/// Parse an RFC-3339 timestamp to epoch seconds. Accepts the fixed
/// `YYYY-MM-DDTHH:MM:SS` head, then an optional `.fraction`, then an optional zone
/// (`Z`/`z`, `±HH:MM`, or `±HHMM`; absent means UTC). Both providers' resets flow
/// through here (Anthropic `+00:00` stamps, Codex `Z` stamps from
/// [`crate::docker::epoch_to_rfc3339`]). Sub-second precision is dropped (the rotator
/// compares whole seconds).
pub(crate) fn parse_rfc3339_utc_secs(s: &str) -> Option<i64> {
    if s.len() < 19
        || s.get(4..5)? != "-"
        || s.get(7..8)? != "-"
        || s.get(10..11)? != "T"
        || s.get(13..14)? != ":"
        || s.get(16..17)? != ":"
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
    // Tail after the seconds: an optional `.fraction`, then an optional zone offset.
    let mut rest = s.get(19..)?;
    if let Some(frac) = rest.strip_prefix('.') {
        let end = frac
            .find(|c: char| !c.is_ascii_digit())
            .unwrap_or(frac.len());
        if end == 0 {
            return None; // a bare '.' with no digits is malformed
        }
        rest = &frac[end..];
    }
    let offset_secs = parse_zone_offset(rest)?;
    let days = days_from_civil(year, month, day);
    let secs = days * 86_400 + i64::from(hour) * 3_600 + i64::from(minute) * 60 + i64::from(second);
    Some(secs - offset_secs)
}

/// A trailing RFC-3339 zone designator → its offset from UTC in seconds (`+05:30` →
/// 19800). Empty or `Z`/`z` is UTC; otherwise `±HH:MM` or `±HHMM`.
fn parse_zone_offset(z: &str) -> Option<i64> {
    if z.is_empty() || z == "Z" || z == "z" {
        return Some(0);
    }
    let (sign, digits) = match z.strip_prefix('+') {
        Some(rest) => (1i64, rest),
        None => (-1i64, z.strip_prefix('-')?),
    };
    let (hh, mm) = match digits.len() {
        5 => (digits.get(0..2)?, digits.get(2..5)?.strip_prefix(':')?),
        4 => (digits.get(0..2)?, digits.get(2..4)?),
        _ => return None,
    };
    let hh: i64 = hh.parse().ok()?;
    let mm: i64 = mm.parse().ok()?;
    if hh > 23 || mm > 59 {
        return None;
    }
    Some(sign * (hh * 3600 + mm * 60))
}

fn days_from_civil(year: i32, month: u32, day: u32) -> i64 {
    let y = i64::from(if month <= 2 { year - 1 } else { year });
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = i64::from((month + 9) % 12);
    let doy = (153 * mp + 2) / 5 + i64::from(day) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

// --- selection ---------------------------------------------------------------

/// Canonicalize a raw account-selection string into its stored form: `"auto"` or an
/// account email. Missing/blank → `"auto"`; a legacy `"none"` (retired: there is no
/// explicit tokenless state anymore — a side without a pin and without provider members
/// in scope simply gets no token) also reads as `"auto"`.
pub(crate) fn normalize_selection(requested: Option<&str>) -> String {
    let want = requested.unwrap_or("").trim();
    if want.is_empty() || want.eq_ignore_ascii_case("none") {
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

fn score_accounts<P: PoolProvider>(app: &App) -> Vec<Scored> {
    let snaps = P::snapshots(app);
    let loads = clone_loads::<P>(app);
    P::usable_emails(app)
        .into_iter()
        .map(|email| {
            let (five, seven) = snaps
                .get(&email)
                .map(|s| (s.five_pct, s.seven_pct))
                .unwrap_or((0.0, 0.0));
            let (headroom_raw, eligible) = P::score_weigh(five, seven);
            let headroom = clamp01(headroom_raw);
            // reset-soon term omitted (ISO reset parsing TODO) → 0.
            let n = *loads.get(&email).unwrap_or(&0) as f64;
            Scored {
                email,
                score: headroom - 0.5 * n,
                eligible,
            }
        })
        .collect()
}

fn best_scored<P: PoolProvider>(app: &App) -> Option<String> {
    let scored = score_accounts::<P>(app);
    if scored.is_empty() {
        return None;
    }
    let mut pool: Vec<&Scored> = scored.iter().filter(|s| s.eligible).collect();
    if pool.is_empty() {
        let members: Vec<String> = scored.iter().map(|s| s.email.clone()).collect();
        return best_saturated_email(
            &rotation_candidates::<P>(app, &members),
            &clone_loads::<P>(app),
        );
    }
    pool.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    pool.first().map(|s| s.email.clone())
}

/// Best Codex account by the same rotation scoring clones use.
/// Returns None when no Codex account is imported.
pub(crate) fn best_codex_email(app: &App) -> Option<String> {
    best_scored::<CodexPool>(app)
}

/// Resolve a clone request's account selection to a concrete account email.
pub(crate) fn resolve_clone_account<P: PoolProvider>(
    app: &App,
    requested: Option<&str>,
) -> Option<String> {
    let emails = P::imported_emails(app);
    if emails.is_empty() {
        return None;
    }
    let want = requested.unwrap_or("").trim();
    if !want.is_empty() && want != AUTO {
        if let Some(hit) = emails.iter().find(|e| e.as_str() == want) {
            return Some(hit.clone());
        }
        tracing::warn!(
            "{} '{want}' not imported; using recommended",
            P::unknown_label()
        );
    }
    best_scored::<P>(app)
}

// --- groups: selection + rotation -----------------------------------------

/// What a clone is bound to (accounts by email). `Group` carries the initial pick to
/// apply right away; `AutoPending` records explicit auto intent before an imported
/// account exists.
pub(crate) enum Assignment {
    Account(String),
    Group { name: String, initial: String },
    AutoPending,
}

/// Resolve a selection string to an [`Assignment`]: an email / `auto` → a single
/// account, or — when the selection is `auto`/blank and the clone is group-bound — a group
/// (with an initial account picked from it). A legacy `group:<name>`
/// selection still binds that group (transport compat with old clients); new writers
/// store `auto` + the clone-level `group` instead. Explicit `auto` without imported
/// accounts is kept as pending auto; outer `None` means no usable concrete assignment
/// and no explicit pending-auto intent.
///
/// `current` is the clone's account right now (for a swap); when resolving a group it
/// makes the pick sticky — a clone moving from a pinned account into a group that
/// already contains that account keeps it, rather than cold-starting its prompt cache.
/// Pass `None` when there's no incumbent (a fresh clone at create time).
pub(crate) fn resolve_assignment<P: PoolProvider>(
    app: &App,
    requested: Option<&str>,
    current: Option<&str>,
    group: Option<&str>,
) -> Option<Assignment> {
    let want = requested.unwrap_or("").trim();
    // A legacy `"none"` falls through to the auto path (no tokenless state anymore).
    // Legacy `group:<name>` selection, or an auto selection on a group-bound clone.
    let group_name = want
        .strip_prefix("group:")
        .map(str::trim)
        .filter(|n| !n.is_empty())
        .or(group)
        .filter(|_| {
            want.is_empty() || want.eq_ignore_ascii_case(AUTO) || want.starts_with("group:")
        });
    if let Some(name) = group_name {
        let initial = pick_group_account::<P>(app, name, current)?;
        return Some(Assignment::Group {
            name: name.to_string(),
            initial,
        });
    }
    match resolve_clone_account::<P>(app, requested) {
        Some(account) => Some(Assignment::Account(account)),
        None if requested.is_some() && (want.is_empty() || want.eq_ignore_ascii_case(AUTO)) => {
            Some(Assignment::AutoPending)
        }
        None => None,
    }
}

/// How many clones each account email is currently assigned to (this side).
fn clone_loads<P: PoolProvider>(app: &App) -> HashMap<String, u32> {
    let mut m = HashMap::new();
    for h in &app.store.get().hosts {
        if let Some(e) = P::host_email(h) {
            *m.entry(e.to_string()).or_insert(0) += 1;
        }
    }
    m
}

#[derive(Debug, Clone)]
pub(crate) struct RotationCandidate {
    pub(crate) email: String,
    pub(crate) five_pct: f64,
    pub(crate) seven_pct: f64,
    pub(crate) five_reset: Option<i64>,
    pub(crate) seven_reset: Option<i64>,
}

fn rotation_candidates<P: PoolProvider>(app: &App, members: &[String]) -> Vec<RotationCandidate> {
    let known = P::usable_emails(app);
    let snaps = P::snapshots(app);
    members
        .iter()
        .filter(|email| known.iter().any(|k| k.as_str() == email.as_str()))
        .map(|email| {
            let snap = snaps.get(email.as_str()).cloned().unwrap_or_default();
            RotationCandidate {
                email: (*email).clone(),
                five_pct: snap.five_pct,
                seven_pct: snap.seven_pct,
                five_reset: snap.five_reset,
                seven_reset: snap.seven_reset,
            }
        })
        .collect()
}

/// Whether an account is out of usable headroom. Pure decision (see [`exhausted`] for
/// the store-backed wrapper); the provider decides which windows gate.
pub(crate) fn is_exhausted<P: PoolProvider>(five: f64, seven: f64) -> bool {
    P::exhausted(five, seven)
}

/// [`is_exhausted`] against `email`'s latest usage view (unknown emails read as 0%).
fn exhausted<P: PoolProvider>(app: &App, email: &str) -> bool {
    let (five, seven) = P::snapshots(app)
        .get(email)
        .map(|s| (s.five_pct, s.seven_pct))
        .unwrap_or((0.0, 0.0));
    is_exhausted::<P>(five, seven)
}

/// Accounts among `members` that can take work: imported, holding a token that still
/// works, and not exhausted. (A member with no token, or with a dead one, is dropped.)
pub(crate) fn eligible_members<P: PoolProvider>(app: &App, members: &[String]) -> Vec<String> {
    let known = P::usable_emails(app);
    members
        .iter()
        .filter(|email| known.iter().any(|k| k.as_str() == email.as_str()))
        .filter(|email| !exhausted::<P>(app, email))
        .cloned()
        .collect()
}

/// Group members that are imported accounts and not exhausted. Missing usage counts as
/// eligible (0% util).
fn eligible_group_accounts<P: PoolProvider>(app: &App, group: &CloneGroup) -> Vec<String> {
    eligible_members::<P>(app, &group.accounts)
}

/// Pick one account from group `group_name` for a new assignment. Stickiness first: if
/// the clone's `current` account is an eligible member of the group, keep it — switching
/// accounts cold-starts the clone's prompt cache, so a clone moving from a pinned
/// account into a group that already contains it shouldn't be rebalanced off it
/// (mirrors the rotator's keep-if-eligible rule in [`assign_rotation`]). Otherwise:
/// among eligible members (or any member if none are eligible), fewest assigned clones
/// first, then lowest spread-window usage, random tiebreak. `None` if the group is
/// empty / has no imported members.
pub(crate) fn pick_group_account<P: PoolProvider>(
    app: &App,
    group_name: &str,
    current: Option<&str>,
) -> Option<String> {
    let cfg = app.config();
    let group = cfg.groups.iter().find(|g| g.name == group_name)?;
    let counts = clone_loads::<P>(app);
    let mut pool = eligible_group_accounts::<P>(app, group);
    if let Some(cur) = current {
        if pool.iter().any(|e| e == cur) {
            return Some(cur.to_string());
        }
    }
    if pool.is_empty() {
        // All over the cap → still need a valid token; fall back to any imported member.
        return best_saturated_email(&rotation_candidates::<P>(app, &group.accounts), &counts);
    }
    shuffle(&mut pool); // randomize ties
    let snaps = P::snapshots(app);
    pool.into_iter().min_by_key(|email| {
        let load = *counts.get(email).unwrap_or(&0);
        let pct = snaps
            .get(email)
            .map(|s| P::spread_pct(s.five_pct, s.seven_pct))
            .unwrap_or(0.0)
            .round() as u32;
        (load, pct)
    })
}

fn pct_key(pct: f64) -> u32 {
    if !pct.is_finite() {
        return 0;
    }
    (pct.max(0.0) * 100.0).round() as u32
}

/// Rank an exhausted account for the saturated fallback (every account is over a cap, but
/// a clone still needs *some* token). The overriding goal is to land on the account that
/// becomes usable **soonest**: an account at the 7d weekly cap is stuck until its weekly
/// reset (days away), so it must never be preferred over an account merely over the 5h
/// session cap (free at the next 5h reset, hours away at most). `seven_capped` is
/// therefore the first sort key; within a class, order by the *binding* window: soonest
/// reset, then lowest usage (the fallback when the reset timestamp is unknown), then
/// fewest clones.
fn saturated_rank(candidate: &RotationCandidate, load: u32) -> (u8, u8, i64, u32, u32, u32) {
    let seven_capped = candidate.seven_pct >= SEVEN_DAY_CAP_PCT;
    // The window that actually gates this account: the 7d window when it's weekly-capped,
    // else the 5h session window (an exhausted, non-weekly-capped account is over the 5h
    // cap by definition, so the 5h reset is when it frees up).
    let (reset, pct) = if seven_capped {
        (candidate.seven_reset, candidate.seven_pct)
    } else {
        (candidate.five_reset, candidate.five_pct)
    };
    let (missing, reset) = match reset {
        Some(reset) => (0, reset),
        None => (1, i64::MAX),
    };
    (
        seven_capped as u8,
        missing,
        reset,
        pct_key(pct),
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

/// Keep a clone on its current (saturated) account rather than churning it — but only when
/// the current account isn't meaningfully worse than the best. A current account stuck at
/// the weekly cap is dropped in favour of one that frees up at the 5h reset; within the
/// same class, keep it if its binding reset is within [`RESET_STICKY_MARGIN_SECS`] of best's
/// (or, when resets are unknown, its usage within [`UTIL_STICKY_MARGIN_PCT`]).
fn keep_saturated_current(current: &RotationCandidate, best: &RotationCandidate) -> bool {
    if current.email == best.email {
        return true;
    }
    let current_capped = current.seven_pct >= SEVEN_DAY_CAP_PCT;
    let best_capped = best.seven_pct >= SEVEN_DAY_CAP_PCT;
    if current_capped != best_capped {
        // Different classes: keep current only if it's the sooner-freeing (5h-only) one.
        // `best` is the top-ranked candidate, so `best_capped` implies every account is
        // weekly-capped; the only reachable mismatch is a weekly-capped current against a
        // non-capped best → switch.
        return !current_capped;
    }
    let (current_reset, current_pct, best_reset, best_pct) = if current_capped {
        (
            current.seven_reset,
            current.seven_pct,
            best.seven_reset,
            best.seven_pct,
        )
    } else {
        (
            current.five_reset,
            current.five_pct,
            best.five_reset,
            best.five_pct,
        )
    };
    match (current_reset, best_reset) {
        (Some(current_reset), Some(best_reset)) => {
            current_reset <= best_reset + RESET_STICKY_MARGIN_SECS
        }
        (None, None) => current_pct <= best_pct + UTIL_STICKY_MARGIN_PCT,
        _ => false,
    }
}

/// Sticky assignment of `clones` to `eligible` account emails (spread-window utilization
/// in `usage`), returning `(clone, email)` pairs. A clone whose current account is
/// still eligible **keeps it** — switching cold-starts the clone's prompt cache, so a
/// clone is never moved just to even out spread. Only clones without an eligible
/// account (over the cap, removed from the group, or unassigned) are placed: fewest
/// assigned clones first (keepers counted), then lowest spread usage, random tiebreak.
pub(crate) fn assign_rotation<P: PoolProvider>(
    clones: &[RmngClone],
    eligible: &[String],
    usage: &HashMap<String, f64>,
) -> Vec<(RmngClone, String)> {
    let mut used: HashMap<String, u32> = HashMap::new();
    let mut out: Vec<(RmngClone, String)> = Vec::with_capacity(clones.len());
    let mut homeless: Vec<RmngClone> = Vec::new();
    for c in clones {
        match P::host_email(c) {
            Some(e) if eligible.iter().any(|x| x == e) => {
                *used.entry(e.to_string()).or_insert(0) += 1;
                out.push((c.clone(), e.to_string()));
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

pub(crate) fn assign_saturated_rotation<P: PoolProvider>(
    clones: &[RmngClone],
    candidates: &[RotationCandidate],
) -> Vec<(RmngClone, String)> {
    if candidates.is_empty() {
        return Vec::new();
    }

    let mut used: HashMap<String, u32> = HashMap::new();
    let mut out: Vec<(RmngClone, String)> = Vec::with_capacity(clones.len());
    let mut homeless: Vec<RmngClone> = Vec::new();

    for clone in clones {
        let current = P::host_email(clone).and_then(|email| {
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
/// a limit, the saturated fallback picks the account that frees up soonest.
async fn rotate_pool<P: PoolProvider>(
    app: &App,
    label: &str,
    members: &[String],
    clones: &[RmngClone],
) {
    let log = P::LOG_LABEL;
    let candidates = rotation_candidates::<P>(app, members);
    if candidates.is_empty() {
        tracing::info!(
            "{log}: pool '{label}' has no imported account; leaving {} clone(s)",
            clones.len()
        );
        return;
    }

    let eligible: Vec<String> = candidates
        .iter()
        .filter(|candidate| !is_exhausted::<P>(candidate.five_pct, candidate.seven_pct))
        .map(|candidate| candidate.email.clone())
        .collect();
    let assignments = if eligible.is_empty() {
        tracing::info!(
            "{log}: pool '{label}' has no under-cap account; using saturated fallback for {} clone(s)",
            clones.len()
        );
        assign_saturated_rotation::<P>(clones, &candidates)
    } else {
        let usage: HashMap<String, f64> = candidates
            .iter()
            .filter(|candidate| eligible.contains(&candidate.email))
            .map(|candidate| {
                (
                    candidate.email.clone(),
                    P::spread_pct(candidate.five_pct, candidate.seven_pct),
                )
            })
            .collect();
        assign_rotation::<P>(clones, &eligible, &usage)
    };

    for (host, email) in assignments {
        if P::host_email(&host) == Some(email.as_str()) {
            continue; // unchanged (sticky keep) → no rewrite
        }
        // Record the decision BEFORE delivering it. The binding is this server's own data;
        // the push is best-effort delivery into a container that may not be able to take
        // one. Writing it only on a successful push froze every clone that could not: an
        // archived clone on a dead account was re-picked and re-thrown-away every pass,
        // measured on CT 105 as twelve clones stuck on three `invalid_grant` accounts, the
        // same warning line repeating every ten minutes for as long as the log went back.
        // `push_stale_tokens` is the retry, and it closes the gap for a running clone whose
        // push fails here within one poll.
        tracing::info!(
            "{log}[{label}]: {} {} -> {}",
            host.id,
            P::host_email(&host).unwrap_or("none"),
            email
        );
        let (id, bound) = (host.id.clone(), email.clone());
        app.store.mutate(|s| {
            if let Some(h) = s.hosts.iter_mut().find(|h| h.id == id) {
                P::set_host_email(h, Some(bound));
            }
        });
        P::forget(app, &host.id);
        // An archived clone's container is stopped or frozen, so an exec into it cannot
        // succeed. `push_stale_tokens_for` already skips these for the same reason; leaving
        // them in scope here bought nothing and cost a docker round trip plus `STAGGER` per
        // clone on every pass, forever.
        if host.archived {
            continue;
        }
        if let Err(e) = push_account_to_clone::<P>(app, &host.id, &email).await {
            tracing::warn!(
                "{log}[{label}]: {} is now bound to {email}, but installing its token failed \
                 (the next reconcile pass retries): {e}",
                host.id
            );
        }
        tokio::time::sleep(STAGGER).await; // gentle on the daemon
    }
}

/// Managed clones bound to the implicit "auto" pool: selection `auto` and not in a named
/// group. Legacy hosts with no selection are treated as pinned (never rotated).
pub(crate) fn auto_pool_clones<P: PoolProvider>(hosts: &[RmngClone]) -> Vec<RmngClone> {
    hosts
        .iter()
        .filter(|h| {
            h.managed
                && P::sticky(h).is_none()
                && h.group.is_none()
                && P::selection(h) == Some(AUTO)
        })
        .cloned()
        .collect()
}

/// One rotation pass over every named group plus the implicit "auto" pool (all imported
/// accounts, recomputed live). Sticky: a clone moves only when its account exhausts or
/// leaves its pool.
pub(crate) async fn rotate_once<P: PoolProvider>(app: &App) {
    let cfg = app.config();
    let hosts = app.store.get().hosts;
    // Named groups.
    let mut by_group: HashMap<String, Vec<RmngClone>> = HashMap::new();
    for h in &hosts {
        // A group-bound clone with an `auto` selection rotates in its live group even
        // when its sticky still names an older one (group changed under it) or is empty
        // (bound but never resolved). Otherwise the sticky rules (legacy ungrouped rows).
        let gname = if h.group.is_some() && P::selection(h) == Some(AUTO) {
            h.group.as_deref()
        } else {
            P::sticky(h)
        };
        if let (Some(g), true) = (gname, h.managed) {
            by_group.entry(g.to_string()).or_default().push(h.clone());
        }
    }
    for (gname, clones) in by_group {
        let Some(group) = cfg.groups.iter().find(|g| g.name == gname) else {
            continue; // group deleted → leave its clones on their current account
        };
        rotate_pool::<P>(app, &gname, &group.accounts, &clones).await;
    }
    // "auto" == a live group of every account that can still run a clone.
    let auto = auto_pool_clones::<P>(&hosts);
    if !auto.is_empty() {
        rotate_pool::<P>(app, "auto", &P::usable_emails(app), &auto).await;
    }
}

/// What binding one side of a clone ended up with: the stored selection plus the
/// account installed right now (if any) and the pool it came from (if any).
pub(crate) struct SideBinding {
    pub selection: String,
    pub group: Option<String>,
    pub email: Option<String>,
}

/// How delivery failures behave: create/fork log them into the op and still bind (the
/// clone exists either way), while a swap fails the request — the operator is watching.
pub(crate) enum AssignStrictness {
    BestEffort,
    Strict,
}

fn op_log(app: &App, op_id: Option<&str>, line: String) {
    if let Some(op_id) = op_id {
        crate::jobs::patch_op(app, op_id, |op| {
            op.log.push(line);
        });
    }
}

/// Bind one side of a clone: normalize the request, resolve it, deliver the token, and
/// assemble the binding. Outer `None` means resolution found nothing (no accounts in
/// scope and no explicit auto intent) — the caller leaves its locals alone.
///
/// Pending auto (explicit auto, nothing imported yet) strips image-carried credentials
/// so the clone boots tokenless — except under `Strict`, where a swap leaves the
/// incumbent alone and just reports the empty binding.
pub(crate) async fn assign_clone_side<P: PoolProvider>(
    app: &App,
    op_id: Option<&str>,
    host_id: &str,
    requested: Option<&str>,
    current: Option<&str>,
    group: Option<&str>,
    strict: AssignStrictness,
) -> anyhow::Result<Option<SideBinding>> {
    let assignment = resolve_assignment::<P>(app, requested, current, group);
    let selection = normalize_selection(requested);
    let Some(assignment) = assignment else {
        return Ok(None);
    };
    let label = P::OP_LABEL;
    let Some((group, email)) = (match assignment {
        Assignment::AutoPending => None,
        Assignment::Account(email) => Some((None, email)),
        Assignment::Group { name, initial } => Some((Some(name), initial)),
    }) else {
        if matches!(strict, AssignStrictness::BestEffort) {
            // No account can take this side yet. Strip any credentials the image
            // carried so the clone boots tokenless instead of running on unknown
            // ones. Idempotent (`rm -f`); best-effort like the assign arm — a
            // failure is logged, not fatal.
            match P::clear(app, host_id).await {
                Ok(()) => op_log(
                    app,
                    op_id,
                    format!("{label}: auto (pending imported account)"),
                ),
                Err(e) => {
                    tracing::warn!("clear_clone_token({host_id}) failed: {e:#}");
                    op_log(
                        app,
                        op_id,
                        format!("{label}: auto (pending) — failed to clear credentials: {e:#}"),
                    );
                }
            }
            P::forget(app, host_id);
        }
        return Ok(Some(SideBinding {
            selection,
            group: None,
            email: None,
        }));
    };
    let what = match &group {
        Some(g) => format!("{email} (group {g})"),
        None => email.clone(),
    };
    match push_account_to_clone::<P>(app, host_id, &email).await {
        Ok(()) => op_log(app, op_id, format!("{label}: assigned {what}")),
        Err(e) => {
            if matches!(strict, AssignStrictness::Strict) {
                return Err(e);
            }
            tracing::warn!("push_account_to_clone({host_id}) failed: {e:#}");
            op_log(
                app,
                op_id,
                format!("{label}: failed to assign {what}: {e:#}"),
            );
        }
    }
    Ok(Some(SideBinding {
        selection,
        group,
        email: Some(email),
    }))
}

/// Move both of a clone's bindings for this side from `old` to `new`, fleet-wide, in one
/// mutation. Returns the ids that were running `old`.
///
/// Both bindings, because they mean different things and both name the dead account: the
/// pin (an explicit operator choice) and the current assignment. Moving the pin is also
/// what lets [`delete_account`] through afterwards, since it refuses while a pin names its
/// target.
///
/// Separated from the config write so it can be tested without one:
/// [`crate::config::save`] writes a fixed relative path, so a test that reached it would
/// drop a `config.json` in whatever directory it ran in.
pub(crate) fn repoint_clones<P: PoolProvider>(app: &App, old: &str, new: &str) -> Vec<String> {
    let (old, new) = (old.to_string(), new.to_string());
    let mut moved = Vec::new();
    app.store.mutate(|s| {
        for h in &mut s.hosts {
            if P::selection(h) == Some(old.as_str()) {
                P::set_selection(h, new.clone());
            }
            if P::host_email(h) == Some(old.as_str()) {
                P::set_host_email(h, Some(new.clone()));
                moved.push(h.id.clone());
            }
        }
    });
    for id in &moved {
        P::forget(app, id);
    }
    moved
}

/// Put `new` wherever `old` sat in `pools`, and take `old` out. Returns the pool names
/// `new` ended up in.
///
/// Membership is the whole reason a replacement account is usable at all: an account in no
/// pool is one the rotator will never hand to a clone. Idempotent on both halves, so an
/// account already in a pool is not duplicated and a pool without `old` is untouched.
pub(crate) fn swap_pool_member(pools: &mut [CloneGroup], old: &str, new: &str) -> Vec<String> {
    let mut joined = Vec::new();
    for pool in pools.iter_mut() {
        if !pool.accounts.iter().any(|a| a == old) {
            continue;
        }
        if !pool.accounts.iter().any(|a| a == new) {
            pool.accounts.push(new.to_string());
        }
        joined.push(pool.name.clone());
    }
    for pool in pools.iter_mut() {
        pool.accounts.retain(|a| a != old);
    }
    joined
}

// --- token delivery ----------------------------------------------------------

/// What was last delivered to a clone, as one comparable string: the token AND the identity
/// that went with it.
///
/// A rebind can hand a clone a different account whose token happens to be pushed already,
/// and comparing tokens alone would call that clone current while it still names the
/// previous account. Claude's identity is what Claude Code declares to Anthropic on every
/// request (the account uuid and email in `~/.claude.json`); Codex's is the account id and
/// the id token it is handed beside the access token in `~/.codex/auth.json`.
///
/// Codex compared the bare access token until this became one function. A Codex rebind onto
/// an account whose token was already pushed therefore read as "already current" on every
/// later pass as well, so the clone kept running under the account it had before with
/// nothing left to correct it.
pub(crate) fn push_key_of(token: &str, identity: &str) -> String {
    format!("{}|{}", fingerprint(token), fingerprint(identity))
}

/// Whether a clone assigned `host_email` is in scope for a push restricted to `only`.
fn in_push_scope(host_email: &str, only: Option<&str>) -> bool {
    only.is_none_or(|want| want == host_email)
}

/// Refresh-if-needed and install `email`'s token into clone `host_id` (== its container
/// name), recording the push so the reconcile pass doesn't repeat it. If the refresh
/// rotated the token, fan it out to the account's other clones in the background.
///
/// The fan-out happens whether or not THIS clone took its copy. The refresh above has
/// already happened, and the provider revokes the previous access token the moment it mints
/// a new one — so every other clone on this account is broken from that instant, and their
/// repair has nothing to do with whether this one succeeded.
///
/// The distinction is load-bearing rather than theoretical. The rotate pass retries stopped
/// clones forever, so the clone that happens to trigger a refresh is often one whose push
/// cannot possibly work. Returning early there stranded the whole account until the next
/// poll: measured twice on CT 105, 6m42s for `pegasis.personal@gmail.com` across 19 clones
/// and 4m33s for `me@pegasis.site`, each time because the triggering clone was stopped.
pub(crate) async fn push_account_to_clone<P: PoolProvider>(
    app: &App,
    host_id: &str,
    email: &str,
) -> Result<()> {
    let (acct, rotated) = P::fresh_access_token(app, email).await?;
    let applied = P::apply(app, host_id, &acct).await;
    if applied.is_ok() {
        P::store(app)
            .pushed
            .lock()
            .unwrap()
            .insert(host_id.to_string(), P::push_key(&acct));
    }
    if rotated {
        let app = app.clone();
        let email = email.to_string();
        tokio::spawn(async move { push_stale_tokens_for::<P>(&app, Some(&email)).await });
    }
    applied
}

/// Fleet-wide reconcile pass: see [`push_stale_tokens_for`].
///
/// Runs at the end of every poll to retry pushes that failed (clone stopped or unreachable)
/// and to catch clones whose assignment changed out of band. The pushed map is in-memory, so
/// the first pass after a server restart re-pushes every clone.
pub(crate) async fn push_stale_tokens<P: PoolProvider>(app: &App) {
    push_stale_tokens_for::<P>(app, None).await;
}

/// Give every clone assigned an account that account's current access token, unless the last
/// successful push already delivered exactly that token under exactly that identity (see
/// [`push_key_of`]). With `only` set, visit just that account's clones.
///
/// Speed is the whole point. A refresh invalidates the previous token immediately, so every
/// clone still holding it is broken until this reaches it — the agent gets a 401, not a
/// warning. Serially that window grew with the fleet; this runs [`PUSH_CONCURRENCY`] at a
/// time and skips clones that cannot receive a push at all, so it is bounded by the slowest
/// clone rather than by their sum.
pub(crate) async fn push_stale_tokens_for<P: PoolProvider>(app: &App, only: Option<&str>) {
    let started = std::time::Instant::now();
    let name = P::NAME;
    // The account a restricted pass was for, named in every line it logs. Without it the
    // fan-out after one refresh and a fleet-wide sweep read identically in the log, and only
    // one of them is evidence that something is wrong.
    let scope = only.map(|e| format!(" [{e}]")).unwrap_or_default();
    let mut targets: Vec<(String, P::Account)> = Vec::new(); // (host, account)
    let mut skipped_fresh = 0usize;
    let mut skipped_no_account = 0usize;

    for host in app.store.get().hosts {
        let Some(email) = P::host_email(&host) else {
            continue;
        };
        // Archived clones stay bound to an account but can never take a push: their
        // container is stopped or frozen. Leaving them in scope meant eight dead hosts on
        // CT 105 failing an exec on every pass, forever.
        if !in_push_scope(email, only) || !host.managed || host.archived {
            continue;
        }
        let Some(acct) = P::store(app).get_by_email(email) else {
            // Bound to an account the store does not have: the clone keeps whatever it has
            // and nothing here can improve it, but staying silent made it indistinguishable
            // from a clone that is up to date.
            skipped_no_account += 1;
            tracing::warn!(
                "clone {} is bound to {name} account {email}, which is not imported; \
                 leaving its token alone",
                host.id
            );
            continue;
        };
        if P::store(app).pushed.lock().unwrap().get(&host.id) == Some(&P::push_key(&acct)) {
            skipped_fresh += 1;
            continue;
        }
        targets.push((host.id.clone(), acct));
    }

    if targets.is_empty() {
        tracing::debug!(
            "{name} token push{scope}: nothing to do ({skipped_fresh} already current, \
             {skipped_no_account} unbound)"
        );
        return;
    }
    tracing::info!(
        "{name} token push{scope}: {} clone(s) to update ({skipped_fresh} already current, \
         {skipped_no_account} unbound)",
        targets.len()
    );

    let (mut ok, mut failed, mut unreachable) = (0usize, 0usize, 0usize);
    for chunk in targets.chunks(PUSH_CONCURRENCY) {
        let results = futures::future::join_all(chunk.iter().map(|(id, acct)| async move {
            // The push is a plain home write now, which works stopped or running — the only
            // clone that cannot take one is a deleted one (mount torn down).
            if !crate::home_overlay::clone_home_present(id) {
                return (id, acct, None);
            }
            (id, acct, Some(P::apply(app, id, acct).await))
        }))
        .await;

        for (id, acct, outcome) in results {
            let email = acct.email();
            match outcome {
                None => {
                    unreachable += 1;
                    tracing::debug!("skipping {name} token push to {id}: no live home");
                }
                Some(Ok(())) => {
                    ok += 1;
                    P::store(app)
                        .pushed
                        .lock()
                        .unwrap()
                        .insert(id.clone(), P::push_key(acct));
                    tracing::info!("pushed fresh {name} token ({email}) to {id}");
                }
                Some(Err(e)) => {
                    failed += 1;
                    tracing::warn!(
                        "pushing {name} token ({email}) to {id} failed (retried next pass): {e}"
                    );
                }
            }
        }
    }

    tracing::info!(
        "{name} token push{scope} done in {:?}: {ok} pushed, {failed} failed, \
         {unreachable} without live home",
        started.elapsed()
    );
}

/// Install both providers' current access tokens into clone `host_id`. `why` names the pass
/// in anything this logs, since the clone itself cannot say what woke it.
///
/// An archived clone is skipped by every push pass while it is down
/// ([`push_stale_tokens_for`]) and re-bound without a push by the rotator, so the
/// credentials on its disk are whatever it was archived with — possibly an account that has
/// since been deleted or gone dark. Without this it runs them until the next poll, up to ten
/// minutes of 401s on a clone the operator was just told is ready. The gen-2 migration needs
/// the same repair for the same reason: it stops the fleet, rewrites every home, and starts
/// it again.
///
/// Best-effort on both sides. A failure here is logged and left to the next reconcile pass.
pub(crate) async fn push_both_sides(app: &App, host_id: &str, why: &str) {
    let Some(host) = app.store.get().hosts.into_iter().find(|h| h.id == host_id) else {
        return;
    };
    push_one_side::<ClaudePool>(app, &host, why).await;
    push_one_side::<CodexPool>(app, &host, why).await;
}

async fn push_one_side<P: PoolProvider>(app: &App, host: &RmngClone, why: &str) {
    let Some(email) = P::host_email(host) else {
        return;
    };
    if let Err(e) = push_account_to_clone::<P>(app, &host.id, email).await {
        tracing::warn!(
            "{why} {}: installing {email}'s {} token failed: {e}",
            host.id,
            <P::Account as AccountKind>::LABEL
        );
    }
}

// --- account lifecycle -------------------------------------------------------

/// Which side a published usage row belongs to. A row written before the provider field
/// existed carries none, and it is Claude's — that is what the rotation snapshots have
/// always read, so the delete path has to agree or a legacy row would outlive its account.
fn row_provider(u: &ClaudeUsage) -> wire::Provider {
    u.provider.unwrap_or(wire::Provider::Claude)
}

/// Delete an imported account by email, then heal the fleet.
///
/// Refuses (Err) if any clone is **pinned** to it (its selection names the email) — a pin is
/// an explicit operator choice, so it must be reassigned first (swap it to another account,
/// a pool, or `auto`). Clones running the account via `auto`/a pool need no pre-work: once
/// the token is gone a [`rotate_once`] pass moves them onto a surviving account (its assign
/// step treats a no-longer-imported account as ineligible).
///
/// Returns the ids of clones that were on the account.
///
/// **The order of the steps is the contract.** Both sides carried it by hand, and the Codex
/// copy's own comment said so ("including its ordering") without saying what it guarantees:
///
///  1. the pin check runs before anything is written, so a refusal leaves the account and
///     every clone exactly as they were;
///  2. the token leaves disk before the published row does, so a usage poll landing in the
///     middle cannot re-publish an account whose token is already gone;
///  3. the clones running it are collected before they are detached — after the detach no
///     row names the account and the list this returns would be empty;
///  4. the row removal and the detaches are ONE mutation, so the screen is right in one
///     frame. The account's row used to sit in the published state until the NEXT usage poll
///     rebuilt it, and that poll walks every remaining account at a 400ms stagger with a 10s
///     timeout each — long enough that a deleted account stayed on screen looking like the
///     delete had failed;
///  5. the re-placement is spawned last, so it sees a store without the account and clones
///     with nothing bound. It is backgrounded because it walks the whole fleet with a
///     per-clone home write and the screen must not wait on that. A clone it cannot place —
///     the account was its pool's only member — simply stays unassigned.
///
/// Everything the operator can see is therefore settled by the time this returns: the token
/// is off disk, the account's row is out of the published state, and no clone still points
/// at it.
pub(crate) async fn delete_account<P: PoolProvider>(app: &App, email: &str) -> Result<Vec<String>> {
    let label = <P::Account as AccountKind>::LABEL;
    let pinned: Vec<String> = app
        .store
        .get()
        .hosts
        .iter()
        .filter(|h| P::selection(h) == Some(email))
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
    let account_id = P::store(app)
        .get_by_email(email)
        .map(|a| a.id().to_string());
    if !P::store(app).delete(email)? {
        bail!("no imported {label} account '{email}'");
    }
    if let Some(id) = &account_id {
        P::store(app).last_good.lock().unwrap().remove(id);
    }

    // Clones currently running the (now-deleted) account. Drop their pushed-token records so
    // the re-placement pushes the replacement token.
    let on_it: Vec<String> = app
        .store
        .get()
        .hosts
        .iter()
        .filter(|h| P::host_email(h) == Some(email))
        .map(|h| h.id.clone())
        .collect();
    for id in &on_it {
        P::forget(app, id);
    }

    app.store.mutate(|s| {
        // This side's row only. The same email can be imported on both sides, and each is a
        // separate account with its own token; the two hand-written copies of this filter
        // had drifted into each other's negation, and one of them read a legacy row with no
        // provider at all as the other side's.
        s.claude_accounts
            .retain(|u| !(row_provider(u) == P::PROVIDER && u.email == email));
        for h in &mut s.hosts {
            if P::host_email(h) == Some(email) {
                P::set_host_email(h, None);
            }
        }
    });

    let bg = app.clone();
    tokio::spawn(async move { rotate_once::<P>(&bg).await });
    Ok(on_it)
}

/// Hand everything `old_email` holds to `new_email`, then delete `old_email`.
///
/// This is what the "sign in again" badge does. Recovering a dead account used to mean
/// deleting it and importing its replacement by hand, which loses two things the operator
/// then has to rebuild from memory: which pools it was in, and which clones were pinned to
/// it by name. Both move here, in one operation, so the replacement lands where the original
/// stood.
///
/// A sign-in as the SAME account is not a replacement — the import has already overwritten
/// the token and cleared the rejection — so it returns early having done nothing. Returns
/// the ids of clones that moved onto `new_email`.
pub(crate) async fn replace_account<P: PoolProvider>(
    app: &App,
    old_email: &str,
    new_email: &str,
) -> Result<Vec<String>> {
    let label = <P::Account as AccountKind>::LABEL;
    if old_email == new_email {
        return Ok(Vec::new());
    }
    if P::store(app).get_by_email(old_email).is_none() {
        bail!("no imported {label} account '{old_email}' to replace");
    }
    if P::store(app).get_by_email(new_email).is_none() {
        bail!("'{new_email}' is not an imported {label} account");
    }

    let mut cfg = app.config();
    let joined = swap_pool_member(&mut cfg.groups, old_email, new_email);
    crate::config::save(&cfg).context("saving the replacement's pool membership")?;
    *app.cfg.write().unwrap() = cfg;

    let moved = repoint_clones::<P>(app, old_email, new_email);
    delete_account::<P>(app, old_email).await?;
    tracing::info!(
        "replaced {label} account {old_email} with {new_email}: {} clone(s), pool(s) {}",
        moved.len(),
        if joined.is_empty() {
            "none".to_string()
        } else {
            joined.join(", ")
        },
    );

    // Deliver the new token to everything that just moved. Backgrounded for the same reason
    // the delete's rotation is: it is one home write per clone.
    let bg = app.clone();
    let email = new_email.to_string();
    tokio::spawn(async move { push_stale_tokens_for::<P>(&bg, Some(&email)).await });
    Ok(moved)
}

/// Delete every imported account the merged pool list leaves unclaimed, on both sides.
///
/// An account in zero pools is removed (the pool tree's rule) — [`delete_account`] settles
/// clones onto surviving accounts, and refuses (Err) when a clone pins the account, which
/// fails the save with that reason instead of stranding the pin.
pub(crate) async fn sweep_ungrouped(app: &App) -> Result<()> {
    let claimed: std::collections::HashSet<String> = app
        .config()
        .groups
        .iter()
        .flat_map(|g| g.accounts.iter().cloned())
        .collect();
    sweep_side::<ClaudePool>(app, &claimed).await?;
    sweep_side::<CodexPool>(app, &claimed).await?;
    Ok(())
}

async fn sweep_side<P: PoolProvider>(
    app: &App,
    claimed: &std::collections::HashSet<String>,
) -> Result<()> {
    for email in P::imported_emails(app) {
        if claimed.contains(&email) {
            continue;
        }
        tracing::info!(
            "removing ungrouped {} account {email} (claimed by no pool)",
            <P::Account as AccountKind>::LABEL
        );
        delete_account::<P>(app, &email).await?;
    }
    Ok(())
}

// --- background loops --------------------------------------------------------

/// Self-scheduling usage-poll loop with 429 backoff. `base_secs` is this side's poll
/// interval (`wire::CLAUDE_POLL_SECS` / `wire::CODEX_POLL_SECS`), floored at 15s because a
/// misconfigured zero would hammer the provider.
pub(crate) async fn run_poller<P: PoolProvider>(app: App, base_secs: u64) {
    const MAX_BACKOFF: Duration = Duration::from_secs(30 * 60);
    let name = P::NAME;
    let mut backoff: u32 = 0;
    loop {
        let any429 = match P::poll(&app).await {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!("{name} usage poll failed: {e}");
                false
            }
        };
        let base = Duration::from_secs(base_secs.max(15));
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
                "{name} usage rate-limited (429); next poll in {}s",
                delay.as_secs()
            );
        }
        tokio::time::sleep(delay).await;
    }
}

/// Self-scheduling [`ROTATE_SECS`] rotation loop.
pub(crate) async fn run_rotator<P: PoolProvider>(app: App) {
    // Let the usage poller publish this side's numbers before the first rotation, or it
    // would rank every account at 0% and place the whole fleet on one of them.
    tokio::time::sleep(Duration::from_secs(30)).await;
    loop {
        rotate_once::<P>(&app).await;
        tokio::time::sleep(Duration::from_secs(ROTATE_SECS)).await;
    }
}

// --- swapping one side of a clone --------------------------------------------

/// The body both swap routes take. One struct, not one per side: the two were identical
/// down to the field docs, and a field added to one of them would have gone unnoticed.
#[derive(serde::Deserialize)]
pub(crate) struct SwapRequest {
    pub host: String,
    /// Account email (a pin — any imported account, even outside the clone's pool),
    /// `auto` (rotate in scope), or legacy `group:<name>` (rebinds the pool).
    pub account: String,
    /// Clone-level pool binding: `Some(name)` binds, `Some("")` unbinds, absent keeps.
    /// A `group:<name>` account implies the bind.
    #[serde(default)]
    pub group: Option<Option<String>>,
}

/// Why a swap did not happen. The two carry different blame, which is the whole reason they
/// are separate: `Rejected` is the operator's request to fix, `Undelivered` is a clone
/// refusing a token that resolved perfectly well.
pub(crate) enum SwapError {
    Rejected(String),
    Undelivered(String),
}

/// Bind one side of a clone to what `req` asks for: resolve the pool binding, deliver the
/// token, and write the result onto the clone's row.
///
/// Strict delivery ([`AssignStrictness::Strict`]): the operator is watching this one, so a
/// push that fails fails the request rather than binding a clone to a token it never got.
pub(crate) async fn swap_side<P: PoolProvider>(
    app: &App,
    req: &SwapRequest,
) -> std::result::Result<SideBinding, SwapError> {
    let host = app
        .store
        .get()
        .hosts
        .into_iter()
        .find(|h| h.id == req.host)
        .ok_or_else(|| SwapError::Rejected(format!("unknown host '{}'", req.host)))?;
    if !host.managed {
        return Err(SwapError::Rejected(format!(
            "'{}' is not a managed clone",
            host.id
        )));
    }
    // A `group:<name>` account rebinds the whole clone (both sides draw from it afterwards);
    // the selection is stored as `auto`. An explicit email overrides this side only — the
    // clone-level group stays for the other side.
    //
    // The helper takes both sides' selections because create and fork carry both. A swap
    // carries one, and the helper's two selection slots behave identically when the other is
    // empty, so which slot this side's request travels in makes no difference here.
    let (bound_group, requested, _) = crate::clone_ops::split_group_binding(
        Some(req.account.clone()),
        None,
        host.group.clone(),
        req.group.clone(),
    );
    crate::clone_ops::validate_group(&app.config(), bound_group.as_deref())
        .map_err(|e| SwapError::Rejected(e.to_string()))?;
    let binding = assign_clone_side::<P>(
        app,
        None,
        &host.id,
        requested.as_deref(),
        P::host_email(&host),
        bound_group.as_deref(),
        AssignStrictness::Strict,
    )
    .await
    .map_err(|e| SwapError::Undelivered(e.to_string()))?
    .ok_or_else(|| {
        SwapError::Rejected(format!(
            "no {} account can take this clone: none is imported, or every one that could \
             has a token that expired and cannot be refreshed",
            <P::Account as AccountKind>::LABEL
        ))
    })?;
    let (id, email, pool, selection) = (
        host.id.clone(),
        binding.email.clone(),
        binding.group.clone(),
        binding.selection.clone(),
    );
    app.store.mutate(|s| {
        if let Some(h) = s.hosts.iter_mut().find(|h| h.id == id) {
            P::set_host_email(h, email);
            P::set_sticky(h, pool);
            P::set_selection(h, selection);
            h.group = bound_group.clone();
        }
    });
    Ok(binding)
}

#[cfg(test)]
mod tests {
    use super::*;

    // The rules below are provider-neutral: the pick, the stickiness, the saturated ranking
    // and the auto pool read the same for both sides. They used to be asserted twice, once
    // per adapter, and the twins had already started to differ in what they covered rather
    // than in what they claimed. A provider still appears in each test, because a pool
    // cannot read a clone's account without knowing which host field holds it — that is all
    // it contributes here. What genuinely differs stays with the adapter: the scoring
    // constants, and the per-side host fields themselves.

    fn acct(email: &str) -> String {
        email.to_string()
    }

    /// A managed clone already running `cur` on `P`'s side.
    fn clone_host<P: PoolProvider>(id: &str, cur: Option<&str>) -> RmngClone {
        let mut h = RmngClone {
            id: id.into(),
            managed: true,
            ..Default::default()
        };
        P::set_host_email(&mut h, cur.map(str::to_string));
        h
    }

    /// A clone with `P`'s selection and pool binding set (and nothing else), for the auto
    /// pool's membership rule. A selection of `None` is the legacy shape: no selection at
    /// all, which reads as pinned.
    fn host_sel<P: PoolProvider>(
        id: &str,
        managed: bool,
        pool: Option<&str>,
        sel: Option<&str>,
    ) -> RmngClone {
        let mut h = RmngClone {
            id: id.into(),
            managed,
            ..Default::default()
        };
        P::set_sticky(&mut h, pool.map(str::to_string));
        if let Some(sel) = sel {
            P::set_selection(&mut h, sel.to_string());
        }
        h
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
    fn legacy_none_normalizes_to_auto() {
        // No tokenless state anymore: "none" reads as auto (lossy — the side resolves
        // in scope and may now get a token).
        assert_eq!(normalize_selection(Some("none")), "auto");
        assert_eq!(normalize_selection(Some("NONE")), "auto");
        assert_eq!(normalize_selection(None), "auto");
        assert_eq!(normalize_selection(Some("me@x.com")), "me@x.com");
    }

    // --- rotation assignment -------------------------------------------------

    #[test]
    fn assignment_rule_a_only_group_accounts() {
        // Every clone is assigned an account from the eligible set, never outside it.
        let eligible = [acct("a@x"), acct("b@x")];
        let clones = [
            clone_host::<ClaudePool>("c1", Some("z@outside")),
            clone_host::<ClaudePool>("c2", None),
        ];
        for (_h, picked) in assign_rotation::<ClaudePool>(&clones, &eligible, &HashMap::new()) {
            assert!(eligible.contains(&picked), "{picked} not in group");
        }
    }

    #[test]
    fn assignment_rule_b_distinct_when_enough_accounts() {
        // |eligible| >= |unassigned clones| ⇒ they land on distinct accounts (run
        // repeatedly: randomized, but the load term forces distinctness here).
        let eligible = [acct("a@x"), acct("b@x"), acct("c@x")];
        let clones = [
            clone_host::<ClaudePool>("c1", None),
            clone_host::<ClaudePool>("c2", None),
            clone_host::<ClaudePool>("c3", None),
        ];
        for _ in 0..50 {
            let got = assign_rotation::<ClaudePool>(&clones, &eligible, &HashMap::new());
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
        let clones = [clone_host::<ClaudePool>("c1", Some("a@x"))];
        for _ in 0..50 {
            let got = assign_rotation::<ClaudePool>(&clones, &eligible, &HashMap::new());
            assert_eq!(got[0].1, "a@x");
        }
    }

    #[test]
    fn assignment_moves_only_ineligible_and_avoids_keepers() {
        // c1 keeps its eligible account A; c2 (account dropped from the group) must
        // move, and lands on B — the keeper on A counts toward A's load.
        let eligible = [acct("a@x"), acct("b@x")];
        let clones = [
            clone_host::<ClaudePool>("c1", Some("a@x")),
            clone_host::<ClaudePool>("c2", Some("z@gone")),
        ];
        for _ in 0..50 {
            let got = assign_rotation::<ClaudePool>(&clones, &eligible, &HashMap::new());
            let by_id: HashMap<_, _> = got.iter().map(|(h, e)| (h.id.clone(), e.clone())).collect();
            assert_eq!(by_id["c1"], "a@x");
            assert_eq!(by_id["c2"], "b@x");
        }
    }

    #[test]
    fn assignment_prefers_less_used_account_on_load_tie() {
        // A fresh clone with two equally-loaded accounts picks the lower spread usage.
        let eligible = [acct("hot@x"), acct("cold@x")];
        let clones = [clone_host::<ClaudePool>("c1", None)];
        let usage = HashMap::from([(acct("hot@x"), 72.0), (acct("cold@x"), 5.0)]);
        for _ in 0..50 {
            let got = assign_rotation::<ClaudePool>(&clones, &eligible, &usage);
            assert_eq!(got[0].1, "cold@x");
        }
    }

    #[test]
    fn assignment_degrades_with_single_eligible() {
        // Only one usable account ⇒ all clones get it even though spread can't hold.
        let eligible = [acct("only@x")];
        let clones = [
            clone_host::<ClaudePool>("c1", Some("only@x")),
            clone_host::<ClaudePool>("c2", Some("old@x")),
        ];
        let got = assign_rotation::<ClaudePool>(&clones, &eligible, &HashMap::new());
        assert!(got.iter().all(|(_, e)| e == "only@x"));
    }

    // --- the saturated fallback ----------------------------------------------

    #[test]
    fn saturated_never_picks_weekly_capped_over_session_capped() {
        // stuck@x is at the weekly cap (unusable for days) but barely touched its 5h
        // window; soon@x is only over the 5h session cap and frees up at the next 5h reset.
        // The clone must land on soon@x — never the account a low 5h number makes look
        // "least used" while its weekly cap keeps it dark for days.
        let candidates = [
            rotation_candidate("stuck@x", 5.0, 97.0, Some(1_000), Some(600_000)),
            rotation_candidate("soon@x", 85.0, 50.0, Some(2_000), Some(700_000)),
        ];
        let clones = [clone_host::<ClaudePool>("c1", Some("stuck@x"))];

        let got = assign_saturated_rotation::<ClaudePool>(&clones, &candidates);

        assert_eq!(got[0].1, "soon@x");
    }

    #[test]
    fn saturated_prefers_soonest_5h_reset_among_session_capped() {
        // Both only over the 5h cap (7d has room) → soonest 5h reset frees up first.
        let candidates = [
            rotation_candidate("soon@x", 90.0, 50.0, Some(1_000), Some(700_000)),
            rotation_candidate("late@x", 90.0, 50.0, Some(2_000), Some(700_000)),
        ];
        let clones = [clone_host::<ClaudePool>("c1", Some("late@x"))];

        let got = assign_saturated_rotation::<ClaudePool>(&clones, &candidates);

        assert_eq!(got[0].1, "soon@x");
    }

    #[test]
    fn saturated_prefers_soonest_7d_reset_when_all_weekly_capped() {
        // Everyone is weekly-capped → the binding window is 7d; soonest weekly reset wins
        // and the (here deliberately inverted) 5h resets are ignored.
        let candidates = [
            rotation_candidate("soon@x", 50.0, 97.0, Some(9_000), Some(500_000)),
            rotation_candidate("late@x", 50.0, 97.0, Some(1_000), Some(600_000)),
        ];
        let clones = [clone_host::<ClaudePool>("c1", Some("late@x"))];

        let got = assign_saturated_rotation::<ClaudePool>(&clones, &candidates);

        assert_eq!(got[0].1, "soon@x");
    }

    /// The same rule read from the Codex side, where there is no five-hour window at all:
    /// the class key is constant, so the weekly reset alone decides. This is what pins the
    /// unified ranking to the order Codex had before the two were merged.
    #[test]
    fn saturated_without_a_five_hour_window_reduces_to_the_weekly_order() {
        let candidates = [
            rotation_candidate("soon@o", 0.0, 97.0, None, Some(500_000)),
            rotation_candidate("late@o", 0.0, 96.0, None, Some(600_000)),
        ];
        let clones = [clone_host::<CodexPool>("c1", Some("late@o"))];

        let got = assign_saturated_rotation::<CodexPool>(&clones, &candidates);

        assert_eq!(got[0].1, "soon@o");
    }

    #[test]
    fn saturated_uses_lower_usage_when_binding_reset_missing() {
        // Both only 5h-capped, no 5h reset timestamp → fall back to the lower 5h usage.
        let candidates = [
            rotation_candidate("hot@x", 98.0, 50.0, None, Some(700_000)),
            rotation_candidate("cool@x", 90.0, 50.0, None, Some(700_000)),
        ];
        let clones = [clone_host::<ClaudePool>("c1", Some("hot@x"))];

        let got = assign_saturated_rotation::<ClaudePool>(&clones, &candidates);

        assert_eq!(got[0].1, "cool@x");
    }

    #[test]
    fn saturated_keeps_current_within_reset_margin() {
        // Same class (both 5h-capped); current's 5h reset is within the sticky margin of
        // best's, so the clone keeps its account (avoids a cold prompt-cache switch).
        let candidates = [
            rotation_candidate("current@x", 90.0, 50.0, Some(1_800), Some(700_000)),
            rotation_candidate("best@x", 90.0, 50.0, Some(1_000), Some(700_000)),
        ];
        let clones = [clone_host::<ClaudePool>("c1", Some("current@x"))];

        let got = assign_saturated_rotation::<ClaudePool>(&clones, &candidates);

        assert_eq!(got[0].1, "current@x");
    }

    /// The other class: both weekly-capped, and the margin holds there too. c1 sits on
    /// soon@o, whose reset is within the sticky margin of best's — churning it onto late@o
    /// would buy nothing.
    #[test]
    fn saturated_keeps_current_within_reset_margin_in_the_weekly_class() {
        let candidates = [
            rotation_candidate("soon@o", 0.0, 97.0, None, Some(500_000)),
            rotation_candidate("late@o", 0.0, 96.0, None, Some(500_100)),
        ];
        let clones = [clone_host::<CodexPool>("c1", Some("soon@o"))];

        let got = assign_saturated_rotation::<CodexPool>(&clones, &candidates);

        assert_eq!(got[0].1, "soon@o");
    }

    #[test]
    fn saturated_moves_missing_reset_current_to_known_reset() {
        // Same class; current has no 5h reset while a peer does → move to the known one.
        let candidates = [
            rotation_candidate("unknown@x", 90.0, 50.0, None, Some(700_000)),
            rotation_candidate("known@x", 94.0, 50.0, Some(1_000), Some(700_000)),
        ];
        let clones = [clone_host::<ClaudePool>("c1", Some("unknown@x"))];

        let got = assign_saturated_rotation::<ClaudePool>(&clones, &candidates);

        assert_eq!(got[0].1, "known@x");
    }

    /// The weekly cap is one number for both sides: at 95% an account is out, whoever it
    /// belongs to. Claude's extra five-hour rule is its own constant and stays with it.
    #[test]
    fn the_weekly_cap_is_the_same_for_both_sides() {
        assert!(!is_exhausted::<ClaudePool>(0.0, 94.9));
        assert!(is_exhausted::<ClaudePool>(0.0, 95.0));
        assert!(!is_exhausted::<CodexPool>(0.0, 94.9));
        assert!(is_exhausted::<CodexPool>(0.0, 95.0));
    }

    // --- the "auto" pool ------------------------------------------------------

    fn auto_pool_case<P: PoolProvider>() {
        let hosts = vec![
            host_sel::<P>("auto1", true, None, Some("auto")), // in
            host_sel::<P>("pinned", true, None, Some("me@x")), // out: pinned to an email
            host_sel::<P>("legacy", true, None, None),        // out: legacy None == pinned
            host_sel::<P>("grouped", true, Some("g"), Some("auto")), // out: its pool handles it
            host_sel::<P>("stopped", false, None, Some("auto")), // out: unmanaged
        ];
        let picked: Vec<String> = auto_pool_clones::<P>(&hosts)
            .into_iter()
            .map(|h| h.id)
            .collect();
        assert_eq!(picked, vec!["auto1"]);
    }

    #[test]
    fn auto_pool_is_only_managed_ungrouped_auto_clones() {
        auto_pool_case::<ClaudePool>();
        auto_pool_case::<CodexPool>();
    }

    /// A grouped clone reads its pool from the clone-level `group`, not only from the
    /// per-side sticky, so it stays out of the auto pool either way.
    #[test]
    fn a_clone_level_pool_also_keeps_a_clone_out_of_the_auto_pool() {
        let mut host = host_sel::<ClaudePool>("grouped", true, None, Some("auto"));
        host.group = Some("g".into());
        assert!(auto_pool_clones::<ClaudePool>(&[host]).is_empty());
    }

    // --- pool membership ------------------------------------------------------

    #[test]
    fn a_replacement_inherits_every_pool_the_old_account_sat_in() {
        let mut pools = vec![
            CloneGroup {
                name: "Personal".into(),
                accounts: vec!["old@x".into(), "other@x".into()],
            },
            CloneGroup {
                name: "Medi".into(),
                accounts: vec!["old@x".into()],
            },
            CloneGroup {
                name: "Untouched".into(),
                accounts: vec!["other@x".into()],
            },
        ];
        let joined = swap_pool_member(&mut pools, "old@x", "new@x");
        assert_eq!(joined, vec!["Personal".to_string(), "Medi".to_string()]);
        assert_eq!(
            pools[0].accounts,
            vec!["other@x".to_string(), "new@x".to_string()]
        );
        assert_eq!(pools[1].accounts, vec!["new@x".to_string()]);
        assert_eq!(
            pools[2].accounts,
            vec!["other@x".to_string()],
            "a pool without it is left alone"
        );

        // Replacing with an account that is already a member neither duplicates it nor
        // leaves the old one behind.
        let mut shared = vec![CloneGroup {
            name: "Personal".into(),
            accounts: vec!["old@x".into(), "new@x".into()],
        }];
        swap_pool_member(&mut shared, "old@x", "new@x");
        assert_eq!(shared[0].accounts, vec!["new@x".to_string()]);
    }

    // --- push scope -----------------------------------------------------------

    /// An unfiltered pass visits every clone; a filtered one visits only the rotated
    /// account's, which is what keeps a refresh from rewriting the whole fleet.
    #[test]
    fn a_filtered_push_pass_visits_only_its_own_account() {
        assert!(in_push_scope("a@x", None));
        assert!(in_push_scope("a@x", Some("a@x")));
        assert!(!in_push_scope("b@x", Some("a@x")));
    }

    /// A rebind can hand a clone an account whose token was already pushed somewhere else.
    /// Comparing tokens alone called that clone current while it still named its old
    /// account, so the identity is part of the key on both sides.
    #[test]
    fn the_push_key_separates_two_accounts_sharing_a_token() {
        assert_ne!(
            push_key_of("same-token", "account-a"),
            push_key_of("same-token", "account-b")
        );
        assert_ne!(
            push_key_of("token-1", "account-a"),
            push_key_of("token-2", "account-a")
        );
        assert_eq!(
            push_key_of("token-1", "account-a"),
            push_key_of("token-1", "account-a")
        );
    }

    // --- published rows -------------------------------------------------------

    /// A row written before the provider field existed is Claude's, which is what the
    /// rotation snapshots read. The delete path has to agree, or such a row would outlive
    /// the account it describes.
    #[test]
    fn a_row_with_no_provider_belongs_to_claude() {
        let mut row = ClaudeUsage {
            id: "a@x".into(),
            email: "a@x".into(),
            provider: None,
            active: false,
            assignable: None,
            error: None,
            stale: None,
            last_updated: 0,
            five_hour: None,
            seven_day: None,
            fable: None,
            spend: None,
            reset_credits: None,
        };
        assert_eq!(row_provider(&row), ClaudePool::PROVIDER);
        row.provider = Some(wire::Provider::Codex);
        assert_eq!(row_provider(&row), CodexPool::PROVIDER);
    }

    // --- time -----------------------------------------------------------------

    #[test]
    fn rfc3339_utc_secs_parses_valid_shapes_and_rejects_malformed() {
        // Bare Z (Codex's epoch_to_rfc3339 form).
        assert_eq!(
            parse_rfc3339_utc_secs("2021-01-01T00:00:00Z"),
            Some(1_609_459_200)
        );
        // The Anthropic usage API's real shape: fractional seconds + `+00:00` offset.
        assert_eq!(
            parse_rfc3339_utc_secs("2026-07-24T22:00:00.469890+00:00"),
            Some(1_784_930_400)
        );
        // Fractional seconds are dropped, not rounded.
        assert_eq!(
            parse_rfc3339_utc_secs("2021-01-01T00:00:00.5+00:00"),
            Some(1_609_459_200)
        );
        // Non-UTC offsets shift to UTC: -05:00 is 5h later in epoch, +05:30 is earlier.
        assert_eq!(
            parse_rfc3339_utc_secs("2021-01-01T00:00:00-05:00"),
            Some(1_609_477_200)
        );
        assert_eq!(
            parse_rfc3339_utc_secs("2021-01-01T00:00:00+05:30"),
            Some(1_609_439_400)
        );
        // `±HHMM` (no colon) is accepted too.
        assert_eq!(
            parse_rfc3339_utc_secs("2021-01-01T00:00:00-0500"),
            Some(1_609_477_200)
        );
        // No zone → treated as UTC.
        assert_eq!(
            parse_rfc3339_utc_secs("2021-01-01T00:00:00"),
            Some(1_609_459_200)
        );
        // Malformed input is rejected, not guessed.
        assert_eq!(parse_rfc3339_utc_secs("not-a-timestamp"), None);
        assert_eq!(parse_rfc3339_utc_secs("2021-13-01T00:00:00Z"), None); // month 13
        assert_eq!(parse_rfc3339_utc_secs("2021-01-01T25:00:00Z"), None); // hour 25
        assert_eq!(parse_rfc3339_utc_secs("2021-01-01T00:00:00."), None); // bare fraction dot
        assert_eq!(parse_rfc3339_utc_secs("2021-01-01T00:00:00+5:00"), None); // 1-digit hour offset
        assert_eq!(parse_rfc3339_utc_secs("2021-01-01T00:00:00+99:00"), None); // offset hour 99
        assert_eq!(parse_rfc3339_utc_secs("2021-01-01"), None); // no time
    }
}
