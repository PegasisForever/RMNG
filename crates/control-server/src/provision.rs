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
//! `Operation` record + the progress→op-log plumbing and calls the flows here; credential
//! pushes go straight through the daemon (tar upload / file read), with no guest scripting.
//! These functions address a clone by its
//! container *name*, which equals the clone id (`RmngClone.managed` rows) — no container id is
//! stored anywhere.
//!
//! Guest scripts are embedded (`include_str!`) and streamed over `docker exec bash -s`:
//! [`crate::docker::DockerCtl::exec_script`]. Binaries (clone-daemon, agent-wrapper) are
//! pushed via `upload_tar`. Clone images are gen-2 preset builds (`crate::derived` builds
//! each preset's Dockerfile into a hash tag on demand); the retired gen-1 registry-template
//! pull is gone.

use anyhow::{Context, Result, bail};
use std::time::{Duration, Instant};

use wire::EnvVar;

use crate::app::App;
use crate::docker::{CLONE_USER, CreateSpec, TarEntry};

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

/// Headless clone: guarantee neither the desktop (`gnome-headless.service`), the capture daemon
/// (`rmng-clone-daemon.service`), nor the session holder (`rmng-session-holder.service`) ever
/// runs. Just removing the `default.target.wants` symlinks is not enough: `rmng-clone-daemon`
/// carries `Wants=gnome-headless.service` and `Wants=rmng-session-holder.service`, so it pulls
/// both up as runtime dependencies independent of `[Install]`, and the lingering user manager
/// starts them at first boot before this script can win the race — which is exactly why headless
/// clones were observed still running gnome-shell + the daemon on :9004.
///
/// A headless clone has no desktop, so the clean fix is to simply **delete the unit files** (real
/// files the template ships in `~/.config/systemd/user`). With no fragment on disk systemd has
/// nothing to start by any path — the `[Install]` want, the `Wants=` pull, or a manual start — and
/// there is no leftover mask symlink to reason about. `daemon-reload` then makes the (possibly
/// already-running) user manager forget the units so nothing restarts them, and `pkill` reaps
/// Headless clone: pin tmux's multi-client sizing policy, then ensure a default `main` tmux
/// session exists (idempotent). Runs as the clone user via a login shell so PATH/SHELL match an
/// interactive session. `termplane` re-creates a missing session on select, so the default session
/// is a convenience, not load-bearing.
///
/// `window-size latest` makes tmux size a session to its **most-recent** client rather than the
/// smallest: the viewer's proxy attaches as a second client, so without this a co-attached human
/// shell (or the proxy's own transient default) would clamp the grid to the smaller size — the
/// "terminal is the wrong size" bug. Written to `~/.tmux.conf` (read when the server starts) and
/// also applied live via `set-option -g` for the session created just below / any running server.
///
/// `/etc/environment` is sourced explicitly because this is the exec that **starts the tmux
/// server**, and tmux fixes its env then — every shell in that server inherits it forever. `runuser
/// -u … -- bash -lc` does NOT run PAM, so `pam_env` never loads `/etc/environment` and the server
/// would otherwise be born without `RMNG_CONTROL_URL` + the `XDG_*`/desktop vars (breaking bare
/// `rmng …` in the viewer terminal). Safe to `.` because the control-server writes that file as
/// plain unquoted `KEY=VALUE` lines (`clone_etc_environment_conf`).
fn headless_tmux_default_script() -> String {
    r#"set -e
runuser -u rmng -- bash -lc '
set -a; . /etc/environment; set +a
cat > ~/.tmux.conf <<EOF
# RMNG: the viewer proxy attaches as a second client — size to the latest (viewer) client, not the
# smallest, so a co-attached human shell never clamps the terminal grid.
set -g window-size latest
EOF
tmux has-session -t main 2>/dev/null || tmux new-session -d -s main -c /home/rmng
tmux set-option -g window-size latest 2>/dev/null || true
'
"#
    .to_string()
}

// --- pure ports -----------------------------------------------------------------------

/// A DNS label (host-id / hostname validity + path-traversal guard). Ported verbatim.
pub fn is_dns_label(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 63
        && s.bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
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
    .map(|(key, value)| EnvVar {
        key: key.to_string(),
        value: value.to_string(),
    })
    .collect()
}

/// The clone-facing control-plane environment every clone needs: `RMNG_CONTROL_URL`, the
/// control-server's own address (`docker.control_host()` — the `rmng-control` DNS alias, or the
/// gateway IP in dev mode), so the in-clone fleet CLI works without an explicit `--server`.
///
/// **Nothing here points at an inference endpoint.** Agents talk to Anthropic/OpenAI directly,
/// authenticated by the short-lived tokens the server writes into their credential files (see
/// [`crate::claude::apply_clone_token`]).
///
/// Fails when the control host does not resolve: that is a broken network config, and the
/// clone would come up brain-dead (its daemon could never phone home either) — fail fast
/// with the reason instead of booting it into a degraded URL the loop could never repair
/// (it resolves through this same function).
pub async fn control_env_vars(app: &App) -> Result<Vec<EnvVar>> {
    let cfg = app.config();
    let ev = |key: &str, value: String| EnvVar {
        key: key.to_string(),
        value,
    };
    let mut vars = Vec::new();

    // The fleet `rmng` CLI's control-server base URL, so a clone can run `rmng …`
    // without `--server` — a bare `rmng clone ls`/`rmng clone ssh`/`rmng clone create …`
    // (the latter spawning a sub clone) just works. The CLI resolves `--server` >
    // `$RMNG_CONTROL_URL` > `http://localhost:9000`; inside a clone `localhost:9000`
    // is unreachable, so this points it at the `rmng-control` alias.
    let control = app.docker.control_host().await.with_context(|| {
        "resolving the control-server host for the in-clone RMNG_CONTROL_URL"
    })?;
    vars.push(ev(
        "RMNG_CONTROL_URL",
        format!("http://{control}:{}", cfg.listen.web),
    ));
    Ok(vars)
}

/// The PER-CLONE identity env: the clone's stable bearer key, as `RMNG_PROXY_KEY`.
///
/// Named for the group proxy that first minted it, and deliberately not renamed — see
/// [`crate::clonekey`]. It is no longer an inference credential (agents authenticate with the
/// tokens the server injects into their credential files); it is how a clone proves *which*
/// clone it is, for sub-clone creation and clone↔clone SSH.
///
/// Kept OUT of [`control_env_vars`] because it is per-clone, not a shared constant — and NEVER
/// put on `RmngClone`/`state.json`/`/events` (it's a secret). Wired into the clone's
/// `/etc/environment` at create (`jobs.rs`) and on every per-clone resync (`clone_reconcile.rs`).
pub(crate) fn clone_key_env_vars(app: &App, host_id: &str) -> Vec<EnvVar> {
    vec![EnvVar {
        key: "RMNG_PROXY_KEY".into(),
        value: app.clone_keys.mint(host_id),
    }]
}

/// The preset's Linear key as `LINEAR_API_KEY` (auths the clone's `linear` MCP).
/// The key stays OUT of the preset Dockerfile (which may hold other secrets, baked
/// into the image) and is injected at runtime instead.
pub(crate) fn preset_env_vars(p: &wire::Preset) -> Vec<EnvVar> {
    if p.linear_key.is_empty() {
        return Vec::new();
    }
    vec![EnvVar {
        key: "LINEAR_API_KEY".into(),
        value: p.linear_key.clone(),
    }]
}

/// The full var list a NEW clone's `/etc/environment` is built from, in precedence order
/// (last duplicate key wins, per [`etc_environment_conf`]).
///
/// Pure so the ordering is testable without Docker. It must stay identical to the order the
/// per-clone resync composes in `clone_reconcile::reconcile_once` — control, router, preset,
/// then the group's `ANTHROPIC_MODEL` — because the two paths write the SAME file. If they
/// disagreed on precedence the value would flip on every reconcile pass.
///
/// `catalog` is the group's live `/v1/models` set (empty when it can't be read yet); `group` is
/// blank for an ungrouped clone, which then keeps Claude Code's built-in default.
pub(crate) fn compose_clone_env(
    control: Vec<EnvVar>,
    clone_key: Vec<EnvVar>,
    preset: &[EnvVar],
) -> Vec<EnvVar> {
    let mut env = control;
    env.extend(clone_key);
    env.extend(preset.iter().cloned());
    // Seeded HERE rather than left to a later sync: every other var above reaches the clone in
    // the create path's one `upload_tar` (~3 s), but `ANTHROPIC_MODEL` used to be added only by
    // a later resync — so a fresh clone spent its first seconds with no default model, running
    // Claude Code on its built-in one instead of ours.
    env.push(crate::clone_reconcile::claude_model_env_var());
    env
}

/// `/etc/environment` body: `KEY=VALUE` lines, skipping empty keys. Last duplicate key wins,
/// which lets preset/control values override the base desktop session defaults.
pub(crate) fn etc_environment_conf(vars: &[EnvVar]) -> String {
    let mut rows: Vec<(&str, &str)> = Vec::new();
    for v in vars.iter().filter(|v| !v.key.is_empty()) {
        rows.retain(|(key, _)| *key != v.key);
        rows.push((&v.key, &v.value));
    }
    rows.into_iter()
        .map(|(key, value)| format!("{key}={value}\n"))
        .collect()
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
/// Returns the `(fish_conf, profile_sh, bashrc_dropin)` tuple, or `None` when the preset
/// has no `PATH` var (or it has no usable dirs). All three are whole files (idempotent by
/// overwrite): fish + profile land in their drop-in dirs, and the bashrc one lands in
/// `/etc/bash.bashrc.d/`, which the template sources from the baked `/etc/bash.bashrc`.
fn preset_path_rc(env_text: &str) -> Option<PresetPathRc> {
    // Last PATH=… line wins (mirrors the shell taking the final assignment).
    let path_val = env_text
        .lines()
        .filter_map(|l| l.strip_prefix("PATH="))
        .next_back()?;
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
    // Whole file for the `/etc/bash.bashrc.d/` drop-in the template sources: overwrite
    // replaces any prior preset, no markers needed.
    let bashrc = format!(
        "# rmng env preset: prepend preset PATH dirs for non-login interactive bash.\n\
         for d in {dirs}; do\n  case \":$PATH:\" in\n    *\":$d:\"*) : ;;\n    *) PATH=\"$d:$PATH\" ;;\n  esac\ndone\n"
    );
    Some(PresetPathRc {
        fish,
        profile,
        bashrc,
    })
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
        "settle" => 97.0,
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
/// yet. Returns the image reference on success (`RmngClone.source`). The container *name* is the
/// The inject → start → wait-ready tail, factored out so the caller
/// can run it under a cleanup trap.
async fn clone_container_after_create(
    app: &App,
    container: &str,
    hostname: &str,
    env: &[EnvVar],
    agent_playbook: &str,
    global_prompt: &str,
    headless: bool,
    on_progress: &mut impl FnMut(&str, &str),
) -> Result<()> {
    let docker = &app.docker;
    let cfg = app.config();

    // Install the clone binaries while the container is still STOPPED, into the
    // `/opt/rmng/bin` dir the template pre-creates (30-user.sh) but leaves EMPTY — the template
    // no longer carries clone-daemon/agent-wrapper. This is the SOLE delivery path: the
    // control-server always copies its own current payloads in before boot, so a fresh clone's
    // `systemd --user` units always exec binaries that match THIS server (no runtime
    // hash-check / hot-swap engine, and none of its create-time churn). A missing payload
    // is a broken server build or an unstaged dev checkout — fail the op, never boot a
    // daemonless clone: the loop's own refresh hard-errors on the same absence, so
    // tolerating it here would only delay the failure by one pass. Dev checkouts stage
    // with the same payloads the image build COPYs (see the Dockerfile's /out stage).
    // upload_tar works on a stopped container.
    let mut bins: Vec<TarEntry> = Vec::with_capacity(CLONE_BINARIES.len());
    for b in CLONE_BINARIES {
        let data = crate::assets::payload(b.payload)
            .with_context(|| format!("clone payload '{}' is not staged", b.payload))?;
        bins.push(TarEntry {
            path: format!("{}/{}", b.dir, b.bin),
            data,
            mode: 0o755,
            uid: 0,
            gid: 0,
        });
    }
    on_progress("inject", "installing clone binaries (pre-boot)");
    // Same set the reconcile loop hashes, in the same order, so a fresh clone's stamp
    // already matches and the first pass does not re-push everything it just got.
    if !headless {
        bins.push(crate::clone_reconcile::session_holder_unit_entry());
    }
    bins.push(crate::clone_reconcile::payload_stamp_entry_for(&bins));
    // (Single upload below, after identity + content join this vec.)
    // Headless clone: mask the desktop units BEFORE first boot, while the container is
    // still stopped. A mask (symlink → /dev/null over the baked unit file) keeps the
    // session from ever starting: the wants-symlinks resolve to masked units and systemd
    // skips them. No daemon-reload, no pkill — those only existed to reap the boot race
    // the old post-start delete allowed. `agent-wrapper` stays enabled. Fails the op:
    // a "headless" clone with a live desktop has no loop backstop.
    // (The holder needs no mask: the template no longer bakes it and it is only
    // injected on headed clones, above.)
    if headless {
        on_progress("inject", "headless: masking desktop units (pre-boot)");
        docker
            .upload_symlinks(
                container,
                &[
                    (
                        "home/rmng/.config/systemd/user/gnome-headless.service".to_string(),
                        "/dev/null".to_string(),
                    ),
                    (
                        "home/rmng/.config/systemd/user/rmng-clone-daemon.service".to_string(),
                        "/dev/null".to_string(),
                    ),
                ],
            )
            .await
            .with_context(|| format!("clone {hostname}: headless unit-mask upload failed"))?;
    }

    // The clone's identity and env, written while the container is STILL STOPPED.
    //
    // This cannot wait until after the start. The template enables lingering for the clone user,
    // so `systemd --user` comes up with PID 1 and imports `/etc/environment` through the
    // `/usr/lib/environment.d/99-environment.conf` symlink within about a second. Every process
    // it goes on to start, the whole desktop session included, inherits that environment for as
    // long as the container runs. A committed image carries the `/etc/environment` of the
    // clone it was committed from, `RMNG_PROXY_KEY` and all. Writing ours seconds after the boot
    // therefore lost a race it could not win: seven clones on the production fleet ran their
    // terminals, editors and agents under the identity of their image's source clone, so
    // `rmng clone self` named that clone and a sub clone created from a terminal would have
    // nested under it. Gen-2 images are built from Dockerfiles and carry no clone identity.
    let preset_conf = clone_etc_environment_conf(env);
    let path_rc = preset_path_rc(&preset_conf);
    let mut identity: Vec<TarEntry> = vec![
        // Fresh random machine-id per clone. The template blanks it (a baked id would give
        // the whole fleet one identity), and systemd-in-docker does NOT persist a generated
        // id into an empty writable /etc/machine-id (it runs with a transient one; seen live
        // in the E2E — hostnamectl broken, id unstable across restarts). Writing a unique id
        // per clone gives stable, collision-free D-Bus/journald identity.
        TarEntry {
            path: "etc/machine-id".into(),
            data: fresh_machine_id()?,
            mode: 0o444,
            uid: 0,
            gid: 0,
        },
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
    if let Some(rc) = &path_rc {
        identity.push(TarEntry {
            path: "etc/fish/conf.d/rmng-preset-path.fish".into(),
            data: rc.fish.clone().into_bytes(),
            mode: 0o644,
            uid: 0,
            gid: 0,
        });
        identity.push(TarEntry {
            path: "etc/profile.d/rmng-preset-path.sh".into(),
            data: rc.profile.clone().into_bytes(),
            mode: 0o644,
            uid: 0,
            gid: 0,
        });
        identity.push(TarEntry {
            path: "etc/bash.bashrc.d/rmng-preset-path.sh".into(),
            data: rc.bashrc.clone().into_bytes(),
            mode: 0o644,
            uid: 0,
            gid: 0,
        });
    }
    on_progress(
        "inject",
        "injecting machine-id + preset env + PATH rc (pre-boot)",
    );
    bins.extend(identity);

    // Everything below is whole files rendered server-side, so it all joins the same
    // pre-boot tar instead of waiting for the container: the agent playbook, Codex
    // parity + stamp, SSH material + stamp, and the initial contents of the four
    // merge-owned files (the template bakes none of them, so on a fresh clone
    // merge-on-empty equals this exact content — see the `*_initial` renderers, each
    // tested against the same source as its loop merge). The loop still merges for
    // lived-in clones, operator edits, and fork-carryover; the create path never merges.
    let mut entries: Vec<TarEntry> = Vec::new();
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
    let mut codex_entries = crate::clone_reconcile::codex_parity_entries(headless, global_prompt);
    codex_entries.push(crate::clone_reconcile::codex_parity_stamp_entry_for(
        &codex_entries,
    ));
    entries.append(&mut codex_entries);
    // SSH: the clone's stable host key + the current authorized_keys, so `ssh -J … rmng@<id>`
    // works the moment the clone is up. The template pre-created ~rmng/.ssh (700) and ships
    // no host keys, so these land with the right owner/perms. Stamp withheld on failure and
    // the loop's ensure_ssh_ready retries — the designed retry, not a mask.
    // `authorized_keys` is the only `~/.ssh` file provisioned; a config baked into the source
    // image stays exactly as the image left it (the server no longer reads or writes it).
    match crate::ssh::clone_ssh_tar_entries(&cfg.data_dir, hostname, &cfg.ssh.authorized_keys) {
        Ok(mut ssh_entries) => {
            ssh_entries.push(crate::clone_reconcile::ssh_stamp_entry());
            entries.append(&mut ssh_entries);
        }
        Err(e) => tracing::warn!("clone {hostname}: ssh material skipped: {e}"),
    }

    // Initial contents of the merge-owned files (modes match what each loop merge sets,
    // so a matching stamp means the loop never rewrites them). See the `*_initial`
    // renderers for the merge-on-empty equivalence.
    let linear_key = crate::clone_reconcile::env_value(env, "LINEAR_API_KEY");
    for (path, data, mode) in [
        (
            format!("home/{CLONE_USER}/.claude.json"),
            crate::clone_reconcile::claude_mcp_initial(headless).into_bytes(),
            0o600,
        ),
        (
            format!("home/{CLONE_USER}/.cursor/mcp.json"),
            crate::clone_reconcile::cursor_mcp_initial(headless, &linear_key).into_bytes(),
            0o600,
        ),
        (
            format!("home/{CLONE_USER}/.codex/config.toml"),
            crate::clone_reconcile::codex_mcp_toml(headless).into_bytes(),
            0o600,
        ),
        (
            format!("home/{CLONE_USER}/.claude/settings.json"),
            crate::clone_reconcile::claude_settings_initial().into_bytes(),
            0o644,
        ),
        (
            format!("home/{CLONE_USER}/.cursor/hooks.json"),
            crate::clone_reconcile::cursor_hooks_initial().into_bytes(),
            0o644,
        ),
    ] {
        entries.push(TarEntry {
            path,
            data,
            mode,
            uid: CLONE_UID,
            gid: CLONE_GID,
        });
    }
    entries.push(crate::clone_reconcile::claude_mcp_stamp_entry_for(
        headless,
    ));
    entries.push(
        crate::clone_reconcile::cursor_mcp_stamp_entry_for(headless, &linear_key),
    );
    entries.push(crate::clone_reconcile::codex_mcp_stamp_entry_for(headless));
    // The activity probe file rides the tar; its registration is rendered above as initial
    // content. Stamp withheld only if this whole upload fails (then the loop retries).
    entries.extend(crate::clone_reconcile::rmng_hook_entries());
    entries.push(crate::clone_reconcile::claude_hook_stamp_entry());

    // The single pre-boot tar: binaries + identity + content + stamps in one daemon
    // Post-start injects are down to the headless tmux session and wait-ready: every file
    // (including the bashrc drop-in) already landed in the single pre-boot tar.
    on_progress("inject", "injecting clone payload: binaries + identity + config (pre-boot)");
    bins.extend(entries);
    docker.upload_tar(container, bins).await?;

    // systemd PID 1 comes up, and the user manager with it, now reading the env written above.
    on_progress("inject", "starting container");
    docker.start_container(container).await?;

    // Headless clone: there is no clone-daemon, so a media `Hello` never arrives — don't wait
    // for one. Start the default `main` tmux session (idempotent; the viewer shows it as the
    // first tab and `termplane` self-heals a missing session on select) and report ready.
    if headless {
        on_progress(
            "wait-ready",
            "headless clone — starting default tmux session",
        );
        let code = docker
            .exec_script(
                container,
                &headless_tmux_default_script(),
                &[],
                &[],
                |_stream, line| {
                    tracing::debug!(target: "provision", "headless-tmux: {line}");
                },
            )
            .await
            .unwrap_or(0);
        if code != 0 {
            tracing::warn!(
                "clone {hostname}: default tmux session start exited {code} (non-fatal)"
            );
        }
        on_progress("ready", &format!("headless clone {hostname} up"));
        return Ok(());
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
            let tail = if logs.trim().is_empty() {
                String::new()
            } else {
                format!("\n{logs}")
            };
            bail!("clone {hostname} exited before its daemon registered; last logs:{tail}");
        }
        tokio::time::sleep(WAIT_READY_POLL).await;
    }
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
/// `remove(force)` → remove the `rmng-dind-<clone>` inner-Docker volume. A 404/in-use on the
/// volume is logged, not fatal (the container removal is what matters). `host_id` is both
/// the container name to stop/remove and the volume-name stem (`rmng-dind-<host_id>`).
///
/// Gen-2 tail (no-op on gen-1 rows, which carry no `dataset`): destroy the home dataset
/// (kept, with a warning, when fork clones still reference it), destroy the origin
/// snapshot it was cloned from when nothing references it anymore, and remove the base
/// image tag when no remaining clone row references it.
pub async fn delete_clone(
    app: &App,
    host_id: &str,
    mut on_progress: impl FnMut(&str, &str),
) -> Result<()> {
    let docker = &app.docker;
    on_progress("queued", &format!("queued delete of {host_id}"));

    // Unpauses first when the clone is archived: a stop signal sent to frozen processes is
    // one nobody can handle, so the daemon would wait out the full timeout and then kill.
    on_progress("stop", "stopping the clone (SIGRTMIN+3, up to 20s)");
    docker.stop_even_if_paused(host_id).await?;

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

    // Gen-2 tail: the row still exists (jobs.rs removes it after this returns), so the
    // dataset + base tag are readable here. Everything below is best-effort cleanup —
    // the container removal above is what matters. Drop the home link with the row,
    // unmount the overlay (it pins the dataset busy), and then destroy the dataset;
    // the dataset itself lives or dies by the ZFS destroy below.
    crate::homes::remove_link(app, host_id).await;
    crate::home_overlay::teardown_merged(crate::zfs::HOMES_DIR, host_id);
    if let Some(row) = gen2_row(app, host_id) {
        if row.dataset.is_some() {
            on_progress("remove", "destroying the home dataset");
            let parent = homes_parent(app);
            let origin = dataset_origin(&parent, host_id);
            match crate::zfs::destroy(&parent, host_id, false) {
                Ok(()) => {
                    if let Some(snap) = origin.filter(|s| s != "-") {
                        if let Err(e) = crate::zfs::destroy_snapshot_if_unreferenced(&parent, &snap)
                        {
                            tracing::warn!(
                                "delete {host_id}: keeping origin snapshot {snap}: {e} (non-fatal)"
                            );
                        }
                    }
                }
                Err(e) => tracing::warn!(
                    "delete {host_id}: keeping home dataset ({e}); fork clones may still \
                     reference it (non-fatal)"
                ),
            }
        }
        if let Some(tag) = row.base_tag {
            purge_image_if_unused(app, host_id, &tag).await;
        }
    }
    Ok(())
}

// --- gen-2 clones ---------------------------------------------------------------------

/// Create-time env keys that belong in a gen-2 clone's `/etc/environment`. Static preset
/// vars live in the profile Dockerfile lines (stage 3); only per-clone dynamic keys are
/// injected here. `ANTHROPIC_MODEL` is seeded at create (same value the reconciler
/// enforces) so fresh clones have a model before the first reconcile pass.
const GEN2_DYNAMIC_KEYS: [&str; 3] = ["RMNG_CONTROL_URL", "RMNG_PROXY_KEY", "ANTHROPIC_MODEL"];

/// Filter create-time env down to the dynamic keys a gen-2 clone injects.
fn gen2_dynamic_env(env: &[EnvVar]) -> Vec<EnvVar> {
    env.iter()
        .filter(|v| GEN2_DYNAMIC_KEYS.contains(&v.key.as_str()))
        .cloned()
        .collect()
}

/// Full Dockerfile text of the named preset (config). Unknown, unnamed, or empty ⇒
/// the default base Dockerfile. Every create/fork/migrate resolves its image from
/// this — never from a caller-supplied base.
pub(crate) fn preset_dockerfile(app: &App, preset_name: Option<&str>) -> String {
    let name = preset_name.unwrap_or("").trim();
    let text = if name.is_empty() {
        String::new()
    } else {
        app.config()
            .presets
            .iter()
            .find(|p| p.name == name)
            .map(|p| p.dockerfile.clone())
            .unwrap_or_default()
    };
    let text = text.trim().to_string();
    if text.is_empty() {
        "FROM pegasis0/rmng-template:latest".to_string()
    } else {
        text
    }
}

/// How a gen-2 create sources the home dataset.
pub enum HomeSource {
    /// Fresh `zfs create` for a new clone / migration.
    Create,
    /// `zfs clone` from a template seed snapshot (template carries default home content).
    CloneFromSnapshot(String),
    /// The dataset already exists (fork cloned it, rebase/migration created it) — use it.
    Reuse,
}

/// The state row for a clone id, if it exists.
fn gen2_row(app: &App, id: &str) -> Option<wire::RmngClone> {
    app.store.get().hosts.iter().find(|h| h.id == id).cloned()
}

/// Configured ZFS homes parent (`docker.homes_parent`, default `tank/rmng/homes`).
/// Read fresh per call — immediate-apply, never cached.
fn homes_parent(app: &App) -> String {
    app.config().docker.homes_parent.clone()
}

/// `zfs get origin` for a clone's dataset: the snapshot it was cloned from, if any.
/// `None` for fresh datasets (origin `-`) and on any error. Provision-local (one `zfs`
/// invocation, no destroy) so the wrapper module needs no read API.
fn dataset_origin(parent: &str, clone_id: &str) -> Option<String> {
    let ds = crate::zfs::dataset_name(parent, clone_id);
    let out = std::process::Command::new("zfs")
        .args(["get", "-H", "-o", "value", "origin", &ds])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let v = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if v.is_empty() || v == "-" {
        None
    } else {
        Some(v)
    }
}

/// `remove_image(tag)` when no remaining clone row (other than `except_id`) references it.
/// Best-effort: logs, never fails the caller (a 409 means still in use — keep it).
async fn purge_image_if_unused(app: &App, except_id: &str, tag: &str) {
    let used = app
        .store
        .get()
        .hosts
        .iter()
        .any(|h| h.id != except_id && h.base_tag.as_deref() == Some(tag));
    if used {
        return;
    }
    if let Err(e) = app.docker.remove_image(tag).await {
        tracing::warn!("keeping image {tag}: {e} (non-fatal)");
    }
}

/// Remove a half-built gen-2 clone: container + dind/ctd volumes, and the dataset when
/// this call created it (never on [`HomeSource::Reuse`] — that dataset holds a live home).
async fn destroy_half_built_clone(app: &App, hostname: &str, created_dataset: bool) {
    let docker = &app.docker;
    docker.remove_container(hostname).await.ok();
    docker
        .remove_volume(&crate::docker::DockerCtl::dind_volume_name(hostname))
        .await
        .ok();
    docker
        .remove_volume(&crate::docker::DockerCtl::ctd_volume_name(hostname))
        .await
        .ok();
    if created_dataset {
        // Unmount first: the merged view pins the dataset busy.
        crate::home_overlay::teardown_merged(crate::zfs::HOMES_DIR, hostname);
        if let Err(e) = crate::zfs::destroy(&homes_parent(app), hostname, false) {
            tracing::warn!("cleanup for {hostname}: keeping dataset: {e} (non-fatal)");
        }
    }
}

/// Create + start a gen-2 clone: home on its own dataset, image = preset Dockerfile.
///
/// Steps: ensure the preset image (lazy hash-tag build of the preset's full Dockerfile
/// text) → `zfs create` (or clone from the template seed snapshot) → `docker create`
/// with the dataset bind → identity/dynamic-env inject → ensure the empty
/// `/home/rmng/clones` mountpoint → start → wait-ready (the [`clone_container_after_create`]
/// tail). Returns the resolved tag for the caller to record as `base_tag`. On failure
/// the container, volumes, AND a dataset this call created are destroyed; a reused
/// dataset is never touched.
///
/// `env` is the composed create-time list (control keys, `LINEAR_API_KEY`, dynamic
/// per-clone keys); everything static lives in the Dockerfile, never here.
#[allow(clippy::too_many_arguments)]
pub async fn clone_container_gen2(
    app: &App,
    dockerfile: &str,
    hostname: &str,
    home: HomeSource,
    env: &[EnvVar],
    agent_playbook: &str,
    global_prompt: &str,
    headless: bool,
    mut on_progress: impl FnMut(&str, &str),
) -> Result<String> {
    if !is_dns_label(hostname) {
        bail!("clone hostname must be a DNS label (lowercase letters, digits, hyphens)");
    }
    let _docker = &app.docker;
    // The preset Dockerfile decides the image: same text twice means one build, and the
    // tag is recorded below as `base_tag`. No label gate: FROM may name any image.
    let tag = crate::derived::ensure_image(app, dockerfile, false, &mut on_progress).await?;
    if tag.is_empty() {
        bail!("a Dockerfile is required for a gen-2 clone");
    }
    clone_container_gen2_from_tag(
        app,
        &tag,
        hostname,
        home,
        env,
        agent_playbook,
        global_prompt,
        headless,
        &mut on_progress,
    )
    .await
}

/// Create + start a gen-2 clone on an EXPLICIT local image tag (rebase + rollback).
/// Same tail as [`clone_container_gen2`] minus the Dockerfile build: the tag must
/// already exist locally (the preset rebuild button warms it).
#[allow(clippy::too_many_arguments)]
pub async fn clone_container_gen2_from_tag(
    app: &App,
    tag: &str,
    hostname: &str,
    home: HomeSource,
    env: &[EnvVar],
    agent_playbook: &str,
    global_prompt: &str,
    headless: bool,
    mut on_progress: impl FnMut(&str, &str),
) -> Result<String> {
    let docker = &app.docker;
    let tag = tag.to_string();
    let cfg = app.config();

    on_progress("queued", &format!("queued gen-2 clone {hostname}"));
    if !docker.image_exists(&tag).await? {
        bail!("base image '{tag}' does not exist");
    }
    docker.ensure_network().await?;

    on_progress("create", &format!("creating home dataset for {hostname}"));
    let parent = cfg.docker.homes_parent.clone();
    let created_dataset = match home {
        HomeSource::Create => {
            crate::zfs::create(&parent, hostname)?;
            true
        }
        HomeSource::CloneFromSnapshot(ref snap) => {
            crate::zfs::clone_dataset(&parent, snap, hostname)?;
            true
        }
        HomeSource::Reuse => false,
    };

    on_progress("create", &format!("mounting home overlay for {hostname}"));
    // The dataset holds upper/ + work/; the skeleton (template home for this image)
    // is shared. The merged view binds at /home/rmng, so the template home layer
    // shows through on fresh datasets and user files persist in the upper.
    let dataset = std::path::PathBuf::from(crate::zfs::dataset_dir(hostname));
    let digest = crate::home_overlay::ensure_skeleton(app, &tag).await?;
    // NB: mount paths come from HOMES_DIR (the mountpoint), not `parent` (the ZFS
    // dataset name) — the daemon rejects relative bind sources.
    let merged = crate::home_overlay::merged_dir(crate::zfs::HOMES_DIR, hostname);
    debug_assert!(
        merged.is_absolute(),
        "overlay merged view must be an absolute bind source"
    );
    crate::home_overlay::ensure_mounted(&dataset, &digest, &merged).await?;

    on_progress("create", &format!("creating container {hostname}"));
    let spec = CreateSpec {
        name: hostname.to_string(),
        image: tag.clone(),
        hostname: hostname.to_string(),
        // Clone env lives only in `/etc/environment`. Gen-2 images are built from Dockerfiles
        // and carry no stale `Config.Env`, so nothing needs cancelling here.
        env: Vec::new(),
        cpus: cfg.docker.clone_cpus,
        memory_mb: cfg.docker.clone_memory_mb,
        sock_source: sock_source_dir(app).await,
        home_dir: Some(merged.to_string_lossy().into_owned()),
        homes_dir: crate::zfs::HOMES_DIR.to_string(),
        // Absolute host path (the pool lives under the homes parent, which the daemon
        // sees through the shared homes bind); ensured at server startup.
        shared_dir: crate::shared::shared_host_dir(),
    };
    let container = match docker.create_clone_container(&spec).await {
        Ok(c) => c,
        Err(e) => {
            destroy_half_built_clone(app, hostname, created_dataset).await;
            return Err(e);
        }
    };

    // Dynamic keys only: static preset env moved to the profile Dockerfile lines.
    let dyn_env = gen2_dynamic_env(env);
    match clone_container_after_create(
        app,
        &container,
        hostname,
        &dyn_env,
        agent_playbook,
        global_prompt,
        headless,
        &mut on_progress,
    )
    .await
    {
        Ok(()) => {
            crate::buildinfra::apply_to_clone(app, &container).await;
            Ok(tag)
        }
        Err(e) => {
            tracing::warn!("gen-2 clone {hostname} failed after create; cleaning up: {e}");
            destroy_half_built_clone(app, hostname, created_dataset).await;
            Err(e)
        }
    }
}

/// Fork a gen-2 clone: snapshot the source home, clone it for the new id, create from the
/// source's recorded base tag. The source keeps running. Overlay drift is silently dropped
/// (fork copies the dataset only). Returns the new clone's tag.
#[allow(clippy::too_many_arguments)]
pub async fn fork_clone(
    app: &App,
    source_id: &str,
    new_id: &str,
    env: &[EnvVar],
    agent_playbook: &str,
    global_prompt: &str,
    headless: bool,
    // Effective preset (payload override wins, else the source's). The fork image
    // ALWAYS follows this preset: its static env feeds derivation from the source's
    // recorded base, so a different preset builds a new tag (cached after the first
    // build) while the same preset reuses the source tag with zero rebuild.
    preset_name: Option<&str>,
    mut on_progress: impl FnMut(&str, &str),
) -> Result<String> {
    if !is_dns_label(new_id) {
        bail!("clone hostname must be a DNS label (lowercase letters, digits, hyphens)");
    }
    let _src =
        gen2_row(app, source_id).ok_or_else(|| anyhow::anyhow!("unknown clone '{source_id}'"))?;
    let dockerfile = preset_dockerfile(app, preset_name);
    // The fork's image comes from the TARGET preset's Dockerfile (built lazily inside
    // `clone_container_gen2`); the source contributes only its home dataset, snapshotted
    // and cloned below. Same preset reuses the source tag with zero rebuild, because the
    // text hashes the same.

    on_progress("snapshot", &format!("snapshotting {source_id}"));
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let snap = crate::zfs::snapshot(
        &homes_parent(app),
        source_id,
        &format!("fork-{new_id}-{ts}"),
    )?;
    on_progress("clone-home", &format!("cloning home for {new_id}"));
    if let Err(e) = crate::zfs::clone_dataset(&homes_parent(app), &snap, new_id) {
        let _ = crate::zfs::destroy_snapshot_if_unreferenced(&homes_parent(app), &snap);
        return Err(e);
    }
    match clone_container_gen2(
        app,
        &dockerfile,
        new_id,
        HomeSource::Reuse,
        env,
        agent_playbook,
        global_prompt,
        headless,
        &mut on_progress,
    )
    .await
    {
        Ok(tag) => Ok(tag),
        Err(e) => {
            destroy_half_built_clone(app, new_id, true).await;
            let _ = crate::zfs::destroy_snapshot_if_unreferenced(&homes_parent(app), &snap);
            Err(e)
        }
    }
}

/// Progress sink for rollback recreates (nothing to report to a failed op).
fn noop_progress(_step: &str, _msg: &str) {}

/// Rebase a gen-2 clone onto a new base tag, keeping the SAME dataset and id. The old
/// container cannot stay (name == id), so: record old tag → stop → remove → create from
/// the new tag → wait-ready. Healthy: purge the old tag when unused. Failed: auto-recreate
/// from the old tag on the same dataset, then report the rebase error. Returns the new tag.
#[allow(clippy::too_many_arguments)]
pub async fn rebase_clone(
    app: &App,
    host_id: &str,
    new_tag: &str,
    env: &[EnvVar],
    agent_playbook: &str,
    global_prompt: &str,
    headless: bool,
    mut on_progress: impl FnMut(&str, &str),
) -> Result<String> {
    let row = gen2_row(app, host_id);
    let old_tag = row
        .as_ref()
        .and_then(|r| r.base_tag.clone())
        .ok_or_else(|| anyhow::anyhow!("clone '{host_id}' has no recorded base tag"))?;
    // Both tags run as-is: the new one was warmed by the preset rebuild button, the old
    // one is still local (it purges only when unused, and this clone uses it). FROM may
    // name any image — no label gate.
    for t in [new_tag, &old_tag] {
        if !app.docker.image_exists(t).await? {
            bail!("image '{t}' is not present locally — prebuild it first");
        }
    }

    on_progress("stop", &format!("stopping {host_id} for rebase"));
    app.docker.stop_even_if_paused(host_id).await?;
    app.docker.remove_container(host_id).await?;

    match clone_container_gen2_from_tag(
        app,
        new_tag,
        host_id,
        HomeSource::Reuse,
        env,
        agent_playbook,
        global_prompt,
        headless,
        &mut on_progress,
    )
    .await
    {
        Ok(tag) => {
            purge_image_if_unused(app, host_id, &old_tag).await;
            Ok(tag)
        }
        Err(e) => {
            on_progress(
                "rollback",
                &format!("rebase failed; recreating from {old_tag}"),
            );
            if let Err(rb) = clone_container_gen2_from_tag(
                app,
                &old_tag,
                host_id,
                HomeSource::Reuse,
                env,
                agent_playbook,
                global_prompt,
                headless,
                noop_progress,
            )
            .await
            {
                anyhow::bail!(
                    "rebase to {new_tag} failed: {e:#}; rollback to {old_tag} also failed: {rb:#}"
                );
            }
            anyhow::bail!("rebase to {new_tag} failed: {e:#} (rolled back to {old_tag})");
        }
    }
}

/// One clone's migration step (stage-3 boot loop calls this per gen-1 row, one at a time):
/// `zfs create` → copy `/home/rmng` out of the STOPPED old container into the dataset →
/// remove the old container (fresh dind/ctd volumes on recreate) → create the gen-2
/// container from the base tag with fresh identity/dynamic env → stop it (migrated clones
/// start with the fleet, not during the window). Returns the copied bytes for the report.
///
/// Account token re-push is NOT done here: stage 3 reads the stored selections and calls
/// `crate::claude::push_account_to_clone` / `crate::codex::push_account_to_clone` after
/// the fleet starts (those need running clones).
#[allow(clippy::too_many_arguments)]
pub async fn migrate_one(
    app: &App,
    host_id: &str,
    base_tag: &str,
    env: &[EnvVar],
    agent_playbook: &str,
    global_prompt: &str,
    headless: bool,
    mut on_progress: impl FnMut(&str, &str),
) -> Result<MigrateReport> {
    if !is_dns_label(host_id) {
        bail!("clone hostname must be a DNS label (lowercase letters, digits, hyphens)");
    }
    on_progress("queued", &format!("queued migration of {host_id}"));
    match migrate_one_inner(
        app,
        host_id,
        base_tag,
        env,
        agent_playbook,
        global_prompt,
        headless,
        &mut on_progress,
    )
    .await
    {
        Ok(report) => Ok(report),
        Err(e) => {
            // One-shot window under a whole-LXC backup: leave no half-built dataset.
            crate::home_overlay::teardown_merged(crate::zfs::HOMES_DIR, host_id);
            let _ = crate::zfs::destroy(&homes_parent(app), host_id, false);
            Err(e)
        }
    }
}

/// Copied home bytes + resolved derived tag, for the per-clone migration report.
pub struct MigrateReport {
    pub bytes: u64,
    pub tag: String,
}

#[allow(clippy::too_many_arguments)]
async fn migrate_one_inner(
    app: &App,
    host_id: &str,
    base_tag: &str,
    env: &[EnvVar],
    agent_playbook: &str,
    global_prompt: &str,
    headless: bool,
    on_progress: &mut impl FnMut(&str, &str),
) -> Result<MigrateReport> {
    on_progress("create", "creating the home dataset");
    crate::zfs::create(&homes_parent(app), host_id)?;

    on_progress("copy", "copying /home/rmng out of the old container");
    let tar = app.docker.download_home_tar(host_id, "/home/rmng").await?;
    let bytes = tar.len() as u64;
    // Into the overlay upper: the merged view then shows old home over the new base.
    let dataset = std::path::PathBuf::from(crate::zfs::dataset_dir(host_id));
    crate::home_overlay::ensure_layout(&dataset)?;
    let upper = crate::home_overlay::upper_dir(&dataset);
    extract_home_tar(&tar, &upper.to_string_lossy())?;

    on_progress("recreate", "removing the old container");
    app.docker.remove_container(host_id).await?;
    for volume in [
        crate::docker::DockerCtl::dind_volume_name(host_id),
        crate::docker::DockerCtl::ctd_volume_name(host_id),
    ] {
        if let Err(e) = app.docker.remove_volume(&volume).await {
            tracing::warn!("migrate {host_id}: removing volume {volume}: {e} (non-fatal)");
        }
    }

    on_progress("recreate", "creating the gen-2 container (stopped)");
    // Migration builds from the row preset's Dockerfile (or the default base when the
    // row names none): the old `base_tag` arg is retired, kept only for signature compat.
    let _ = base_tag;
    let dockerfile = preset_dockerfile(
        app,
        gen2_row(app, host_id)
            .as_ref()
            .and_then(|r| r.preset_name.as_deref()),
    );
    let tag = clone_container_gen2(
        app,
        &dockerfile,
        host_id,
        HomeSource::Reuse,
        env,
        agent_playbook,
        global_prompt,
        headless,
        &mut *on_progress,
    )
    .await?;

    on_progress("stop", "stopping the migrated clone");
    app.docker.stop_even_if_paused(host_id).await?;
    on_progress("done", &format!("clone {host_id} migrated ({bytes} bytes)"));
    Ok(MigrateReport { bytes, tag })
}

/// Extract a daemon `download_from_container` tar of `/home/rmng` into the dataset dir.
/// The archive roots every entry under the basename (`rmng/...`), so the first component
/// is stripped. Runs as CT root, preserving the archived owners/modes. Entries escaping
/// the destination (`..`, absolute) are refused.
fn extract_home_tar(tar_bytes: &[u8], dest: &str) -> Result<()> {
    let mut archive = tar::Archive::new(tar_bytes);
    archive.set_preserve_permissions(true);
    for entry in archive.entries()? {
        let mut entry = entry?;
        let path = entry.path()?.to_path_buf();
        let mut comps = path.components();
        comps.next(); // strip the `rmng/` top-level dir
        let rel: std::path::PathBuf = comps.collect();
        if rel.as_os_str().is_empty() {
            continue;
        }
        if rel.components().any(|c| {
            matches!(
                c,
                std::path::Component::ParentDir
                    | std::path::Component::RootDir
                    | std::path::Component::Prefix(_)
            )
        }) {
            anyhow::bail!("refusing to extract {rel:?} outside the dataset");
        }
        entry.unpack(std::path::Path::new(dest).join(rel))?;
    }
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
    CloneBinary {
        payload: "clone-daemon",
        bin: "rmng-clone-daemon",
        dir: "opt/rmng/bin",
    },
    CloneBinary {
        payload: "agent-wrapper",
        bin: "agent-wrapper",
        dir: "opt/rmng/bin",
    },
    // Fleet management CLI, installed on every clone for explicit operator use.
    CloneBinary {
        payload: "rmng-cli",
        bin: "rmng",
        dir: "usr/local/bin",
    },
];

// --- archive --------------------------------------------------------------------------

fn archive_pct(step: &str) -> Option<f64> {
    Some(match step {
        "queued" => 0.0,
        "stop" => 75.0,
        "done" => 100.0,
        _ => return None,
    })
}

fn unarchive_pct(step: &str) -> Option<f64> {
    Some(match step {
        "queued" => 0.0,
        "start" => 60.0,
        "render" => 80.0,
        "done" => 100.0,
        _ => return None,
    })
}

/// Progress step → percentage for a gen-2 one-shot migration. Matches the `migrate_one`
/// step keys.
fn migrate_pct(step: &str) -> Option<f64> {
    Some(match step {
        "queued" => 0.0,
        "create" => 10.0,
        "copy" => 30.0,
        "recreate" => 60.0,
        "stop" => 90.0,
        "done" => 100.0,
        _ => return None,
    })
}

// --- op-log pct helpers (exposed for jobs.rs step tables) -----------------------------

/// The clone/pull/commit/delete/archive step→pct tables, exposed so `jobs.rs` maps a streamed step
/// key to the operation's coarse percentage without re-deriving it. (Monitors-apply is
/// intentionally NOT an Operation — web.rs streams its `[ct]` lines directly — so there is
/// no monitors table here.)
/// Progress step → percentage for a commit-from-clone, kept so old `Commit` ops in state
/// still render. No new commit ops can be filed: the commit path is deleted.
fn commit_pct(step: &str) -> Option<f64> {
    Some(match step {
        "queued" => 0.0,
        "prepare" => 15.0,
        "commit" => 40.0,
        "done" => 100.0,
        _ => return None,
    })
}
/// Progress step → percentage for the retired gen-1 template pull. Kept so old persisted
/// `Pull` operations still render; no new ones are created.
fn pull_pct(step: &str) -> Option<f64> {
    Some(match step {
        "queued" => 0.0,
        "pull" => 2.0,
        "verify" => 91.0,
        "done" => 100.0,
        _ => return None,
    })
}

pub fn step_pct(kind: wire::OperationKind, step: &str) -> Option<f64> {
    match kind {
        wire::OperationKind::Clone => clone_pct(step),
        wire::OperationKind::Pull => pull_pct(step),
        wire::OperationKind::Commit => commit_pct(step),
        wire::OperationKind::Delete => delete_pct(step),
        wire::OperationKind::Archive => archive_pct(step),
        wire::OperationKind::Unarchive => unarchive_pct(step),
        // Self-update has no provision step table — `jobs::run_update` drives its pct directly.
        wire::OperationKind::Update => None,
        wire::OperationKind::Migrate => migrate_pct(step),
        // Prebuild drives its own pct (build streaming has no coarse table).
        wire::OperationKind::Prebuild => None,
    }
}

/// Discover the shared clone-socket source directory to bind into a new clone at
/// `/srv/rmng-sock`. From the self-setup env report's sock-mount discovery (the clone source
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
    fn gen2_dynamic_env_keeps_model_key() {
        // ANTHROPIC_MODEL is seeded at create so fresh clones have a model before the
        // first reconcile pass; static keys stay out (they bake into the image).
        let env = |key: &str| wire::EnvVar {
            key: key.into(),
            value: "v".into(),
        };
        let got: Vec<String> = gen2_dynamic_env(&[
            env("RMNG_CONTROL_URL"),
            env("RMNG_PROXY_KEY"),
            env("ANTHROPIC_MODEL"),
            env("SOME_STATIC"),
        ])
        .into_iter()
        .map(|v| v.key)
        .collect();
        assert_eq!(
            got,
            vec!["RMNG_CONTROL_URL", "RMNG_PROXY_KEY", "ANTHROPIC_MODEL"]
        );
    }

    #[test]
    fn provision_uses_ssh_clone_entries_contract() {
        // Guards that provision's SSH injection targets the clone-user .ssh path (the template
        // pre-creates it 700). If this path ever changes, StrictModes will reject the key.
        if std::process::Command::new("ssh-keygen")
            .arg("-?")
            .output()
            .is_err()
        {
            return;
        }
        let dir = std::env::temp_dir().join(format!("rmng-prov-ssh-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let e = crate::ssh::clone_ssh_tar_entries(
            dir.to_str().unwrap(),
            "c1",
            &["ssh-ed25519 A a".into()],
        )
        .unwrap();
        assert!(e.iter().any(|t| t.path == "home/rmng/.ssh/authorized_keys"
            && t.mode == 0o600
            && t.uid == 1000));
        // The create path provisions exactly ONE file under ~/.ssh. It must not write `config`
        // (a managed `Host *` there leaked clone-local User/IdentityFile onto every destination),
        // and no PRIVATE key may land in ~/.ssh — gcr-ssh-agent adopts anything it finds there
        // into the login keyring and then crashes on it, wedging all ssh/git in the clone.
        let home: Vec<&str> = e
            .iter()
            .map(|t| t.path.as_str())
            .filter(|p| p.starts_with("home/"))
            .collect();
        assert_eq!(
            home,
            vec!["home/rmng/.ssh/authorized_keys"],
            "authorized_keys is the only ~/.ssh file the create path writes"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A clone's `~/.ssh/config` (and any client key in there) is the USER's. The create path must
    /// not read it, write it, or run a migration over it — a source image that carries one keeps it
    /// verbatim. Guards the source so the removed fleet-key logic cannot quietly come back.
    #[test]
    fn create_path_never_touches_the_clone_ssh_config() {
        let src = include_str!("provision.rs");
        let body = &src[..src.find("mod tests").unwrap_or(src.len())];
        for banned in [
            "read_clone_ssh_config",
            "merge_ssh_config",
            "fleet_public_key",
            "ssh_prepare_script",
        ] {
            assert!(
                !body.contains(banned),
                "{banned} must not be called from the create path any more"
            );
        }
    }

    #[test]
    fn etc_environment_conf_skips_empty_keys_and_formats() {
        let vars = vec![
            EnvVar {
                key: "FOO".into(),
                value: "1".into(),
            },
            EnvVar {
                key: "".into(),
                value: "dropped".into(),
            },
            EnvVar {
                key: "BAR".into(),
                value: "a b".into(),
            },
        ];
        assert_eq!(etc_environment_conf(&vars), "FOO=1\nBAR=a b\n");
    }

    #[test]
    fn clone_etc_environment_conf_includes_base_session_and_lets_preset_override() {
        let vars = vec![
            EnvVar {
                key: "XDG_CURRENT_DESKTOP".into(),
                value: "custom".into(),
            },
            EnvVar {
                key: "RMNG_CONTROL_URL".into(),
                value: "http://rmng-control:9000".into(),
            },
        ];
        let body = clone_etc_environment_conf(&vars);
        assert!(body.contains("XDG_SESSION_DESKTOP=gnome\n"));
        assert!(body.contains("RMNG_CONTROL_URL=http://rmng-control:9000\n"));
        assert!(body.contains("XDG_CURRENT_DESKTOP=custom\n"));
        assert_eq!(body.matches("XDG_CURRENT_DESKTOP=").count(), 1);
    }

    /// The tmux server a headless clone boots with is the environment every agent typed into a pane
    /// inherits, fixed for the life of the server. `/etc/environment` is sourced into it, so the
    /// retired keys have to be unset AFTER that source, not before.
    #[test]
    fn headless_tmux_script_sources_etc_environment_and_starts_a_session() {
        let script = headless_tmux_default_script();
        assert!(script.contains(". /etc/environment"), "{script}");
        // The session is still created, and the window-size option still applied.
        assert!(
            script.contains("tmux new-session -d -s main -c /home/rmng"),
            "{script}"
        );
        assert!(script.contains("window-size latest"), "{script}");
        // Gen-2 images carry no stale Config.Env, so no key cancellations remain.
        assert!(!script.contains("unset "), "stale cancellation:\n{script}");
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
        assert!(
            rc.fish.contains("for d in \"/opt/b/bin\" \"/opt/a/bin\""),
            "fish: {}",
            rc.fish
        );
        assert!(
            rc.profile
                .contains("for d in \"/opt/b/bin\" \"/opt/a/bin\""),
            "profile: {}",
            rc.profile
        );
        // fish prepends with the contains-guard.
        assert!(rc.fish.contains("set -gx PATH \"$d\" $PATH"));
        // sh/bash use the case-guard prepend.
        assert!(rc.profile.contains("*) PATH=\"$d:$PATH\" ;;"));
        // bashrc drop-in is a whole file (overwrite-idempotent, no markers).
        assert!(rc.bashrc.contains("for d in"));
        assert!(!rc.bashrc.contains(">>>"));
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
            "queued",
            "create",
            "inject",
            "start",
            "wait-ready",
            "ready",
            "monitors",
            "accounts",
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
