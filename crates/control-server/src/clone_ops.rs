//! Side-effect-free helpers shared by the `claude` and `codex` account subsystems.
//!
//! These were private to `claude.rs` when Claude was the only provider; `codex.rs`
//! needs the identical logic, so they live here (moved verbatim — no behavior change).
//! Two are new for Codex: a hand-rolled JWT claim decoder (`jwt_claims` / `jwt_exp_ms`;
//! the Codex OAuth response carries no `expires_in`, so expiry is read from the
//! access-token JWT `exp`) and the generalized `run_clone_op` (parameterized by guest
//! script, so each provider runs its own import script).

use std::sync::{Mutex, PoisonError};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Result, bail};

use crate::app::App;
use crate::docker::CLONE_USER;

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
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0)
}


/// A short `: <prefix>` of an error body for log lines (empty stays empty).
pub(crate) fn snippet(s: &str) -> String {
    if s.is_empty() { String::new() } else { format!(": {}", &s[..s.len().min(120)]) }
}

/// Non-cryptographic randomness from `/dev/urandom` (mirrors `files::rand_hex`),
/// enough to shuffle/tiebreak rotation; falls back to the clock.
pub(crate) fn rand_u64() -> u64 {
    use std::io::Read;
    let mut buf = [0u8; 8];
    if std::fs::File::open("/dev/urandom").and_then(|mut f| f.read_exact(&mut buf)).is_ok() {
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
/// `views` are sorted pinned-email-first then alphabetical; the combined list is then
/// stable-sorted by provider rank so grouping is deterministic. This is what lets the
/// Claude and Codex pollers coexist without clobbering each other (each poller previously
/// did `s.claude_accounts = views`, which would erase the other provider).
pub(crate) fn replace_provider_views(
    app: &App,
    provider: wire::Provider,
    mut views: Vec<wire::ClaudeUsage>,
    pinned: Option<&str>,
) {
    views.sort_by(|a, b| {
        let ap = Some(a.email.as_str()) == pinned;
        let bp = Some(b.email.as_str()) == pinned;
        if ap != bp {
            return if ap { std::cmp::Ordering::Less } else { std::cmp::Ordering::Greater };
        }
        a.email.cmp(&b.email)
    });
    app.store.mutate(|s| {
        let mut merged: Vec<wire::ClaudeUsage> =
            s.claude_accounts.iter().filter(|u| u.provider != Some(provider)).cloned().collect();
        merged.extend(views.iter().cloned());
        merged.sort_by_key(|u| provider_rank(u.provider));
        s.claude_accounts = merged;
    });
}

/// Run one import-script op (`status`|`read`|`clear`|`apply`) inside clone `container`
/// via `docker exec bash -s`, returning its raw stdout+stderr. `script` is the guest
/// script body (`include_str!`); `extra` are extra positional args (e.g. the base64
/// credentials for `apply`). Script args: `<user> <op> [extra…]`. Generalized from the
/// original claude-only `provision::run_clone_op` so each provider passes its own script.
pub(crate) async fn run_clone_op(
    app: &App,
    container: &str,
    script: &str,
    op: &str,
    extra: &[&str],
) -> Result<String> {
    let mut args: Vec<String> = vec![CLONE_USER.to_string(), op.to_string()];
    args.extend(extra.iter().map(|s| s.to_string()));

    let mut out = String::new();
    let code = app
        .docker
        .exec_script(container, script, &[], &args, |_stream, line| {
            out.push_str(line);
            out.push('\n');
        })
        .await?;

    if code == 0 {
        Ok(out)
    } else {
        bail!("clone op '{op}' failed in {container} (exit {code}): {}", out.trim());
    }
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
    use base64::Engine;
    use base64::engine::general_purpose::STANDARD as B64;

    #[test]
    fn one_poll_at_a_time() {
        let flag = Mutex::new(false);
        let first = try_poll(&flag).expect("nothing was polling");
        assert!(try_poll(&flag).is_none(), "a second poll started while the first ran");
        drop(first);
        assert!(try_poll(&flag).is_some(), "the flag outlived the poll that set it");
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
            let Some(_guard) = try_poll(&flag) else { return };
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
        assert!(try_poll(&flag).is_some(), "the next poll was locked out forever");
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
        // Seed: two claude, one codex.
        app.store.mutate(|s| {
            s.claude_accounts =
                vec![view("a@c", Provider::Claude), view("b@c", Provider::Claude), view("z@o", Provider::Codex)];
        });
        // A codex poll publishes a new codex set (pinned y@o first).
        replace_provider_views(
            &app,
            Provider::Codex,
            vec![view("z@o", Provider::Codex), view("y@o", Provider::Codex)],
            Some("y@o"),
        );
        let st = app.store.get();
        // Both claude rows still present.
        assert_eq!(st.claude_accounts.iter().filter(|u| u.provider == Some(Provider::Claude)).count(), 2);
        // Codex rows are the new set, pinned first.
        let codex: Vec<_> = st
            .claude_accounts
            .iter()
            .filter(|u| u.provider == Some(Provider::Codex))
            .map(|u| u.email.as_str())
            .collect();
        assert_eq!(codex, vec!["y@o", "z@o"]);
        // An empty codex publish drops all codex rows but keeps claude.
        replace_provider_views(&app, Provider::Codex, vec![], None);
        let st2 = app.store.get();
        assert_eq!(st2.claude_accounts.len(), 2);
        assert!(st2.claude_accounts.iter().all(|u| u.provider == Some(Provider::Claude)));
    }


    #[test]
    fn b64url_roundtrip_via_standard_encoder() {
        // Derive base64url from the existing standard-base64 encoder (+→-, /→_, drop =).
        for sample in ["", "f", "fo", "foo", "foob", "fooba", "foobar", "?>? subtle/+bytes"] {
            let std_b64 = B64.encode(sample.as_bytes());
            let url = std_b64.trim_end_matches('=').replace('+', "-").replace('/', "_");
            assert_eq!(b64url_decode(&url).unwrap(), sample.as_bytes(), "sample {sample:?}");
        }
        // Invalid input rejected.
        assert!(b64url_decode("A").is_none());
        assert!(b64url_decode("****").is_none());
    }

    #[test]
    fn jwt_claims_and_exp() {
        let payload = r#"{"exp":2000000000,"email":"a@openai.com","https://api.openai.com/auth":{"chatgpt_plan_type":"plus","chatgpt_account_id":"acc-1"}}"#;
        let b64 = B64.encode(payload.as_bytes());
        let url = b64.trim_end_matches('=').replace('+', "-").replace('/', "_");
        let jwt = format!("eyJhbGciOiJub25lIn0.{url}.sig");
        let claims = jwt_claims(&jwt).unwrap();
        assert_eq!(claims["email"], "a@openai.com");
        assert_eq!(claims["https://api.openai.com/auth"]["chatgpt_plan_type"], "plus");
        assert_eq!(claims["https://api.openai.com/auth"]["chatgpt_account_id"], "acc-1");
        assert_eq!(jwt_exp_ms(&jwt), Some(2_000_000_000_000));
        // Non-JWT input yields no claims.
        assert!(jwt_claims("not-a-jwt").is_none());
        assert!(jwt_exp_ms("a.b").is_none());
    }
}
