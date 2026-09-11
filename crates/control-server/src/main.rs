//! control-server — the fleet hub (see ../README.md).
//!
//! One tokio service binding the video plane, web API + SSE + static frontend, port-forward
//! data plane (9005), and SSH bastion; `smbd` serves retained clone homes on port 445.

mod agentlog;
mod app;
mod assets;
mod boot;
mod buildinfra;
mod cgroup;
mod chat;
mod claude;
mod clone_ops;
mod clone_plan;
mod clone_reconcile;
mod clonekey;
mod codex;
mod config;
mod derived;
mod docker;
mod files;
mod forward;
mod home_overlay;
mod homes;
mod jobs;
mod ledger;
mod mediaplane;
mod monitor;
mod naming;
mod oauth;
mod pool;
mod provision;
mod shared;
mod smb;
mod ssh;
mod state;
mod stuck;
mod stucklog;
mod termplane;
mod token_unmigrate;
mod update;
mod web;
mod zfs;

use std::sync::Arc;

use anyhow::Result;

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                // `clip` (the clipboard broker) logs debug by default: copy/paste-driven
                // only (sparse), and the go-to trail for cross-machine clipboard issues.
                .unwrap_or_else(|_| {
                    tracing_subscriber::EnvFilter::new("info,tower_http=warn,clip=debug")
                }),
        )
        .init();

    // Self-upgrade helper mode (detached container from the NEW image). A container launched by
    // `jobs::run_update` runs `rmng-control-server self-upgrade <handoff>`: it stops+removes the
    // old container and recreates it, then exits. Diverges — never becomes the normal server.
    // Placed after tracing init (so the helper gets logging) and before config::load().
    let argv: Vec<String> = std::env::args().collect();
    if argv.get(1).map(String::as_str) == Some("self-upgrade") {
        let handoff = argv
            .get(2)
            .cloned()
            .unwrap_or_else(|| update::HANDOFF_PATH.to_string());
        update::self_upgrade_main(&handoff).await; // diverges
    }

    let cfg = config::load()?;
    let store = Arc::new(state::StateStore::load(config::state_path())?);
    state::spawn_watcher(store.clone());

    // Self-heal the ZFS device node (see zfs::ensure_dev_zfs): /dev is tmpfs, so the
    // node vanishes on CT reboot, and the host node must not be bind-mounted in.
    // Non-fatal by design.
    zfs::ensure_dev_zfs();

    // Snapshot each clone's retired `group` binding BEFORE anything mutates the state store.
    // `RmngClone` has no such field any more, so the first `store.mutate` below persists
    // `state.json` without it and the binding is unrecoverable — see `read_raw_clone_pools`.
    let pools_before = token_unmigrate::read_raw_clone_pools(&config::state_path());

    let app = app::App::new(store, cfg, wire::DATA_DIR);

    // Seed ControlState with the config's active layout + preset names so the sidebar
    // switcher renders correctly on a fresh boot, before any `/api/config` PUT or
    // `/api/layout/activate` call runs.
    web::mirror_layout_to_state(&app);
    // Same for the sidebar's pool sections: seed the configured pools so accounts
    // group correctly from the first frame.
    web::mirror_groups_to_state(&app);

    // Probe the Docker environment (daemon reachable, self-container detection, sock mount,
    // render node) and cache the report so `GET /api/setup/env` + the wizard can render it.
    // Non-fatal: a down daemon / failed check must NOT stop the server booting — the wizard
    // is exactly where the operator fixes those. `ensure_network` only runs here once setup
    // is latched complete (the network is lazy).
    // Bounded: the shared bollard client's request timeout is 1 h (a derived-image build
    // legitimately runs that long), so a wedged-but-connectable daemon would otherwise
    // block THIS await — and with it the whole server boot — for up to an hour.
    // Runs BEFORE `reconcile_pending`: self_setup is what populates the cached env report with
    // the detected self-container id, and reconcile needs that id to inspect the running image
    // and verify the update digest. (Daemon down/unresponsive → this times out, the env cache
    // stays default, and reconcile correctly falls back to the optimistic "digest unverified".)
    {
        let setup_complete = app.config().setup_complete;
        match tokio::time::timeout(
            std::time::Duration::from_secs(30),
            app.docker.self_setup(setup_complete),
        )
        .await
        {
            Ok(report) if report.required_ok() => {}
            Ok(_) => tracing::error!(
                "Docker self-setup reported failing required checks; the server is up so the \
                 setup wizard can show the details (GET /api/setup/env)"
            ),
            Err(_) => tracing::error!(
                "Docker self-setup timed out after 30s (daemon connected but unresponsive?); \
                 booting anyway — retry via the wizard's env checklist"
            ),
        }
    }

    // The running image's git revision, published to browsers on every `/events` connect so a
    // page whose bundle predates an upgrade reloads itself. Read here once rather than per
    // connect: it costs two Docker inspects and cannot change while this process lives.
    // Runs after `self_setup`, which is what detects the self-container id. A dev run (no self
    // container, or an image built without `GIT_SHA`) keeps the per-boot id instead.
    {
        let reference = wire::SERVER_IMAGE;
        let (repo, _) = crate::docker::split_reference(reference);
        if let Some(id) = app.docker.env().await.self_container {
            match app.docker.self_image_info(&id, &repo).await {
                Ok(info) => {
                    if let Some(rev) = info.revision.as_deref() {
                        app.set_build_id(rev);
                        tracing::info!("running image revision {rev}");
                    }
                }
                Err(e) => tracing::warn!("reading own image revision: {e}"),
            }
        }
    }

    // Shared build infra (pull-through Hub mirror + remote BuildKit): ensure the two infra
    // containers exist + run. Gated on setup-complete + the master toggle; runs after
    // `self_setup` (which ensured the `rmng` network). Non-fatal + bounded — a down/slow
    // daemon (or a first-run image pull) logs and retries next boot, same posture as
    // `ensure_network`. 120 s covers a cold pull of registry + buildkit.
    {
        if app.config().setup_complete {
            match tokio::time::timeout(
                std::time::Duration::from_secs(120),
                app.docker.ensure_build_infra(),
            )
            .await
            {
                Ok(Ok(())) => {}
                Ok(Err(e)) => {
                    tracing::warn!("build-infra ensure failed: {e:#} (retries next boot)")
                }
                Err(_) => {
                    tracing::warn!("build-infra ensure timed out after 120s (retries next boot)")
                }
            }
        }
    }

    // A persisted `Running` operation is a corpse from a server that crashed/was killed
    // mid-op (an `Operation` lives only while its driving task runs). Mark such ops `Error`
    // + prune them, so a same-named clone/pull/commit isn't blocked forever by the in-flight
    // guards. State-only — safe with Docker down.
    //
    // Resolve a surviving self-update Operation FIRST, before fail_stale_ops would clobber it as
    // "interrupted". self_setup above already populated the env cache with our self-container id,
    // so reconcile's running-image digest check can actually run. Best-effort; a no-op when the
    // handoff is absent (normal boot).
    update::reconcile_pending(&app).await;
    jobs::fail_stale_ops(&app);

    // Boot reconciliation: `state.json` is authoritative for clone rows, but the daemon is
    // authoritative for what actually exists — diff them once so drift is visible instead
    // of silent. Orphan rows (managed clone, no container — someone `docker rm`ed it behind
    // the server) and unknown managed containers (a container with our label but no row —
    // e.g. a build worker left over from a crashed bootstrap) are LOGGED, not auto-fixed:
    // deleting either side automatically could destroy something the operator wanted.
    // Best-effort + bounded — a down/wedged daemon skips this (same posture as self-setup).
    {
        match tokio::time::timeout(
            std::time::Duration::from_secs(10),
            app.docker.list_managed_containers(),
        )
        .await
        {
            Ok(Ok(live)) => {
                let live_names: std::collections::HashSet<&str> =
                    live.iter().map(|c| c.name.as_str()).collect();
                let hosts = app.store.get().hosts;
                for h in hosts.iter().filter(|h| h.managed) {
                    if !live_names.contains(h.id.as_str()) {
                        tracing::warn!(
                            "reconcile: managed clone '{}' has no container on the daemon \
                             (removed behind the server?) — delete the row in the UI or \
                             recreate the clone",
                            h.id
                        );
                    }
                }
                let known: std::collections::HashSet<&str> =
                    hosts.iter().map(|h| h.id.as_str()).collect();
                for c in live.iter().filter(|c| !known.contains(c.name.as_str())) {
                    tracing::warn!(
                        "reconcile: managed container '{}' (image {}, {}) has no clone row — \
                         a leftover from a crashed operation? Remove it with `docker rm`",
                        c.name,
                        c.image,
                        if c.running { "running" } else { "stopped" }
                    );
                }
            }
            Ok(Err(e)) => tracing::warn!("reconcile: listing managed containers failed: {e:#}"),
            Err(_) => tracing::warn!("reconcile: listing managed containers timed out after 10s"),
        }
    }

    // One-shot reverse token migration: recover the OAuth credentials the retired group-proxy
    // era left in the per-group CLIProxyAPI `auth-dir`s and write them back into the RMNG-owned
    // stores (claude-accounts.json / codex-accounts.json + cloneGroups/codexGroups), so an
    // upgraded deployment carries every account across with no operator re-login. Stamp-gated
    // (runs once) and best-effort: it must NOT block boot, so any panic is caught and logged.
    //
    // ORDERING IS LOAD-BEARING. The `rmng-cliproxy` sidecar is torn down FIRST: while it runs it
    // keeps per-group CLIProxyAPI processes alive, and those refresh OAuth tokens on their own
    // schedule. Since a refresh token is single-use, a rotation landing after we copy a
    // credential would invalidate the copy — leaving dead tokens in the stores and forcing a
    // re-login of every account, exactly what this migration exists to avoid.
    app.docker.remove_retired_group_proxy().await;
    if let Err(e) = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        token_unmigrate::unmigrate_group_proxy_tokens(&app, &pools_before)
    })) {
        tracing::error!("group-proxy token reverse-migration panicked (booting anyway): {e:?}");
    }
    // The migration above can rebuild the pool list: re-mirror so the boot snapshot
    // carries the migrated pools, not the pre-migration ones.
    web::mirror_groups_to_state(&app);

    // GStreamer init MUST finish before smb/ssh (and any other child
    // spawners). Those supervisors otherwise inherit gst-plugin-scanner pipes and
    // hang media init forever — web/video/forward never bind.
    let app_for_bg = app.clone();
    let app_for_media = app.clone();
    boot::run_late_boot(
        mediaplane::init,
        move || {
            // Gen-2 one-shot migration FIRST: any gen-1 row (managed, no dataset) is
            // migrated one clone at a time, with one Migrate op per clone in the jobs
            // UI. The fleet stops for the window and (non-archived) restarts after.
            // No gen-1 rows ⇒ no-op. Runs under the whole-LXC backup.
            tokio::spawn(jobs::migrate_all_on_boot(app_for_bg.clone()));
            // Home overlays do not survive a CT reboot (mounts, unlike containers):
            // re-establish every managed clone's merged view before anything serves it.
            // Best-effort per clone; migration mounts its own as it goes.
            tokio::spawn(home_overlay::remount_all(app_for_bg.clone()));
            // Background loops: the per-clone agent-state monitor poller, the one-shot
            // clone-home sync (links data/hosts/<id> → the clone's dataset dir, archived
            // included, so every home is browsable in one place; runs once here, the
            // create job links eagerly and the delete job unlinks), the smbd supervisor that serves that
            // same directory as the `clones` SMB share (port 445), so the homes are browsable over
            // `smb://<host>/clones` too.
            tokio::spawn(monitor::run(app_for_bg.clone()));
            tokio::spawn(clone_reconcile::run(app_for_bg.clone()));
            tokio::spawn(homes::sync_all(app_for_bg.clone()));
            // Reads each clone's agent session logs through the symlinks `homes` maintains —
            // hence spawned after it. Supplies per-clone token totals and the activity signal
            // for agents RMNG did not launch (a human running `claude` over SSH).
            tokio::spawn(agentlog::run_scanner(app_for_bg.clone()));
            // Reads the same transcripts through the same symlinks, for a different reason:
            // it keeps a distilled, greppable copy under data/ledger so a clone's work history
            // outlives the clone. Its directory listing is also the registry of names already
            // used, which is what stops a new clone inheriting a retired one's history.
            tokio::spawn(ledger::run(app_for_bg.clone()));
            // The shared pool: one dir at <homes>/.shared (daemon-visible, unlike
            // anything under data/), bound into every clone at /home/rmng/shared
            // from first boot (see CreateSpec::shared_dir) and served as the
            // `shared` SMB share. Ensured here once; the bind needs no upkeep. A pool
            // failure aborts startup: booting clones without it silently breaks the
            // read-write-both-sides design (Docker would invent a root-owned dir).
            if let Err(e) = shared::ensure_pool() {
                tracing::error!(target: "shared", "ensuring the shared pool: {e:#} — aborting startup");
                std::process::exit(1);
            }
            tokio::spawn(buildinfra::run(app_for_bg.clone()));
            // Scheduled chat delivery: fires operator-queued messages once their time passes.
            // Disk-backed, so anything that came due during a restart goes out on the first tick.
            tokio::spawn(chat::run_scheduler(app_for_bg.clone()));
            tokio::spawn(smb::run(app_for_bg.clone()));
            tokio::spawn(ssh::run(app_for_bg.clone()));
            // Claude + Codex account subsystems. Each provider runs a usage poller (which
            // also refreshes tokens and fans any rotation out to the clones running that
            // account) and a rotator (which re-balances group-bound clones off an exhausted
            // account). Together they are the only thing that writes a clone's credential
            // files — the reconciler deliberately has no token logic.
            tokio::spawn(claude::run_poller(app_for_bg.clone()));
            tokio::spawn(claude::run_rotator(app_for_bg.clone()));
            tokio::spawn(codex::run_poller(app_for_bg.clone()));
            tokio::spawn(codex::run_rotator(app_for_bg.clone()));
        },
        move |media_init| {
            // Port 1 (video) — ingest clone dmabufs, VA-API encode, serve the viewer.
            mediaplane::spawn(app_for_media, media_init);
        },
    );

    web::serve(app).await
}
