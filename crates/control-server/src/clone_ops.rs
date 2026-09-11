//! Side-effect-free helpers shared by the `claude` and `codex` account subsystems.
//!
//! These were private to `claude.rs` when Claude was the only provider; `codex.rs`
//! needs the identical logic, so they live here (moved verbatim — no behavior change).
//! Two are new for Codex: a hand-rolled JWT claim decoder (`jwt_claims` / `jwt_exp_ms`;
//! the Codex OAuth response carries no `expires_in`, so expiry is read from the
//! access-token JWT `exp`).

use std::sync::{Mutex, PoisonError};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::app::App;

/// Says a poll is running, and says so again once it stops however it stops.
///
/// Both usage pollers keep a "one at a time" flag. Setting it, awaiting the poll, then
/// clearing it reads as correct and is not: an async function can be *dropped* at an await
/// and the line after it never runs. `poll_once` is awaited inside HTTP handlers
/// (`/api/claude/import` and `/api/claude/refresh`, and the Codex pair), and axum drops a
/// handler's future the moment the client disconnects. One import whose caller hung up left
/// the flag set on CT 106 at 18:21 on 2026-08-04, and every poll after it returned
/// "already polling" for the rest of the process's life.
///
/// That is not a poll being late. `fresh_access_token` has two callers and the poller is one
/// of them; the other only fires when a clone changes account. So the stuck flag stopped
/// every Claude token refresh on that host, silently, with nothing in any log.
///
/// Clearing on `Drop` is what makes the flag honest: cancelled, panicked, returned early or
/// finished, the poll is over and the next one may start.
pub(crate) struct PollGuard<'a> {
    flag: &'a Mutex<bool>,
}

/// Claim `flag` for one poll, or `None` when a poll already holds it.
pub(crate) fn try_poll(flag: &Mutex<bool>) -> Option<PollGuard<'_>> {
    let mut held = flag.lock().unwrap_or_else(PoisonError::into_inner);
    if *held {
        return None;
    }
    *held = true;
    drop(held);
    Some(PollGuard { flag })
}

impl Drop for PollGuard<'_> {
    fn drop(&mut self) {
        // Poison is stepped over rather than unwrapped. A panic that happened while the flag
        // was held would poison it, and panicking again here, during that unwind, aborts the
        // process. Leaving the flag set would be worse than the panic that set it.
        *self.flag.lock().unwrap_or_else(PoisonError::into_inner) = false;
    }
}

/// Milliseconds since the Unix epoch (0 if the clock is before the epoch).
pub(crate) fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// A short `: <prefix>` of an error body for log lines (empty stays empty).
///
/// Counted in characters, because a byte slice panics when the cut lands inside a multi-byte
/// one. Every provider error body reaches this, and the panic would take the whole poller
/// task with it (`main.rs` spawns it unsupervised), so one error page with a typographic
/// quote at the wrong offset would end token refreshes until the container restarts.
pub(crate) fn snippet(s: &str) -> String {
    if s.is_empty() {
        String::new()
    } else {
        format!(": {}", s.chars().take(120).collect::<String>())
    }
}

/// Non-cryptographic randomness from `/dev/urandom` (mirrors `files::rand_hex`),
/// enough to shuffle/tiebreak rotation; falls back to the clock.
pub(crate) fn rand_u64() -> u64 {
    use std::io::Read;
    let mut buf = [0u8; 8];
    if std::fs::File::open("/dev/urandom")
        .and_then(|mut f| f.read_exact(&mut buf))
        .is_ok()
    {
        u64::from_le_bytes(buf)
    } else {
        now_ms() as u64
    }
}

/// In-place Fisher–Yates shuffle.
pub(crate) fn shuffle<T>(v: &mut [T]) {
    for i in (1..v.len()).rev() {
        let j = (rand_u64() % (i as u64 + 1)) as usize;
        v.swap(i, j);
    }
}

/// Stable ordering rank for a provider so a merged `claude_accounts` list groups Claude
/// rows before Codex rows deterministically regardless of which poller wrote last.
fn provider_rank(p: Option<wire::Provider>) -> u8 {
    match p {
        Some(wire::Provider::Claude) => 0,
        Some(wire::Provider::Codex) => 1,
        None => 2,
    }
}

/// Publish `views` (all of `provider`) into `ControlState.claude_accounts`, replacing
/// exactly this provider's existing rows and leaving every other provider's rows intact.
/// `views` are sorted alphabetical; the combined list is then
/// stable-sorted by provider rank so grouping is deterministic. This is what lets the
/// Claude and Codex pollers coexist without clobbering each other (each poller previously
/// did `s.claude_accounts = views`, which would erase the other provider).
/// Resolve the clone-level group from an explicit `group` request field plus legacy
/// `group:<name>` per-side selections. `explicit` is the request's `group` key:
/// `Some(name)` binds, `Some("")` unbinds, `None` (key absent) keeps the legacy behavior
/// below. Without it, both sides naming the same group → that group; one side → that
/// side; different groups → the Claude side wins (warn — only reachable from a
/// hand-written request; the UI offers a single picker); neither → `inherit` (the source
/// clone's group for fork; create passes `None`). Selections carrying a `group:` prefix
/// are rewritten to `"auto"` (they resolve inside the group now).
pub(crate) fn split_group_binding(
    claude_sel: Option<String>,
    codex_sel: Option<String>,
    inherit: Option<String>,
    explicit: Option<Option<String>>,
) -> (Option<String>, Option<String>, Option<String>) {
    let rewrite = |sel: Option<String>| {
        sel.map(|s| {
            if s.trim_start().starts_with("group:") {
                "auto".to_string()
            } else {
                s
            }
        })
    };
    if let Some(g) = explicit {
        let group = g.filter(|s| !s.trim().is_empty());
        return (group, rewrite(claude_sel), rewrite(codex_sel));
    }
    fn extract(sel: &Option<String>) -> Option<String> {
        sel.as_deref()
            .and_then(|s| s.strip_prefix("group:"))
            .map(|n| n.trim().to_string())
            .filter(|n| !n.is_empty())
    }
    let g1 = extract(&claude_sel);
    let g2 = extract(&codex_sel);
    let group = match (g1, g2) {
        (Some(a), Some(b)) => {
            if a != b {
                tracing::warn!(
                    "clone bound to two different groups ({a:?} vs {b:?}) — keeping {a:?}"
                );
            }
            Some(a)
        }
        (Some(a), None) | (None, Some(a)) => Some(a),
        (None, None) => inherit,
    };
    (group, rewrite(claude_sel), rewrite(codex_sel))
}

/// The fork source both the clone modal and `POST /api/fork` resolve the same way: the
/// preset's default fork clone where it still exists and is forkable (managed, not
/// archived), else the oldest forkable clone (first in store order — clones append on
/// creation, so the head is the oldest survivor).
pub(crate) fn resolve_fork_source(app: &App, preset_default: Option<&str>) -> Option<String> {
    let st = app.store.get();
    let mut forkable = st.hosts.iter().filter(|h| h.managed && !h.archived);
    if let Some(def) = preset_default.map(str::trim).filter(|s| !s.is_empty()) {
        if forkable.clone().any(|h| h.id == def) {
            return Some(def.to_string());
        }
    }
    forkable.next().map(|h| h.id.clone())
}

/// Delete every imported account the merged pool list leaves unclaimed. An account in
/// zero groups is removed (the group tree's rule) — the existing per-provider delete path
/// settles clones onto surviving accounts, and refuses (Err) when a clone pins the account,
/// which fails the save with that reason instead of stranding the pin.
/// Reject a binding to a pool that does not exist. Without this a typo'd group would
/// resolve to nothing and the clone would sit tokenless with no error anywhere — the
/// rotator skips unknown groups silently by design (it cannot tell a deleted pool from a
/// config that failed to load).
pub(crate) fn validate_group(app: &App, group: Option<&str>) -> anyhow::Result<()> {
    if let Some(name) = group {
        anyhow::ensure!(
            app.config().groups.iter().any(|g| g.name == name),
            "unknown account pool {name:?}"
        );
    }
    Ok(())
}

pub(crate) async fn sweep_ungrouped_accounts(app: &App) -> anyhow::Result<()> {
    use std::collections::HashSet;
    let claimed: HashSet<String> = app
        .config()
        .groups
        .iter()
        .flat_map(|g| g.accounts.iter().cloned())
        .collect();
    for email in app.claude.emails() {
        if !claimed.contains(&email) {
            tracing::info!("removing ungrouped Claude account {email} (claimed by no pool)");
            crate::claude::delete_account(app, &email).await?;
        }
    }
    for email in app.codex.emails() {
        if !claimed.contains(&email) {
            tracing::info!("removing ungrouped Codex account {email} (claimed by no pool)");
            crate::codex::delete_account(app, &email).await?;
        }
    }
    Ok(())
}

pub(crate) fn replace_provider_views(
    app: &App,
    provider: wire::Provider,
    mut views: Vec<wire::ClaudeUsage>,
) {
    // An account deleted while this pass was running must not ride back in on it. A poll
    // snapshots its account list at the top and then spends a 400ms stagger and up to a 10s
    // fetch per account, which is plenty of room for a delete to land in the middle — and a
    // deleted account reappearing on screen reads as a delete that silently failed.
    let still_imported: std::collections::HashSet<String> = match provider {
        wire::Provider::Claude => app.claude.emails(),
        wire::Provider::Codex => app.codex.emails(),
    }
    .into_iter()
    .collect();
    views.retain(|u| still_imported.contains(&u.email));

    views.sort_by(|a, b| a.email.cmp(&b.email));
    let mut changes = Vec::new();
    app.store.mutate(|s| {
        let mut merged: Vec<wire::ClaudeUsage> = s
            .claude_accounts
            .iter()
            .filter(|u| u.provider != Some(provider))
            .cloned()
            .collect();
        let was: Vec<&wire::ClaudeUsage> = s
            .claude_accounts
            .iter()
            .filter(|u| u.provider == Some(provider))
            .collect();
        changes = usability_changes(&was, &views);
        merged.extend(views.iter().cloned());
        merged.sort_by_key(|u| provider_rank(u.provider));
        s.claude_accounts = merged;
    });
    for (email, reason) in changes {
        match reason {
            Some(why) => tracing::error!(
                "{provider:?} account {email} can no longer run a clone and is out of the \
                 rotation until it is signed in again: {why}"
            ),
            None => tracing::info!("{provider:?} account {email} is usable again"),
        }
    }
}

/// Accounts whose usability flipped between two published view sets, as
/// `(email, Some(reason))` for one that just went dark and `(email, None)` for one that
/// recovered. An account seen for the first time counts as a change only when it arrives
/// unusable, so a restart still reports a dead account instead of inheriting silence.
///
/// Edge-triggered on purpose: a poller that logged the state would print the same line
/// every pass, and a line printed every pass is one nobody reads.
fn usability_changes(
    was: &[&wire::ClaudeUsage],
    now: &[wire::ClaudeUsage],
) -> Vec<(String, Option<String>)> {
    let usable = |u: &wire::ClaudeUsage| u.assignable.unwrap_or(true);
    let mut out = Vec::new();
    for v in now {
        let before = was.iter().find(|u| u.email == v.email).map(|u| usable(u));
        match (before, usable(v)) {
            (Some(true) | None, false) => out.push((
                v.email.clone(),
                Some(
                    v.error
                        .clone()
                        .unwrap_or_else(|| "no reason recorded".into()),
                ),
            )),
            (Some(false), true) => out.push((v.email.clone(), None)),
            _ => {}
        }
    }
    out
}

/// Decode a JWT's payload claims (the middle `.`-delimited segment, base64url, no
/// padding) into a JSON value. `None` if the token isn't a well-formed three-segment JWT
/// or the payload isn't valid base64url-encoded JSON. Hand-rolled base64url decode — no
/// new dependency (the standard-base64 *encoder* lives in `provision::b64_encode`).
pub(crate) fn jwt_claims(token: &str) -> Option<serde_json::Value> {
    let payload = token.split('.').nth(1)?;
    let bytes = b64url_decode(payload)?;
    serde_json::from_slice(&bytes).ok()
}

/// The `exp` claim (seconds since epoch) of `token`, as epoch **milliseconds**. `None`
/// if the token has no numeric `exp` claim.
pub(crate) fn jwt_exp_ms(token: &str) -> Option<i64> {
    let exp = jwt_claims(token)?.get("exp")?.as_i64()?;
    Some(exp * 1000)
}

/// Decode base64url (RFC 4648 §5: `-`/`_`, padding optional). `None` on any invalid
/// character or a truncated 1-char final quantum.
fn b64url_decode(s: &str) -> Option<Vec<u8>> {
    fn val(c: u8) -> Option<u8> {
        match c {
            b'A'..=b'Z' => Some(c - b'A'),
            b'a'..=b'z' => Some(c - b'a' + 26),
            b'0'..=b'9' => Some(c - b'0' + 52),
            b'-' => Some(62),
            b'_' => Some(63),
            _ => None,
        }
    }
    let s = s.trim_end_matches('=').as_bytes();
    let mut out = Vec::with_capacity(s.len() * 3 / 4);
    for c in s.chunks(4) {
        if c.len() == 1 {
            return None; // a lone trailing char is not valid base64
        }
        let b0 = val(c[0])?;
        let b1 = val(c[1])?;
        out.push((b0 << 2) | (b1 >> 4));
        if c.len() >= 3 {
            let b2 = val(c[2])?;
            out.push(((b1 & 0x0f) << 4) | (b2 >> 2));
            if c.len() == 4 {
                let b3 = val(c[3])?;
                out.push(((b2 & 0x03) << 6) | b3);
            }
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn split_group_binding_prefers_the_explicit_group() {
        // Explicit bind wins; legacy prefixes in the sides are neutralized to auto.
        let (g, claude, codex) = split_group_binding(
            Some("group:old".into()),
            Some("me@x.com".into()),
            Some("old".into()),
            Some(Some("new".into())),
        );
        assert_eq!(g.as_deref(), Some("new"));
        assert_eq!(claude.as_deref(), Some("auto"));
        assert_eq!(codex.as_deref(), Some("me@x.com"));
        // Explicit unbind clears even an inherited group.
        let (g, _, _) =
            split_group_binding(None, None, Some("old".into()), Some(Some(String::new())));
        assert_eq!(g, None);
        // Absent key keeps the legacy behavior: prefixes bind, else inherit.
        let (g, claude, _) =
            split_group_binding(Some("group:team".into()), None, Some("other".into()), None);
        assert_eq!(g.as_deref(), Some("team"));
        assert_eq!(claude.as_deref(), Some("auto"));
        let (g, _, _) = split_group_binding(None, None, Some("other".into()), None);
        assert_eq!(g.as_deref(), Some("other"));
    }
    use base64::Engine;
    use base64::engine::general_purpose::STANDARD as B64;

    /// A provider error body is arbitrary bytes from the network, and cutting it at byte 120
    /// panicked whenever a multi-byte character straddled that offset. The panic landed in
    /// the usage poller, which runs unsupervised, so it ended every token refresh in the
    /// process.
    #[test]
    fn a_long_error_body_is_cut_on_a_character_boundary() {
        let body = format!("{}\u{201c}invalid_grant\u{201d}", "x".repeat(119));
        let out = snippet(&body);
        assert_eq!(
            out.chars().count(),
            122,
            "the `: ` prefix plus 120 characters"
        );
        assert!(
            out.ends_with('\u{201c}'),
            "cut after the quote, not inside it: {out}"
        );
        assert_eq!(snippet(""), "", "an empty body stays empty");
    }

    fn usage_view(email: &str, assignable: bool, error: Option<&str>) -> wire::ClaudeUsage {
        wire::ClaudeUsage {
            id: email.into(),
            email: email.into(),
            provider: Some(wire::Provider::Claude),
            active: false,
            assignable: Some(assignable),
            error: error.map(str::to_string),
            stale: None,
            last_updated: 0,
            five_hour: None,
            seven_day: None,
            fable: None,
            spend: None,
            reset_credits: None,
        }
    }

    #[test]
    fn a_dead_account_is_reported_once_and_again_when_it_comes_back() {
        let live = usage_view("a@x", true, None);
        let dead = usage_view("a@x", false, Some("refresh 400: invalid_grant"));

        // Falling over reports the reason.
        let out = usability_changes(&[&live], std::slice::from_ref(&dead));
        assert_eq!(
            out,
            vec![(
                "a@x".to_string(),
                Some("refresh 400: invalid_grant".to_string())
            )]
        );
        // Every pass after that is silent: the poller runs every few minutes, and a line
        // printed every pass is one nobody reads.
        assert!(usability_changes(&[&dead], std::slice::from_ref(&dead)).is_empty());
        // Recovery is one line too.
        assert_eq!(
            usability_changes(&[&dead], vec![live.clone()].as_slice()),
            vec![("a@x".to_string(), None)]
        );
        assert!(usability_changes(&[&live], std::slice::from_ref(&live)).is_empty());
    }

    #[test]
    fn an_account_that_is_dead_the_first_time_it_is_seen_is_reported() {
        // A restart publishes into an empty set. Treating "unseen" as usable would swallow
        // the one line that says the fleet lost an account while the server was down.
        let dead = usage_view("a@x", false, Some("refresh 400: invalid_grant"));
        assert_eq!(usability_changes(&[], std::slice::from_ref(&dead)).len(), 1);
        let live = usage_view("a@x", true, None);
        assert!(usability_changes(&[], std::slice::from_ref(&live)).is_empty());
    }

    #[test]
    fn one_poll_at_a_time() {
        let flag = Mutex::new(false);
        let first = try_poll(&flag).expect("nothing was polling");
        assert!(
            try_poll(&flag).is_none(),
            "a second poll started while the first ran"
        );
        drop(first);
        assert!(
            try_poll(&flag).is_some(),
            "the flag outlived the poll that set it"
        );
    }

    /// The regression. The flag used to be cleared on the line after `poll_inner(app).await`,
    /// which an async cancellation never reaches: axum drops a handler's future when its
    /// client disconnects. One import whose caller hung up stopped Claude usage polling, and
    /// with it every token refresh, until the process was restarted.
    #[tokio::test]
    async fn a_cancelled_poll_releases_the_flag() {
        let flag = Mutex::new(false);
        let (started, mut wait) = tokio::sync::mpsc::channel::<()>(1);

        let poll = async {
            let Some(_guard) = try_poll(&flag) else {
                return;
            };
            started.send(()).await.unwrap();
            // Never finishes, standing in for a poll still awaiting Anthropic when the
            // client goes away.
            std::future::pending::<()>().await;
        };

        // `select!` drops the losing branch, which is exactly what axum does to a handler.
        tokio::select! {
            _ = poll => unreachable!("the pending future cannot finish"),
            _ = wait.recv() => {}
        }

        assert!(!*flag.lock().unwrap(), "a cancelled poll left the flag set");
        assert!(
            try_poll(&flag).is_some(),
            "the next poll was locked out forever"
        );
    }

    /// A minimal imported Codex account. Only the id and the email are read by anything under
    /// test here; the tokens exist because the struct has no default.
    fn imported_codex(email: &str) -> crate::codex::StoredCodexAccount {
        crate::codex::StoredCodexAccount {
            id: format!("codex:{email}"),
            email: email.into(),
            account_id: email.into(),
            plan: String::new(),
            active: false,
            access_token: String::new(),
            id_token: String::new(),
            refresh_token: String::new(),
            expires_at: now_ms() + 60 * 60 * 1000,
            last_refresh: None,
        }
    }

    #[test]
    fn replace_provider_views_preserves_other_provider() {
        use wire::{ClaudeUsage, Provider};
        fn view(email: &str, provider: Provider) -> ClaudeUsage {
            ClaudeUsage {
                id: format!("{email}|{provider:?}"),
                email: email.into(),
                provider: Some(provider),
                active: false,
                assignable: Some(true),
                error: None,
                stale: None,
                last_updated: 0,
                five_hour: None,
                seven_day: None,
                fable: None,
                spend: None,
                reset_credits: None,
            }
        }
        let app = crate::app::App::test_app();
        // Import first. A publish now drops any account the store no longer holds, so that a
        // delete landing mid-poll is not undone by the pass it interrupted — which makes an
        // unimported email a row that cannot be published at all.
        for email in ["z@o", "y@o"] {
            crate::codex::upsert_account(&app, imported_codex(email)).unwrap();
        }
        // Seed: two claude, one codex.
        app.store.mutate(|s| {
            s.claude_accounts = vec![
                view("a@c", Provider::Claude),
                view("b@c", Provider::Claude),
                view("z@o", Provider::Codex),
            ];
        });
        // A codex poll publishes a new codex set, sorted alphabetical.
        replace_provider_views(
            &app,
            Provider::Codex,
            vec![view("z@o", Provider::Codex), view("y@o", Provider::Codex)],
        );
        let st = app.store.get();
        // Both claude rows still present.
        assert_eq!(
            st.claude_accounts
                .iter()
                .filter(|u| u.provider == Some(Provider::Claude))
                .count(),
            2
        );
        // Codex rows are the new set, alphabetical.
        let codex: Vec<_> = st
            .claude_accounts
            .iter()
            .filter(|u| u.provider == Some(Provider::Codex))
            .map(|u| u.email.as_str())
            .collect();
        assert_eq!(codex, vec!["y@o", "z@o"]);
        // An empty codex publish drops all codex rows but keeps claude.
        replace_provider_views(&app, Provider::Codex, vec![]);
        let st2 = app.store.get();
        assert_eq!(st2.claude_accounts.len(), 2);
        assert!(
            st2.claude_accounts
                .iter()
                .all(|u| u.provider == Some(Provider::Claude))
        );
    }

    /// A poll snapshots its account list at the top and then spends a 400ms stagger and up to
    /// a 10s fetch per account, so a delete lands in the middle of one routinely. Publishing
    /// the pass's own snapshot would put the deleted account back on screen, which reads as a
    /// delete that silently failed.
    #[test]
    fn a_publish_cannot_resurrect_an_account_deleted_while_the_poll_ran() {
        use wire::{ClaudeUsage, Provider};
        let app = crate::app::App::test_app();
        crate::codex::upsert_account(&app, imported_codex("kept@o")).unwrap();
        crate::codex::upsert_account(&app, imported_codex("deleted@o")).unwrap();

        let views: Vec<ClaudeUsage> = ["kept@o", "deleted@o"]
            .iter()
            .map(|email| ClaudeUsage {
                id: format!("codex:{email}"),
                email: (*email).into(),
                provider: Some(Provider::Codex),
                active: false,
                assignable: Some(true),
                error: None,
                stale: None,
                last_updated: 0,
                five_hour: None,
                seven_day: None,
                fable: None,
                spend: None,
                reset_credits: None,
            })
            .collect();

        // The delete, mid-pass: the store loses the account while `views` still names it.
        crate::codex::test_delete(&app, "deleted@o");
        replace_provider_views(&app, Provider::Codex, views);

        let published: Vec<String> = app
            .store
            .get()
            .claude_accounts
            .into_iter()
            .map(|u| u.email)
            .collect();
        assert_eq!(published, vec!["kept@o".to_string()]);
    }

    #[test]
    fn b64url_roundtrip_via_standard_encoder() {
        // Derive base64url from the existing standard-base64 encoder (+→-, /→_, drop =).
        for sample in [
            "",
            "f",
            "fo",
            "foo",
            "foob",
            "fooba",
            "foobar",
            "?>? subtle/+bytes",
        ] {
            let std_b64 = B64.encode(sample.as_bytes());
            let url = std_b64
                .trim_end_matches('=')
                .replace('+', "-")
                .replace('/', "_");
            assert_eq!(
                b64url_decode(&url).unwrap(),
                sample.as_bytes(),
                "sample {sample:?}"
            );
        }
        // Invalid input rejected.
        assert!(b64url_decode("A").is_none());
        assert!(b64url_decode("****").is_none());
    }

    #[test]
    fn jwt_claims_and_exp() {
        let payload = r#"{"exp":2000000000,"email":"a@openai.com","https://api.openai.com/auth":{"chatgpt_plan_type":"plus","chatgpt_account_id":"acc-1"}}"#;
        let b64 = B64.encode(payload.as_bytes());
        let url = b64
            .trim_end_matches('=')
            .replace('+', "-")
            .replace('/', "_");
        let jwt = format!("eyJhbGciOiJub25lIn0.{url}.sig");
        let claims = jwt_claims(&jwt).unwrap();
        assert_eq!(claims["email"], "a@openai.com");
        assert_eq!(
            claims["https://api.openai.com/auth"]["chatgpt_plan_type"],
            "plus"
        );
        assert_eq!(
            claims["https://api.openai.com/auth"]["chatgpt_account_id"],
            "acc-1"
        );
        assert_eq!(jwt_exp_ms(&jwt), Some(2_000_000_000_000));
        // Non-JWT input yields no claims.
        assert!(jwt_claims("not-a-jwt").is_none());
        assert!(jwt_exp_ms("a.b").is_none());
    }
}
