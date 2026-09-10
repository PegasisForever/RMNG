//! One account pool shared by both providers: selection, rotation, and rebinding.
//!
//! `claude.rs` and `codex.rs` used to carry this logic twice — same signatures, bodies
//! differing only in a provider filter, a host field, and one usage window. Every
//! rotation fix shipped twice by hand, so the core moved here, parameterized by a small
//! [`PoolProvider`] adapter each side implements. What stays per side: the token stores,
//! OAuth/refresh, usage polling, and the delete/replace flows (different file formats,
//! different views).
//!
//! The one deliberate unification: [`RotationCandidate`] always carries both windows and
//! the saturated ranking is the Claude class-aware one. For Codex the five-hour window
//! is always empty, so every saturated Codex candidate is weekly-capped and the ranking
//! reduces exactly to the old Codex order — same behaviour, one code path.

use std::collections::HashMap;
use std::time::Duration;

use wire::{CloneGroup, RmngClone};

use crate::app::App;
use crate::clone_ops::{rand_u64, shuffle};

pub(crate) const AUTO: &str = "auto";
pub(crate) const SESSION_HEADROOM_PCT: f64 = 20.0;
pub(crate) const SEVEN_DAY_CAP_PCT: f64 = 95.0;
pub(crate) const RESET_STICKY_MARGIN_SECS: i64 = 15 * 60;
pub(crate) const UTIL_STICKY_MARGIN_PCT: f64 = 5.0;
pub(crate) const STAGGER: Duration = Duration::from_millis(400);

/// The per-provider bits the shared core cannot know: which rows are ours, which host
/// field we bind, how we score, and how we deliver a token. Implemented once per side
/// (`ClaudePool` / `CodexPool` below); everything else in this module is generic over it.
pub(crate) trait PoolProvider {
    /// Emails holding a token that still works (rotation-eligible).
    fn usable_emails(app: &App) -> Vec<String>;
    /// Every imported email, usable or not (explicit pins resolve against these).
    fn imported_emails(app: &App) -> Vec<String>;
    /// Latest usage windows per email, for this provider's rows only.
    fn snapshots(app: &App) -> HashMap<String, UsageSnapshot>;
    /// The account installed on this clone right now, if any.
    fn host_email(h: &RmngClone) -> Option<&str>;
    /// Record a new installed account on a host row.
    fn bind_host(h: &mut RmngClone, email: String);
    /// The operator's selection for this side (`auto` or a pin email).
    fn selection(h: &RmngClone) -> Option<&str>;
    /// Rewrite the selection (account renames).
    fn set_selection(h: &mut RmngClone, sel: String);
    /// The pool this side's current pick came from (legacy per-side stickies).
    fn sticky(h: &RmngClone) -> Option<&str>;
    /// Drop the pushed-token record so the next delivery re-pushes.
    fn forget(app: &App, host_id: &str);
    /// Install this side's token into a clone.
    async fn push(app: &App, host_id: &str, email: &str) -> anyhow::Result<()>;
    /// Whether these windows leave no usable headroom.
    fn exhausted(five_pct: f64, seven_pct: f64) -> bool;
    /// (headroom score, eligible) for an account whith these windows.
    fn score_weigh(five_pct: f64, seven_pct: f64) -> (f64, bool);
    /// The window spread-balancing compares (5h for Claude, 7d for Codex).
    fn spread_pct(five_pct: f64, seven_pct: f64) -> f64;
    /// "clone account" vs "codex account" in the unknown-pin warning.
    fn unknown_label() -> &'static str;
    /// "rotate" vs "codex rotate" in log lines.
    const LOG_LABEL: &'static str;
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
    fn usable_emails(app: &App) -> Vec<String> {
        app.claude.usable_emails()
    }
    fn imported_emails(app: &App) -> Vec<String> {
        app.claude.emails()
    }
    fn snapshots(app: &App) -> HashMap<String, UsageSnapshot> {
        let st = app.store.get();
        st.claude_accounts
            .iter()
            .filter(|u| u.provider != Some(wire::Provider::Codex))
            .map(|u| {
                (
                    u.email.clone(),
                    snapshot_of(
                        u.five_hour
                            .as_ref()
                            .map(|w| (w.pct, w.resets_at.as_deref())),
                        u.seven_day
                            .as_ref()
                            .map(|w| (w.pct, w.resets_at.as_deref())),
                    ),
                )
            })
            .collect()
    }
    fn host_email(h: &RmngClone) -> Option<&str> {
        h.claude_account_email.as_deref()
    }
    fn bind_host(h: &mut RmngClone, email: String) {
        h.claude_account_email = Some(email);
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
    fn forget(app: &App, host_id: &str) {
        app.claude.forget_pushed(host_id);
    }
    async fn push(app: &App, host_id: &str, email: &str) -> anyhow::Result<()> {
        crate::claude::push_account_to_clone(app, host_id, email).await
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
    fn unknown_label() -> &'static str {
        "clone account"
    }
    const LOG_LABEL: &'static str = "rotate";
}

impl PoolProvider for CodexPool {
    fn usable_emails(app: &App) -> Vec<String> {
        app.codex.usable_emails()
    }
    fn imported_emails(app: &App) -> Vec<String> {
        app.codex.emails()
    }
    fn snapshots(app: &App) -> HashMap<String, UsageSnapshot> {
        let st = app.store.get();
        st.claude_accounts
            .iter()
            .filter(|u| u.provider == Some(wire::Provider::Codex))
            .map(|u| {
                (
                    u.email.clone(),
                    snapshot_of(
                        None,
                        u.seven_day
                            .as_ref()
                            .map(|w| (w.pct, w.resets_at.as_deref())),
                    ),
                )
            })
            .collect()
    }
    fn host_email(h: &RmngClone) -> Option<&str> {
        h.codex_account_email.as_deref()
    }
    fn bind_host(h: &mut RmngClone, email: String) {
        h.codex_account_email = Some(email);
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
    fn forget(app: &App, host_id: &str) {
        app.codex.forget_pushed(host_id);
    }
    async fn push(app: &App, host_id: &str, email: &str) -> anyhow::Result<()> {
        crate::codex::push_account_to_clone(app, host_id, email).await
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
    fn unknown_label() -> &'static str {
        "codex account"
    }
    const LOG_LABEL: &'static str = "codex rotate";
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
        return best_saturated_email(&rotation_candidates::<P>(app, &members), &clone_loads::<P>(app));
    }
    pool.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    pool.first().map(|s| s.email.clone())
}

/// Resolve a clone request's account selection to a concrete account email.
pub(crate) fn resolve_clone_account<P: PoolProvider>(app: &App, requested: Option<&str>) -> Option<String> {
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
        .filter(|_| want.is_empty() || want.eq_ignore_ascii_case(AUTO) || want.starts_with("group:"));
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
pub(crate) fn pick_group_account<P: PoolProvider>(app: &App, group_name: &str, current: Option<&str>) -> Option<String> {
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
async fn rotate_pool<P: PoolProvider>(app: &App, label: &str, members: &[String], clones: &[RmngClone]) {
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
                P::bind_host(h, bound);
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
        if let Err(e) = P::push(app, &host.id, &email).await {
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
            h.managed && P::sticky(h).is_none() && h.group.is_none() && P::selection(h) == Some(AUTO)
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

/// Move both of a clone's bindings for this side from `old` to `new`, fleet-wide, in one
/// mutation. Returns the ids that were running `old`.
///
/// Both bindings, because they mean different things and both name the dead account: the
/// pin (an explicit operator choice) and the current assignment. Moving the pin is also
/// what lets [`delete_account`](crate::claude::delete_account)-style guards through
/// afterwards, since they refuse while a pin names their target.
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
                P::bind_host(h, new.clone());
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
pub(crate) fn swap_pool_member(
    pools: &mut [CloneGroup],
    old: &str,
    new: &str,
) -> Vec<String> {
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
