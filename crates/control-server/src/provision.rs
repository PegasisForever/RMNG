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
//! drives credential ops via [`run_clone_op`]; `web.rs`/`mcp.rs` call [`redeploy_clone`] +
//! [`apply_monitors`]. These functions take the `container` id (the clone's Docker id, from
//! `Host.container`), not a Proxmox ctid.
//!
//! Guest scripts are embedded (`include_str!`) and streamed over `docker exec bash -s`:
//! [`crate::docker::DockerCtl::exec_script`]. Binaries (clone-daemon, agent-wrapper, patched
//! gnome-shell deb) are pushed via `upload_tar`.

use anyhow::{Result, bail};
use std::time::{Duration, Instant};

use wire::{AppConfig, EnvVar};

use crate::app::App;
use crate::docker::{CreateSpec, TarEntry, CLONE_USER};

const PROVISION_SCRIPT: &str = include_str!("../scripts/provision-clone.sh");
const APPLY_MONITORS_SCRIPT: &str = include_str!("../scripts/apply-monitors.sh");
const IMPORT_SCRIPT: &str = include_str!("../scripts/claude-import.sh");

/// The clone user's uid/gid inside every image (created uid 1000 by `provision-clone.sh`).
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

/// The monitor layout as the clone-daemon's `RMNG_MONITORS` env: CSV of `WxH+X+Y[*]`
/// (position in the unified desktop, `*` = primary). Ported verbatim from `orchestrate.rs`.
pub fn monitors_csv(cfg: &AppConfig) -> String {
    cfg.effective_monitors()
        .iter()
        .map(|m| format!("{}x{}+{}+{}{}", m.width, m.height, m.x, m.y, if m.primary { "*" } else { "" }))
        .collect::<Vec<_>>()
        .join(",")
}

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

/// Standard base64 (no line wrapping). Ported verbatim from `orchestrate.rs` — used to
/// pass the credentials JSON to `claude-import.sh apply`.
pub fn b64_encode(bytes: &[u8]) -> String {
    const A: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity((bytes.len() + 2) / 3 * 4);
    for chunk in bytes.chunks(3) {
        let b = [chunk[0], *chunk.get(1).unwrap_or(&0), *chunk.get(2).unwrap_or(&0)];
        let n = ((b[0] as u32) << 16) | ((b[1] as u32) << 8) | b[2] as u32;
        out.push(A[(n >> 18 & 63) as usize] as char);
        out.push(A[(n >> 12 & 63) as usize] as char);
        out.push(if chunk.len() > 1 { A[(n >> 6 & 63) as usize] as char } else { '=' });
        out.push(if chunk.len() > 2 { A[(n & 63) as usize] as char } else { '=' });
    }
    out
}

/// Resolve a caller-supplied image — a repo-tag reference (`rmng/template:<name>`), a full
/// `sha256:…` id, or a bare 64-hex id — to the **canonical** [`wire::ImageInfo`] `reference`
/// of the matching clone-source image. `None` when nothing in the listed clone sources
/// matches (i.e. the input isn't a labeled `rmng.image=1` image at all).
///
/// This is what keeps `Host.source` canonical regardless of the caller's input form: the
/// in-use accounting (web.rs `fill_in_use_by`) and the images-delete 409 guard both compare
/// `Host.source == ImageInfo.reference`, so a host created from an id form must still record
/// the reference — otherwise its base image would show as unused and be deletable under
/// live clones.
pub fn resolve_reference(images: &[wire::ImageInfo], input: &str) -> Option<String> {
    images
        .iter()
        .find(|i| i.reference == input || i.id == input || i.id.strip_prefix("sha256:") == Some(input))
        .map(|i| i.reference.clone())
}

/// The clone→control-server + detector-inference env every clone needs, as
/// `environment.d`-style `KEY=VALUE` [`EnvVar`]s. Points the detector's feedback + agent
/// `set_state` MCP at THIS control-server and the detector's vision model at the configured
/// inference server. Ported from `orchestrate.rs::control_env_vars`, with the Proxmox
/// UDP-`advertise_ip` trick replaced by `docker.control_ip()` (the static `.2` address on
/// the rmng bridge — see `docker.rs`). Empty control URLs (with a warning) if the control
/// IP can't be resolved, so clones fall back to the compiled detector defaults.
pub async fn control_env_vars(app: &App) -> Vec<EnvVar> {
    let cfg = app.config();
    let ev = |key: &str, value: String| EnvVar { key: key.to_string(), value };
    let mut vars = Vec::new();
    match app.docker.control_ip().await {
        Ok(ip) => {
            vars.push(ev("RMNG_CONTROL_URL", format!("http://{ip}:{}", cfg.listen.web)));
            vars.push(ev("AGENT_CONTROL_MCP_URL", format!("http://{ip}:{}", cfg.listen.clone_mcp)));
        }
        Err(e) => tracing::warn!(
            "control_env_vars: could not resolve the control-server IP ({e}); \
             clones fall back to the compiled detector defaults"
        ),
    }
    let infer = cfg.detector_inference_url.trim();
    if !infer.is_empty() {
        vars.push(ev("RMNG_INFERENCE_URL", infer.to_string()));
    }
    vars
}

/// `~/.config/environment.d/30-rmng-preset.conf` body: `KEY=VALUE` lines for the control +
/// preset env, skipping keys with an empty name. This is the per-clone preset env, read by
/// `systemd --user` (environment.d) into the session + every user unit at boot.
fn preset_env_conf(vars: &[EnvVar]) -> String {
    vars.iter().filter(|v| !v.key.is_empty()).map(|v| format!("{}={}\n", v.key, v.value)).collect()
}

/// Shell-rc files that prepend a preset's `PATH` dirs for interactive shells. The Rust port
/// of the deleted `clone.sh::write_preset_path_rc`.
///
/// A preset `PATH` needs more than environment.d: interactive shells rewrite `PATH` on
/// startup (login bash re-runs `/etc/profile`, which hard-resets it; fish rebuilds `$PATH`),
/// so the inherited value reaches GUI apps but not a terminal — every OTHER preset var
/// survives. Mirror the template's `rmng-local-bin` blocks: prepend the preset's dirs inside
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

/// Progress step → percentage for a clone-container create. Matches the plan's table.
fn clone_pct(step: &str) -> Option<f64> {
    Some(match step {
        "queued" => 0.0,
        "allocate" => 8.0,
        "create" => 20.0,
        "inject" => 35.0,
        "start" => 55.0,
        "wait-ready" => 75.0,
        "done" => 100.0,
        _ => return None,
    })
}

/// Create + start a clone container from an `rmng.image=1` source image, injecting its
/// identity/preset/PATH files, and wait for its daemon to register.
///
/// Steps (→ pct): `queued` 0, `allocate` 8, `create` 20, `inject` 35, `start` 55,
/// `wait-ready` 75, `done` 100. Returns `(container_id, ip, reference)` on success — the new
/// container id (`Host.container`), its static IP (`Host.host`), and the **canonical**
/// image reference (`Host.source`; see [`resolve_reference`] — the caller may have passed an
/// id form, but state must always record the reference for in-use accounting). On any
/// failure BEFORE readiness, a cleanup trap removes the created container + its per-clone
/// dind volume so a retry isn't blocked by a stale same-named container (gotcha #7).
///
/// `image` must be a clone source (`rmng.image=1`); `env` is the resolved control + preset
/// env (control URLs first so a preset can still override). One `upload_tar` injects: a
/// fresh random `/etc/machine-id` (always — a committed image carries a baked one), the preset
/// `30-rmng-preset.conf` (uid 1000, home entries), and — when the preset sets `PATH` — the
/// fish/profile preset-PATH rc (root-owned `/etc`). After start, when the preset set `PATH`,
/// the bashrc marker block is appended via an exec (a plain tar can't append). wait-ready
/// polls the mediaplane for the daemon's `Hello{clone_id == hostname}` ≤ 90 s; a timeout with
/// the container still running SUCCEEDS with a warning in the op log; a dead container FAILS
/// with a `docker logs` tail folded into the op log.
pub async fn clone_container(
    app: &App,
    image: &str,
    hostname: &str,
    env: &[EnvVar],
    mut on_progress: impl FnMut(&str, &str),
) -> Result<(String, String, String)> {
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

    // The rmng bridge is lazy; make sure it's up before allocating an IP on it.
    docker.ensure_network().await?;

    // allocate → create → inject/start, with a bounded retry on the IP-allocation race:
    // two concurrent clones can both pick the lowest free IP (allocate_ip reads state +
    // a network inspect, neither of which sees the sibling until it STARTS), and Docker
    // rejects the loser at container start with 403 "Address already in use" (seen live
    // in the E2E). On that specific failure re-allocate — the winner is attached by then,
    // so the next inspect skips its IP — and retry with a fresh container.
    const IP_RACE_ATTEMPTS: usize = 3;
    for attempt in 1..=IP_RACE_ATTEMPTS {
        // Allocate the lowest free clone IP, reserving IPs already claimed in state.json
        // (they may not yet appear in the live network inspect).
        on_progress("allocate", "allocating a static clone IP");
        let reserved: Vec<String> =
            app.store.get().hosts.iter().map(|h| h.host.clone()).filter(|s| !s.is_empty()).collect();
        let ip = docker.allocate_ip(&reserved).await?;
        on_progress("allocate", &format!("clone IP {ip}"));

        // Create the container (name == host id) from the CANONICAL reference (equivalent
        // to the caller's input — same image — but keeps `docker ps`'s Image column
        // readable). A stale same-named container 409s here — the daemon message is
        // surfaced verbatim (gotcha #7).
        on_progress("create", &format!("creating container {hostname} at {ip}"));
        let spec = CreateSpec {
            name: hostname.to_string(),
            image: reference.clone(),
            ip: ip.clone(),
            hostname: hostname.to_string(),
            env: env.iter().filter(|v| !v.key.is_empty()).map(|v| (v.key.clone(), v.value.clone())).collect(),
            cpus: cfg.docker.clone_cpus,
            memory_mb: cfg.docker.clone_memory_mb,
            sock_source: sock_source_dir(app).await,
        };
        let container = match docker.create_clone_container(&spec).await {
            Ok(id) => id,
            Err(e) => bail!("{e}"),
        };

        // From here on, a failure must tear the half-built clone down. Run the rest under
        // a guard that removes the container + its dind volumes on any early return.
        let result = clone_container_after_create(app, &container, hostname, env, &mut on_progress).await;
        match result {
            Ok(()) => return Ok((container, ip, reference)),
            Err(e) => {
                tracing::warn!("clone {hostname} failed after create; cleaning up: {e}");
                docker.remove_container(&container).await.ok();
                docker.remove_volume(&crate::docker::DockerCtl::dind_volume_name(hostname)).await.ok();
                docker.remove_volume(&crate::docker::DockerCtl::ctd_volume_name(hostname)).await.ok();
                if attempt < IP_RACE_ATTEMPTS && is_address_in_use(&e) {
                    let note = format!("IP {ip} lost to a concurrent clone; retrying (attempt {attempt}/{IP_RACE_ATTEMPTS})");
                    tracing::warn!("clone {hostname}: {note}");
                    on_progress("allocate", &note);
                    continue;
                }
                return Err(e);
            }
        }
    }
    // Every loop iteration returns (success, non-race error, or final-attempt error).
    unreachable!("clone_container retry loop always returns")
}

/// True when a container-start failure is Docker's static-IP collision ("failed to set up
/// container networking: Address already in use") — the one failure class worth an
/// allocate-again retry.
fn is_address_in_use(e: &anyhow::Error) -> bool {
    format!("{e:#}").contains("Address already in use")
}

/// The inject → start → wait-ready tail of [`clone_container`], factored out so the caller
/// can run it under a cleanup trap.
async fn clone_container_after_create(
    app: &App,
    container: &str,
    hostname: &str,
    env: &[EnvVar],
    on_progress: &mut impl FnMut(&str, &str),
) -> Result<()> {
    let docker = &app.docker;

    // Container must be running to upload_tar into it (the daemon extracts into the live
    // rootfs). systemd PID 1 comes up; we inject the identity/preset files, then the units
    // pick them up (environment.d is read by `systemd --user` under linger at boot).
    on_progress("inject", "starting container to inject identity + preset");
    docker.start_container(container).await?;

    // Build the single upload_tar: machine-id (always), preset env conf + PATH rc.
    let preset_conf = preset_env_conf(env);
    let path_rc = preset_path_rc(&preset_conf);
    let mut entries: Vec<TarEntry> = vec![
        // Fresh random machine-id: a committed image bakes one in, and systemd-in-docker
        // does NOT persist a generated id into an empty writable /etc/machine-id (it runs
        // with a transient one; seen live in the E2E — hostnamectl broken, id unstable
        // across restarts). Writing a unique id per clone gives stable, collision-free
        // D-Bus/journald identity; commit truncates it again, so images never carry it.
        TarEntry { path: "etc/machine-id".into(), data: fresh_machine_id()?, mode: 0o444, uid: 0, gid: 0 },
        // Per-clone preset env (control URLs + preset vars), owned by the clone user.
        TarEntry {
            path: format!("home/{CLONE_USER}/.config/environment.d/30-rmng-preset.conf"),
            data: preset_conf.clone().into_bytes(),
            mode: 0o644,
            uid: CLONE_UID,
            gid: CLONE_GID,
        },
    ];
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
            on_progress("done", &format!("clone {hostname} up + registered"));
            return Ok(());
        }
        if Instant::now() >= deadline {
            // Timeout: distinguish "still booting" (container alive) from "died".
            if docker.is_running(container).await.unwrap_or(false) {
                // Succeed with a warning: the clone is up but its daemon hasn't registered
                // yet (headless GNOME + user units can be slow on first boot).
                on_progress(
                    "done",
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

// --- bootstrap base image -------------------------------------------------------------

/// Progress step → percentage for a base-image bootstrap. Matches the plan's table (the
/// `provision` step's 18–88 span is filled by [`bootstrap_base_image`] itself as the guest
/// script's `[ct]` lines stream in).
fn bootstrap_pct(step: &str) -> Option<f64> {
    Some(match step {
        "queued" => 0.0,
        "pull" => 10.0,
        "create" => 12.0,
        "inject" => 15.0,
        "provision" => 18.0,
        "cleanup" => 90.0,
        "stop" => 92.0,
        "commit" => 94.0,
        "done" => 100.0,
        _ => return None,
    })
}

/// Build the wizard base image `rmng/template:<name>` from `ubuntu:26.04` (the from-zero
/// path). Steps (→ pct): `queued` 0, `pull` 2–10, `create` 12, `inject` 15, `provision`
/// 18–88 (the guest script's `[ct]` lines advance sub-progress + set the op message),
/// `cleanup` 90, `stop` 92, `commit` 94, `done` 100. Returns the committed image reference.
///
/// The build container is a privileged `sleep infinity` on the DEFAULT bridge (so NAT + apt
/// work — no dependency on the rmng network existing yet). `provision-clone.sh` runs under
/// `docker exec bash -s` with `DEBIAN_FRONTEND=noninteractive` + `SYSTEMD_OFFLINE=1` (its
/// `systemctl enable/mask/set-default` are pure symlink ops with no bus). The embedded
/// clone-daemon / agent-wrapper / patched gnome-shell deb are pushed in first (skip-missing
/// WARN, as before). Then cleanup (apt clean, truncate machine-id, rm staged files), stop
/// (t=2 — `sleep` ignores TERM, so KILL is harmless), commit with `set_boot_config` + the
/// `rmng.image=1`/`rmng.base=1` labels, and finally rm the build container. Rejects an
/// already-taken `rmng/template:<name>` tag up front (gotcha #8 lineage stays clean).
pub async fn bootstrap_base_image(
    app: &App,
    name: &str,
    mut on_progress: impl FnMut(&str, &str),
) -> Result<String> {
    if !is_dns_label(name) {
        bail!("image name must be a DNS label (lowercase letters, digits, hyphens)");
    }
    let cfg = app.config();
    let docker = &app.docker;
    let reference = format!("{}:{}", crate::docker::IMAGE_REPO, name);

    on_progress("queued", &format!("queued base-image build {reference}"));

    // Reject a taken tag up front — a commit would overwrite an existing image otherwise.
    if docker.image_exists(&reference).await? {
        bail!("an image named '{reference}' already exists; pick another name or delete it first");
    }

    // Pull the fixed base OS (2–10%; the daemon error, e.g. a Docker Hub rate limit, is
    // surfaced verbatim — gotcha #9).
    on_progress("pull", &format!("pulling {}", crate::docker::BASE_DOCKER_IMAGE));
    docker
        .pull_image(crate::docker::BASE_DOCKER_IMAGE, |_step, msg| on_progress("pull", msg))
        .await?;

    // Build container: privileged sleep-infinity on the default bridge, started so we can
    // exec into it. Name it after the image so `docker ps` is readable; if a stale one
    // exists, tear it down first (a previous failed build).
    let build_name = format!("rmng-build-{name}");
    docker.remove_container(&build_name).await.ok();
    on_progress("create", &format!("creating build container {build_name}"));
    let build = docker.create_build_container(&build_name, crate::docker::BASE_DOCKER_IMAGE).await?;

    // Everything after create must clean the build container up on any failure.
    let result = bootstrap_after_create(app, &cfg, &build, name, &reference, &mut on_progress).await;
    // Always remove the build container (success committed already; failure leaves nothing).
    docker.remove_container(&build).await.ok();
    result.map(|_| reference)
}

/// The inject → provision → commit tail of [`bootstrap_base_image`], factored out so the
/// caller always removes the build container afterward (success or failure).
async fn bootstrap_after_create(
    app: &App,
    cfg: &AppConfig,
    build: &str,
    name: &str,
    reference: &str,
    on_progress: &mut impl FnMut(&str, &str),
) -> Result<()> {
    let docker = &app.docker;

    // Push the provision script + the payload binaries into the build container. Match
    // `provision-clone.sh`'s expected paths: /root/rmng-clone-daemon, /root/agent-wrapper,
    // /root/gnome-shell-patched.deb. Skip-missing WARN behavior (a dev checkout may lack
    // staged payloads), same as the old `stage_binary`.
    on_progress("inject", "pushing provision assets into the build container");
    let mut entries: Vec<TarEntry> = Vec::new();
    for (payload_name, dest) in [
        ("clone-daemon", "root/rmng-clone-daemon"),
        ("agent-wrapper", "root/agent-wrapper"),
        ("gnome-shell.deb", "root/gnome-shell-patched.deb"),
    ] {
        match crate::assets::payload(payload_name) {
            Some(bytes) => {
                entries.push(TarEntry { path: dest.into(), data: bytes, mode: 0o755, uid: 0, gid: 0 })
            }
            None => tracing::warn!("{payload_name} payload missing; skipping (provision falls back)"),
        }
    }
    if !entries.is_empty() {
        docker.upload_tar(build, entries).await?;
    }

    // Assert the clone user resolves to uid 1000 once during bootstrap. The user is created
    // by provision-clone.sh; we assert AFTER provisioning below (it doesn't exist yet here).

    // Provision (18–88%): stream provision-clone.sh with the noninteractive + offline
    // systemd env. Its `    [ct] …` lines advance sub-progress + set the op message. Args:
    // <username> <password> <monitors> <clone_socket>.
    on_progress("provision", "provisioning the base image (headless GNOME + toolbox)");
    let mons = monitors_csv(cfg);
    let args: Vec<String> = vec![
        CLONE_USER.to_string(),
        CLONE_USER.to_string(), // password == username on the base image
        mons,
        cfg.clone_socket.clone(),
    ];
    let env = vec![
        ("DEBIAN_FRONTEND".to_string(), "noninteractive".to_string()),
        ("SYSTEMD_OFFLINE".to_string(), "1".to_string()),
    ];
    // Sub-progress: nudge the pct from 18 toward 88 as `[ct]` lines arrive, so the bar moves
    // during the long apt/toolbox phase without knowing the step count ahead of time.
    let mut ct_lines = 0u32;
    let code = docker
        .exec_script(build, PROVISION_SCRIPT, &env, &args, |stream, line| {
            // The guest's step lines look like `    [ct] <message>`; strip the marker for
            // the op message, keep raw lines on stderr as plain log context.
            if let Some(msg) = line.trim_start().strip_prefix("[ct] ") {
                ct_lines += 1;
                // Asymptotic approach to 88 from 18: never overshoots, always moves.
                let pct = 18.0 + (88.0 - 18.0) * (1.0 - 0.94_f64.powi(ct_lines as i32));
                on_progress("provision", &format!("{pct:.0}% {msg}"));
            } else if stream == "err" && !line.trim().is_empty() {
                on_progress("provision", line);
            }
        })
        .await?;
    if code != 0 {
        bail!("provision-clone.sh failed in the build container (exit {code})");
    }

    // Assert uid: id -u rmng == 1000 (gotcha #2 — the tar owner mapping relies on it).
    let (id_code, id_out) = docker.exec_capture(build, &["id", "-u", CLONE_USER]).await?;
    let uid = id_out.trim();
    if id_code != 0 || uid != "1000" {
        bail!(
            "clone user '{CLONE_USER}' resolved to uid '{uid}' (expected 1000); the tar owner \
             mapping for home/{CLONE_USER}/** would be wrong"
        );
    }

    // Cleanup (90%): apt clean, truncate machine-id, rm the staged binaries so they aren't
    // baked into the image (provision-clone.sh installed them to /opt/rmng/bin already).
    on_progress("cleanup", "cleaning apt cache + machine-id + staged files");
    let cleanup = "set -e\n\
        apt-get clean >/dev/null 2>&1 || true\n\
        rm -rf /var/lib/apt/lists/* /tmp/* /var/tmp/* 2>/dev/null || true\n\
        : > /etc/machine-id 2>/dev/null || true\n\
        rm -f /root/rmng-clone-daemon /root/agent-wrapper /root/gnome-shell-patched.deb 2>/dev/null || true\n";
    let clean_code = docker
        .exec_script(build, cleanup, &[], &[], |_s, line| tracing::debug!(target: "provision", "cleanup: {line}"))
        .await?;
    if clean_code != 0 {
        tracing::warn!("bootstrap cleanup exited {clean_code} (non-fatal)");
    }

    // Stop (92%): t=2 — the build container's PID 1 is `sleep`, which ignores SIGTERM, so
    // the daemon SIGKILLs it after 2s (harmless — nothing to flush).
    on_progress("stop", "stopping the build container");
    docker.stop_container(build).await?;

    // Commit (94%): rmng/template:<name> with the boot config baked (StopSignal etc.) + the
    // image/base labels.
    on_progress("commit", &format!("committing {reference}"));
    let labels = vec![
        (crate::docker::LABEL_IMAGE.to_string(), "1".to_string()),
        (crate::docker::LABEL_BASE.to_string(), "1".to_string()),
    ];
    docker.commit(build, name, /*set_boot_config=*/ true, /*pause=*/ false, &labels).await?;

    on_progress("done", &format!("base image {reference} ready"));
    Ok(())
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

/// Commit a RUNNING clone to a new clone-source image `rmng/template:<name>`. Steps (→ pct):
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
    let reference = format!("{}:{}", crate::docker::IMAGE_REPO, name);

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
/// volume is logged, not fatal (the container removal is what matters). `host_id` names the
/// dind volume (`rmng-dind-<host_id>`); `container` is the Docker id to stop/remove.
pub async fn delete_clone(
    app: &App,
    container: &str,
    host_id: &str,
    mut on_progress: impl FnMut(&str, &str),
) -> Result<()> {
    let docker = &app.docker;
    on_progress("queued", &format!("queued delete of {host_id}"));

    on_progress("stop", "stopping the clone (SIGRTMIN+3, up to 20s)");
    docker.stop_container(container).await?;

    on_progress("remove", "removing the container");
    docker.remove_container(container).await?;

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

// --- redeploy -------------------------------------------------------------------------

/// The clone user's `systemd --user` units, in the order redeploy touches them.
const REDEPLOY_UNITS: &[(&str, &str)] = &[
    ("clone-daemon", "rmng-clone-daemon.service"),
    ("agent-wrapper", "agent-wrapper.service"),
];

/// Hot-swap a running clone's `clone-daemon` (+ `agent-wrapper` unless `daemon_only`)
/// binaries WITHOUT reprovisioning. Per unit: `systemctl --user stop` (exec'd as the clone
/// user with its `XDG_RUNTIME_DIR`/`DBUS_SESSION_BUS_ADDRESS` — linger guarantees the user
/// manager is up) → `upload_tar` the payload binary to **`/opt/rmng/bin/<name>`** (the units
/// exec from there; the old `redeploy.sh` pushed to `$HOME`, a latent path bug this fixes) →
/// `reset-failed` + `start`. No username arg — the clone user (`CLONE_USER`) is compiled in
/// (fixes mcp.rs's stray `"pega"`). Skips a unit whose payload is missing (WARN).
pub async fn redeploy_clone(
    app: &App,
    container: &str,
    daemon_only: bool,
    mut on_progress: impl FnMut(&str, &str),
) -> Result<()> {
    let docker = &app.docker;

    // Resolve the clone user's uid inside the container for the XDG/DBUS env.
    let (uid_code, uid_out) = docker.exec_capture(container, &["id", "-u", CLONE_USER]).await?;
    let uid = uid_out.trim().to_string();
    if uid_code != 0 || uid.is_empty() {
        bail!("could not resolve uid of '{CLONE_USER}' in {container}: {}", uid_out.trim());
    }

    for (payload_name, unit) in REDEPLOY_UNITS {
        if daemon_only && *payload_name == "agent-wrapper" {
            continue;
        }
        let Some(bytes) = crate::assets::payload(payload_name) else {
            tracing::warn!("redeploy: {payload_name} payload missing; skipping");
            on_progress("skip", &format!("{payload_name} payload missing; skipped"));
            continue;
        };

        // The on-disk binary name in /opt/rmng/bin (provision-clone.sh installs these names).
        let bin_name = match *payload_name {
            "clone-daemon" => "rmng-clone-daemon",
            other => other,
        };

        on_progress("stop", &format!("stopping {unit}"));
        run_user_systemctl(app, container, &uid, &["stop", unit]).await.ok();

        on_progress("push", &format!("pushing {bin_name} → /opt/rmng/bin"));
        docker
            .upload_tar(
                container,
                vec![TarEntry {
                    path: format!("opt/rmng/bin/{bin_name}"),
                    data: bytes,
                    mode: 0o755,
                    uid: 0,
                    gid: 0,
                }],
            )
            .await?;

        on_progress("start", &format!("starting {unit}"));
        run_user_systemctl(app, container, &uid, &["reset-failed", unit]).await.ok();
        run_user_systemctl(app, container, &uid, &["start", unit]).await?;
    }

    on_progress("done", "redeploy complete");
    Ok(())
}

/// Run `systemctl --user <args>` inside a clone as [`CLONE_USER`] with the user-manager env
/// (linger guarantees the manager is up). Uses `runuser` + the `XDG_RUNTIME_DIR`/session-bus
/// address, matching `apply-monitors.sh`'s `uctl`.
async fn run_user_systemctl(app: &App, container: &str, uid: &str, args: &[&str]) -> Result<()> {
    let mut cmd: Vec<&str> = vec![
        "runuser",
        "-u",
        CLONE_USER,
        "--",
        "env",
    ];
    let xdg = format!("XDG_RUNTIME_DIR=/run/user/{uid}");
    let dbus = format!("DBUS_SESSION_BUS_ADDRESS=unix:path=/run/user/{uid}/bus");
    cmd.push(&xdg);
    cmd.push(&dbus);
    cmd.push("systemctl");
    cmd.push("--user");
    cmd.extend_from_slice(args);
    let (code, out) = app.docker.exec_capture(container, &cmd).await?;
    if code != 0 {
        bail!("systemctl --user {} failed (exit {code}): {}", args.join(" "), out.trim());
    }
    Ok(())
}

// --- monitors -------------------------------------------------------------------------

/// Apply the configured monitor layout to a RUNNING clone without reprovisioning: streams
/// [`APPLY_MONITORS_SCRIPT`] over `docker exec bash -s` as root with args `<user> <csv>`. Its
/// `[ct]` lines flow to the progress callback (as `apply` steps).
pub async fn apply_monitors(
    app: &App,
    container: &str,
    mut on_progress: impl FnMut(&str, &str),
) -> Result<()> {
    let cfg = app.config();
    let csv = monitors_csv(&cfg);
    on_progress("queued", "applying monitor layout");
    let args = vec![CLONE_USER.to_string(), csv];
    let code = app
        .docker
        .exec_script(container, APPLY_MONITORS_SCRIPT, &[], &args, |_stream, line| {
            let msg = line.trim_start().strip_prefix("[ct] ").unwrap_or(line);
            if !msg.trim().is_empty() {
                on_progress("apply", msg);
            }
        })
        .await?;
    if code != 0 {
        bail!("apply-monitors.sh failed in {container} (exit {code})");
    }
    on_progress("done", "monitor layout applied");
    Ok(())
}

// --- claude-import backend ------------------------------------------------------------

/// Run one [`claude-import.sh`] op (`status`|`read`|`clear`|`apply`) inside clone `container`
/// via `docker exec bash -s`, returning its raw stdout+stderr. `extra` are extra positional
/// args (e.g. the base64 credentials for `apply`). This is `claude.rs`'s backend (Task 6
/// delegates its token flows to it), replacing the retired SSH `run_clone_op`.
///
/// Script args: `<user> <op> [b64]`. `status` never fails (stderr merged in the script);
/// the others surface a non-zero exit as an error.
pub async fn run_clone_op(app: &App, container: &str, op: &str, extra: &[&str]) -> Result<String> {
    let mut args: Vec<String> = vec![CLONE_USER.to_string(), op.to_string()];
    args.extend(extra.iter().map(|s| s.to_string()));

    let mut out = String::new();
    let code = app
        .docker
        .exec_script(container, IMPORT_SCRIPT, &[], &args, |_stream, line| {
            out.push_str(line);
            out.push('\n');
        })
        .await?;

    if code == 0 {
        Ok(out)
    } else {
        bail!("clone op '{op}' failed in {container} (exit {code}): {}", out.trim());
    }
}

// --- op-log pct helpers (exposed for jobs.rs step tables) -----------------------------

/// The clone/bootstrap/commit/delete step→pct tables, exposed so `jobs.rs` maps a streamed
/// step key to the operation's coarse percentage without re-deriving it. (Monitors-apply is
/// intentionally NOT an Operation — web.rs streams its `[ct]` lines directly — so there is
/// no monitors table here.)
pub fn step_pct(kind: wire::OperationKind, step: &str) -> Option<f64> {
    match kind {
        wire::OperationKind::Clone => clone_pct(step),
        wire::OperationKind::Bootstrap => bootstrap_pct(step),
        wire::OperationKind::Commit => commit_pct(step),
        wire::OperationKind::Delete => delete_pct(step),
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
    use wire::MonitorSpec;

    fn cfg_with_monitors(mons: Vec<MonitorSpec>) -> AppConfig {
        let mut c = AppConfig::default();
        c.monitors = mons;
        c
    }

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
    fn monitors_csv_format() {
        let cfg = cfg_with_monitors(vec![
            MonitorSpec { width: 2560, height: 1440, x: 2560, y: 0, primary: true },
            MonitorSpec { width: 1920, height: 1080, x: 0, y: 0, primary: false },
        ]);
        assert_eq!(monitors_csv(&cfg), "2560x1440+2560+0*,1920x1080+0+0");
    }

    #[test]
    fn monitors_csv_falls_back_to_default() {
        // Empty config → effective_monitors' dual-1440p default.
        let cfg = cfg_with_monitors(vec![]);
        assert_eq!(monitors_csv(&cfg), "2560x1440+2560+0*,2560x1440+0+0");
    }

    #[test]
    fn b64_parity() {
        // Round-trip parity with the classic base64 alphabet + padding.
        assert_eq!(b64_encode(b""), "");
        assert_eq!(b64_encode(b"f"), "Zg==");
        assert_eq!(b64_encode(b"fo"), "Zm8=");
        assert_eq!(b64_encode(b"foo"), "Zm9v");
        assert_eq!(b64_encode(b"foob"), "Zm9vYg==");
        assert_eq!(b64_encode(b"fooba"), "Zm9vYmE=");
        assert_eq!(b64_encode(b"foobar"), "Zm9vYmFy");
        // A KEY=VALUE line, as the credentials-json path uses it.
        assert_eq!(b64_encode(b"PATH=/x/y"), "UEFUSD0veC95");
    }

    #[test]
    fn preset_env_conf_skips_empty_keys_and_formats() {
        let vars = vec![
            EnvVar { key: "FOO".into(), value: "1".into() },
            EnvVar { key: "".into(), value: "dropped".into() },
            EnvVar { key: "BAR".into(), value: "a b".into() },
        ];
        assert_eq!(preset_env_conf(&vars), "FOO=1\nBAR=a b\n");
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
        assert_eq!(step_pct(Clone, "allocate"), Some(8.0));
        assert_eq!(step_pct(Clone, "create"), Some(20.0));
        assert_eq!(step_pct(Clone, "inject"), Some(35.0));
        assert_eq!(step_pct(Clone, "start"), Some(55.0));
        assert_eq!(step_pct(Clone, "wait-ready"), Some(75.0));
        assert_eq!(step_pct(Clone, "done"), Some(100.0));

        assert_eq!(step_pct(Bootstrap, "pull"), Some(10.0));
        assert_eq!(step_pct(Bootstrap, "create"), Some(12.0));
        assert_eq!(step_pct(Bootstrap, "inject"), Some(15.0));
        assert_eq!(step_pct(Bootstrap, "provision"), Some(18.0));
        assert_eq!(step_pct(Bootstrap, "cleanup"), Some(90.0));
        assert_eq!(step_pct(Bootstrap, "stop"), Some(92.0));
        assert_eq!(step_pct(Bootstrap, "commit"), Some(94.0));
        assert_eq!(step_pct(Bootstrap, "done"), Some(100.0));

        assert_eq!(step_pct(Commit, "prepare"), Some(15.0));
        assert_eq!(step_pct(Commit, "commit"), Some(40.0));

        assert_eq!(step_pct(Delete, "stop"), Some(40.0));
        assert_eq!(step_pct(Delete, "remove"), Some(75.0));

        // Unknown step keys yield None (jobs.rs leaves the pct unchanged).
        assert_eq!(step_pct(Clone, "bogus"), None);
    }
}
