//! The state store — `state.json` is the single source of truth. This process is
//! its only *intentional* writer (atomic temp-write + rename). A directory watcher
//! reloads on external hand-edits, gated on a content compare so our own writes
//! don't loop. Ports the behavior of `control-server/app/lib/state.server.ts`.

use std::path::{Path, PathBuf};
use std::sync::RwLock;
use std::time::Duration;

use anyhow::{Context, Result};
use tokio::sync::broadcast;
use wire::ControlState;

pub struct StateStore {
    inner: RwLock<Inner>,
    /// SSE bus: compact-JSON snapshots, one per change.
    tx: broadcast::Sender<String>,
    path: PathBuf,
}

struct Inner {
    state: ControlState,
    /// Canonical file serialization (pretty + trailing newline) — the watcher gate.
    serialized_file: String,
}

fn to_file(state: &ControlState) -> String {
    // Matches the Bun writer: 2-space pretty + trailing newline.
    let mut s = serde_json::to_string_pretty(state).expect("ControlState serializes");
    s.push('\n');
    s
}

fn to_sse(state: &ControlState) -> String {
    serde_json::to_string(state).expect("ControlState serializes")
}

impl StateStore {
    pub fn load(path: PathBuf) -> Result<Self> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)
                .with_context(|| format!("creating {}", dir.display()))?;
        }
        let state = read_from_disk(&path);
        let serialized_file = to_file(&state);
        tracing::info!(
            hosts = state.hosts.len(),
            selected = ?state.selected,
            "state loaded"
        );
        let (tx, _) = broadcast::channel(64);
        Ok(Self { inner: RwLock::new(Inner { state, serialized_file }), tx, path })
    }

    pub fn get(&self) -> ControlState {
        self.inner.read().unwrap().state.clone()
    }

    /// Cheap read of just the selected host id (hot path: media frame routing).
    pub fn selected(&self) -> Option<String> {
        self.inner.read().unwrap().state.selected.clone()
    }

    /// A new SSE subscriber: the current snapshot + a live receiver.
    pub fn subscribe(&self) -> (String, broadcast::Receiver<String>) {
        let inner = self.inner.read().unwrap();
        (to_sse(&inner.state), self.tx.subscribe())
    }

    /// Apply `f` to a draft, persist atomically, broadcast. Returns the new state.
    pub fn mutate(&self, f: impl FnOnce(&mut ControlState)) -> ControlState {
        let mut inner = self.inner.write().unwrap();
        let mut draft = inner.state.clone();
        f(&mut draft);
        let file = to_file(&draft);
        if let Err(e) = persist(&self.path, &file) {
            tracing::error!("persist failed: {e:#}");
        }
        inner.state = draft.clone();
        inner.serialized_file = file;
        let _ = self.tx.send(to_sse(&draft));
        draft
    }

    /// Re-read from disk; broadcast only if the content genuinely changed (so our
    /// own atomic writes, which reserialize identically, are ignored).
    fn reload_if_changed(&self) {
        let disk = read_from_disk(&self.path);
        let disk_file = to_file(&disk);
        let mut inner = self.inner.write().unwrap();
        if disk_file == inner.serialized_file {
            return;
        }
        tracing::info!(
            hosts = disk.hosts.len(),
            selected = ?disk.selected,
            "state reloaded from disk"
        );
        inner.state = disk.clone();
        inner.serialized_file = disk_file;
        drop(inner);
        let _ = self.tx.send(to_sse(&disk));
    }
}

fn read_from_disk(path: &Path) -> ControlState {
    match std::fs::read_to_string(path) {
        Ok(s) => serde_json::from_str(&s).unwrap_or_else(|e| {
            tracing::warn!("state.json parse error ({e}); using empty state");
            ControlState::default()
        }),
        Err(_) => ControlState::default(),
    }
}

fn persist(path: &Path, contents: &str) -> Result<()> {
    let tmp = path.with_extension(format!("tmp.{}", std::process::id()));
    std::fs::write(&tmp, contents).with_context(|| format!("writing {}", tmp.display()))?;
    std::fs::rename(&tmp, path).with_context(|| format!("renaming into {}", path.display()))?;
    Ok(())
}

/// Spawn a directory watcher (blocking thread) that reloads on external edits.
pub fn spawn_watcher(store: std::sync::Arc<StateStore>) {
    use notify::{Event, RecursiveMode, Watcher};

    let path = store.path.clone();
    let dir = path.parent().map(Path::to_path_buf).unwrap_or_else(|| PathBuf::from("."));
    std::thread::spawn(move || {
        let (tx, rx) = std::sync::mpsc::channel::<()>();
        let mut watcher = match notify::recommended_watcher(move |res: notify::Result<Event>| {
            if res.is_ok() {
                let _ = tx.send(());
            }
        }) {
            Ok(w) => w,
            Err(e) => {
                tracing::warn!("state watch disabled: {e}");
                return;
            }
        };
        if let Err(e) = watcher.watch(&dir, RecursiveMode::NonRecursive) {
            tracing::warn!("state watch disabled: {e}");
            return;
        }
        tracing::info!("watching {} for external state edits", dir.display());
        // Debounce: coalesce bursts, then reload once.
        loop {
            if rx.recv().is_err() {
                break;
            }
            while rx.recv_timeout(Duration::from_millis(150)).is_ok() {}
            store.reload_if_changed();
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use wire::Host;

    fn temp_path() -> PathBuf {
        use std::sync::atomic::{AtomicU32, Ordering};
        static N: AtomicU32 = AtomicU32::new(0);
        let mut p = std::env::temp_dir();
        p.push(format!(
            "rmng-state-test-{}-{}.json",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_file(&p);
        p
    }

    #[test]
    fn mutate_persists_and_reads_back() {
        let path = temp_path();
        let store = StateStore::load(path.clone()).unwrap();
        store.mutate(|s| {
            s.hosts.push(Host { id: "h1".into(), host: "1.2.3.4".into(), port: 3389, ..Default::default() });
            s.selected = Some("h1".into());
        });
        // round-trips from disk
        let reloaded = StateStore::load(path.clone()).unwrap();
        let st = reloaded.get();
        assert_eq!(st.hosts.len(), 1);
        assert_eq!(st.selected.as_deref(), Some("h1"));
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn subscribe_gets_current_snapshot() {
        let path = temp_path();
        let store = StateStore::load(path.clone()).unwrap();
        store.mutate(|s| s.selected = Some("x".into()));
        let (snapshot, _rx) = store.subscribe();
        let parsed: ControlState = serde_json::from_str(&snapshot).unwrap();
        assert_eq!(parsed.selected.as_deref(), Some("x"));
        let _ = std::fs::remove_file(&path);
    }
}
