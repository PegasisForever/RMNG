//! Antigravity (Google Gemini via Code Assist) usage view for the group-proxy usage poller.
//!
//! Unlike Claude/Codex, Antigravity exposes **no** per-account usage/quota endpoint RMNG can
//! poll: its quota is a weekly, complexity-weighted budget, and the only credit signal
//! (`AntigravityCreditsHint`) lives inside the running CLIProxyAPI process, populated from
//! inference responses with no management endpoint to read it. So this module makes no network
//! call — it produces a display-only *presence* row: the account appears under its group as
//! connected, with no 5h/7d/Fable bars. The account is fully usable for inference; CLIProxyAPI
//! owns its OAuth refresh and per-request selection.

use wire::ClaudeUsage;

use crate::clone_ops::now_ms;

/// Build a token-free, metric-less [`ClaudeUsage`] presence row for one authenticated
/// Antigravity (Gemini) account. There is no upstream usage endpoint, so this is a synchronous
/// constructor (no `Result`): the row exists purely so the operator can see the account is
/// connected to the group. All usage windows are `None` (see the module docs for why).
pub(crate) fn usage_view(id: String, email: String, active: bool) -> ClaudeUsage {
    ClaudeUsage {
        id,
        email,
        provider: Some(wire::Provider::Antigravity),
        active,
        assignable: None,
        error: None,
        stale: None,
        last_updated: now_ms(),
        five_hour: None,
        seven_day: None,
        fable: None,
        spend: None,
        reset_credits: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn presence_row_is_metric_less_and_tagged_antigravity() {
        let u = usage_view("grp|a@b.com".to_string(), "a@b.com".to_string(), true);
        assert_eq!(u.provider, Some(wire::Provider::Antigravity));
        assert_eq!(u.email, "a@b.com");
        assert!(u.active);
        // No pollable quota → no bars, no spend, no reset credits, no error/stale.
        assert!(u.five_hour.is_none());
        assert!(u.seven_day.is_none());
        assert!(u.fable.is_none());
        assert!(u.spend.is_none());
        assert!(u.reset_credits.is_none());
        assert!(u.error.is_none());
        assert!(u.stale.is_none());
    }
}
