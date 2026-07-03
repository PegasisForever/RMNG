//! Operation lifecycle — wraps the Docker clone/bootstrap/commit/delete flows in an
//! `Operation` persisted into `ControlState` and streamed to the UI over SSE. Ported from
//! `jobs.server.ts`; the backend is now `provision.rs` (bollard), not the retired SSH+`pct`
//! path. Jobs run in the background: the API creates the op and returns its id immediately;
//! updates flow over `/events`.
//!
//! The coarse step→pct mapping lives in `provision` (its `step_pct` tables), so a streamed
//! step key maps to the same percentage the backend intends. This file owns the `Operation`
//! record + the progress→op-log plumbing; the flows themselves live in `provision`.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use wire::{Host, Operation, OperationKind, OperationStatus};

use crate::app::App;
use crate::provision::{
    self, clone_container, commit_clone_image, control_env_vars, delete_clone, is_dns_label,
    pull_template, PullProgress,
};

const LOG_LIMIT: usize = 200;
const PRUNE_DONE_MS: u64 = 8_000;
const PRUNE_ERROR_MS: u64 = 60_000;

#[derive(Debug)]
pub struct JobError(pub String);
impl std::fmt::Display for JobError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}
impl std::error::Error for JobError {}

/// Linear ticket metadata stamped onto a cloned `Host`.
#[derive(Debug, Clone, Default)]
pub struct LinearMeta {
    /// Lowercase Linear workspace name / ticket prefix (e.g. `"we"`).
    pub workspace: Option<String>,
    pub ticket: Option<String>,
    pub ticket_url: Option<String>,
    pub branch: Option<String>,
    pub display_name: Option<String>,
    pub label: Option<String>,
}

/// Everything the API hands to `start_clone`.
#[derive(Debug, Clone, Default)]
pub struct CloneSpec {
    /// The clone-source image reference (`rmng/template:<name>`) or id to clone from.
    pub source_image: String,
    pub new_hostname: String,
    pub linear: Option<LinearMeta>,
    /// Requested Claude account: an email, `"auto"`, or `None` (= auto).
    pub claude_account: Option<String>,
    pub first_message: Option<String>,
    pub agent_instructions: Option<String>,
    pub claude_instructions: Option<String>,
    /// Resolved env-preset vars to write into the clone's session env at creation.
    pub env: Vec<wire::EnvVar>,
}

fn now_ms() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0)
}

fn new_op_id() -> String {
    static N: AtomicU64 = AtomicU64::new(0);
    let n = N.fetch_add(1, Ordering::Relaxed);
    let t = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
    format!("op_{:08x}", (t as u64).wrapping_add(n.wrapping_mul(0x9E3779B97F4A7C15)) & 0xFFFF_FFFF)
}

fn make_op(kind: OperationKind, target: &str, source: Option<&str>) -> Operation {
    let message = match kind {
        OperationKind::Clone => format!("queued clone of {}", source.unwrap_or("?")),
        OperationKind::Pull => format!("queued template pull → {target}"),
        OperationKind::Commit => format!("queued commit of {}", source.unwrap_or("?")),
        OperationKind::Delete => format!("queued delete of {target}"),
    };
    Operation {
        id: new_op_id(),
        kind,
        target: target.to_string(),
        source: source.map(str::to_string),
        status: OperationStatus::Running,
        step: "queued".into(),
        pct: 0.0,
        message,
        log: Vec::new(),
        started_at: now_ms(),
        finished_at: None,
    }
}

fn patch_op(app: &App, op_id: &str, f: impl FnOnce(&mut Operation)) {
    app.store.mutate(|s| {
        if let Some(op) = s.operations.iter_mut().find(|o| o.id == op_id) {
            f(op);
        }
    });
}

fn fail_op(app: &App, op_id: &str, msg: String) {
    tracing::warn!(op = op_id, "operation failed: {msg}");
    patch_op(app, op_id, |op| {
        op.status = OperationStatus::Error;
        op.message = msg.clone();
        op.log.push(format!("error: {msg}"));
        op.finished_at = Some(now_ms());
    });
    schedule_prune(app.clone(), op_id.to_string(), PRUNE_ERROR_MS);
}

fn schedule_prune(app: App, op_id: String, delay_ms: u64) {
    tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(delay_ms)).await;
        app.store.mutate(|s| s.operations.retain(|o| o.id != op_id));
    });
}

/// A progress callback for op `op_id` of `kind`: maps a streamed `(step, message)` onto the
/// operation record — the coarse pct from `provision`'s step→pct table for `kind`, the
/// message, and a capped rolling log. `provision` may emit a sub-progress pct inline in the
/// message (e.g. `"57% installing …"` during the long bootstrap phase); we keep the coarse
/// table pct here and let the message carry the fine detail.
fn op_progress(app: &App, op_id: &str, kind: OperationKind) -> impl FnMut(&str, &str) {
    let app = app.clone();
    let op_id = op_id.to_string();
    move |step: &str, msg: &str| {
        let pct = provision::step_pct(kind, step);
        patch_op(&app, &op_id, |op| {
            op.step = step.to_string();
            if let Some(p) = pct {
                op.pct = p;
            }
            op.message = msg.to_string();
            op.log.push(format!("{step}: {msg}"));
            if op.log.len() > LOG_LIMIT {
                let drop = op.log.len() - LOG_LIMIT;
                op.log.drain(0..drop);
            }
        });
    }
}

/// The pull-flow analogue of [`op_progress`]: consumes [`PullProgress`] directly (the pull
/// flow doesn't use the shared `(step, msg)` callback). A `Step` transition sets the
/// step/message + a log line and raises the pct to the `pull_pct` floor; a `Pct` byte tick
/// raises the bar (monotonic `max`) + updates the message with NO log line — a single pull
/// emits up to ~100 byte ticks, which would swamp the op log; a `Log` line (per-layer pull
/// status) pushes to the op log + updates the message WITHOUT touching `step` or `pct` — it
/// fires mid-`"pull"` step, same as the old bootstrap's per-layer log lines.
fn pull_op_progress(app: &App, op_id: &str) -> impl FnMut(PullProgress) {
    let app = app.clone();
    let op_id = op_id.to_string();
    move |ev: PullProgress| match ev {
        PullProgress::Step { step, msg } => {
            let pct = provision::step_pct(OperationKind::Pull, &step);
            patch_op(&app, &op_id, |op| {
                op.step = step;
                if let Some(p) = pct {
                    op.pct = op.pct.max(p);
                }
                op.log.push(format!("{}: {msg}", op.step));
                op.message = msg;
                if op.log.len() > LOG_LIMIT {
                    let drop = op.log.len() - LOG_LIMIT;
                    op.log.drain(0..drop);
                }
            });
        }
        PullProgress::Pct { pct, msg } => {
            patch_op(&app, &op_id, |op| {
                op.pct = op.pct.max(pct);
                op.message = msg;
            });
        }
        PullProgress::Log { msg } => {
            patch_op(&app, &op_id, |op| {
                op.log.push(format!("{}: {msg}", op.step));
                op.message = msg;
                if op.log.len() > LOG_LIMIT {
                    let drop = op.log.len() - LOG_LIMIT;
                    op.log.drain(0..drop);
                }
            });
        }
    }
}

/// Mark every persisted `Running` operation as `Error` ("interrupted by server restart") and
/// schedule it for prune. Called once at boot: an `Operation` lives only while its driving
/// task runs, so any `Running` op loaded from `state.json` is a corpse from a server that
/// crashed/was killed mid-op. Left as-is it blocks same-named ops forever (every start_*
/// guard rejects a target with a Running op). Touches only state, so it's safe with Docker
/// down.
pub fn fail_stale_ops(app: &App) {
    let stale: Vec<String> = app
        .store
        .get()
        .operations
        .iter()
        .filter(|o| o.status == OperationStatus::Running)
        .map(|o| o.id.clone())
        .collect();
    if stale.is_empty() {
        return;
    }
    app.store.mutate(|s| {
        for op in s.operations.iter_mut().filter(|o| o.status == OperationStatus::Running) {
            op.status = OperationStatus::Error;
            op.message = "interrupted by server restart".into();
            op.log.push("error: interrupted by server restart".into());
            op.finished_at = Some(now_ms());
        }
    });
    for id in stale {
        tracing::warn!(op = id.as_str(), "marking stale Running op as Error (interrupted by server restart)");
        schedule_prune(app.clone(), id, PRUNE_ERROR_MS);
    }
}

/// Pick a free host id for a ticket base name (`base`, then `base a..z`). Race-free
/// when called immediately before `start_clone` (single state snapshot).
pub fn next_free_hostname(app: &App, base: &str) -> String {
    let st = app.store.get();
    let mut taken: std::collections::HashSet<String> = st.hosts.iter().map(|h| h.id.clone()).collect();
    for o in &st.operations {
        if o.status == OperationStatus::Running {
            taken.insert(o.target.clone());
        }
    }
    if !taken.contains(base) {
        return base.to_string();
    }
    for i in 0..26u8 {
        let candidate = format!("{base}{}", (b'a' + i) as char);
        if !taken.contains(&candidate) {
            return candidate;
        }
    }
    base.to_string()
}

/// Validate + register a clone op, then drive it in the background. Images clone
/// concurrently (nothing on the source to lock), so there is no source-busy check — only the
/// hostname's validity + uniqueness are gated.
pub fn start_clone(app: &App, spec: CloneSpec) -> Result<Operation, JobError> {
    if spec.source_image.trim().is_empty() {
        return Err(JobError("a source image is required".into()));
    }
    if !is_dns_label(&spec.new_hostname) {
        return Err(JobError(
            "new hostname must be a DNS label (lowercase letters, digits, hyphens)".into(),
        ));
    }
    let st = app.store.get();
    if st.hosts.iter().any(|h| h.id == spec.new_hostname) {
        return Err(JobError(format!("a host named '{}' already exists", spec.new_hostname)));
    }
    if st.operations.iter().any(|o| o.status == OperationStatus::Running && o.target == spec.new_hostname) {
        return Err(JobError(format!("'{}' is already being created", spec.new_hostname)));
    }

    let op = make_op(OperationKind::Clone, &spec.new_hostname, Some(&spec.source_image));
    let op_for_return = op.clone();
    let op_id = op.id.clone();
    app.store.mutate(|s| s.operations.push(op));

    let app2 = app.clone();
    tokio::spawn(async move { run_clone(app2, op_id, spec).await });
    Ok(op_for_return)
}

async fn run_clone(app: App, op_id: String, spec: CloneSpec) {
    let progress = op_progress(&app, &op_id, OperationKind::Clone);

    // The clone→control-server + inference URLs (auto-detected) go into the clone's session
    // env first; the operator's chosen preset follows (so a preset key can still override).
    let mut env = control_env_vars(&app).await;
    env.extend(spec.env.iter().cloned());
    // `image_ref` is the CANONICAL reference of the image actually used (the caller may have
    // passed an id form — MCP/raw API); `Host.source` must record the reference so the
    // commit flow can stamp lineage. The backing container's name is the host id — that's
    // how every later call (dials, redeploy, credential ops, delete) addresses it.
    let image_ref =
        match clone_container(&app, &spec.source_image, &spec.new_hostname, &env, progress).await {
            Ok(v) => v,
            Err(e) => return fail_op(&app, &op_id, e.to_string()),
        };

    // Register the new managed host. `host` is display-only for managed clones (dials go
    // by container name == id), so it just records the name. Clones ship with fixed
    // `rmng`/`rmng` credentials baked into the base image (the old Proxmox
    // credential-inheritance from a source host is gone — images have no per-host
    // credentials to inherit). RDP port stays 3389 for the media path.
    app.store.mutate(|s| {
        let mut host = Host {
            id: spec.new_hostname.clone(),
            host: spec.new_hostname.clone(),
            port: 3389,
            username: "rmng".into(),
            password: "rmng".into(),
            managed: true,
            source: Some(image_ref.clone()),
            ..Default::default()
        };
        if let Some(m) = &spec.linear {
            host.linear_workspace = m.workspace.clone();
            host.linear_ticket = m.ticket.clone();
            host.linear_ticket_url = m.ticket_url.clone();
            host.linear_branch = m.branch.clone();
            host.display_name = m.display_name.clone();
            host.linear_label = m.label.clone();
        }
        s.hosts.push(host);
        if let Some(op) = s.operations.iter_mut().find(|o| o.id == op_id) {
            op.status = OperationStatus::Done;
            op.step = "done".into();
            op.pct = 100.0;
            op.message = format!("clone {} ready", spec.new_hostname);
            op.finished_at = Some(now_ms());
        }
    });
    schedule_prune(app.clone(), op_id.clone(), PRUNE_DONE_MS);

    // Assign a Claude account/group (or explicitly none): record the operator's
    // selection + the resolved account in state (UI shows it immediately), then install
    // the account's current access token into the clone's ~/.claude/.credentials.json
    // (the server refreshes + re-pushes it thereafter). A group-bound clone records its
    // group; the rotator re-balances it. "none" installs no token — the clone keeps
    // whatever (if anything) it inherited from the image.
    if let Some(assignment) = crate::claude::resolve_assignment(&app, spec.claude_account.as_deref()) {
        let selection = crate::claude::normalize_selection(spec.claude_account.as_deref());
        let (group, account) = match assignment {
            crate::claude::Assignment::Group { name, initial } => (Some(name), Some(initial)),
            crate::claude::Assignment::Account(a) => (None, Some(a)),
            crate::claude::Assignment::None => (None, None),
        };
        let id = spec.new_hostname.clone();
        let (email, group_set) = (account.clone(), group.clone());
        app.store.mutate(|s| {
            if let Some(h) = s.hosts.iter_mut().find(|h| h.id == id) {
                h.claude_selection = Some(selection.clone());
                h.claude_account_email = email.clone();
                h.claude_group = group_set.clone();
            }
        });
        match account {
            None => patch_op(&app, &op_id, |op| {
                op.log.push("account: none (no token installed)".into())
            }),
            Some(email) => {
                let label = match &group {
                    Some(g) => format!("{email} (group {g})"),
                    None => email.clone(),
                };
                match crate::claude::push_account_to_clone(&app, &spec.new_hostname, &email).await {
                    Ok(()) => patch_op(&app, &op_id, |op| op.log.push(format!("account: assigned {label}"))),
                    Err(e) => {
                        tracing::warn!("push_account_to_clone({}) failed: {e}", spec.new_hostname);
                        patch_op(&app, &op_id, |op| {
                            op.log.push(format!("account: failed to assign {label}: {e}"))
                        });
                    }
                }
            }
        }
    }

    // Kick off the agent: hand it the ticket URL (ticket clones) or the plain
    // first message, plus any instruction overrides. Detached; it waits for the
    // wrapper to come up.
    let ticket_url = spec.linear.as_ref().and_then(|m| m.ticket_url.clone());
    let has_msg = spec.first_message.as_deref().map(str::trim).is_some_and(|s| !s.is_empty());
    if ticket_url.is_some() || has_msg {
        if let Some(host) = app.store.get().hosts.into_iter().find(|h| h.id == spec.new_hostname) {
            tokio::spawn(crate::chat::kickoff_agent(
                app.clone(),
                host,
                crate::chat::KickoffOpts {
                    ticket_url,
                    message: spec.first_message.clone(),
                    agent_instructions: spec.agent_instructions.clone(),
                    claude_instructions: spec.claude_instructions.clone(),
                },
            ));
        }
    }
}

/// Pull the clone template from `reference` (a registry `repo:tag`) and retag it locally as
/// `rmng/template:<name>`. Drives a `Pull`-kind Operation with the image name as its target;
/// no Host is registered (a template is not a host). Guards (same shape the retired bootstrap
/// had): `name` is a DNS label + no op is already in flight for the same target.
pub fn start_pull(app: &App, name: &str, reference: &str) -> Result<Operation, JobError> {
    if !is_dns_label(name) {
        return Err(JobError(
            "image name must be a DNS label (lowercase letters, digits, hyphens)".into(),
        ));
    }
    let st = app.store.get();
    if st.operations.iter().any(|o| o.status == OperationStatus::Running && o.target == name) {
        return Err(JobError(format!("'{name}' is already being pulled")));
    }
    let op = make_op(OperationKind::Pull, name, None);
    let (ret, op_id) = (op.clone(), op.id.clone());
    app.store.mutate(|s| s.operations.push(op));
    let (app2, name, reference) = (app.clone(), name.to_string(), reference.to_string());
    tokio::spawn(async move { run_pull(app2, op_id, name, reference).await });
    Ok(ret)
}

async fn run_pull(app: App, op_id: String, name: String, reference: String) {
    let progress = pull_op_progress(&app, &op_id);
    let local_ref = match pull_template(&app, &reference, &name, progress).await {
        Ok(r) => r,
        // `{e:#}` (not `e.to_string()`, which prints only the outermost context) — a pull
        // failure's useful part is usually the daemon's verbatim message (e.g. "pull access
        // denied … repository does not exist"), buried under a `with_context` layer.
        Err(e) => return fail_op(&app, &op_id, format!("{e:#}")),
    };
    patch_op(&app, &op_id, |op| {
        op.status = OperationStatus::Done;
        op.step = "done".into();
        op.pct = 100.0;
        op.message = format!("template {local_ref} ready");
        op.finished_at = Some(now_ms());
    });
    schedule_prune(app.clone(), op_id, PRUNE_DONE_MS);
}

/// Validate + register a commit-from-clone op, then drive it in the background. Guards:
/// the host is a managed clone, the target tag is free (no existing image AND no in-flight
/// commit racing for it), and the host has no operation already in flight.
pub fn start_commit(app: &App, host_id: &str, name: &str) -> Result<Operation, JobError> {
    if !is_dns_label(name) {
        return Err(JobError(
            "image name must be a DNS label (lowercase letters, digits, hyphens)".into(),
        ));
    }
    let st = app.store.get();
    let host = st
        .hosts
        .iter()
        .find(|h| h.id == host_id)
        .cloned()
        .ok_or_else(|| JobError(format!("unknown host '{host_id}'")))?;
    if !host.managed {
        return Err(JobError(format!("'{host_id}' is not a managed clone — only clones can be committed")));
    }
    let reference = format!("{}:{}", crate::docker::IMAGE_REPO, name);
    // Reject a tag already targeted by another running commit/pull (a race the pure
    // `image_exists` check in provision can't see yet). The existing-image check happens in
    // provision (needs the daemon); here we only guard the in-flight duplicate.
    if st.operations.iter().any(|o| {
        o.status == OperationStatus::Running
            && matches!(o.kind, OperationKind::Commit | OperationKind::Pull)
            && o.target == name
    }) {
        return Err(JobError(format!("an image named '{name}' is already being built")));
    }
    if st.operations.iter().any(|o| o.status == OperationStatus::Running && o.target == host_id) {
        return Err(JobError(format!("'{host_id}' already has an operation in flight")));
    }

    // Target = the image name (what's being produced); source = the host it's committed from.
    let op = make_op(OperationKind::Commit, name, Some(host_id));
    let (ret, op_id) = (op.clone(), op.id.clone());
    app.store.mutate(|s| s.operations.push(op));

    let app2 = app.clone();
    let host_id = host_id.to_string();
    let (name, source) = (name.to_string(), host.source.clone().unwrap_or_default());
    tokio::spawn(async move { run_commit(app2, op_id, host_id, name, source, reference).await });
    Ok(ret)
}

async fn run_commit(
    app: App,
    op_id: String,
    host_id: String,
    name: String,
    source: String,
    reference: String,
) {
    let progress = op_progress(&app, &op_id, OperationKind::Commit);
    if let Err(e) = commit_clone_image(&app, &host_id, &name, &source, progress).await {
        return fail_op(&app, &op_id, e.to_string());
    }
    patch_op(&app, &op_id, |op| {
        op.status = OperationStatus::Done;
        op.step = "done".into();
        op.pct = 100.0;
        op.message = format!("image {reference} ready");
        op.finished_at = Some(now_ms());
    });
    schedule_prune(app.clone(), op_id, PRUNE_DONE_MS);
}

/// Validate + register a delete op, then drive it in the background. A managed clone is
/// torn down through `provision::delete_clone` (container name == host id); an unmanaged
/// row (a legacy/plain host) is simply removed from state.
pub fn start_delete(app: &App, host_id: &str) -> Result<Operation, JobError> {
    let st = app.store.get();
    let host = st.hosts.iter().find(|h| h.id == host_id).cloned();
    let Some(host) = host else {
        return Err(JobError(format!("unknown host '{host_id}'")));
    };
    if st.operations.iter().any(|o| o.status == OperationStatus::Running && o.target == host_id) {
        return Err(JobError(format!("'{host_id}' already has an operation in flight")));
    }

    let op = make_op(OperationKind::Delete, host_id, None);
    let op_for_return = op.clone();
    let op_id = op.id.clone();
    app.store.mutate(|s| s.operations.push(op));

    let app2 = app.clone();
    let host_id = host_id.to_string();
    let managed = host.managed;
    tokio::spawn(async move { run_delete(app2, op_id, host_id, managed).await });
    Ok(op_for_return)
}

async fn run_delete(app: App, op_id: String, host_id: String, managed: bool) {
    if managed {
        let progress = op_progress(&app, &op_id, OperationKind::Delete);
        if let Err(e) = delete_clone(&app, &host_id, progress).await {
            return fail_op(&app, &op_id, e.to_string());
        }
    } else {
        // Unmanaged row: nothing to tear down, just unregister it.
        patch_op(&app, &op_id, |op| {
            op.step = "remove".into();
            op.pct = 75.0;
            op.message = "unregistering host (no container)".into();
        });
    }

    app.store.mutate(|s| {
        s.hosts.retain(|h| h.id != host_id);
        if s.selected.as_deref() == Some(host_id.as_str()) {
            s.selected = s.hosts.first().map(|h| h.id.clone());
        }
        if let Some(op) = s.operations.iter_mut().find(|o| o.id == op_id) {
            op.status = OperationStatus::Done;
            op.step = "done".into();
            op.pct = 100.0;
            op.message = if managed {
                format!("clone {host_id} destroyed")
            } else {
                "host removed".into()
            };
            op.finished_at = Some(now_ms());
        }
    });
    schedule_prune(app.clone(), op_id, PRUNE_DONE_MS);
    let dd = app.config().data_dir;
    crate::files::delete_notes(&dd, &host_id);
    crate::chat::delete_chat(&dd, &host_id);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    /// A minimal App backed by a throwaway temp data dir (ClaudeStore/state don't touch the
    /// repo). Docker is constructed I/O-free — `fail_stale_ops` never touches it.
    fn test_app() -> App {
        static N: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let dir = std::env::temp_dir().join(format!(
            "rmng-jobs-test-{}-{}",
            std::process::id(),
            N.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let store = Arc::new(crate::state::StateStore::load(dir.join("state.json")).unwrap());
        let cfg = wire::AppConfig { data_dir: dir.to_string_lossy().into_owned(), ..Default::default() };
        App::new(store, cfg)
    }

    fn running_op(id: &str, target: &str) -> Operation {
        Operation {
            id: id.into(),
            kind: OperationKind::Pull,
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

    #[tokio::test]
    async fn fail_stale_ops_marks_running_as_error() {
        let app = test_app();
        app.store.mutate(|s| {
            s.operations.push(running_op("op_a", "tpl-a"));
            // A finished op must be left untouched.
            s.operations.push(Operation { status: OperationStatus::Done, ..running_op("op_b", "tpl-b") });
        });

        fail_stale_ops(&app);

        let st = app.store.get();
        let a = st.operations.iter().find(|o| o.id == "op_a").unwrap();
        assert_eq!(a.status, OperationStatus::Error);
        assert_eq!(a.message, "interrupted by server restart");
        assert!(a.finished_at.is_some());
        assert!(a.log.iter().any(|l| l.contains("interrupted by server restart")));
        let b = st.operations.iter().find(|o| o.id == "op_b").unwrap();
        assert_eq!(b.status, OperationStatus::Done); // untouched
        // No Running op remains, so a same-target op is no longer blocked forever.
        assert!(!st.operations.iter().any(|o| o.status == OperationStatus::Running));
    }

    /// Per-layer pull `Status` events (surfaced to `pull_op_progress` as [`PullProgress::Log`])
    /// must reach the op LOG + message like the retired bootstrap's pull logging did, but
    /// without moving `step` off `"pull"` or perturbing the byte-driven `pct` — that's owned
    /// exclusively by [`PullProgress::Pct`] (`Bytes` events), which must stay message-only.
    #[tokio::test]
    async fn pull_log_event_reaches_op_log_without_moving_pct_or_step() {
        let app = test_app();
        app.store.mutate(|s| s.operations.push(running_op("op_a", "tpl-a")));
        let mut progress = pull_op_progress(&app, "op_a");

        progress(PullProgress::Log { msg: "aaaaaaaaaaaa: Downloading".into() });

        let st = app.store.get();
        let op = st.operations.iter().find(|o| o.id == "op_a").unwrap();
        assert_eq!(op.step, "pull"); // unmoved
        assert_eq!(op.pct, 40.0); // unmoved — pct stays byte-driven
        assert_eq!(op.message, "aaaaaaaaaaaa: Downloading");
        assert!(op.log.iter().any(|l| l == "pull: aaaaaaaaaaaa: Downloading"));

        // A subsequent `Pct` (Bytes) tick updates pct + message but must NOT add a log line —
        // the log stays exactly as the `Log` event left it.
        let log_len_before = op.log.len();
        progress(PullProgress::Pct { pct: 50.0, msg: "pulling docker.io/x:y: 55%".into() });
        let st = app.store.get();
        let op = st.operations.iter().find(|o| o.id == "op_a").unwrap();
        assert_eq!(op.pct, 50.0);
        assert_eq!(op.message, "pulling docker.io/x:y: 55%");
        assert_eq!(op.log.len(), log_len_before); // no new log line from a Pct/Bytes tick
    }
}
