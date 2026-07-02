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
    self, bootstrap_base_image, clone_container, commit_clone_image, control_env_vars,
    delete_clone, is_dns_label,
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
        OperationKind::Bootstrap => format!("queued base-image build {target}"),
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
        container: None,
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
    let (container, ip) =
        match clone_container(&app, &spec.source_image, &spec.new_hostname, &env, progress).await {
            Ok(v) => v,
            Err(e) => return fail_op(&app, &op_id, e.to_string()),
        };

    // Register the new managed host. Clones ship with fixed `rmng`/`rmng` credentials baked
    // into the base image (the old Proxmox credential-inheritance from a source host is gone —
    // images have no per-host credentials to inherit). RDP port stays 3389 for the media path.
    app.store.mutate(|s| {
        let mut host = Host {
            id: spec.new_hostname.clone(),
            host: ip.clone(),
            port: 3389,
            username: "rmng".into(),
            password: "rmng".into(),
            container: Some(container.clone()),
            source: Some(spec.source_image.clone()),
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
            op.container = Some(container.clone());
            op.status = OperationStatus::Done;
            op.step = "done".into();
            op.pct = 100.0;
            op.message = format!("clone {} ready at {ip}", spec.new_hostname);
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
                match crate::claude::push_account_to_clone(&app, &spec.new_hostname, &container, &email).await {
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

/// Bootstrap the wizard base image `rmng/template:<name>` from the fixed base OS
/// (from-zero). Drives a `Bootstrap`-kind Operation with the image name as its target; no
/// Host is registered (an image is not a host).
pub fn start_bootstrap(app: &App, name: &str) -> Result<Operation, JobError> {
    if !is_dns_label(name) {
        return Err(JobError(
            "image name must be a DNS label (lowercase letters, digits, hyphens)".into(),
        ));
    }
    let st = app.store.get();
    if st.operations.iter().any(|o| o.status == OperationStatus::Running && o.target == name) {
        return Err(JobError(format!("'{name}' is already being built")));
    }
    let op = make_op(OperationKind::Bootstrap, name, None);
    let (ret, op_id) = (op.clone(), op.id.clone());
    app.store.mutate(|s| s.operations.push(op));
    let (app2, name) = (app.clone(), name.to_string());
    tokio::spawn(async move { run_bootstrap(app2, op_id, name).await });
    Ok(ret)
}

async fn run_bootstrap(app: App, op_id: String, name: String) {
    let progress = op_progress(&app, &op_id, OperationKind::Bootstrap);
    let reference = match bootstrap_base_image(&app, &name, progress).await {
        Ok(r) => r,
        Err(e) => return fail_op(&app, &op_id, e.to_string()),
    };
    patch_op(&app, &op_id, |op| {
        op.status = OperationStatus::Done;
        op.step = "done".into();
        op.pct = 100.0;
        op.message = format!("base image {reference} ready");
        op.finished_at = Some(now_ms());
    });
    schedule_prune(app.clone(), op_id, PRUNE_DONE_MS);
}

/// Validate + register a commit-from-clone op, then drive it in the background. Guards:
/// the host is a managed clone (has a container), the target tag is free (no existing image
/// AND no in-flight commit racing for it), and the host has no operation already in flight.
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
    let Some(container) = host.container.clone() else {
        return Err(JobError(format!("'{host_id}' has no container — only managed clones can be committed")));
    };
    let reference = format!("{}:{}", crate::docker::IMAGE_REPO, name);
    // Reject a tag already targeted by another running commit/bootstrap (a race the pure
    // `image_exists` check in provision can't see yet). The existing-image check happens in
    // provision (needs the daemon); here we only guard the in-flight duplicate.
    if st.operations.iter().any(|o| {
        o.status == OperationStatus::Running
            && matches!(o.kind, OperationKind::Commit | OperationKind::Bootstrap)
            && o.target == name
    }) {
        return Err(JobError(format!("an image named '{name}' is already being built")));
    }
    if st.operations.iter().any(|o| o.status == OperationStatus::Running && o.target == host_id) {
        return Err(JobError(format!("'{host_id}' already has an operation in flight")));
    }

    // Target = the image name (what's being produced); source = the host it's committed from.
    let mut op = make_op(OperationKind::Commit, name, Some(host_id));
    op.container = Some(container.clone());
    let (ret, op_id) = (op.clone(), op.id.clone());
    app.store.mutate(|s| s.operations.push(op));

    let app2 = app.clone();
    let (name, source) = (name.to_string(), host.source.clone().unwrap_or_default());
    tokio::spawn(async move { run_commit(app2, op_id, container, name, source, reference).await });
    Ok(ret)
}

async fn run_commit(
    app: App,
    op_id: String,
    container: String,
    name: String,
    source: String,
    reference: String,
) {
    let progress = op_progress(&app, &op_id, OperationKind::Commit);
    if let Err(e) = commit_clone_image(&app, &container, &name, &source, progress).await {
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

/// Validate + register a delete op, then drive it in the background. A managed clone
/// (`container: Some`) is torn down through `provision::delete_clone`; an unmanaged row
/// (`container: None` — a legacy/plain host) is simply removed from state.
pub fn start_delete(app: &App, host_id: &str) -> Result<Operation, JobError> {
    let st = app.store.get();
    let host = st.hosts.iter().find(|h| h.id == host_id).cloned();
    let Some(host) = host else {
        return Err(JobError(format!("unknown host '{host_id}'")));
    };
    if st.operations.iter().any(|o| o.status == OperationStatus::Running && o.target == host_id) {
        return Err(JobError(format!("'{host_id}' already has an operation in flight")));
    }

    let mut op = make_op(OperationKind::Delete, host_id, None);
    op.container = host.container.clone();
    let op_for_return = op.clone();
    let op_id = op.id.clone();
    app.store.mutate(|s| s.operations.push(op));

    let app2 = app.clone();
    let host_id = host_id.to_string();
    let container = host.container.clone();
    tokio::spawn(async move { run_delete(app2, op_id, host_id, container).await });
    Ok(op_for_return)
}

async fn run_delete(app: App, op_id: String, host_id: String, container: Option<String>) {
    if let Some(container) = &container {
        let progress = op_progress(&app, &op_id, OperationKind::Delete);
        if let Err(e) = delete_clone(&app, container, &host_id, progress).await {
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
            op.message = match &container {
                Some(_) => format!("clone {host_id} destroyed"),
                None => "host removed".into(),
            };
            op.finished_at = Some(now_ms());
        }
    });
    schedule_prune(app.clone(), op_id, PRUNE_DONE_MS);
    let dd = app.config().data_dir;
    crate::files::delete_notes(&dd, &host_id);
    crate::chat::delete_chat(&dd, &host_id);
}
