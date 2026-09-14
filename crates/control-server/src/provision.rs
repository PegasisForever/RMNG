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
//! pushes go straight into the clone's live home on the local filesystem, with no guest scripting.
//! These functions address a clone by its
//! container *name*, which equals the clone id (`RmngClone.managed` rows) — no container id is
//! stored anywhere.
//!
//! Guest scripts are embedded (`include_str!`) and streamed over `docker exec bash -s`:
//! [`crate::docker::DockerCtl::exec_script`]. Binaries (clone-daemon, agent-wrapper) are
//! pushed via `upload_tar`. Clone images are gen-2 preset builds (`crate::derived` builds
//! each preset's Dockerfile into a hash tag on demand); the retired gen-1 registry-template
//! pull is gone.

use anyhow::{Context, Result, anyhow, bail};
use std::time::{Duration, Instant};

use wire::EnvVar;

use crate::app::App;
use crate::clone_home::CloneHome;
use crate::clone_plan::Side;
use crate::docker::{CLONE_USER, CreateSpec, TarEntry};
use crate::operation::OpHandle;

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
/// Poll interval while waiting for readiness. Short on purpose: the check is one
/// map lookup, and a 2 s interval added up to 2 s of pure wait to every fork.
const WAIT_READY_POLL: Duration = Duration::from_millis(200);

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
    let control =
        app.docker.control_host().await.with_context(
            || "resolving the control-server host for the in-clone RMNG_CONTROL_URL",
        )?;
    vars.push(ev(
        "RMNG_CONTROL_URL",
        format!("http://{control}:{}", wire::PORT_WEB),
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

/// The preset's own [`wire::Preset::vars`], then its Linear key as `LINEAR_API_KEY` (auths the
/// clone's `linear` MCP).
///
/// Both are injected at runtime rather than baked into the preset Dockerfile. An image `ENV`
/// reaches `docker exec` and nothing else: systemd is PID 1 in a clone and does not hand its
/// own environment to the services it starts, so an `ENV` never reaches an SSH login or the
/// desktop session. `/etc/environment` — where these end up — reaches all of them.
///
/// The Linear key goes last so a preset cannot shadow it with a `vars` row of the same name;
/// `etc_environment_conf` lets the last duplicate win.
pub(crate) fn preset_env_vars(p: &wire::Preset) -> Vec<EnvVar> {
    let mut out: Vec<EnvVar> = p
        .vars
        .iter()
        .filter(|v| !v.key.is_empty())
        .cloned()
        .collect();
    if !p.linear_key.is_empty() {
        out.push(EnvVar {
            key: "LINEAR_API_KEY".into(),
            value: p.linear_key.clone(),
        });
    }
    out
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

// --- clone container ------------------------------------------------------------------

/// Progress step → percentage for a clone-container create.
///
/// Carried by the create flow's `OpSpec` (see [`crate::operation`]), not looked up from the
/// operation kind: a flow owns the table it is scored against.
///
/// Known gap, left alone deliberately: the fork/build lead-in steps (`snapshot`,
/// `clone-home`, `build`) have no reading here, so the bar holds at 0 until `create`. They
/// cannot simply be added below `create`, because `clone_container_gen2_from_tag` re-emits
/// `queued` (0.0) after them and would drag the bar backwards. An unscored step is benign —
/// it leaves the bar where it is — so this waits for the create flow's own step keys to be
/// made unique.
pub(crate) fn clone_pct(step: &str) -> Option<f64> {
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
/// Account + settle work a fork runs while the clone boots. Spawned after the pre-boot
/// upload lands (the tar owns `.claude.json` until then) and joined before every return
/// of [`clone_container_after_create`], so boot failure still runs the existing destroy
/// with nothing left writing. The joined account binds come back out through the return
/// values; settle is best-effort fire-and-join.
pub(crate) struct ForkPostUpload {
    pub app: App,
    pub op: OpHandle,
    pub id: String,
    pub group: Option<String>,
    pub claude: Side,
    pub codex: Side,
}

/// One provider's (selection, email, pool), as [`crate::jobs::bind_side`] answers it.
pub(crate) type AccountBind = (Option<String>, Option<String>, Option<String>);

struct PostUploadTasks {
    accounts: tokio::task::JoinHandle<(AccountBind, AccountBind)>,
    settle: tokio::task::JoinHandle<()>,
}

fn spawn_post_upload(w: ForkPostUpload) -> PostUploadTasks {
    let app2 = w.app.clone();
    let id2 = w.id.clone();
    let accounts = tokio::spawn(async move {
        tokio::join!(
            crate::jobs::bind_side::<crate::pool::ClaudePool>(
                &w.app,
                &w.op,
                &w.id,
                w.group.clone(),
                &w.claude
            ),
            crate::jobs::bind_side::<crate::pool::CodexPool>(
                &w.app,
                &w.op,
                &w.id,
                w.group.clone(),
                &w.codex
            ),
        )
    });
    let settle = tokio::spawn(async move {
        // Results dropped, exactly like the inline code: both are best-effort.
        tokio::join!(
            crate::homes::ensure_now(&app2, &id2),
            crate::ssh::allow_clone_now(&app2, &id2)
        );
    });
    PostUploadTasks { accounts, settle }
}

/// Join spawned post-upload work. Settle errors stay dropped (as today); an accounts
/// task panic fails the op, like the inline code panicking would.
async fn join_post_upload(
    t: Option<PostUploadTasks>,
) -> Result<Option<(AccountBind, AccountBind)>> {
    match t {
        None => Ok(None),
        Some(t) => {
            let _ = t.settle.await;
            let acc = t.accounts.await.context("accounts task ended")?;
            Ok(Some(acc))
        }
    }
}

/// Abort spawned post-upload work and wait out the aborts, so a failing boot's destroy
/// runs with nothing left writing.
async fn abort_post_upload(t: Option<PostUploadTasks>) {
    if let Some(t) = t {
        t.accounts.abort();
        t.settle.abort();
        let _ = t.accounts.await;
        let _ = t.settle.await;
    }
}

async fn clone_container_after_create(
    app: &App,
    container: &str,
    hostname: &str,
    env: &[EnvVar],
    agent_playbook: &str,
    global_prompt: &str,
    headless: bool,
    post_upload: Option<ForkPostUpload>,
    on_progress: &mut impl FnMut(&str, &str),
) -> Result<Option<(AccountBind, AccountBind)>> {
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
        // Direct into the live home (mounted before container create): same landing as
        // the old symlink-tar upload, no daemon roundtrip.
        for unit in [
            ".config/systemd/user/gnome-headless.service",
            ".config/systemd/user/rmng-clone-daemon.service",
        ] {
            crate::home_overlay::symlink_clone_home(hostname, unit, "/dev/null")
                .with_context(|| format!("clone {hostname}: masking {unit} failed"))?;
        }
    }

    // Headed clones: mask the Evolution data-server units pre-boot. The shell activates
    // them on every start (~250ms storm serialised with our session build) and needs
    // none of them for capture: calendar/addressbook stay empty, everything else is
    // identical. Masked activation fails fast with no error loop (validated live); the
    // storm's trigger (gnome-shell-calendar-server) then has nothing to wait on.
    // goa-daemon is D-Bus-only with no unit and was only ever pulled in by the source
    // registry, so masking the registry keeps it down too.
    // Same for the five gvfs volume monitors: they each spawn and scan for hardware
    // volumes a container never has (~230ms, sometimes inside our session-build wait).
    // gvfs-daemon itself and gvfs-metadata stay: Files keeps trash + metadata, and
    // nothing in the capture path changes.
    if !headless {
        for unit in [
            ".config/systemd/user/evolution-source-registry.service",
            ".config/systemd/user/evolution-calendar-factory.service",
            ".config/systemd/user/evolution-addressbook-factory.service",
            ".config/systemd/user/gvfs-afc-volume-monitor.service",
            ".config/systemd/user/gvfs-goa-volume-monitor.service",
            ".config/systemd/user/gvfs-gphoto2-volume-monitor.service",
            ".config/systemd/user/gvfs-mtp-volume-monitor.service",
            ".config/systemd/user/gvfs-udisks2-volume-monitor.service",
        ] {
            crate::home_overlay::symlink_clone_home(hostname, unit, "/dev/null")
                .with_context(|| format!("clone {hostname}: masking {unit} failed"))?;
        }
    }

    // Chrome's profile lock is a symlink naming `<hostname>-<pid>`, and it lives in the
    // HOME — so a fork inherits the source's lock while booting under a new hostname, and
    // Chrome refuses to start rather than break a lock it reads as another machine's. The
    // same lock rides in from a template home too. Clearing it here rather than in
    // `fork_clone` is deliberate: this is the one point create, fork, rebase and restore
    // all pass through, and the container is still stopped, so nothing can be holding a
    // lock we would be wrong to drop.
    crate::home_overlay::clear_clone_browser_profile_locks(hostname);

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
    let identity: Vec<TarEntry> = vec![
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
    on_progress("inject", "injecting machine-id + preset env (pre-boot)");
    bins.extend(identity);

    // Render content before boot. Merge-owned files use the mounted home as their base,
    // preserving both image defaults and fork/rebase carryover.
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
    match crate::ssh::clone_ssh_tar_entries(&app.data_dir(), hostname, &cfg.ssh.authorized_keys) {
        Ok(mut ssh_entries) => {
            ssh_entries.push(crate::clone_reconcile::ssh_stamp_entry());
            entries.append(&mut ssh_entries);
        }
        Err(e) => tracing::warn!("clone {hostname}: ssh material skipped: {e}"),
    }

    entries.extend(crate::clone_reconcile::managed_home_entries(
        std::path::Path::new(crate::zfs::HOMES_DIR),
        hostname,
        headless,
        &crate::clone_reconcile::env_value(env, "LINEAR_API_KEY"),
    )?);

    // The single pre-boot tar: binaries + identity + content + stamps in one daemon
    // Post-start injects are down to the headless tmux session and wait-ready: every file
    // (including the bashrc drop-in) already landed in the single pre-boot tar.
    on_progress(
        "inject",
        "injecting clone payload: binaries + identity + config (pre-boot)",
    );
    bins.extend(entries);
    docker.upload_tar(container, bins).await?;

    // Forks: accounts + settle run from here (the tar owns `.claude.json` until now),
    // overlapping boot; joined before every return below.
    let post_tasks = post_upload.map(spawn_post_upload);

    // systemd PID 1 comes up, and the user manager with it, now reading the env written above.
    on_progress("inject", "starting container");
    if let Err(e) = docker.start_container(container).await {
        abort_post_upload(post_tasks).await;
        return Err(e);
    }

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
        return Ok(join_post_upload(post_tasks).await?);
    }

    // wait-ready: poll the mediaplane for the daemon's Hello (keyed by clone_id == hostname).
    // The sleep is only a fallback: every Hello notifies, so we usually wake within ms.
    on_progress("wait-ready", "waiting for the clone-daemon to register");
    let deadline = Instant::now() + WAIT_READY_TIMEOUT;
    loop {
        if app.media.is_connected(hostname) {
            on_progress("ready", &format!("clone {hostname} up + registered"));
            return Ok(join_post_upload(post_tasks).await?);
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
                return Ok(join_post_upload(post_tasks).await?);
            }
            // Dead: fold the container's log tail into the op log, then fail.
            let logs = docker.container_logs_tail(container, 30).await;
            let tail = if logs.trim().is_empty() {
                String::new()
            } else {
                format!("\n{logs}")
            };
            abort_post_upload(post_tasks).await;
            bail!("clone {hostname} exited before its daemon registered; last logs:{tail}");
        }
        app.media.wait_hello_tick(WAIT_READY_POLL).await;
    }
}

// --- delete ---------------------------------------------------------------------------

/// Progress step → percentage for a clone delete. Matches the plan's table.
pub(crate) fn delete_pct(step: &str) -> Option<f64> {
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
    let home = CloneHome::of(app, host_id);
    home.teardown();
    if let Some(row) = gen2_row(app, host_id) {
        if crate::clone_home::is_gen2(&row) {
            on_progress("remove", "destroying the home dataset");
            // Read the origin BEFORE the destroy: afterwards there is nothing left to ask.
            let origin = home.origin();
            match home.destroy(false) {
                Ok(()) => {
                    if let Some(snap) = origin {
                        if let Err(e) = home.drop_snapshot(&snap) {
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
        let home = CloneHome::of(app, hostname);
        home.teardown();
        if let Err(e) = home.destroy(false) {
            tracing::warn!("cleanup for {hostname}: keeping dataset: {e} (non-fatal)");
        }
    }
}

/// Create + start a gen-2 clone on an EXPLICIT local image tag (rebase + rollback).
/// The tag must already exist locally: callers build it first with
/// [`crate::derived::ensure_image`], and the preset rebuild button warms it.
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
    post_upload: Option<ForkPostUpload>,
    mut on_progress: impl FnMut(&str, &str),
) -> Result<(String, Option<(AccountBind, AccountBind)>)> {
    if !is_dns_label(hostname) {
        bail!("clone hostname must be a DNS label (lowercase letters, digits, hyphens)");
    }
    let docker = &app.docker;
    let tag = tag.to_string();
    let cfg = app.config();

    on_progress("queued", &format!("queued gen-2 clone {hostname}"));
    // No image check here: every caller ensures the tag first (`ensure_image` on the
    // create/fork/migrate paths, an explicit `image_exists` loop on rebase), so a
    // re-check is one more daemon roundtrip on every clone for a race it cannot close
    // anyway (check-then-create is not atomic — a prune in between fails at create
    // either way, and the error arm below still cleans up).
    on_progress("create", &format!("creating home dataset for {hostname}"));
    let clone_home = CloneHome::of(app, hostname);
    // The daemon network and the home dataset do not touch each other: one wait
    // instead of two. Either error fails the op exactly as before (network first,
    // matching the old serial order).
    let (net, created_dataset) = tokio::join!(
        docker.ensure_network(),
        async {
            Ok::<bool, anyhow::Error>(match home {
                HomeSource::Create => {
                    clone_home.create_dataset()?;
                    true
                }
                HomeSource::CloneFromSnapshot(ref snap) => {
                    clone_home.clone_dataset_from(snap)?;
                    true
                }
                HomeSource::Reuse => false,
            })
        }
    );
    net?;
    let created_dataset = created_dataset?;

    on_progress("create", &format!("mounting home overlay for {hostname}"));
    // The dataset holds upper/ + work/; the skeleton (template home for this image)
    // is shared. The merged view binds at /home/rmng, so the template home layer
    // shows through on fresh datasets and user files persist in the upper.
    clone_home.ensure_overlay(app, &tag).await?;
    let merged = clone_home.merged();

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
        browse_root: crate::clone_home::browse_root().display().to_string(),
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

    // The FULL env, not a filtered subset. There used to be a `GEN2_DYNAMIC_KEYS` allowlist
    // here, from when a preset's static vars were meant to live in its Dockerfile: it let four
    // keys through and dropped the rest. That also made this path disagree with the resync in
    // `clone_reconcile`, which never filtered — so the two writers of the SAME file composed
    // different contents, and `compose_clone_env` asks them to stay identical.
    match clone_container_after_create(
        app,
        &container,
        hostname,
        env,
        agent_playbook,
        global_prompt,
        headless,
        post_upload,
        &mut on_progress,
    )
    .await
    {
        Ok(joined) => {
            crate::buildinfra::apply_to_clone(app, &container).await;
            Ok((tag, joined))
        }
        Err(e) => {
            tracing::warn!("gen-2 clone {hostname} failed after create; cleaning up: {e}");
            destroy_half_built_clone(app, hostname, created_dataset).await;
            Err(e)
        }
    }
}

/// Fork a gen-2 clone: snapshot the source home, clone it for the new id, create from the
/// target preset's Dockerfile. The source keeps running. Overlay drift is silently dropped
/// (fork copies the dataset only). Returns the new clone's tag, plus any early-joined
/// accounts work (`Some` when `post_upload` was given).
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
    // build) while the same preset reuses the source tag with zero rebuild — unless
    // `rebuild` forces a fresh build with a fresh base pull.
    preset_name: Option<&str>,
    rebuild: bool,
    post_upload: Option<ForkPostUpload>,
    mut on_progress: impl FnMut(&str, &str),
) -> Result<(String, Option<(AccountBind, AccountBind)>)> {
    if !is_dns_label(new_id) {
        bail!("clone hostname must be a DNS label (lowercase letters, digits, hyphens)");
    }
    let _src =
        gen2_row(app, source_id).ok_or_else(|| anyhow::anyhow!("unknown clone '{source_id}'"))?;
    let dockerfile = preset_dockerfile(app, preset_name);
    // The fork's image comes from the TARGET preset's Dockerfile (built lazily inside
    // `clone_container_gen2_from_tag`); the source contributes only its home dataset, snapshotted
    // and cloned below. Same preset reuses the source tag with zero rebuild, because the
    // text hashes the same — unless `rebuild` forces a fresh build.

    on_progress("snapshot", &format!("snapshotting {source_id}"));
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let src_home = CloneHome::of(app, source_id);
    let snap = src_home.snapshot(&format!("fork-{new_id}-{ts}"))?;
    on_progress("clone-home", &format!("cloning home for {new_id}"));
    if let Err(e) = CloneHome::of(app, new_id).clone_dataset_from(&snap) {
        let _ = src_home.drop_snapshot(&snap);
        return Err(e);
    }
    let built = async {
        let tag = crate::derived::ensure_image(app, &dockerfile, rebuild, &mut on_progress).await?;
        clone_container_gen2_from_tag(
            app,
            &tag,
            new_id,
            HomeSource::Reuse,
            env,
            agent_playbook,
            global_prompt,
            headless,
            post_upload,
            &mut on_progress,
        )
        .await
    }
    .await;
    match built {
        Ok(joined) => Ok(joined),
        Err(e) => {
            destroy_half_built_clone(app, new_id, true).await;
            let _ = src_home.drop_snapshot(&snap);
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
    // Check the stopped home; invalid content leaves the old container intact for recovery.
    crate::clone_reconcile::managed_home_entries(
        std::path::Path::new(crate::zfs::HOMES_DIR),
        host_id,
        headless,
        &crate::clone_reconcile::env_value(env, "LINEAR_API_KEY"),
    )?;
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
        None,
        &mut on_progress,
    )
    .await
    {
        Ok((tag, _)) => {
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
                None,
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
/// [`crate::pool::push_both_sides`] after the fleet starts (it needs running clones).
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
            let home = CloneHome::of(app, host_id);
            home.teardown();
            let _ = home.destroy(false);
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
    let home = CloneHome::of(app, host_id);
    home.create_dataset()?;

    on_progress("copy", "copying /home/rmng out of the old container");
    // Into the overlay upper: the merged view then shows old home over the new base.
    // No overlay yet — the recreate below mounts it, on top of what lands here.
    home.ensure_layout()?;
    let upper = home.upper();
    // Streamed, not buffered: the archive goes daemon -> extractor without ever being a
    // whole home in memory, so several clones can migrate at once (see
    // `jobs::migrate_all_on_boot`). The byte count is tallied off the stream itself.
    let counted = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));
    let stream = app.docker.download_tar_stream(host_id, "/home/rmng")?;
    let dest = upper.to_string_lossy().into_owned();
    let handle = tokio::runtime::Handle::current();
    let tally = counted.clone();
    tokio::task::spawn_blocking(move || -> Result<()> {
        let reader = tokio_util::io::SyncIoBridge::new_with_handle(
            tokio_util::io::StreamReader::new(stream),
            handle,
        );
        extract_home_tar(
            CountingReader {
                inner: reader,
                tally,
            },
            &dest,
        )
    })
    .await
    .context("the home extract task did not finish")??;
    let bytes = counted.load(std::sync::atomic::Ordering::Relaxed);

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
    let image = crate::derived::ensure_image(app, &dockerfile, false, &mut *on_progress).await?;
    let tag = clone_container_gen2_from_tag(
        app,
        &image,
        host_id,
        HomeSource::Reuse,
        env,
        agent_playbook,
        global_prompt,
        headless,
        None,
        &mut *on_progress,
    )
    .await
    .map(|t| t.0)?;

    on_progress("stop", "stopping the migrated clone");
    app.docker.stop_even_if_paused(host_id).await?;
    on_progress("done", &format!("clone {host_id} migrated ({bytes} bytes)"));
    Ok(MigrateReport { bytes, tag })
}

/// Counts the bytes pulled through the home archive stream, so the per-clone report keeps
/// its "N bytes" figure now that the archive is never a `Vec` whose `len()` could be read.
struct CountingReader<R> {
    inner: R,
    tally: std::sync::Arc<std::sync::atomic::AtomicU64>,
}

impl<R: std::io::Read> std::io::Read for CountingReader<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let n = self.inner.read(buf)?;
        self.tally
            .fetch_add(n as u64, std::sync::atomic::Ordering::Relaxed);
        Ok(n)
    }
}

/// Strip the archive's `rmng/` top-level component and refuse anything that would escape
/// the destination. `None` = the path WAS the top-level dir (nothing to extract).
///
/// Applied to entry paths AND to hard-link targets — they are rooted the same way, and
/// stripping only the former is what made every home with hard links fail to migrate.
fn home_archive_rel(path: &std::path::Path) -> Result<Option<std::path::PathBuf>> {
    let mut comps = path.components();
    comps.next(); // strip the `rmng/` top-level dir
    let rel: std::path::PathBuf = comps.collect();
    if rel.as_os_str().is_empty() {
        return Ok(None);
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
    Ok(Some(rel))
}

/// Extract a daemon `download_from_container` tar of `/home/rmng` into the dataset dir.
/// The archive roots every entry under the basename (`rmng/...`), so the first component
/// is stripped. Runs as CT root, preserving the archived owners/modes. Entries escaping
/// the destination (`..`, absolute) are refused.
///
/// Takes a READER, not a slice: migration streams the archive straight through rather
/// than holding a whole home in memory (see `DockerCtl::download_tar_stream`).
///
/// Hard links are collected and applied AFTER the main pass. Two reasons: the link target
/// needs the same `rmng/` strip the entry path gets, and an archive may name a target it
/// has not written yet. Missing the first of those is why homes carrying a `uv` cache, a
/// `pnpm` store or any other hard-linked tree failed with
/// `No such file or directory (os error 2) when hard linking rmng/...` — on CT 104 that
/// was 4 of 8 clones, up to 28 276 hard-linked files in one home.
fn extract_home_tar<R: std::io::Read>(reader: R, dest: &str) -> Result<()> {
    let mut archive = tar::Archive::new(reader);
    archive.set_preserve_permissions(true);
    // LOAD-BEARING, same as the skeleton export. The tar crate defaults to giving every
    // extracted file to the running process — root — so without this a migrated home
    // arrives entirely root-owned and the clone user cannot write to it. Measured on
    // CT 104's first run: 554 501 files landed as uid 0 against 35 as uid 1000.
    archive.set_preserve_ownerships(true);
    let dest = std::path::Path::new(dest);
    let mut links: Vec<(std::path::PathBuf, std::path::PathBuf)> = Vec::new();
    for entry in archive.entries()? {
        let mut entry = entry?;
        let Some(rel) = home_archive_rel(&entry.path()?)? else {
            continue;
        };
        if entry.header().entry_type() == tar::EntryType::Link {
            let target = entry
                .link_name()?
                .ok_or_else(|| anyhow!("hard link {rel:?} carries no target"))?;
            let src = home_archive_rel(&target)?
                .ok_or_else(|| anyhow!("hard link {rel:?} targets the archive root"))?;
            links.push((src, rel));
            continue;
        }
        entry.unpack(dest.join(rel))?;
    }
    for (src, dst) in links {
        let (src, dst) = (dest.join(src), dest.join(dst));
        if let Some(parent) = dst.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("mkdir {} for a hard link", parent.display()))?;
        }
        let _ = std::fs::remove_file(&dst);
        std::fs::hard_link(&src, &dst)
            .with_context(|| format!("hard linking {} to {}", src.display(), dst.display()))?;
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

pub(crate) fn archive_pct(step: &str) -> Option<f64> {
    Some(match step {
        "queued" => 0.0,
        "stop" => 75.0,
        "done" => 100.0,
        _ => return None,
    })
}

pub(crate) fn unarchive_pct(step: &str) -> Option<f64> {
    Some(match step {
        "queued" => 0.0,
        "start" => 60.0,
        "render" => 80.0,
        "done" => 100.0,
        _ => return None,
    })
}

/// Progress step → percentage for a gen-2 one-shot migration. Matches the `migrate_one`
/// step keys, plus the one step the JOB emits before `migrate_one` is called.
///
/// `pre-stop` is that step, and it exists because of a drift: `jobs::run_migrate` stops the
/// source container first (the home copy needs a stable source) and used to emit that as
/// `stop` — the same key `migrate_one` uses for its LAST step. The bar therefore jumped to
/// 90% before the migration had copied a byte, then fell back to 0 at `migrate_one`'s
/// `queued`. Two different moments cannot share one step key.
pub(crate) fn migrate_pct(step: &str) -> Option<f64> {
    Some(match step {
        "queued" => 0.0,
        "pre-stop" => 5.0,
        "create" => 10.0,
        "copy" => 30.0,
        "recreate" => 60.0,
        "stop" => 90.0,
        "done" => 100.0,
        _ => return None,
    })
}

/// Progress step → percentage for a gen-2 REBASE (`jobs::run_rebase` → [`rebase_clone`]).
///
/// Rebase has no `wire::OperationKind` variant of its own, so it files as `Clone`. It used
/// to be scored against [`clone_pct`] because of that, and [`clone_pct`] has no `stop` — the
/// first thing a rebase does (stopping the clone so its home is a stable source) had no
/// reading on the bar at all. The table now travels with the operation's spec instead of
/// being derived from its kind, so a borrowed kind can no longer borrow the wrong table.
///
/// `queued` is 20 here, not 0: it is emitted by `clone_container_gen2_from_tag` for the
/// CONTAINER phase, after the image build and the stop, so scoring it as the start of the
/// operation would drag the bar backwards mid-rebase.
pub(crate) fn rebase_pct(step: &str) -> Option<f64> {
    Some(match step {
        "build" => 5.0,
        "stop" => 15.0,
        "queued" => 20.0,
        "create" => 30.0,
        "inject" => 45.0,
        "start" => 60.0,
        "wait-ready" => 75.0,
        "ready" => 90.0,
        // The failure arm: the old tag is being recreated, and the operation ends Error.
        "rollback" => 95.0,
        "done" => 100.0,
        _ => return None,
    })
}

/// The table for a flow with no coarse pct at all: an image build streams its own step lines
/// as messages, so the bar holds until the runner finishes the operation. Named rather than
/// left implicit, so a spec always says which table it is scored against.
pub(crate) fn no_pct(_step: &str) -> Option<f64> {
    None
}

// --- op-log pct tables (carried by the flows' OpSpecs) --------------------------------
//
// Each table lives next to the code that emits its step keys, and an operation carries the
// one it is scored against in its `OpSpec` (see `crate::operation`). There is deliberately no
// by-kind index: one existed, and a rebase filed under `OperationKind::Clone` was scored
// against the clone table, which has no reading for the `stop` step a rebase opens with.
// Carrying the table on the spec is what makes that unrepresentable. (Monitors-apply is
// intentionally NOT an Operation — web.rs streams its `[ct]` lines directly — so there is no
// monitors table here.)
//
// The retired `Commit` and `Pull` kinds have no table. They did, justified as letting old
// persisted ops still render — but `pct` is a STORED field on the operation row, written as
// it runs and never recomputed on read, so an old op renders from its own record and the
// tables only ever served the by-kind index that used to sit here. Deleting the index left
// them with no caller, which is the answer to whether they were load-bearing.

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
    // Dev mode / not-yet-probed: use the directory of the clone socket path.
    let sock = wire::CLONE_SOCKET;
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

    /// A home carrying a hard link (a `uv` cache linked into a venv, a `pnpm` store linked
    /// into `node_modules`) must extract with the link intact. The archive roots BOTH the
    /// entry path and the link target at `rmng/`; stripping only the former made the
    /// extract fail with `No such file or directory (os error 2) when hard linking
    /// rmng/...` and took down 4 of CT 104's 8 clones.
    #[test]
    fn extract_home_tar_rebases_hard_link_targets_onto_the_destination() {
        let body = b"payload";
        // Stamp the archive with OUR uid/gid: the extractor restores ownership now, so a
        // foreign owner would need root. The hard-link behaviour under test is unaffected.
        let (uid, gid) = unsafe { (libc::geteuid(), libc::getegid()) };
        let mut builder = tar::Builder::new(Vec::new());
        // The daemon's archive carries a dir entry before anything inside it; `unpack`
        // does not invent parents.
        for dir in [
            "rmng/",
            "rmng/.cache/",
            "rmng/.cache/pkg/",
            "rmng/.venv/",
            "rmng/.venv/site/",
        ] {
            let mut h = tar::Header::new_gnu();
            h.set_size(0);
            h.set_mode(0o755);
            h.set_uid(uid as u64);
            h.set_gid(gid as u64);
            h.set_entry_type(tar::EntryType::Directory);
            h.set_cksum();
            builder.append_data(&mut h, dir, std::io::empty()).unwrap();
        }
        let mut file = tar::Header::new_gnu();
        file.set_size(body.len() as u64);
        file.set_mode(0o644);
        file.set_uid(uid as u64);
        file.set_gid(gid as u64);
        file.set_cksum();
        builder
            .append_data(&mut file, "rmng/.cache/pkg/thing", &body[..])
            .unwrap();
        let mut link = tar::Header::new_gnu();
        link.set_size(0);
        link.set_mode(0o644);
        link.set_uid(uid as u64);
        link.set_gid(gid as u64);
        link.set_entry_type(tar::EntryType::Link);
        builder
            .append_link(&mut link, "rmng/.venv/site/thing", "rmng/.cache/pkg/thing")
            .unwrap();
        let archive = builder.into_inner().unwrap();

        let dest = std::env::temp_dir().join(format!("rmng-hardlink-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dest);
        std::fs::create_dir_all(&dest).unwrap();
        extract_home_tar(&archive[..], &dest.to_string_lossy()).unwrap();

        let original = dest.join(".cache/pkg/thing");
        let linked = dest.join(".venv/site/thing");
        assert_eq!(std::fs::read(&original).unwrap(), body);
        assert_eq!(
            std::fs::read(&linked).unwrap(),
            body,
            "the hard link must resolve inside the destination, not against the CWD"
        );
        // Really a hard link, not a second copy.
        use std::os::unix::fs::MetadataExt;
        assert_eq!(
            std::fs::metadata(&original).unwrap().ino(),
            std::fs::metadata(&linked).unwrap().ino(),
        );
        let _ = std::fs::remove_dir_all(&dest);
    }

    /// The extractor must keep the ARCHIVED owner, not give everything to the process
    /// running it. Without `set_preserve_ownerships` a migrated home arrives entirely
    /// root-owned and the clone user cannot write to it — 554 501 files on CT 104.
    /// Root-only: chown needs privilege, so it self-skips elsewhere.
    #[test]
    fn extract_home_tar_preserves_the_archived_owner() {
        if unsafe { libc::geteuid() } != 0 {
            eprintln!("skipping: needs root to restore ownership");
            return;
        }
        let body = b"payload";
        let mut builder = tar::Builder::new(Vec::new());
        let mut dir = tar::Header::new_gnu();
        dir.set_size(0);
        dir.set_mode(0o755);
        dir.set_uid(1000);
        dir.set_gid(1000);
        dir.set_entry_type(tar::EntryType::Directory);
        dir.set_cksum();
        builder
            .append_data(&mut dir, "rmng/", std::io::empty())
            .unwrap();
        let mut file = tar::Header::new_gnu();
        file.set_size(body.len() as u64);
        file.set_mode(0o644);
        file.set_uid(1000);
        file.set_gid(1000);
        file.set_cksum();
        builder
            .append_data(&mut file, "rmng/owned", &body[..])
            .unwrap();
        let archive = builder.into_inner().unwrap();

        let dest = std::env::temp_dir().join(format!("rmng-own-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dest);
        std::fs::create_dir_all(&dest).unwrap();
        extract_home_tar(&archive[..], &dest.to_string_lossy()).unwrap();
        use std::os::unix::fs::MetadataExt;
        let md = std::fs::metadata(dest.join("owned")).unwrap();
        assert_eq!(
            (md.uid(), md.gid()),
            (1000, 1000),
            "archived owner must survive"
        );
        let _ = std::fs::remove_dir_all(&dest);
    }

    /// A link target that escapes the dataset is refused, like any other entry.
    #[test]
    fn extract_home_tar_refuses_an_escaping_hard_link_target() {
        let mut builder = tar::Builder::new(Vec::new());
        let mut link = tar::Header::new_gnu();
        link.set_size(0);
        link.set_mode(0o644);
        link.set_entry_type(tar::EntryType::Link);
        builder
            .append_link(&mut link, "rmng/evil", "rmng/../../etc/shadow")
            .unwrap();
        let archive = builder.into_inner().unwrap();
        let dest = std::env::temp_dir().join(format!("rmng-hardlink-esc-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dest);
        std::fs::create_dir_all(&dest).unwrap();
        let err = extract_home_tar(&archive[..], &dest.to_string_lossy()).unwrap_err();
        assert!(
            format!("{err:#}").contains("outside the dataset"),
            "{err:#}"
        );
        let _ = std::fs::remove_dir_all(&dest);
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

    /// A preset's vars are delivered by the server, not baked into the image, because an image
    /// `ENV` reaches `docker exec` and nothing else — systemd is PID 1 in a clone and does not
    /// hand its own environment to the services it starts, so an SSH login and the desktop
    /// session both miss it. `/etc/environment` reaches all three, and this is what puts them
    /// there.
    #[test]
    fn preset_vars_reach_etc_environment_and_cannot_shadow_the_linear_key() {
        let p = wire::Preset {
            name: "medi".into(),
            linear_key: "lin_api_real".into(),
            vars: vec![
                EnvVar {
                    key: "TURBO_TEAM".into(),
                    value: "talktomedi".into(),
                },
                // A blank value is a real setting: `KEY=` clears an inherited value.
                EnvVar {
                    key: "BLANK".into(),
                    value: String::new(),
                },
                // A preset must not be able to hand its clones someone else's Linear key by
                // naming the variable itself — the real key is appended after these.
                EnvVar {
                    key: "LINEAR_API_KEY".into(),
                    value: "lin_api_impostor".into(),
                },
                // Defence in depth: `merge_presets` already drops blank keys on save, so this
                // row can only come from a hand-edited config.json.
                EnvVar {
                    key: String::new(),
                    value: "orphan".into(),
                },
            ],
            ..Default::default()
        };

        let body = clone_etc_environment_conf(&preset_env_vars(&p));
        assert!(body.contains("TURBO_TEAM=talktomedi\n"), "{body}");
        assert!(body.contains("BLANK=\n"), "{body}");
        assert!(body.contains("LINEAR_API_KEY=lin_api_real\n"), "{body}");
        assert!(!body.contains("lin_api_impostor"), "{body}");
        assert_eq!(body.matches("LINEAR_API_KEY=").count(), 1, "{body}");
        assert!(!body.contains("=orphan"), "{body}");
        // The base desktop session is still underneath them.
        assert!(body.contains("XDG_CURRENT_DESKTOP=GNOME\n"), "{body}");
    }

    /// The create path must compose the same `/etc/environment` the resync does. They are two
    /// writers of one file, so a key either of them omits flips on the next pass.
    ///
    /// A `GEN2_DYNAMIC_KEYS` allowlist used to sit between this and the create-time write,
    /// passing four keys and dropping everything else — which silently meant a preset's vars
    /// reached a clone only on the first resync, ~30 s after it booted, and never at all if the
    /// create path was the only writer.
    #[test]
    fn create_time_env_carries_the_preset_vars_through() {
        let p = wire::Preset {
            name: "medi".into(),
            vars: vec![EnvVar {
                key: "TURBO_TEAM".into(),
                value: "talktomedi".into(),
            }],
            ..Default::default()
        };
        let composed = compose_clone_env(
            vec![EnvVar {
                key: "RMNG_CONTROL_URL".into(),
                value: "http://rmng-control:9000".into(),
            }],
            vec![EnvVar {
                key: "RMNG_PROXY_KEY".into(),
                value: "k".into(),
            }],
            &preset_env_vars(&p),
        );
        let keys: Vec<&str> = composed.iter().map(|v| v.key.as_str()).collect();
        assert!(keys.contains(&"TURBO_TEAM"), "{keys:?}");
        assert!(keys.contains(&"RMNG_CONTROL_URL"), "{keys:?}");
        assert!(keys.contains(&"RMNG_PROXY_KEY"), "{keys:?}");
        assert!(keys.contains(&"ANTHROPIC_MODEL"), "{keys:?}");
    }

    /// A preset with no vars and no key contributes nothing — the clone keeps only the base
    /// session env.
    #[test]
    fn a_preset_with_nothing_set_adds_no_env() {
        let p = wire::Preset {
            name: "bare".into(),
            ..Default::default()
        };
        assert_eq!(preset_env_vars(&p), Vec::new());
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
    fn step_pct_tables_match_plan() {
        assert_eq!(clone_pct("queued"), Some(0.0));
        assert_eq!(clone_pct("create"), Some(20.0));
        assert_eq!(clone_pct("inject"), Some(35.0));
        assert_eq!(clone_pct("start"), Some(55.0));
        assert_eq!(clone_pct("wait-ready"), Some(75.0));
        assert_eq!(clone_pct("ready"), Some(80.0));
        assert_eq!(clone_pct("monitors"), Some(85.0));
        assert_eq!(clone_pct("accounts"), Some(95.0));
        assert_eq!(clone_pct("done"), Some(100.0));

        assert_eq!(delete_pct("stop"), Some(40.0));
        assert_eq!(delete_pct("remove"), Some(75.0));

        // Unknown step keys yield None (the runner leaves the pct unchanged).
        assert_eq!(clone_pct("bogus"), None);

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
            let pct = clone_pct(step).expect("known clone step");
            assert!(pct >= prev, "clone step {step} pct {pct} < previous {prev}");
            prev = pct;
        }
    }

    /// A rebase files as `OperationKind::Clone` (wire has no `Rebase` variant) but carries
    /// [`rebase_pct`] in its spec, which is the whole point of the table travelling on the
    /// spec rather than being looked up by kind. The table must cover every step it emits,
    /// in emission order, without going backwards — `stop` above all, which is the step the
    /// create table was missing when rebase was scored against it.
    #[test]
    fn rebase_table_covers_every_step_the_flow_emits() {
        // build (derived::ensure_image) → stop (rebase_clone) → the container phase
        // (clone_container_gen2_from_tag) → done (the runner).
        let order = [
            "build",
            "stop",
            "queued",
            "create",
            "inject",
            "start",
            "wait-ready",
            "ready",
            "done",
        ];
        let mut prev = -1.0_f64;
        for step in order {
            let pct = rebase_pct(step).unwrap_or_else(|| panic!("rebase step {step} has no pct"));
            assert!(
                pct >= prev,
                "rebase step {step} pct {pct} < previous {prev}"
            );
            prev = pct;
        }
        // The failure arm still has a reading, and clone_pct never did.
        assert!(rebase_pct("rollback").is_some());
        assert_eq!(clone_pct("stop"), None);
    }

    /// The job-side pre-stop and `migrate_one`'s final stop are two different moments and
    /// must not share a step key: they did, and the bar hit 90% before a byte was copied.
    #[test]
    fn migrate_pre_stop_scores_below_the_copy() {
        assert_eq!(migrate_pct("pre-stop"), Some(5.0));
        assert_eq!(migrate_pct("stop"), Some(90.0));
        assert!(migrate_pct("pre-stop") < migrate_pct("copy"));
    }
}
