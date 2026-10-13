//! The orphan home sweep: destroy every home dataset that no clone owns any more.
//!
//! A clone delete removes the row even when its dataset destroy fails, and before this
//! sweep nothing ever came back for the dataset. Two failures made that common:
//!
//! - **Fork parents.** A fork is a `zfs clone` of `<parent>@fork-<child>-<ts>`, so the
//!   parent cannot be destroyed while any fork child lives. Deleting the parent first
//!   kept its dataset, and deleting the children later never revisited it — a whole
//!   chain of forks stayed on disk. This sweep runs after every delete, so the parent
//!   goes as soon as its last child does, and the chain unwinds leaf-first.
//! - **Busy mounts.** Copies of a home's mounts in other mount namespaces keep the
//!   dataset busy. [`crate::clone_home::CloneHome::destroy`] now detaches those and
//!   retries; a home that is still busy is retried on the next sweep.
//!
//! The sweep is reactive, not timed. Only a delete makes a new orphan (its row goes), so
//! every delete runs one. The boot sweep catches what an older build or a crash left, and
//! a home that stayed busy, which the next delete or boot retries. Nothing else wakes it.
//!
//! Keeping the row as a "zombie" until the dataset is gone was the alternative. The
//! sweep is used instead because it also clears what older builds leaked, and because
//! it needs no row state that every list, guard and UI path would have to filter out.
//!
//! What counts as an orphan is deliberately narrow. A dataset is destroyed only when
//! ALL of these hold:
//! - it is a direct child of `docker.homes_parent` (the CT can see the whole pool),
//! - its mountpoint is the one this server pins (`/srv/rmng-homes/<id>`),
//! - no state row has its id, no container has its name, and no running operation
//!   targets or forks from it,
//! - no other dataset was cloned from it (it would fail anyway; this keeps the order).
//!
//! No age guard is needed for a dataset a create or fork is still building: the flow
//! files its operation before it makes the dataset, and the sweep lists the datasets
//! before it reads state, so such a dataset always comes with a running operation.
//!
//! The sweep refuses to run while `state.json` failed to parse (the in-memory fleet may
//! be empty then) or when the container list cannot be read. The homes parent must
//! belong to this server alone: a second server on the same parent would see the
//! first one's homes as orphans.
//!
//! The same pass drops `fork-*` snapshots that nothing was cloned from — a fork whose
//! origin-snapshot cleanup failed left one behind on its source.

use std::collections::{HashMap, HashSet};

use wire::OperationStatus;

use crate::app::App;
use crate::clone_home::CloneHome;
use crate::zfs::{HomeDataset, HomeSnapshot};

/// The snapshot prefix the fork path uses ([`crate::provision::fork_clone`]).
const FORK_SNAP_PREFIX: &str = "fork-";

/// One sweep at a time: two would race to destroy the same chain.
static SWEEP: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// The boot sweep: whatever an older build or a crash left behind. The boot task runs
/// it once the fleet is up.
pub(crate) async fn boot(app: &App) {
    sweep(app).await;
}

/// Sweep now, in the background. The delete job calls this once the row is gone, so a
/// fork parent kept only for this child goes right after it.
pub(crate) fn kick(app: &App) {
    let app = app.clone();
    tokio::spawn(async move { sweep(&app).await });
}

/// What keeps a dataset alive no matter what ZFS says.
#[derive(Default)]
struct Owners {
    /// Ids with a state row, a container, or a running operation on them.
    ids: HashSet<String>,
    /// Ids a running operation acts on (target or source): their snapshots are in use.
    busy: HashSet<String>,
}

/// The work one sweep will do, in order.
#[derive(Debug, Default, PartialEq)]
struct Plan {
    /// Orphan ids, leaf-first: no entry has a fork child later in the list.
    datasets: Vec<String>,
    /// Stray `fork-*` snapshots on datasets that stay.
    snapshots: Vec<String>,
}

async fn sweep(app: &App) {
    let _one = SWEEP.lock().await;
    let parent = app.config().docker.homes_parent.clone();
    // List the datasets BEFORE reading state. A create files its operation before it
    // makes its dataset, so any dataset in this listing that belongs to a create in
    // flight has that operation in the state read below.
    let listed = {
        let parent = parent.clone();
        tokio::task::spawn_blocking(move || {
            Ok::<_, anyhow::Error>((
                crate::zfs::list_homes(&parent)?,
                crate::zfs::list_snapshots(&parent)?,
            ))
        })
        .await
    };
    let (homes, snaps) = match listed {
        Ok(Ok(v)) => v,
        Ok(Err(e)) => return tracing::warn!(target: "home_gc", "listing {parent}: {e:#}"),
        Err(e) => return tracing::warn!(target: "home_gc", "listing {parent}: {e}"),
    };
    if app.store.is_degraded() {
        return tracing::warn!(target: "home_gc", "state.json does not parse; not sweeping");
    }
    let state = app.store.get();
    let containers = match app.docker.container_names().await {
        Ok(c) => c,
        Err(e) => return tracing::warn!(target: "home_gc", "not sweeping: {e:#}"),
    };
    let mut owners = Owners::default();
    owners.ids.extend(state.hosts.iter().map(|h| h.id.clone()));
    owners.ids.extend(containers);
    for op in state
        .operations
        .iter()
        .filter(|o| o.status == OperationStatus::Running)
    {
        owners.busy.insert(op.target.clone());
        owners.busy.extend(op.source.clone());
    }
    owners.ids.extend(owners.busy.iter().cloned());

    let plan = plan(&homes, &snaps, &owners);
    if plan == Plan::default() {
        return;
    }
    let origins: HashMap<String, Option<String>> =
        homes.into_iter().map(|h| (h.id, h.origin)).collect();
    let _ = tokio::task::spawn_blocking(move || execute(&parent, plan, &origins)).await;
}

/// Destroy what the plan names. A dataset whose fork child failed to go is skipped: it
/// would only fail with "dependent clones".
fn execute(parent: &str, plan: Plan, origins: &HashMap<String, Option<String>>) {
    let mut kept: HashSet<String> = HashSet::new();
    for id in &plan.datasets {
        let has_kept_child = origins.iter().any(|(child, origin)| {
            kept.contains(child)
                && origin.as_deref().and_then(snapshot_dataset)
                    == Some(dataset_of(parent, id).as_str())
        });
        if has_kept_child {
            kept.insert(id.clone());
            continue;
        }
        let home = CloneHome::new(parent, id);
        // Unmount a stale merged view first: it pins the dataset busy.
        home.teardown();
        match home.destroy(true) {
            Ok(()) => {
                tracing::info!(target: "home_gc", "destroyed orphan home {}", home.dataset());
                if let Some(snap) = origins.get(id).cloned().flatten() {
                    // Its parent may already be gone with it; only a live one keeps it.
                    if let Err(e) = home.drop_snapshot(&snap) {
                        if !crate::zfs::is_missing(&e) {
                            tracing::warn!(target: "home_gc", "keeping {snap}: {e:#}");
                        }
                    }
                }
            }
            Err(e) if crate::zfs::is_missing(&e) => {}
            Err(e) => {
                tracing::warn!(target: "home_gc", "keeping orphan home {}: {e:#}", home.dataset());
                kept.insert(id.clone());
            }
        }
    }
    for snap in &plan.snapshots {
        match crate::zfs::destroy_snapshot_if_unreferenced(parent, snap) {
            Ok(()) => tracing::info!(target: "home_gc", "destroyed stray snapshot {snap}"),
            Err(e) if crate::zfs::is_missing(&e) => {}
            Err(e) => tracing::warn!(target: "home_gc", "keeping stray snapshot {snap}: {e:#}"),
        }
    }
}

fn dataset_of(parent: &str, id: &str) -> String {
    crate::zfs::dataset_name(parent, id)
}

/// The dataset half of `name@snap`.
fn snapshot_dataset(snap: &str) -> Option<&str> {
    snap.split_once('@').map(|(ds, _)| ds)
}

/// The id half of `<parent>/<id>`, for a dataset named in an origin.
fn id_of(ds: &str) -> &str {
    ds.rsplit('/').next().unwrap_or(ds)
}

/// Decide what one sweep destroys. Pure, so every guard is unit-tested.
fn plan(homes: &[HomeDataset], snaps: &[HomeSnapshot], owners: &Owners) -> Plan {
    // How many datasets were cloned from each id. Every dataset counts, owned or not.
    let mut children: HashMap<&str, usize> = HashMap::new();
    for h in homes {
        if let Some(ds) = h.origin.as_deref().and_then(snapshot_dataset) {
            *children.entry(id_of(ds)).or_default() += 1;
        }
    }
    let mut candidates: Vec<&HomeDataset> = homes
        .iter()
        .filter(|h| !owners.ids.contains(&h.id) && h.mountpoint == crate::zfs::dataset_dir(&h.id))
        .collect();
    // Leaf-first: take every candidate with no children left, drop it from its parent's
    // count, repeat. What never reaches zero hangs off an owned clone and stays.
    let mut datasets = Vec::new();
    loop {
        let (free, rest): (Vec<&HomeDataset>, Vec<&HomeDataset>) = candidates
            .into_iter()
            .partition(|h| children.get(h.id.as_str()).copied().unwrap_or(0) == 0);
        if free.is_empty() {
            break;
        }
        for h in free {
            if let Some(ds) = h.origin.as_deref().and_then(snapshot_dataset) {
                if let Some(n) = children.get_mut(id_of(ds)) {
                    *n -= 1;
                }
            }
            datasets.push(h.id.clone());
        }
        candidates = rest;
    }
    let doomed: HashSet<&str> = datasets.iter().map(String::as_str).collect();
    let snapshots = snaps
        .iter()
        .filter(|s| {
            let Some((ds, name)) = s.name.split_once('@') else {
                return false;
            };
            let id = id_of(ds);
            name.starts_with(FORK_SNAP_PREFIX)
                && !s.has_clones
                && !owners.busy.contains(id)
                // An orphan's own snapshots go with its `destroy -r`.
                && !doomed.contains(id)
        })
        .map(|s| s.name.clone())
        .collect();
    Plan {
        datasets,
        snapshots,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const P: &str = "rpool/rmng-homes-101";

    fn home(id: &str, origin: Option<&str>) -> HomeDataset {
        HomeDataset {
            id: id.into(),
            origin: origin.map(|o| format!("{P}/{o}")),
            mountpoint: format!("/srv/rmng-homes/{id}"),
        }
    }

    fn snap(name: &str, has_clones: bool) -> HomeSnapshot {
        HomeSnapshot {
            name: format!("{P}/{name}"),
            has_clones,
        }
    }

    fn owners(ids: &[&str]) -> Owners {
        Owners {
            ids: ids.iter().map(|s| s.to_string()).collect(),
            busy: HashSet::new(),
        }
    }

    #[test]
    fn an_owned_home_is_never_swept() {
        let homes = [home("live", None), home("gone", None)];
        let p = plan(&homes, &[], &owners(&["live"]));
        assert_eq!(p.datasets, vec!["gone".to_string()]);
    }

    #[test]
    fn a_fork_chain_unwinds_leaf_first() {
        // a ← b ← c, none owned: c, then b, then a.
        let homes = [
            home("a", None),
            home("b", Some("a@fork-b-1")),
            home("c", Some("b@fork-c-2")),
        ];
        let p = plan(&homes, &[], &owners(&[]));
        assert_eq!(p.datasets, vec!["c", "b", "a"]);
    }

    #[test]
    fn an_orphan_parent_waits_for_its_live_fork() {
        // The bug in the issue: parent deleted first, child still live.
        let homes = [
            home("parent", None),
            home("child", Some("parent@fork-child-1")),
        ];
        let p = plan(&homes, &[], &owners(&["child"]));
        assert!(p.datasets.is_empty());
        // Once the child is deleted too, the parent goes.
        let homes = [home("parent", None)];
        let p = plan(&homes, &[], &owners(&[]));
        assert_eq!(p.datasets, vec!["parent"]);
    }

    #[test]
    fn a_dataset_with_a_foreign_mountpoint_is_kept() {
        let mut h = home("x", None);
        h.mountpoint = "/somewhere/else".into();
        assert!(plan(&[h], &[], &owners(&[])).datasets.is_empty());
    }

    #[test]
    fn stray_fork_snapshots_on_live_homes_go_but_nothing_else() {
        let homes = [home("live", None), home("gone", None)];
        let snaps = [
            snap("live@fork-x-1", false), // stray → goes
            snap("live@fork-y-2", true),  // a clone uses it → stays
            snap("live@manual", false),   // not ours → stays
            snap("gone@fork-z-3", false), // goes with `destroy -r gone`
        ];
        let p = plan(&homes, &snaps, &owners(&["live"]));
        assert_eq!(p.snapshots, vec![format!("{P}/live@fork-x-1")]);
    }

    #[test]
    fn a_fork_in_flight_keeps_its_source_snapshot() {
        let homes = [home("src", None)];
        let snaps = [snap("src@fork-new-1", false)];
        let mut o = owners(&["src"]);
        o.busy.insert("src".into());
        assert!(plan(&homes, &snaps, &o).snapshots.is_empty());
    }
}
