//! The operation runner — one implementation of everything that surrounds a long clone flow,
//! so a flow is only its own work.
//!
//! ## Why this module exists
//!
//! Every long job in [`crate::jobs`] used to carry its own copy of two blocks. A guard block
//! (find the row → is it managed? → state precondition → "already has an operation in
//! flight" → file the operation → spawn the driver) and a finalize block (`status` / `step` /
//! `pct` / `message` / `finished_at` written by hand inside a `store.mutate` that also patched
//! the clone's row). Six copies of each, and they drifted apart exactly the way copies do:
//!
//! * rebase filed itself under `OperationKind::Clone` (there is no `Rebase` variant in
//!   `wire`) and was therefore scored against the CREATE step table, which has no `stop` —
//!   the first thing a rebase does had no reading on the progress bar at all;
//! * prebuild re-implemented the op-log cap inline instead of using the shared one, so a
//!   third copy of the same `drain(0..)` had to be kept in step by hand;
//! * delete was the one flow that never looked at `archived`, with nothing in the code to
//!   say whether that was a decision or an omission.
//!
//! So filing, guarding, progress scoring, log capping, finalizing and pruning live here,
//! once. A flow hands the runner an [`OpSpec`] — what it is, what it targets, what must be
//! true before it may start, and the step→pct table it is scored against — plus a body. The
//! body does its own work against an [`OpHandle`] and hands back a [`Finish`]: the completion
//! message and the state edit it wants applied in the SAME `store.mutate` as the operation's
//! own completion.
//!
//! ## The step table travels with the spec
//!
//! It is NOT derived from `OperationKind`. A flow with no kind of its own (rebase) still
//! brings its own table and its own queued message, so it can never be scored against a
//! table that does not contain its steps. `provision::step_pct(kind, step)` survives as the
//! by-kind index for old persisted operations, but nothing on the live path reads it.
//!
//! ## One mutate, one frame
//!
//! `store.mutate` broadcasts an SSE frame per call. Writing the finished operation in one
//! mutate and the clone's row in another lets a client read the first frame and see a
//! finished operation against a row that has not caught up — a clone announced ready before
//! it is in the list, a deleted clone still in it. [`Finish`] exists so the body can hand
//! the row edit back instead of applying it itself, and the runner lands both in one write.

use std::future::Future;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use futures::future::BoxFuture;
use wire::{ControlState, Operation, OperationKind, OperationStatus, RmngClone};

use crate::app::App;

/// Op-log lines kept per operation. The whole record is cloned into every SSE frame, and a
/// build or a chatty settle step can emit thousands of lines, so the log is a rolling tail.
const LOG_LIMIT: usize = 200;

/// How long a finished operation stays in state before it is pruned. Long enough for the UI
/// to show the green finish, short enough that the list is the live work.
pub(crate) const PRUNE_DONE_MS: u64 = 8_000;
/// A failed operation lingers far longer: its message is the only record of what went wrong.
pub(crate) const PRUNE_ERROR_MS: u64 = 60_000;

/// A refusal from the guard pass, surfaced to the API as a 400. Public so `web.rs` can
/// render it; the string is operator-facing.
#[derive(Debug)]
pub struct JobError(pub String);
impl std::fmt::Display for JobError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}
impl std::error::Error for JobError {}

pub(crate) fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

fn new_op_id() -> String {
    static N: AtomicU64 = AtomicU64::new(0);
    let n = N.fetch_add(1, Ordering::Relaxed);
    let t = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!(
        "op_{:08x}",
        (t as u64).wrapping_add(n.wrapping_mul(0x9E3779B97F4A7C15)) & 0xFFFF_FFFF
    )
}

// --- spec ------------------------------------------------------------------------------

/// A step→pct table: the coarse percentage a streamed step key is worth. `None` means "no
/// reading for this step", and the runner then leaves the bar where it is.
pub(crate) type StepTable = fn(&str) -> Option<f64>;

/// Everything the runner needs to know about an operation before its body starts.
pub(crate) struct OpSpec {
    kind: OperationKind,
    target: String,
    source: Option<String>,
    queued: Option<String>,
    steps: StepTable,
    guards: Guards,
}

impl OpSpec {
    /// A spec with no source, the kind's default queued message, no step table (the bar
    /// stays put until the runner finishes it) and no guards. Add what the flow needs.
    pub(crate) fn new(kind: OperationKind, target: impl Into<String>) -> Self {
        Self {
            kind,
            target: target.into(),
            source: None,
            queued: None,
            steps: crate::provision::no_pct,
            guards: Guards::none(),
        }
    }

    /// What this operation acts FROM — a fork's source clone, a rebase's target preset.
    pub(crate) fn source(mut self, source: impl Into<String>) -> Self {
        self.source = Some(source.into());
        self
    }

    /// Override the queued message. A flow that borrows another kind's variant needs this:
    /// a rebase files as `Clone`, and the kind's own label would read "queued clone of
    /// preset-x" for an operation that clones nothing.
    pub(crate) fn queued(mut self, message: impl Into<String>) -> Self {
        self.queued = Some(message.into());
        self
    }

    /// The table this flow's streamed steps are scored against. Carried here, never derived
    /// from `kind` — see the module doc.
    pub(crate) fn steps(mut self, steps: StepTable) -> Self {
        self.steps = steps;
        self
    }

    pub(crate) fn guards(mut self, guards: Guards) -> Self {
        self.guards = guards;
        self
    }
}

// --- guards ----------------------------------------------------------------------------

/// What must be true before an operation is filed.
///
/// [`Guards::on_clone`] is the whole shared set for a flow that acts on an existing clone:
/// the row exists, it is managed, and nothing else is in flight on it. A flow that needs
/// MORE adds it ([`Guards::archived`], [`Guards::gen2`]). A flow that needs LESS has to say
/// why in a comment at the call site, because that is exactly how the drift this runner
/// replaces started: `start_delete` was the one flow that never checked `archived`, and the
/// code could not tell you whether that was deliberate.
#[derive(Default, Clone, Copy)]
pub(crate) struct Guards {
    /// The target must name a row in `s.hosts`.
    pub(crate) row: bool,
    /// …and that row must be `managed` (a clone this server built and owns a container for).
    pub(crate) managed: bool,
    /// `Some(true)`: the row must be archived. `Some(false)`: it must not be. `None`: either
    /// is fine, and the call site says why.
    pub(crate) archived: Option<bool>,
    /// `Some(true)`: the row must be gen-2 ([`crate::clone_home::is_gen2`] — its home is a
    /// ZFS dataset this server owns). `Some(false)`: it must still be gen-1.
    pub(crate) gen2: Option<bool>,
    /// No other `Running` operation may target this id. Two flows on one clone race over
    /// its container and its row.
    pub(crate) idle_target: bool,
    /// No other `Running` operation of the SAME kind may exist anywhere.
    pub(crate) idle_kind: bool,
    /// No other `Running` operation may exist at all. Only the control-server self-update
    /// asks for this: the swap kills the server, which would abort every in-flight
    /// clone/delete/rebase with it.
    pub(crate) idle_fleet: bool,
}

impl Guards {
    /// Nothing is checked. For a flow whose target is not a row yet (a create).
    pub(crate) fn none() -> Self {
        Self::default()
    }

    /// The shared set for a flow that acts on an existing clone.
    pub(crate) fn on_clone() -> Self {
        Self {
            row: true,
            managed: true,
            idle_target: true,
            ..Self::default()
        }
    }

    pub(crate) fn archived(mut self, want: bool) -> Self {
        self.archived = Some(want);
        self
    }

    pub(crate) fn gen2(mut self, want: bool) -> Self {
        self.gen2 = Some(want);
        self
    }
}

/// Run the guard pass for `spec` against the current state.
///
/// Order is load-bearing: the state preconditions (archived / generation) are checked BEFORE
/// "already in flight", so a clone that is simply in the wrong state is told so even while
/// its own operation is running. Reversing it turns "'x' is not archived" into "'x' already
/// has an operation in flight", which sends the operator looking for a job that does not
/// exist.
pub(crate) fn check_guards(app: &App, spec: &OpSpec) -> Result<(), JobError> {
    let st = app.store.get();
    let target = spec.target.as_str();

    if spec.guards.idle_fleet
        && st
            .operations
            .iter()
            .any(|o| o.status == OperationStatus::Running)
    {
        return Err(JobError(
            "another operation is in flight; wait for it to finish".into(),
        ));
    }

    if spec.guards.row {
        let Some(row) = st.hosts.iter().find(|h| h.id == target) else {
            return Err(JobError(format!("unknown clone '{target}'")));
        };
        if spec.guards.managed && !row.managed {
            return Err(JobError(format!("'{target}' is not a managed clone")));
        }
        match spec.guards.archived {
            Some(true) if !row.archived => {
                return Err(JobError(format!("'{target}' is not archived")));
            }
            Some(false) if row.archived => {
                return Err(JobError(format!("'{target}' is already archived")));
            }
            _ => {}
        }
        // `clone_home::is_gen2` is the one definition of the generation split (the PRESENCE
        // of `dataset`). Rebase used to test `base_tag.is_none()` here instead — a second
        // definition of the same thing. The tag is still checked, deeper in
        // `provision::rebase_clone`, which bails with "no recorded base tag"; that is the
        // right place for it, because the tag is what that code actually needs.
        match spec.guards.gen2 {
            Some(true) if !crate::clone_home::is_gen2(row) => {
                return Err(JobError(format!("'{target}' is not a gen-2 clone")));
            }
            Some(false) if crate::clone_home::is_gen2(row) => {
                return Err(JobError(format!("'{target}' is already a gen-2 clone")));
            }
            _ => {}
        }
    }

    if spec.guards.idle_target
        && st
            .operations
            .iter()
            .any(|o| o.status == OperationStatus::Running && o.target == target)
    {
        return Err(JobError(format!(
            "'{target}' already has an operation in flight"
        )));
    }
    if spec.guards.idle_kind
        && st
            .operations
            .iter()
            .any(|o| o.status == OperationStatus::Running && o.kind == spec.kind)
    {
        return Err(JobError(format!(
            "a {} is already in flight",
            kind_noun(spec.kind)
        )));
    }
    Ok(())
}

fn kind_noun(kind: OperationKind) -> &'static str {
    match kind {
        OperationKind::Clone => "clone",
        OperationKind::Pull => "pull",
        OperationKind::Commit => "commit",
        OperationKind::Delete => "delete",
        OperationKind::Archive => "archive",
        OperationKind::Unarchive => "unarchive",
        OperationKind::Update => "control-server update",
        OperationKind::Migrate => "migration",
        OperationKind::Prebuild => "prebuild",
    }
}

// --- the running operation --------------------------------------------------------------

/// A running operation's write end: everything a body may say about its own progress.
///
/// Cheap to clone (an `App` handle plus two strings), so a body can hold one and hand owned
/// progress sinks to `provision` at the same time.
#[derive(Clone)]
pub(crate) struct OpHandle {
    app: App,
    op_id: String,
    target: String,
    steps: StepTable,
}

impl OpHandle {
    /// This operation's id. Needed by the few helpers that log into an op they were handed
    /// rather than one they own ([`crate::pool::assign_clone_side`]).
    pub(crate) fn id(&self) -> &str {
        &self.op_id
    }

    /// Record one streamed `(step, message)`: the coarse pct from this operation's OWN step
    /// table, the message, and a capped rolling log line.
    ///
    /// `provision` may emit a sub-progress pct inline in the message (e.g. `"57% installing
    /// …"` during the long bootstrap phase); the coarse table pct stays here and the message
    /// carries the fine detail. A step the table has no reading for leaves the bar alone.
    pub(crate) fn step(&self, step: &str, msg: &str) {
        let pct = (self.steps)(step);
        patch_op(&self.app, &self.op_id, |op| {
            op.step = step.to_string();
            if let Some(p) = pct {
                op.pct = p;
            }
            op.message = msg.to_string();
            push_capped(&mut op.log, format!("{step}: {msg}"));
        });
    }

    /// An owned progress sink for the `provision` flows, which take `impl FnMut(&str, &str)`.
    /// Call it as often as needed: each sink carries its own clone of the handle.
    pub(crate) fn progress(&self) -> impl FnMut(&str, &str) + use<> {
        let h = self.clone();
        move |step: &str, msg: &str| h.step(step, msg)
    }

    /// Append one line to the op log without touching step, pct or message. For the
    /// best-effort side notes a flow wants on the record (an account assignment that failed,
    /// a startup script's output).
    pub(crate) fn log(&self, line: impl Into<String>) {
        let line = line.into();
        patch_op(&self.app, &self.op_id, |op| push_capped(&mut op.log, line));
    }
}

/// Append to an operation's progress log, capped. Shared with `jobs::run_update`,
/// whose pull callback writes into the op directly (it borrows `(app, op_id)` rather than a
/// progress closure, to keep the borrow checker out of the way), and with
/// [`crate::pool`], which logs assignment delivery into the create/fork op it runs under.
pub(crate) fn push_capped(log: &mut Vec<String>, line: String) {
    log.push(line);
    if log.len() > LOG_LIMIT {
        let drop = log.len() - LOG_LIMIT;
        log.drain(0..drop);
    }
}

/// Apply `f` to operation `op_id` in one mutate. Shared with [`crate::pool`], which logs
/// assignment delivery into the create/fork op it runs under.
pub(crate) fn patch_op(app: &App, op_id: &str, f: impl FnOnce(&mut Operation)) {
    app.store.mutate(|s| {
        if let Some(op) = s.operations.iter_mut().find(|o| o.id == op_id) {
            f(op);
        }
    });
}

/// Mark an operation failed and schedule its (long) prune.
pub(crate) fn fail_op(app: &App, op_id: &str, msg: String) {
    tracing::warn!(op = op_id, "operation failed: {msg}");
    patch_op(app, op_id, |op| {
        op.status = OperationStatus::Error;
        op.message = msg.clone();
        op.log.push(format!("error: {msg}"));
        op.finished_at = Some(now_ms());
    });
    schedule_prune(app.clone(), op_id.to_string(), PRUNE_ERROR_MS);
}

pub(crate) fn schedule_prune(app: App, op_id: String, delay_ms: u64) {
    tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(delay_ms)).await;
        app.store.mutate(|s| s.operations.retain(|o| o.id != op_id));
    });
}

// --- what a body hands back ---------------------------------------------------------------

type RowEdit = Box<dyn FnOnce(&mut RmngClone) + Send>;
type StateEdit = Box<dyn FnOnce(&mut ControlState) + Send>;
type AfterHook = Box<dyn FnOnce(App, ControlState) -> BoxFuture<'static, ()> + Send>;

/// What a body hands back when its work succeeded: the completion message, the state edit
/// that must land in the SAME mutate as the operation's completion, and the tail that runs
/// once both are written.
///
/// The edit is split in two because most flows only touch their own row, and saying so is
/// clearer than re-finding it: [`Finish::row`] patches the operation's target row,
/// [`Finish::state`] is for anything wider (adding a row, removing one, moving `selected`).
/// When a flow uses both, the ROW edit is applied first — archive depends on it, because its
/// state edit picks the operator's next clone with `!h.archived` and must not pick the clone
/// it has just archived.
pub(crate) struct Finish {
    message: String,
    row: Option<RowEdit>,
    state: Option<StateEdit>,
    after: Option<AfterHook>,
}

impl Finish {
    pub(crate) fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            row: None,
            state: None,
            after: None,
        }
    }

    /// Patch the operation's TARGET row (`base_tag`, `source`, `dataset`, `archived`, …).
    /// A no-op when the target names no row.
    pub(crate) fn row(mut self, f: impl FnOnce(&mut RmngClone) + Send + 'static) -> Self {
        self.row = Some(Box::new(f));
        self
    }

    /// Edit the whole state: insert a row, drop one, move `selected`.
    pub(crate) fn state(mut self, f: impl FnOnce(&mut ControlState) + Send + 'static) -> Self {
        self.state = Some(Box::new(f));
        self
    }

    /// Work that has to happen AFTER the operation is finished and the state is written —
    /// reconciling the clone's outside-the-container pieces, starting the agent, moving the
    /// viewer. It is handed the post-mutate state, so a flow that moved `selected` can
    /// compare it against what it captured before returning.
    pub(crate) fn after<Fut>(
        mut self,
        f: impl FnOnce(App, ControlState) -> Fut + Send + 'static,
    ) -> Self
    where
        Fut: Future<Output = ()> + Send + 'static,
    {
        self.after = Some(Box::new(move |app, st| Box::pin(f(app, st))));
        self
    }
}

// --- the runner ---------------------------------------------------------------------------

/// File `spec` into state and drive `body` in the background. The returned `Operation` is
/// the record the API hands straight back: the work streams over `/events`.
///
/// The body gets an owned `App` and the operation's [`OpHandle`]. Returning `Err` fails the
/// operation with `{e:#}`; returning [`Finish`] completes it, applies the state edit in the
/// same mutate, prunes it on a timer and runs the tail.
pub(crate) fn run_op<F, Fut>(app: &App, spec: OpSpec, body: F) -> Result<Operation, JobError>
where
    F: FnOnce(App, OpHandle) -> Fut + Send + 'static,
    Fut: Future<Output = anyhow::Result<Finish>> + Send + 'static,
{
    check_guards(app, &spec)?;
    Ok(run_op_unguarded(app, spec, body))
}

/// [`run_op`] with the guard pass left out, for the two flows that cannot use it as one step.
///
/// A create has nothing to guard at all: it files against the id it is about to add, so
/// `Guards::none()` can never refuse it and a `Result` would be a lie. A rebase has to
/// interleave — the shared set first, then its own preset checks — so it calls
/// [`check_guards`] itself and drives the already-guarded spec here. Any other caller is
/// skipping the guard pass, which is the thing this module exists to make impossible.
pub(crate) fn run_op_unguarded<F, Fut>(app: &App, spec: OpSpec, body: F) -> Operation
where
    F: FnOnce(App, OpHandle) -> Fut + Send + 'static,
    Fut: Future<Output = anyhow::Result<Finish>> + Send + 'static,
{
    let (filed, handle) = file(app, spec);
    let app2 = app.clone();
    tokio::spawn(async move { drive(app2, handle, body).await });
    filed
}

/// Guard and file an operation, and hand it back for a caller that drives it itself.
///
/// Only the control-server self-update uses this. Its body hands off to a helper that STOPS
/// this container mid-operation, so the operation deliberately ends this process still
/// `Running` at 85% and the rebooted server's `update::reconcile_pending` finalizes it —
/// there is no `Finish` to return, because the task never gets to return anything.
pub(crate) fn file_op(app: &App, spec: OpSpec) -> Result<Operation, JobError> {
    check_guards(app, &spec)?;
    Ok(file(app, spec).0)
}

/// Build the `Operation` record, push it into state, and answer (the filed copy, its handle).
fn file(app: &App, spec: OpSpec) -> (Operation, OpHandle) {
    let OpSpec {
        kind,
        target,
        source,
        queued,
        steps,
        guards: _,
    } = spec;
    let message = queued.unwrap_or_else(|| default_queued(kind, &target, source.as_deref()));
    let op = Operation {
        id: new_op_id(),
        kind,
        target: target.clone(),
        source,
        status: OperationStatus::Running,
        step: "queued".into(),
        pct: 0.0,
        message,
        log: Vec::new(),
        started_at: now_ms(),
        finished_at: None,
    };
    let handle = OpHandle {
        app: app.clone(),
        op_id: op.id.clone(),
        target,
        steps,
    };
    let filed = op.clone();
    app.store.mutate(|s| s.operations.push(op));
    (filed, handle)
}

/// The queued label a kind gets when the spec does not override it.
fn default_queued(kind: OperationKind, target: &str, source: Option<&str>) -> String {
    match kind {
        OperationKind::Clone => format!("queued clone of {}", source.unwrap_or("?")),
        OperationKind::Pull => format!("queued template pull → {target}"),
        OperationKind::Commit => format!("queued commit of {}", source.unwrap_or("?")),
        OperationKind::Delete => format!("queued delete of {target}"),
        OperationKind::Archive => format!("queued archive of {target}"),
        OperationKind::Unarchive => format!("queued unarchive of {target}"),
        OperationKind::Update => "queued control-server update".to_string(),
        // Stage 2 owns the migrate flow; the label keeps filed ops readable meanwhile.
        OperationKind::Migrate => format!("queued migration of {target}"),
        OperationKind::Prebuild => format!("queued derived-image build → {target}"),
    }
}

async fn drive<F, Fut>(app: App, handle: OpHandle, body: F)
where
    F: FnOnce(App, OpHandle) -> Fut + Send + 'static,
    Fut: Future<Output = anyhow::Result<Finish>> + Send + 'static,
{
    let (op_id, target) = (handle.op_id.clone(), handle.target.clone());
    match body(app.clone(), handle).await {
        Ok(finish) => finish_op(&app, &op_id, &target, finish).await,
        Err(e) => fail_op(&app, &op_id, format!("{e:#}")),
    }
}

/// Complete an operation: the row edit, the wider state edit and the operation's own
/// `Done`/`step`/`pct`/`message`/`finished_at` in ONE mutate, then the prune timer, then the
/// flow's tail. One mutate is one SSE frame, so no client can see a finished operation
/// against a row that has not caught up with it.
async fn finish_op(app: &App, op_id: &str, target: &str, finish: Finish) {
    let Finish {
        message,
        row,
        state,
        after,
    } = finish;
    let new_state = app.store.mutate(|s| {
        if let Some(f) = row {
            if let Some(h) = s.hosts.iter_mut().find(|h| h.id == target) {
                f(h);
            }
        }
        if let Some(f) = state {
            f(s);
        }
        if let Some(op) = s.operations.iter_mut().find(|o| o.id == op_id) {
            op.status = OperationStatus::Done;
            op.step = "done".into();
            op.pct = 100.0;
            op.message = message;
            op.finished_at = Some(now_ms());
        }
    });
    schedule_prune(app.clone(), op_id.to_string(), PRUNE_DONE_MS);
    if let Some(f) = after {
        f(app.clone(), new_state).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    /// A minimal App backed by a throwaway temp data dir (ClaudeStore/state don't touch the
    /// repo). Docker is constructed I/O-free — the guard pass never touches it.
    fn test_app() -> App {
        static N: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let dir = std::env::temp_dir().join(format!(
            "rmng-op-test-{}-{}",
            std::process::id(),
            N.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let store = Arc::new(crate::state::StateStore::load(dir.join("state.json")).unwrap());
        let cfg = wire::AppConfig::default();
        App::new(store, cfg, &dir.to_string_lossy())
    }

    fn running_op(id: &str, target: &str, kind: OperationKind) -> Operation {
        Operation {
            id: id.into(),
            kind,
            target: target.into(),
            source: None,
            status: OperationStatus::Running,
            step: "pull".into(),
            pct: 40.0,
            message: "pulling".into(),
            log: vec!["pull: pulling".into()],
            started_at: now_ms(),
            finished_at: None,
        }
    }

    /// The shared guard set, checked once here instead of six times across the flows.
    #[tokio::test]
    async fn on_clone_guards_reject_unknown_unmanaged_and_busy() {
        let app = test_app();
        app.store.mutate(|s| {
            s.hosts.push(RmngClone {
                id: "plain".into(),
                host: "plain".into(),
                ..Default::default()
            });
            s.hosts.push(RmngClone {
                id: "managed".into(),
                host: "managed".into(),
                managed: true,
                ..Default::default()
            });
        });
        let spec = |id: &str| OpSpec::new(OperationKind::Archive, id).guards(Guards::on_clone());

        assert!(
            check_guards(&app, &spec("nope"))
                .unwrap_err()
                .0
                .contains("unknown clone")
        );
        assert!(
            check_guards(&app, &spec("plain"))
                .unwrap_err()
                .0
                .contains("not a managed")
        );
        check_guards(&app, &spec("managed")).unwrap();

        app.store.mutate(|s| {
            s.operations
                .push(running_op("op_1", "managed", OperationKind::Archive))
        });
        assert!(
            check_guards(&app, &spec("managed"))
                .unwrap_err()
                .0
                .contains("in flight")
        );
    }

    /// A state precondition is reported BEFORE "in flight", so a clone in the wrong state is
    /// told so even while its own operation runs.
    #[tokio::test]
    async fn state_preconditions_are_checked_before_in_flight() {
        let app = test_app();
        app.store.mutate(|s| {
            s.hosts.push(RmngClone {
                id: "stored".into(),
                host: "stored".into(),
                managed: true,
                archived: true,
                ..Default::default()
            });
            s.operations
                .push(running_op("op_1", "stored", OperationKind::Archive));
        });
        let archive = OpSpec::new(OperationKind::Archive, "stored")
            .guards(Guards::on_clone().archived(false));
        assert!(
            check_guards(&app, &archive)
                .unwrap_err()
                .0
                .contains("already archived")
        );
        let unarchive = OpSpec::new(OperationKind::Unarchive, "stored")
            .guards(Guards::on_clone().archived(true));
        assert!(
            check_guards(&app, &unarchive)
                .unwrap_err()
                .0
                .contains("in flight")
        );
    }

    /// Migrate demands gen-1, rebase demands gen-2, and both read the one definition of the
    /// split ([`crate::clone_home::is_gen2`]).
    #[tokio::test]
    async fn generation_guard_reads_one_definition() {
        let app = test_app();
        app.store.mutate(|s| {
            s.hosts.push(RmngClone {
                id: "gen1".into(),
                host: "gen1".into(),
                managed: true,
                ..Default::default()
            });
            s.hosts.push(RmngClone {
                id: "gen2".into(),
                host: "gen2".into(),
                managed: true,
                dataset: Some("pool/homes/gen2".into()),
                ..Default::default()
            });
        });
        let needs_gen2 =
            |id: &str| OpSpec::new(OperationKind::Clone, id).guards(Guards::on_clone().gen2(true));
        let needs_gen1 = |id: &str| {
            OpSpec::new(OperationKind::Migrate, id).guards(Guards::on_clone().gen2(false))
        };
        assert!(
            check_guards(&app, &needs_gen2("gen1"))
                .unwrap_err()
                .0
                .contains("not a gen-2")
        );
        check_guards(&app, &needs_gen2("gen2")).unwrap();
        assert!(
            check_guards(&app, &needs_gen1("gen2"))
                .unwrap_err()
                .0
                .contains("already a gen-2")
        );
        check_guards(&app, &needs_gen1("gen1")).unwrap();
    }

    /// `idle_fleet` (the self-update guard) refuses on ANY running operation, whatever it
    /// targets; `idle_kind` (prebuild) refuses only on its own kind.
    #[tokio::test]
    async fn fleet_and_kind_guards() {
        let app = test_app();
        app.store.mutate(|s| {
            s.operations
                .push(running_op("op_1", "some-clone", OperationKind::Clone))
        });
        let update = OpSpec::new(OperationKind::Update, "control-server").guards(Guards {
            idle_fleet: true,
            ..Guards::none()
        });
        assert!(
            check_guards(&app, &update)
                .unwrap_err()
                .0
                .contains("in flight")
        );

        let prebuild = OpSpec::new(OperationKind::Prebuild, "sha-abc").guards(Guards {
            idle_target: true,
            idle_kind: true,
            ..Guards::none()
        });
        check_guards(&app, &prebuild).unwrap();
        app.store.mutate(|s| {
            s.operations
                .push(running_op("op_2", "sha-xyz", OperationKind::Prebuild))
        });
        assert!(
            check_guards(&app, &prebuild)
                .unwrap_err()
                .0
                .contains("in flight")
        );
    }

    /// The log cap is one implementation: prebuild used to carry its own copy.
    #[test]
    fn log_cap_keeps_the_tail() {
        let mut log = Vec::new();
        for i in 0..(LOG_LIMIT + 5) {
            push_capped(&mut log, format!("line {i}"));
        }
        assert_eq!(log.len(), LOG_LIMIT);
        assert_eq!(log[0], "line 5");
        assert_eq!(log[LOG_LIMIT - 1], format!("line {}", LOG_LIMIT + 4));
    }
}
