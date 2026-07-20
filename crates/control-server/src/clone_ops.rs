//! Side-effect-free helpers shared by the `claude` and `codex` account subsystems.
//!
//! These were private to `claude.rs` when Claude was the only provider; `codex.rs`
//! needs the identical logic, so they live here (moved verbatim — no behavior change).
//! Two are new for Codex: a hand-rolled JWT claim decoder (`jwt_claims` / `jwt_exp_ms`;
//! the Codex OAuth response carries no `expires_in`, so expiry is read from the
//! access-token JWT `exp`) and the generalized `run_clone_op` (parameterized by guest
//! script, so each provider runs its own import script).

use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Result, bail};

use crate::app::App;
use crate::docker::CLONE_USER;

/// Milliseconds since the Unix epoch (0 if the clock is before the epoch).
pub(crate) fn now_ms() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0)
}

/// The `{…}` substring of `s` (login-shell noise can wrap the JSON), else trimmed `s`.
pub(crate) fn extract_json(s: &str) -> &str {
    match (s.find('{'), s.rfind('}')) {
        (Some(a), Some(b)) if b >= a => &s[a..=b],
        _ => s.trim(),
    }
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
    fn extract_json_strips_shell_noise() {
        assert_eq!(extract_json("noise {\"a\":1} tail"), "{\"a\":1}");
        assert_eq!(extract_json("  bare text  "), "bare text");
    }

    #[test]
    fn b64url_roundtrip_via_standard_encoder() {
        // Derive base64url from the existing standard-base64 encoder (+→-, /→_, drop =).
        for sample in ["", "f", "fo", "foo", "foob", "fooba", "foobar", "?>? subtle/+bytes"] {
            let std_b64 = crate::provision::b64_encode(sample.as_bytes());
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
        let b64 = crate::provision::b64_encode(payload.as_bytes());
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
