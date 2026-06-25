//! Operation lifecycle — wraps the Proxmox SSH calls in an `Operation` persisted
//! into `ControlState` and streamed to the UI over SSE. Ported from
//! `jobs.server.ts`. Jobs run in the background: the API creates the op and
//! returns its id immediately; updates flow over `/events`.
//!
//! Difference from the legacy flow: there is **no `wait-swap`** step — rmng
//! clones have no g-r-d holder handing a desktop to an RDP client; a clone is
//! ready once its CT is up and registered. (Account assignment + agent kickoff
//! hooks are wired as Phases 2's Claude/chat modules land.)

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use wire::{Host, Operation, OperationKind, OperationStatus};

use crate::app::App;
use crate::orchestrate::{clone_ct, delete_ct, is_dns_label};

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
    pub workspace: Option<wire::LinearWorkspace>,
    pub ticket: Option<String>,
    pub ticket_url: Option<String>,
    pub branch: Option<String>,
    pub display_name: Option<String>,
    pub label: Option<String>,
}

/// Everything the API hands to `start_clone`.
#[derive(Debug, Clone, Default)]
pub struct CloneSpec {
    pub source_id: String,
    pub new_hostname: String,
    pub linear: Option<LinearMeta>,
    /// Requested Claude account: an email, `"auto"`, or `None` (= auto).
    pub claude_account: Option<String>,
    pub first_message: Option<String>,
    pub agent_instructions: Option<String>,
    pub claude_instructions: Option<String>,
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

fn clone_pct(step: &str) -> Option<f64> {
    Some(match step {
        "queued" => 0.0,
        "locate" => 8.0,
        "storage" => 15.0,
        "allocate" => 22.0,
        "shutdown-source" => 32.0,
        "snapshot" => 48.0,
        "start-source" => 58.0,
        "identity" => 66.0,
        "config" => 74.0,
        "start-clone" => 84.0,
        "wait-lease" => 92.0,
        "done" => 100.0,
        _ => return None,
    })
}

fn delete_pct(step: &str) -> Option<f64> {
    Some(match step {
        "queued" => 0.0,
        "check" => 15.0,
        "stop" => 45.0,
        "destroy" => 80.0,
        "done" => 100.0,
        _ => return None,
    })
}

fn make_op(kind: OperationKind, target: &str, source: Option<&str>) -> Operation {
    let message = match kind {
        OperationKind::Clone => format!("queued clone of {}", source.unwrap_or("?")),
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
        ctid: None,
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

/// Validate + register a clone op, then drive it in the background.
pub fn start_clone(app: &App, spec: CloneSpec) -> Result<Operation, JobError> {
    if !is_dns_label(&spec.source_id) {
        return Err(JobError("source id must be a DNS label".into()));
    }
    if !is_dns_label(&spec.new_hostname) {
        return Err(JobError(
            "new hostname must be a DNS label (lowercase letters, digits, hyphens)".into(),
        ));
    }
    let st = app.store.get();
    if !st.hosts.iter().any(|h| h.id == spec.source_id) {
        return Err(JobError(format!("unknown source host '{}'", spec.source_id)));
    }
    if st.hosts.iter().any(|h| h.id == spec.new_hostname) {
        return Err(JobError(format!("a host named '{}' already exists", spec.new_hostname)));
    }
    if st.operations.iter().any(|o| o.status == OperationStatus::Running && o.target == spec.new_hostname) {
        return Err(JobError(format!("'{}' is already being created", spec.new_hostname)));
    }
    if st.operations.iter().any(|o| {
        o.status == OperationStatus::Running
            && o.kind == OperationKind::Clone
            && o.source.as_deref() == Some(spec.source_id.as_str())
    }) {
        return Err(JobError(format!("source '{}' is busy with another clone", spec.source_id)));
    }

    let op = make_op(OperationKind::Clone, &spec.new_hostname, Some(&spec.source_id));
    let op_for_return = op.clone();
    let op_id = op.id.clone();
    app.store.mutate(|s| s.operations.push(op));

    let app2 = app.clone();
    tokio::spawn(async move { run_clone(app2, op_id, spec).await });
    Ok(op_for_return)
}

async fn run_clone(app: App, op_id: String, spec: CloneSpec) {
    let cfg = app.config();
    let progress = {
        let app = app.clone();
        let op_id = op_id.clone();
        move |step: &str, msg: &str| {
            let pct = clone_pct(step);
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
    };

    let (ctid, ip) = match clone_ct(&cfg.proxmox, &spec.source_id, &spec.new_hostname, progress).await {
        Ok(v) => v,
        Err(e) => return fail_op(&app, &op_id, e.to_string()),
    };

    // Register the new host (inheriting credentials from the source) and complete.
    app.store.mutate(|s| {
        let src = s.hosts.iter().find(|h| h.id == spec.source_id).cloned();
        let mut host = Host {
            id: spec.new_hostname.clone(),
            host: ip.clone(),
            port: src.as_ref().map(|h| h.port).unwrap_or(3389),
            username: src.as_ref().map(|h| h.username.clone()).unwrap_or_default(),
            password: src.as_ref().map(|h| h.password.clone()).unwrap_or_default(),
            domain: src.as_ref().and_then(|h| h.domain.clone()),
            gdm_username: src.as_ref().and_then(|h| h.gdm_username.clone()),
            gdm_password: src.as_ref().and_then(|h| h.gdm_password.clone()),
            ctid: Some(ctid),
            source: Some(spec.source_id.clone()),
            ..Default::default()
        };
        if let Some(m) = &spec.linear {
            host.linear_workspace = m.workspace;
            host.linear_ticket = m.ticket.clone();
            host.linear_ticket_url = m.ticket_url.clone();
            host.linear_branch = m.branch.clone();
            host.display_name = m.display_name.clone();
            host.linear_label = m.label.clone();
        }
        s.hosts.push(host);
        if let Some(op) = s.operations.iter_mut().find(|o| o.id == op_id) {
            op.ctid = Some(ctid);
            op.status = OperationStatus::Done;
            op.step = "done".into();
            op.pct = 100.0;
            op.message = format!("CT {ctid} ready at {ip}");
            op.finished_at = Some(now_ms());
        }
    });
    schedule_prune(app.clone(), op_id.clone(), PRUNE_DONE_MS);

    // Assign a Claude account: stamp the email (UI shows it immediately) then
    // install the long-lived token into the clone's ~/.claude/.credentials.json.
    if let Some(account) = crate::claude::resolve_clone_account(&app, spec.claude_account.as_deref()) {
        let id = spec.new_hostname.clone();
        let email = account.email.clone();
        app.store.mutate(|s| {
            if let Some(h) = s.hosts.iter_mut().find(|h| h.id == id) {
                h.claude_account_email = Some(email.clone());
            }
        });
        if let Some(host) = app.store.get().hosts.into_iter().find(|h| h.id == spec.new_hostname) {
            match crate::claude::apply_clone_token(&host, &account.long_lived_token).await {
                Ok(()) => patch_op(&app, &op_id, |op| {
                    op.log.push(format!("account: assigned {}", account.email))
                }),
                Err(e) => {
                    tracing::warn!("apply_clone_token({}) failed: {e}", spec.new_hostname);
                    patch_op(&app, &op_id, |op| {
                        op.log.push(format!("account: failed to assign {}: {e}", account.email))
                    });
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

/// Bootstrap a template/clone from a base image (from-zero). Registers the result
/// as a template (clonable). Drives a clone-kind Operation with no source.
pub fn start_bootstrap(app: &App, hostname: &str) -> Result<Operation, JobError> {
    if !is_dns_label(hostname) {
        return Err(JobError("hostname must be a DNS label".into()));
    }
    let st = app.store.get();
    if st.hosts.iter().any(|h| h.id == hostname) {
        return Err(JobError(format!("a host named '{hostname}' already exists")));
    }
    if st.operations.iter().any(|o| o.status == OperationStatus::Running && o.target == hostname) {
        return Err(JobError(format!("'{hostname}' is already being created")));
    }
    let op = make_op(OperationKind::Clone, hostname, None);
    let (ret, op_id) = (op.clone(), op.id.clone());
    app.store.mutate(|s| s.operations.push(op));
    let (app2, host) = (app.clone(), hostname.to_string());
    tokio::spawn(async move { run_bootstrap(app2, op_id, host).await });
    Ok(ret)
}

async fn run_bootstrap(app: App, op_id: String, hostname: String) {
    let cfg = app.config();
    let progress = {
        let (app, op_id) = (app.clone(), op_id.clone());
        move |step: &str, msg: &str| {
            let pct = clone_pct(step);
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
    };
    let (ctid, ip) = match crate::orchestrate::bootstrap_template(&cfg, &hostname, progress).await {
        Ok(v) => v,
        Err(e) => return fail_op(&app, &op_id, e.to_string()),
    };
    app.store.mutate(|s| {
        s.hosts.push(Host { id: hostname.clone(), host: ip.clone(), port: 3389, ctid: Some(ctid), ..Default::default() });
        if !s.templates.iter().any(|t| t == &hostname) {
            s.templates.push(hostname.clone()); // bootstrapped CT is a clonable template
        }
        if let Some(op) = s.operations.iter_mut().find(|o| o.id == op_id) {
            op.ctid = Some(ctid);
            op.status = OperationStatus::Done;
            op.step = "done".into();
            op.pct = 100.0;
            op.message = format!("template {hostname} (CT {ctid}) ready at {ip}");
            op.finished_at = Some(now_ms());
        }
    });
    schedule_prune(app.clone(), op_id, PRUNE_DONE_MS);
}

/// Validate + register a delete op, then drive it in the background.
pub fn start_delete(app: &App, host_id: &str) -> Result<Operation, JobError> {
    let st = app.store.get();
    let host = st.hosts.iter().find(|h| h.id == host_id).cloned();
    let Some(host) = host else {
        return Err(JobError(format!("unknown host '{host_id}'")));
    };
    if st.templates.iter().any(|t| t == host_id) {
        return Err(JobError(format!("'{host_id}' is a template and cannot be deleted")));
    }
    if st.operations.iter().any(|o| o.status == OperationStatus::Running && o.target == host_id) {
        return Err(JobError(format!("'{host_id}' already has an operation in flight")));
    }

    let mut op = make_op(OperationKind::Delete, host_id, None);
    op.ctid = host.ctid;
    let op_for_return = op.clone();
    let op_id = op.id.clone();
    app.store.mutate(|s| s.operations.push(op));

    let app2 = app.clone();
    let host_id = host_id.to_string();
    tokio::spawn(async move { run_delete(app2, op_id, host_id, host.ctid).await });
    Ok(op_for_return)
}

async fn run_delete(app: App, op_id: String, host_id: String, ctid: Option<u32>) {
    let cfg = app.config();
    if let Some(ctid) = ctid {
        let progress = {
            let app = app.clone();
            let op_id = op_id.clone();
            move |step: &str, msg: &str| {
                let pct = delete_pct(step);
                patch_op(&app, &op_id, |op| {
                    op.step = step.to_string();
                    if let Some(p) = pct {
                        op.pct = p;
                    }
                    op.message = msg.to_string();
                    op.log.push(format!("{step}: {msg}"));
                });
            }
        };
        if let Err(e) = delete_ct(&cfg.proxmox, ctid, progress).await {
            return fail_op(&app, &op_id, e.to_string());
        }
    } else {
        patch_op(&app, &op_id, |op| {
            op.step = "destroy".into();
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
            op.message = match ctid {
                Some(c) => format!("CT {c} destroyed"),
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
