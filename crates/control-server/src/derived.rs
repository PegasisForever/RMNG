//! Gen-2 derived images: each preset carries its own FULL Dockerfile, built into a
//! hash tag (`rmng-p-<hash(file text)>`), lazily on first create that needs it.
//!
//! Same text twice means one build (the tag already exists — skip). Same text NEVER
//! rebuilds: a base release under the same tag does not invalidate it. Refresh is
//! manual — edit the Dockerfile (any text change re-tags) or hit the preset's rebuild
//! button (prebuild). One async lock per tag, so parallel creates share the build
//! instead of racing it. Build failure fails the caller with the daemon's log attached.
//!
//! The Dockerfile is used VERBATIM: no FROM rewrite, no digest pinning, no appended
//! ENV. It may name any image, not only clone sources. Secrets (ENV lines) are baked
//! into the layers — accepted: anyone with daemon access can read them. The Linear key
//! stays OUT of the file: it remains a preset field, injected at runtime.

use std::collections::HashMap;
use std::sync::{Arc, LazyLock};

use anyhow::{Result, bail};
use tokio::sync::Mutex;

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

/// Ensure the image for a preset Dockerfile: tag = hash of the file text, build the
/// text verbatim on miss. Empty text falls back to the default base Dockerfile.
pub async fn ensure_image(
    app: &App,
    dockerfile: &str,
    mut on_progress: impl FnMut(&str, &str),
) -> Result<String> {
    let text = dockerfile.trim();
    let text = if text.is_empty() {
        "FROM pegasis0/rmng-template:latest"
    } else {
        text
    };
    let tag = wire::config::dockerfile_tag(text);

    if app.docker.image_exists(&tag).await? {
        return Ok(tag);
    }
    // One build per tag: a parallel create waits here, then finds the image present.
    let lock = lock_for(&tag);
    let _guard = lock.lock().await;
    if app.docker.image_exists(&tag).await? {
        return Ok(tag);
    }
    on_progress("build", &format!("building preset image {tag}"));
    app.docker
        .build_derived_image(&tag, text, |step| on_progress("build", step))
        .await?;
    on_progress("build", &format!("preset image {tag} ready"));
    Ok(tag)
}

/// Warm a preset image without creating: ensure + build on miss, discarding the tag.
/// The preset card's rebuild button calls this so the first real create finds the
/// image present. Takes the editor's current text (which may be unsaved); saving is
/// separate.
pub async fn prebuild(
    app: &App,
    dockerfile: &str,
    mut on_progress: impl FnMut(&str, &str),
) -> Result<String> {
    if dockerfile.trim().is_empty() {
        bail!("a Dockerfile is required to prebuild");
    }
    ensure_image(app, dockerfile, &mut on_progress).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tag_is_stable_for_same_text() {
        assert_eq!(
            wire::config::dockerfile_tag("FROM x:latest\n"),
            wire::config::dockerfile_tag("FROM x:latest")
        );
    }

    #[test]
    fn tag_changes_with_any_text_change() {
        assert_ne!(
            wire::config::dockerfile_tag("FROM x:latest"),
            wire::config::dockerfile_tag("FROM x:latest\nRUN foo")
        );
    }
}
