//! Per-clone identity keys (`RMNG_PROXY_KEY`).
//!
//! Every managed clone carries a stable random bearer in its `/etc/environment`. It began life
//! as the group proxy's router credential — which is why the variable is named the way it is —
//! but it outlived that architecture as the clone's **identity token**, and two features that
//! have nothing to do with inference still depend on it:
//!
//!   - `web::resolve_parent` reads the `X-RMNG-Proxy-Key` header to auto-detect which clone is
//!     asking when a clone creates a sub clone (sent by `control-client`);
//!   - the fleet CLI's `running_inside_clone` uses its presence to pick direct clone↔clone SSH
//!     over the operator bastion jump.
//!
//! So the key survives the group-proxy revert, and deliberately **keeps its old name**: renaming
//! it would need every existing clone's `/etc/environment` rewritten plus a fallback read of the
//! old name during the transition, and any clone that had not yet reconciled would silently lose
//! sub-clone creation and clone↔clone SSH.
//!
//! Keys are read from and written to `data/cliproxy-instances.json` — also the retired name, kept
//! on purpose: an upgraded deployment's clones already hold the keys in that file, and pointing
//! at a fresh path would mint new ones and invalidate the identity of every clone in the fleet.
//! Only the `routerKeys` map is read; the rest of that file (instance ports, CLIProxyAPI secrets)
//! is ignored and rewritten away on the first mint.
//!
//! The file is `0600` — these are bearer secrets. They never enter `state.json` or `/events`.

use std::collections::HashMap;
use std::io::Read;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

/// The on-disk file. Named for the retired group proxy — see the module docs for why that is
/// deliberate. Unknown fields from the group-proxy era are dropped by serde on load.
const KEY_FILE: &str = "cliproxy-instances.json";

/// camelCase on purpose: the group-proxy era wrote this file as `routerKeys`, and every clone's
/// `/etc/environment` already holds the matching value. Reading it under any other name would
/// mint fresh keys and silently break sub-clone creation + clone↔clone SSH fleet-wide.
#[derive(Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct KeyFile {
    /// `clone_id` → its stable identity bearer (`RMNG_PROXY_KEY`).
    #[serde(default)]
    router_keys: HashMap<String, String>,
}

struct Inner {
    file: KeyFile,
    /// key → clone_id reverse index, for resolving a presented bearer.
    index: HashMap<String, String>,
    path: PathBuf,
}

/// Per-clone identity keys, hung off `App`.
pub struct CloneKeys {
    inner: Mutex<Inner>,
}

/// 32 random bytes, hex-encoded. Reads `/dev/urandom` (Linux target), so no crate dependency
/// and cryptographically strong.
fn random_token() -> String {
    let mut buf = [0u8; 32];
    match std::fs::File::open("/dev/urandom").and_then(|mut f| f.read_exact(&mut buf)) {
        Ok(()) => buf.iter().map(|b| format!("{b:02x}")).collect(),
        // Unreachable on Linux; a time-seeded fallback still beats a fixed key.
        Err(_) => {
            let n = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0);
            format!("{n:032x}{:032x}", std::process::id())
        }
    }
}

impl CloneKeys {
    pub fn load(data_dir: &str) -> Self {
        let path = std::path::Path::new(data_dir).join(KEY_FILE);
        let file: KeyFile = std::fs::read_to_string(&path)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();
        let index = file
            .router_keys
            .iter()
            .map(|(clone, key)| (key.clone(), clone.clone()))
            .collect();
        Self {
            inner: Mutex::new(Inner { file, index, path }),
        }
    }

    fn persist(inner: &Inner) {
        let Ok(body) = serde_json::to_vec_pretty(&inner.file) else {
            return;
        };
        if let Some(dir) = inner.path.parent() {
            std::fs::create_dir_all(dir).ok();
        }
        // Write + chmod via a temp file so a reader never sees a truncated file and the
        // secrets are never briefly world-readable.
        let tmp = inner.path.with_extension(format!("tmp.{}", std::process::id()));
        if std::fs::write(&tmp, &body).is_err() {
            return;
        }
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600)).ok();
        if let Err(e) = std::fs::rename(&tmp, &inner.path) {
            tracing::warn!(target: "clonekey", "persisting clone keys failed: {e}");
            std::fs::remove_file(&tmp).ok();
        }
    }

    /// This clone's identity key, minting + persisting one on first use. Stable for the clone's
    /// life: the same clone id always gets the same key back, which is what lets an existing
    /// deployment's clones keep working across this upgrade.
    pub fn mint(&self, clone_id: &str) -> String {
        let mut inner = self.inner.lock().unwrap();
        if let Some(key) = inner.file.router_keys.get(clone_id) {
            return key.clone();
        }
        let key = random_token();
        inner.file.router_keys.insert(clone_id.to_string(), key.clone());
        inner.index.insert(key.clone(), clone_id.to_string());
        Self::persist(&inner);
        key
    }

    /// Resolve a presented bearer to the owning clone id. `None` for an unknown key.
    pub fn clone_for_token(&self, token: &str) -> Option<String> {
        self.inner.lock().unwrap().index.get(token).cloned()
    }

    /// Drop a clone's key on delete, so a stale key can never identify as it again.
    pub fn forget(&self, clone_id: &str) {
        let mut inner = self.inner.lock().unwrap();
        if let Some(key) = inner.file.router_keys.remove(clone_id) {
            inner.index.remove(&key);
            Self::persist(&inner);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_dir(tag: &str) -> String {
        let dir = std::env::temp_dir().join(format!("rmng-clonekey-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir.to_string_lossy().into_owned()
    }

    #[test]
    fn mint_is_stable_per_clone_and_resolves_back() {
        let dir = tmp_dir("mint");
        let keys = CloneKeys::load(&dir);
        let a = keys.mint("clone-a");
        assert_eq!(keys.mint("clone-a"), a, "a clone's key must never change under it");
        let b = keys.mint("clone-b");
        assert_ne!(a, b, "two clones must not share an identity");
        assert_eq!(keys.clone_for_token(&a).as_deref(), Some("clone-a"));
        assert_eq!(keys.clone_for_token(&b).as_deref(), Some("clone-b"));
        assert_eq!(keys.clone_for_token("nonsense"), None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn forget_revokes_the_key() {
        let dir = tmp_dir("forget");
        let keys = CloneKeys::load(&dir);
        let a = keys.mint("clone-a");
        keys.forget("clone-a");
        assert_eq!(keys.clone_for_token(&a), None, "a deleted clone's key must not resolve");
        // A same-named clone created later gets a fresh key, not the revoked one.
        assert_ne!(keys.mint("clone-a"), a);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The upgrade path that matters: a deployment coming from the group-proxy era already has
    /// `routerKeys` in this file, and every clone's `/etc/environment` holds the matching value.
    /// Reading them back unchanged is what keeps sub-clone creation and clone↔clone SSH working
    /// across the revert; minting fresh keys would silently break both.
    #[test]
    fn adopts_existing_router_keys_and_ignores_group_proxy_fields() {
        let dir = tmp_dir("adopt");
        std::fs::write(
            std::path::Path::new(&dir).join(KEY_FILE),
            r#"{
                "instances": {"team": {"port": 9100, "inbound_key": "secret"}},
                "routerKeys": {"clone-a": "PREEXISTING"},
                "adminSecret": "another-secret"
            }"#,
        )
        .unwrap();
        let keys = CloneKeys::load(&dir);
        assert_eq!(keys.mint("clone-a"), "PREEXISTING", "an existing clone's key was rotated");
        assert_eq!(keys.clone_for_token("PREEXISTING").as_deref(), Some("clone-a"));
        // Minting a new clone rewrites the file; the retired group-proxy fields fall away.
        keys.mint("clone-b");
        let body = std::fs::read_to_string(std::path::Path::new(&dir).join(KEY_FILE)).unwrap();
        assert!(body.contains("PREEXISTING"), "rewrite must not drop existing keys");
        assert!(!body.contains("another-secret"), "retired secrets must not be rewritten back");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn keys_persist_across_a_reload_and_the_file_is_0600() {
        let dir = tmp_dir("persist");
        let a = {
            let keys = CloneKeys::load(&dir);
            keys.mint("clone-a")
        };
        let path = std::path::Path::new(&dir).join(KEY_FILE);
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "identity bearers must never be world-readable");
        let reloaded = CloneKeys::load(&dir);
        assert_eq!(reloaded.mint("clone-a"), a, "a restart must not change a clone's identity");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
