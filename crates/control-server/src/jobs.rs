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

use wire::{Operation, OperationKind, OperationStatus, RmngClone};

use crate::app::App;
use crate::clone_plan::{ClonePlan, Side};
use crate::provision::{
    self, HomeSource, clone_container_gen2, clone_key_env_vars, compose_clone_env,
    control_env_vars, delete_clone, fork_clone, migrate_one, preset_env_vars,
    rebase_clone,
};

const LOG_LIMIT: usize = 200;
pub(crate) const PRUNE_DONE_MS: u64 = 8_000;
pub(crate) const PRUNE_ERROR_MS: u64 = 60_000;

#[derive(Debug)]
pub struct JobError(pub String);
impl std::fmt::Display for JobError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}
impl std::error::Error for JobError {}

fn now_ms() -> i64 {
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

fn make_op(kind: OperationKind, target: &str, source: Option<&str>) -> Operation {
    let message = match kind {
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

/// Append to an operation's progress log. Shared with [`crate::pool`], which logs
/// assignment delivery into the create/fork op it runs under.
pub(crate) fn patch_op(app: &App, op_id: &str, f: impl FnOnce(&mut Operation)) {
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

pub(crate) fn schedule_prune(app: App, op_id: String, delay_ms: u64) {
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
        for op in s
            .operations
            .iter_mut()
            .filter(|o| o.status == OperationStatus::Running)
        {
            op.status = OperationStatus::Error;
            op.message = "interrupted by server restart".into();
            op.log.push("error: interrupted by server restart".into());
            op.finished_at = Some(now_ms());
        }
    });
    for id in stale {
        tracing::warn!(
            op = id.as_str(),
            "marking stale Running op as Error (interrupted by server restart)"
        );
        schedule_prune(app.clone(), id, PRUNE_ERROR_MS);
    }
}

/// How long the preset startup script may run before the create/fork op stops waiting.
/// Best-effort: on timeout the script keeps running in the clone, the op just moves on.
const STARTUP_SCRIPT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(300);
/// Op-log lines keep this tail of the script's combined output; the full text goes to tracing.
const STARTUP_SCRIPT_LOG_TAIL: usize = 4000;

/// Run the effective preset's startup script as the clone user, as the last settle step
/// of create/fork. The script arrives over stdin (`bash -s`), so no quoting layer sits
/// between the Settings text and the interpreter. Best-effort: any failure or timeout is
/// logged to the op, never fatal to the clone — a broken script must not fail a healthy
/// provision.
async fn run_startup_script(app: &App, op_id: &str, clone_id: &str, preset_name: Option<&str>) {
    let cfg = app.config();
    let script = preset_name
        .and_then(|n| cfg.presets.iter().find(|p| p.name == n))
        .map(|p| p.startup_script.trim().to_string())
        .filter(|s| !s.is_empty());
    let Some(script) = script else {
        patch_op(app, op_id, |op| {
            op.log.push("startup script: none configured".into())
        });
        return;
    };
    let cmd = ["bash".to_string(), "-s".to_string()];
    let run = app.docker.exec_capture(
        clone_id,
        &cmd,
        "rmng",
        Some("/home/rmng"),
        &[],
        Some(script.as_bytes()),
    );
    match tokio::time::timeout(STARTUP_SCRIPT_TIMEOUT, run).await {
        Err(_) => {
            tracing::warn!("startup script on {clone_id} timed out (left running)");
            patch_op(app, op_id, |op| {
                op.log
                    .push("startup script: timed out after 5m, left running in the clone".into())
            });
        }
        Ok(Err(e)) => {
            tracing::warn!("startup script on {clone_id} failed to start: {e:#}");
            patch_op(app, op_id, |op| {
                op.log
                    .push(format!("startup script: failed to start: {e:#}"))
            });
        }
        Ok(Ok(out)) => {
            let mut combined = out.stdout;
            if !out.stderr.trim().is_empty() {
                combined.push_str("\n--- stderr ---\n");
                combined.push_str(&out.stderr);
            }
            tracing::info!(
                "startup script on {clone_id} exited {}: {combined}",
                out.exit_code
            );
            // Floor the cut to a char boundary so the slice cannot panic.
            let tail = String::from_utf8_lossy(combined.as_bytes());
            let tail = if tail.len() > STARTUP_SCRIPT_LOG_TAIL {
                let mut cut = tail.len() - STARTUP_SCRIPT_LOG_TAIL;
                while !tail.is_char_boundary(cut) {
                    cut += 1;
                }
                format!("…[truncated]\n{}", &tail[cut..])
            } else {
                tail.into_owned()
            };
            // A failing script is still a successful provision: logged, never fatal.
            patch_op(app, op_id, |op| {
                op.log
                    .push(format!("startup script: exit {}", out.exit_code));
                op.log.push(tail);
            });
        }
    }
}

pub fn start_clone(app: &App, plan: ClonePlan) -> Operation {
    let op = make_op(
        OperationKind::Clone,
        &plan.id,
        plan.source.as_ref().map(|s| s.id.as_str()),
    );
    let op_for_return = op.clone();
    let op_id = op.id.clone();
    app.store.mutate(|s| s.operations.push(op));
    let app2 = app.clone();
    tokio::spawn(async move { run_clone(app2, op_id, plan).await });
    op_for_return
}

/// Build the planned clone: image and home, accounts, then the settle steps that live
/// outside the container. The clone is added to `s.hosts` only at the very end, because a
/// clone's presence there is the client's "ready to connect" signal.
async fn run_clone(app: App, op_id: String, plan: ClonePlan) {
    let id = plan.id.as_str();
    let preset = plan.preset_name.as_deref();
    let progress = op_progress(&app, &op_id, OperationKind::Clone);
    let env = match gen2_create_env(&app, preset, id).await {
        Ok(env) => env,
        Err(e) => return fail_op(&app, &op_id, format!("{e:#}")),
    };
    let (playbook, prompt) = gen2_playbook_prompt(&app, preset);
    let built = match &plan.source {
        // Image: a hash tag built on demand from the preset's Dockerfile. Home: a fresh
        // dataset, or a clone of the template seed snapshot where the template carries one.
        None => {
            let home = match app.config().docker.seed_snapshot.clone().unwrap_or_default() {
                s if !s.trim().is_empty() => HomeSource::CloneFromSnapshot(s),
                _ => HomeSource::Create,
            };
            let dockerfile = crate::provision::preset_dockerfile(&app, preset);
            clone_container_gen2(
                &app,
                &dockerfile,
                id,
                home,
                &env,
                &playbook,
                &prompt,
                plan.headless,
                plan.rebuild,
                progress,
            )
            .await
        }
        Some(src) => {
            fork_clone(
                &app,
                &src.id,
                id,
                &env,
                &playbook,
                &prompt,
                plan.headless,
                preset,
                plan.rebuild,
                progress,
            )
            .await
        }
    };
    let image_ref = match built {
        Ok(v) => v,
        Err(e) => return fail_op(&app, &op_id, format!("{e:#}")),
    };
    // A clone built from an image boots on the template's baked `RMNG_MONITORS`, one monitor
    // nobody chose; bring it to the active layout preset. A fork's home remembers its own.
    if plan.source.is_none() && !plan.headless {
        crate::mediaplane::apply_active_layout_when_ready(app.clone(), plan.id.clone());
    }

    let mut progress = op_progress(&app, &op_id, OperationKind::Clone);
    progress("accounts", "assigning agent accounts");
    let claude = bind_side::<crate::pool::ClaudePool>(&app, &op_id, &plan, &plan.claude).await;
    let codex = bind_side::<crate::pool::CodexPool>(&app, &op_id, &plan, &plan.codex).await;

    // Everything the clone needs that lives outside its container. Each has a reconcile loop
    // that would do this 10 to 15 s after the clone lands in `s.hosts` — which is exactly
    // when the operator opens it and finds the shared folder missing.
    progress(
        "settle",
        "attaching the shared folder, home link and SSH access",
    );
    crate::homes::ensure_now(&app, id).await;
    crate::ssh::allow_clone_now(&app, id).await;

    if plan.run_startup_script {
        progress("settle", "running the preset startup script");
        run_startup_script(&app, &op_id, id, preset).await;
    } else {
        patch_op(&app, &op_id, |op| {
            op.log.push("startup script: skipped by request".into())
        });
    }

    let daemon_up = app.media.is_connected(id);
    let dataset = match &plan.source {
        None => plan.id.clone(),
        Some(_) => crate::zfs::dataset_name(&app.config().docker.homes_parent, id),
    };
    let linear = plan.linear.clone().unwrap_or_default();
    let ticket_url = linear.ticket_url.clone();
    app.store.mutate(|s| {
        s.hosts.insert(
            0,
            RmngClone {
                id: plan.id.clone(),
                host: plan.id.clone(),
                port: 3389,
                username: "rmng".into(),
                password: "rmng".into(),
                managed: true,
                source: Some(image_ref.clone()),
                dataset: Some(dataset),
                base_tag: Some(image_ref),
                claude_selection: claude.0,
                claude_account_email: claude.1,
                claude_group: claude.2,
                codex_selection: codex.0,
                codex_account_email: codex.1,
                codex_group: codex.2,
                group: plan.group.clone(),
                preset_name: plan.preset_name.clone(),
                headless: plan.headless,
                linear_workspace: linear.workspace,
                linear_ticket: linear.ticket,
                linear_ticket_url: linear.ticket_url,
                linear_branch: linear.branch,
                display_name: linear.display_name,
                linear_label: linear.label,
                ..Default::default()
            },
        );
        if let Some(op) = s.operations.iter_mut().find(|o| o.id == op_id) {
            op.status = OperationStatus::Done;
            op.step = "done".into();
            op.pct = 100.0;
            op.message = match (plan.headless, daemon_up) {
                (true, _) => format!("headless clone {id} ready"),
                (false, true) => format!("clone {id} ready"),
                (false, false) => format!(
                    "clone {id} created but its daemon hasn't registered yet (still booting; \
                     check it in the UI)"
                ),
            };
            op.finished_at = Some(now_ms());
        }
    });
    schedule_prune(app.clone(), op_id.clone(), PRUNE_DONE_MS);
    // A forked home carries the source's files with a fresh /etc (no stamps): converge it.
    if plan.source.is_some() {
        crate::clone_reconcile::spawn_converge_after_start(&app, id, "fork");
    }

    // Start the agent on its ticket or first message; a clone with neither stays quiet.
    if ticket_url.is_some() || plan.first_message.is_some() {
        if let Some(host) = app.store.get().hosts.into_iter().find(|h| h.id == plan.id) {
            tokio::spawn(crate::chat::kickoff_agent(
                app.clone(),
                host,
                crate::chat::KickoffOpts {
                    ticket_url,
                    message: plan.first_message.clone(),
                    agent_instructions: plan.agent_instructions.clone(),
                    claude_instructions: plan.claude_instructions.clone(),
                },
            ));
        }
    }
}

/// Settle one provider's account and answer the row's (selection, email, pool). A fork that
/// keeps its source's account still gets the token pushed fresh — never copied from the
/// source's files. Best-effort: a failure is logged into the op, never fatal.
async fn bind_side<P: crate::pool::PoolProvider>(
    app: &App,
    op_id: &str,
    plan: &ClonePlan,
    side: &Side,
) -> (Option<String>, Option<String>, Option<String>) {
    let src = plan.source.as_ref();
    let inherited = (
        src.and_then(|s| P::selection(s).map(str::to_string)),
        src.and_then(|s| P::host_email(s).map(str::to_string)),
        src.and_then(|s| P::sticky(s).map(str::to_string)),
    );
    let requested = match side {
        Side::Inherit => {
            if let Some(email) = inherited.1.clone() {
                let pushed = P::push(app, &plan.id, &email).await;
                patch_op(app, op_id, |op| {
                    op.log.push(match pushed {
                        Ok(()) => format!("{}: inherited {email}", P::OP_LABEL),
                        Err(e) => format!("{}: failed to inherit {email}: {e}", P::OP_LABEL),
                    })
                });
            }
            return inherited;
        }
        Side::Assign(sel) => sel.clone(),
    };
    match crate::pool::assign_clone_side::<P>(
        app,
        Some(op_id),
        &plan.id,
        requested.as_deref(),
        inherited.1.as_deref(),
        plan.group.as_deref(),
        crate::pool::AssignStrictness::BestEffort,
    )
    .await
    {
        Ok(Some(b)) => (Some(b.selection), b.email, b.group),
        Ok(None) => inherited,
        Err(e) => {
            tracing::warn!("unexpected assignment failure: {e:#}");
            patch_op(app, op_id, |op| {
                op.log
                    .push(format!("{}: assignment failed: {e:#}", P::OP_LABEL))
            });
            inherited
        }
    }
}

/// Validate + register a control-server self-update op, then drive it in the background.
/// Guard: reject if ANY operation is running — the swap kills the server, which would abort
/// every in-flight clone/pull/commit. `reference` is `config.docker.serverImage`.
pub fn start_update(app: &App, reference: &str) -> Result<Operation, JobError> {
    let st = app.store.get();
    if st
        .operations
        .iter()
        .any(|o| o.status == OperationStatus::Running)
    {
        return Err(JobError(
            "another operation is in flight; wait for it to finish before updating".into(),
        ));
    }
    let op = make_op(OperationKind::Update, "control-server", None);
    let (ret, op_id) = (op.clone(), op.id.clone());
    app.store.mutate(|s| s.operations.push(op));
    let (app2, reference) = (app.clone(), reference.to_string());
    tokio::spawn(async move { run_update(app2, op_id, reference).await });
    Ok(ret)
}

async fn run_update(app: App, op_id: String, reference: String) {
    // 1. Determine our own container id (can't self-update in dev mode).
    let self_id = match app.docker.env().await.self_container {
        Some(id) => id,
        None => {
            return fail_op(
                &app,
                &op_id,
                "not running as a container (dev mode) — nothing to update".into(),
            );
        }
    };

    // 2. Pull the new image (2–80% of the bar). patch_op writes each tick into the op; the
    //    pull callback borrows (app_cb, op_cb) and calls patch_op directly — no separate
    //    progress closure to fight the borrow checker.
    patch_op(&app, &op_id, |op| {
        op.step = "pull".into();
        op.message = format!("pulling {reference}");
    });
    {
        let (app_cb, op_cb) = (app.clone(), op_id.clone());
        let pull = app
            .docker
            .pull_image(&reference, |ev| match ev {
                crate::docker::PullEvent::Status { layer, status } => {
                    patch_op(&app_cb, &op_cb, |op| {
                        op.log.push(format!("pull: {layer}: {status}"));
                        if op.log.len() > 200 {
                            let d = op.log.len() - 200;
                            op.log.drain(0..d);
                        }
                    });
                }
                crate::docker::PullEvent::Bytes { frac } => {
                    patch_op(&app_cb, &op_cb, |op| {
                        op.pct = op.pct.max(2.0 + frac * 78.0);
                        op.message = format!("pulling {reference}: {}%", (frac * 100.0) as i64);
                    });
                }
            })
            .await;
        if let Err(e) = pull {
            return fail_op(&app, &op_id, format!("pull failed: {e:#}"));
        }
    }

    // 3. Capture our run-spec.
    patch_op(&app, &op_id, |op| {
        op.step = "capture".into();
        op.message = "capturing run-spec".into();
    });
    let resp = match app.docker.inspect_self(&self_id).await {
        Ok(r) => r,
        Err(e) => return fail_op(&app, &op_id, format!("inspecting self: {e:#}")),
    };
    let spec = match crate::docker::SelfSpec::from_inspect(&resp, &reference) {
        Ok(s) => s,
        Err(e) => return fail_op(&app, &op_id, format!("capturing run-spec: {e:#}")),
    };

    // 4. Resolve the target digest (for boot reconcile) from the JUST-PULLED image's own LOCAL
    //    RepoDigest, NOT the registry index descriptor. reconcile compares this against the
    //    running container's local RepoDigest (`self_image_info`), so it must be the same
    //    source/shape: a multi-arch/index image's descriptor digest differs from the platform
    //    image digest the recreated container reports, which would flag every successful update
    //    as a false Error. Best-effort → `None` (reconcile then completes optimistically).
    let target_digest = app.docker.image_repo_digest(&reference).await;

    // 5. Write the handoff + launch the detached helper from the NEW image.
    patch_op(&app, &op_id, |op| {
        op.step = "handoff".into();
        op.message = "handing off to the updater".into();
    });
    let handoff = crate::update::Handoff {
        spec,
        op_id: op_id.clone(),
        target_digest,
    };
    if let Err(e) = crate::update::write_handoff(&handoff) {
        return fail_op(&app, &op_id, format!("writing handoff: {e:#}"));
    }
    let socket = wire::DOCKER_SOCKET.to_string();
    if let Err(e) = app
        .docker
        .launch_upgrade_helper(&reference, &self_id, &socket)
        .await
    {
        crate::update::clear_handoff();
        return fail_op(&app, &op_id, format!("launching updater: {e:#}"));
    }
    // The helper now stops us; this task dies with the container. Leave the op Running at 85%
    // — the rebooted server's reconcile_pending finalizes it.
    patch_op(&app, &op_id, |op| {
        op.pct = op.pct.max(85.0);
        op.message = "updater launched — the server will restart on the new image".into();
    });
}

/// Validate + register a delete op, then drive it in the background. A managed clone is
/// torn down through `provision::delete_clone` (container name == clone id); an unmanaged
/// row (a legacy/plain clone) is simply removed from state.
pub fn start_delete(app: &App, host_id: &str) -> Result<Operation, JobError> {
    let st = app.store.get();
    let host = st.hosts.iter().find(|h| h.id == host_id).cloned();
    let Some(host) = host else {
        return Err(JobError(format!("unknown clone '{host_id}'")));
    };
    if st
        .operations
        .iter()
        .any(|o| o.status == OperationStatus::Running && o.target == host_id)
    {
        return Err(JobError(format!(
            "'{host_id}' already has an operation in flight"
        )));
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
    // Last chance at this clone's transcripts. The `hosts/<id>` symlink disappears the moment the
    // container stops, so whatever has not been tailed by then is gone for good, and a worker
    // clone deletes itself seconds after the last correction is typed. Best effort: it is bounded
    // by its own timeout and it cannot fail the delete.
    crate::ledger::tail_once(&app, &host_id).await;
    if managed {
        let progress = op_progress(&app, &op_id, OperationKind::Delete);
        if let Err(e) = delete_clone(&app, &host_id, progress).await {
            return fail_op(&app, &op_id, format!("{e:#}"));
        }
    } else {
        // Unmanaged row: nothing to tear down, just unregister it.
        patch_op(&app, &op_id, |op| {
            op.step = "remove".into();
            op.pct = 75.0;
            op.message = "unregistering clone (no container)".into();
        });
    }

    // Forget the clone's server-side secrets + push bookkeeping so nothing outlives it: a
    // revoked identity key can never identify as this clone again, and a same-named clone
    // created later starts from a clean slate rather than inheriting stale state.
    app.clone_keys.forget(&host_id);
    app.claude.forget_pushed(&host_id);
    app.codex.forget_pushed(&host_id);

    let previously_selected = app.store.selected();
    let state = app.store.mutate(|s| {
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
                "clone removed".into()
            };
            op.finished_at = Some(now_ms());
        }
    });
    // Deleting the watched clone moves the operator onto another one, which has been holding
    // whatever layout it was last viewed with. Bring it to the active preset, exactly as a
    // deliberate switch would.
    if state.selected != previously_selected {
        if let Some(id) = state.selected.as_deref() {
            crate::mediaplane::apply_active_layout(&app, id);
        }
    }
    schedule_prune(app.clone(), op_id, PRUNE_DONE_MS);
    let dd = app.data_dir();
    crate::files::delete_notes(&dd, &host_id);
    crate::chat::delete_chat(&dd, &host_id);
    // Anything the operator queued for a clone that no longer exists can never be delivered.
    crate::chat::delete_schedules(&dd, &host_id);
}

/// Everything a gen-2 fork/rebase/migrate needs from the source row's preset: the clone's
/// full session env (control URL + per-clone identity key + preset vars; the gen-2 create
/// path filters it to dynamic keys for inject and static keys for the image build).
async fn gen2_create_env(
    app: &App,
    preset_name: Option<&str>,
    new_id: &str,
) -> anyhow::Result<Vec<wire::EnvVar>> {
    let vars: Vec<wire::EnvVar> = app
        .config()
        .presets
        .iter()
        .find(|p| Some(p.name.as_str()) == preset_name)
        .map(preset_env_vars)
        .unwrap_or_default();
    Ok(compose_clone_env(
        control_env_vars(app).await?,
        clone_key_env_vars(app, new_id),
        &vars,
    ))
}

fn gen2_playbook_prompt(app: &App, preset_name: Option<&str>) -> (String, String) {
    let preset = app
        .config()
        .presets
        .into_iter()
        .find(|p| Some(p.name.as_str()) == preset_name);
    let cfg = app.config();
    (
        crate::web::compose_playbook(&cfg, preset.as_ref()),
        crate::web::compose_global_prompt(&cfg, preset.as_ref()),
    )
}

/// Rebase a gen-2 clone onto a preset's image, keeping its dataset and id. Guard: the
/// row must carry a base tag (gen-2), the preset must exist, and no Running op on the
/// clone. The image resolves + builds inside the background run, so build progress
/// streams on the op; `rebuild` forces a fresh build even when the tag exists.
pub fn start_rebase(
    app: &App,
    host_id: &str,
    preset_name: &str,
    rebuild: bool,
) -> Result<Operation, JobError> {
    let st = app.store.get();
    let row = st.hosts.iter().find(|h| h.id == host_id).cloned();
    let Some(row) = row else {
        return Err(JobError(format!("unknown clone '{host_id}'")));
    };
    if !row.managed {
        return Err(JobError(format!("'{host_id}' is not a managed clone")));
    }
    if row.base_tag.is_none() {
        return Err(JobError(format!(
            "'{host_id}' is not a gen-2 clone (no base tag)"
        )));
    }
    let preset_name = preset_name.trim().to_string();
    if preset_name.is_empty() {
        return Err(JobError("a preset is required".into()));
    }
    if !app.config().presets.iter().any(|p| p.name == preset_name) {
        return Err(JobError(format!("unknown preset '{preset_name}'")));
    }
    if st
        .operations
        .iter()
        .any(|o| o.status == OperationStatus::Running && o.target == host_id)
    {
        return Err(JobError(format!(
            "'{host_id}' already has an operation in flight"
        )));
    }
    let op = make_op(OperationKind::Clone, host_id, Some(&preset_name));
    let op_for_return = op.clone();
    let op_id = op.id.clone();
    app.store.mutate(|s| s.operations.push(op));
    let (app2, host_id) = (app.clone(), host_id.to_string());
    tokio::spawn(async move { run_rebase(app2, op_id, host_id, preset_name, rebuild).await });
    Ok(op_for_return)
}

async fn run_rebase(app: App, op_id: String, host_id: String, preset_name: String, rebuild: bool) {
    let mut progress = op_progress(&app, &op_id, OperationKind::Clone);
    let row = match app.store.get().hosts.into_iter().find(|h| h.id == host_id) {
        Some(h) => h,
        None => return fail_op(&app, &op_id, format!("unknown clone '{host_id}'")),
    };
    // Image follows the TARGET preset (built lazily here, so build progress streams on
    // this op); env/playbook stay on the clone's own bindings — rebase swaps the image
    // only, never the preset.
    let dockerfile = crate::provision::preset_dockerfile(&app, Some(&preset_name));
    let new_tag =
        match crate::derived::ensure_image(&app, &dockerfile, rebuild, &mut progress).await {
            Ok(t) => t,
            Err(e) => return fail_op(&app, &op_id, format!("{e:#}")),
        };
    let env = match gen2_create_env(&app, row.preset_name.as_deref(), &host_id).await {
        Ok(env) => env,
        Err(e) => return fail_op(&app, &op_id, format!("{e:#}")),
    };
    let (playbook, prompt) = gen2_playbook_prompt(&app, row.preset_name.as_deref());
    // An archived clone rests stopped, but the swap below boots a container (and the
    // rollback arm recreates one too). Remember the rest state and put it back down
    // afterwards; if the stop fails the archived-state reconciler stops it instead.
    let was_archived = row.archived;
    match rebase_clone(
        &app,
        &host_id,
        &new_tag,
        &env,
        &playbook,
        &prompt,
        row.headless,
        progress,
    )
    .await
    {
        Ok(tag) => {
            if was_archived {
                if let Err(e) = app.docker.stop_even_if_paused(&host_id).await {
                    tracing::warn!(
                        target: "clone",
                        "rebase of archived clone '{host_id}': rest stop failed: {e:#}"
                    );
                }
            }
            app.store.mutate(|s| {
                if let Some(h) = s.hosts.iter_mut().find(|h| h.id == host_id) {
                    h.base_tag = Some(tag.clone());
                    h.source = Some(tag.clone());
                }
                if let Some(op) = s.operations.iter_mut().find(|o| o.id == op_id) {
                    op.status = OperationStatus::Done;
                    op.step = "done".into();
                    op.pct = 100.0;
                    op.message = if was_archived {
                        format!("clone {host_id} rebased onto {tag} (stays archived)")
                    } else {
                        format!("clone {host_id} rebased onto {tag}")
                    };
                    op.finished_at = Some(now_ms());
                }
            });
            schedule_prune(app.clone(), op_id, PRUNE_DONE_MS);
            // Rebases end stopped (archived) or running: the waiter covers both.
            crate::clone_reconcile::spawn_converge_after_start(&app, &host_id, "rebase");
        }
        Err(e) => {
            if was_archived {
                // The rollback recreates from the old tag, which also boots: rest it.
                if let Err(rb) = app.docker.stop_even_if_paused(&host_id).await {
                    tracing::warn!(
                        target: "clone",
                        "rebase of archived clone '{host_id}': rest stop failed: {rb:#}"
                    );
                }
            }
            fail_op(&app, &op_id, format!("{e:#}"))
        }
    }
}

/// Migrate one gen-1 clone (managed row without a dataset). Files a `Migrate` op and
/// drives it; the clone stays STOPPED — the boot loop starts the fleet after the window.
/// Guard: row must exist, be managed, and have no dataset yet; no Running op on it.
pub fn start_migrate(app: &App, host_id: &str) -> Result<Operation, JobError> {
    let st = app.store.get();
    let row = st.hosts.iter().find(|h| h.id == host_id).cloned();
    let Some(row) = row else {
        return Err(JobError(format!("unknown clone '{host_id}'")));
    };
    if !row.managed {
        return Err(JobError(format!("'{host_id}' is not a managed clone")));
    }
    if row.dataset.is_some() {
        return Err(JobError(format!("'{host_id}' is already a gen-2 clone")));
    }
    if st
        .operations
        .iter()
        .any(|o| o.status == OperationStatus::Running && o.target == host_id)
    {
        return Err(JobError(format!(
            "'{host_id}' already has an operation in flight"
        )));
    }
    let op = make_op(OperationKind::Migrate, host_id, None);
    let op_for_return = op.clone();
    let op_id = op.id.clone();
    app.store.mutate(|s| s.operations.push(op));
    let (app2, host_id) = (app.clone(), host_id.to_string());
    tokio::spawn(async move { run_migrate(app2, op_id, host_id).await });
    Ok(op_for_return)
}

async fn run_migrate(app: App, op_id: String, host_id: String) {
    let mut progress = op_progress(&app, &op_id, OperationKind::Migrate);
    let row = match app.store.get().hosts.into_iter().find(|h| h.id == host_id) {
        Some(h) => h,
        None => return fail_op(&app, &op_id, format!("unknown clone '{host_id}'")),
    };
    let Some(base) = row.source.clone() else {
        return fail_op(
            &app,
            &op_id,
            format!("clone '{host_id}' has no source image"),
        );
    };
    // The home copy needs a stable source: stop it first (best-effort — it may already
    // be stopped; the boot loop stops the whole fleet beforehand anyway).
    progress("stop", &format!("stopping {host_id} for migration"));
    if let Err(e) = app.docker.stop_even_if_paused(&host_id).await {
        tracing::warn!("migrate {host_id}: pre-stop failed: {e} (continuing)");
    }
    let env = match gen2_create_env(&app, row.preset_name.as_deref(), &host_id).await {
        Ok(env) => env,
        Err(e) => return fail_op(&app, &op_id, format!("{e:#}")),
    };
    // Playbook/prompt injects are skipped: the copied home already carries the files the
    // gen-1 create wrote; re-injecting would only rewrite identical content.
    match migrate_one(&app, &host_id, &base, &env, "", "", row.headless, progress).await {
        Ok(report) => {
            let dataset = crate::zfs::dataset_name(&app.config().docker.homes_parent, &host_id);
            app.store.mutate(|s| {
                if let Some(h) = s.hosts.iter_mut().find(|h| h.id == host_id) {
                    h.dataset = Some(dataset);
                    h.base_tag = Some(report.tag.clone());
                    h.source = Some(report.tag.clone());
                }
                if let Some(op) = s.operations.iter_mut().find(|o| o.id == op_id) {
                    op.status = OperationStatus::Done;
                    op.step = "done".into();
                    op.pct = 100.0;
                    op.message = format!("clone {host_id} migrated ({} bytes)", report.bytes);
                    op.finished_at = Some(now_ms());
                }
            });
            schedule_prune(app.clone(), op_id, PRUNE_DONE_MS);
            // The fleet restarts after the window: the waiter catches this clone's boot.
            crate::clone_reconcile::spawn_converge_after_start(&app, &host_id, "migrate");
        }
        Err(e) => fail_op(&app, &op_id, format!("{e:#}")),
    }
}

/// Wait until op `op_id` leaves `Running` (poll 5 s). The boot loop runs migrations one
/// at a time through the same `start_migrate` path the API uses, so the jobs UI shows
/// each clone's progress while the window runs.
async fn wait_op_terminal(app: &App, op_id: &str) {
    loop {
        let running = app
            .store
            .get()
            .operations
            .iter()
            .any(|o| o.id == op_id && o.status == OperationStatus::Running);
        if !running {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_secs(5)).await;
    }
}

/// Boot one-shot: migrate every gen-1 row (managed, no dataset) to gen-2, one clone at a
/// time. No gen-1 rows ⇒ no-op. Runs under the whole-LXC backup: per-clone failures log
/// and continue with one retry at the end; the fleet (non-archived) starts after the
/// window, with stored account tokens re-pushed onto the running clones.
pub async fn migrate_all_on_boot(app: App) {
    let gen1: Vec<String> = app
        .store
        .get()
        .hosts
        .into_iter()
        .filter(|h| h.managed && h.dataset.is_none())
        .map(|h| h.id)
        .collect();
    if gen1.is_empty() {
        return;
    }
    tracing::warn!(
        "gen-2 migration: {} gen-1 clone(s) detected, migrating one at a time: {}",
        gen1.len(),
        gen1.join(", ")
    );
    // Stable source for the home copies: stop the whole fleet first (best-effort).
    for h in app
        .store
        .get()
        .hosts
        .into_iter()
        .filter(|h| h.managed && !h.archived)
    {
        if let Err(e) = app.docker.stop_even_if_paused(&h.id).await {
            tracing::warn!("migrate: pre-stopping {} failed: {e} (continuing)", h.id);
        }
    }
    let mut failed: Vec<String> = Vec::new();
    let mut pass = 0;
    for id in gen1.clone() {
        match start_migrate(&app, &id) {
            Ok(op) => {
                wait_op_terminal(&app, &op.id).await;
                let ok = app
                    .store
                    .get()
                    .operations
                    .iter()
                    .any(|o| o.id == op.id && o.status == OperationStatus::Done);
                if ok {
                    pass += 1;
                } else {
                    failed.push(id);
                }
            }
            Err(e) => {
                tracing::warn!("migrate {id}: could not file op: {e}");
                failed.push(id);
            }
        }
    }
    // One retry pass for the failures.
    let mut retry_failed: Vec<String> = Vec::new();
    for id in failed {
        match start_migrate(&app, &id) {
            Ok(op) => {
                wait_op_terminal(&app, &op.id).await;
                let ok = app
                    .store
                    .get()
                    .operations
                    .iter()
                    .any(|o| o.id == op.id && o.status == OperationStatus::Done);
                if ok {
                    pass += 1;
                } else {
                    retry_failed.push(id);
                }
            }
            Err(e) => {
                tracing::warn!("migrate {id}: retry could not file op: {e}");
                retry_failed.push(id);
            }
        }
    }
    // Start the fleet: every migrated non-archived clone, then re-push its stored
    // account tokens (those need running clones). Best-effort per clone.
    let mut started = 0;
    for h in app
        .store
        .get()
        .hosts
        .into_iter()
        .filter(|h| h.managed && !h.archived && h.dataset.is_some())
    {
        if let Err(e) = app.docker.start_container(&h.id).await {
            tracing::warn!("migrate: starting {} failed: {e} (continuing)", h.id);
            continue;
        }
        started += 1;
        if let Some(email) = h.claude_account_email {
            if let Err(e) = crate::claude::push_account_to_clone(&app, &h.id, &email).await {
                tracing::warn!("migrate: re-pushing Claude account to {} failed: {e}", h.id);
            }
        }
        if let Some(email) = h.codex_account_email {
            if let Err(e) = crate::codex::push_account_to_clone(&app, &h.id, &email).await {
                tracing::warn!("migrate: re-pushing Codex account to {} failed: {e}", h.id);
            }
        }
    }
    tracing::warn!(
        "gen-2 migration: {pass} passed, {} failed ({}), {started} started",
        retry_failed.len(),
        retry_failed.join(", "),
    );
}

/// Warm a preset image without creating (`POST /api/images/prebuild`): always rebuild the
/// posted Dockerfile text with a fresh base pull, even when its tag exists. The preset
/// card's rebuild button posts the editor's current text (which may be unsaved); saving
/// is separate. Drives a `Prebuild` op (no coarse pct table — the build streams step
/// lines as messages).
pub fn start_prebuild(app: &App, dockerfile: String) -> Result<Operation, JobError> {
    if dockerfile.trim().is_empty() {
        return Err(JobError("a Dockerfile is required to prebuild".into()));
    }
    let target = wire::config::dockerfile_tag(&dockerfile);
    let st = app.store.get();
    if st.operations.iter().any(|o| {
        o.status == OperationStatus::Running
            && (o.kind == OperationKind::Prebuild || o.target == target)
    }) {
        return Err(JobError("a prebuild or pull is already in flight".into()));
    }
    let op = make_op(OperationKind::Prebuild, &target, None);
    let op_for_return = op.clone();
    let op_id = op.id.clone();
    app.store.mutate(|s| s.operations.push(op));
    let app2 = app.clone();
    tokio::spawn(async move { run_prebuild(app2, op_id, dockerfile).await });
    Ok(op_for_return)
}

async fn run_prebuild(app: App, op_id: String, dockerfile: String) {
    let res = crate::derived::prebuild(&app, &dockerfile, |step, msg| {
        patch_op(&app, &op_id, |op| {
            op.step = step.to_string();
            op.message = msg.to_string();
            op.log.push(format!("{step}: {msg}"));
            if op.log.len() > LOG_LIMIT {
                let drop = op.log.len() - LOG_LIMIT;
                op.log.drain(0..drop);
            }
        });
    })
    .await;
    match res {
        Ok(tag) => {
            patch_op(&app, &op_id, |op| {
                op.status = OperationStatus::Done;
                op.step = "done".into();
                op.pct = 100.0;
                op.message = format!("derived image {tag} ready");
                op.finished_at = Some(now_ms());
            });
            schedule_prune(app.clone(), op_id, PRUNE_DONE_MS);
        }
        Err(e) => fail_op(&app, &op_id, format!("{e:#}")),
    }
}

/// Stop a managed clone without removing its container, volumes, or per-clone files.
pub fn start_archive(app: &App, host_id: &str) -> Result<Operation, JobError> {
    let st = app.store.get();
    let host = st
        .hosts
        .iter()
        .find(|h| h.id == host_id)
        .ok_or_else(|| JobError(format!("unknown clone '{host_id}'")))?;
    if !host.managed {
        return Err(JobError(format!("'{host_id}' is not a managed clone")));
    }
    if host.archived {
        return Err(JobError(format!("'{host_id}' is already archived")));
    }
    if st
        .operations
        .iter()
        .any(|o| o.status == OperationStatus::Running && o.target == host_id)
    {
        return Err(JobError(format!(
            "'{host_id}' already has an operation in flight"
        )));
    }

    let op = make_op(OperationKind::Archive, host_id, None);
    let op_for_return = op.clone();
    let op_id = op.id.clone();
    app.store.mutate(|s| s.operations.push(op));

    let app2 = app.clone();
    let host_id = host_id.to_string();
    tokio::spawn(async move { run_archive(app2, op_id, host_id).await });
    Ok(op_for_return)
}

async fn run_archive(app: App, op_id: String, host_id: String) {
    // Same last chance as a delete. An archived clone refuses exec and `homes` drops its
    // symlink, so its transcripts are unreachable from the moment it stops even though the
    // files themselves survive.
    crate::ledger::tail_once(&app, &host_id).await;
    let mut progress = op_progress(&app, &op_id, OperationKind::Archive);
    // Shut the clone down. Its memory goes back to the host, and restoring is a boot: systemd,
    // the desktop session, the inner Docker daemon and the agent all start again.
    //
    // `stop_even_if_paused` rather than a plain stop, because a clone archived by the build
    // that froze them instead is still paused, and a stop signal sent to frozen processes is
    // one nobody can handle: the daemon waits out the full timeout and then kills.
    progress("stop", "stopping the clone (SIGRTMIN+3, up to 20s)");
    if let Err(e) = app.docker.stop_even_if_paused(&host_id).await {
        return fail_op(&app, &op_id, e.to_string());
    }

    let previously_selected = app.store.selected();
    let state = app.store.mutate(|s| {
        if let Some(host) = s.hosts.iter_mut().find(|h| h.id == host_id) {
            host.archived = true;
            host.monitor_state = None;
            host.activity_unknown = false;
            host.local_ip = None;
            host.unread = false;
        }
        // Archiving the clone the operator is watching has to move them off it. A selection
        // left pointing at a stopped clone aims the viewer at something that will never send
        // another frame, and leaves a still one on screen that looks live. `activate` refuses
        // to select an archived clone, so nothing else would ever clear this.
        if s.selected.as_deref() == Some(host_id.as_str()) {
            s.selected = s
                .hosts
                .iter()
                .find(|h| !h.archived && h.managed)
                .map(|h| h.id.clone());
        }
        if let Some(op) = s.operations.iter_mut().find(|o| o.id == op_id) {
            op.status = OperationStatus::Done;
            op.step = "done".into();
            op.pct = 100.0;
            op.message = format!("clone {host_id} archived");
            op.finished_at = Some(now_ms());
        }
    });
    // The clone the operator lands on has been holding its own layout since it was last
    // viewed. Bring it to the active preset, as a deliberate switch does.
    if state.selected != previously_selected {
        if let Some(id) = state.selected.as_deref() {
            crate::mediaplane::apply_active_layout(&app, id);
        }
    }
    drop(progress);
    schedule_prune(app.clone(), op_id, PRUNE_DONE_MS);
}

/// Start an archived managed clone without recreating it.
pub fn start_unarchive(app: &App, host_id: &str) -> Result<Operation, JobError> {
    let st = app.store.get();
    let host = st
        .hosts
        .iter()
        .find(|h| h.id == host_id)
        .ok_or_else(|| JobError(format!("unknown clone '{host_id}'")))?;
    if !host.managed {
        return Err(JobError(format!("'{host_id}' is not a managed clone")));
    }
    if !host.archived {
        return Err(JobError(format!("'{host_id}' is not archived")));
    }
    if st
        .operations
        .iter()
        .any(|o| o.status == OperationStatus::Running && o.target == host_id)
    {
        return Err(JobError(format!(
            "'{host_id}' already has an operation in flight"
        )));
    }

    let op = make_op(OperationKind::Unarchive, host_id, None);
    let op_for_return = op.clone();
    let op_id = op.id.clone();
    app.store.mutate(|s| s.operations.push(op));

    let app2 = app.clone();
    let host_id = host_id.to_string();
    tokio::spawn(async move { run_unarchive(app2, op_id, host_id).await });
    Ok(op_for_return)
}

async fn run_unarchive(app: App, op_id: String, host_id: String) {
    let mut progress = op_progress(&app, &op_id, OperationKind::Unarchive);
    // Start a stopped clone, and thaw a paused one first. Archiving stops the container, but a
    // clone archived by the build that froze them instead is still paused, and an upgrade must
    // not strand it.
    progress("start", "restoring the archived clone");
    if let Err(e) = app.docker.resume_container(&host_id).await {
        return fail_op(&app, &op_id, e.to_string());
    }

    app.store.mutate(|s| {
        if let Some(host) = s.hosts.iter_mut().find(|h| h.id == host_id) {
            host.archived = false;
            host.monitor_state = None;
            host.activity_unknown = false;
            host.local_ip = None;
            host.unread = false;
        }
        if let Some(op) = s.operations.iter_mut().find(|o| o.id == op_id) {
            op.status = OperationStatus::Done;
            op.step = "done".into();
            op.pct = 100.0;
            op.message = format!("clone {host_id} restored");
            op.finished_at = Some(now_ms());
        }
    });
    drop(progress);
    // A restart rebuilds the container's mount table and gives it a new pid, so the
    // home link is gone even though the clone itself is intact. Re-apply it here for
    // the same reason the create path does: an unarchived clone is presented as ready.
    // (The shared pool and /dev/shm need no re-apply: both are create-time config now.)
    crate::homes::ensure_now(&app, &host_id).await;
    crate::ssh::allow_clone_now(&app, &host_id).await;
    push_current_tokens(&app, &host_id).await;
    // Fresh /etc on a carried-over home: converge content now that it boots.
    crate::clone_reconcile::spawn_converge_after_start(&app, &host_id, "unarchive");
    schedule_prune(app.clone(), op_id, PRUNE_DONE_MS);
}

/// Install both providers' current access tokens into a clone that has just come back.
///
/// An archived clone is skipped by every push pass while it is down ([`crate::claude::
/// push_stale_tokens_for`]) and re-bound without a push by the rotator, so the credentials
/// on its disk are whatever it was archived with — possibly an account that has since been
/// deleted or gone dark. Without this it runs them until the next poll, up to ten minutes of
/// 401s on a clone the operator was just told is ready.
///
/// Best-effort on both halves. A failure here is logged and left to the next reconcile pass.
async fn push_current_tokens(app: &App, host_id: &str) {
    let Some(host) = app.store.get().hosts.into_iter().find(|h| h.id == host_id) else {
        return;
    };
    if let Some(email) = host.claude_account_email.as_deref() {
        if let Err(e) = crate::claude::push_account_to_clone(app, host_id, email).await {
            tracing::warn!("unarchive {host_id}: installing {email}'s Claude token failed: {e}");
        }
    }
    if let Some(email) = host.codex_account_email.as_deref() {
        if let Err(e) = crate::codex::push_account_to_clone(app, host_id, email).await {
            tracing::warn!("unarchive {host_id}: installing {email}'s Codex token failed: {e}");
        }
    }
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
        let cfg = wire::AppConfig::default();
        App::new(store, cfg, &dir.to_string_lossy())
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
            s.operations.push(Operation {
                status: OperationStatus::Done,
                ..running_op("op_b", "tpl-b")
            });
        });

        fail_stale_ops(&app);

        let st = app.store.get();
        let a = st.operations.iter().find(|o| o.id == "op_a").unwrap();
        assert_eq!(a.status, OperationStatus::Error);
        assert_eq!(a.message, "interrupted by server restart");
        assert!(a.finished_at.is_some());
        assert!(
            a.log
                .iter()
                .any(|l| l.contains("interrupted by server restart"))
        );
        let b = st.operations.iter().find(|o| o.id == "op_b").unwrap();
        assert_eq!(b.status, OperationStatus::Done); // untouched
        // No Running op remains, so a same-target op is no longer blocked forever.
        assert!(
            !st.operations
                .iter()
                .any(|o| o.status == OperationStatus::Running)
        );
    }

    /// The self-update swap kills the server, aborting every in-flight clone/pull/commit, so
    /// `start_update` refuses while ANY op is Running.
    #[tokio::test]
    async fn start_update_rejects_when_an_op_is_running() {
        let app = test_app();
        app.store
            .mutate(|s| s.operations.push(running_op("op_x", "some-clone")));
        let err = start_update(&app, "pegasis0/rmng:latest").unwrap_err();
        assert!(
            err.0.contains("in flight") || err.0.contains("already"),
            "got: {}",
            err.0
        );
    }

    #[tokio::test]
    async fn archive_and_unarchive_register_lifecycle_ops() {
        let app = test_app();
        app.store.mutate(|s| {
            s.hosts.push(RmngClone {
                id: "clone-a".into(),
                host: "clone-a".into(),
                managed: true,
                ..Default::default()
            });
        });

        let archive = start_archive(&app, "clone-a").unwrap();
        assert_eq!(archive.kind, OperationKind::Archive);
        assert_eq!(archive.target, "clone-a");
        assert!(
            app.store
                .get()
                .operations
                .iter()
                .any(|op| op.id == archive.id)
        );
        assert!(
            start_unarchive(&app, "clone-a")
                .unwrap_err()
                .0
                .contains("not archived")
        );
    }

    #[tokio::test]
    async fn archive_validation_rejects_unmanaged_and_wrong_state() {
        let app = test_app();
        app.store.mutate(|s| {
            s.hosts.push(RmngClone {
                id: "plain".into(),
                host: "plain".into(),
                ..Default::default()
            });
            s.hosts.push(RmngClone {
                id: "stored".into(),
                host: "stored".into(),
                managed: true,
                archived: true,
                ..Default::default()
            });
        });

        assert!(
            start_archive(&app, "plain")
                .unwrap_err()
                .0
                .contains("not a managed")
        );
        assert!(
            start_archive(&app, "stored")
                .unwrap_err()
                .0
                .contains("already archived")
        );
        let unarchive = start_unarchive(&app, "stored").unwrap();
        assert_eq!(unarchive.kind, OperationKind::Unarchive);
        assert!(
            start_unarchive(&app, "stored")
                .unwrap_err()
                .0
                .contains("in flight")
        );
    }
}
