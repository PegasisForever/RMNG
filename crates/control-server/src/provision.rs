//! `provision.rs` — the clone lifecycle over Docker (bollard).
//!
//! The Rust port of RMNG's fleet orchestration, replacing the retired SSH+`pct`+bash path
//! (`orchestrate.rs` + `mounts.rs` + `clone.sh`/`bootstrap.sh`/`delete.sh`/`redeploy.sh`).
//! Every operation drives the dumb, composable [`DockerCtl`] primitives in `docker.rs`
//! into full flows and streams progress through the callers' `FnMut(&str, &str)` callback
//! (the `P <step> <msg>` bash protocol is gone — Rust emits `(step, message)` directly; a
//! guest script's own stdout lines are line-buffered into the operation log).
//!
//! Caller-facing division of responsibility (as with `orchestrate.rs`): `jobs.rs` owns the
//! `Operation` record + the progress→op-log plumbing and calls the flows here; `claude.rs`
//! drives credential ops via [`run_clone_op`]. These functions address a clone by its
//! container *name*, which equals the host id (`Host.managed` rows) — no container id is
//! stored anywhere.
//!
//! Guest scripts are embedded (`include_str!`) and streamed over `docker exec bash -s`:
//! [`crate::docker::DockerCtl::exec_script`]. Binaries (clone-daemon, agent-wrapper) are
//! pushed via `upload_tar`. The clone TEMPLATE itself is no longer built in-product — it is
//! pulled from a registry by [`pull_template`] (the retired in-product bootstrap ran
//! `provision-clone.sh` inside a build container; that recipe now lives in
//! `template/Dockerfile` + `template/setup/`, published as a Docker image).

use anyhow::{Result, bail};
use std::time::{Duration, Instant};

use wire::EnvVar;

use crate::app::App;
use crate::docker::{CreateSpec, PullEvent, TarEntry, CLONE_USER};

/// The clone user's uid/gid inside every image (created uid 1000 by `template/setup/30-user.sh`
/// at template build).
/// tar entries under `home/rmng/**` carry this verbatim so the daemon extracts them owned
/// by the clone user (gotcha #2).
const CLONE_UID: u64 = 1000;
const CLONE_GID: u64 = 1000;

/// How long to wait for a freshly-created clone's daemon to register (`Hello`) before
/// treating it as "started but not yet ready" (a warning, not a failure — the clone is
/// still booting its headless GNOME + user units under linger).
const WAIT_READY_TIMEOUT: Duration = Duration::from_secs(90);
/// Poll interval while waiting for readiness.
const WAIT_READY_POLL: Duration = Duration::from_secs(2);

// --- pure ports -----------------------------------------------------------------------

/// A DNS label (host-id / hostname validity + path-traversal guard). Ported verbatim.
pub fn is_dns_label(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 63
        && s.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        && !s.starts_with('-')
        && !s.ends_with('-')
}

/// A fresh random machine-id file body: 32 lowercase hex chars + newline, from
/// `/dev/urandom` (the same format `systemd-machine-id-setup` writes). Injected per
/// clone because systemd-in-docker won't persist one itself (see the caller). Errors
/// instead of degrading: a silent all-zero fallback would hand every clone the SAME
/// id — exactly the collision this exists to prevent.
fn fresh_machine_id() -> Result<Vec<u8>> {
    use anyhow::Context as _;
    use std::io::Read;
    let mut buf = [0u8; 16];
    std::fs::File::open("/dev/urandom")
        .and_then(|mut f| f.read_exact(&mut buf))
        .context("reading /dev/urandom for a fresh clone machine-id")?;
    let mut s: String = buf.iter().map(|b| format!("{b:02x}")).collect();
    s.push('\n');
    Ok(s.into_bytes())
}

/// Resolve a caller-supplied image — a repo-tag reference (e.g. `pegasis0/rmng-template:latest`),
/// a full `sha256:…` id, or a bare 64-hex id — to the **canonical** [`wire::ImageInfo`] `reference`
/// of the matching clone-source image. `None` when nothing in the listed clone sources
/// matches (i.e. the input isn't a labeled `rmng.image=1` image at all).
///
/// This is what keeps the created container's `Image` column canonical regardless of the
/// caller's input form: the in-use accounting (web.rs `fill_in_use_by`) and the
/// images-delete 409 guard both compare `ManagedContainer.image == ImageInfo.reference`,
/// so a clone created from an id form must still be created FROM the reference — otherwise
/// its base image would show as unused and be deletable under live clones. `Host.source`
/// records it too (commit lineage).
pub fn resolve_reference(images: &[wire::ImageInfo], input: &str) -> Option<String> {
    images
        .iter()
        .find(|i| i.reference == input || i.id == input || i.id.strip_prefix("sha256:") == Some(input))
        .map(|i| i.reference.clone())
}

/// Base desktop session env every clone needs before its preset/control values are added.
pub(crate) fn base_session_env_vars() -> Vec<EnvVar> {
    [
        ("XDG_CURRENT_DESKTOP", "GNOME"),
        ("XDG_SESSION_DESKTOP", "gnome"),
        ("DESKTOP_SESSION", "gnome"),
        ("XDG_SESSION_CLASS", "user"),
        ("XDG_MENU_PREFIX", "gnome-"),
        ("XDG_SESSION_TYPE", "wayland"),
    ]
    .into_iter()
    .map(|(key, value)| EnvVar { key: key.to_string(), value: value.to_string() })
    .collect()
}

/// The clone→control-server + detector-inference env every clone needs, as
/// [`EnvVar`]s. Points the detector's feedback + agent
/// `set_state` MCP at THIS control-server and the detector's vision model at the configured
/// inference server. The control host is `docker.control_host()` — the `rmng-control`
/// DNS alias on the rmng bridge (the gateway IP in dev mode; see `docker.rs`). Empty
/// control URLs (with a warning) if it can't be resolved, so clones fall back to the
/// compiled detector defaults.
pub async fn control_env_vars(app: &App) -> Vec<EnvVar> {
    let cfg = app.config();
    let ev = |key: &str, value: String| EnvVar { key: key.to_string(), value };
    let mut vars = Vec::new();
    match app.docker.control_host().await {
        Ok(control) => {
            vars.push(ev("RMNG_CONTROL_URL", format!("http://{control}:{}", cfg.listen.web)));
            vars.push(ev("AGENT_CONTROL_MCP_URL", format!("http://{control}:{}", cfg.listen.clone_mcp)));
            // Group-proxy router: every clone's agents reach the control-server's `/cc`
            // reverse proxy at a constant URL; the router maps the clone's per-clone bearer
            // key → its group instance. Claude Code appends `/v1/messages` + `/v1/models`
            // to ANTHROPIC_BASE_URL; the gateway-discovery flag lets its picker learn the
            // instance's `/v1/models` catalog. The per-clone bearer (ANTHROPIC_AUTH_TOKEN /
            // RMNG_PROXY_KEY) is added separately by `router_env_vars` (it's per-clone, not
            // shared). See `docs/superpowers/specs/2026-07-19-cliproxy-group-proxy-plan.md`.
            vars.push(ev("ANTHROPIC_BASE_URL", format!("http://{control}:{}/cc", cfg.listen.web)));
            vars.push(ev("CLAUDE_CODE_ENABLE_GATEWAY_MODEL_DISCOVERY", "1".to_string()));
        }
        Err(e) => tracing::warn!(
            "control_env_vars: could not resolve the control-server host ({e}); \
             clones fall back to the compiled detector defaults"
        ),
    }
    let infer = cfg.detector_inference_url.trim();
    if !infer.is_empty() {
        vars.push(ev("RMNG_INFERENCE_URL", infer.to_string()));
    }
    vars
}

/// The PER-CLONE group-proxy env: the clone's stable router bearer key, exposed both as
/// `ANTHROPIC_AUTH_TOKEN` (Claude Code) and `RMNG_PROXY_KEY` (referenced by the generated
/// Codex + OpenCode provider configs). Minted + persisted by [`crate::cliproxy`] on first
/// use (stable for the clone's life; the router maps it back to this host id). Kept OUT of
/// [`control_env_vars`] because it is per-clone, not a shared constant — and NEVER put on
/// `Host`/`state.json`/`/events` (it's a secret). Wired into the clone's `/etc/environment`
/// at create (`jobs.rs`) and on every per-clone resync (`clone_reconcile.rs`).
pub(crate) fn router_env_vars(app: &App, host_id: &str) -> Vec<EnvVar> {
    let key = app.cliproxy.mint_router_key(host_id);
    vec![
        EnvVar { key: "ANTHROPIC_AUTH_TOKEN".into(), value: key.clone() },
        EnvVar { key: "RMNG_PROXY_KEY".into(), value: key },
    ]
}

/// The clone-facing base URL of the group-proxy router's OpenAI-compatible surface
/// (`http://{control}:{web}/cc/v1`) — what the generated Codex + OpenCode provider configs
/// point their `base_url`/`baseURL` at. Derived from the same control-host resolution
/// `control_env_vars` uses; `None` (with a warning) when the control host can't be resolved,
/// so the config generators fall back to their old behavior instead of baking a broken URL.
pub(crate) async fn cc_base_url(app: &App) -> Option<String> {
    let cfg = app.config();
    match app.docker.control_host().await {
        Ok(control) => Some(format!("http://{control}:{}/cc/v1", cfg.listen.web)),
        Err(e) => {
            tracing::warn!(
                "cc_base_url: could not resolve the control-server host ({e}); Codex/OpenCode \
                 provider configs will omit the RMNG group-proxy provider this pass"
            );
            None
        }
    }
}

/// The preset's env plus its Linear key as `LINEAR_API_KEY` (auths the clone's
/// `linear` MCP). A `LINEAR_API_KEY` var set explicitly in the preset wins.
pub(crate) fn preset_env_vars(p: &wire::Preset) -> Vec<EnvVar> {
    let mut vars = p.vars.clone();
    if !p.linear_key.is_empty() && !vars.iter().any(|v| v.key == "LINEAR_API_KEY") {
        vars.push(EnvVar { key: "LINEAR_API_KEY".into(), value: p.linear_key.clone() });
    }
    vars
}

/// `/etc/environment` body: `KEY=VALUE` lines, skipping empty keys. Last duplicate key wins,
/// which lets preset/control values override the base desktop session defaults.
pub(crate) fn etc_environment_conf(vars: &[EnvVar]) -> String {
    let mut rows: Vec<(&str, &str)> = Vec::new();
    for v in vars.iter().filter(|v| !v.key.is_empty()) {
        rows.retain(|(key, _)| *key != v.key);
        rows.push((&v.key, &v.value));
    }
    rows.into_iter().map(|(key, value)| format!("{key}={value}\n")).collect()
}

pub(crate) fn clone_etc_environment_conf(vars: &[EnvVar]) -> String {
    let mut all = base_session_env_vars();
    all.extend(vars.iter().cloned());
    etc_environment_conf(&all)
}

/// Shell-rc files that prepend a preset's `PATH` dirs for interactive shells. The Rust port
/// of the deleted `clone.sh::write_preset_path_rc`.
///
/// A preset `PATH` needs more than `/etc/environment`: interactive shells rewrite `PATH` on
/// startup (login bash re-runs `/etc/profile`, which hard-resets it; fish rebuilds `$PATH`).
/// Mirror the template's `rmng-local-bin` blocks: prepend the preset's dirs inside
/// fish (`conf.d`), login sh/bash (`profile.d`), and non-login interactive bash
/// (`/etc/bash.bashrc`). We always PREPEND (never replace) so the shell keeps its system dirs
/// even if the preset set `PATH` outright, and drop any `$PATH` token; dirs are reversed so
/// the listed order wins (each is prepended in turn).
///
/// Returns the `(fish_conf, profile_sh, bashrc_block)` tuple, or `None` when the preset has
/// no `PATH` var (or it has no usable dirs). The bashrc block is marker-delimited so a
/// re-provision can delete+re-append it; the fish + profile files are whole-file replacements
/// (idempotent by overwrite). All three are dropped as root-owned `/etc` files by the caller.
fn preset_path_rc(env_text: &str) -> Option<PresetPathRc> {
    // Last PATH=… line wins (mirrors the shell taking the final assignment).
    let path_val = env_text
        .lines()
        .filter_map(|l| l.strip_prefix("PATH="))
        .last()?;
    // Reversed, quoted, `$PATH`/empty tokens dropped — the fish/sh loops each PREPEND in
    // turn, so reversing makes the listed left-to-right order win.
    let mut rev: Vec<String> = Vec::new();
    for seg in path_val.split(':') {
        match seg {
            "" | "$PATH" | "${PATH}" => continue,
            _ => rev.insert(0, format!("\"{seg}\"")),
        }
    }
    if rev.is_empty() {
        return None;
    }
    let dirs = rev.join(" ");

    let fish = format!(
        "for d in {dirs}\n    if not contains -- \"$d\" $PATH\n        set -gx PATH \"$d\" $PATH\n    end\nend\n"
    );
    let profile = format!(
        "# rmng env preset: prepend the preset PATH dirs for login sh/bash.\n\
         for d in {dirs}; do\n  case \":$PATH:\" in\n    *\":$d:\"*) : ;;\n    *) PATH=\"$d:$PATH\" ;;\n  esac\ndone\n"
    );
    // Marker-delimited so the append-to-/etc/bash.bashrc step can delete a prior block first.
    let bashrc = format!(
        "# >>> rmng-preset-path >>>\n\
         # rmng env preset: prepend preset PATH dirs for non-login interactive bash.\n\
         for d in {dirs}; do\n  case \":$PATH:\" in\n    *\":$d:\"*) : ;;\n    *) PATH=\"$d:$PATH\" ;;\n  esac\ndone\n\
         # <<< rmng-preset-path <<<\n"
    );
    Some(PresetPathRc { fish, profile, bashrc })
}

/// The three shell-rc payloads a preset `PATH` needs (see [`preset_path_rc`]).
struct PresetPathRc {
    fish: String,
    profile: String,
    bashrc: String,
}

// --- clone container ------------------------------------------------------------------

/// Progress step → percentage for a clone-container create.
fn clone_pct(step: &str) -> Option<f64> {
    Some(match step {
        "queued" => 0.0,
        "create" => 20.0,
        "inject" => 35.0,
        "start" => 55.0,
        "wait-ready" => 75.0,
        // `clone_container` returns at `ready`; `run_clone` drives the rest of the tail
        // (monitors → accounts → done), so 100% is only reached once the clone is actually
        // connectable — not the moment its daemon first registers.
        "ready" => 80.0,
        "monitors" => 85.0,
        "accounts" => 95.0,
        "done" => 100.0,
        _ => return None,
    })
}

/// Create + start a clone container from an `rmng.image=1` source image, injecting its
/// identity/preset/PATH files, and wait for its daemon to register.
///
/// Steps (→ pct): `queued` 0, `create` 20, `inject` 35, `start` 55, `wait-ready` 75,
/// `ready` 80 — `ready` is this fn's TERMINAL step (daemon registered, or timed-out
/// still-booting). The remaining `monitors` 85 / `accounts` 95 / `done` 100 steps are driven
/// by the caller (`run_clone`), so this fn returning does NOT mean the clone is connectable
/// yet. Returns the **canonical** image reference on success (`Host.source`; see
/// [`resolve_reference`] — the caller may have passed an id form, but state must always
/// record the reference so the commit flow can stamp lineage). The container *name* is the
/// hostname (== host id) — that's the clone's address (Docker DNS on the rmng bridge; its
/// IP is plain Docker IPAM, never allocated or stored here). No id is returned or stored.
/// On any failure BEFORE readiness, a cleanup trap removes the created container + its
/// per-clone dind volume so a retry isn't blocked by a stale same-named container
/// (gotcha #7).
///
/// `image` must be a clone source (`rmng.image=1`); `env` is the resolved control + preset
/// env (control URLs first so a preset can still override). One `upload_tar` injects: a
/// fresh random `/etc/machine-id` (always — a committed image carries a baked one),
/// `/etc/environment`, and — when the preset sets `PATH` — the fish/profile preset-PATH rc
/// (root-owned `/etc`). After start, when the preset set `PATH`,
/// the bashrc marker block is appended via an exec (a plain tar can't append). wait-ready
/// polls the mediaplane for the daemon's `Hello{clone_id == hostname}` ≤ 90 s; a timeout with
/// the container still running SUCCEEDS with a warning in the op log; a dead container FAILS
/// with a `docker logs` tail folded into the op log.
pub async fn clone_container(
    app: &App,
    image: &str,
    hostname: &str,
    env: &[EnvVar],
    agent_playbook: &str,
    mut on_progress: impl FnMut(&str, &str),
) -> Result<String> {
    if !is_dns_label(hostname) {
        bail!("clone hostname must be a DNS label (lowercase letters, digits, hyphens)");
    }
    let cfg = app.config();
    let docker = &app.docker;

    on_progress("queued", &format!("queued clone {hostname}"));

    // Validate the source is actually a clone-source image (label rmng.image=1) — not just
    // any image id. The image picker only offers labeled images, but a raw MCP/API caller
    // could pass anything (reference, sha256: id, or bare id), so gate it here AND resolve
    // whatever form was passed to the canonical reference — everything downstream
    // (`Host.source`, in-use accounting, delete guards) keys on the reference.
    if !docker.image_exists(image).await? {
        bail!("source image '{image}' does not exist");
    }
    let images = docker.list_rmng_images().await?;
    let Some(reference) = resolve_reference(&images, image) else {
        bail!("image '{image}' is not a clone source (missing the `rmng.image=1` label)");
    };

    // The rmng bridge is lazy; make sure it's up before joining it.
    docker.ensure_network().await?;

    // Create the container (name == host id) from the CANONICAL reference (equivalent
    // to the caller's input — same image — but keeps `docker ps`'s Image column
    // readable). Its IP is Docker IPAM's business; the name is the address. A stale
    // same-named container 409s here — the daemon message is surfaced verbatim
    // (gotcha #7).
    on_progress("create", &format!("creating container {hostname}"));
    let spec = CreateSpec {
        name: hostname.to_string(),
        image: reference.clone(),
        hostname: hostname.to_string(),
        env: env.iter().filter(|v| !v.key.is_empty()).map(|v| (v.key.clone(), v.value.clone())).collect(),
        cpus: cfg.docker.clone_cpus,
        memory_mb: cfg.docker.clone_memory_mb,
        sock_source: sock_source_dir(app).await,
    };
    let container = docker.create_clone_container(&spec).await?;

    // From here on, a failure must tear the half-built clone down. Run the rest under
    // a guard that removes the container + its dind volumes on any early return.
    match clone_container_after_create(app, &container, hostname, env, agent_playbook, &mut on_progress).await {
        Ok(()) => {
            // Shared build infra: optimistically apply the Hub mirror + remote buildx builder
            // now (idempotent, best-effort; the buildinfra reconciler is the backstop if the
            // inner dockerd isn't up yet). No-op when the feature is off.
            crate::buildinfra::apply_to_clone(app, &container).await;
            Ok(reference)
        }
        Err(e) => {
            tracing::warn!("clone {hostname} failed after create; cleaning up: {e}");
            docker.remove_container(&container).await.ok();
            docker.remove_volume(&crate::docker::DockerCtl::dind_volume_name(hostname)).await.ok();
            docker.remove_volume(&crate::docker::DockerCtl::ctd_volume_name(hostname)).await.ok();
            Err(e)
        }
    }
}

/// The inject → start → wait-ready tail of [`clone_container`], factored out so the caller
/// can run it under a cleanup trap.
async fn clone_container_after_create(
    app: &App,
    container: &str,
    hostname: &str,
    env: &[EnvVar],
    agent_playbook: &str,
    on_progress: &mut impl FnMut(&str, &str),
) -> Result<()> {
    let docker = &app.docker;
    let cfg = app.config();

    // Install the clone binaries while the container is still STOPPED, into the
    // `/opt/rmng/bin` dir the template pre-creates (30-user.sh) but leaves EMPTY — the template
    // no longer carries clone-daemon/agent-wrapper. This is the SOLE delivery path: the
    // control-server always copies its own current payloads in before boot, so a fresh clone's
    // `systemd --user` units always exec binaries that match THIS server (no runtime
    // hash-check / hot-swap engine, and none of its create-time churn). `payload` is None only
    // in a dev checkout with nothing staged under `embedded-bin/` — then the clone boots with
    // no daemon (WARN), matching the pre-existing dev caveat. upload_tar works on a stopped
    // container.
    let mut bins: Vec<TarEntry> = CLONE_BINARIES
        .iter()
        .filter_map(|b| {
            crate::assets::payload(b.payload).map(|data| TarEntry {
                path: format!("{}/{}", b.dir, b.bin),
                data,
                mode: 0o755,
                uid: 0,
                gid: 0,
            })
        })
        .collect();
    if bins.is_empty() {
        tracing::warn!(
            "clone {hostname}: no clone binaries staged (assets::payload empty) — it will boot \
             without clone-daemon/agent-wrapper; stage crates/control-server/embedded-bin/ for dev"
        );
    } else {
        on_progress("inject", "installing clone binaries (pre-boot)");
        if bins.len() == CLONE_BINARIES.len() {
            bins.push(crate::clone_reconcile::payload_stamp_entry_for(&bins));
        }
        docker.upload_tar(container, bins).await?;
    }

    // systemd PID 1 comes up; we then inject identity + preset files before the user units
    // settle, so their PAM-created environment sees `/etc/environment`.
    on_progress("inject", "starting container to inject identity + preset");
    docker.start_container(container).await?;

    // Belt-and-suspenders: reconcile /dev/shm the moment the clone is up, so its desktop never
    // touches the 64 MB default even for the first reconcile tick. This clone was just created
    // WITH `shm_size` in its HostConfig, so it's normally already at target and this is a no-op
    // — it exists so the guarantee doesn't hinge on the create-path constant alone. Autonomous
    // (`unless-stopped`) restarts back to 64 MB are the reconcile loop's job, not this hook's.
    crate::shm::ensure_now(app, container).await;

    // Codex reads global guidance + MCP config from ~/.codex. Prepare the directory before the
    // tar upload so ownership stays correct even for older templates where the Codex install
    // did not create it.
    on_progress("inject", "preparing Codex guidance + MCP config");
    let code = docker
        .exec_script(container, crate::clone_reconcile::codex_prepare_script(), &[], &[], |_stream, line| {
            tracing::debug!(target: "provision", "codex-prepare: {line}");
        })
        .await?;
    if code != 0 {
        tracing::warn!("clone {hostname}: Codex config directory prepare exited {code} (reconciler will retry)");
    }
    let code = docker
        .exec_script(container, crate::clone_reconcile::codex_cli_install_script(), &[], &[], |_stream, line| {
            tracing::debug!(target: "provision", "codex-cli-install: {line}");
        })
        .await?;
    if code != 0 {
        tracing::warn!("clone {hostname}: Codex CLI install exited {code} (reconciler will retry)");
    }

    // Build the single upload_tar: machine-id (always), /etc/environment + PATH rc.
    let preset_conf = clone_etc_environment_conf(env);
    let path_rc = preset_path_rc(&preset_conf);
    let mut entries: Vec<TarEntry> = vec![
        // Fresh random machine-id: a committed image bakes one in, and systemd-in-docker
        // does NOT persist a generated id into an empty writable /etc/machine-id (it runs
        // with a transient one; seen live in the E2E — hostnamectl broken, id unstable
        // across restarts). Writing a unique id per clone gives stable, collision-free
        // D-Bus/journald identity; commit truncates it again, so images never carry it.
        TarEntry { path: "etc/machine-id".into(), data: fresh_machine_id()?, mode: 0o444, uid: 0, gid: 0 },
        // Per-clone env (base desktop session + control URLs + preset vars), read by PAM for
        // SSH sessions and the lingering user manager.
        TarEntry {
            path: "etc/environment".into(),
            data: preset_conf.clone().into_bytes(),
            mode: 0o644,
            uid: 0,
            gid: 0,
        },
    ];
    // The Settings-editable agent playbook (global + preset append), read by the agent-wrapper
    // at startup (AGENT_INSTRUCTIONS_PATH). Empty ⇒ skip; the wrapper then uses its baked-in
    // default. Distinct from /etc/environment (this is a multi-KB markdown blob, not a KEY=VALUE).
    if !agent_playbook.trim().is_empty() {
        entries.push(TarEntry {
            path: format!("home/{CLONE_USER}/.config/rmng/agent-instructions.md"),
            data: agent_playbook.as_bytes().to_vec(),
            mode: 0o644,
            uid: CLONE_UID,
            gid: CLONE_GID,
        });
    }
    let control_mcp_url = env
        .iter()
        .find(|v| v.key == "AGENT_CONTROL_MCP_URL")
        .map(|v| v.value.as_str());
    // The group-proxy /cc/v1 base for the generated Codex/OpenCode provider configs, derived
    // from the ANTHROPIC_BASE_URL (`.../cc`) that `control_env_vars` injected into this env.
    let cc_base = env
        .iter()
        .find(|v| v.key == "ANTHROPIC_BASE_URL")
        .map(|v| format!("{}/v1", v.value));
    let mut codex_entries =
        crate::clone_reconcile::codex_parity_entries(hostname, control_mcp_url, cc_base.as_deref());
    codex_entries.push(crate::clone_reconcile::codex_parity_stamp_entry_for(&codex_entries));
    entries.append(&mut codex_entries);
    if let Some(rc) = &path_rc {
        entries.push(TarEntry {
            path: "etc/fish/conf.d/rmng-preset-path.fish".into(),
            data: rc.fish.clone().into_bytes(),
            mode: 0o644,
            uid: 0,
            gid: 0,
        });
        entries.push(TarEntry {
            path: "etc/profile.d/rmng-preset-path.sh".into(),
            data: rc.profile.clone().into_bytes(),
            mode: 0o644,
            uid: 0,
            gid: 0,
        });
    }
    // SSH: the clone's stable host key + the current authorized_keys, so `ssh -J … rmng@<id>`
    // works the moment the clone is up. The template pre-created ~rmng/.ssh (700) and ships
    // no host keys, so these land with the right owner/perms. Best-effort: a keygen failure
    // must not fail the whole clone — log and continue (SSH just won't work until the next
    // reconcile push).
    match crate::ssh::clone_ssh_tar_entries(&cfg.data_dir, hostname, &cfg.ssh.authorized_keys) {
        Ok(mut ssh_entries) => {
            ssh_entries.push(crate::clone_reconcile::ssh_stamp_entry());
            entries.append(&mut ssh_entries);
        }
        Err(e) => tracing::warn!("clone {hostname}: ssh material skipped: {e}"),
    }

    on_progress("inject", "injecting machine-id + preset env + PATH rc");
    docker.upload_tar(container, entries).await?;

    // The bashrc block can't go in the tar (it's an APPEND, not a whole file — /etc/bash.bashrc
    // already exists in the image). Delete any prior rmng-preset-path block then re-append,
    // so a re-provision stays idempotent. Only when the preset sets PATH.
    on_progress("start", &format!("clone {hostname} starting"));
    if let Some(rc) = &path_rc {
        let script = format!(
            "set -e\n\
             sed -i '/# >>> rmng-preset-path >>>/,/# <<< rmng-preset-path <<</d' /etc/bash.bashrc 2>/dev/null || true\n\
             cat >> /etc/bash.bashrc <<'RMNG_PRESET_PATH_EOF'\n{}RMNG_PRESET_PATH_EOF\n",
            rc.bashrc
        );
        let code = docker
            .exec_script(container, &script, &[], &[], |_stream, line| {
                tracing::debug!(target: "provision", "bashrc-append: {line}");
            })
            .await?;
        if code != 0 {
            // Non-fatal: the preset PATH still reaches fish + login shells; only non-login
            // interactive bash misses it. Warn rather than tear the clone down.
            tracing::warn!("clone {hostname}: bashrc preset-PATH append exited {code} (non-fatal)");
        }
    }

    // wait-ready: poll the mediaplane for the daemon's Hello (keyed by clone_id == hostname).
    on_progress("wait-ready", "waiting for the clone-daemon to register");
    let deadline = Instant::now() + WAIT_READY_TIMEOUT;
    loop {
        if app.media.is_connected(hostname) {
            on_progress("ready", &format!("clone {hostname} up + registered"));
            return Ok(());
        }
        if Instant::now() >= deadline {
            // Timeout: distinguish "still booting" (container alive) from "died".
            if docker.is_running(container).await.unwrap_or(false) {
                // Succeed with a warning: the clone is up but its daemon hasn't registered
                // yet (headless GNOME + user units can be slow on first boot).
                on_progress(
                    "ready",
                    &format!(
                        "clone {hostname} started but its daemon hasn't registered within {}s \
                         (still booting; check it in the UI)",
                        WAIT_READY_TIMEOUT.as_secs()
                    ),
                );
                return Ok(());
            }
            // Dead: fold the container's log tail into the op log, then fail.
            let logs = docker.container_logs_tail(container, 30).await;
            let tail = if logs.trim().is_empty() { String::new() } else { format!("\n{logs}") };
            bail!("clone {hostname} exited before its daemon registered; last logs:{tail}");
        }
        tokio::time::sleep(WAIT_READY_POLL).await;
    }
}

// --- template pull --------------------------------------------------------------------

/// A template-pull progress event. Unlike the shared `(step, msg)` callback the clone /
/// commit / delete flows use, the pull emits a coarse STEP transition (jobs maps it to the
/// [`pull_pct`] table), a fine byte-progress PCT inside the long `pull` step (so the
/// aggregate download fraction reaches the op bar without a log line per byte tick), or a
/// per-layer status LOG line (message + op log, no pct move) — the same volume-capped
/// per-(layer, status) transitions the retired in-product bootstrap logged.
#[derive(Debug, Clone)]
pub enum PullProgress {
    /// A coarse step transition (`queued`/`pull`/`verify`/`done`); maps to [`pull_pct`].
    Step { step: String, msg: String },
    /// Fine byte progress inside the `pull` step: an absolute pct (2–90) + a message.
    Pct { pct: f64, msg: String },
    /// A per-layer pull status line (`docker.rs`'s deduped `PullEvent::Status`): pushed to the
    /// op log + the message, WITHOUT moving `step` off `"pull"` or touching `pct` (pct stays
    /// byte-driven via [`PullProgress::Pct`]).
    Log { msg: String },
}

/// Progress step → percentage for a template pull. The `pull` step's 2–90 span is filled by
/// [`pull_template`] itself from aggregate byte progress (`2 + frac·88`), so the table only
/// pins the coarse floors.
fn pull_pct(step: &str) -> Option<f64> {
    Some(match step {
        "queued" => 0.0,
        "pull" => 2.0,
        "verify" => 91.0,
        "done" => 100.0,
        _ => return None,
    })
}

/// Pull the clone template from `remote_ref` (a registry `repo:tag`) and return that same
/// reference as the canonical clone-source ref clones are created FROM. No local retag: the
/// pulled image keeps its own repo:tag (e.g. `pegasis0/rmng-template:latest`), which is what
/// the image picker lists and what `createClone` passes back. This REPLACES the retired
/// in-product bootstrap (which provisioned a base from `ubuntu` inside a build container); the
/// template is now built by `template/Dockerfile` and published to a registry.
///
/// Steps (→ pct): `queued` 0, `pull` 2–90 (aggregate byte progress via [`PullProgress::Pct`]),
/// `verify` 91, `done` 100. Returns the pulled reference.
///
/// The pulled image must carry `rmng.image=1` — else it isn't an RMNG template and would just
/// sit around unused (it never enters the picker, which filters on that label). A non-standard
/// `StopSignal` only WARNs (clones off it hang 20 s on stop, but that's no reason to refuse the
/// pull). Re-pulling the same `repo:tag` naturally moves the local tag onto the fresh image
/// (standard `docker pull`) — that IS the refresh, so there's nothing to guard.
pub async fn pull_template(
    app: &App,
    remote_ref: &str,
    mut on_progress: impl FnMut(PullProgress),
) -> Result<String> {
    let remote = remote_ref.trim();
    if remote.is_empty() {
        bail!("a template reference is required");
    }
    if remote.chars().any(char::is_whitespace) {
        bail!("template reference '{remote}' must not contain whitespace");
    }
    // A `repo@sha256:…` digest ref is mis-split by `split_reference` (it treats the digest's
    // own `:` as the tag separator), so refuse it — pull a `repo:tag` reference instead.
    if remote.contains('@') {
        bail!("digest references ('{remote}') aren't supported — pull a repo:tag reference instead");
    }

    let docker = &app.docker;

    on_progress(PullProgress::Step {
        step: "queued".into(),
        msg: format!("queued template pull {remote}"),
    });

    // Pull (2–90%): map the aggregate byte fraction onto `2 + frac·88`. `Status` lines (already
    // deduped per-(layer, status) transition by `pull_image`) land in the op LOG + message, as
    // the retired in-product bootstrap logged them — same formatting, without moving pct;
    // `Bytes` drives the fine pct + message with NO log line (it fires up to ~100 times per
    // pull, which would swamp the log). A daemon error (e.g. a Docker Hub rate limit) is
    // surfaced verbatim by `pull_image` (gotcha #9).
    on_progress(PullProgress::Step { step: "pull".into(), msg: format!("pulling {remote}") });
    {
        let on_progress = &mut on_progress;
        docker
            .pull_image(remote, |event| match event {
                PullEvent::Status { layer, status } => {
                    let msg = if layer.is_empty() { status } else { format!("{layer}: {status}") };
                    on_progress(PullProgress::Log { msg });
                }
                PullEvent::Bytes { frac } => {
                    let pct = 2.0 + frac * 88.0;
                    on_progress(PullProgress::Pct {
                        pct,
                        msg: format!("pulling {remote}: {}%", (frac * 100.0) as i64),
                    });
                }
            })
            .await?;
    }

    // Verify (91%): the pulled image must be an RMNG template (`rmng.image=1`) — else it isn't
    // a clone source and would just sit around unlisted (the picker filters on this label).
    on_progress(PullProgress::Step {
        step: "verify".into(),
        msg: format!("verifying {remote} is an RMNG template"),
    });
    let labels = docker.image_labels(remote).await?;
    if labels.get(crate::docker::LABEL_IMAGE).map(String::as_str) != Some("1") {
        bail!(
            "'{remote}' is not an RMNG template (missing the `{}=1` label) — build one with \
             template/Dockerfile and push it, then pull that reference",
            crate::docker::LABEL_IMAGE
        );
    }
    // A template SHOULD carry StopSignal=SIGRTMIN+3 so clones stop cleanly (gotcha #5); warn
    // if it doesn't, but don't refuse an otherwise-valid template over it.
    match docker.image_stop_signal(remote).await? {
        Some(sig) if sig == "SIGRTMIN+3" => {}
        other => tracing::warn!(
            "template {remote} StopSignal is {:?} (expected SIGRTMIN+3); clones off it may hang \
             20s on stop before SIGKILL",
            other.as_deref().unwrap_or("<unset>")
        ),
    }

    on_progress(PullProgress::Step { step: "done".into(), msg: format!("template {remote} ready") });
    Ok(remote.to_string())
}

// --- commit clone image ---------------------------------------------------------------

/// Progress step → percentage for a commit-from-clone. Matches the plan's table.
fn commit_pct(step: &str) -> Option<f64> {
    Some(match step {
        "queued" => 0.0,
        "prepare" => 15.0,
        "commit" => 40.0,
        "done" => 100.0,
        _ => return None,
    })
}

/// Commit a RUNNING clone to a new clone-source image `<name>:latest`. Steps (→ pct):
/// `queued` 0, `prepare` 15, `commit` 40, `done` 100. Returns the committed reference.
///
/// `prepare` runs `sync; truncate -s0 /etc/machine-id` inside the clone so the image doesn't
/// bake the source clone's identity. `commit` freezes the container (`pause=true`) — this can
/// take minutes for a large clone — with the `rmng.image=1` + `rmng.created-from=<source>`
/// labels. Volume mounts are excluded by `docker commit`, so the clone's inner-Docker state
/// (`/var/lib/docker`) never enters the image (gotcha #11). Logs the baked-credentials
/// warning (gotcha #10): any on-disk Claude token / secret in the clone's home travels into
/// the image.
pub async fn commit_clone_image(
    app: &App,
    container: &str,
    name: &str,
    source: &str,
    mut on_progress: impl FnMut(&str, &str),
) -> Result<String> {
    if !is_dns_label(name) {
        bail!("image name must be a DNS label (lowercase letters, digits, hyphens)");
    }
    let docker = &app.docker;
    // The name is the full image repository (no `rmng/template` namespace); Docker's default
    // `latest` tag makes the reference `<name>:latest`.
    let reference = format!("{name}:latest");

    on_progress("queued", &format!("queued commit → {reference}"));
    if docker.image_exists(&reference).await? {
        bail!("an image named '{reference}' already exists; pick another name or delete it first");
    }

    // Prepare: flush + clear machine-id in the running clone so committed images don't carry
    // the source clone's identity (a fresh id is regenerated on the next clone's first boot,
    // since clone_container also injects an empty machine-id).
    on_progress("prepare", "flushing filesystem + clearing machine-id in the clone");
    let prep_code = docker
        .exec_script(container, "sync; truncate -s0 /etc/machine-id\n", &[], &[], |_s, line| {
            tracing::debug!(target: "provision", "commit-prepare: {line}")
        })
        .await?;
    if prep_code != 0 {
        tracing::warn!("commit-prepare exited {prep_code} in {container} (non-fatal; proceeding)");
    }

    // The commit bakes whatever is on the clone's disk into the image — including any
    // on-disk Claude credentials / secrets in the clone user's home (gotcha #10).
    tracing::warn!(
        "committing {container} → {reference}: on-disk credentials (e.g. \
         ~/.claude/.credentials.json) in the clone are baked into the new image"
    );
    on_progress(
        "commit",
        "committing image (this can take minutes; on-disk credentials are baked in)",
    );
    let labels = vec![
        (crate::docker::LABEL_IMAGE.to_string(), "1".to_string()),
        (crate::docker::LABEL_CREATED_FROM.to_string(), source.to_string()),
        // `docker commit` INHERITS the parent image's labels, so a clone descended from
        // the wizard base carries `rmng.base=1` — explicitly override it or every user
        // commit wears the base badge and steals the picker preselect (found in E2E).
        (crate::docker::LABEL_BASE.to_string(), "0".to_string()),
    ];
    docker.commit(container, name, /*set_boot_config=*/ true, /*pause=*/ true, &labels).await?;

    on_progress("done", &format!("image {reference} ready"));
    Ok(reference)
}

// --- delete ---------------------------------------------------------------------------

/// Progress step → percentage for a clone delete. Matches the plan's table.
fn delete_pct(step: &str) -> Option<f64> {
    Some(match step {
        "queued" => 0.0,
        "stop" => 40.0,
        "remove" => 75.0,
        "done" => 100.0,
        _ => return None,
    })
}

/// Destroy a managed clone: `stop` (the image's `StopSignal=SIGRTMIN+3` gives systemd a
/// clean 20 s shutdown — without it every stop is a 20 s hang + SIGKILL, gotcha #5) →
/// `remove(force)` → remove the `rmng-dind-<host>` inner-Docker volume. A 404/in-use on the
/// volume is logged, not fatal (the container removal is what matters). `host_id` is both
/// the container name to stop/remove and the volume-name stem (`rmng-dind-<host_id>`).
pub async fn delete_clone(
    app: &App,
    host_id: &str,
    mut on_progress: impl FnMut(&str, &str),
) -> Result<()> {
    let docker = &app.docker;
    on_progress("queued", &format!("queued delete of {host_id}"));

    on_progress("stop", "stopping the clone (SIGRTMIN+3, up to 20s)");
    docker.stop_container(host_id).await?;

    on_progress("remove", "removing the container");
    docker.remove_container(host_id).await?;

    // The per-clone inner-Docker volumes are named + not auto-removed with the
    // container; drop them explicitly. In-use / already-gone is logged, not fatal.
    for volume in [
        crate::docker::DockerCtl::dind_volume_name(host_id),
        crate::docker::DockerCtl::ctd_volume_name(host_id),
    ] {
        match docker.remove_volume(&volume).await {
            Ok(()) => {}
            Err(e) => tracing::warn!("delete {host_id}: removing volume {volume}: {e} (non-fatal)"),
        }
    }

    on_progress("done", &format!("clone {host_id} destroyed"));
    Ok(())
}

// --- clone binaries -------------------------------------------------------------------

/// One binary the control-server installs into every clone before boot: the
/// [`crate::assets::payload`] name to resolve its bytes, and where it lands on the clone
/// filesystem. The service binaries go under `/opt/rmng/bin` (pre-created 0755 root:root by
/// `template/setup/30-user.sh`; the `systemd --user` units exec them by absolute path); the
/// `rmng` CLI goes to `/usr/local/bin` so it's on every shell's PATH (`/opt/rmng/bin` is
/// not). The template itself no longer carries any of these — the control-server is their
/// sole source, installed at create time (see [`clone_container_after_create`]). That
/// replaces the retired hash-check / hot-swap engine.
pub struct CloneBinary {
    /// Asset name passed to [`crate::assets::payload`] (`clone-daemon`, `agent-wrapper`,
    /// `rmng-cli`).
    pub payload: &'static str,
    /// The installed binary name (what the unit execs / the shell resolves).
    pub bin: &'static str,
    /// Install dir, tar-archive relative (no leading slash).
    pub dir: &'static str,
}

/// The binaries injected into every clone at create time.
pub const CLONE_BINARIES: &[CloneBinary] = &[
    CloneBinary { payload: "clone-daemon", bin: "rmng-clone-daemon", dir: "opt/rmng/bin" },
    CloneBinary { payload: "agent-wrapper", bin: "agent-wrapper", dir: "opt/rmng/bin" },
    // The fleet CLI: talks to this control-server via RMNG_CONTROL_URL (preset into every
    // clone's /etc/environment), so in-clone agents can manage the fleet with plain commands.
    CloneBinary { payload: "rmng-cli", bin: "rmng", dir: "usr/local/bin" },
];

// --- op-log pct helpers (exposed for jobs.rs step tables) -----------------------------

/// The clone/pull/commit/delete step→pct tables, exposed so `jobs.rs` maps a streamed step
/// key to the operation's coarse percentage without re-deriving it. (Monitors-apply is
/// intentionally NOT an Operation — web.rs streams its `[ct]` lines directly — so there is
/// no monitors table here.)
pub fn step_pct(kind: wire::OperationKind, step: &str) -> Option<f64> {
    match kind {
        wire::OperationKind::Clone => clone_pct(step),
        wire::OperationKind::Pull => pull_pct(step),
        wire::OperationKind::Commit => commit_pct(step),
        wire::OperationKind::Delete => delete_pct(step),
        // Self-update has no provision step table — `jobs::run_update` drives its pct directly.
        wire::OperationKind::Update => None,
    }
}

/// Discover the shared clone-socket source directory to bind into a new clone at
/// `/srv/rmng-sock`. From the self-setup env report's sock-mount discovery (the host source
/// of our own container's socket mount); empty in dev/test (the bind is then skipped).
async fn sock_source_dir(app: &App) -> String {
    // The self-setup report records the mount detail as "mounted from <src>"; parse it back
    // out. If unavailable, fall back to the socket file's parent directory from config.
    let env = app.docker.env().await;
    if let Some(src) = env.sock_mount_detail.strip_prefix("mounted from ") {
        let src = src.trim();
        if !src.is_empty() {
            return src.to_string();
        }
    }
    // Dev mode / not-yet-probed: use the directory of the configured clone socket path.
    let sock = app.config().clone_socket;
    std::path::Path::new(&sock)
        .parent()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dns_label_validation() {
        assert!(is_dns_label("pega-we-142"));
        assert!(is_dns_label("a"));
        assert!(!is_dns_label("UPPER"));
        assert!(!is_dns_label("-lead"));
        assert!(!is_dns_label("trail-"));
        assert!(!is_dns_label("has space"));
        assert!(!is_dns_label(""));
    }

    #[test]
    fn resolve_reference_canonicalizes_every_input_form() {
        const HEX_A: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        const HEX_B: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
        let img = |reference: &str, hex: &str| wire::ImageInfo {
            id: format!("sha256:{hex}"),
            reference: reference.into(),
            size_bytes: 0,
            created_at: String::new(),
            base: false,
            created_from: None,
            in_use_by: Vec::new(),
        };
        let images = vec![img("rmng/template:base", HEX_A), img("rmng/template:dev", HEX_B)];

        // Repo-tag reference → itself.
        assert_eq!(
            resolve_reference(&images, "rmng/template:base").as_deref(),
            Some("rmng/template:base")
        );
        // Full `sha256:` id → its reference.
        assert_eq!(
            resolve_reference(&images, &format!("sha256:{HEX_B}")).as_deref(),
            Some("rmng/template:dev")
        );
        // Bare 64-hex id (prefix-stripped form) → its reference.
        assert_eq!(resolve_reference(&images, HEX_A).as_deref(), Some("rmng/template:base"));
        // No match (unknown reference, unknown id, empty) → None.
        assert_eq!(resolve_reference(&images, "rmng/template:nope"), None);
        assert_eq!(resolve_reference(&images, "sha256:cccc"), None);
        assert_eq!(resolve_reference(&images, ""), None);
        // Empty image list → None.
        assert_eq!(resolve_reference(&[], "rmng/template:base"), None);
    }

    #[test]
    fn provision_uses_ssh_clone_entries_contract() {
        // Guards that provision's SSH injection targets the clone-user .ssh path (the template
        // pre-creates it 700). If this path ever changes, StrictModes will reject the key.
        if std::process::Command::new("ssh-keygen").arg("-?").output().is_err() {
            return;
        }
        let dir = std::env::temp_dir().join(format!("rmng-prov-ssh-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let e = crate::ssh::clone_ssh_tar_entries(dir.to_str().unwrap(), "c1", &["ssh-ed25519 A a".into()])
            .unwrap();
        assert!(e.iter().any(|t| t.path == "home/rmng/.ssh/authorized_keys" && t.mode == 0o600 && t.uid == 1000));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn etc_environment_conf_skips_empty_keys_and_formats() {
        let vars = vec![
            EnvVar { key: "FOO".into(), value: "1".into() },
            EnvVar { key: "".into(), value: "dropped".into() },
            EnvVar { key: "BAR".into(), value: "a b".into() },
        ];
        assert_eq!(etc_environment_conf(&vars), "FOO=1\nBAR=a b\n");
    }

    #[test]
    fn clone_etc_environment_conf_includes_base_session_and_lets_preset_override() {
        let vars = vec![
            EnvVar { key: "XDG_CURRENT_DESKTOP".into(), value: "custom".into() },
            EnvVar { key: "RMNG_CONTROL_URL".into(), value: "http://rmng-control:9000".into() },
        ];
        let body = clone_etc_environment_conf(&vars);
        assert!(body.contains("XDG_SESSION_DESKTOP=gnome\n"));
        assert!(body.contains("RMNG_CONTROL_URL=http://rmng-control:9000\n"));
        assert!(body.contains("XDG_CURRENT_DESKTOP=custom\n"));
        assert_eq!(body.matches("XDG_CURRENT_DESKTOP=").count(), 1);
    }

    #[test]
    fn preset_path_rc_none_without_path() {
        assert!(preset_path_rc("FOO=1\nBAR=2\n").is_none());
        // A PATH with only $PATH / empty tokens yields no usable dirs → None.
        assert!(preset_path_rc("PATH=$PATH\n").is_none());
        assert!(preset_path_rc("PATH=:\n").is_none());
    }

    #[test]
    fn preset_path_rc_reverses_and_prepends() {
        // Listed order a:b (a first) → reversed so each prepend leaves a in front.
        let rc = preset_path_rc("PATH=/opt/a/bin:/opt/b/bin:$PATH\n").unwrap();
        // Reversed → "/opt/b/bin" then "/opt/a/bin" in the loop dir list.
        assert!(rc.fish.contains("for d in \"/opt/b/bin\" \"/opt/a/bin\""), "fish: {}", rc.fish);
        assert!(rc.profile.contains("for d in \"/opt/b/bin\" \"/opt/a/bin\""), "profile: {}", rc.profile);
        // fish prepends with the contains-guard.
        assert!(rc.fish.contains("set -gx PATH \"$d\" $PATH"));
        // sh/bash use the case-guard prepend.
        assert!(rc.profile.contains("*) PATH=\"$d:$PATH\" ;;"));
        // bashrc block is marker-delimited (so re-provision can delete+re-append).
        assert!(rc.bashrc.starts_with("# >>> rmng-preset-path >>>\n"));
        assert!(rc.bashrc.trim_end().ends_with("# <<< rmng-preset-path <<<"));
    }

    #[test]
    fn preset_path_rc_takes_last_path_line() {
        // The LAST PATH= line wins (mirrors shell assignment order).
        let rc = preset_path_rc("PATH=/first\nFOO=1\nPATH=/second:$PATH\n").unwrap();
        assert!(rc.fish.contains("\"/second\""), "{}", rc.fish);
        assert!(!rc.fish.contains("\"/first\""), "{}", rc.fish);
    }

    #[test]
    fn step_pct_tables_match_plan() {
        use wire::OperationKind::*;
        assert_eq!(step_pct(Clone, "queued"), Some(0.0));
        assert_eq!(step_pct(Clone, "create"), Some(20.0));
        assert_eq!(step_pct(Clone, "inject"), Some(35.0));
        assert_eq!(step_pct(Clone, "start"), Some(55.0));
        assert_eq!(step_pct(Clone, "wait-ready"), Some(75.0));
        assert_eq!(step_pct(Clone, "ready"), Some(80.0));
        assert_eq!(step_pct(Clone, "monitors"), Some(85.0));
        assert_eq!(step_pct(Clone, "accounts"), Some(95.0));
        assert_eq!(step_pct(Clone, "done"), Some(100.0));

        assert_eq!(step_pct(Pull, "queued"), Some(0.0));
        assert_eq!(step_pct(Pull, "pull"), Some(2.0));
        assert_eq!(step_pct(Pull, "verify"), Some(91.0));
        assert_eq!(step_pct(Pull, "done"), Some(100.0));

        assert_eq!(step_pct(Commit, "prepare"), Some(15.0));
        assert_eq!(step_pct(Commit, "commit"), Some(40.0));

        assert_eq!(step_pct(Delete, "stop"), Some(40.0));
        assert_eq!(step_pct(Delete, "remove"), Some(75.0));

        // Unknown step keys yield None (jobs.rs leaves the pct unchanged).
        assert_eq!(step_pct(Clone, "bogus"), None);

        // The clone table must be monotonic non-decreasing in emission order, so the progress
        // bar never jumps backwards across the create → ready → monitors → accounts → done tail.
        let clone_order = [
            "queued", "create", "inject", "start", "wait-ready", "ready", "monitors", "accounts",
            "done",
        ];
        let mut prev = -1.0_f64;
        for step in clone_order {
            let pct = step_pct(Clone, step).expect("known clone step");
            assert!(pct >= prev, "clone step {step} pct {pct} < previous {prev}");
            prev = pct;
        }
    }
}
