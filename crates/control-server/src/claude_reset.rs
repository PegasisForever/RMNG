//! Claude usage resets: the Claude side of what `codex.rs` does with ChatGPT's reset credits.
//!
//! A Claude subscription can hold **reset grants**. Each grant gives a number of resets
//! (`resets_left` of `resets_total`), clears some usage windows when used (`clears`:
//! `five_hour`, `seven_day`, ...), and can end on a date (`ends_at`). Anthropic calls the
//! program `cedar_ember`. Claude Code reads it as one more block of the usage reply
//! (`GET /api/oauth/usage?cedar_ember=1`) and uses a grant with
//! `POST /api/organizations/{org}/reset_rate_limits`
//! `{ "program": "cedar_ember", "grant_id", "request_id" }`. Both shapes here are the ones
//! Claude Code 2.1.318 parses; no account had a grant when this was written, so the parse is
//! lenient: a grant that does not read is dropped, and never costs the account its usage.
//!
//! The reply depends on who asks: with a User-Agent that is not Claude Code's, the block says
//! `ineligible_reason: "surface"` and lists no grants. So the usage fetch and the claim both
//! send [`CLI_USER_AGENT`].
//!
//! Auto-use is the Codex gate ([`crate::codex::choose_reset_target`]): it fires only when every
//! Claude account is over the weekly cap with no weekly reset within 24h, and then spends one
//! reset that clears the weekly window, on the account with the most such resets. One mark per
//! account and weekly window (`ControlState.claude_reset_marks`) keeps a restart or a failed
//! claim from spending a second one.

use anyhow::{Result, bail};
use serde::Deserialize;

use crate::account::FETCH_TIMEOUT;
use crate::clone_ops::snippet;

/// The reset program's name, in the usage reply and in the claim.
const PROGRAM: &str = "cedar_ember";
const API_BASE: &str = "https://api.anthropic.com";
const OAUTH_BETA_HEADER: &str = "oauth-2025-04-20";
/// Claude Code's own User-Agent. Anthropic shows the reset grants only to Claude Code (any
/// other surface reads `ineligible_reason: "surface"`), and refuses one too old
/// (`cli_version`). Raise the version when the log says so ([`Status::parse`]).
pub(crate) const CLI_USER_AGENT: &str = "claude-cli/2.1.318 (external, cli)";

/// The `cedar_ember` block of the usage reply. Every field is optional: a field that is
/// missing or of another type reads as absent.
#[derive(Debug, Default, Deserialize)]
pub(crate) struct Status {
    #[serde(default)]
    ineligible_reason: Option<String>,
    /// Each grant on its own: one that does not parse is dropped, not the whole block.
    #[serde(default)]
    grants: Option<Vec<serde_json::Value>>,
}

/// One reset grant.
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct Grant {
    pub(crate) id: String,
    #[serde(default)]
    label: Option<String>,
    #[serde(default)]
    resets_total: Option<i64>,
    #[serde(default)]
    pub(crate) resets_left: Option<i64>,
    #[serde(default)]
    ends_at: Option<String>,
    /// The windows a use clears: `five_hour`, `seven_day`, `seven_day_opus`, ...
    #[serde(default)]
    clears: Option<Vec<String>>,
    #[serde(default)]
    paused: Option<bool>,
    /// Anthropic says it can be used now (for one that needs the account at its limit, only
    /// once the account is at it).
    #[serde(default)]
    usable_now: Option<bool>,
    /// `session`, `weekly` or `full`.
    #[serde(default, rename = "type")]
    kind: Option<String>,
    /// Why it cannot be used now: `cooldown`, `blocked_seven_day`, `needs_limit`, ...
    #[serde(default)]
    unavailable_reason: Option<String>,
}

impl Grant {
    fn left(&self) -> i64 {
        self.resets_left.unwrap_or(0).max(0)
    }
    fn clears_weekly(&self) -> bool {
        self.clears
            .as_ref()
            .is_some_and(|c| c.iter().any(|w| w == "seven_day"))
    }
    /// It can be spent on the weekly window right now.
    fn spendable_weekly(&self) -> bool {
        self.usable_now == Some(true)
            && self.paused != Some(true)
            && self.left() > 0
            && self.clears_weekly()
    }
    fn name(&self) -> String {
        if let Some(l) = self
            .label
            .as_deref()
            .map(str::trim)
            .filter(|l| !l.is_empty())
        {
            return l.to_string();
        }
        match self.kind.as_deref() {
            Some("session") => "5-hour reset",
            Some("weekly") => "Weekly reset",
            Some("full") => "Full reset",
            _ => "Reset",
        }
        .to_string()
    }
    /// One line for the tooltip: "Weekly reset: 2 of 3 left, until 2026-11-30, not now
    /// (cooldown)".
    fn line(&self) -> String {
        let mut s = format!("{}: {}", self.name(), self.left());
        if let Some(t) = self.resets_total.filter(|t| *t > 0) {
            s.push_str(&format!(" of {t}"));
        }
        s.push_str(" left");
        if let Some(day) = self.ends_at.as_deref().and_then(|e| e.get(0..10)) {
            s.push_str(&format!(", until {day}"));
        }
        if self.paused == Some(true) {
            s.push_str(", paused");
        } else if self.left() > 0 && self.usable_now != Some(true) {
            match self.unavailable_reason.as_deref() {
                Some(r) => s.push_str(&format!(", not now ({})", r.replace('_', " "))),
                None => s.push_str(", not now"),
            }
        }
        s
    }
}

impl Status {
    /// Read the block out of the usage reply's `cedar_ember` value. `None` when it is absent,
    /// `null`, or not an object.
    pub(crate) fn parse(v: Option<&serde_json::Value>) -> Option<Status> {
        let v = v.filter(|v| v.is_object())?;
        let s: Status = serde_json::from_value(v.clone()).ok()?;
        // These two say the server cannot see the grants at all, on every account.
        if let Some(r @ ("surface" | "cli_version")) = s.ineligible_reason.as_deref() {
            tracing::warn!(
                "claude reset: Anthropic does not show the reset grants to this request ({r}); raise CLI_USER_AGENT"
            );
        }
        Some(s)
    }

    pub(crate) fn grants(&self) -> Vec<Grant> {
        self.grants
            .iter()
            .flatten()
            .filter_map(|g| match serde_json::from_value::<Grant>(g.clone()) {
                Ok(g) => Some(g),
                Err(e) => {
                    tracing::warn!("claude reset: dropped a grant that does not parse ({e}): {g}");
                    None
                }
            })
            .collect()
    }

    /// The resets left on all its grants; `None` with no grant (most accounts), so the card
    /// shows nothing rather than a 0 on every account.
    pub(crate) fn credits(&self) -> Option<i64> {
        let g = self.grants();
        (!g.is_empty()).then(|| g.iter().map(Grant::left).sum())
    }

    /// The tooltip: one line per grant. `None` with no grant.
    pub(crate) fn detail(&self) -> Option<String> {
        let g = self.grants();
        (!g.is_empty()).then(|| g.iter().map(Grant::line).collect::<Vec<_>>().join("\n"))
    }

    /// For the auto-use gate: the grant to spend on the weekly window (the one that ends
    /// first, so the one that would be lost first), and how many such resets there are.
    pub(crate) fn weekly_pick(&self) -> (Option<String>, i64) {
        let mut g: Vec<Grant> = self
            .grants()
            .into_iter()
            .filter(Grant::spendable_weekly)
            .collect();
        let credits = g.iter().map(Grant::left).sum();
        // No end date sorts last.
        g.sort_by_key(|g| {
            g.ends_at
                .as_deref()
                .and_then(crate::pool::parse_rfc3339_utc_secs)
                .unwrap_or(i64::MAX)
        });
        (g.first().map(|g| g.id.clone()), credits)
    }
}

/// One account's inputs to the auto-reset gate: the Codex gate's, plus the grant to spend.
pub(crate) struct Facts {
    pub(crate) fleet: crate::codex::FleetFacts,
    pub(crate) grant_id: Option<String>,
}

/// What the claim answered.
#[derive(Debug, Deserialize)]
pub(crate) struct ClaimReply {
    /// `reset`, `already_used`, `not_limited`, `cooldown`, `ineligible`, `unavailable`.
    #[serde(default)]
    pub(crate) result: Option<String>,
    #[serde(default)]
    pub(crate) reason: Option<String>,
    #[serde(default)]
    pub(crate) resets_left: Option<i64>,
}

/// Use one reset of `grant_id`. `request_id` makes a resend of the same claim harmless.
pub(crate) async fn claim(
    http: &reqwest::Client,
    token: &str,
    org_uuid: &str,
    grant_id: &str,
    request_id: &str,
) -> Result<ClaimReply> {
    let resp = http
        .post(format!(
            "{API_BASE}/api/organizations/{org_uuid}/reset_rate_limits"
        ))
        .timeout(FETCH_TIMEOUT)
        .header("Authorization", format!("Bearer {token}"))
        .header("anthropic-beta", OAUTH_BETA_HEADER)
        .header("User-Agent", CLI_USER_AGENT)
        .json(&serde_json::json!({
            "program": PROGRAM,
            "grant_id": grant_id,
            "request_id": request_id,
        }))
        .send()
        .await?;
    let status = resp.status();
    let text = resp.text().await.unwrap_or_default();
    if !status.is_success() {
        bail!("reset claim {}{}", status.as_u16(), snippet(&text));
    }
    Ok(serde_json::from_str(&text)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn status(v: serde_json::Value) -> Status {
        Status::parse(Some(&v)).unwrap()
    }

    #[test]
    fn no_grant_reads_as_nothing() {
        // The real block of every production account on 2026-11-04.
        let s = status(serde_json::json!({
            "eligible": false, "ineligible_reason": "no_grant", "at_limit": false,
            "exhausted": [], "grants": [], "next_grant_id": null,
            "weekly_resets_at": "2026-11-06T11:00:00+00:00", "cooldown_until": null,
            "event_props": null, "paused_held": false
        }));
        assert_eq!(s.credits(), None);
        assert_eq!(s.detail(), None);
        assert_eq!(s.weekly_pick(), (None, 0));
        assert!(Status::parse(Some(&serde_json::Value::Null)).is_none());
        assert!(Status::parse(None).is_none());
    }

    #[test]
    fn grants_sum_describe_and_pick_the_one_ending_first() {
        let s = status(serde_json::json!({
            "eligible": true,
            "grants": [
                { "id": "late", "label": "", "resets_total": 3, "resets_left": 2,
                  "ends_at": "2026-12-30T00:00:00Z", "clears": ["five_hour", "seven_day"],
                  "paused": false, "usable_now": true, "type": "full" },
                { "id": "soon", "label": "Weekly reset", "resets_total": 1, "resets_left": 1,
                  "ends_at": "2026-11-30T00:00:00+00:00", "clears": ["seven_day"],
                  "usable_now": true, "type": "weekly" },
                { "id": "session", "resets_left": 4, "clears": ["five_hour"],
                  "usable_now": true, "type": "session" },
                { "id": "cool", "resets_left": 1, "clears": ["seven_day"], "usable_now": false,
                  "unavailable_reason": "cooldown", "type": "weekly" },
                { "label": "no id: dropped" }
            ]
        }));
        assert_eq!(s.credits(), Some(2 + 1 + 4 + 1));
        assert_eq!(
            s.detail().unwrap(),
            "Full reset: 2 of 3 left, until 2026-12-30\n\
             Weekly reset: 1 of 1 left, until 2026-11-30\n\
             5-hour reset: 4 left\n\
             Weekly reset: 1 left, not now (cooldown)"
        );
        // The 5-hour one does not clear the week, and the cooling one is not usable now.
        assert_eq!(s.weekly_pick(), (Some("soon".into()), 3));
    }

    #[test]
    fn a_paused_or_spent_grant_is_never_picked() {
        let s = status(serde_json::json!({ "grants": [
            { "id": "p", "resets_left": 2, "clears": ["seven_day"], "usable_now": true, "paused": true },
            { "id": "z", "resets_left": 0, "clears": ["seven_day"], "usable_now": true },
        ]}));
        assert_eq!(s.weekly_pick(), (None, 0));
        assert_eq!(s.credits(), Some(2));
        assert!(s.detail().unwrap().starts_with("Reset: 2 left, paused"));
    }

    #[test]
    fn claim_reply_parses() {
        let r: ClaimReply = serde_json::from_str(
            r#"{"result":"reset","reason":null,"resets_left":1,"cleared":["seven_day"]}"#,
        )
        .unwrap();
        assert_eq!(r.result.as_deref(), Some("reset"));
        assert_eq!(r.resets_left, Some(1));
    }
}
