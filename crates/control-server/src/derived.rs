//! Gen-2 derived images: base Dockerfile + per-template profile lines, built into a
//! hash tag (`rmng-p-<hash(lines + static env + base digest)>`), lazily on first create
//! that needs them.
//!
//! Same inputs twice mean one build (the tag already exists — skip). One async lock per
//! tag, so parallel creates share the build instead of racing it. Build failure fails the
//! caller with the daemon's log attached; always-latest means no old-tag picking.
//!
//! Static preset env reaches the image as `ENV` lines composed at build time (the create
//! path passes the clone preset's non-dynamic vars). Dynamic per-clone keys
//! (`RMNG_CONTROL_URL`, `RMNG_PROXY_KEY`) stay create-time injects, never baked.

use std::collections::HashMap;
use std::sync::{Arc, LazyLock};

use anyhow::{Result, bail};
use tokio::sync::Mutex;
use wire::EnvVar;

use crate::app::App;

/// One in-flight build per tag. The map entry is created under a short sync lock; the
/// per-tag mutex is held across the whole build so a second create for the same tag
/// waits and then finds the image present.
static BUILD_LOCKS: LazyLock<std::sync::Mutex<HashMap<String, Arc<Mutex<()>>>>> =
    LazyLock::new(Default::default);

fn lock_for(tag: &str) -> Arc<Mutex<()>> {
    let mut map = BUILD_LOCKS.lock().unwrap();
    map.entry(tag.to_string())
        .or_insert_with(|| Arc::new(Mutex::new(())))
        .clone()
}

/// Pin a Dockerfile FROM to the base's repo digest (`<repo>@sha256:…`). The bare digest
/// alone is NOT a resolvable reference — the daemon reads `FROM sha256:…` as repository
/// `sha256` and tries to pull it — so the repo prefix is load-bearing.
fn pinned_from_ref(base: &str, digest: &str) -> String {
    let (repo, _) = crate::docker::split_reference(base);
    format!("{}@{}", repo, digest.trim())
}

/// Quote one `ENV` value for a Dockerfile line: bare when it is plain
/// (`[A-Za-z0-9_./:+-]*`), double-quoted with `\`/`"`/newline escapes otherwise.
fn env_value(value: &str) -> String {
    if !value.is_empty()
        && value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '/' | ':' | '+' | '-'))
    {
        return value.to_string();
    }
    let mut out = String::with_capacity(value.len() + 2);
    out.push('"');
    for c in value.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            _ => out.push(c),
        }
    }
    out.push('"');
    out
}

/// Render the derived Dockerfile: base digest first, then the operator's profile lines,
/// then the static env as `ENV`. Sorted by key so the same vars hash the same.
///
/// The `LABEL rmng.image=1` stamp makes derived images qualify as clone sources
/// everywhere `resolve_reference` gates on it, so fork/rebase can hand a recorded
/// derived tag (`rmng-p-*`) back as a base. It is constant, so it never changes the
/// derived tag hash (which covers only lines + static env + base digest).
pub fn render_dockerfile(base_digest: &str, profile_lines: &str, static_env: &[EnvVar]) -> String {
    let mut out = String::new();
    out.push_str("FROM ");
    out.push_str(base_digest.trim());
    out.push('\n');
    out.push_str("LABEL rmng.image=1\n");
    let lines = profile_lines.trim();
    if !lines.is_empty() {
        out.push_str(lines);
        out.push('\n');
    }
    let mut vars: Vec<(&str, &str)> = static_env
        .iter()
        .filter(|v| !v.key.is_empty())
        .map(|v| (v.key.as_str(), v.value.as_str()))
        .collect();
    vars.sort_unstable();
    for (k, v) in vars {
        out.push_str("ENV ");
        out.push_str(k);
        out.push('=');
        out.push_str(&env_value(v));
        out.push('\n');
    }
    out
}

/// Resolve the derived tag for a base reference, building on miss. `profile_lines` is the
/// template text from config; `static_env` the clone preset's non-dynamic vars.
///
/// The build pins `FROM` to the base image's current repo digest (a base release changes
/// the digest → new tag → next create auto-rebuilds). With no lines and no static env the
/// tag still hashes (stable), but the build is skipped when the base reference itself
/// already exists as a local image under the derived tag — in practice that only happens
/// when a previous build produced it.
pub async fn resolve_tag(
    app: &App,
    base_ref: &str,
    profile_lines: &str,
    static_env: &[EnvVar],
    mut on_progress: impl FnMut(&str, &str),
) -> Result<String> {
    let base = base_ref.trim();
    if base.is_empty() {
        bail!("a base image reference is required for a gen-2 clone");
    }
    // Pin FROM to the base's repo digest (`<repo>@sha256:…`) so a re-pushed base tag
    // rebuilds instead of reusing a stale tag. The bare digest is kept nowhere: it is not
    // a resolvable FROM reference (see `pinned_from_ref`).
    let from_ref = match app.docker.image_repo_digest(base).await {
        Some(d) if !d.is_empty() => pinned_from_ref(base, &d),
        _ => base.to_string(),
    };
    let pairs: Vec<(String, String)> = static_env
        .iter()
        .filter(|v| !v.key.is_empty())
        .map(|v| (v.key.clone(), v.value.clone()))
        .collect();
    let tag = wire::config::derived_tag(profile_lines, &pairs, &from_ref);

    if app.docker.image_exists(&tag).await? {
        return Ok(tag);
    }
    // One build per tag: a parallel create waits here, then finds the image present.
    let lock = lock_for(&tag);
    let _guard = lock.lock().await;
    if app.docker.image_exists(&tag).await? {
        return Ok(tag);
    }
    on_progress("build", &format!("building derived image {tag}"));
    build(app, &tag, &from_ref, profile_lines, static_env, &mut on_progress).await?;
    Ok(tag)
}

/// Warm a derived tag without creating: resolve + build on miss, discarding the tag.
/// The prebuild button calls this so the first real create finds the image present.
pub async fn prebuild(
    app: &App,
    mut on_progress: impl FnMut(&str, &str),
) -> Result<String> {
    let cfg = app.config();
    resolve_tag(
        app,
        &cfg.docker.template_reference,
        cfg.docker.profile_lines.as_deref().unwrap_or(""),
        &[],
        &mut on_progress,
    )
    .await
}

/// Build `tag` from the rendered Dockerfile via [`DockerCtl::build_derived_image`].
/// The daemon's build log streams into `on_progress` messages; any error item fails with
/// the collected log attached (surfaced inside `build_derived_image`).
async fn build(
    app: &App,
    tag: &str,
    base_digest: &str,
    profile_lines: &str,
    static_env: &[EnvVar],
    on_progress: &mut impl FnMut(&str, &str),
) -> Result<()> {
    let dockerfile = render_dockerfile(base_digest, profile_lines, static_env);
    on_progress("build", &format!("building {tag} (FROM {base_digest})"));
    app.docker
        .build_derived_image(tag, &dockerfile, |step| on_progress("build", step))
        .await?;
    on_progress("build", &format!("derived image {tag} ready"));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn from_pins_to_repo_at_digest_not_bare_digest() {
        // Regression: a bare `sha256:…` FROM makes the daemon pull repository `sha256`.
        assert_eq!(
            pinned_from_ref("pegasis0/rmng-template:latest", "sha256:abc"),
            "pegasis0/rmng-template@sha256:abc"
        );
        assert_eq!(
            pinned_from_ref("registry:5000/img:v1", "sha256:abc"),
            "registry:5000/img@sha256:abc"
        );
    }

    #[test]
    fn dockerfile_pins_base_then_lines_then_sorted_env() {
        let out = render_dockerfile(
            "repo@sha256:abc",
            "RUN apt-get install -y foo",
            &[
                EnvVar { key: "B".into(), value: "2".into() },
                EnvVar { key: "A".into(), value: "1".into() },
            ],
        );
        assert_eq!(
            out,
            "FROM repo@sha256:abc\nLABEL rmng.image=1\nRUN apt-get install -y foo\nENV A=1\nENV B=2\n"
        );
    }

    #[test]
    fn dockerfile_without_lines_or_env_is_from_plus_label() {
        assert_eq!(
            render_dockerfile("base:latest", "  \n", &[]),
            "FROM base:latest\nLABEL rmng.image=1\n"
        );
    }

    #[test]
    fn env_value_quotes_only_when_needed() {
        assert_eq!(env_value("plain-1.2:/x"), "plain-1.2:/x");
        assert_eq!(env_value("has space"), "\"has space\"");
        assert_eq!(env_value("q\"q"), "\"q\\\"q\"");
        assert_eq!(env_value("a\nb"), "\"a\\nb\"");
        assert_eq!(env_value(""), "\"\"");
    }
}
