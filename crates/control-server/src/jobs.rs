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
use crate::provision::{
    self, HomeSource, clone_container_gen2, clone_key_env_vars, compose_clone_env,
    control_env_vars, delete_clone, fork_clone, is_dns_label, migrate_one, preset_env_vars,
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

/// Linear ticket metadata stamped onto a cloned `RmngClone`.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
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
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CloneSpec {
    /// Retired: the image used to come from the caller's picked base. The effective
    /// preset's Dockerfile decides now; kept for payload compat and ignored.
    pub source_image: String,
    pub new_hostname: String,
    pub linear: Option<LinearMeta>,
    /// Requested Claude account: an email, `"auto"`, `"none"`, `"group:<name>"`, or `None`
    /// (= auto).
    pub claude_account: Option<String>,
    /// Requested Codex account, same forms. Independent of `claude_account` — a clone can
    /// hold both.
    pub codex_account: Option<String>,
    pub first_message: Option<String>,
    pub agent_instructions: Option<String>,
    pub claude_instructions: Option<String>,
    /// Clone preset name used to derive env/playbook, persisted for future reconciliation.
    pub preset_name: Option<String>,
    /// Resolved env-preset vars to write into the clone's `/etc/environment` at creation.
    pub env: Vec<wire::EnvVar>,
    /// Composed agent playbook (global + preset append) injected into the clone at creation
    /// as ~/.config/rmng/agent-instructions.md. Empty ⇒ no file injected. (Layers b + d.)
    pub agent_playbook: String,
    /// Composed global agent prompt (global + preset append) written to every agent's native
    /// rules file (CLAUDE.md / AGENTS.md) at creation. (Layers a + c.)
    pub global_prompt: String,
    /// Create a **headless clone**: same template, but the desktop (`gnome-headless`) and
    /// capture daemon (`rmng-clone-daemon`) user units are disabled at provision and a default
    /// tmux session is started. Persisted on `RmngClone.headless`; drives the viewer tmux view.
    pub headless: bool,
    /// Parent clone id when this clone should be created as a sub clone (one level deep). Already
    /// validated by the caller (`web::clone`): the parent exists, is managed, and is itself
    /// top-level. `None` = top-level clone. Persisted on `RmngClone.parent`; purely cosmetic.
    pub parent: Option<String>,
    /// Run the preset's startup script as the clone user as the last settle step.
    /// Every caller defaults this on; opt out per request, never per preset.
    pub run_startup_script: bool,
}

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

/// Pick a free clone id for a ticket base name (`base`, then `base a..z`). Race-free
/// when called immediately before `start_clone` (single state snapshot).
///
/// A name is taken if a clone holds it, if a clone is being created under it, or if a clone that
/// no longer exists left a transcript ledger under it. That last one is why cloning the same
/// ticket twice keeps walking the alphabet after the first clone is deleted: the ledger is filed
/// by clone name and outlives the clone (see [`crate::ledger`]).
pub fn next_free_hostname(app: &App, base: &str) -> String {
    let st = app.store.get();
    let mut taken: std::collections::HashSet<String> =
        st.hosts.iter().map(|h| h.id.clone()).collect();
    for o in &st.operations {
        if o.status == OperationStatus::Running {
            taken.insert(o.target.clone());
        }
    }
    taken.extend(crate::ledger::reserved_names(&app.data_dir()));
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
                op.log.push(
                    "startup script: timed out after 5m, left running in the clone".into(),
                )
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

pub fn start_clone(app: &App, spec: CloneSpec) -> Result<Operation, JobError> {
    // The preset Dockerfile decides the image; no caller-supplied base is needed.
    let _ = spec.source_image.as_str();
    if !is_dns_label(&spec.new_hostname) {
        return Err(JobError(
            "new hostname must be a DNS label (lowercase letters, digits, hyphens)".into(),
        ));
    }
    let st = app.store.get();
    if st.hosts.iter().any(|h| h.id == spec.new_hostname) {
        return Err(JobError(format!(
            "a clone named '{}' already exists",
            spec.new_hostname
        )));
    }
    if st
        .operations
        .iter()
        .any(|o| o.status == OperationStatus::Running && o.target == spec.new_hostname)
    {
        return Err(JobError(format!(
            "'{}' is already being created",
            spec.new_hostname
        )));
    }
    // A retired clone keeps its transcript ledger, and the ledger is filed by clone name. Handing
    // the name to a new clone would file two unrelated histories in one bucket, so the name stays
    // spent until the operator says otherwise. `next_free_hostname` skips these, so only an
    // exact-hostname create (the fleet CLI's `clone create <hostname>`) reaches this rejection.
    let data_dir = app.data_dir();
    if crate::ledger::reserved_names(&data_dir).contains(&spec.new_hostname) {
        return Err(JobError(format!(
            "a retired clone was named '{name}'; its transcript ledger still holds that history. \
             Pick another name, or remove {}/{name} to release it.",
            crate::ledger::ledger_root(&data_dir).display(),
            name = spec.new_hostname
        )));
    }
    // Sub-clone invariant (defense in depth; `web::resolve_parent` already validated): the parent
    // must exist, be a managed clone, and be top-level — nesting is one level deep.
    if let Some(parent) = &spec.parent {
        match st.hosts.iter().find(|h| &h.id == parent) {
            None => return Err(JobError(format!("parent clone '{parent}' not found"))),
            Some(h) if !h.managed => {
                return Err(JobError(format!(
                    "parent clone '{parent}' is not a managed clone"
                )));
            }
            Some(h) if h.parent.is_some() => {
                return Err(JobError(format!(
                    "parent clone '{parent}' is itself a sub clone; sub clones are one level deep"
                )));
            }
            Some(_) => {}
        }
    }

    let op = make_op(
        OperationKind::Clone,
        &spec.new_hostname,
        Some(&spec.source_image),
    );
    let op_for_return = op.clone();
    let op_id = op.id.clone();
    app.store.mutate(|s| s.operations.push(op));

    let app2 = app.clone();
    tokio::spawn(async move { run_clone(app2, op_id, spec).await });
    Ok(op_for_return)
}

async fn run_clone(app: App, op_id: String, spec: CloneSpec) {
    let progress = op_progress(&app, &op_id, OperationKind::Clone);

    // The clone's full session env, composed in the same precedence order the per-clone resync
    // uses (`provision::compose_clone_env`): the control URL, the per-clone identity key
    // (RMNG_PROXY_KEY — minted server-side, never serialized onto `RmngClone`/state), the
    // operator's preset, then Claude Code's default model. An unresolvable control host
    // fails the op: booting the clone into a degraded URL helps nobody (see control_env_vars).
    let control = match control_env_vars(&app).await {
        Ok(vars) => vars,
        Err(e) => return fail_op(&app, &op_id, format!("{e:#}")),
    };
    let env = crate::provision::compose_clone_env(
        control,
        crate::provision::clone_key_env_vars(&app, &spec.new_hostname),
        &spec.env,
    );
    // Gen-2 create: image = hash-tag built lazily from the effective preset's FULL
    // Dockerfile text (verbatim, no FROM rewrite); home = fresh dataset, or a clone of
    // the template seed snapshot when the template carries default home content.
    // Returns the resolved tag, recorded below as the clone's `base_tag`. The backing
    // container's name is the clone id — that's how every later call addresses it.
    let cfg = app.config();
    let dockerfile = crate::provision::preset_dockerfile(&app, spec.preset_name.as_deref());
    let home = match cfg.docker.seed_snapshot.clone().unwrap_or_default() {
        s if !s.trim().is_empty() => HomeSource::CloneFromSnapshot(s),
        _ => HomeSource::Create,
    };
    let image_ref = match clone_container_gen2(
        &app,
        &dockerfile,
        &spec.new_hostname,
        home,
        &env,
        &spec.agent_playbook,
        &spec.global_prompt,
        spec.headless,
        progress,
    )
    .await
    {
        Ok(v) => v,
        Err(e) => return fail_op(&app, &op_id, format!("{e:#}")),
    };

    // The container is up and its daemon has registered (or timed out still-booting) — the op
    // now sits at the `ready` step (~80%). The clone is NOT connectable yet: the account tokens
    // still have to be pushed. The client treats a clone's PRESENCE in `s.hosts` as "ready to
    // connect", so we keep the clone OUT of state and the op RUNNING until this whole tail
    // settles — otherwise a viewer connecting at "100%" hits a not-yet-provisioned clone. The
    // clone is registered + the op marked `done` at the very end, below, once the clone is
    // genuinely streamable.
    //
    // A new clone boots on the template's baked `RMNG_MONITORS`, a single monitor nobody chose.
    // Bring it to the active layout preset as soon as its daemon registers, which is usually
    // already true here. Only new clones get this: every existing one keeps the layout it was
    // last viewed with until the operator switches to it. Headless clones run no session.
    if !spec.headless {
        crate::mediaplane::apply_active_layout_when_ready(app.clone(), spec.new_hostname.clone());
    }

    // (`progress` at the top of this fn was moved into `clone_container`; make a fresh one for
    // the remaining `accounts` step.)
    let mut progress = op_progress(&app, &op_id, OperationKind::Clone);

    progress("accounts", "assigning agent accounts");

    // Assign a Claude account/group (or explicitly none). The operator's selection + the
    // resolved account are COLLECTED into locals here and baked into the Host at the terminal
    // add below (there is no host in `s.hosts` yet); the token itself is installed into the
    // clone's ~/.claude/.credentials.json now (the server refreshes + re-pushes it thereafter).
    // A group-bound clone records its group; the rotator re-balances it. "none" installs no
    // token AND strips any credentials the image carried, so the clone boots provably tokenless.
    // One group for both sides: legacy `group:<name>` picks in either account field bind
    // the clone once (see `split_group_binding`); both `auto` sides then resolve inside it.
    let (bound_group, claude_req, codex_req) = crate::clone_ops::split_group_binding(
        spec.claude_account.clone(),
        spec.codex_account.clone(),
        None,
        None,
    );
    let mut claude_selection: Option<String> = None;
    let mut claude_account_email: Option<String> = None;
    let mut claude_group: Option<String> = None;
    if let Some(assignment) = crate::claude::resolve_assignment(
        &app,
        claude_req.as_deref(),
        None,
        bound_group.as_deref(),
    ) {
        let selection = crate::claude::normalize_selection(claude_req.as_deref());
        let (group, account, pending_auto) = match assignment {
            crate::claude::Assignment::Group { name, initial } => {
                (Some(name), Some(initial), false)
            }
            crate::claude::Assignment::Account(a) => (None, Some(a), false),
            crate::claude::Assignment::AutoPending => (None, None, true),
            crate::claude::Assignment::None => (None, None, false),
        };
        claude_selection = Some(selection);
        claude_account_email = account.clone();
        claude_group = group.clone();
        match account {
            None if pending_auto => {
                patch_op(&app, &op_id, |op| {
                    op.log
                        .push("account: auto (pending imported account)".into())
                });
            }
            None => {
                // Explicit "none": strip any credentials the image carried so the clone
                // boots tokenless, instead of trusting the template to be clean. Idempotent
                // (`rm -f`), so a clean image just reports "cleared". Best-effort like the
                // assign arm — a failure is logged, not fatal to the clone create.
                match crate::claude::clear_clone_token(&app, &spec.new_hostname).await {
                    Ok(()) => patch_op(&app, &op_id, |op| {
                        op.log.push("account: none (credentials cleared)".into())
                    }),
                    Err(e) => {
                        tracing::warn!("clear_clone_token({}) failed: {e}", spec.new_hostname);
                        patch_op(&app, &op_id, |op| {
                            op.log
                                .push(format!("account: none — failed to clear credentials: {e}"))
                        });
                    }
                }
                app.claude.forget_pushed(&spec.new_hostname);
            }
            Some(email) => {
                let label = match &group {
                    Some(g) => format!("{email} (group {g})"),
                    None => email.clone(),
                };
                match crate::claude::push_account_to_clone(&app, &spec.new_hostname, &email).await {
                    Ok(()) => patch_op(&app, &op_id, |op| {
                        op.log.push(format!("account: assigned {label}"))
                    }),
                    Err(e) => {
                        tracing::warn!("push_account_to_clone({}) failed: {e}", spec.new_hostname);
                        patch_op(&app, &op_id, |op| {
                            op.log
                                .push(format!("account: failed to assign {label}: {e}"))
                        });
                    }
                }
            }
        }
    }

    // Assign a Codex account/group (or explicitly none), independently of Claude — a clone
    // can hold both. Same shape as the Claude block above; collected into locals + baked into
    // the Host at the terminal add below.
    let mut codex_selection: Option<String> = None;
    let mut codex_account_email: Option<String> = None;
    let mut codex_group: Option<String> = None;
    if let Some(assignment) = crate::codex::resolve_assignment(
        &app,
        codex_req.as_deref(),
        None,
        bound_group.as_deref(),
    ) {
        let selection = crate::codex::normalize_selection(codex_req.as_deref());
        let (group, account, pending_auto) = match assignment {
            crate::codex::Assignment::Group { name, initial } => (Some(name), Some(initial), false),
            crate::codex::Assignment::Account(a) => (None, Some(a), false),
            crate::codex::Assignment::AutoPending => (None, None, true),
            crate::codex::Assignment::None => (None, None, false),
        };
        codex_selection = Some(selection);
        codex_account_email = account.clone();
        codex_group = group.clone();
        match account {
            None if pending_auto => {
                patch_op(&app, &op_id, |op| {
                    op.log
                        .push("codex account: auto (pending imported account)".into())
                });
            }
            None => {
                // Explicit "none": strip any codex auth the image carried (see the Claude block).
                match crate::codex::clear_clone_token(&app, &spec.new_hostname).await {
                    Ok(()) => patch_op(&app, &op_id, |op| {
                        op.log
                            .push("codex account: none (credentials cleared)".into())
                    }),
                    Err(e) => {
                        tracing::warn!(
                            "codex clear_clone_token({}) failed: {e}",
                            spec.new_hostname
                        );
                        patch_op(&app, &op_id, |op| {
                            op.log.push(format!(
                                "codex account: none — failed to clear credentials: {e}"
                            ))
                        });
                    }
                }
                app.codex.forget_pushed(&spec.new_hostname);
            }
            Some(email) => {
                let label = match &group {
                    Some(g) => format!("{email} (group {g})"),
                    None => email.clone(),
                };
                match crate::codex::push_account_to_clone(&app, &spec.new_hostname, &email).await {
                    Ok(()) => patch_op(&app, &op_id, |op| {
                        op.log.push(format!("codex account: assigned {label}"))
                    }),
                    Err(e) => {
                        tracing::warn!(
                            "codex push_account_to_clone({}) failed: {e}",
                            spec.new_hostname
                        );
                        patch_op(&app, &op_id, |op| {
                            op.log
                                .push(format!("codex account: failed to assign {label}: {e}"))
                        });
                    }
                }
            }
        }
    }

    // Everything a clone needs that lives OUTSIDE its container. Each of the three has a
    // reconcile loop that would apply it 10 to 15 s after the clone lands in `s.hosts`, and
    // those loops read `s.hosts`, so the wait would start only once the op says ready. That is
    // exactly when the operator opens the clone and finds the shared folder missing.
    //
    // The home symlink is the one with reach: SMB browsing, the file API, token accounting, the
    // transcript ledger and activity detection all read a clone through it.
    progress(
        "settle",
        "attaching the shared folder, home link and SSH access",
    );
    crate::homes::ensure_now(&app, &spec.new_hostname).await;
    // Before the store write below, so the bastion's forward allowlist and the clone's own
    // "ready" signal land together. It takes the id explicitly for that reason.
    crate::ssh::allow_clone_now(&app, &spec.new_hostname).await;

    // Last settle step: the preset's startup script as the clone user (best-effort).
    if spec.run_startup_script {
        progress("settle", "running the preset startup script");
        run_startup_script(&app, &op_id, &spec.new_hostname, spec.preset_name.as_deref()).await;
    } else {
        patch_op(&app, &op_id, |op| {
            op.log.push("startup script: skipped by request".into())
        });
    }

    // Register the fully-provisioned clone and mark the op done — the clone is now genuinely
    // connectable. A clone's PRESENCE in `s.hosts` is the client's "ready to connect" signal, so
    // it is added HERE, at the same instant the bar reaches 100%. `host` is display-only for
    // managed clones (dials go by container name == id); clones ship with fixed `rmng`/`rmng`
    // credentials baked into the base image. RDP port stays 3389 for the media path. The
    // group binding resolved above is baked in so the UI shows it the moment the clone appears.
    // `daemon_up` reflects whether the clone's daemon has registered (vs. still booting).
    let daemon_up = app.media.is_connected(&spec.new_hostname);
    app.store.mutate(|s| {
        let mut host = RmngClone {
            id: spec.new_hostname.clone(),
            host: spec.new_hostname.clone(),
            port: 3389,
            username: "rmng".into(),
            password: "rmng".into(),
            managed: true,
            source: Some(image_ref.clone()),
            dataset: Some(spec.new_hostname.clone()),
            base_tag: Some(image_ref.clone()),
            claude_selection: claude_selection.clone(),
            claude_account_email: claude_account_email.clone(),
            claude_group: claude_group.clone(),
            codex_selection: codex_selection.clone(),
            codex_account_email: codex_account_email.clone(),
            codex_group: codex_group.clone(),
            group: bound_group.clone(),
            preset_name: spec.preset_name.clone(),
            headless: spec.headless,
            parent: spec.parent.clone(),
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
        s.hosts.insert(0, host);
        if let Some(op) = s.operations.iter_mut().find(|o| o.id == op_id) {
            op.status = OperationStatus::Done;
            op.step = "done".into();
            op.pct = 100.0;
            op.message = if spec.headless {
                // Headless clones run no clone-daemon by design — never expect a media Hello.
                format!("headless clone {} ready", spec.new_hostname)
            } else if daemon_up {
                format!("clone {} ready", spec.new_hostname)
            } else {
                format!(
                    "clone {} created but its daemon hasn't registered yet (still booting; \
                     check it in the UI)",
                    spec.new_hostname
                )
            };
            op.finished_at = Some(now_ms());
        }
    });

    schedule_prune(app.clone(), op_id.clone(), PRUNE_DONE_MS);

    // Kick off the agent: hand it the ticket URL (ticket clones) or the plain
    // first message, plus any instruction overrides. Detached; it waits for the
    // wrapper to come up.
    let ticket_url = spec.linear.as_ref().and_then(|m| m.ticket_url.clone());
    let has_msg = spec
        .first_message
        .as_deref()
        .map(str::trim)
        .is_some_and(|s| !s.is_empty());
    if ticket_url.is_some() || has_msg {
        if let Some(host) = app
            .store
            .get()
            .hosts
            .into_iter()
            .find(|h| h.id == spec.new_hostname)
        {
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

/// Everything the API hands to `start_fork`. Every payload field is optional:
/// `None` inherits the source clone's binding (preset, ticket context, accounts).
/// Mirrors the matching `CloneSpec` fields without touching the create path.
#[derive(Debug, Clone, Default)]
pub struct ForkSpec {
    pub source_id: String,
    pub new_hostname: String,
    pub headless: bool,
    /// Same as [`CloneSpec::run_startup_script`]: run the preset's startup script as the
    /// clone user as the last settle step. Defaults on; opt out per request.
    pub run_startup_script: bool,
    pub preset_name: Option<String>,
    pub linear: Option<LinearMeta>,
    pub claude_account: Option<String>,
    pub codex_account: Option<String>,
    /// Clone-level pool binding: `Some(Some(name))` binds, `Some(None)` unbinds,
    /// `None` inherits the source's group. New writers use this; legacy `group:<name>`
    /// account picks still bind (see `split_group_binding`).
    pub group: Option<Option<String>>,
    pub first_message: Option<String>,
    pub agent_instructions: Option<String>,
    pub claude_instructions: Option<String>,
}

/// Fork a gen-2 clone: snapshot + clone the source home, create from its recorded base
/// tag. The fork inherits the source's preset, accounts, and ticket context; guard: no
/// Running op on either end, and the new hostname is valid + unused.
pub fn start_fork(app: &App, spec: ForkSpec) -> Result<Operation, JobError> {
    let st = app.store.get();
    let source_id = spec.source_id.as_str();
    let new_id = spec.new_hostname.as_str();
    let src = st.hosts.iter().find(|h| h.id == source_id).cloned();
    let Some(src) = src else {
        return Err(JobError(format!("unknown clone '{source_id}'")));
    };
    if !src.managed {
        return Err(JobError(format!("'{source_id}' is not a managed clone")));
    }
    if src.base_tag.is_none() {
        return Err(JobError(format!(
            "'{source_id}' is not a gen-2 clone (no base tag)"
        )));
    }
    if !is_dns_label(new_id) {
        return Err(JobError(
            "new hostname must be a DNS label (lowercase letters, digits, hyphens)".into(),
        ));
    }
    if st.hosts.iter().any(|h| h.id == new_id) {
        return Err(JobError(format!("a clone named '{new_id}' already exists")));
    }
    if st.operations.iter().any(|o| {
        o.status == OperationStatus::Running && (o.target == new_id || o.target == source_id)
    }) {
        return Err(JobError(format!(
            "'{source_id}' or '{new_id}' already has an operation in flight"
        )));
    }
    if let Some(name) = spec.preset_name.as_deref() {
        if !app.config().presets.iter().any(|p| p.name == name) {
            return Err(JobError(format!("unknown preset '{name}'")));
        }
    }
    let op = make_op(OperationKind::Clone, new_id, Some(source_id));
    let op_for_return = op.clone();
    let op_id = op.id.clone();
    app.store.mutate(|s| s.operations.push(op));
    let app2 = app.clone();
    tokio::spawn(async move { run_fork(app2, op_id, spec).await });
    Ok(op_for_return)
}

async fn run_fork(app: App, op_id: String, spec: ForkSpec) {
    let source_id = spec.source_id.clone();
    let new_id = spec.new_hostname.clone();
    let headless = spec.headless;
    let progress = op_progress(&app, &op_id, OperationKind::Clone);
    let src = match app
        .store
        .get()
        .hosts
        .into_iter()
        .find(|h| h.id == source_id)
    {
        Some(h) => h,
        None => return fail_op(&app, &op_id, format!("unknown clone '{source_id}'")),
    };
    // Payload wins, source fills the gaps: an explicit preset re-derives env/playbook,
    // otherwise the source's preset drives the fork (unchanged legacy behavior).
    let preset_name = spec.preset_name.clone().or(src.preset_name.clone());
    let env = match gen2_create_env(&app, preset_name.as_deref(), &new_id).await {
        Ok(env) => env,
        Err(e) => return fail_op(&app, &op_id, format!("{e:#}")),
    };
    let (playbook, prompt) = gen2_playbook_prompt(&app, preset_name.as_deref());
    let base_tag = match fork_clone(
        &app,
        &source_id,
        &new_id,
        &env,
        &playbook,
        &prompt,
        headless,
        preset_name.as_deref(),
        progress,
    )
    .await
    {
        Ok(t) => t,
        Err(e) => return fail_op(&app, &op_id, format!("{e:#}")),
    };

    let mut progress = op_progress(&app, &op_id, OperationKind::Clone);
    progress("accounts", "assigning agent accounts");
    // Explicit payload selections are resolved exactly like the create path (group /
    // account / auto-pending / none, token pushed or credentials cleared). Omitted
    // fields inherit the source's bindings with the tokens pushed fresh (short-lived —
    // never copied from the source's files). Best-effort per provider: logged, not fatal.
    // Fork inherits the source's group; an override naming another `group:<name>`
    // rebinds the fork (both `auto` sides resolve inside the new group below).
    let (bound_group, claude_req, codex_req) = crate::clone_ops::split_group_binding(
        spec.claude_account.clone().or(src.claude_selection.clone()),
        spec.codex_account.clone().or(src.codex_selection.clone()),
        src.group.clone(),
        spec.group.clone(),
    );
    let group_changed = bound_group != src.group;
    let mut claude_selection = src.claude_selection.clone();
    let mut claude_account_email = src.claude_account_email.clone();
    let mut claude_group = src.claude_group.clone();
    if spec.claude_account.is_some() || group_changed {
        let selection = crate::claude::normalize_selection(claude_req.as_deref());
        if let Some(assignment) = crate::claude::resolve_assignment(
            &app,
            claude_req.as_deref(),
            src.claude_account_email.as_deref(),
            bound_group.as_deref(),
        ) {
            let (group, account, pending_auto) = match assignment {
                crate::claude::Assignment::Group { name, initial } => {
                    (Some(name), Some(initial), false)
                }
                crate::claude::Assignment::Account(a) => (None, Some(a), false),
                crate::claude::Assignment::AutoPending => (None, None, true),
                crate::claude::Assignment::None => (None, None, false),
            };
            claude_selection = Some(selection);
            claude_account_email = account.clone();
            claude_group = group.clone();
            match account {
                None if pending_auto => patch_op(&app, &op_id, |op| {
                    op.log
                        .push("account: auto (pending imported account)".into())
                }),
                None => match crate::claude::clear_clone_token(&app, &new_id).await {
                    Ok(()) => {
                        app.claude.forget_pushed(&new_id);
                        patch_op(&app, &op_id, |op| {
                            op.log.push("account: none (credentials cleared)".into())
                        })
                    }
                    Err(e) => {
                        tracing::warn!("fork {new_id}: clear_clone_token failed: {e}");
                        patch_op(&app, &op_id, |op| {
                            op.log
                                .push(format!("account: none — failed to clear credentials: {e}"))
                        });
                    }
                },
                Some(email) => {
                    match crate::claude::push_account_to_clone(&app, &new_id, &email).await {
                        Ok(()) => patch_op(&app, &op_id, |op| {
                            op.log.push(format!("account: assigned {email}"))
                        }),
                        Err(e) => {
                            tracing::warn!("fork {new_id}: Claude assign failed: {e}");
                            patch_op(&app, &op_id, |op| {
                                op.log
                                    .push(format!("account: failed to assign {email}: {e}"))
                            });
                        }
                    }
                }
            }
        }
        // resolve_assignment returning None keeps the inherited locals above.
    } else if let Some(email) = src.claude_account_email.clone() {
        match crate::claude::push_account_to_clone(&app, &new_id, &email).await {
            Ok(()) => patch_op(&app, &op_id, |op| {
                op.log
                    .push(format!("account: inherited {email} from {source_id}"))
            }),
            Err(e) => {
                tracing::warn!("fork {new_id}: inheriting Claude account failed: {e}");
                patch_op(&app, &op_id, |op| {
                    op.log
                        .push(format!("account: failed to inherit {email}: {e}"))
                });
            }
        }
    }
    let mut codex_selection = src.codex_selection.clone();
    let mut codex_account_email = src.codex_account_email.clone();
    let mut codex_group = src.codex_group.clone();
    if spec.codex_account.is_some() || group_changed {
        let selection = crate::codex::normalize_selection(codex_req.as_deref());
        if let Some(assignment) = crate::codex::resolve_assignment(
            &app,
            codex_req.as_deref(),
            src.codex_account_email.as_deref(),
            bound_group.as_deref(),
        ) {
            let (group, account, pending_auto) = match assignment {
                crate::codex::Assignment::Group { name, initial } => {
                    (Some(name), Some(initial), false)
                }
                crate::codex::Assignment::Account(a) => (None, Some(a), false),
                crate::codex::Assignment::AutoPending => (None, None, true),
                crate::codex::Assignment::None => (None, None, false),
            };
            codex_selection = Some(selection);
            codex_account_email = account.clone();
            codex_group = group.clone();
            match account {
                None if pending_auto => patch_op(&app, &op_id, |op| {
                    op.log
                        .push("codex account: auto (pending imported account)".into())
                }),
                None => match crate::codex::clear_clone_token(&app, &new_id).await {
                    Ok(()) => {
                        app.codex.forget_pushed(&new_id);
                        patch_op(&app, &op_id, |op| {
                            op.log
                                .push("codex account: none (credentials cleared)".into())
                        })
                    }
                    Err(e) => {
                        tracing::warn!("fork {new_id}: codex clear failed: {e}");
                        patch_op(&app, &op_id, |op| {
                            op.log.push(format!(
                                "codex account: none — failed to clear credentials: {e}"
                            ))
                        });
                    }
                },
                Some(email) => {
                    match crate::codex::push_account_to_clone(&app, &new_id, &email).await {
                        Ok(()) => patch_op(&app, &op_id, |op| {
                            op.log.push(format!("codex account: assigned {email}"))
                        }),
                        Err(e) => {
                            tracing::warn!("fork {new_id}: Codex assign failed: {e}");
                            patch_op(&app, &op_id, |op| {
                                op.log
                                    .push(format!("codex account: failed to assign {email}: {e}"))
                            });
                        }
                    }
                }
            }
        }
    } else if let Some(email) = src.codex_account_email.clone() {
        match crate::codex::push_account_to_clone(&app, &new_id, &email).await {
            Ok(()) => patch_op(&app, &op_id, |op| {
                op.log
                    .push(format!("codex account: inherited {email} from {source_id}"))
            }),
            Err(e) => {
                tracing::warn!("fork {new_id}: inheriting Codex account failed: {e}");
                patch_op(&app, &op_id, |op| {
                    op.log
                        .push(format!("codex account: failed to inherit {email}: {e}"))
                });
            }
        }
    }

    progress(
        "settle",
        "attaching the shared folder, home link and SSH access",
    );
    crate::homes::ensure_now(&app, &new_id).await;
    crate::ssh::allow_clone_now(&app, &new_id).await;

    // Last settle step, mirroring create: the preset's startup script (best-effort).
    if spec.run_startup_script {
        progress("settle", "running the preset startup script");
        run_startup_script(&app, &op_id, &new_id, preset_name.as_deref()).await;
    } else {
        patch_op(&app, &op_id, |op| {
            op.log.push("startup script: skipped by request".into())
        });
    }

    let daemon_up = app.media.is_connected(&new_id);
    let dataset = crate::zfs::dataset_name(&app.config().docker.homes_parent, &new_id);
    // Payload linear metadata replaces the source's ticket context wholesale when
    // present (a plain-mode fork clears the source ticket); without it the source
    // context carries over field for field.
    let linear = spec.linear.clone();
    let first_message = spec.first_message.clone();
    let agent_instructions = spec.agent_instructions.clone();
    let claude_instructions = spec.claude_instructions.clone();
    let (
        linear_workspace,
        linear_ticket,
        linear_ticket_url,
        linear_branch,
        display_name,
        linear_label,
    ) = match &linear {
        Some(m) => (
            m.workspace.clone(),
            m.ticket.clone(),
            m.ticket_url.clone(),
            m.branch.clone(),
            m.display_name.clone(),
            m.label.clone(),
        ),
        None => (
            src.linear_workspace.clone(),
            src.linear_ticket.clone(),
            src.linear_ticket_url.clone(),
            src.linear_branch.clone(),
            src.display_name.clone(),
            src.linear_label.clone(),
        ),
    };
    // Cloned before the row write below moves the tuple fields into the closure.
    let ticket_url = linear_ticket_url.clone();
    app.store.mutate(|s| {
        let host = RmngClone {
            id: new_id.clone(),
            host: new_id.clone(),
            port: 3389,
            username: "rmng".into(),
            password: "rmng".into(),
            managed: true,
            source: Some(base_tag.clone()),
            dataset: Some(dataset),
            base_tag: Some(base_tag.clone()),
            claude_selection: claude_selection.clone(),
            claude_account_email: claude_account_email.clone(),
            claude_group: claude_group.clone(),
            codex_selection: codex_selection.clone(),
            codex_account_email: codex_account_email.clone(),
            codex_group: codex_group.clone(),
            group: bound_group.clone(),
            preset_name: preset_name.clone(),
            headless,
            linear_workspace,
            linear_ticket,
            linear_ticket_url,
            linear_branch,
            display_name,
            linear_label,
            ..Default::default()
        };
        s.hosts.insert(0, host);
        if let Some(op) = s.operations.iter_mut().find(|o| o.id == op_id) {
            op.status = OperationStatus::Done;
            op.step = "done".into();
            op.pct = 100.0;
            op.message = if daemon_up {
                format!("clone {new_id} forked from {source_id}")
            } else {
                format!(
                    "clone {new_id} forked but its daemon hasn't registered yet (still booting; \
                     check it in the UI)"
                )
            };
            op.finished_at = Some(now_ms());
        }
    });
    schedule_prune(app.clone(), op_id.clone(), PRUNE_DONE_MS);
    // Forked home carries the source's files with a fresh /etc (no stamps): converge it.
    crate::clone_reconcile::spawn_converge_after_start(&app, &new_id, "fork");

    // Kick off the agent, mirroring the create tail: an explicit ticket URL or first
    // message starts work on the fork; a pure inherit (no payload, no source ticket)
    // stays quiet.
    let has_msg = first_message
        .as_deref()
        .map(str::trim)
        .is_some_and(|s| !s.is_empty());
    if ticket_url.is_some() || has_msg {
        if let Some(host) = app.store.get().hosts.into_iter().find(|h| h.id == new_id) {
            tokio::spawn(crate::chat::kickoff_agent(
                app.clone(),
                host,
                crate::chat::KickoffOpts {
                    ticket_url,
                    message: first_message.clone(),
                    agent_instructions: agent_instructions.clone(),
                    claude_instructions: claude_instructions.clone(),
                },
            ));
        }
    }
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

/// Warm a preset image without creating (`POST /api/images/prebuild`): build the posted
/// Dockerfile text on miss, discarding the tag. The preset card's rebuild button posts
/// the editor's current text (which may be unsaved); saving is separate. Drives a
/// `Prebuild` op (no coarse pct table — the build streams step lines as messages).
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

    /// Stand in for a clone that has been deleted: its ledger directory is all that is left.
    fn retire(app: &App, id: &str) {
        let dir = crate::ledger::ledger_root(&app.data_dir()).join(id);
        std::fs::create_dir_all(dir).unwrap();
    }

    #[test]
    fn a_retired_clones_name_is_not_handed_out_again() {
        let app = test_app();
        assert_eq!(next_free_hostname(&app, "pega-we-142"), "pega-we-142");

        // The clone came and went. Its transcripts are filed under that name, so the next clone
        // for the same ticket takes the next letter rather than inheriting the history.
        retire(&app, "pega-we-142");
        assert_eq!(next_free_hostname(&app, "pega-we-142"), "pega-we-142a");
        retire(&app, "pega-we-142a");
        assert_eq!(next_free_hostname(&app, "pega-we-142"), "pega-we-142b");
    }

    #[tokio::test]
    async fn an_exact_hostname_that_a_retired_clone_used_is_rejected() {
        let app = test_app();
        retire(&app, "worker-7");
        let spec = CloneSpec {
            source_image: "img:latest".into(),
            new_hostname: "worker-7".into(),
            ..Default::default()
        };
        let err = start_clone(&app, spec).unwrap_err().0;
        assert!(err.contains("retired"), "{err}");
        // The message names the directory to remove, so the rejection is actionable.
        assert!(err.contains("ledger/worker-7"), "{err}");

        // A name nobody has used is untouched by the check.
        let ok = CloneSpec {
            source_image: "img:latest".into(),
            new_hostname: "worker-8".into(),
            ..Default::default()
        };
        assert!(start_clone(&app, ok).is_ok());
    }

    #[test]
    fn clonespec_default_requests_no_account() {
        // `Default` leaves both selections absent, which the account layer reads as "auto" —
        // NOT as "none". A clone created with no explicit account still gets one.
        let spec = CloneSpec {
            new_hostname: "x".into(),
            ..Default::default()
        };
        assert!(spec.claude_account.is_none());
        assert!(spec.codex_account.is_none());
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
