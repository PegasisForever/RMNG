//! Claude usage parsing for the group-proxy usage poller.
//!
//! Under the group-proxy model (see
//! `docs/superpowers/specs/2026-07-19-cliproxy-group-proxy-plan.md`) RMNG no longer owns
//! Claude tokens: CLIProxyAPI holds each account's OAuth credentials in its per-group
//! `auth-dir`, refreshes them, and selects an account per request. RMNG only *reads* the
//! current access token out of the `auth-dir` to display usage. This module keeps just the
//! usage-fetch + response-parsing chain: [`crate::cliproxy::run_usage_poller`] enumerates the
//! `auth-dir` credentials and calls [`fetch_usage_view`] for each, which hits Anthropic's
//! `/oauth/usage` endpoint with the (unrefreshed) token and maps the response into a
//! token-free [`ClaudeUsage`]. An expired token → 401 → an `Err` the caller surfaces as stale
//! (CLIProxyAPI refreshes it on the next proxied request).

use std::time::Duration;

use anyhow::{Result, bail};
use serde::Deserialize;
use wire::{ClaudeSpend, ClaudeUsage, ClaudeUsageWindow};

use crate::clone_ops::{now_ms, snippet};

const USAGE_URL: &str = "https://api.anthropic.com/api/oauth/usage";
const OAUTH_BETA_HEADER: &str = "oauth-2025-04-20";
const USER_AGENT: &str = "claude-swap/1.0";
const FETCH_TIMEOUT: Duration = Duration::from_secs(10);

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
/// The model a scoped limit applies to, e.g. `{ "display_name": "Fable" }`.
#[derive(Deserialize)]
struct RawLimitModel {
    #[serde(default)]
    display_name: Option<String>,
}
#[derive(Deserialize)]
struct RawLimitScope {
    #[serde(default)]
    model: Option<RawLimitModel>,
}
/// One entry of the `limits` array. The Fable weekly cap only appears here (as a
/// `weekly_scoped` limit whose `scope.model.display_name` is "Fable") — there is no
/// top-level `fable` field, so we read it out of this list.
#[derive(Deserialize)]
struct RawLimit {
    #[serde(default)]
    percent: Option<f64>,
    #[serde(default)]
    resets_at: Option<String>,
    #[serde(default)]
    scope: Option<RawLimitScope>,
}
#[derive(Deserialize)]
struct RawUsage {
    #[serde(default)]
    five_hour: Option<RawWindow>,
    #[serde(default)]
    seven_day: Option<RawWindow>,
    #[serde(default)]
    extra_usage: Option<RawExtra>,
    #[serde(default)]
    limits: Vec<RawLimit>,
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

/// Pull the Fable model-scoped weekly limit out of the `limits` array, matching on the
/// scope's model display name (robust to a version suffix like "Fable 5"). `None` when
/// the account has no Fable-scoped limit.
fn fable_window(limits: &[RawLimit]) -> Option<ClaudeUsageWindow> {
    limits
        .iter()
        .find(|l| {
            l.scope
                .as_ref()
                .and_then(|s| s.model.as_ref())
                .and_then(|m| m.display_name.as_deref())
                .is_some_and(|name| name.to_ascii_lowercase().contains("fable"))
        })
        .map(|l| ClaudeUsageWindow {
            pct: l.percent.unwrap_or(0.0).round(),
            resets_at: l.resets_at.clone(),
        })
}

/// Map a raw `/oauth/usage` payload into a token-free [`ClaudeUsage`] keyed by an explicit
/// group-scoped id/email (`<group>|<email>`). Centralizes the window / fable / spend parsing.
fn usage_from_raw(id: String, email: String, active: bool, raw: RawUsage) -> ClaudeUsage {
    let fable = fable_window(&raw.limits);
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
        id,
        email,
        provider: Some(wire::Provider::Claude),
        active,
        assignable: None,
        error: None,
        stale: None,
        last_updated: now_ms(),
        five_hour: to_window(raw.five_hour),
        seven_day: to_window(raw.seven_day),
        fable,
        spend,
        reset_credits: None,
    }
}

/// Fetch usage for a raw access token and map it into a [`ClaudeUsage`] view — the
/// group-proxy usage poller's entry point. Does NOT refresh the token (CLIProxyAPI owns
/// refresh under the group-proxy model); a 401 surfaces as an `Err` the caller marks stale.
pub(crate) async fn fetch_usage_view(
    http: &reqwest::Client,
    id: String,
    email: String,
    active: bool,
    token: &str,
) -> Result<ClaudeUsage> {
    let raw = fetch_usage(http, token).await?;
    Ok(usage_from_raw(id, email, active, raw))
}

#[cfg(test)]
mod tests {
    use super::*;

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
        let u = usage_from_raw("g|a@b".into(), "a@b".into(), true, raw);
        assert_eq!(u.five_hour.unwrap().pct, 7.0);
        assert_eq!(u.seven_day.unwrap().pct, 2.0);
        assert!(u.fable.is_none()); // no `limits` array → no fable window
        assert!(u.spend.is_none()); // extra usage disabled → no spend line
    }

    #[test]
    fn parses_fable_from_scoped_limits() {
        // The real /oauth/usage response carries the model-scoped Fable weekly cap only
        // inside `limits` (as a `weekly_scoped` entry) — never as a top-level field. Its
        // `percent` is a bare integer and `resets_at` an offset timestamp with fractional
        // seconds. The unscoped `weekly_all` entry must NOT be mistaken for it.
        let body = r#"{
            "five_hour": {"utilization": 23.0, "resets_at": "2026-07-20T01:20:00.469592+00:00"},
            "seven_day": {"utilization": 61.0, "resets_at": "2026-07-24T22:00:00.469612+00:00"},
            "limits": [
                {"kind": "weekly_all", "percent": 61, "resets_at": "2026-07-24T22:00:00.469612+00:00", "scope": null},
                {"kind": "weekly_scoped", "percent": 8, "resets_at": "2026-07-24T22:00:00.469890+00:00",
                 "scope": {"model": {"id": null, "display_name": "Fable"}, "surface": null}}
            ]
        }"#;
        let raw: RawUsage = serde_json::from_str(body).unwrap();
        let fable = usage_from_raw("g|a@b".into(), "a@b".into(), false, raw)
            .fable
            .expect("fable window present");
        assert_eq!(fable.pct, 8.0);
        assert_eq!(fable.resets_at.as_deref(), Some("2026-07-24T22:00:00.469890+00:00"));
    }
}
