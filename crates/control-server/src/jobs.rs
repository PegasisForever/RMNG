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
    self, PullProgress, clone_container, commit_clone_image, control_env_vars, delete_clone,
    is_dns_label, pull_template,
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
    /// The clone-source image reference (e.g. `pegasis0/rmng-template:latest`) or id to clone from.
    pub source_image: String,
    pub new_hostname: String,
    pub linear: Option<LinearMeta>,
    /// The account pool this clone's agents route through (one CLIProxyAPI instance per
    /// group). `None` = no inference until a group is bound. This is the sole account binding
    /// under the group-proxy model — the `/cc` router maps clone → group → instance.
    pub group: Option<String>,
    pub first_message: Option<String>,
    pub agent_instructions: Option<String>,
    pub claude_instructions: Option<String>,
    /// Clone preset name used to derive env/playbook, persisted for future reconciliation.
    pub preset_name: Option<String>,
    /// Resolved env-preset vars to write into the clone's `/etc/environment` at creation.
    pub env: Vec<wire::EnvVar>,
    /// Composed agent playbook (global + preset append) injected into the clone at creation
    /// as ~/.config/rmng/agent-instructions.md. Empty ⇒ no file injected.
    pub agent_playbook: String,
    /// Create a **headless clone**: same template, but the desktop (`gnome-headless`) and
    /// capture daemon (`rmng-clone-daemon`) user units are disabled at provision and a default
    /// tmux session is started. Persisted on `Host.headless`; drives the viewer tmux view.
    pub headless: bool,
    /// Parent host id when this clone should be created as a sub host (one level deep). Already
    /// validated by the caller (`web::clone`): the parent exists, is managed, and is itself
    /// top-level. `None` = top-level clone. Persisted on `Host.parent`; purely cosmetic.
    pub parent: Option<String>,
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

/// Pick a free host id for a ticket base name (`base`, then `base a..z`). Race-free
/// when called immediately before `start_clone` (single state snapshot).
pub fn next_free_hostname(app: &App, base: &str) -> String {
    let st = app.store.get();
    let mut taken: std::collections::HashSet<String> =
        st.hosts.iter().map(|h| h.id.clone()).collect();
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
        return Err(JobError(format!(
            "a host named '{}' already exists",
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
    // Sub-host invariant (defense in depth; `web::resolve_parent` already validated): the parent
    // must exist, be a managed clone, and be top-level — nesting is one level deep.
    if let Some(parent) = &spec.parent {
        match st.hosts.iter().find(|h| &h.id == parent) {
            None => return Err(JobError(format!("parent host '{parent}' not found"))),
            Some(h) if !h.managed => {
                return Err(JobError(format!(
                    "parent host '{parent}' is not a managed clone"
                )));
            }
            Some(h) if h.parent.is_some() => {
                return Err(JobError(format!(
                    "parent host '{parent}' is itself a sub host; sub hosts are one level deep"
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

    // The clone→control-server + inference URLs (auto-detected) go into the clone's session
    // env first; the operator's chosen preset follows (so a preset key can still override).
    let mut env = control_env_vars(&app).await;
    // Per-clone group-proxy router key (ANTHROPIC_AUTH_TOKEN / RMNG_PROXY_KEY), minted +
    // persisted server-side and mapped back to this host id by the `/cc` router. Additive:
    // it lives alongside the existing token push; a clone with no group just gets a 409 from
    // the router until one is bound. Never serialized onto `Host`/state.
    env.extend(crate::provision::router_env_vars(&app, &spec.new_hostname));
    env.extend(spec.env.iter().cloned());
    // `image_ref` is the CANONICAL reference of the image actually used (the caller may have
    // passed an id form — MCP/raw API); `Host.source` must record the reference so the
    // commit flow can stamp lineage. The backing container's name is the host id — that's
    // how every later call (dials, redeploy, credential ops, delete) addresses it.
    let image_ref = match clone_container(
        &app,
        &spec.source_image,
        &spec.new_hostname,
        &env,
        &spec.agent_playbook,
        spec.headless,
        progress,
    )
    .await
    {
        Ok(v) => v,
        Err(e) => return fail_op(&app, &op_id, e.to_string()),
    };

    // The container is up and its daemon has registered (or timed out still-booting) — the op
    // now sits at the `ready` step (~80%). The clone is NOT connectable yet: the account tokens
    // still have to be pushed. The client treats a host's PRESENCE in `s.hosts` as "ready to
    // connect", so we keep the host OUT of state and the op RUNNING until this whole tail
    // settles — otherwise a viewer connecting at "100%" hits a not-yet-provisioned clone. The
    // host is registered + the op marked `done` at the very end, below, once the clone is
    // genuinely streamable.
    //
    // (`progress` at the top of this fn was moved into `clone_container`; make a fresh one for
    // the remaining `accounts` step. New clones get their monitor layout from the daemon's
    // `Hello` → server `SetMonitors` live push (no restart-based apply here); the baked
    // `RMNG_MONITORS` default just covers the brief pre-connect window.)
    let mut progress = op_progress(&app, &op_id, OperationKind::Clone);

    progress("accounts", "binding agent account group");

    // Group-proxy binding: the clone's agents route through ONE account group's CLIProxyAPI
    // instance via the control-server's `/cc` router (the per-clone router key was already
    // injected into the clone's env above by `router_env_vars`). Binding is a pure map update
    // — the group is recorded on the Host below and the router resolves clone → group →
    // instance at request time; there is no clone-side credential push. `None` leaves the
    // clone without inference until a group is bound (the router answers 409 until then).
    let group = spec.group.clone();
    match &group {
        Some(g) => patch_op(&app, &op_id, |op| {
            op.log.push(format!("account: group {g}"))
        }),
        None => patch_op(&app, &op_id, |op| {
            op.log.push("account: no group bound".into())
        }),
    }

    // Register the fully-provisioned host and mark the op done — the clone is now genuinely
    // connectable. A host's PRESENCE in `s.hosts` is the client's "ready to connect" signal, so
    // it is added HERE, at the same instant the bar reaches 100%. `host` is display-only for
    // managed clones (dials go by container name == id); clones ship with fixed `rmng`/`rmng`
    // credentials baked into the base image. RDP port stays 3389 for the media path. The
    // group binding resolved above is baked in so the UI shows it the moment the host appears.
    // `daemon_up` reflects whether the clone's daemon has registered (vs. still booting).
    let daemon_up = app.media.is_connected(&spec.new_hostname);
    app.store.mutate(|s| {
        let mut host = Host {
            id: spec.new_hostname.clone(),
            host: spec.new_hostname.clone(),
            port: 3389,
            username: "rmng".into(),
            password: "rmng".into(),
            managed: true,
            source: Some(image_ref.clone()),
            group: group.clone(),
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
    app.tokens.register_host(&spec.new_hostname);

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

/// Pull the clone template from `reference` (a registry `repo:tag`) — no local retag; the
/// pulled image keeps its own `repo:tag`, which becomes the clone-source reference. Drives a
/// `Pull`-kind Operation with the reference as its target; no Host is registered (a template
/// is not a host). Guard: no op is already in flight for the same reference.
pub fn start_pull(app: &App, reference: &str) -> Result<Operation, JobError> {
    let st = app.store.get();
    if st
        .operations
        .iter()
        .any(|o| o.status == OperationStatus::Running && o.target == reference)
    {
        return Err(JobError(format!("'{reference}' is already being pulled")));
    }
    let op = make_op(OperationKind::Pull, reference, None);
    let (ret, op_id) = (op.clone(), op.id.clone());
    app.store.mutate(|s| s.operations.push(op));
    let (app2, reference) = (app.clone(), reference.to_string());
    tokio::spawn(async move { run_pull(app2, op_id, reference).await });
    Ok(ret)
}

async fn run_pull(app: App, op_id: String, reference: String) {
    let progress = pull_op_progress(&app, &op_id);
    let pulled_ref = match pull_template(&app, &reference, progress).await {
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
        op.message = format!("template {pulled_ref} ready");
        op.finished_at = Some(now_ms());
    });
    schedule_prune(app.clone(), op_id, PRUNE_DONE_MS);
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
    let socket = app.config().docker.socket;
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
        return Err(JobError(format!(
            "'{host_id}' is not a managed clone — only clones can be committed"
        )));
    }
    let reference = format!("{name}:latest");
    // Reject a tag already targeted by another running commit/pull (a race the pure
    // `image_exists` check in provision can't see yet). The existing-image check happens in
    // provision (needs the daemon); here we only guard the in-flight duplicate.
    if st.operations.iter().any(|o| {
        o.status == OperationStatus::Running
            && matches!(o.kind, OperationKind::Commit | OperationKind::Pull)
            && o.target == name
    }) {
        return Err(JobError(format!(
            "an image named '{name}' is already being built"
        )));
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
    app.tokens.forget_host(&host_id);
    schedule_prune(app.clone(), op_id, PRUNE_DONE_MS);
    let dd = app.config().data_dir;
    crate::files::delete_notes(&dd, &host_id);
    crate::chat::delete_chat(&dd, &host_id);
}

/// Stop a managed clone without removing its container, volumes, or per-host files.
pub fn start_archive(app: &App, host_id: &str) -> Result<Operation, JobError> {
    let st = app.store.get();
    let host = st
        .hosts
        .iter()
        .find(|h| h.id == host_id)
        .ok_or_else(|| JobError(format!("unknown host '{host_id}'")))?;
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
    let mut progress = op_progress(&app, &op_id, OperationKind::Archive);
    progress("stop", "stopping the clone (SIGRTMIN+3, up to 20s)");
    if let Err(e) = app.docker.stop_container(&host_id).await {
        return fail_op(&app, &op_id, e.to_string());
    }

    app.store.mutate(|s| {
        if let Some(host) = s.hosts.iter_mut().find(|h| h.id == host_id) {
            host.archived = true;
            host.monitor_state = None;
            host.local_ip = None;
            host.unread = false;
        }
        if let Some(op) = s.operations.iter_mut().find(|o| o.id == op_id) {
            op.status = OperationStatus::Done;
            op.step = "done".into();
            op.pct = 100.0;
            op.message = format!("clone {host_id} archived");
            op.finished_at = Some(now_ms());
        }
    });
    app.tokens.set_archived(&host_id, true);
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
        .ok_or_else(|| JobError(format!("unknown host '{host_id}'")))?;
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
    progress("start", "starting the archived clone");
    if let Err(e) = app.docker.start_container(&host_id).await {
        return fail_op(&app, &op_id, e.to_string());
    }

    app.store.mutate(|s| {
        if let Some(host) = s.hosts.iter_mut().find(|h| h.id == host_id) {
            host.archived = false;
            host.monitor_state = None;
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
    app.tokens.set_archived(&host_id, false);
    drop(progress);
    crate::shm::ensure_now(&app, &host_id).await;
    schedule_prune(app.clone(), op_id, PRUNE_DONE_MS);
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
        let cfg = wire::AppConfig {
            data_dir: dir.to_string_lossy().into_owned(),
            ..Default::default()
        };
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

    #[test]
    fn clonespec_default_has_no_group() {
        let spec = CloneSpec {
            new_hostname: "x".into(),
            ..Default::default()
        };
        assert!(spec.group.is_none());
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

    /// Per-layer pull `Status` events (surfaced to `pull_op_progress` as [`PullProgress::Log`])
    /// must reach the op LOG + message like the retired bootstrap's pull logging did, but
    /// without moving `step` off `"pull"` or perturbing the byte-driven `pct` — that's owned
    /// exclusively by [`PullProgress::Pct`] (`Bytes` events), which must stay message-only.
    #[tokio::test]
    async fn pull_log_event_reaches_op_log_without_moving_pct_or_step() {
        let app = test_app();
        app.store
            .mutate(|s| s.operations.push(running_op("op_a", "tpl-a")));
        let mut progress = pull_op_progress(&app, "op_a");

        progress(PullProgress::Log {
            msg: "aaaaaaaaaaaa: Downloading".into(),
        });

        let st = app.store.get();
        let op = st.operations.iter().find(|o| o.id == "op_a").unwrap();
        assert_eq!(op.step, "pull"); // unmoved
        assert_eq!(op.pct, 40.0); // unmoved — pct stays byte-driven
        assert_eq!(op.message, "aaaaaaaaaaaa: Downloading");
        assert!(
            op.log
                .iter()
                .any(|l| l == "pull: aaaaaaaaaaaa: Downloading")
        );

        // A subsequent `Pct` (Bytes) tick updates pct + message but must NOT add a log line —
        // the log stays exactly as the `Log` event left it.
        let log_len_before = op.log.len();
        progress(PullProgress::Pct {
            pct: 50.0,
            msg: "pulling docker.io/x:y: 55%".into(),
        });
        let st = app.store.get();
        let op = st.operations.iter().find(|o| o.id == "op_a").unwrap();
        assert_eq!(op.pct, 50.0);
        assert_eq!(op.message, "pulling docker.io/x:y: 55%");
        assert_eq!(op.log.len(), log_len_before); // no new log line from a Pct/Bytes tick
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
            s.hosts.push(Host {
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
            s.hosts.push(Host {
                id: "plain".into(),
                host: "plain".into(),
                ..Default::default()
            });
            s.hosts.push(Host {
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
