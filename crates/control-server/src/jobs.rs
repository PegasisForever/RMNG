//! The clone flows — delete, rebase, migrate, prebuild, archive, unarchive and create — plus
//! the boot-time fleet passes that drive them. Ported from `jobs.server.ts`; the backend is
//! now `provision.rs` (bollard), not the retired SSH+`pct` path. Jobs run in the background:
//! the API creates the operation and returns its id immediately; updates flow over `/events`.
//!
//! Everything that SURROUNDS a flow — filing the `Operation`, the guard pass, progress
//! scoring, the op-log cap, finalizing, pruning — lives in [`crate::operation`], once. A flow
//! here is a `start_*` that builds an [`OpSpec`] (what it is, what it targets, what must be
//! true first, which step→pct table scores it) and a `run_*` body that does the work against
//! an [`OpHandle`] and hands back a [`Finish`]: its completion message and the state edit the
//! runner applies in the same mutate. The step→pct tables themselves live in `provision`,
//! next to the code that emits the step keys.

use wire::{Operation, OperationKind, OperationStatus, RmngClone};

use crate::app::App;
use crate::clone_plan::{ClonePlan, Side};
use crate::operation::{self, Finish, Guards, OpHandle, OpSpec};
use crate::provision::{
    self, HomeSource, clone_container_gen2_from_tag, clone_key_env_vars, compose_clone_env,
    control_env_vars, delete_clone, fork_clone, migrate_one, preset_env_vars, rebase_clone,
};

/// The operation-record plumbing, re-exported at its historical path: `pool` logs account
/// delivery into the create/fork op it runs under, and `update` finishes and prunes the
/// self-update op that outlived the server process that filed it.
pub(crate) use crate::operation::{
    JobError, PRUNE_DONE_MS, PRUNE_ERROR_MS, now_ms, patch_op, schedule_prune,
};

/// Mark every persisted `Running` operation as `Error` ("interrupted by server restart") and
/// schedule it for prune. Called once at boot: an `Operation` lives only while its driving
/// task runs, so any `Running` op loaded from `state.json` is a corpse from a server that
/// crashed/was killed mid-op. Left as-is it blocks same-named ops forever (the runner's
/// `idle_target` guard rejects a target with a Running op). Touches only state, so it's safe
/// with Docker down.
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

/// Home-relative path of the startup-script skip stamp: the sha256 of the script text
/// that last ran cleanly (exit 0) on this home lineage.
const STARTUP_SCRIPT_STAMP_REL: &str = ".config/rmng/startup-script.sha256";

/// Stable content hash of one preset startup script for the skip stamp.
fn startup_script_stamp(script: &str) -> String {
    use sha2::{Digest, Sha256};
    format!("{:x}", Sha256::digest(script.as_bytes()))
}

/// Run the effective preset's startup script as the clone user, as the last settle step
/// of create/fork. The script arrives over stdin (`bash -s`), so no quoting layer sits
/// between the Settings text and the interpreter. Best-effort: any failure or timeout is
/// logged to the op, never fatal to the clone — a broken script must not fail a healthy
/// provision.
async fn run_startup_script(app: &App, op: &OpHandle, clone_id: &str, preset_name: Option<&str>) {
    let cfg = app.config();
    let script = preset_name
        .and_then(|n| cfg.presets.iter().find(|p| p.name == n))
        .map(|p| p.startup_script.trim().to_string())
        .filter(|s| !s.is_empty());
    let Some(script) = script else {
        op.log("startup script: none configured");
        return;
    };
    // A fork carries its source's home, script effects included: when this exact text
    // already ran cleanly on this home lineage, re-running is pure wait (and a hazard
    // for a non-idempotent script). A missing or mismatched stamp always runs, so
    // pre-stamp clones, edited scripts and fresh homes behave exactly as before.
    let stamp = startup_script_stamp(&script);
    if crate::home_overlay::read_home_file(
        std::path::Path::new(crate::zfs::HOMES_DIR),
        clone_id,
        STARTUP_SCRIPT_STAMP_REL,
    )
    .is_ok_and(|cur| cur.as_deref() == Some(stamp.as_bytes()))
    {
        op.log("startup script: already applied on this home, skipped");
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
            op.log("startup script: timed out after 5m, left running in the clone");
        }
        Ok(Err(e)) => {
            tracing::warn!("startup script on {clone_id} failed to start: {e:#}");
            op.log(format!("startup script: failed to start: {e:#}"));
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
            op.log(format!("startup script: exit {}", out.exit_code));
            op.log(tail);
            // Record a clean run so a fork carrying this home skips the re-run above.
            // A failed run stamps nothing: the next fork tries again. A stamp that
            // fails to write is a warning, never a failed clone.
            if out.exit_code == 0 {
                if let Err(e) = crate::home_overlay::write_home_file(
                    std::path::Path::new(crate::zfs::HOMES_DIR),
                    clone_id,
                    STARTUP_SCRIPT_STAMP_REL,
                    stamp.as_bytes(),
                    0o644,
                ) {
                    tracing::warn!("startup script stamp on {clone_id} not written: {e:#}");
                    op.log("startup script: applied, but the skip-stamp was not written");
                }
            }
        }
    }
}

// --- create / fork ----------------------------------------------------------------------

pub fn start_clone(app: &App, plan: ClonePlan) -> Operation {
    let mut spec = OpSpec::new(OperationKind::Clone, plan.id.clone())
        .steps(provision::clone_pct)
        // The one flow that files under `Guards::none()`. A create has no row to guard: the
        // id it targets is the one it is about to add, and `clone_plan::plan` has already
        // refused a name that collides with an existing clone or a retired one. Nothing in
        // the shared guard set has anything to look at, so nothing can refuse it.
        .guards(Guards::none());
    if let Some(src) = plan.source.as_ref() {
        spec = spec.source(src.id.clone());
    }
    operation::run_op_unguarded(app, spec, move |app, op| run_clone(app, op, plan))
}

/// Build the planned clone: image and home, accounts, then the settle steps that live
/// outside the container. The clone is added to `s.hosts` only at the very end — through the
/// runner's finalize, in the same mutate as the finished operation — because a clone's
/// presence there is the client's "ready to connect" signal.
async fn run_clone(app: App, op: OpHandle, plan: ClonePlan) -> anyhow::Result<Finish> {
    let id = plan.id.clone();
    let preset = plan.preset_name.clone();
    let mut progress = op.progress();
    let env = gen2_create_env(&app, preset.as_deref(), &id).await?;
    let (playbook, prompt) = gen2_playbook_prompt(&app, preset.as_deref());
    let image_ref = match &plan.source {
        // Image: a hash tag built on demand from the preset's Dockerfile. Home: a fresh
        // dataset, or a clone of the template seed snapshot where the template carries one.
        None => {
            let home = match app
                .config()
                .docker
                .seed_snapshot
                .clone()
                .unwrap_or_default()
            {
                s if !s.trim().is_empty() => HomeSource::CloneFromSnapshot(s),
                _ => HomeSource::Create,
            };
            let dockerfile = crate::provision::preset_dockerfile(&app, preset.as_deref());
            let tag = crate::derived::ensure_image(&app, &dockerfile, plan.rebuild, &mut progress)
                .await?;
            clone_container_gen2_from_tag(
                &app,
                &tag,
                &id,
                home,
                &env,
                &playbook,
                &prompt,
                plan.headless,
                progress,
            )
            .await?
        }
        Some(src) => {
            fork_clone(
                &app,
                &src.id,
                &id,
                &env,
                &playbook,
                &prompt,
                plan.headless,
                preset.as_deref(),
                plan.rebuild,
                progress,
            )
            .await?
        }
    };
    // A clone built from an image boots on one monitor nobody chose (the daemon's
    // 1920x1080 default); bring it to the active layout preset. A fork's home remembers
    // its own.
    if plan.source.is_none() && !plan.headless {
        crate::mediaplane::apply_active_layout_when_ready(app.clone(), id.clone());
    }

    op.step("accounts", "assigning agent accounts");
    // The two sides touch different pools and different credential files, so they run
    // together: same assignments, one wait instead of two. (Log line order between the
    // two may vary; the row below uses both results either way.)
    let (claude, codex) = tokio::join!(
        bind_side::<crate::pool::ClaudePool>(&app, &op, &plan, &plan.claude),
        bind_side::<crate::pool::CodexPool>(&app, &op, &plan, &plan.codex)
    );

    // Everything the clone needs that lives outside its container. Each has a reconcile loop
    // that would do this 10 to 15 s after the clone lands in `s.hosts` — which is exactly
    // when the operator opens it and finds the shared folder missing.
    op.step(
        "settle",
        "attaching the shared folder, home link and SSH access",
    );
    // A homes symlink and the bastion allowlist: different files, one wait.
    tokio::join!(
        crate::homes::ensure_now(&app, &id),
        crate::ssh::allow_clone_now(&app, &id)
    );

    if plan.run_startup_script {
        op.step("settle", "running the preset startup script");
        run_startup_script(&app, &op, &id, preset.as_deref()).await;
    } else {
        op.log("startup script: skipped by request");
    }

    let daemon_up = app.media.is_connected(&id);
    // Recorded so the row reads as gen-2 (`clone_home::is_gen2`). The VALUE is derivable
    // and nothing reads it back — it comes off the clone's own home here so that the row
    // and every later path agree by construction. Writing it by hand is what once put the
    // bare id in this field, and a bare id parses as a pool name: the boot remount then
    // failed and left the clone showing the template home with its real one unmounted.
    let dataset = crate::clone_home::CloneHome::of(&app, &id).dataset();
    let linear = plan.linear.clone().unwrap_or_default();
    let ticket_url = linear.ticket_url.clone();
    let row = RmngClone {
        id: id.clone(),
        host: id.clone(),
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
        parent: plan.parent.clone(),
        preset_name: plan.preset_name.clone(),
        headless: plan.headless,
        linear_workspace: linear.workspace,
        linear_ticket: linear.ticket,
        linear_ticket_url: linear.ticket_url,
        linear_branch: linear.branch,
        display_name: linear.display_name,
        linear_label: linear.label,
        ..Default::default()
    };
    let message = match (plan.headless, daemon_up) {
        (true, _) => format!("headless clone {id} ready"),
        (false, true) => format!("clone {id} ready"),
        (false, false) => format!(
            "clone {id} created but its daemon hasn't registered yet (still booting; \
             check it in the UI)"
        ),
    };
    let first_message = plan.first_message.clone();
    let agent_instructions = plan.agent_instructions.clone();
    let claude_instructions = plan.claude_instructions.clone();
    Ok(Finish::new(message)
        .state(move |s| s.hosts.insert(0, row))
        .after(move |_app, _st| async move {
            // No post-op converge on this path: the pre-boot tar already uploaded every
            // stamp the converge would check (payload, codex parity, ssh, the five
            // managed-home merges), so a fork's converge is a provable no-op wrapped in
            // a 30-minute poll task. Rebase/migrate/unarchive keep theirs (their
            // pre-boot coverage differs).
            // TEMPORARILY DISABLED — the only automatic first message in the server, and
            // the only way a turn starts without an operator asking for one. Nothing may
            // prompt a clone but the web UI's composer until the kickoff is reworked.
            //
            // Restore by uncommenting this block, dropping the `let _` below, and removing
            // the `#[allow(dead_code)]` from `chat::kickoff_agent` and `chat::KickoffOpts`.
            //
            // Start the agent on its ticket or first message; a clone with neither stays
            // quiet.
            // if ticket_url.is_some() || first_message.is_some() {
            //     if let Some(host) = app.store.get().hosts.into_iter().find(|h| h.id == id) {
            //         tokio::spawn(crate::chat::kickoff_agent(
            //             app.clone(),
            //             host,
            //             crate::chat::KickoffOpts {
            //                 ticket_url,
            //                 message: first_message,
            //                 agent_instructions,
            //                 claude_instructions,
            //             },
            //         ));
            //     }
            // }
            //
            // The plan still carries all four: they are recorded on the row and read back by
            // the clone dialog, so they are collected and stored exactly as before — only the
            // send is gone.
            let _ = (&ticket_url, &first_message, &agent_instructions, &claude_instructions);
        }))
}

/// Settle one provider's account and answer the row's (selection, email, pool).
/// Best-effort: a failure is logged into the op, never fatal.
async fn bind_side<P: crate::pool::PoolProvider>(
    app: &App,
    op: &OpHandle,
    plan: &ClonePlan,
    side: &Side,
) -> (Option<String>, Option<String>, Option<String>) {
    let requested = match side {
        Side::Assign(sel) => sel.clone(),
    };
    // A new clone has no incumbent account: stickiness toward the source's account would
    // keep what the plan just refused to inherit, so the rotator always picks fresh.
    let current: Option<&str> = None;
    match crate::pool::assign_clone_side::<P>(
        app,
        Some(op.id()),
        &plan.id,
        requested.as_deref(),
        current,
        plan.group.as_deref(),
        crate::pool::AssignStrictness::BestEffort,
    )
    .await
    {
        Ok(Some(b)) => (Some(b.selection), b.email, b.group),
        // No account can take this side yet (or the assign failed): follow the group with
        // nothing installed, never the source's account.
        Ok(None) => (
            Some(crate::pool::normalize_selection(requested.as_deref())),
            None,
            plan.group.clone(),
        ),
        Err(e) => {
            tracing::warn!("unexpected assignment failure: {e:#}");
            op.log(format!("{}: assignment failed: {e:#}", P::OP_LABEL));
            (
                Some(crate::pool::normalize_selection(requested.as_deref())),
                None,
                plan.group.clone(),
            )
        }
    }
}

// --- control-server self-update -----------------------------------------------------------

/// Validate + register a control-server self-update op, then drive it in the background.
/// `reference` is `config.docker.serverImage`.
///
/// This is the one operation that does NOT run under [`operation::run_op`]: its body hands
/// off to a helper that stops this container, so the task never returns a `Finish` and the
/// operation deliberately ends this process still `Running` at 85%. It files through
/// [`operation::file_op`] so the guard pass is still the shared one — here
/// `Guards::idle_fleet`, because the swap kills the server and would abort every in-flight
/// clone/delete/rebase with it.
pub fn start_update(app: &App, reference: &str) -> Result<Operation, JobError> {
    let spec = OpSpec::new(OperationKind::Update, "control-server").guards(Guards {
        idle_fleet: true,
        ..Guards::none()
    });
    let op = operation::file_op(app, spec)?;
    let op_id = op.id.clone();
    let (app2, reference) = (app.clone(), reference.to_string());
    tokio::spawn(async move { run_update(app2, op_id, reference).await });
    Ok(op)
}

async fn run_update(app: App, op_id: String, reference: String) {
    // 1. Determine our own container id (can't self-update in dev mode).
    let self_id = match app.docker.env().await.self_container {
        Some(id) => id,
        None => {
            return operation::fail_op(
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
                        // The shared cap, not a third copy of it.
                        operation::push_capped(&mut op.log, format!("pull: {layer}: {status}"));
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
            return operation::fail_op(&app, &op_id, format!("pull failed: {e:#}"));
        }
    }

    // 3. Capture our run-spec.
    patch_op(&app, &op_id, |op| {
        op.step = "capture".into();
        op.message = "capturing run-spec".into();
    });
    let resp = match app.docker.inspect_self(&self_id).await {
        Ok(r) => r,
        Err(e) => return operation::fail_op(&app, &op_id, format!("inspecting self: {e:#}")),
    };
    let spec = match crate::docker::SelfSpec::from_inspect(&resp, &reference) {
        Ok(s) => s,
        Err(e) => return operation::fail_op(&app, &op_id, format!("capturing run-spec: {e:#}")),
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
        return operation::fail_op(&app, &op_id, format!("writing handoff: {e:#}"));
    }
    let socket = wire::DOCKER_SOCKET.to_string();
    if let Err(e) = app
        .docker
        .launch_upgrade_helper(&reference, &self_id, &socket)
        .await
    {
        crate::update::clear_handoff();
        return operation::fail_op(&app, &op_id, format!("launching updater: {e:#}"));
    }
    // The helper now stops us; this task dies with the container. Leave the op Running at 85%
    // — the rebooted server's reconcile_pending finalizes it.
    patch_op(&app, &op_id, |op| {
        op.pct = op.pct.max(85.0);
        op.message = "updater launched — the server will restart on the new image".into();
    });
}

// --- delete -------------------------------------------------------------------------------

/// Validate + register a delete op, then drive it in the background. A managed clone is
/// torn down through `provision::delete_clone` (container name == clone id); an unmanaged
/// row (a legacy/plain clone) is simply removed from state.
pub fn start_delete(app: &App, host_id: &str) -> Result<Operation, JobError> {
    let spec = OpSpec::new(OperationKind::Delete, host_id)
        .steps(provision::delete_pct)
        .guards(Guards {
            // Two deliberate loosenings of the shared set, both stated rather than left as
            // the accident they used to be (this was the one flow whose guard block never
            // mentioned `archived` at all, and nothing said whether that was on purpose).
            //
            // `managed: false` — an unmanaged row is a legacy/plain clone with no container
            // behind it. Deleting it IS just unregistering it, and refusing would leave the
            // operator no way to get the row out of the list.
            //
            // `archived: None` — an archived clone stays deletable, on purpose. Archive means
            // "stopped, data kept"; delete is the only other way out of it, and
            // `provision::delete_clone` unpauses before it stops precisely so that a frozen
            // archived clone can still be destroyed. Demanding `!archived` here would strand
            // every archived clone in the list forever.
            managed: false,
            archived: None,
            ..Guards::on_clone()
        });
    let host_id = host_id.to_string();
    operation::run_op(app, spec, move |app, op| run_delete(app, op, host_id))
}

async fn run_delete(app: App, op: OpHandle, host_id: String) -> anyhow::Result<Finish> {
    // Last chance at this clone's transcripts. The `hosts/<id>` symlink disappears the moment the
    // container stops, so whatever has not been tailed by then is gone for good, and a worker
    // clone deletes itself seconds after the last correction is typed. Best effort: it is bounded
    // by its own timeout and it cannot fail the delete.
    crate::ledger::tail_once(&app, &host_id).await;
    // Read off the row rather than carrying a flag in from the guard pass: the guard has
    // already proved the row is there, and one reader is one definition.
    let managed = app
        .store
        .get()
        .hosts
        .iter()
        .any(|h| h.id == host_id && h.managed);
    if managed {
        delete_clone(&app, &host_id, op.progress()).await?;
    } else {
        // Unmanaged row: nothing to tear down, just unregister it.
        op.step("remove", "unregistering clone (no container)");
    }

    // Forget the clone's server-side secrets + push bookkeeping so nothing outlives it: a
    // revoked identity key can never identify as this clone again, and a same-named clone
    // created later starts from a clean slate rather than inheriting stale state.
    app.clone_keys.forget(&host_id);
    app.claude.forget_pushed(&host_id);
    app.codex.forget_pushed(&host_id);

    let previously_selected = app.store.selected();
    let message = if managed {
        format!("clone {host_id} destroyed")
    } else {
        "clone removed".to_string()
    };
    let gone = host_id.clone();
    Ok(Finish::new(message)
        .state(move |s| {
            s.hosts.retain(|h| h.id != gone);
            if s.selected.as_deref() == Some(gone.as_str()) {
                s.selected = s.hosts.first().map(|h| h.id.clone());
            }
        })
        .after(move |app, st| async move {
            // Deleting the watched clone moves the operator onto another one, which has been
            // holding whatever layout it was last viewed with. Bring it to the active preset,
            // exactly as a deliberate switch would.
            if st.selected != previously_selected {
                if let Some(id) = st.selected.as_deref() {
                    crate::mediaplane::apply_active_layout(&app, id);
                }
            }
            let dd = app.data_dir();
            crate::files::delete_notes(&dd, &host_id);
            crate::chat::delete_chat(&dd, &host_id);
            // Anything the operator queued for a clone that no longer exists can never be
            // delivered.
            crate::chat::delete_schedules(&dd, &host_id);
        }))
}

// --- shared gen-2 inputs --------------------------------------------------------------------

/// Everything a gen-2 fork/rebase/migrate needs from the source row's preset: the clone's
/// full session env — control URL, per-clone identity key, and the preset's own vars. It is
/// used whole: the create path writes all of it to `/etc/environment`, which is the one
/// carrier an SSH login, the desktop session and the agent all read.
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

// --- rebase ---------------------------------------------------------------------------------

/// Rebase a gen-2 clone onto a preset's image, keeping its dataset and id. The image
/// resolves + builds inside the background run, so build progress streams on the op;
/// `rebuild` forces a fresh build even when the tag exists.
///
/// `wire::OperationKind` has no `Rebase` variant, so this files as `Clone` — but it carries
/// its own queued message and its own step table (`provision::rebase_pct`, which unlike the
/// create table contains rebase's first step, `stop`). That is the whole point of the table
/// travelling with the spec: a borrowed kind can no longer borrow the wrong table.
pub fn start_rebase(
    app: &App,
    host_id: &str,
    preset_name: &str,
    rebuild: bool,
) -> Result<Operation, JobError> {
    let preset_name = preset_name.trim().to_string();
    let spec = OpSpec::new(OperationKind::Clone, host_id)
        .source(preset_name.clone())
        .queued(format!("queued rebase of {host_id} onto {preset_name}"))
        .steps(provision::rebase_pct)
        // The shared set plus gen-2: a rebase swaps the image under an existing dataset, so
        // there has to be one. `archived` is deliberately unconstrained — an archived clone
        // can be rebased, and is put back to rest by the body.
        .guards(Guards::on_clone().gen2(true));
    // The shared guard pass runs FIRST, as it did when every flow open-coded it: the operator
    // hears about the clone before they hear about the preset. The two preset checks below
    // are this flow's own — nothing about them generalises — so they sit here rather than in
    // `Guards`, and the spec is driven unguarded because it has just been guarded.
    operation::check_guards(app, &spec)?;
    if preset_name.is_empty() {
        return Err(JobError("a preset is required".into()));
    }
    if !app.config().presets.iter().any(|p| p.name == preset_name) {
        return Err(JobError(format!("unknown preset '{preset_name}'")));
    }
    let host_id = host_id.to_string();
    Ok(operation::run_op_unguarded(app, spec, move |app, op| {
        run_rebase(app, op, host_id, preset_name, rebuild)
    }))
}

async fn run_rebase(
    app: App,
    op: OpHandle,
    host_id: String,
    preset_name: String,
    rebuild: bool,
) -> anyhow::Result<Finish> {
    let mut progress = op.progress();
    let row = app
        .store
        .get()
        .hosts
        .into_iter()
        .find(|h| h.id == host_id)
        .ok_or_else(|| anyhow::anyhow!("unknown clone '{host_id}'"))?;
    // Image follows the TARGET preset (built lazily here, so build progress streams on
    // this op); env/playbook stay on the clone's own bindings — rebase swaps the image
    // only, never the preset.
    let dockerfile = crate::provision::preset_dockerfile(&app, Some(&preset_name));
    let new_tag = crate::derived::ensure_image(&app, &dockerfile, rebuild, &mut progress).await?;
    let env = gen2_create_env(&app, row.preset_name.as_deref(), &host_id).await?;
    let (playbook, prompt) = gen2_playbook_prompt(&app, row.preset_name.as_deref());
    // An archived clone rests stopped, but the swap below boots a container. Remember the
    // rest state and put it back down afterwards.
    let was_archived = row.archived;
    let swapped = rebase_clone(
        &app,
        &host_id,
        &new_tag,
        &env,
        &playbook,
        &prompt,
        row.headless,
        progress,
    )
    .await;
    if was_archived {
        // Owed on BOTH arms: the swap boots a container on success, and the rollback
        // recreates (and boots) one on failure. If the stop fails, the archived-state
        // reconciler stops it instead.
        if let Err(e) = app.docker.stop_even_if_paused(&host_id).await {
            tracing::warn!(
                target: "clone",
                "rebase of archived clone '{host_id}': rest stop failed: {e:#}"
            );
        }
    }
    let tag = swapped?;
    let message = if was_archived {
        format!("clone {host_id} rebased onto {tag} (stays archived)")
    } else {
        format!("clone {host_id} rebased onto {tag}")
    };
    let tag_for_row = tag;
    Ok(Finish::new(message)
        .row(move |h| {
            h.base_tag = Some(tag_for_row.clone());
            h.source = Some(tag_for_row);
        })
        .after(move |app, _st| async move {
            // Rebases end stopped (archived) or running: the waiter covers both.
            crate::clone_reconcile::spawn_converge_after_start(&app, &host_id, "rebase");
        }))
}

// --- migrate ---------------------------------------------------------------------------------

/// Migrate one gen-1 clone (managed row without a dataset). Files a `Migrate` op and
/// drives it; the clone stays STOPPED — the boot loop starts the fleet after the window.
pub fn start_migrate(app: &App, host_id: &str) -> Result<Operation, JobError> {
    let spec = OpSpec::new(OperationKind::Migrate, host_id)
        .steps(provision::migrate_pct)
        // The shared set plus gen-1: there is nothing to migrate once the row has a dataset.
        .guards(Guards::on_clone().gen2(false));
    let host_id = host_id.to_string();
    operation::run_op(app, spec, move |app, op| run_migrate(app, op, host_id))
}

async fn run_migrate(app: App, op: OpHandle, host_id: String) -> anyhow::Result<Finish> {
    let progress = op.progress();
    let row = app
        .store
        .get()
        .hosts
        .into_iter()
        .find(|h| h.id == host_id)
        .ok_or_else(|| anyhow::anyhow!("unknown clone '{host_id}'"))?;
    let base = row
        .source
        .clone()
        .ok_or_else(|| anyhow::anyhow!("clone '{host_id}' has no source image"))?;
    // The home copy needs a stable source: stop it first (best-effort — it may already
    // be stopped; the boot loop stops the whole fleet beforehand anyway).
    //
    // The step key is `pre-stop`, not `stop`: `migrate_one` ends on `stop` (90%), and sharing
    // the key put the bar at 90% before a single byte had been copied.
    op.step("pre-stop", &format!("stopping {host_id} for migration"));
    if let Err(e) = app.docker.stop_even_if_paused(&host_id).await {
        tracing::warn!("migrate {host_id}: pre-stop failed: {e} (continuing)");
    }
    let env = gen2_create_env(&app, row.preset_name.as_deref(), &host_id).await?;
    // Playbook/prompt injects are skipped: the copied home already carries the files the
    // gen-1 create wrote; re-injecting would only rewrite identical content.
    let report = migrate_one(&app, &host_id, &base, &env, "", "", row.headless, progress).await?;
    // Presence is the gen-2 marker; the value comes off the clone's own home.
    let dataset = crate::clone_home::CloneHome::of(&app, &host_id).dataset();
    let tag = report.tag;
    Ok(
        Finish::new(format!("clone {host_id} migrated ({} bytes)", report.bytes))
            .row(move |h| {
                h.dataset = Some(dataset);
                h.base_tag = Some(tag.clone());
                h.source = Some(tag);
            })
            .after(move |app, _st| async move {
                // The fleet restarts after the window: the waiter catches this clone's boot.
                crate::clone_reconcile::spawn_converge_after_start(&app, &host_id, "migrate");
            }),
    )
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

/// How many clones migrate at once in [`migrate_all_on_boot`].
///
/// Was one. The per-clone cost is a home copy — measured at ~7 minutes for a 12 GB home
/// on CT 104 — so a serial pass over a real fleet runs for hours (CT 106: 104 clones,
/// homes to 44.8 GB). Concurrency is only safe because the home archive streams rather
/// than buffering (`DockerCtl::download_tar_stream`); buffering four homes at once would
/// have cost their combined size in RSS.
const MIGRATE_CONCURRENCY: usize = 4;

/// Migrate `ids`, up to [`MIGRATE_CONCURRENCY`] at a time. Returns (passed, failed ids).
///
/// Success is read off the ROW, not the operation: a finished op is pruned 8 s later
/// ([`PRUNE_DONE_MS`]), which a 5 s poll can easily miss — and the row carrying a dataset
/// at all ([`crate::clone_home::is_gen2`]) is the authoritative record that the clone is
/// gen-2 now.
async fn migrate_pass(app: &App, ids: Vec<String>) -> (usize, Vec<String>) {
    use futures::StreamExt;
    let results = futures::stream::iter(ids.into_iter().map(|id| {
        let app = app.clone();
        async move {
            match start_migrate(&app, &id) {
                Ok(op) => {
                    wait_op_terminal(&app, &op.id).await;
                    let migrated = app
                        .store
                        .get()
                        .hosts
                        .iter()
                        .any(|h| h.id == id && crate::clone_home::is_gen2(h));
                    if migrated { Ok(()) } else { Err(id) }
                }
                Err(e) => {
                    tracing::warn!("migrate {id}: could not file op: {e}");
                    Err(id)
                }
            }
        }
    }))
    .buffer_unordered(MIGRATE_CONCURRENCY)
    .collect::<Vec<_>>()
    .await;
    let mut pass = 0;
    let mut failed = Vec::new();
    for r in results {
        match r {
            Ok(()) => pass += 1,
            Err(id) => failed.push(id),
        }
    }
    (pass, failed)
}

/// Boot one-shot: migrate every gen-1 row (managed, no dataset — see
/// [`crate::clone_home::is_gen2`]) to gen-2,
/// [`MIGRATE_CONCURRENCY`] at a time. No gen-1 rows ⇒ no-op. Runs under the whole-LXC
/// backup: per-clone failures log and continue with one retry at the end; the fleet
/// (non-archived) starts after the window, with stored account tokens re-pushed onto the
/// running clones.
///
/// Returns whether there was anything to migrate — a `false` means boot still owes the
/// fleet a start ([`boot_start_fleet`]).
pub async fn migrate_all_on_boot(app: App) -> bool {
    let gen1: Vec<String> = app
        .store
        .get()
        .hosts
        .into_iter()
        .filter(|h| h.managed && !crate::clone_home::is_gen2(h))
        .map(|h| h.id)
        .collect();
    if gen1.is_empty() {
        return false;
    }
    tracing::warn!(
        "gen-2 migration: {} gen-1 clone(s) detected, {MIGRATE_CONCURRENCY} at a time: {}",
        gen1.len(),
        gen1.join(", ")
    );
    // Same normalisation the plain boot path does: a clone the daemon can restart will
    // fight the window's stops.
    normalize_restart_policies(&app).await;
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
    let (mut pass, failed) = migrate_pass(&app, gen1.clone()).await;
    // One retry pass for the failures.
    let (retry_pass, retry_failed) = migrate_pass(&app, failed).await;
    pass += retry_pass;
    // Start the fleet: every migrated non-archived clone, then re-push its stored
    // account tokens (those need running clones). Best-effort per clone.
    let mut started = 0;
    for h in app
        .store
        .get()
        .hosts
        .into_iter()
        .filter(|h| h.managed && !h.archived && crate::clone_home::is_gen2(h))
    {
        if let Err(e) = app.docker.start_container(&h.id).await {
            tracing::warn!("migrate: starting {} failed: {e} (continuing)", h.id);
            continue;
        }
        started += 1;
        crate::pool::push_both_sides(&app, &h.id, "migrate").await;
    }
    tracing::warn!(
        "gen-2 migration: {pass} passed, {} failed ({}), {started} started",
        retry_failed.len(),
        retry_failed.join(", "),
    );
    true
}

// --- boot + crash recovery --------------------------------------------------------------------

/// Start the fleet at boot: every managed, non-archived clone that is not already running.
///
/// This exists because clone containers carry `restart: no` (see
/// `docker.rs::create_clone_container`). The daemon used to do this, but it did it the
/// instant it started — before the server, and therefore before the home overlays were
/// mounted — which handed clones an empty home. Starting them HERE, after
/// `home_overlay::remount_all`, makes that race impossible rather than recoverable.
pub async fn boot_start_fleet(app: &App) {
    normalize_restart_policies(app).await;
    let want: Vec<String> = app
        .store
        .get()
        .hosts
        .into_iter()
        .filter(|h| h.managed && !h.archived)
        .map(|h| h.id)
        .collect();
    let (mut started, mut already) = (0usize, 0usize);
    for id in want {
        match app.docker.is_running(&id).await {
            Ok(true) => already += 1,
            Ok(false) => match app.docker.start_container(&id).await {
                Ok(()) => started += 1,
                Err(e) => tracing::warn!("boot: starting {id} failed: {e:#}"),
            },
            Err(e) => tracing::warn!("boot: liveness of {id}: {e:#}"),
        }
    }
    tracing::info!("boot: fleet start — {started} started, {already} already running");
}

/// Take every managed clone off the daemon's restart policy, archived ones included.
///
/// New clones are created with `restart: no`; this is for the ones that are not new. Until
/// a clone is normalised the daemon still starts it at CT boot, ahead of the server, which
/// is the race the policy change exists to remove. One boot fixes an existing fleet.
pub(crate) async fn normalize_restart_policies(app: &App) {
    let ids: Vec<String> = app
        .store
        .get()
        .hosts
        .into_iter()
        .filter(|h| h.managed)
        .map(|h| h.id)
        .collect();
    let mut changed = 0usize;
    for id in ids {
        match app.docker.ensure_no_restart_policy(&id).await {
            Ok(true) => changed += 1,
            Ok(false) => {}
            Err(e) => tracing::warn!("boot: restart policy of {id}: {e:#}"),
        }
    }
    if changed > 0 {
        tracing::info!("boot: took {changed} clone(s) off the daemon restart policy");
    }
}

/// How often [`crash_recovery`] looks for a clone that fell over.
const CRASH_SWEEP: std::time::Duration = std::time::Duration::from_secs(30);

/// Restart a managed clone that stopped on its own.
///
/// `restart: no` (see `docker.rs::create_clone_container`) took this away from the daemon,
/// so the server takes it back — and does it better: the daemon would happily revive an
/// ARCHIVED clone's container, while this knows the difference.
///
/// Two guards against fighting a deliberate stop. A clone with a Running operation is left
/// alone (archive, rebase and migrate all file their op BEFORE they stop the container),
/// and a clone must be seen stopped on two consecutive sweeps before it is touched, so a
/// container caught mid-restart is not raced.
pub async fn crash_recovery(app: App) {
    let mut down_last_sweep: std::collections::HashSet<String> = Default::default();
    loop {
        tokio::time::sleep(CRASH_SWEEP).await;
        let st = app.store.get();
        let busy: std::collections::HashSet<&str> = st
            .operations
            .iter()
            .filter(|o| o.status == OperationStatus::Running)
            .map(|o| o.target.as_str())
            .collect();
        let mut down_now = std::collections::HashSet::new();
        for h in st.hosts.iter().filter(|h| h.managed && !h.archived) {
            if busy.contains(h.id.as_str()) {
                continue;
            }
            if !matches!(app.docker.is_running(&h.id).await, Ok(false)) {
                continue;
            }
            if down_last_sweep.contains(&h.id) {
                match app.docker.start_container(&h.id).await {
                    Ok(()) => tracing::warn!("crash recovery: restarted {}", h.id),
                    Err(e) => tracing::warn!("crash recovery: restarting {}: {e:#}", h.id),
                }
            } else {
                down_now.insert(h.id.clone());
            }
        }
        down_last_sweep = down_now;
    }
}

// --- prebuild ------------------------------------------------------------------------------

/// Warm a preset image without creating (`POST /api/images/prebuild`): always rebuild the
/// posted Dockerfile text with a fresh base pull, even when its tag exists. The preset
/// card's rebuild button posts the editor's current text (which may be unsaved); saving
/// is separate.
pub fn start_prebuild(app: &App, dockerfile: String) -> Result<Operation, JobError> {
    if dockerfile.trim().is_empty() {
        return Err(JobError("a Dockerfile is required to prebuild".into()));
    }
    let target = wire::config::dockerfile_tag(&dockerfile);
    let spec = OpSpec::new(OperationKind::Prebuild, target)
        // No coarse table: the build streams its own step lines as messages, so the bar holds
        // until the runner finishes the op. `no_pct` says that out loud.
        .steps(provision::no_pct)
        .guards(Guards {
            // The target is an image TAG, not a clone row, so the row half of the shared set
            // has nothing to look at. `idle_kind` is the addition: two builds at once fight
            // over the same daemon for no gain, whichever Dockerfiles they are building.
            idle_target: true,
            idle_kind: true,
            ..Guards::none()
        });
    operation::run_op(app, spec, move |app, op| run_prebuild(app, op, dockerfile))
}

async fn run_prebuild(app: App, op: OpHandle, dockerfile: String) -> anyhow::Result<Finish> {
    // The runner's progress sink, which carries the shared op-log cap. This flow used to
    // re-implement that cap inline — a third copy of the same `drain(0..)` to keep in step.
    let tag = crate::derived::ensure_image(&app, &dockerfile, true, op.progress()).await?;
    // Warm the overlay lower too: the first fork on a new tag otherwise pays a full
    // `/home/rmng` export + unpack on its own critical path. Failing here fails the
    // op — the fork would hit the same error later with less context.
    let mut progress = op.progress();
    progress("warm", &format!("exporting home skeleton for {tag}"));
    crate::home_overlay::ensure_skeleton(&app, &tag).await?;
    Ok(Finish::new(format!("derived image {tag} ready")))
}

// --- archive / unarchive -----------------------------------------------------------------------

/// Stop a managed clone without removing its container, volumes, or per-clone files.
pub fn start_archive(app: &App, host_id: &str) -> Result<Operation, JobError> {
    let spec = OpSpec::new(OperationKind::Archive, host_id)
        .steps(provision::archive_pct)
        .guards(Guards::on_clone().archived(false));
    let host_id = host_id.to_string();
    operation::run_op(app, spec, move |app, op| run_archive(app, op, host_id))
}

async fn run_archive(app: App, op: OpHandle, host_id: String) -> anyhow::Result<Finish> {
    // Same last chance as a delete. An archived clone refuses exec and `homes` drops its
    // symlink, so its transcripts are unreachable from the moment it stops even though the
    // files themselves survive.
    crate::ledger::tail_once(&app, &host_id).await;
    // Shut the clone down. Its memory goes back to the host, and restoring is a boot: systemd,
    // the desktop session, the inner Docker daemon and the agent all start again.
    //
    // `stop_even_if_paused` rather than a plain stop, because a clone archived by the build
    // that froze them instead is still paused, and a stop signal sent to frozen processes is
    // one nobody can handle: the daemon waits out the full timeout and then kills.
    op.step("stop", "stopping the clone (SIGRTMIN+3, up to 20s)");
    app.docker.stop_even_if_paused(&host_id).await?;

    let previously_selected = app.store.selected();
    let gone = host_id.clone();
    Ok(Finish::new(format!("clone {host_id} archived"))
        .row(|h| {
            h.archived = true;
            h.monitor_state = None;
            h.activity_unknown = false;
            h.local_ip = None;
            h.unread = false;
        })
        // Runs AFTER the row edit above, which this depends on: the replacement selection is
        // picked with `!h.archived`, and the clone being archived must already read as
        // archived so it cannot pick itself.
        .state(move |s| {
            // Archiving the clone the operator is watching has to move them off it. A selection
            // left pointing at a stopped clone aims the viewer at something that will never send
            // another frame, and leaves a still one on screen that looks live. `activate` refuses
            // to select an archived clone, so nothing else would ever clear this.
            if s.selected.as_deref() == Some(gone.as_str()) {
                s.selected = s
                    .hosts
                    .iter()
                    .find(|h| !h.archived && h.managed)
                    .map(|h| h.id.clone());
            }
        })
        .after(move |app, st| async move {
            // The clone the operator lands on has been holding its own layout since it was last
            // viewed. Bring it to the active preset, as a deliberate switch does.
            if st.selected != previously_selected {
                if let Some(id) = st.selected.as_deref() {
                    crate::mediaplane::apply_active_layout(&app, id);
                }
            }
        }))
}

/// Start an archived managed clone without recreating it.
pub fn start_unarchive(app: &App, host_id: &str) -> Result<Operation, JobError> {
    let spec = OpSpec::new(OperationKind::Unarchive, host_id)
        .steps(provision::unarchive_pct)
        .guards(Guards::on_clone().archived(true));
    let host_id = host_id.to_string();
    operation::run_op(app, spec, move |app, op| run_unarchive(app, op, host_id))
}

async fn run_unarchive(app: App, op: OpHandle, host_id: String) -> anyhow::Result<Finish> {
    // Start a stopped clone, and thaw a paused one first. Archiving stops the container, but a
    // clone archived by the build that froze them instead is still paused, and an upgrade must
    // not strand it.
    op.step("start", "restoring the archived clone");
    app.docker.resume_container(&host_id).await?;

    Ok(Finish::new(format!("clone {host_id} restored"))
        .row(|h| {
            h.archived = false;
            h.monitor_state = None;
            h.activity_unknown = false;
            h.local_ip = None;
            h.unread = false;
        })
        .after(move |app, _st| async move {
            // A restart rebuilds the container's mount table and gives it a new pid, so the
            // home link is gone even though the clone itself is intact. Re-apply it here for
            // the same reason the create path does: an unarchived clone is presented as ready.
            // (The shared pool and /dev/shm need no re-apply: both are create-time config now.)
            crate::homes::ensure_now(&app, &host_id).await;
            crate::ssh::allow_clone_now(&app, &host_id).await;
            crate::pool::push_both_sides(&app, &host_id, "unarchive").await;
            // Fresh /etc on a carried-over home: converge content now that it boots.
            crate::clone_reconcile::spawn_converge_after_start(&app, &host_id, "unarchive");
        }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[test]
    fn startup_script_stamp_is_stable_and_text_sensitive() {
        assert_eq!(
            startup_script_stamp("echo hi"),
            startup_script_stamp("echo hi")
        );
        assert_ne!(
            startup_script_stamp("echo hi"),
            startup_script_stamp("echo bye")
        );
        assert_ne!(
            startup_script_stamp("echo hi"),
            startup_script_stamp("echo hi\n")
        );
    }

    /// A minimal App backed by a throwaway temp data dir (ClaudeStore/state don't touch the
    /// repo). Docker is constructed I/O-free — `fail_stale_ops` and the guard pass never
    /// touch it.
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

    /// The self-update swap kills the server, aborting every in-flight clone/delete/rebase, so
    /// `start_update` files under the runner's `idle_fleet` guard and is refused while ANY op
    /// is Running.
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

    /// The archive/unarchive flows against the runner: a filed op is registered, and the
    /// `archived` precondition is reported before "in flight".
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

    /// A delete is deliberately allowed on an ARCHIVED clone and on an UNMANAGED row — the
    /// two loosenings of the shared guard set, which used to be one unexplained omission.
    #[tokio::test]
    async fn delete_accepts_archived_and_unmanaged_rows() {
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
        assert_eq!(
            start_delete(&app, "plain").unwrap().kind,
            OperationKind::Delete
        );
        assert_eq!(
            start_delete(&app, "stored").unwrap().kind,
            OperationKind::Delete
        );
        assert!(
            start_delete(&app, "nope")
                .unwrap_err()
                .0
                .contains("unknown clone")
        );
        // …but still one at a time per clone.
        assert!(
            start_delete(&app, "stored")
                .unwrap_err()
                .0
                .contains("in flight")
        );
    }

    /// A rebase borrows `OperationKind::Clone` (wire has no `Rebase` variant) and therefore
    /// must carry its own step table: the create table has no `stop`, which is the first
    /// thing a rebase does. The guard pass runs before the flow's own preset checks.
    #[tokio::test]
    async fn rebase_guards_run_before_the_preset_checks() {
        let app = test_app();
        app.store.mutate(|s| {
            s.hosts.push(RmngClone {
                id: "gen2".into(),
                host: "gen2".into(),
                managed: true,
                dataset: Some("pool/homes/gen2".into()),
                base_tag: Some("rmng-derived:abc".into()),
                ..Default::default()
            });
            s.hosts.push(RmngClone {
                id: "gen1".into(),
                host: "gen1".into(),
                managed: true,
                ..Default::default()
            });
        });

        // The shared guard set answers first, whatever the preset argument says.
        assert!(
            start_rebase(&app, "gen1", "dev", false)
                .unwrap_err()
                .0
                .contains("not a gen-2")
        );
        assert!(
            start_rebase(&app, "nope", "dev", false)
                .unwrap_err()
                .0
                .contains("unknown clone")
        );
        // Then the flow's own checks.
        assert!(
            start_rebase(&app, "gen2", "  ", false)
                .unwrap_err()
                .0
                .contains("a preset is required")
        );
        assert!(
            start_rebase(&app, "gen2", "dev", false)
                .unwrap_err()
                .0
                .contains("unknown preset")
        );
        // The table a rebase is scored against contains its first step; the create table,
        // which it used to be scored against, does not.
        assert_eq!(provision::rebase_pct("stop"), Some(15.0));
        assert_eq!(provision::clone_pct("stop"), None);
    }
}
