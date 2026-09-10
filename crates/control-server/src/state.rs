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
    /// Set when the last disk read failed to parse. While set, `mutate` applies
    /// changes in memory and broadcasts them but refuses to persist: writing the
    /// in-memory state over a corrupt file would destroy the fleet record for
    /// good. A later healthy read (boot or watcher reload) clears it.
    degraded: bool,
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
            std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
        }
        let (state, healthy) = read_from_disk(&path);
        let serialized_file = to_file(&state);
        // Non-`managed` clones are legacy/unmanaged rows (an old `state.json` whose
        // `ctid`/`container` keys serde dropped, or hand-added plain clones): they carry
        // no managed Docker clone and are just deletable UI rows. Surface the count so an
        // operator migrating from an older backend sees at a glance how many rows won't
        // have a live container behind them.
        let unmanaged = state.hosts.iter().filter(|h| !h.managed).count();
        tracing::info!(
            hosts = state.hosts.len(),
            unmanaged,
            selected = ?state.selected,
            "state loaded"
        );
        let (tx, _) = broadcast::channel(64);
        Ok(Self {
            inner: RwLock::new(Inner {
                state,
                serialized_file,
                degraded: !healthy,
            }),
            tx,
            path,
        })
    }

    pub fn get(&self) -> ControlState {
        self.inner.read().unwrap().state.clone()
    }

    /// Cheap read of just the selected clone id (hot path: media frame routing).
    pub fn selected(&self) -> Option<String> {
        self.inner.read().unwrap().state.selected.clone()
    }

    /// A new SSE subscriber: the current snapshot + a live receiver.
    pub fn subscribe(&self) -> (String, broadcast::Receiver<String>) {
        let inner = self.inner.read().unwrap();
        (to_sse(&inner.state), self.tx.subscribe())
    }

    /// Apply `f` to a draft, persist atomically, broadcast. Returns the new state.
    ///
    /// While `degraded` (the last disk read failed to parse) the draft is applied
    /// in memory and broadcast but NOT persisted, so a corrupt `state.json` can
    /// never be overwritten with an empty fleet.
    pub fn mutate(&self, f: impl FnOnce(&mut ControlState)) -> ControlState {
        let mut inner = self.inner.write().unwrap();
        let mut draft = inner.state.clone();
        f(&mut draft);
        let file = to_file(&draft);
        if inner.degraded {
            tracing::error!(
                "refusing to persist state while the last disk read failed; fix state.json"
            );
        } else if let Err(e) = persist(&self.path, &file) {
            tracing::error!("persist failed: {e:#}");
        }
        inner.state = draft.clone();
        inner.serialized_file = file;
        let _ = self.tx.send(to_sse(&draft));
        draft
    }

    /// Re-read from disk; broadcast only if the content genuinely changed (so our
    /// own atomic writes, which reserialize identically, are ignored).
    ///
    /// The read happens **inside** the write lock. Reading first and then taking the lock
    /// leaves a window in which a `mutate` lands between the two: its change is then
    /// overwritten in memory by the older snapshot, and the next `mutate` writes that
    /// regression back to disk. The window is small but the loss is silent, and this runs on
    /// every filesystem event on the data dir.
    fn reload_if_changed(&self) {
        let mut inner = self.inner.write().unwrap();
        let (disk, healthy) = read_from_disk(&self.path);
        if !healthy {
            // A corrupt file must never replace memory: that would blank the fleet
            // in memory, and the next mutate would once have persisted it. Stay
            // degraded (persist stays refused) until a healthy read arrives.
            if !inner.degraded {
                tracing::error!(
                    "state.json no longer parses; keeping in-memory state and refusing to persist"
                );
            }
            inner.degraded = true;
            return;
        }
        let disk_file = to_file(&disk);
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
        inner.degraded = false;
        drop(inner);
        let _ = self.tx.send(to_sse(&disk));
    }
}

/// Read the state file. Returns the state plus whether it parsed: a missing file
/// is a healthy empty state (first boot), but a present-but-unparseable file is
/// corrupt and must never be written back over.
fn read_from_disk(path: &Path) -> (ControlState, bool) {
    match std::fs::read_to_string(path) {
        Ok(s) => match serde_json::from_str(&s) {
            Ok(state) => (migrate_clone_groups(state), true),
            Err(e) => {
                tracing::error!(
                    "state.json parse error ({e}); running without persisting until it parses again"
                );
                (ControlState::default(), false)
            }
        },
        Err(_) => (ControlState::default(), true),
    }
}

/// One-shot migration of the retired per-side `group:<name>` selections into the single
/// clone-level `group` (both sides naming it → it; one side → that side; different sides →
/// the Claude side wins with a warning — only reachable from hand-written state). Runs on
/// every load but only rewrites rows that still carry a `group:` selection, so migrated
/// state passes through untouched. Persisted on the next regular save.
fn migrate_clone_groups(mut state: ControlState) -> ControlState {
    fn extract(sel: &Option<String>) -> Option<String> {
        sel.as_deref()
            .and_then(|s| s.strip_prefix("group:"))
            .map(|n| n.trim().to_string())
            .filter(|n| !n.is_empty())
    }
    for h in state.hosts.iter_mut() {
        if h.group.is_some() {
            continue;
        }
        let g1 = extract(&h.claude_selection);
        let g2 = extract(&h.codex_selection);
        let group = match (g1, g2) {
            (Some(a), Some(b)) => {
                if a != b {
                    tracing::warn!(
                        "clone {} bound to two different groups ({a:?} vs {b:?}) — keeping {a:?}",
                        h.id
                    );
                }
                Some(a)
            }
            (Some(a), None) | (None, Some(a)) => Some(a),
            (None, None) => None,
        };
        if let Some(g) = group {
            if h.claude_selection
                .as_deref()
                .is_some_and(|s| s.starts_with("group:"))
            {
                h.claude_selection = Some("auto".to_string());
            }
            if h.codex_selection
                .as_deref()
                .is_some_and(|s| s.starts_with("group:"))
            {
                h.codex_selection = Some("auto".to_string());
            }
            // A side pinned to an email keeps its pin; the group feeds its `auto` side(s).
            h.group = Some(g);
        }
        // Retired `"none"` (no explicit tokenless state anymore): the side rejoins `auto`
        // and resolves in scope. Lossy by design — a side whose scope holds provider
        // accounts gets a token where it previously had none.
        for sel in [&mut h.claude_selection, &mut h.codex_selection] {
            if sel
                .as_deref()
                .is_some_and(|s| s.eq_ignore_ascii_case("none"))
            {
                *sel = Some("auto".to_string());
            }
        }
    }
    state
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
    let dir = path
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."));
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
    #[test]
    fn load_migrates_group_selections_into_the_shared_group() {
        let mut state = ControlState::default();
        state.hosts = vec![
            wire::RmngClone {
                id: "both".into(),
                claude_selection: Some("group:team".into()),
                codex_selection: Some("group:team".into()),
                ..Default::default()
            },
            wire::RmngClone {
                id: "split".into(),
                claude_selection: Some("group:a".into()),
                codex_selection: Some("group:b".into()),
                ..Default::default()
            },
            wire::RmngClone {
                id: "pinned".into(),
                claude_selection: Some("me@x.com".into()),
                codex_selection: Some("group:team".into()),
                ..Default::default()
            },
        ];
        let out = migrate_clone_groups(state);
        let by_id = |id: &str| out.hosts.iter().find(|h| h.id == id).unwrap();
        let both = by_id("both");
        assert_eq!(both.group.as_deref(), Some("team"));
        assert_eq!(both.claude_selection.as_deref(), Some("auto"));
        assert_eq!(both.codex_selection.as_deref(), Some("auto"));
        // Conflict → the Claude side wins.
        assert_eq!(by_id("split").group.as_deref(), Some("a"));
        // An explicit pin survives; the group feeds the other side.
        let pinned = by_id("pinned");
        assert_eq!(pinned.group.as_deref(), Some("team"));
        assert_eq!(pinned.claude_selection.as_deref(), Some("me@x.com"));
        assert_eq!(pinned.codex_selection.as_deref(), Some("auto"));
    }

    #[test]
    fn load_migrates_legacy_none_selections_to_auto() {
        let mut state = ControlState::default();
        state.hosts = vec![wire::RmngClone {
            id: "tokenless".into(),
            claude_selection: Some("none".into()),
            codex_selection: Some("NONE".into()),
            ..Default::default()
        }];
        let out = migrate_clone_groups(state);
        let h = &out.hosts[0];
        assert_eq!(h.claude_selection.as_deref(), Some("auto"));
        assert_eq!(h.codex_selection.as_deref(), Some("auto"));
        assert_eq!(h.group, None);
    }

    /// The one thing that makes a downgrade during an outage survivable.
    ///
    /// An older binary has no `Unknown` variant and no `#[serde(other)]`, and serde fails the
    /// WHOLE document on an unknown variant — so a single `"monitorState": "unknown"` anywhere
    /// would send it to `ControlState::default()`, and the next mutation would persist an empty
    /// fleet. The fourth state rides a new FIELD, which old parsers ignore, and the enum keeps
    /// its three-word vocabulary.
    #[test]
    fn an_unknown_clone_serializes_as_idle_with_a_flag_beside_it() {
        let mut state = ControlState::default();
        state.hosts = vec![wire::RmngClone {
            id: "a".into(),
            monitor_state: Some(wire::MonitorState::Unknown),
            activity_unknown: true,
            ..Default::default()
        }];
        for body in [to_file(&state), to_sse(&state)] {
            let v: serde_json::Value = serde_json::from_str(&body).unwrap();
            let host = &v["hosts"][0];
            assert_eq!(
                host["monitorState"], "idle",
                "the wire vocabulary is unchanged"
            );
            assert_eq!(
                host["activityUnknown"], true,
                "and the truth rides beside it"
            );
        }
        // The in-memory reading is untouched by writing it out.
        assert_eq!(
            state.hosts[0].monitor_state,
            Some(wire::MonitorState::Unknown)
        );
    }

    /// A clone that is genuinely idle must not be mistaken for one we cannot read.
    #[test]
    fn a_real_idle_carries_no_flag() {
        let mut state = ControlState::default();
        state.hosts = vec![wire::RmngClone {
            id: "a".into(),
            monitor_state: Some(wire::MonitorState::Idle),
            ..Default::default()
        }];
        let v: serde_json::Value = serde_json::from_str(&to_file(&state)).unwrap();
        assert_eq!(v["hosts"][0]["monitorState"], "idle");
        assert_eq!(
            v["hosts"][0]["activityUnknown"], false,
            "a real idle is not a missing one"
        );
    }
    use wire::RmngClone;

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

    /// The watcher's "did someone edit this by hand?" gate is a STRING compare between what we
    /// last wrote and a reserialization of what we read back. Any map field whose iteration
    /// order is not stable across a parse round-trip silently breaks it: every one of our own
    /// writes then looks like an external edit, so the server broadcasts a redundant full state
    /// to every client and re-installs a disk snapshot out of band.
    ///
    /// `clone_tokens` is the only map in `ControlState`, and it is a `BTreeMap` for exactly this
    /// reason. Swap it to a `HashMap` and this fails from two entries upward.
    #[test]
    fn serialization_is_stable_across_a_parse_round_trip() {
        let mut state = ControlState::default();
        for i in 0..8 {
            state.clone_tokens.insert(
                format!("clone-{i}"),
                wire::CloneTokens {
                    input_tokens: i,
                    output_tokens: i,
                    fable_active: false,
                },
            );
        }
        let written = to_file(&state);
        let parsed: ControlState = serde_json::from_str(&written).unwrap();
        assert_eq!(
            to_file(&parsed),
            written,
            "reserializing a parsed state must be byte-identical, or reload_if_changed \
             treats every one of our own writes as an external edit"
        );
    }

    #[test]
    fn mutate_persists_and_reads_back() {
        let path = temp_path();
        let store = StateStore::load(path.clone()).unwrap();
        store.mutate(|s| {
            s.hosts.push(RmngClone {
                id: "h1".into(),
                host: "1.2.3.4".into(),
                port: 3389,
                ..Default::default()
            });
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
    fn legacy_state_loads_clones_as_unmanaged() {
        // A Proxmox-era state.json (clones carry the retired `ctid`, plus a top-level
        // `templates` list) loads with every clone `managed: false` — serde drops the
        // stale keys, so these are plain unmanaged rows. Guards the state-store load path
        // (the wire crate covers the serde drop; this covers our fixture-through-load).
        let path = temp_path();
        let legacy = r#"{
            "hosts": [
                { "id": "pega-old", "host": "10.0.0.9", "username": "u", "password": "p", "ctid": 5 }
            ],
            "templates": ["rmng-template"]
        }"#;
        std::fs::write(&path, legacy).unwrap();
        let store = StateStore::load(path.clone()).unwrap();
        let st = store.get();
        assert_eq!(st.hosts.len(), 1);
        assert!(!st.hosts[0].managed); // legacy ctid dropped → unmanaged
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

    /// A corrupt state file must never be overwritten with an empty fleet: while the
    /// last disk read failed to parse, `mutate` applies in memory only. A later
    /// healthy read (e.g. the operator fixing the file by hand) clears the guard
    /// and persisting resumes.
    #[test]
    fn corrupt_state_is_never_persisted_over() {
        let path = temp_path();
        std::fs::write(&path, "{ this is not json").unwrap();
        let store = StateStore::load(path.clone()).unwrap();
        // In memory the server runs on (empty, like before) and mutations apply.
        let st = store.mutate(|s| s.selected = Some("x".into()));
        assert_eq!(st.selected.as_deref(), Some("x"));
        // But the corrupt bytes on disk are untouched.
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "{ this is not json"
        );
        // The operator fixes the file by hand; the watcher reload picks it up and
        // the guard clears, so the next mutation persists again.
        std::fs::write(&path, to_file(&ControlState::default())).unwrap();
        store.reload_if_changed();
        store.mutate(|s| s.selected = Some("y".into()));
        let reloaded = StateStore::load(path.clone()).unwrap();
        assert_eq!(reloaded.get().selected.as_deref(), Some("y"));
        let _ = std::fs::remove_file(&path);
    }
}
