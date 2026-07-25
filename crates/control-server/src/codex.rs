//! Codex (OpenAI/ChatGPT) usage parsing for the group-proxy usage poller — the sibling of
//! `claude.rs`.
//!
//! Under the group-proxy model (see
//! `docs/superpowers/specs/2026-07-19-cliproxy-group-proxy-plan.md`) RMNG no longer owns
//! Codex tokens: CLIProxyAPI holds each account's OAuth credentials in its per-group
//! `auth-dir`, refreshes them, and selects an account per request. RMNG only *reads* the
//! current access token + ChatGPT account id out of the `auth-dir` to display usage. This
//! module keeps just the usage-fetch + response-parsing chain:
//! [`crate::cliproxy::run_usage_poller`] enumerates the `auth-dir` credentials and calls
//! [`fetch_usage_view`] for each, which hits ChatGPT's `/wham/usage` endpoint with the
//! (unrefreshed) token and maps the response into a token-free [`ClaudeUsage`]. An expired
//! token → 401 → an `Err` the caller surfaces as stale.

use std::time::Duration;

use anyhow::{Result, bail};
use serde::Deserialize;
use wire::{ClaudeUsage, ClaudeUsageWindow};

use crate::clone_ops::{now_ms, snippet};

const USAGE_URL: &str = "https://chatgpt.com/backend-api/wham/usage";
const FETCH_TIMEOUT: Duration = Duration::from_secs(10);

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

/// Map a raw `/wham/usage` payload into a token-free [`ClaudeUsage`] keyed by an explicit
/// group-scoped id/email (`<group>|<email>`). Centralizes the window / reset-credit parsing.
fn usage_from_raw(id: String, email: String, active: bool, raw: RawUsage) -> ClaudeUsage {
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
    let _ = raw.plan_type; // plan is not part of the usage view
    let reset_credits = raw
        .rate_limit_reset_credits
        .as_ref()
        .and_then(|c| c.available_count);
    ClaudeUsage {
        id,
        email,
        provider: Some(wire::Provider::Codex),
        active,
        assignable: None,
        error: None,
        stale: None,
        last_updated: now_ms(),
        five_hour,
        seven_day,
        fable: None, // Fable is a Claude-model limit; never present for Codex.
        spend: None,
        reset_credits,
    }
}

/// Fetch usage for a raw access token + account id and map it into a [`ClaudeUsage`] view —
/// the group-proxy usage poller's entry point. Does NOT refresh the token (CLIProxyAPI owns
/// refresh under the group-proxy model); a 401 surfaces as an `Err` the caller marks stale.
pub(crate) async fn fetch_usage_view(
    http: &reqwest::Client,
    id: String,
    email: String,
    active: bool,
    token: &str,
    account_id: &str,
) -> Result<ClaudeUsage> {
    let raw = fetch_usage(http, token, account_id).await?;
    Ok(usage_from_raw(id, email, active, raw))
}

#[cfg(test)]
mod tests {
    use super::*;

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
        let u = usage_from_raw("g|z@o".into(), "z@o".into(), false, serde_json::from_str(body).unwrap());
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
        let u2 = usage_from_raw("g|z@o".into(), "z@o".into(), false, serde_json::from_str(swapped).unwrap());
        assert_eq!(u2.five_hour.as_ref().unwrap().pct, 12.0);
        assert_eq!(u2.seven_day.as_ref().unwrap().pct, 3.0);
        assert!(u2.five_hour.as_ref().unwrap().resets_at.is_none());
    }

    #[test]
    fn to_usage_reads_reset_credits() {
        let raw: RawUsage = serde_json::from_str(
            r#"{"plan_type":"pro","rate_limit":{"secondary_window":{"used_percent":96,"limit_window_seconds":604800,"reset_at":1783392770}},"rate_limit_reset_credits":{"available_count":4}}"#,
        )
        .unwrap();
        let u = usage_from_raw("g|z@o".into(), "z@o".into(), true, raw);
        assert_eq!(u.reset_credits, Some(4));
        assert_eq!(u.seven_day.unwrap().pct, 96.0);
    }

    #[test]
    fn to_usage_absent_reset_credits_is_none() {
        let raw: RawUsage = serde_json::from_str(r#"{"rate_limit":{}}"#).unwrap();
        assert_eq!(usage_from_raw("g|z@o".into(), "z@o".into(), true, raw).reset_credits, None);
    }
}
