//! `docker.rs` — the bollard-backed orchestration substrate.
//!
//! This is RMNG's only fleet backend now: every clone/base-image/network operation the
//! control-server performs goes through [`DockerCtl`] against ONE local Docker daemon
//! (unix socket), replacing the retired SSH+`pct` Proxmox path in `orchestrate.rs`. The
//! primitives here are dumb and composable — `provision.rs` (Task 5) stitches them into
//! clone create/bootstrap/commit/delete flows; `App` (Task 6) owns the `Arc<DockerCtl>`,
//! runs [`DockerCtl::self_setup`] at startup and from the setup wizard, and serves the
//! [`EnvReport`] as `GET /api/setup/env`.
//!
//! Design notes carried from the port's global context:
//! - Addressing is Docker DNS, not static IPs: on the user-defined `rmng` bridge the
//!   embedded resolver serves every container's *name* (== host id), and the
//!   control-server attaches itself under the [`CONTROL_ALIAS`] network alias (so the
//!   URLs baked into clones survive it being recreated). Clone IPs are plain Docker
//!   IPAM — nothing allocates or stores them; dev mode (server on the host) is the one
//!   consumer of raw IPs, via [`DockerCtl::inspect_ip`] / the subnet gateway.
//! - bollard exec output is chunk-, not line-aligned — [`LineSplitter`] reassembles
//!   complete lines per stream before the caller's callback fires (gotcha #1).
//! - tar uid/gid is applied verbatim by the daemon; callers set uid/gid 1000 on
//!   `home/rmng/**` entries (gotcha #2).
//! - Clone images need `StopSignal=SIGRTMIN+3` baked in or every stop is a 20 s hang +
//!   SIGKILL (gotcha #5); [`DockerCtl::commit`] with `set_boot_config` does this.
//! - Stale same-named containers 409 on create — callers get the daemon message
//!   verbatim (gotcha #7).

use std::collections::HashMap;
use std::net::Ipv4Addr;

use anyhow::{Context, Result, anyhow, bail};
use bollard::Docker;
use bollard::container::LogOutput;
use bollard::errors::Error as BollardError;
use bollard::exec::{CreateExecOptions, StartExecResults};
use bollard::models::{
    ContainerConfig, ContainerCreateBody, ContainerInspectResponse, EndpointSettings, HostConfig,
    Ipam, IpamConfig, Mount, MountBindOptions, MountBindOptionsPropagationEnum, MountPointTypeEnum,
    MountTypeEnum, NetworkConnectRequest, NetworkCreateRequest, NetworkingConfig, RestartPolicy,
    RestartPolicyNameEnum, VolumeCreateOptions,
};
use bollard::query_parameters::{
    CommitContainerOptionsBuilder, CreateContainerOptionsBuilder, CreateImageOptionsBuilder,
    ListImagesOptionsBuilder, RemoveContainerOptionsBuilder, RemoveImageOptionsBuilder,
    RemoveVolumeOptionsBuilder, StatsOptionsBuilder, StopContainerOptionsBuilder,
};
use futures::StreamExt;
use serde::{Deserialize, Serialize};
use tokio::io::AsyncWriteExt;
use tokio::sync::RwLock;
use wire::{ContainerStats, DockerConfig, EnvCheckRow, ExecResult, ImageInfo, SetupEnv, UpdateStatus};

// --- Constants ------------------------------------------------------------------------

/// The user-defined bridge every clone (and the control-server) attaches to. Created
/// lazily at wizard finish + before each clone; its subnet is one-time config.
pub const NETWORK: &str = "rmng";
/// The control-server's DNS alias on the [`NETWORK`] bridge. Clones dial the baked
/// `RMNG_CONTROL_URL`/`AGENT_CONTROL_MCP_URL` through this name, so the operator's
/// container name doesn't matter and recreating the container never strands the URLs.
pub const CONTROL_ALIAS: &str = "rmng-control";
/// The in-clone Linux user the agent + desktop run as (uid 1000).
pub const CLONE_USER: &str = "rmng";
/// Stop timeout for systemd-PID-1 clones (with `StopSignal=SIGRTMIN+3` baked in).
pub const STOP_TIMEOUT_SECS: i32 = 20;

/// Label marking an image as a clone source (shown in the image picker).
pub const LABEL_IMAGE: &str = "rmng.image";
/// Label marking the wizard-built base image.
pub const LABEL_BASE: &str = "rmng.base";
/// Label recording the reference an image was committed from (lineage).
pub const LABEL_CREATED_FROM: &str = "rmng.created-from";
/// Label stamped on every RMNG-managed container (clone + build workers).
pub const LABEL_MANAGED: &str = "rmng.managed";
/// Label stamped on RMNG shared-infra containers (`rmng-registry`, `rmng-buildkit`).
/// Deliberately NOT `rmng.managed` — infra is excluded from clone sweeps
/// (`list_managed_containers`) and the boot reconcile's "unknown managed container" warning,
/// exactly like the `rmng-self-upgrade` helper.
pub const LABEL_INFRA: &str = "rmng.infra";
/// Config fingerprint stamped on an infra container whose spec has settings beyond its
/// image (e.g. `rmng-buildkit`'s GC cap baked into `buildkitd.toml`). A change here forces
/// a recreate even when the image tag is unchanged, so a `buildkit_cache_gb` bump actually
/// takes effect. Absent on containers with no such settings (e.g. `rmng-registry`).
pub const LABEL_INFRA_CONFIG: &str = "rmng.infra-config";

/// The clone-daemon media socket bind target inside every clone.
const SOCK_DIR: &str = "/srv/rmng-sock";
/// Where each clone's per-clone named volume mounts (inner Docker state — never
/// committed, see gotcha #11).
const DIND_TARGET: &str = "/var/lib/docker";
/// Docker ≥28's containerd image store — needs its own volume for the same
/// overlay-on-overlay reason as [`DIND_TARGET`].
const CTD_TARGET: &str = "/var/lib/containerd";
/// Extra swap over the memory limit, in bytes (+8 GiB, matching LXC parity).
const SWAP_BYTES: i64 = 8 * 1024 * 1024 * 1024;

/// The host directory lxcfs serves its cgroup-aware `/proc` replacements from, when the
/// optional `lxcfs` package is installed + mounted on the Docker host.
const LXCFS_PROC_DIR: &str = "/var/lib/lxcfs/proc";
/// The proc files lxcfs virtualizes; each is bound over a clone's matching `/proc/<file>`
/// (by [`lxcfs_proc_mounts`]) so `free`/`nproc`/`htop` reflect the clone's cgroup limits
/// instead of the host totals. `meminfo` doubles as the availability-probe target.
const LXCFS_PROC_FILES: [&str; 6] = ["meminfo", "cpuinfo", "stat", "uptime", "loadavg", "swaps"];
/// The single lxcfs file the availability probe stats — its presence ⇒ lxcfs is mounted
/// and [`lxcfs_proc_mounts`] may safely bind the clone `/proc` files.
const LXCFS_PROBE_FILE: &str = "/var/lib/lxcfs/proc/meminfo";
/// Cap on how long the managed-mode lxcfs probe waits for its throwaway container to exit.
/// A `test -f` is near-instant; this only guards against a wedged daemon so `self_setup`
/// can never hang on the probe. On timeout the probe is treated as absent and the
/// container force-removed.
const LXCFS_PROBE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(15);

// --- Report ---------------------------------------------------------------------------

/// The self-setup verdict, filled by [`DockerCtl::self_setup`] and served as
/// `GET /api/setup/env` (via [`EnvReport::to_setup_env`]) + consumed by
/// `config_test("docker")`. Also carries internal facts (control IP, self-container id)
/// downstream phases read directly.
#[derive(Debug, Clone, Default)]
pub struct EnvReport {
    /// Docker daemon reachable (ping ok).
    pub daemon_ok: bool,
    /// Daemon version string (`Version`/`ApiVersion`), for the detail line.
    pub daemon_version: Option<String>,
    /// Why the daemon row failed (client build / connect error), when `!daemon_ok` —
    /// e.g. "Socket not found: /var/run/docker.sock" on a bare `docker run` without the
    /// sock bind. Rendered by the wizard via `GET /api/setup/env`.
    pub daemon_detail: Option<String>,
    /// The control-server's own container id (full 64-hex) when running inside Docker;
    /// `None` = dev mode (running on the host directly).
    pub self_container: Option<String>,
    /// What clones reach the control-server as: the [`CONTROL_ALIAS`] DNS name when the
    /// server runs as a container on the rmng bridge; the bridge gateway IP (`.1`) in dev
    /// mode (server on the host — clones can't resolve a host process by name).
    pub control_host: Option<String>,
    /// Why the `rmng` network setup / self-attach step failed, when it did (only attempted
    /// once `setup_complete`). `None` = succeeded or not attempted. Non-fatal to the report
    /// (it's not in [`required_ok`]); the wizard-finish path surfaces it as a `networkWarning`
    /// so a fresh deploy learns its clones' baked control URL won't resolve yet.
    pub network_detail: Option<String>,
    /// The shared clone-socket mount is present on our own container (required).
    pub sock_mount_ok: bool,
    /// Detail for the sock-mount row (the discovered source path, or why it's missing).
    pub sock_mount_detail: String,
    /// `/dev/dri/renderD128` exists (required for the media/streaming plane).
    pub dri_ok: bool,
    /// lxcfs is installed on the Docker host (optional) — probed by
    /// [`DockerCtl::probe_lxcfs`]. When true, new clones get lxcfs's cgroup-aware `/proc`
    /// files bound over their own so `free`/`nproc`/`htop` reflect the clone's limits;
    /// when false, clones keep host-wide `/proc` (graceful degradation). Not a *required*
    /// check — absence is advisory only.
    pub lxcfs_ok: bool,
}

impl EnvReport {
    /// True when nothing *required* failed. `self_container = None` (dev mode) is an
    /// informational state, never a failure.
    pub fn required_ok(&self) -> bool {
        self.daemon_ok && self.sock_mount_ok && self.dri_ok
    }

    /// Project into the wire DTO `GET /api/setup/env` returns. Rows, in order: daemon
    /// reachability, self-container detection (info — absence = dev mode), sock-mount
    /// presence (required), `/dev/dri/renderD128` presence (required), lxcfs availability
    /// (advisory — absence = clones see host-wide `/proc`).
    pub fn to_setup_env(&self) -> SetupEnv {
        let daemon_detail = match (&self.daemon_ok, &self.daemon_version) {
            (true, Some(v)) => format!("Docker {v}"),
            (true, None) => "reachable".to_string(),
            (false, _) => self
                .daemon_detail
                .clone()
                .unwrap_or_else(|| "cannot reach the Docker daemon over the configured socket".to_string()),
        };
        let self_detail = match &self.self_container {
            Some(id) => format!("container {}", short_id(id)),
            None => "not running inside a container (dev mode)".to_string(),
        };
        SetupEnv {
            rows: vec![
                EnvCheckRow {
                    id: "dockerDaemon".into(),
                    label: "Docker daemon".into(),
                    ok: self.daemon_ok,
                    detail: daemon_detail,
                    required: true,
                },
                EnvCheckRow {
                    id: "selfContainer".into(),
                    label: "Control-server container".into(),
                    // Info row: dev mode is a legitimate state, not a failure.
                    ok: true,
                    detail: self_detail,
                    required: false,
                },
                EnvCheckRow {
                    id: "sockMount".into(),
                    label: "Clone media socket mount".into(),
                    ok: self.sock_mount_ok,
                    detail: self.sock_mount_detail.clone(),
                    required: true,
                },
                EnvCheckRow {
                    id: "renderNode".into(),
                    label: "GPU render node (/dev/dri/renderD128)".into(),
                    ok: self.dri_ok,
                    detail: if self.dri_ok {
                        "present".into()
                    } else {
                        "/dev/dri/renderD128 not found — the video plane needs a render node".into()
                    },
                    required: true,
                },
                EnvCheckRow {
                    id: "lxcfs".into(),
                    label: "LXCFS (clones see their own CPU/RAM limits)".into(),
                    ok: self.lxcfs_ok,
                    // Advisory: absence just means clones see host-wide /proc.
                    detail: if self.lxcfs_ok {
                        "present".into()
                    } else {
                        "not installed on the Docker host — clones see host-wide /proc values; apt install lxcfs".into()
                    },
                    required: false,
                },
            ],
        }
    }
}

// --- DockerCtl ------------------------------------------------------------------------

/// The bollard client + the latest self-setup verdict. Cheap to `clone` the `Arc` around
/// it; `App` holds one `Arc<DockerCtl>` for the process lifetime.
pub struct DockerCtl {
    /// The bollard client, or the client-build error (e.g. the socket file doesn't
    /// exist — bollard checks the path in `connect_with_unix`). [`Self::daemon`] retries
    /// on every call, so the server boots without Docker (bare `docker run`, no sock
    /// bind) and a socket that appears later (dev: dockerd restart) heals without a
    /// server restart. std (not tokio) lock: never held across an await.
    client: std::sync::RwLock<Result<Docker, String>>,
    /// The resolved daemon socket path (config `docker.socket`, default applied).
    socket: String,
    /// The user-configured subnet (validated `/16`–`/24` IPv4 CIDR at config merge).
    /// Interior-mutable so a wizard subnet change reaches the lazily-materialized `rmng`
    /// bridge: `config_put` pushes it via [`Self::set_subnet`] before the network is created
    /// (otherwise the bridge would be built with the boot-time default and every later boot
    /// would reject the mismatched network).
    subnet: std::sync::RwLock<String>,
    env: RwLock<EnvReport>,
}

/// The set of things needed to create a clone container. `provision.rs` fills this from
/// config + the chosen image. No IP: the clone joins the rmng bridge under plain Docker
/// IPAM and is addressed by its name via the embedded DNS.
#[derive(Debug, Clone)]
pub struct CreateSpec {
    /// Container name (= host id, DNS-label-safe): e.g. `pega-dev-123`.
    pub name: String,
    /// Source image reference or id.
    pub image: String,
    /// Hostname inside the container (usually == `name`).
    pub hostname: String,
    /// Session env baked at create (`KEY=VALUE` pairs → container `Env`).
    pub env: Vec<(String, String)>,
    /// CPU limit (whole cores → `nano_cpus`).
    pub cpus: u32,
    /// Memory limit in MiB (+8 GiB swap).
    pub memory_mb: u32,
    /// The shared clone media socket *directory* on the host to bind at `/srv/rmng-sock`.
    /// The daemon path is `<this>/clones.sock`; empty skips the mount (dev/test).
    pub sock_source: String,
}

/// A desired shared-infra container, the input to [`DockerCtl::ensure_infra_container`].
struct InfraSpec {
    name: &'static str,
    image: String,
    /// Args appended to the image ENTRYPOINT (`None` = image default).
    cmd: Option<Vec<String>>,
    env: Vec<String>,
    mounts: Vec<Mount>,
    privileged: bool,
    /// Files dropped into the created-but-not-started container (e.g. `buildkitd.toml`).
    files: Vec<TarEntry>,
    /// Fingerprint of image-independent config baked into this container (e.g. the buildkit
    /// GC cap). When it differs from the running container's `rmng.infra-config` label, the
    /// container is recreated even if the image is unchanged. `None` = image is the only spec.
    config_fingerprint: Option<String>,
}

/// A captured control-server run-spec: everything `create_container` needs to recreate our
/// own container, projected from a live self-inspect. Serialized into the update handoff
/// file so the `self-upgrade` helper (a fresh process from the new image) can recreate us.
/// The only field NOT copied from the running container is the image — overridden to
/// `new_image_ref`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SelfSpec {
    /// The container name (== how the deployment named us), leading `/` stripped.
    pub container_name: String,
    /// The image the recreated container runs (the newly pulled ref).
    pub new_image_ref: String,
    /// The image id we were running before the swap (for the create-error fallback).
    pub old_image_id: String,
    /// Captured `Config` (hostname/env/labels/exposed_ports/stop_signal/stop_timeout/…).
    pub config: ContainerConfig,
    /// Captured `HostConfig` (privileged/pid_mode/init/mounts/port_bindings/restart_policy/…).
    pub host_config: HostConfig,
    /// Captured network attachments + aliases (preserves the rmng-control alias).
    pub networks: HashMap<String, EndpointSettings>,
}

impl SelfSpec {
    /// Project a container inspect into a `SelfSpec`, overriding the image to `new_image_ref`.
    /// Pure (no I/O) so it's unit-testable against a fixture inspect.
    pub fn from_inspect(resp: &ContainerInspectResponse, new_image_ref: &str) -> Result<SelfSpec> {
        let container_name =
            resp.name.clone().unwrap_or_default().trim_start_matches('/').to_string();
        let old_image_id = resp.image.clone().ok_or_else(|| anyhow!("inspect has no image id"))?;
        let config = resp.config.clone().ok_or_else(|| anyhow!("inspect has no config"))?;
        let host_config =
            resp.host_config.clone().ok_or_else(|| anyhow!("inspect has no host_config"))?;
        let networks = resp
            .network_settings
            .as_ref()
            .and_then(|n| n.networks.clone())
            .unwrap_or_default();
        Ok(SelfSpec {
            container_name,
            new_image_ref: new_image_ref.to_string(),
            old_image_id,
            config,
            host_config,
            networks,
        })
    }
}

/// One RMNG-managed container as the daemon lists it (label `rmng.managed=1`): clones
/// (name == host id) and `rmng-build-*` workers. See
/// [`DockerCtl::list_managed_containers`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManagedContainer {
    /// Container name (`/`-prefix stripped); falls back to the id if nameless.
    pub name: String,
    /// The image reference the container was created from, as `docker ps` shows it.
    pub image: String,
    pub running: bool,
}

/// The running control-server image's identity, read from its own container → image.
/// `repo_digest` is the `repo@sha256:…` for the configured repo (the update-check
/// baseline); `revision`/`created` are the OCI labels stamped by scripts/publish-server.sh.
#[derive(Debug, Clone, Default)]
pub struct ServerImageInfo {
    pub repo_digest: Option<String>,
    pub revision: Option<String>,
    pub created: Option<String>,
}

/// One file to drop into a container via `upload_tar`. `mode` is the raw unix mode
/// (e.g. `0o644`); `uid`/`gid` are applied verbatim by the daemon (gotcha #2 — callers
/// pass 1000 for `home/rmng/**`).
#[derive(Debug, Clone)]
pub struct TarEntry {
    /// In-archive path, relative, no leading slash (e.g. `home/rmng/.config/foo`).
    pub path: String,
    pub data: Vec<u8>,
    pub mode: u32,
    pub uid: u64,
    pub gid: u64,
}

/// One event from [`DockerCtl::pull_image`]'s stream. `info.error` frames are surfaced as
/// a hard `Err` instead (e.g. Docker Hub rate limits — gotcha #9), never as an event.
#[derive(Debug, Clone, PartialEq)]
pub enum PullEvent {
    /// A per-(layer, status) transition — one line per transition (not per byte tick),
    /// deduped exactly as the pre-rework callback did. `layer` is the daemon's short layer
    /// id (empty for the odd status line that isn't layer-scoped, e.g. `Pulling from
    /// library/ubuntu`).
    Status { layer: String, status: String },
    /// Aggregate download+extract byte progress across all layers, monotonic and
    /// throttled to integer-percent changes. See [`PullAggregator`].
    Bytes { frac: f64 },
}

impl DockerCtl {
    /// Build the client holder from config. Infallible and I/O-free — the server must
    /// boot the wizard even when Docker is absent entirely (bare `docker run` without
    /// the sock bind). A failed client build (missing socket file) is stored; every
    /// daemon-touching call surfaces it via [`Self::daemon`], and `self_setup` reports
    /// it as the failing `dockerDaemon` env row.
    pub fn connect(cfg: &DockerConfig) -> Self {
        let socket = cfg.socket.trim();
        let socket = if socket.is_empty() { "/var/run/docker.sock" } else { socket }.to_string();
        let client = build_client(&socket).map_err(|e| {
            tracing::warn!(target: "docker", "{e:#} — booting anyway; the setup wizard shows the failure");
            format!("{e:#}")
        });
        Self {
            client: std::sync::RwLock::new(client),
            socket,
            subnet: std::sync::RwLock::new(cfg.subnet.clone()),
            env: RwLock::new(EnvReport::default()),
        }
    }

    /// Refresh the cached subnet from config. The `rmng` bridge is materialized lazily
    /// (wizard finish / first clone), so the subnet the operator sets in the wizard must be
    /// pushed here *before* that happens — see the field doc. Called from `config_put` on
    /// every config write, so the long-lived ctl never drifts from the persisted config.
    pub fn set_subnet(&self, subnet: &str) {
        *self.subnet.write().unwrap() = subnet.to_string();
    }

    /// The bollard client (cheap `Arc` clone), rebuilding it first if the initial build
    /// failed — so a socket that appears after boot starts working without a restart.
    /// All daemon-touching methods go through this; the `Err` carries the build failure.
    fn daemon(&self) -> Result<Docker> {
        if let Ok(d) = &*self.client.read().unwrap() {
            return Ok(d.clone());
        }
        let mut slot = self.client.write().unwrap();
        if let Ok(d) = &*slot {
            return Ok(d.clone()); // another caller won the retry race
        }
        match build_client(&self.socket) {
            Ok(d) => {
                tracing::info!(target: "docker", "Docker client connected at {}", self.socket);
                *slot = Ok(d.clone());
                Ok(d)
            }
            Err(e) => {
                *slot = Err(format!("{e:#}"));
                Err(e)
            }
        }
    }

    /// The latest self-setup verdict (a clone of the internal report).
    pub async fn env(&self) -> EnvReport {
        self.env.read().await.clone()
    }

    /// What clones reach the control-server as: the [`CONTROL_ALIAS`] DNS name normally,
    /// or the bridge gateway IP in dev mode (server on the host, not a container on the
    /// bridge). Reads the cached report; falls back to computing the gateway from the
    /// subnet if `self_setup` hasn't run.
    pub async fn control_host(&self) -> Result<String> {
        if let Some(host) = self.env.read().await.control_host.clone() {
            return Ok(host);
        }
        // Not yet probed: derive from config (dev-mode gateway is the safe default until
        // a self-container is detected).
        let subnet = self.subnet.read().unwrap().clone();
        let plan = SubnetPlan::parse(&subnet)?;
        Ok(plan.gateway().to_string())
    }

    // --- self-setup -------------------------------------------------------------------

    /// Probe the environment and refresh [`EnvReport`]. Called at startup, from
    /// `config_test("docker")`, and at wizard finish. Steps:
    /// 1. ping + version (daemon reachable?),
    /// 2. self-detect our own container id (hostname inspect → `/proc/self/mountinfo`
    ///    fallback → none = dev mode),
    /// 3. `ensure_network()` **only when** `setup_complete` (network is lazy),
    /// 4. control host = the [`CONTROL_ALIAS`] DNS name (managed) / the gateway IP (dev
    ///    mode); connect self under the alias when both a self-container and the network
    ///    exist,
    /// 5. sock-mount discovery from our own mounts (required),
    /// 6. `dri_ok` = `/dev/dri/renderD128` exists,
    /// 7. `lxcfs_ok` = lxcfs installed on the host (optional — junk-free probe).
    ///
    /// Never bails on a down daemon — it records the failure in the report so the wizard
    /// can render it. `setup_complete` is passed in by the caller (`App` reads config). A
    /// step 3/4 failure is non-fatal too — recorded in `report.network_detail` (not in
    /// [`EnvReport::required_ok`]) so the wizard-finish caller can surface it as a warning
    /// while the remaining steps still run.
    pub async fn self_setup(&self, setup_complete: bool) -> EnvReport {
        let mut report = EnvReport::default();

        // 1. daemon reachable? (`daemon()` also covers "client never built" — e.g. the
        // socket file doesn't exist at all on a bare `docker run` without the bind.)
        let version = match self.daemon() {
            Ok(d) => d.version().await.map_err(anyhow::Error::from),
            Err(e) => Err(e),
        };
        match version {
            Ok(v) => {
                report.daemon_ok = true;
                report.daemon_version = match (v.version, v.api_version) {
                    (Some(ver), Some(api)) => Some(format!("{ver} (API {api})")),
                    (Some(ver), None) => Some(ver),
                    (None, Some(api)) => Some(format!("API {api}")),
                    (None, None) => None,
                };
            }
            Err(e) => {
                tracing::warn!(target: "docker", "daemon unreachable: {e:#}");
                // Nothing else can be probed against a dead daemon; still fill the
                // static bits (control IP from config, DRI from the fs) so the wizard
                // shows what it can.
                report.daemon_detail = Some(format!("{e:#}"));
                let subnet = self.subnet.read().unwrap().clone();
                report.control_host = SubnetPlan::parse(&subnet).ok().map(|p| p.gateway().to_string());
                report.dri_ok = std::path::Path::new("/dev/dri/renderD128").exists();
                report.sock_mount_detail = "Docker daemon unreachable".into();
                *self.env.write().await = report.clone();
                return report;
            }
        }

        // 2. self-detect container id.
        report.self_container = self.detect_self_container().await;

        // 3. lazy network: only materialize once setup is latched complete.
        if setup_complete {
            if let Err(e) = self.ensure_network().await {
                tracing::warn!(target: "docker", "ensure_network failed during self_setup: {e}");
                report.network_detail = Some(format!("{e}"));
            }
        }

        // 4. control host + connect self under the DNS alias (managed clone-fleet mode).
        if let Some(id) = &report.self_container {
            report.control_host = Some(CONTROL_ALIAS.to_string());
            // Best-effort: attach ourselves to the network under the alias so baked
            // RMNG_CONTROL_URLs resolve. Only meaningful once the network exists.
            if setup_complete {
                if let Err(e) = self.connect_self_to_network(id).await {
                    tracing::warn!(target: "docker", "connect self to {NETWORK} as {CONTROL_ALIAS} failed: {e}");
                    // Don't clobber an earlier ensure_network failure (it's the root cause).
                    report.network_detail.get_or_insert_with(|| {
                        format!("attaching the control-server to the {NETWORK} network as {CONTROL_ALIAS} failed: {e}")
                    });
                }
            }
        } else {
            // dev mode: the server is on the host; clones reach it via the gateway IP
            // (they can't resolve a host process by name).
            let subnet = self.subnet.read().unwrap().clone();
            match SubnetPlan::parse(&subnet) {
                Ok(plan) => report.control_host = Some(plan.gateway().to_string()),
                Err(e) => tracing::warn!(target: "docker", "subnet {subnet:?} unparseable: {e}"),
            }
        }

        // 5. sock-mount discovery from our own container's mounts.
        let (ok, detail) = self.discover_sock_mount(report.self_container.as_deref()).await;
        report.sock_mount_ok = ok;
        report.sock_mount_detail = detail;

        // 6. render node.
        report.dri_ok = std::path::Path::new("/dev/dri/renderD128").exists();

        // 7. lxcfs (optional): can each clone see its own cgroup limits in /proc? Probed
        // without creating host-side junk — see `probe_lxcfs`. Absence degrades gracefully
        // (clones keep host-wide /proc); a later `apt install lxcfs` + restart re-probes.
        report.lxcfs_ok = self.probe_lxcfs(report.self_container.as_deref()).await;

        *self.env.write().await = report.clone();
        report
    }

    /// Detect the control-server's own container id. First tries the kernel hostname
    /// (Docker sets it to the short container id) via a container inspect; falls back to
    /// scanning `/proc/self/mountinfo` for `/var/lib/docker/containers/<64hex>/`. Returns
    /// `None` on the host (dev mode).
    async fn detect_self_container(&self) -> Option<String> {
        // Best-effort client: only called after the daemon check, but degrade to the
        // unconfirmed mountinfo fallback rather than aborting if it's gone.
        let docker = self.daemon().ok();
        // Hostname == short container id under Docker's default config.
        if let Ok(host) = std::env::var("HOSTNAME").or_else(|_| read_hostname()) {
            let host = host.trim();
            if host.len() >= 12 && host.bytes().all(|b| b.is_ascii_hexdigit()) {
                if let Some(d) = &docker {
                    if let Ok(info) = d.inspect_container(host, None::<bollard::query_parameters::InspectContainerOptions>).await {
                        if let Some(id) = info.id {
                            return Some(id);
                        }
                    }
                }
            }
        }
        // Fallback: our own cgroup/overlay path names the container id.
        if let Ok(mountinfo) = std::fs::read_to_string("/proc/self/mountinfo") {
            if let Some(id) = extract_container_id_from_mountinfo(&mountinfo) {
                // Confirm the id is real before trusting it.
                if let Some(d) = &docker {
                    if let Ok(info) = d.inspect_container(&id, None::<bollard::query_parameters::InspectContainerOptions>).await {
                        if let Some(cid) = info.id {
                            return Some(cid);
                        }
                    }
                }
                return Some(id);
            }
        }
        None
    }

    /// Whether lxcfs is available on the Docker host — i.e. [`LXCFS_PROBE_FILE`] exists —
    /// so [`lxcfs_proc_mounts`] may bind the clone `/proc` files. Cached on
    /// [`EnvReport::lxcfs_ok`] and re-probed on every `self_setup` (boot / config test /
    /// wizard finish), so installing lxcfs then restarting the server (or Settings→Test)
    /// picks it up with no extra plumbing. Any docker error ⇒ treated as absent (logged),
    /// never fatal to boot.
    ///
    /// Two probe paths, both junk-free:
    /// - **Dev mode** (server on the host, `self_container = None`): stat the path directly
    ///   — same idiom as `dri_ok`, and it's the exact host path a clone bind would source
    ///   from, so it predicts the clone binds precisely.
    /// - **Managed mode** (server in a container): our mount namespace can't see the host
    ///   fs, and a docker bind whose source is missing would silently CREATE a directory
    ///   there — poisoning later detection and breaking clone starts (a dir bound over a
    ///   proc file). So we probe from a throwaway container off our OWN image (guaranteed
    ///   present on the daemon) that stats the host path through a read-only bind of `/`
    ///   (binding `/` can never create junk).
    async fn probe_lxcfs(&self, self_container: Option<&str>) -> bool {
        match self_container {
            None => std::path::Path::new(LXCFS_PROBE_FILE).exists(),
            Some(id) => match self.lxcfs_container_probe(id).await {
                Ok(present) => present,
                Err(e) => {
                    tracing::warn!(target: "docker", "lxcfs probe failed (treating as absent): {e:#}");
                    false
                }
            },
        }
    }

    /// The managed-mode lxcfs probe: run a short-lived container off our own image that
    /// `test -f`s the host lxcfs file via a read-only bind of `/`. Returns whether it
    /// exited 0 (file present). The probe container is deterministically named per
    /// control-server and force-removed after — a crashed earlier run is reclaimed by the
    /// pre-clean, so it never leaves host-side junk. Deliberately NOT `rmng.managed`-
    /// labeled: it's ephemeral infra, not a fleet member (keeps it out of the managed
    /// listings in `web`/`homes`). We do our own removal (not `auto_remove`) so the exit
    /// code can be read without racing the daemon's autoremoval of a sub-second container.
    async fn lxcfs_container_probe(&self, self_id: &str) -> Result<bool> {
        let docker = self.daemon()?;
        // Our own image (the digest id `Image` reports) — guaranteed present on the
        // daemon, and ubuntu-based so `sh`/`test` exist.
        let image = docker
            .inspect_container(self_id, None::<bollard::query_parameters::InspectContainerOptions>)
            .await
            .context("inspecting self for the lxcfs probe image")?
            .image
            .ok_or_else(|| anyhow!("self container has no image id"))?;

        let name = format!("rmng-lxcfs-probe-{}", short_id(self_id));
        // Reclaim a probe container a crashed earlier run may have left (idempotent, 404-ok).
        let _ = self.remove_container(&name).await;

        let host_config = HostConfig {
            // Read-only bind of the host root: the source `/` always exists (never creates
            // junk) and the probe only READS `/host/var/lib/lxcfs/proc/meminfo` through it.
            binds: Some(vec!["/:/host:ro".to_string()]),
            // No network needed; keep the probe off the rmng bridge entirely.
            network_mode: Some("none".to_string()),
            ..Default::default()
        };
        let body = ContainerCreateBody {
            image: Some(image),
            // `sh -c` uses the shell's builtin `test`, so it works regardless of coreutils
            // layout; exit 0 ⇒ file present ⇒ lxcfs available.
            entrypoint: Some(vec![
                "sh".to_string(),
                "-c".to_string(),
                format!("test -f /host{LXCFS_PROBE_FILE}"),
            ]),
            cmd: Some(Vec::new()), // clear any inherited Cmd
            host_config: Some(host_config),
            ..Default::default()
        };
        let opts = CreateContainerOptionsBuilder::new().name(&name).build();
        let id = docker
            .create_container(Some(opts), body)
            .await
            .context("creating the lxcfs probe container")?
            .id;

        // Run + read the exit code, then ALWAYS remove the probe container (any outcome).
        let outcome = self.wait_lxcfs_probe(&docker, &id).await;
        let _ = self.remove_container(&id).await;
        outcome
    }

    /// Start the lxcfs probe container and read its exit code. `Ok(true)` = exited 0 (file
    /// present); `Ok(false)` = exited non-zero (absent — bollard surfaces a non-zero exit
    /// as [`BollardError::DockerContainerWaitError`], an expected quiet result, not a probe
    /// failure); `Err` only for genuine transport failures the caller logs. No `auto_remove`
    /// on the container, so the exited-but-present container still yields its stored exit
    /// code here without racing removal.
    async fn wait_lxcfs_probe(&self, docker: &Docker, id: &str) -> Result<bool> {
        docker
            .start_container(id, None::<bollard::query_parameters::StartContainerOptions>)
            .await
            .context("starting the lxcfs probe container")?;
        let mut waits =
            docker.wait_container(id, None::<bollard::query_parameters::WaitContainerOptions>);
        let frame = tokio::time::timeout(LXCFS_PROBE_TIMEOUT, waits.next())
            .await
            .map_err(|_| anyhow!("lxcfs probe container did not exit within {LXCFS_PROBE_TIMEOUT:?}"))?;
        match frame {
            Some(Ok(resp)) => Ok(resp.status_code == 0),
            // test = 1 (absent) or 127 (no `test`) — both mean "not available", quietly.
            Some(Err(BollardError::DockerContainerWaitError { .. })) => Ok(false),
            Some(Err(e)) => Err(e.into()),
            None => Err(anyhow!("lxcfs probe wait returned no frames")),
        }
    }

    /// Discover the host source of the shared clone-socket mount from our own container's
    /// mount list (a `Volume` or `Bind` whose destination is under `/srv/rmng-sock`).
    /// Required: an error row (`ok=false`) if we're a container without it. In dev mode
    /// (no self-container) the mount lives on the host fs — checked directly.
    async fn discover_sock_mount(&self, self_id: Option<&str>) -> (bool, String) {
        let Some(id) = self_id else {
            // Dev mode: no container to inspect; the socket dir is just a host path.
            let present = std::path::Path::new(SOCK_DIR).exists();
            return if present {
                (true, format!("{SOCK_DIR} present on host (dev mode)"))
            } else {
                (true, "dev mode — clone socket materialized at runtime".into())
            };
        };
        let docker = match self.daemon() {
            Ok(d) => d,
            Err(e) => return (false, format!("could not inspect self container: {e:#}")),
        };
        match docker.inspect_container(id, None::<bollard::query_parameters::InspectContainerOptions>).await {
            Ok(info) => {
                let found = info.mounts.unwrap_or_default().into_iter().find(|m| {
                    matches!(m.typ, Some(MountPointTypeEnum::VOLUME) | Some(MountPointTypeEnum::BIND))
                        && m.destination.as_deref().map(|d| d == SOCK_DIR || d.starts_with(&format!("{SOCK_DIR}/"))).unwrap_or(false)
                });
                match found {
                    Some(m) => {
                        let src = m.source.clone().or(m.name.clone()).unwrap_or_default();
                        (true, format!("mounted from {src}"))
                    }
                    None => (
                        false,
                        format!("no mount at {SOCK_DIR} — add `-v <host-sock-dir>:{SOCK_DIR}` to the control-server container"),
                    ),
                }
            }
            Err(e) => (false, format!("could not inspect self container: {e}")),
        }
    }

    /// Attach our own container to the rmng network under the [`CONTROL_ALIAS`] DNS
    /// alias (idempotent — an already-connected endpoint is fine). NOTE: if the endpoint
    /// already exists WITHOUT the alias (an attach from an older build), Docker keeps the
    /// old endpoint; `docker network disconnect rmng <ctr>` once and re-run setup.
    async fn connect_self_to_network(&self, self_id: &str) -> Result<()> {
        let cfg = NetworkConnectRequest {
            container: Some(self_id.to_string()),
            endpoint_config: Some(EndpointSettings {
                aliases: Some(vec![CONTROL_ALIAS.to_string()]),
                ..Default::default()
            }),
        };
        match self.daemon()?.connect_network(NETWORK, cfg).await {
            Ok(()) => Ok(()),
            // 403 = already connected; treat as success.
            Err(BollardError::DockerResponseServerError { status_code: 403, .. }) => Ok(()),
            Err(e) => Err(e.into()),
        }
    }

    // --- network ----------------------------------------------------------------------

    /// Create the `rmng` bridge if it doesn't already exist, with a static IPAM matching
    /// the configured subnet (gateway `.1`). Idempotent: a matching existing network is a
    /// no-op; an existing network with a *different* subnet errors (operator must
    /// `docker network rm rmng`). Called lazily — at wizard finish + before each clone.
    pub async fn ensure_network(&self) -> Result<()> {
        let subnet = self.subnet.read().unwrap().clone();
        let plan = SubnetPlan::parse(&subnet)?;
        // Already present? Verify its subnet matches.
        match self.daemon()?.inspect_network(NETWORK, None::<bollard::query_parameters::InspectNetworkOptions>).await {
            Ok(net) => {
                let existing = net
                    .ipam
                    .and_then(|i| i.config)
                    .and_then(|c| c.into_iter().next())
                    .and_then(|c| c.subnet);
                match existing {
                    Some(sn) if sn == plan.cidr() => {
                        tracing::debug!(target: "docker", "{NETWORK} network already present with subnet {sn}");
                        return Ok(());
                    }
                    Some(sn) => bail!(
                        "the `{NETWORK}` Docker network already exists with subnet {sn}, but config wants {}. \
                         Delete it with `docker network rm {NETWORK}` and re-run setup.",
                        plan.cidr()
                    ),
                    None => bail!(
                        "the `{NETWORK}` Docker network exists but has no IPv4 subnet; \
                         delete it with `docker network rm {NETWORK}` and re-run setup."
                    ),
                }
            }
            Err(BollardError::DockerResponseServerError { status_code: 404, .. }) => {} // not present → create
            Err(e) => return Err(anyhow!("inspecting the {NETWORK} network: {e}")),
        }

        let req = NetworkCreateRequest {
            name: NETWORK.to_string(),
            driver: Some("bridge".to_string()),
            enable_ipv4: Some(true),
            ipam: Some(Ipam {
                driver: Some("default".to_string()),
                config: Some(vec![IpamConfig {
                    subnet: Some(plan.cidr()),
                    gateway: Some(plan.gateway().to_string()),
                    ..Default::default()
                }]),
                ..Default::default()
            }),
            labels: Some(HashMap::from([(LABEL_MANAGED.to_string(), "1".to_string())])),
            ..Default::default()
        };
        self.daemon()?.create_network(req).await.with_context(|| format!("creating the {NETWORK} network"))?;
        tracing::info!(target: "docker", "created the {NETWORK} bridge with subnet {}", plan.cidr());
        Ok(())
    }

    /// Ensure the shared build-infra containers + volumes exist and run: `rmng-registry`
    /// (pull-through Docker Hub cache) and `rmng-buildkit` (shared BuildKit daemon), both on
    /// the `rmng` bridge, labeled `rmng.infra=1`, `restart: unless-stopped`. Idempotent:
    /// create-if-absent, start-if-stopped, recreate-if-image-drifted (cache volumes survive a
    /// recreate). MUST run after `ensure_network` (the containers attach to `NETWORK`).
    pub async fn ensure_build_infra(&self, cfg: &wire::DockerConfig) -> Result<()> {
        self.ensure_volume(crate::buildinfra::REGISTRY_DATA_VOL).await?;
        self.ensure_volume(crate::buildinfra::BUILDKIT_CACHE_VOL).await?;

        self.ensure_infra_container(InfraSpec {
            name: crate::buildinfra::REGISTRY_CONTAINER,
            image: cfg.registry_image.clone(),
            cmd: None,
            env: vec!["REGISTRY_PROXY_REMOTEURL=https://registry-1.docker.io".to_string()],
            mounts: vec![Mount {
                target: Some("/var/lib/registry".to_string()),
                source: Some(crate::buildinfra::REGISTRY_DATA_VOL.to_string()),
                typ: Some(MountTypeEnum::VOLUME),
                ..Default::default()
            }],
            privileged: false,
            files: vec![],
            config_fingerprint: None,
        })
        .await?;

        self.ensure_infra_container(InfraSpec {
            name: crate::buildinfra::BUILDKIT_CONTAINER,
            image: cfg.buildkit_image.clone(),
            // moby/buildkit's ENTRYPOINT is `buildkitd`; these are its args.
            cmd: Some(vec![
                "--addr".to_string(),
                "tcp://0.0.0.0:1234".to_string(),
                "--config".to_string(),
                "/etc/buildkit/buildkitd.toml".to_string(),
            ]),
            env: vec![],
            mounts: vec![Mount {
                target: Some("/var/lib/buildkit".to_string()),
                source: Some(crate::buildinfra::BUILDKIT_CACHE_VOL.to_string()),
                typ: Some(MountTypeEnum::VOLUME),
                ..Default::default()
            }],
            privileged: true,
            files: vec![TarEntry {
                path: "etc/buildkit/buildkitd.toml".to_string(),
                data: crate::buildinfra::render_buildkitd_toml(cfg.buildkit_cache_gb).into_bytes(),
                mode: 0o644,
                uid: 0,
                gid: 0,
            }],
            config_fingerprint: Some(cfg.buildkit_cache_gb.to_string()),
        })
        .await?;
        Ok(())
    }

    /// Ensure one infra container matches `spec`: create-if-absent (dropping `spec.files` in
    /// before start), start-if-stopped, recreate-if-image-drifted. Best-effort image pull
    /// first. Cache volumes are external (survive the recreate).
    async fn ensure_infra_container(&self, spec: InfraSpec) -> Result<()> {
        let docker = self.daemon()?;
        match docker
            .inspect_container(spec.name, None::<bollard::query_parameters::InspectContainerOptions>)
            .await
        {
            Ok(info) => {
                let running = info.state.as_ref().and_then(|s| s.running).unwrap_or(false);
                let cur_image =
                    info.config.as_ref().and_then(|c| c.image.clone()).unwrap_or_default();
                let cur_fingerprint = info
                    .config
                    .as_ref()
                    .and_then(|c| c.labels.as_ref())
                    .and_then(|l| l.get(LABEL_INFRA_CONFIG))
                    .cloned();
                if cur_image != spec.image || cur_fingerprint != spec.config_fingerprint {
                    tracing::info!(
                        target: "docker",
                        "{}: image {cur_image:?}→{:?} / config {cur_fingerprint:?}→{:?}, recreating",
                        spec.name, spec.image, spec.config_fingerprint
                    );
                    self.stop_container(spec.name).await.ok();
                    self.remove_container(spec.name).await.ok();
                    // fall through to (re)create
                } else if running {
                    return Ok(()); // present, correct image, running
                } else {
                    self.start_container(spec.name).await?; // present + correct but stopped
                    return Ok(());
                }
            }
            Err(BollardError::DockerResponseServerError { status_code: 404, .. }) => {} // absent
            Err(e) => return Err(anyhow!("inspecting infra container {}: {e}", spec.name)),
        }

        self.pull_if_absent(&spec.image).await?;

        let host_config = HostConfig {
            privileged: Some(spec.privileged),
            mounts: Some(spec.mounts.clone()),
            restart_policy: Some(RestartPolicy {
                name: Some(RestartPolicyNameEnum::UNLESS_STOPPED),
                ..Default::default()
            }),
            ..Default::default()
        };
        let body = ContainerCreateBody {
            image: Some(spec.image.clone()),
            cmd: spec.cmd.clone(),
            env: if spec.env.is_empty() { None } else { Some(spec.env.clone()) },
            labels: Some({
                let mut m = HashMap::from([(LABEL_INFRA.to_string(), "1".to_string())]);
                if let Some(fp) = &spec.config_fingerprint {
                    m.insert(LABEL_INFRA_CONFIG.to_string(), fp.clone());
                }
                m
            }),
            host_config: Some(host_config),
            networking_config: Some(NetworkingConfig {
                endpoints_config: Some(HashMap::from([(
                    NETWORK.to_string(),
                    EndpointSettings::default(),
                )])),
            }),
            ..Default::default()
        };
        let opts = CreateContainerOptionsBuilder::new().name(spec.name).build();
        let id = docker
            .create_container(Some(opts), body)
            .await
            .with_context(|| format!("creating infra container {}", spec.name))?
            .id;
        if !spec.files.is_empty() {
            // upload_tar works on a created-but-stopped container.
            self.upload_tar(&id, spec.files).await?;
        }
        self.start_container(&id).await?;
        tracing::info!(target: "docker", "ensured infra container {} ({})", spec.name, spec.image);
        Ok(())
    }

    /// Pull `reference` only if the daemon doesn't already have it (infra images are pinned;
    /// no need to re-pull each boot). Streams events into the void — infra pulls are silent.
    async fn pull_if_absent(&self, reference: &str) -> Result<()> {
        if self.daemon()?.inspect_image(reference).await.is_ok() {
            return Ok(());
        }
        tracing::info!(target: "docker", "pulling infra image {reference}");
        self.pull_image(reference, |_| {}).await
    }

    // --- images -----------------------------------------------------------------------

    /// Pull an image, streaming [`PullEvent`]s: `Status` deduped per-(layer, status)
    /// transition (the Operation log gets one line per transition, not per byte tick) and
    /// `Bytes` aggregate progress ticks throttled to integer-percent changes by
    /// [`PullAggregator`]. `info.error` is surfaced as a hard error verbatim (e.g. Docker
    /// Hub rate limits on `ubuntu:26.04`, gotcha #9).
    pub async fn pull_image(&self, reference: &str, mut on_event: impl FnMut(PullEvent)) -> Result<()> {
        let (image, tag) = split_reference(reference);
        let opts = CreateImageOptionsBuilder::new().from_image(&image).tag(&tag).build();
        let docker = self.daemon()?;
        let mut stream = docker.create_image(Some(opts), None, None);
        // Track the last status emitted per layer so we don't spam a line per byte.
        let mut last_status: HashMap<String, String> = HashMap::new();
        let mut aggregator = PullAggregator::default();
        while let Some(item) = stream.next().await {
            let info = item.with_context(|| format!("pulling {reference}"))?;
            if let Some(err) = info.error.filter(|e| !e.is_empty()) {
                bail!("pulling {reference}: {err}");
            }
            let id = info.id.clone().unwrap_or_default();
            let status = info.status.clone().unwrap_or_default();

            // Byte-progress ticks happen far more often than status transitions (many
            // ticks share the same "Downloading"/"Extracting" status), so this runs on
            // every frame, independent of the status dedup below.
            let (current, total) = info.progress_detail.map(|p| (p.current, p.total)).unwrap_or_default();
            if let Some(frac) = aggregator.observe(&id, &status, current, total) {
                on_event(PullEvent::Bytes { frac });
            }

            if status.is_empty() {
                continue;
            }
            // Emit once per (layer, status) transition.
            if last_status.get(&id).map(|s| s != &status).unwrap_or(true) {
                last_status.insert(id.clone(), status.clone());
                on_event(PullEvent::Status { layer: id, status });
            }
        }
        Ok(())
    }

    /// True if a local image with this reference/id exists.
    pub async fn image_exists(&self, reference: &str) -> Result<bool> {
        match self.daemon()?.inspect_image(reference).await {
            Ok(_) => Ok(true),
            Err(BollardError::DockerResponseServerError { status_code: 404, .. }) => Ok(false),
            Err(e) => Err(anyhow!("inspecting image {reference}: {e}")),
        }
    }

    /// An image's labels (`ImageInspect.Config.Labels`), or an empty map if it has none.
    /// The template-pull verify reads this to require `rmng.image=1` on the pulled image.
    pub async fn image_labels(&self, reference: &str) -> Result<HashMap<String, String>> {
        let info = self
            .daemon()?
            .inspect_image(reference)
            .await
            .with_context(|| format!("inspecting image {reference}"))?;
        Ok(info.config.and_then(|c| c.labels).unwrap_or_default())
    }

    /// The running control-server image's identity: inspect our own container to get its
    /// image id, then inspect that image for the RepoDigest matching `repo` + the OCI
    /// version labels. `repo` is the reference-without-tag of `docker.serverImage`.
    pub async fn self_image_info(&self, self_id: &str, repo: &str) -> Result<ServerImageInfo> {
        let docker = self.daemon()?;
        let ctr = docker
            .inspect_container(self_id, None::<bollard::query_parameters::InspectContainerOptions>)
            .await
            .context("inspecting self container for image info")?;
        let image_id = ctr.image.clone().ok_or_else(|| anyhow!("self container has no image id"))?;
        let img = docker.inspect_image(&image_id).await.context("inspecting self image")?;
        let labels = img.config.as_ref().and_then(|c| c.labels.clone()).unwrap_or_default();
        // RepoDigest for our repo, e.g. "pegasis0/rmng@sha256:…". Match on the repo prefix.
        let repo_digest = img
            .repo_digests
            .unwrap_or_default()
            .into_iter()
            .find(|rd| rd.starts_with(&format!("{repo}@")));
        Ok(ServerImageInfo {
            repo_digest,
            revision: labels.get("org.opencontainers.image.revision").cloned().filter(|s| !s.is_empty()),
            created: labels.get("org.opencontainers.image.created").cloned().filter(|s| !s.is_empty()),
        })
    }

    /// The LOCAL RepoDigest of a (just-pulled) image, trimmed to the bare `sha256:…`.
    /// Inspects `reference` and picks the `repo@sha256:…` entry for `reference`'s repo — the
    /// SAME source + shape [`self_image_info`] reads for the running container — so boot
    /// reconcile compares like-for-like. Unlike the registry descriptor digest (see
    /// [`Self::registry_digest`]), this is the platform image's own digest, which is what the
    /// recreated container reports for a multi-arch/index image. Best-effort: any failure
    /// (daemon down, image gone, no matching repo digest) yields `None`.
    pub async fn image_repo_digest(&self, reference: &str) -> Option<String> {
        let (repo, _tag) = split_reference(reference);
        let img = self.daemon().ok()?.inspect_image(reference).await.ok()?;
        img.repo_digests
            .unwrap_or_default()
            .into_iter()
            .find(|rd| rd.starts_with(&format!("{repo}@")))
            .map(|rd| rd.split_once('@').map(|(_, d)| d.to_string()).unwrap_or(rd))
    }

    /// The remote manifest digest of `reference` from the registry, WITHOUT pulling
    /// (Docker's `/distribution/{name}/json`). Returns the descriptor digest string
    /// (`sha256:…`). Surfaces registry errors verbatim (auth / rate-limit / not-found).
    pub async fn registry_digest(&self, reference: &str) -> Result<String> {
        let info = self
            .daemon()?
            .inspect_registry_image(reference, None)
            .await
            .with_context(|| format!("querying the registry for {reference}"))?;
        // bollard's DistributionInspect carries an OCI descriptor with the manifest digest;
        // the digest is optional in the model, so treat its absence as an error.
        info.descriptor
            .digest
            .filter(|d| !d.is_empty())
            .ok_or_else(|| anyhow!("registry returned no manifest digest for {reference}"))
    }

    /// Compute the full update status for the UI: current identity + remote digest +
    /// available flag. Never bails — registry / daemon failures land in `status.error`
    /// with `available = false`, so the UI can always render something.
    pub async fn check_update(&self, reference: &str, self_id: Option<&str>) -> UpdateStatus {
        let (repo, _tag) = split_reference(reference);
        let mut status = UpdateStatus {
            current_revision: None,
            current_created: None,
            current_digest: None,
            remote_digest: None,
            available: false,
            reference: reference.to_string(),
            error: None,
        };
        // Current identity (dev mode / no self container → leave current_* None).
        if let Some(id) = self_id {
            match self.self_image_info(id, &repo).await {
                Ok(info) => {
                    status.current_revision = info.revision;
                    status.current_created = info.created;
                    status.current_digest = info.repo_digest.map(|rd| {
                        // Keep just the sha256:… part for a clean compare/display.
                        rd.split_once('@').map(|(_, d)| d.to_string()).unwrap_or(rd)
                    });
                }
                Err(e) => status.error = Some(format!("reading current image: {e}")),
            }
        }
        // Remote digest.
        match self.registry_digest(reference).await {
            Ok(remote) => {
                status.available = is_update_available(status.current_digest.as_deref(), &remote);
                status.remote_digest = Some(remote);
            }
            Err(e) => {
                // Don't overwrite a current-image error with the remote one; append.
                let msg = format!("checking registry: {e}");
                status.error = Some(match status.error.take() {
                    Some(prev) => format!("{prev}; {msg}"),
                    None => msg,
                });
            }
        }
        status
    }

    /// An image's `Config.StopSignal` (e.g. `SIGRTMIN+3`), or `None` when unset. The
    /// template-pull verify WARNs when a pulled template lacks the clean-stop signal (clones
    /// off it hang 20 s on stop before SIGKILL — gotcha #5).
    pub async fn image_stop_signal(&self, reference: &str) -> Result<Option<String>> {
        let info = self
            .daemon()?
            .inspect_image(reference)
            .await
            .with_context(|| format!("inspecting image {reference}"))?;
        Ok(info.config.and_then(|c| c.stop_signal).filter(|s| !s.is_empty()))
    }

    /// List clone-source images (label `rmng.image=1`), newest first, projected to the
    /// wire [`ImageInfo`]. `in_use_by` is left empty here — the caller (web.rs) fills it
    /// from [`Self::list_managed_containers`] (which containers run on which image).
    pub async fn list_rmng_images(&self) -> Result<Vec<ImageInfo>> {
        let filters: HashMap<String, Vec<String>> =
            HashMap::from([("label".to_string(), vec![format!("{LABEL_IMAGE}=1")])]);
        let opts = ListImagesOptionsBuilder::new().all(false).filters(&filters).build();
        let summaries = self.daemon()?.list_images(Some(opts)).await.context("listing rmng images")?;
        let mut out: Vec<ImageInfo> = summaries
            .into_iter()
            .map(|s| {
                let reference =
                    s.repo_tags.first().cloned().unwrap_or_else(|| s.id.clone());
                ImageInfo {
                    id: s.id,
                    reference,
                    size_bytes: s.size,
                    created_at: epoch_to_rfc3339(s.created),
                    base: s.labels.get(LABEL_BASE).map(|v| v == "1").unwrap_or(false),
                    created_from: s.labels.get(LABEL_CREATED_FROM).cloned(),
                    in_use_by: Vec::new(),
                }
            })
            .collect();
        // Newest first (created is epoch seconds).
        out.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        Ok(out)
    }

    /// Commit a container to an image at `<name>:latest` — the user-supplied name is the
    /// full repository (no `rmng/template` namespace); Docker defaults the tag to `latest`.
    /// With `set_boot_config`,
    /// bakes the systemd-PID-1 boot overrides so clones off this image stop cleanly
    /// (gotcha #5): Entrypoint `/sbin/init`, Cmd cleared, `StopSignal=SIGRTMIN+3`, and
    /// `container=docker` in Env. `labels` are always applied (merged over the boot
    /// env-derived config); `pause` freezes the container during the commit. Returns the
    /// new image id. Note: `docker commit` excludes volume mounts, so the clone's inner
    /// Docker state never enters the image (gotcha #11).
    pub async fn commit(
        &self,
        container: &str,
        name: &str,
        set_boot_config: bool,
        pause: bool,
        labels: &[(String, String)],
    ) -> Result<String> {
        let opts = CommitContainerOptionsBuilder::new()
            .container(container)
            // The user's name IS the repository — no `rmng/template` prefix. Docker requires
            // a tag, so we default it to `latest` (image lists show `<name>:latest`).
            .repo(name)
            .tag("latest")
            .pause(pause)
            .build();

        let mut label_map: HashMap<String, String> = labels.iter().cloned().collect();
        // Every RMNG image is a clone source by definition.
        label_map.entry(LABEL_IMAGE.to_string()).or_insert_with(|| "1".to_string());

        let mut config = ContainerConfig { labels: Some(label_map), ..Default::default() };
        if set_boot_config {
            config.entrypoint = Some(vec!["/sbin/init".to_string()]);
            config.cmd = Some(Vec::new()); // clear inherited Cmd
            config.stop_signal = Some("SIGRTMIN+3".to_string());
            config.env = Some(vec!["container=docker".to_string()]);
        }

        let res = self
            .daemon()?
            .commit_container(opts, config)
            .await
            .with_context(|| format!("committing {container} to {name}:latest"))?;
        tracing::info!(target: "docker", "committed {container} -> {name}:latest ({})", short_id(&res.id));
        Ok(res.id)
    }

    /// Remove an image by reference/id. **No force** — a daemon 409 (still in use by a
    /// container) is surfaced verbatim so the operator sees why.
    pub async fn remove_image(&self, reference: &str) -> Result<()> {
        let opts = RemoveImageOptionsBuilder::new().force(false).build();
        match self.daemon()?.remove_image(reference, Some(opts), None).await {
            Ok(_) => Ok(()),
            Err(BollardError::DockerResponseServerError { status_code: 404, .. }) => {
                Ok(()) // already gone
            }
            Err(BollardError::DockerResponseServerError { status_code: 409, message }) => {
                bail!("cannot remove image {reference}: {message}")
            }
            Err(e) => Err(anyhow!("removing image {reference}: {e}")),
        }
    }

    // --- containers -------------------------------------------------------------------

    /// List every RMNG-managed container (label `rmng.managed=1`), running or not — the
    /// daemon-side truth the boot reconciler diffs `state.json` against and the image
    /// in-use accounting reads (which containers were created from which image).
    pub async fn list_managed_containers(&self) -> Result<Vec<ManagedContainer>> {
        let filters: HashMap<String, Vec<String>> =
            HashMap::from([("label".to_string(), vec![format!("{LABEL_MANAGED}=1")])]);
        let opts = bollard::query_parameters::ListContainersOptionsBuilder::new()
            .all(true)
            .filters(&filters)
            .build();
        let list = self.daemon()?.list_containers(Some(opts)).await.context("listing managed containers")?;
        Ok(list
            .into_iter()
            .map(|c| ManagedContainer {
                // Docker prefixes names with `/` for historic reasons; a nameless summary
                // (shouldn't happen) degrades to the id.
                name: c
                    .names
                    .unwrap_or_default()
                    .into_iter()
                    .next()
                    .map(|n| n.trim_start_matches('/').to_string())
                    .unwrap_or_else(|| c.id.unwrap_or_default()),
                image: c.image.unwrap_or_default(),
                running: matches!(c.state, Some(bollard::models::ContainerSummaryStateEnum::RUNNING)),
            })
            .collect())
    }

    /// Create a privileged systemd-PID-1 clone container on the rmng network (dynamic
    /// IP; the name is the address). Bakes the stop signal + timeout, mounts the shared clone socket + a per-clone
    /// `rmng-dind-<name>` volume at `/var/lib/docker` (overlay-on-overlay fix), applies
    /// CPU/memory (+8 GiB swap) limits and `restart: unless-stopped`. Returns the new
    /// container id. Does NOT start it (caller decides). A stale same-named container
    /// 409s here — the daemon message is surfaced verbatim (gotcha #7).
    pub async fn create_clone_container(&self, spec: &CreateSpec) -> Result<String> {
        let dind_volume = Self::dind_volume_name(&spec.name);
        let ctd_volume = Self::ctd_volume_name(&spec.name);
        // Ensure the per-clone inner-Docker volumes exist (idempotent).
        self.ensure_volume(&dind_volume).await?;
        self.ensure_volume(&ctd_volume).await?;

        let mut mounts = vec![
            Mount {
                target: Some(DIND_TARGET.to_string()),
                source: Some(dind_volume.clone()),
                typ: Some(MountTypeEnum::VOLUME),
                ..Default::default()
            },
            // Docker ≥28 stores images via the containerd snapshotter under
            // /var/lib/containerd, NOT /var/lib/docker — without its own volume the
            // inner daemon mounts overlay-on-overlay and every `docker run` fails
            // with EINVAL (found live in the E2E; the classic dind volume alone no
            // longer covers gotcha #11's fix).
            Mount {
                target: Some(CTD_TARGET.to_string()),
                source: Some(ctd_volume.clone()),
                typ: Some(MountTypeEnum::VOLUME),
                ..Default::default()
            },
        ];
        if !spec.sock_source.trim().is_empty() {
            mounts.push(Mount {
                target: Some(SOCK_DIR.to_string()),
                source: Some(spec.sock_source.clone()),
                typ: Some(MountTypeEnum::BIND),
                bind_options: Some(MountBindOptions {
                    propagation: Some(MountBindOptionsPropagationEnum::RSHARED),
                    ..Default::default()
                }),
                ..Default::default()
            });
        }
        // lxcfs (optional): when the host has it installed (probed by `self_setup`), bind
        // its cgroup-aware /proc files over the clone's so `free`/`nproc`/htop reflect the
        // clone's cpu/memory limits. No-op when absent — the clone keeps host-wide /proc.
        // These are container config: they mount before /sbin/init and are NEVER baked
        // into a commit (a commit captures the image fs, not HostConfig binds). Only NEW
        // clones created after the probe saw lxcfs get them; existing containers are
        // untouched.
        mounts.extend(lxcfs_proc_mounts(self.env.read().await.lxcfs_ok));

        let mem = (spec.memory_mb as i64) * 1024 * 1024;
        let host_config = HostConfig {
            privileged: Some(true),
            nano_cpus: Some((spec.cpus as i64) * 1_000_000_000),
            memory: Some(mem),
            memory_swap: Some(mem + SWAP_BYTES),
            // LXC parity: the old LXC clones booted systemd, which mounts /dev/shm as tmpfs at
            // ~50% of RAM (≈16 GB for a 32 GB clone). Docker's default is a fixed 64 MB, which
            // Chromium/Electron (Chrome, VSCode) exhaust fast — a failed shm allocation makes
            // them SIGILL. Derived from `mem` (no config knob): tmpfs is lazily allocated so a
            // large ceiling is free until used, and used pages are charged to THIS clone's own
            // memory cgroup, so a runaway shm consumer only OOMs its own clone. Only desktop
            // clones get this; the self-upgrade helper + lxcfs probe run no desktop.
            shm_size: Some(mem / 2),
            mounts: Some(mounts),
            restart_policy: Some(RestartPolicy {
                name: Some(RestartPolicyNameEnum::UNLESS_STOPPED),
                ..Default::default()
            }),
            ..Default::default()
        };

        let mut env: Vec<String> = spec.env.iter().map(|(k, v)| format!("{k}={v}")).collect();
        // systemd inside a container needs this marker.
        if !env.iter().any(|e| e.starts_with("container=")) {
            env.push("container=docker".to_string());
        }

        let body = ContainerCreateBody {
            hostname: Some(spec.hostname.clone()),
            image: Some(spec.image.clone()),
            env: Some(env),
            labels: Some(HashMap::from([(LABEL_MANAGED.to_string(), "1".to_string())])),
            stop_signal: Some("SIGRTMIN+3".to_string()),
            stop_timeout: Some(STOP_TIMEOUT_SECS as i64),
            host_config: Some(host_config),
            networking_config: Some(NetworkingConfig {
                endpoints_config: Some(HashMap::from([(
                    NETWORK.to_string(),
                    EndpointSettings::default(),
                )])),
            }),
            ..Default::default()
        };

        let opts = CreateContainerOptionsBuilder::new().name(&spec.name).build();
        let res = self
            .daemon()?
            .create_container(Some(opts), body)
            .await
            .with_context(|| format!("creating clone container {}", spec.name))?;
        Ok(res.id)
    }

    /// Full inspect of our own container (Config + HostConfig + NetworkSettings), the input
    /// to [`SelfSpec::from_inspect`].
    pub async fn inspect_self(&self, self_id: &str) -> Result<ContainerInspectResponse> {
        self.daemon()?
            .inspect_container(self_id, None::<bollard::query_parameters::InspectContainerOptions>)
            .await
            .with_context(|| format!("inspecting self container {self_id}"))
    }

    /// Create + start a container from a captured [`SelfSpec`], reusing the container name.
    /// The caller must have already removed any container holding that name (the swap does
    /// stop→remove→create). The image is `spec.new_image_ref` (the fallback path passes a
    /// spec whose `new_image_ref` was set to the old image id). Returns the new container id.
    pub async fn create_and_start_from_spec(&self, spec: &SelfSpec) -> Result<String> {
        let c = &spec.config;
        let body = ContainerCreateBody {
            hostname: c.hostname.clone(),
            env: c.env.clone(),
            labels: c.labels.clone(),
            exposed_ports: c.exposed_ports.clone(),
            entrypoint: c.entrypoint.clone(),
            cmd: c.cmd.clone(),
            stop_signal: c.stop_signal.clone(),
            stop_timeout: c.stop_timeout,
            image: Some(spec.new_image_ref.clone()),
            host_config: Some(spec.host_config.clone()),
            networking_config: Some(NetworkingConfig {
                endpoints_config: Some(spec.networks.clone()),
            }),
            ..Default::default()
        };
        let opts = CreateContainerOptionsBuilder::new().name(&spec.container_name).build();
        let docker = self.daemon()?;
        let id = docker
            .create_container(Some(opts), body)
            .await
            .with_context(|| format!("recreating container {}", spec.container_name))?
            .id;
        if let Err(e) = docker
            .start_container(&id, None::<bollard::query_parameters::StartContainerOptions>)
            .await
        {
            // Create succeeded but start failed: the created-but-stopped container still holds
            // the name. Remove it (best-effort, 404-tolerant) so a fallback recreate / retry
            // won't 409 on the name and leave the host with a stopped container and nothing
            // running — the exact state the create-error fallback exists to prevent.
            let _ = self.remove_container(&id).await;
            return Err(e)
                .with_context(|| format!("starting recreated container {}", spec.container_name));
        }
        Ok(id)
    }

    /// Launch the detached `self-upgrade` helper container from `new_image` (already pulled).
    /// It mounts the docker socket + the /data volume (so it can read the handoff + config)
    /// and runs `rmng-control-server self-upgrade`. Named `rmng-self-upgrade`, NOT
    /// `rmng.managed`-labeled (ephemeral infra, kept out of managed sweeps), `network: none`,
    /// pre-cleaned. The helper outlives the old container's removal. `socket` is
    /// `config.docker.socket` — the docker.sock is bound directly (respects a custom path)
    /// rather than discovered, because Compose stores it under Mounts, not HostConfig.Binds.
    pub async fn launch_upgrade_helper(&self, new_image: &str, self_id: &str, socket: &str) -> Result<()> {
        const HELPER_NAME: &str = "rmng-self-upgrade";
        // Reclaim a leftover helper from a crashed earlier run (idempotent, 404-ok).
        let _ = self.remove_container(HELPER_NAME).await;

        // Discover our /data source (named volume or bind) so the helper reads the same
        // handoff + config. `mounts` covers BOTH compose (long-syntax → Mounts) and the
        // one-liner. docker.sock is bound directly from `socket` below.
        let me = self.inspect_self(self_id).await?;
        let mut mounts: Vec<Mount> = Vec::new();
        if let Some(ms) = me.mounts.clone() {
            for m in ms {
                if m.destination.as_deref() == Some("/data") {
                    let is_vol = m.name.is_some();
                    mounts.push(Mount {
                        target: Some("/data".to_string()),
                        source: m.name.clone().or(m.source.clone()),
                        typ: Some(if is_vol { MountTypeEnum::VOLUME } else { MountTypeEnum::BIND }),
                        ..Default::default()
                    });
                }
            }
        }

        if mounts.is_empty() {
            tracing::warn!(
                target: "update",
                "no /data mount discovered on self ({self_id}); launching the self-upgrade helper \
                 without it — the helper won't see the handoff/config and the update op may stay \
                 stuck Running"
            );
        }
        let host_config = HostConfig {
            // docker.sock as a bind (source == target == the configured socket path).
            binds: Some(vec![format!("{socket}:{socket}")]),
            mounts: if mounts.is_empty() { None } else { Some(mounts) },
            network_mode: Some("none".to_string()),
            auto_remove: Some(false),
            // The control-server itself runs `--privileged` (which bypasses AppArmor). This
            // helper is deliberately unprivileged (it only needs docker.sock + /data), but in
            // RMNG's nested-Docker-in-LXC deployment the host denies loading the docker-default
            // AppArmor profile ("apparmor_parser … Access denied"), so an unprivileged container
            // is stuck in `Created` and never starts. Opt out of AppArmor so the helper starts
            // wherever the privileged main container does. No-op on hosts without AppArmor.
            security_opt: Some(vec!["apparmor=unconfined".to_string()]),
            ..Default::default()
        };
        let body = ContainerCreateBody {
            image: Some(new_image.to_string()),
            entrypoint: Some(vec![
                "/usr/local/bin/rmng-control-server".to_string(),
                "self-upgrade".to_string(),
                "/data/update-handoff.json".to_string(),
            ]),
            cmd: Some(Vec::new()),
            host_config: Some(host_config),
            ..Default::default()
        };
        let opts = CreateContainerOptionsBuilder::new().name(HELPER_NAME).build();
        let docker = self.daemon()?;
        let id = docker.create_container(Some(opts), body).await.context("creating self-upgrade helper")?.id;
        docker
            .start_container(&id, None::<bollard::query_parameters::StartContainerOptions>)
            .await
            .context("starting self-upgrade helper")?;
        Ok(())
    }

    /// Ensure a named volume exists (idempotent — create is safe to repeat).
    async fn ensure_volume(&self, name: &str) -> Result<()> {
        let opts = VolumeCreateOptions {
            name: Some(name.to_string()),
            labels: Some(HashMap::from([(LABEL_MANAGED.to_string(), "1".to_string())])),
            ..Default::default()
        };
        self.daemon()?.create_volume(opts).await.with_context(|| format!("creating volume {name}"))?;
        Ok(())
    }

    /// Start a container. bollard treats 304 (already started) as success, so this is a
    /// no-op when it's already running.
    pub async fn start_container(&self, id: &str) -> Result<()> {
        match self.daemon()?.start_container(id, None::<bollard::query_parameters::StartContainerOptions>).await {
            Ok(()) => Ok(()),
            Err(e) => Err(anyhow!("starting container {id}: {e}")),
        }
    }

    /// Stop a container with the systemd stop signal + the 20 s timeout. bollard maps 304
    /// (already stopped) to success; 404 (already gone) is tolerated here.
    pub async fn stop_container(&self, id: &str) -> Result<()> {
        let opts = StopContainerOptionsBuilder::new().t(STOP_TIMEOUT_SECS).build();
        match self.daemon()?.stop_container(id, Some(opts)).await {
            Ok(()) => Ok(()),
            Err(BollardError::DockerResponseServerError { status_code: 404, .. }) => Ok(()),
            Err(e) => Err(anyhow!("stopping container {id}: {e}")),
        }
    }

    /// Restart our own container in place (the programmatic twin of `docker restart rmng`) —
    /// the daemon stops+starts the same container, which re-reads config.json on boot. Used
    /// to apply restart-required settings. The `--restart unless-stopped` policy is a backstop
    /// if the daemon's restart is interrupted. Uses the systemd stop timeout.
    pub async fn restart_self(&self, self_id: &str) -> Result<()> {
        let opts = bollard::query_parameters::RestartContainerOptionsBuilder::new()
            .t(STOP_TIMEOUT_SECS)
            .build();
        self.daemon()?
            .restart_container(self_id, Some(opts))
            .await
            .with_context(|| format!("restarting self container {self_id}"))?;
        Ok(())
    }

    /// Remove a container (force + volumes-owned-by-container). Tolerates 404 (gone). The
    /// per-clone named volume is NOT removed here — it is named + reused; callers use
    /// [`DockerCtl::remove_volume`] for that.
    pub async fn remove_container(&self, id: &str) -> Result<()> {
        let opts = RemoveContainerOptionsBuilder::new().force(true).build();
        match self.daemon()?.remove_container(id, Some(opts)).await {
            Ok(()) => Ok(()),
            Err(BollardError::DockerResponseServerError { status_code: 404, .. }) => Ok(()),
            Err(e) => Err(anyhow!("removing container {id}: {e}")),
        }
    }

    /// Remove a named volume (force). Tolerates 404 (gone). A 409 (still in use) is
    /// surfaced so the caller knows to remove the container first.
    pub async fn remove_volume(&self, name: &str) -> Result<()> {
        let opts = RemoveVolumeOptionsBuilder::new().force(true).build();
        match self.daemon()?.remove_volume(name, Some(opts)).await {
            Ok(()) => Ok(()),
            Err(BollardError::DockerResponseServerError { status_code: 404, .. }) => Ok(()),
            Err(BollardError::DockerResponseServerError { status_code: 409, message }) => {
                bail!("cannot remove volume {name}: {message}")
            }
            Err(e) => Err(anyhow!("removing volume {name}: {e}")),
        }
    }

    /// The per-clone inner-Docker volume name for a clone (`rmng-dind-<name>`), so callers
    /// can pair `remove_container` + `remove_volume` on delete.
    pub fn dind_volume_name(name: &str) -> String {
        format!("rmng-dind-{name}")
    }

    /// The per-clone containerd-store volume name (`rmng-ctd-<name>`), sibling of
    /// [`Self::dind_volume_name`] — see `CTD_TARGET`.
    pub fn ctd_volume_name(name: &str) -> String {
        format!("rmng-ctd-{name}")
    }

    /// The container's IPv4 on the rmng network, or `None` if not attached / not running.
    /// Dev mode's dial path: a host process can't use Docker's embedded DNS, so
    /// `App::dial_host` resolves a clone's bridge IP through this instead.
    pub async fn inspect_ip(&self, id: &str) -> Result<Option<String>> {
        let info = self
            .daemon()?
            .inspect_container(id, None::<bollard::query_parameters::InspectContainerOptions>)
            .await
            .with_context(|| format!("inspecting container {id}"))?;
        let ip = info
            .network_settings
            .and_then(|ns| ns.networks)
            .and_then(|nets| nets.get(NETWORK).and_then(|e| e.ip_address.clone()))
            .filter(|s| !s.is_empty());
        Ok(ip)
    }

    /// Whether a container is currently running. `false` (not an error) if it doesn't
    /// exist.
    pub async fn is_running(&self, id: &str) -> Result<bool> {
        match self.daemon()?.inspect_container(id, None::<bollard::query_parameters::InspectContainerOptions>).await {
            Ok(info) => Ok(info.state.and_then(|s| s.running).unwrap_or(false)),
            Err(BollardError::DockerResponseServerError { status_code: 404, .. }) => Ok(false),
            Err(e) => Err(anyhow!("inspecting container {id}: {e}")),
        }
    }

    /// One-shot live resource sample for a container (`stream=false` so the daemon
    /// returns a single frame then disconnects; `one_shot=false` so it collects TWO CPU
    /// cycles and fills `precpu_stats`, which the CPU-delta math needs). Returns `None` —
    /// never an error — for a stopped/missing container (the daemon yields no usable
    /// memory sample), so the monitor poller can just skip it. CPU is a percent-of-one-
    /// core figure ([`cpu_percent`]); memory follows docker-CLI semantics (`usage` minus
    /// reclaimable `inactive_file` cache). Best-effort: a dead daemon yields `None` too.
    pub async fn container_stats(&self, name: &str) -> Option<ContainerStats> {
        let opts = StatsOptionsBuilder::new().stream(false).one_shot(false).build();
        let docker = self.daemon().ok()?;
        let mut stream = docker.stats(name, Some(opts));
        // `stream=false` yields exactly one frame (after the two cycles) then closes; a
        // stopped or missing container errors / yields nothing → None.
        let s = stream.next().await?.ok()?;

        // Memory: a running container reports a non-empty `memory_stats` with `usage`; a
        // stopped one reports an empty object (no `usage`) — the running gate.
        let mem = s.memory_stats?;
        let usage = mem.usage?;
        let mem_limit = mem.limit.unwrap_or(0);
        // Subtract reclaimable page cache so the number matches `docker stats` (cgroup v2
        // exports `inactive_file`; v1 `total_inactive_file`).
        let inactive = mem
            .stats
            .as_ref()
            .and_then(|m| m.get("inactive_file").or_else(|| m.get("total_inactive_file")))
            .copied()
            .unwrap_or(0);
        let mem_used = usage.saturating_sub(inactive);

        // CPU: delta of the container's total vs the system's total across the two samples
        // the daemon just collected, scaled by the online-CPU count.
        let cpu = s.cpu_stats.unwrap_or_default();
        let precpu = s.precpu_stats.unwrap_or_default();
        let cpu_total = cpu.cpu_usage.as_ref().and_then(|u| u.total_usage).unwrap_or(0);
        let precpu_total = precpu.cpu_usage.as_ref().and_then(|u| u.total_usage).unwrap_or(0);
        let system = cpu.system_cpu_usage.unwrap_or(0);
        let presystem = precpu.system_cpu_usage.unwrap_or(0);
        // `online_cpus` can be absent on older daemons — fall back to the per-CPU vector
        // length, then to 1, so a valid sample never collapses to 0% for lack of a count.
        let online = cpu
            .online_cpus
            .map(u64::from)
            .filter(|&n| n > 0)
            .or_else(|| {
                cpu.cpu_usage
                    .as_ref()
                    .and_then(|u| u.percpu_usage.as_ref())
                    .map(|v| v.len() as u64)
                    .filter(|&n| n > 0)
            })
            .unwrap_or(1);
        let cpu_pct = cpu_percent(cpu_total, precpu_total, system, presystem, online);

        Some(ContainerStats { cpu_pct, mem_used, mem_limit })
    }

    /// The container's host PID (`State.Pid`), or `None` when it isn't running (the daemon
    /// reports pid 0 for stopped containers) or doesn't exist (404). The clone-home
    /// reconciler ([`crate::homes`]) turns this into a `/proc/<pid>/root/home/rmng` symlink
    /// under `data/hosts/`; that only resolves when the control-server shares the host PID
    /// namespace (`pid: "host"` in compose.yaml). A dead daemon is a real error (retried).
    pub async fn container_pid(&self, name_or_id: &str) -> Result<Option<i64>> {
        match self.daemon()?.inspect_container(name_or_id, None::<bollard::query_parameters::InspectContainerOptions>).await {
            Ok(info) => Ok(info.state.and_then(|s| s.pid).filter(|&p| p > 0)),
            Err(BollardError::DockerResponseServerError { status_code: 404, .. }) => Ok(None),
            Err(e) => Err(anyhow!("inspecting container {name_or_id}: {e}")),
        }
    }

    /// `(host PID, HostConfig.Memory bytes)` for a running container, from a single inspect.
    /// `None` when the container is stopped/gone (pid 0 / 404) or its memory limit is unset
    /// (0 / absent — unlimited, so there's no basis to size `/dev/shm` from). The `/dev/shm`
    /// reconciler ([`crate::shm`]) uses the PID to enter the clone's mount namespace and
    /// `Memory / 2` as the LXC-parity remount target. Like [`container_pid`], the PID is only
    /// resolvable into `/proc` when the control-server shares the host PID namespace
    /// (`pid: "host"`). A dead daemon is a real error (retried).
    pub async fn container_pid_and_memory(&self, name_or_id: &str) -> Result<Option<(i64, i64)>> {
        match self.daemon()?.inspect_container(name_or_id, None::<bollard::query_parameters::InspectContainerOptions>).await {
            Ok(info) => {
                let pid = info.state.and_then(|s| s.pid).filter(|&p| p > 0);
                let mem = info.host_config.and_then(|h| h.memory).filter(|&m| m > 0);
                Ok(pid.zip(mem))
            }
            Err(BollardError::DockerResponseServerError { status_code: 404, .. }) => Ok(None),
            Err(e) => Err(anyhow!("inspecting container {name_or_id}: {e}")),
        }
    }

    /// The last `n` combined stdout+stderr log lines of a container, newest at the end,
    /// as one newline-joined string. For the wait-ready death path: a clone whose systemd
    /// PID 1 died before its daemon registered leaves its failure in these logs, which the
    /// caller folds into the operation log. Best-effort — a log-fetch failure yields an
    /// empty string rather than masking the real (container-dead) error.
    pub async fn container_logs_tail(&self, id: &str, n: usize) -> String {
        let opts = bollard::query_parameters::LogsOptionsBuilder::new()
            .stdout(true)
            .stderr(true)
            .tail(&n.to_string())
            .build();
        let docker = match self.daemon() {
            Ok(d) => d,
            Err(e) => {
                tracing::debug!(target: "docker", "logs tail for {id}: {e:#}");
                return String::new();
            }
        };
        let mut stream = docker.logs(id, Some(opts));
        let mut lines: Vec<String> = Vec::new();
        while let Some(item) = stream.next().await {
            match item {
                Ok(out) => {
                    let text = String::from_utf8_lossy(out.into_bytes().as_ref()).into_owned();
                    for line in text.split('\n') {
                        let line = line.trim_end_matches('\r');
                        if !line.is_empty() {
                            lines.push(line.to_string());
                        }
                    }
                }
                Err(e) => {
                    tracing::debug!(target: "docker", "logs tail for {id}: {e}");
                    break;
                }
            }
        }
        // Keep only the last `n` (a chunk can carry more than one line).
        if lines.len() > n {
            lines.drain(0..lines.len() - n);
        }
        lines.join("\n")
    }

    // --- file upload ------------------------------------------------------------------

    /// Upload files into a running container by building an in-memory tar and PUTting it
    /// to `/`. uid/gid/mode are applied verbatim by the daemon (gotcha #2 — callers pass
    /// uid/gid 1000 for `home/rmng/**`). Paths are archive-relative (no leading slash);
    /// they extract relative to `/`.
    pub async fn upload_tar(&self, container: &str, entries: Vec<TarEntry>) -> Result<()> {
        let archive = build_tar(&entries).context("building upload tar")?;
        self.daemon()?
            .upload_to_container(
                container,
                Some(
                    bollard::query_parameters::UploadToContainerOptionsBuilder::new()
                        .path("/")
                        .build(),
                ),
                bollard::body_full(archive.into()),
            )
            .await
            .with_context(|| format!("uploading tar to {container}"))?;
        Ok(())
    }

    // --- exec -------------------------------------------------------------------------

    /// Feed a shell script over exec stdin to `bash -s` (optionally with extra env +
    /// positional args), streaming stdout/stderr through **separate per-stream line
    /// buffers** (bollard `LogOutput` chunks are NOT line-aligned — gotcha #1). The
    /// callback fires once per completed line with the stream tag (`"out"`/`"err"`);
    /// remainders are flushed at EOF. The stdin write is driven concurrently with the
    /// output drain — the attach is one multiplexed connection, so writing the whole
    /// script first could deadlock against early output. The real exit code comes from
    /// `inspect_exec` afterward (the stream ending doesn't carry it). No client-side
    /// timeout.
    pub async fn exec_script(
        &self,
        container: &str,
        script: &str,
        env: &[(String, String)],
        args: &[String],
        mut on_line: impl FnMut(&str, &str),
    ) -> Result<i64> {
        // `bash -s -- <args...>` reads the script from stdin; `$0` is `bash`, `$1..` args.
        let mut cmd = vec!["bash".to_string(), "-s".to_string(), "--".to_string()];
        cmd.extend(args.iter().cloned());
        let env_lines: Vec<String> = env.iter().map(|(k, v)| format!("{k}={v}")).collect();

        let exec = self
            .daemon()?
            .create_exec(
                container,
                CreateExecOptions {
                    cmd: Some(cmd),
                    env: if env_lines.is_empty() { None } else { Some(env_lines) },
                    attach_stdin: Some(true),
                    attach_stdout: Some(true),
                    attach_stderr: Some(true),
                    ..Default::default()
                },
            )
            .await
            .with_context(|| format!("creating script exec in {container}"))?;

        let StartExecResults::Attached { mut output, mut input } =
            self.daemon()?.start_exec(&exec.id, None).await?
        else {
            bail!("exec started detached unexpectedly");
        };

        // Feed the script CONCURRENTLY with draining output. The exec attach multiplexes
        // stdin and stdout/stderr over ONE upgraded connection, so writing the whole
        // script before reading could deadlock: a script that emits enough output early
        // fills the socket buffers while later script bytes are still unwritten, and
        // with no client timeout (by design) that hang would be permanent. `join!` keeps
        // the drain running while stdin flushes; the shutdown (EOF) is what lets
        // `bash -s` finish parsing.
        let write_fut = async {
            let res = input.write_all(script.as_bytes()).await;
            // Always signal EOF, even after a failed write — bash may still produce a
            // useful error line, and the real outcome comes from `inspect_exec` below.
            input.shutdown().await.ok();
            res
        };

        let mut out_buf = LineSplitter::default();
        let mut err_buf = LineSplitter::default();
        let read_fut = async {
            while let Some(chunk) = output.next().await {
                match chunk? {
                    LogOutput::StdOut { message } | LogOutput::Console { message } => {
                        out_buf.push(&message, |line| on_line("out", line));
                    }
                    LogOutput::StdErr { message } => {
                        err_buf.push(&message, |line| on_line("err", line));
                    }
                    LogOutput::StdIn { .. } => {}
                }
            }
            Ok::<(), BollardError>(())
        };
        let (write_res, read_res) = tokio::join!(write_fut, read_fut);

        // Flush trailing partials (a script may end without a newline) BEFORE any error
        // handling, so already-captured output always reaches the caller.
        out_buf.flush(|line| on_line("out", line));
        err_buf.flush(|line| on_line("err", line));

        read_res.with_context(|| format!("streaming exec output from {container}"))?;
        if let Err(e) = write_res {
            if is_benign_stdin_write_error(&e) {
                // The exec process stopped reading stdin before consuming the whole
                // script (early `exit`, `set -e` bail, bash parse error): the daemon
                // tears the stdin stream down and the tail write fails. Not fatal —
                // `inspect_exec` reports the real exit code.
                tracing::debug!(target: "docker", "exec stdin closed early: {e}");
            } else {
                return Err(anyhow!("writing script to exec stdin: {e}"));
            }
        }

        let code = self.daemon()?.inspect_exec(&exec.id).await?.exit_code.unwrap_or(-1);
        Ok(code)
    }

    /// Run an arbitrary command (argv, no TTY) in `container` via docker exec, capturing
    /// stdout and stderr into **separate buffered strings** (bollard `LogOutput` tags each
    /// frame by stream and chunks are NOT line-aligned — gotcha #1 — so [`LineSplitter`]
    /// reassembles complete lines per stream). Honors `user` (uid or name), optional
    /// `workdir`, `env` (`KEY=VAL`), and optional `stdin` bytes (fed concurrently with the
    /// output drain, same as [`Self::exec_script`], to avoid the one-connection deadlock).
    /// Output is UTF-8-lossy (binary is out of scope). The real exit code comes from
    /// `inspect_exec`. This is the backend for `POST /api/hosts/:id/exec` (`rmng exec`).
    pub async fn exec_capture(
        &self,
        container: &str,
        cmd: &[String],
        user: &str,
        workdir: Option<&str>,
        env: &[String],
        stdin: Option<&[u8]>,
    ) -> Result<ExecResult> {
        let exec = self
            .daemon()?
            .create_exec(
                container,
                CreateExecOptions {
                    cmd: Some(cmd.to_vec()),
                    user: Some(user.to_string()),
                    working_dir: workdir.map(str::to_string),
                    env: if env.is_empty() { None } else { Some(env.to_vec()) },
                    attach_stdin: Some(stdin.is_some()),
                    attach_stdout: Some(true),
                    attach_stderr: Some(true),
                    ..Default::default()
                },
            )
            .await
            .with_context(|| format!("creating exec in {container}"))?;

        let StartExecResults::Attached { mut output, mut input } =
            self.daemon()?.start_exec(&exec.id, None).await?
        else {
            bail!("exec started detached unexpectedly");
        };

        // Feed stdin (if any) CONCURRENTLY with draining output; always signal EOF so the
        // command sees end-of-input. See `exec_script` for the deadlock rationale.
        let write_fut = async {
            let res = match stdin {
                Some(data) => input.write_all(data).await,
                None => Ok(()),
            };
            input.shutdown().await.ok();
            res
        };

        // Reassemble complete lines per stream, then rebuild the buffered output: each
        // completed line gets its `\n` restored; a trailing partial (no newline) does not.
        let mut out_buf = LineSplitter::default();
        let mut err_buf = LineSplitter::default();
        let mut stdout = String::new();
        let mut stderr = String::new();
        let read_fut = async {
            while let Some(chunk) = output.next().await {
                match chunk? {
                    LogOutput::StdOut { message } | LogOutput::Console { message } => {
                        out_buf.push(&message, |line| {
                            stdout.push_str(line);
                            stdout.push('\n');
                        });
                    }
                    LogOutput::StdErr { message } => {
                        err_buf.push(&message, |line| {
                            stderr.push_str(line);
                            stderr.push('\n');
                        });
                    }
                    LogOutput::StdIn { .. } => {}
                }
            }
            Ok::<(), BollardError>(())
        };
        let (write_res, read_res) = tokio::join!(write_fut, read_fut);

        // Flush trailing partials (output that ended without a newline) BEFORE error
        // handling, so already-captured output always reaches the caller.
        out_buf.flush(|line| stdout.push_str(line));
        err_buf.flush(|line| stderr.push_str(line));

        read_res.with_context(|| format!("streaming exec output from {container}"))?;
        if let Err(e) = write_res {
            if is_benign_stdin_write_error(&e) {
                // The command stopped reading stdin before consuming it all: the daemon
                // tears the stdin stream down and the tail write fails. Not fatal —
                // `inspect_exec` reports the real exit code.
                tracing::debug!(target: "docker", "exec stdin closed early: {e}");
            } else {
                return Err(anyhow!("writing to exec stdin: {e}"));
            }
        }

        let exit_code = self.daemon()?.inspect_exec(&exec.id).await?.exit_code.unwrap_or(-1);
        Ok(ExecResult { exit_code, stdout, stderr })
    }
}

/// Per-request timeout for plain (non-hijacked) daemon calls. `docker commit` of a
/// provisioned base image exports a multi-GB layer diff and legitimately runs for
/// minutes; at bollard's 120 s default the client cancels mid-CreateDiff (daemon logs
/// status=499) and the bootstrap dies at the commit step. Hijacked streams (exec
/// attach) are not subject to this timeout, so only slow one-shot calls need the room.
const CLIENT_TIMEOUT_SECS: u64 = 3600;

/// Build a bollard client for `socket`. No daemon I/O — bollard only validates that the
/// socket path exists, which is exactly the failure [`DockerCtl::daemon`] retries on.
fn build_client(socket: &str) -> Result<Docker> {
    Docker::connect_with_unix(socket, CLIENT_TIMEOUT_SECS, bollard::API_DEFAULT_VERSION)
        .with_context(|| format!("connecting to the Docker daemon at {socket}"))
}

// --- Pure helpers ---------------------------------------------------------------------

/// A parsed subnet: the canonical CIDR the rmng bridge is created with, plus its `.1`
/// gateway (dev mode's control host). Computed from the network *base* (`addr & mask`),
/// so a config like `10.99.0.5/24` still yields `10.99.0.0/24` (host bits masked off).
/// Nothing reserves or allocates clone IPs anymore — Docker IPAM owns them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SubnetPlan {
    /// Network base (host bits zeroed).
    base: u32,
    /// Prefix length (16–24 per config validation).
    prefix: u8,
}

impl SubnetPlan {
    /// Parse an IPv4 CIDR (the config validator already guarantees `/16`–`/24`, but this
    /// re-checks defensively). Masks host bits off the address to get the network base.
    pub fn parse(cidr: &str) -> Result<Self> {
        let (ip, prefix) = cidr.split_once('/').ok_or_else(|| anyhow!("subnet {cidr:?} is not CIDR"))?;
        let addr: Ipv4Addr = ip.parse().with_context(|| format!("subnet {cidr:?} has a bad IPv4 address"))?;
        let prefix: u8 = prefix.parse().with_context(|| format!("subnet {cidr:?} has a bad prefix"))?;
        if !(1..=32).contains(&prefix) {
            bail!("subnet {cidr:?} prefix out of range");
        }
        let mask = prefix_to_mask(prefix);
        let base = u32::from(addr) & mask;
        Ok(Self { base, prefix })
    }

    /// The canonical `base/prefix` CIDR string (host bits zeroed).
    pub fn cidr(&self) -> String {
        format!("{}/{}", Ipv4Addr::from(self.base), self.prefix)
    }

    /// `.1` — the bridge gateway (the control-server address in dev mode).
    pub fn gateway(&self) -> Ipv4Addr {
        Ipv4Addr::from(self.base + 1)
    }
}

/// Convert a prefix length to a big-endian netmask (`/24` → `0xFFFFFF00`).
fn prefix_to_mask(prefix: u8) -> u32 {
    if prefix == 0 {
        0
    } else {
        u32::MAX << (32 - prefix as u32)
    }
}

/// Split an image reference into `(name-without-tag, tag)`, defaulting the tag to
/// `latest`. Handles a registry host with a port (`host:5000/img:tag`) by only treating
/// the final `:` after the last `/` as the tag separator.
pub fn split_reference(reference: &str) -> (String, String) {
    let last_slash = reference.rfind('/').map(|i| i + 1).unwrap_or(0);
    match reference[last_slash..].rfind(':') {
        Some(rel) => {
            let abs = last_slash + rel;
            (reference[..abs].to_string(), reference[abs + 1..].to_string())
        }
        None => (reference.to_string(), "latest".to_string()),
    }
}

/// Aggregates bollard's per-layer pull progress into one monotonic `0.0..=1.0` fraction,
/// weighted 70% download / 30% extract (`frac = 0.7·Σdl_cur/Σdl_tot + 0.3·Σex_cur/Σex_tot`)
/// since download dominates a fresh image pull's wall-clock time. Pure — no daemon I/O —
/// fed frame-by-frame by [`DockerCtl::pull_image`] via [`Self::observe`]. Two invariants
/// make it safe to drive a `state.json` write / SSE broadcast per emission:
/// - **Monotonic**: reports `max(frac, peak)`. Layers register at different times with
///   different `total`s, so the raw sum can transiently *shrink* as the denominator grows
///   mid-pull; the reported value never goes backwards.
/// - **Throttled**: [`Self::observe`] returns `Some` only on an integer-percent change,
///   capping emissions at ≤100 per pull.
///
/// Layers reported `Already exists` (a cache hit — nothing to download or extract) weigh
/// zero: excluded from both sums rather than counted as "already done", so a pull that's
/// mostly cache hits still reflects the (small) amount of real work left.
#[derive(Debug, Default)]
pub struct PullAggregator {
    /// Per-layer `(current, total)` download bytes.
    downloads: HashMap<String, (i64, i64)>,
    /// Per-layer `(current, total)` extract bytes.
    extracts: HashMap<String, (i64, i64)>,
    /// The highest fraction reported so far (the monotonic floor for the next emission).
    peak: f64,
    /// The last emitted integer percent (0..=100), so repeated ticks under the same
    /// whole percent don't re-emit.
    last_percent: Option<i64>,
}

impl PullAggregator {
    /// Feed one pull-stream frame's `id`/`status`/`progress_detail.{current,total}` (off a
    /// bollard `CreateImageInfo`). Returns the new aggregate fraction only when the integer
    /// percent changed since the last emission; `None` otherwise (including frames with no
    /// layer id, or a status this aggregator doesn't track bytes for).
    pub fn observe(&mut self, id: &str, status: &str, current: Option<i64>, total: Option<i64>) -> Option<f64> {
        if id.is_empty() {
            return None;
        }
        match status {
            "Already exists" => {
                self.downloads.remove(id);
                self.extracts.remove(id);
            }
            "Downloading" => {
                let (Some(c), Some(t)) = (current, total) else { return None };
                self.downloads.insert(id.to_string(), (c, t));
            }
            "Extracting" => {
                let (Some(c), Some(t)) = (current, total) else { return None };
                self.extracts.insert(id.to_string(), (c, t));
            }
            // "Pulling fs layer", "Waiting", "Verifying Checksum", "Pull complete", etc. —
            // no byte counts to fold in.
            _ => return None,
        }

        let frac = Self::weighted_frac(&self.downloads, &self.extracts);
        self.peak = self.peak.max(frac);
        let percent = (self.peak * 100.0) as i64;
        if self.last_percent == Some(percent) {
            None
        } else {
            self.last_percent = Some(percent);
            Some(self.peak)
        }
    }

    fn weighted_frac(downloads: &HashMap<String, (i64, i64)>, extracts: &HashMap<String, (i64, i64)>) -> f64 {
        let (dl_cur, dl_tot) = Self::sum_bytes(downloads);
        let (ex_cur, ex_tot) = Self::sum_bytes(extracts);
        let dl_frac = if dl_tot > 0 { dl_cur as f64 / dl_tot as f64 } else { 0.0 };
        let ex_frac = if ex_tot > 0 { ex_cur as f64 / ex_tot as f64 } else { 0.0 };
        0.7 * dl_frac + 0.3 * ex_frac
    }

    fn sum_bytes(layers: &HashMap<String, (i64, i64)>) -> (i64, i64) {
        layers.values().fold((0, 0), |(cur, tot), &(c, t)| (cur + c, tot + t))
    }
}

/// Docker's CPU-percent formula, in *percent-of-one-core* units (100 == one fully-used
/// core; a container busy across four cores reads ~400). Pure over the raw counters
/// bollard's stats hand back, so it's unit-testable without a daemon. Yields 0 — never
/// NaN/∞ — when either delta is non-positive (the first sample carries no `precpu`, and an
/// idle window has a zero system delta) or the online-CPU count is unknown.
fn cpu_percent(cpu_total: u64, precpu_total: u64, system: u64, presystem: u64, online_cpus: u64) -> f64 {
    let cpu_delta = cpu_total.saturating_sub(precpu_total) as f64;
    let system_delta = system.saturating_sub(presystem) as f64;
    if cpu_delta <= 0.0 || system_delta <= 0.0 || online_cpus == 0 {
        return 0.0;
    }
    (cpu_delta / system_delta) * online_cpus as f64 * 100.0
}

/// A short (12-hex) form of a full container/image id for log lines. `sha256:` prefixes
/// are stripped first.
fn short_id(id: &str) -> String {
    let id = id.strip_prefix("sha256:").unwrap_or(id);
    id.chars().take(12).collect()
}

/// Whether a remote digest represents an update over the running one. Unknown local digest
/// (dev build / no RepoDigest) → true (can't prove up-to-date, so offer the update).
fn is_update_available(current_digest: Option<&str>, remote_digest: &str) -> bool {
    match current_digest {
        Some(cur) => cur != remote_digest,
        None => true,
    }
}

/// The lxcfs `/proc` binds for a clone's `HostConfig`, given the cached probe verdict
/// (`lxcfs_ok`). Pure (no I/O) so it's unit-testable and safe: it emits binds ONLY when
/// the probe confirmed lxcfs is present, which is the single guard against the
/// missing-source-creates-a-junk-directory hazard.
///
/// Empty when `!lxcfs_ok` — the clone keeps today's host-wide `/proc` (graceful
/// degradation). When available, each of the six virtualized files ([`LXCFS_PROC_FILES`])
/// is bind-mounted over the clone's matching `/proc/<file>` so cgroup-aware tools
/// (`free`, `nproc`, `htop`) report the clone's cpu/memory limits, not the host totals.
///
/// - **rw, not ro**: mirrors the lxcfs upstream Docker README example
///   (`-v /var/lib/lxcfs/proc/…:/proc/…:rw`). The FUSE fs serves them read-mostly, but the
///   canonical container bind is rw (some readers open the files rw).
/// - **`Mount` API, not `-v` binds**: a missing source ERRORS the create rather than
///   silently materializing a host directory over the proc file — defense-in-depth, though
///   the `lxcfs_ok` guard already means the sources exist.
fn lxcfs_proc_mounts(lxcfs_ok: bool) -> Vec<Mount> {
    if !lxcfs_ok {
        return Vec::new();
    }
    LXCFS_PROC_FILES
        .iter()
        .map(|f| Mount {
            target: Some(format!("/proc/{f}")),
            source: Some(format!("{LXCFS_PROC_DIR}/{f}")),
            typ: Some(MountTypeEnum::BIND),
            read_only: Some(false),
            ..Default::default()
        })
        .collect()
}

/// Whether an exec-stdin write error is the *expected* result of the exec process
/// exiting before consuming all of its stdin (a script that `exit`s early, a `set -e`
/// bail, a bash parse error): the daemon tears the stdin stream down and the tail write
/// fails. These are logged, not fatal — `inspect_exec` still reports the real exit code.
/// Anything else (a genuine transport failure) is surfaced to the caller.
fn is_benign_stdin_write_error(e: &std::io::Error) -> bool {
    matches!(
        e.kind(),
        std::io::ErrorKind::BrokenPipe
            | std::io::ErrorKind::ConnectionReset
            | std::io::ErrorKind::UnexpectedEof
            | std::io::ErrorKind::WriteZero
    )
}

/// Read the container hostname from `/etc/hostname` (Docker writes the short id there).
fn read_hostname() -> std::io::Result<String> {
    std::fs::read_to_string("/etc/hostname").map(|s| s.trim().to_string())
}

/// Scan `/proc/self/mountinfo` for a `/var/lib/docker/containers/<64hex>/` path and
/// return the 64-hex id if present (the control-server's own container id).
fn extract_container_id_from_mountinfo(mountinfo: &str) -> Option<String> {
    const MARKER: &str = "/containers/";
    for line in mountinfo.lines() {
        if let Some(idx) = line.find(MARKER) {
            let rest = &line[idx + MARKER.len()..];
            let id: String = rest.chars().take_while(|c| c.is_ascii_hexdigit()).collect();
            if id.len() == 64 {
                return Some(id);
            }
        }
    }
    None
}

/// Format epoch seconds as an RFC 3339 / ISO-8601 UTC timestamp (`YYYY-MM-DDTHH:MM:SSZ`),
/// so `ImageInfo.created_at` is a real ISO string without pulling in a date crate.
pub(crate) fn epoch_to_rfc3339(secs: i64) -> String {
    // Days-from-civil algorithm (Howard Hinnant), valid across the proleptic Gregorian
    // calendar; we only ever feed it positive, in-range Docker image timestamps.
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    let (hh, mm, ss) = (rem / 3600, (rem % 3600) / 60, rem % 60);
    // Shift epoch so the era starts at 0000-03-01.
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365; // [0, 399]
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = doy - (153 * mp + 2) / 5 + 1; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 }; // [1, 12]
    let year = if m <= 2 { y + 1 } else { y };
    format!("{year:04}-{m:02}-{d:02}T{hh:02}:{mm:02}:{ss:02}Z")
}

/// Build an in-memory tar from [`TarEntry`]s using the `tar` crate. mode/uid/gid are
/// written verbatim into each header (the daemon extracts with those owners — gotcha #2).
fn build_tar(entries: &[TarEntry]) -> Result<Vec<u8>> {
    let mut builder = tar::Builder::new(Vec::new());
    for e in entries {
        let mut header = tar::Header::new_gnu();
        // Path relative, no leading slash (extracts relative to the request's `path`).
        let path = e.path.trim_start_matches('/');
        header.set_size(e.data.len() as u64);
        header.set_mode(e.mode);
        header.set_uid(e.uid);
        header.set_gid(e.gid);
        header.set_entry_type(tar::EntryType::Regular);
        header.set_cksum();
        builder.append_data(&mut header, path, e.data.as_slice()).with_context(|| format!("adding {} to tar", e.path))?;
    }
    let archive = builder.into_inner().context("finalizing tar")?;
    Ok(archive)
}

/// Reassembles complete lines from arbitrarily-chunked byte input (one per stream). The
/// daemon's exec `LogOutput` frames split lines at arbitrary boundaries; this holds a
/// partial until a `\n` completes it. `\r\n` is normalized to a trailing-CR strip so
/// CRLF scripts don't leave a stray `\r` on each line.
#[derive(Default)]
pub struct LineSplitter {
    buf: Vec<u8>,
}

impl LineSplitter {
    /// Feed a chunk; fire `on_line` for each completed line (without the trailing `\n`/`\r`).
    pub fn push(&mut self, chunk: &[u8], mut on_line: impl FnMut(&str)) {
        self.buf.extend_from_slice(chunk);
        loop {
            let Some(nl) = self.buf.iter().position(|&b| b == b'\n') else { break };
            let mut line: Vec<u8> = self.buf.drain(..=nl).collect();
            line.pop(); // drop '\n'
            if line.last() == Some(&b'\r') {
                line.pop(); // drop '\r' (CRLF)
            }
            on_line(&String::from_utf8_lossy(&line));
        }
    }

    /// Flush any trailing partial line (input that ended without a newline).
    pub fn flush(&mut self, mut on_line: impl FnMut(&str)) {
        if self.buf.is_empty() {
            return;
        }
        let mut line = std::mem::take(&mut self.buf);
        if line.last() == Some(&b'\r') {
            line.pop();
        }
        on_line(&String::from_utf8_lossy(&line));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- self-spec projection ---------------------------------------------------------

    #[test]
    fn self_spec_from_inspect_projects_fields() {
        // Minimal ContainerInspectResponse JSON (bollard models are Deserialize).
        let json = r#"{
            "Id": "abc123",
            "Name": "/rmng",
            "Image": "sha256:oldimageid",
            "Config": { "Hostname": "rmng", "Env": ["RUST_LOG=info"], "Image": "rmng:latest" },
            "HostConfig": { "Privileged": true, "PidMode": "host" },
            "NetworkSettings": { "Networks": { "rmng": { "Aliases": ["rmng-control"] } } }
        }"#;
        let resp: super::ContainerInspectResponse = serde_json::from_str(json).unwrap();
        let spec = super::SelfSpec::from_inspect(&resp, "pegasis0/rmng:latest").unwrap();
        assert_eq!(spec.container_name, "rmng"); // leading slash stripped
        assert_eq!(spec.old_image_id, "sha256:oldimageid");
        assert_eq!(spec.new_image_ref, "pegasis0/rmng:latest");
        assert_eq!(spec.host_config.privileged, Some(true));
        assert_eq!(spec.host_config.pid_mode.as_deref(), Some("host"));
        // Config (hostname) must survive the projection — the recreated container needs it.
        assert_eq!(spec.config.hostname.as_deref(), Some("rmng"));
        // The network attachment AND its rmng-control alias must survive — the design's key
        // guarantee so the recreated server keeps its stable in-network address.
        let net = spec.networks.get("rmng").expect("rmng network preserved");
        assert!(
            net.aliases.iter().flatten().any(|a| a == "rmng-control"),
            "rmng-control alias must survive projection, got {:?}",
            net.aliases
        );
    }

    // --- client construction ----------------------------------------------------------

    /// A missing socket FILE must not prevent construction (the no-socket boot path:
    /// bare `docker run` without the sock bind). The build error is deferred to
    /// `daemon()`, which carries the socket path in its message for the env row.
    #[test]
    fn connect_without_socket_defers_the_error() {
        let cfg = DockerConfig {
            socket: "/nonexistent/rmng-test-docker.sock".into(),
            ..Default::default()
        };
        let ctl = DockerCtl::connect(&cfg); // must not panic
        let err = format!("{:#}", ctl.daemon().expect_err("daemon() must fail without a socket"));
        assert!(
            err.contains("/nonexistent/rmng-test-docker.sock"),
            "error should name the socket path: {err}"
        );
    }

    #[tokio::test]
    async fn set_subnet_refreshes_derived_network_params() {
        // The `rmng` bridge is derived from DockerCtl.subnet at materialization time (wizard
        // finish / first clone). Before the fix `subnet` was a boot-time snapshot, so a
        // wizard subnet change never reached ensure_network — the bridge came up on the old
        // default and every later boot rejected the mismatch. `set_subnet` must make the
        // derived params (here the dev-mode gateway, same SubnetPlan ensure_network uses)
        // reflect the new subnet immediately.
        let ctl = DockerCtl::connect(&DockerConfig { subnet: "10.99.0.0/24".into(), ..Default::default() });
        assert_eq!(ctl.control_host().await.unwrap(), "10.99.0.1");
        ctl.set_subnet("10.98.0.0/24");
        assert_eq!(ctl.control_host().await.unwrap(), "10.98.0.1");
    }

    // --- cpu percent ------------------------------------------------------------------

    #[test]
    fn cpu_percent_scales_by_online_cpus() {
        // The container burned 50% of the wall-clock CPU delta; on an 8-core host that's
        // 50% × 8 = 400% of one core (i.e. four cores' worth).
        assert_eq!(cpu_percent(500, 0, 1000, 0, 8), 400.0);
        // A single fully-used core on a 1-CPU host reads 100%.
        assert_eq!(cpu_percent(1000, 0, 1000, 0, 1), 100.0);
        // Half of one core on a 4-core host: 50% of the delta on one core = 200%? No — the
        // container used 250/1000 = 25% of the delta, × 4 = 100%.
        assert_eq!(cpu_percent(250, 0, 1000, 0, 4), 100.0);
    }

    #[test]
    fn cpu_percent_uses_deltas_not_absolutes() {
        // Only the movement between the two samples counts: container advanced 200,
        // system advanced 800, on 4 cores → 200/800 × 4 × 100 = 100%.
        assert_eq!(cpu_percent(1200, 1000, 5800, 5000, 4), 100.0);
    }

    #[test]
    fn cpu_percent_zero_system_delta_is_zero_not_nan() {
        // A zero (or backwards) system delta must not divide-by-zero into NaN/∞.
        let v = cpu_percent(500, 0, 1000, 1000, 8);
        assert_eq!(v, 0.0);
        assert!(v.is_finite());
        // System counter went backwards (daemon restart / rollover): still 0, not negative.
        assert_eq!(cpu_percent(500, 0, 900, 1000, 8), 0.0);
    }

    #[test]
    fn cpu_percent_missing_precpu_first_sample_is_zero() {
        // The very first sample has no precpu, so cpu_total == precpu_total (both 0) → a
        // zero container delta → 0%, never a spurious spike.
        assert_eq!(cpu_percent(0, 0, 0, 0, 4), 0.0);
        // Container counter present but system counter absent (both precpu fields 0):
        // system_delta == system, cpu_delta == cpu_total; guard only trips on non-positive
        // deltas, so a real system counter still computes — this checks the degenerate
        // all-zero-precpu case where the system side is also 0.
        assert_eq!(cpu_percent(500, 0, 0, 0, 4), 0.0);
    }

    #[test]
    fn cpu_percent_zero_online_cpus_is_zero() {
        // An unknown CPU count (0) is guarded rather than multiplying to 0 implicitly.
        assert_eq!(cpu_percent(500, 0, 1000, 0, 0), 0.0);
    }

    // --- subnet plan ------------------------------------------------------------------

    #[test]
    fn subnet_masks_host_bits() {
        // A config with host bits set still yields the correct network base + gateway.
        let plan = SubnetPlan::parse("10.99.0.5/24").unwrap();
        assert_eq!(plan.cidr(), "10.99.0.0/24");
        assert_eq!(plan.gateway().to_string(), "10.99.0.1");
    }

    #[test]
    fn subnet_16_wide_gateway() {
        let plan = SubnetPlan::parse("172.30.0.0/16").unwrap();
        assert_eq!(plan.cidr(), "172.30.0.0/16");
        assert_eq!(plan.gateway().to_string(), "172.30.0.1");
    }

    #[test]
    fn prefix_mask_values() {
        assert_eq!(prefix_to_mask(24), 0xFFFF_FF00);
        assert_eq!(prefix_to_mask(16), 0xFFFF_0000);
        assert_eq!(prefix_to_mask(20), 0xFFFF_F000);
        assert_eq!(prefix_to_mask(32), 0xFFFF_FFFF);
    }

    // --- line splitter --------------------------------------------------------------

    fn collect_pushes(chunks: &[&[u8]]) -> (Vec<String>, Vec<String>) {
        let mut splitter = LineSplitter::default();
        let mut mid = Vec::new();
        for c in chunks {
            splitter.push(c, |l| mid.push(l.to_string()));
        }
        let mut flushed = Vec::new();
        splitter.flush(|l| flushed.push(l.to_string()));
        (mid, flushed)
    }

    #[test]
    fn line_splitter_reassembles_across_chunk_boundaries() {
        // A line split mid-word across two chunks emits once, whole.
        let (mid, flushed) = collect_pushes(&[b"hel", b"lo\nwor", b"ld\n"]);
        assert_eq!(mid, vec!["hello", "world"]);
        assert!(flushed.is_empty());
    }

    #[test]
    fn line_splitter_multiple_lines_in_one_chunk() {
        let (mid, flushed) = collect_pushes(&[b"a\nb\nc\n"]);
        assert_eq!(mid, vec!["a", "b", "c"]);
        assert!(flushed.is_empty());
    }

    #[test]
    fn line_splitter_trailing_partial_flushes() {
        // No final newline → the remainder comes out on flush, not before.
        let (mid, flushed) = collect_pushes(&[b"partial line"]);
        assert!(mid.is_empty());
        assert_eq!(flushed, vec!["partial line"]);
    }

    #[test]
    fn line_splitter_strips_crlf() {
        let (mid, flushed) = collect_pushes(&[b"win\r\ndows\r\n"]);
        assert_eq!(mid, vec!["win", "dows"]);
        assert!(flushed.is_empty());
        // A trailing CR without LF is stripped on flush too.
        let (_, flushed2) = collect_pushes(&[b"tail\r"]);
        assert_eq!(flushed2, vec!["tail"]);
    }

    #[test]
    fn line_splitter_empty_lines_preserved() {
        let (mid, _) = collect_pushes(&[b"\n\na\n"]);
        assert_eq!(mid, vec!["", "", "a"]);
    }

    // --- tar building ---------------------------------------------------------------

    #[test]
    fn build_tar_applies_path_mode_uid_gid() {
        let entries = vec![
            TarEntry {
                path: "home/rmng/.config/foo".into(),
                data: b"hello".to_vec(),
                mode: 0o644,
                uid: 1000,
                gid: 1000,
            },
            // A leading slash is stripped so it extracts relative to the request path.
            TarEntry { path: "/etc/motd".into(), data: b"root file".to_vec(), mode: 0o600, uid: 0, gid: 0 },
        ];
        let archive = build_tar(&entries).unwrap();
        // Read it back and assert the header metadata round-trips verbatim.
        let mut ar = tar::Archive::new(archive.as_slice());
        let mut seen: Vec<(String, u32, u64, u64, u64)> = Vec::new();
        for entry in ar.entries().unwrap() {
            let entry = entry.unwrap();
            let h = entry.header();
            seen.push((
                entry.path().unwrap().to_string_lossy().into_owned(),
                h.mode().unwrap(),
                h.uid().unwrap(),
                h.gid().unwrap(),
                h.size().unwrap(),
            ));
        }
        assert_eq!(seen.len(), 2);
        assert_eq!(seen[0], ("home/rmng/.config/foo".into(), 0o644, 1000, 1000, 5));
        // Leading slash stripped.
        assert_eq!(seen[1].0, "etc/motd");
        assert_eq!((seen[1].1, seen[1].2, seen[1].3), (0o600, 0, 0));
    }

    #[test]
    fn build_tar_uid_gid_applied_verbatim() {
        // The API applies whatever it's given; a nonsense uid/gid still round-trips.
        let entries = vec![TarEntry { path: "x".into(), data: vec![], mode: 0o755, uid: 4242, gid: 99 }];
        let archive = build_tar(&entries).unwrap();
        let mut ar = tar::Archive::new(archive.as_slice());
        let e = ar.entries().unwrap().next().unwrap().unwrap();
        assert_eq!(e.header().uid().unwrap(), 4242);
        assert_eq!(e.header().gid().unwrap(), 99);
        assert_eq!(e.header().mode().unwrap(), 0o755);
    }

    // --- reference splitting + timestamp --------------------------------------------

    #[test]
    fn split_reference_defaults_and_ports() {
        assert_eq!(split_reference("ubuntu:26.04"), ("ubuntu".into(), "26.04".into()));
        assert_eq!(split_reference("ubuntu"), ("ubuntu".into(), "latest".into()));
        assert_eq!(split_reference("rmng/template:base"), ("rmng/template".into(), "base".into()));
        // A registry host with a port is not mistaken for a tag.
        assert_eq!(
            split_reference("registry:5000/img:v1"),
            ("registry:5000/img".into(), "v1".into())
        );
        assert_eq!(split_reference("registry:5000/img"), ("registry:5000/img".into(), "latest".into()));
    }

    // --- pull aggregator --------------------------------------------------------------

    #[test]
    fn pull_aggregator_monotonic_under_growing_totals() {
        let mut agg = PullAggregator::default();
        let mut peak = 0.0_f64;
        for (cur, tot) in [(50, 100), (60, 100)] {
            if let Some(f) = agg.observe("a", "Downloading", Some(cur), Some(tot)) {
                assert!(f >= peak, "fraction regressed: {f} < {peak}");
                peak = f;
            }
        }
        assert!((peak - 0.42).abs() < 1e-9, "expected 0.7 * 0.60 = 0.42, got {peak}");

        // A much bigger layer registers mid-pull: the raw sum-based fraction would drop
        // sharply (0.7 * 60/100_060 ≈ 0.00042), but the reported value must never regress
        // below the prior peak even though the totals grew.
        let dropped = agg.observe("b", "Downloading", Some(0), Some(100_000));
        if let Some(f) = dropped {
            assert!(f >= peak, "peak regressed when a large new layer joined: {f} < {peak}");
        }
    }

    #[test]
    fn pull_aggregator_cached_layers_weigh_zero() {
        let mut agg = PullAggregator::default();
        // A fully cached layer (no progress_detail — a real "Already exists" frame never
        // carries one) must not appear in either sum's denominator.
        assert!(agg.observe("a", "Already exists", None, None).is_some());
        let frac = agg.observe("b", "Downloading", Some(50), Some(100)).unwrap();
        assert!(
            (frac - 0.35).abs() < 1e-9,
            "cached layer must not inflate the denominator: expected 0.7 * 0.50 = 0.35, got {frac}"
        );
    }

    #[test]
    fn pull_aggregator_throttles_to_integer_percent_changes() {
        let mut agg = PullAggregator::default();
        let mut emissions = 0;
        let mut last_frac = 0.0_f64;
        // 501 byte-granular updates — far more ticks than there are percent points to cross.
        for cur in 0..=500 {
            if let Some(f) = agg.observe("a", "Downloading", Some(cur), Some(500)) {
                emissions += 1;
                last_frac = f;
            }
        }
        // Download-only progress tops out at 0.7·1.0 = 0.7 (30% is reserved for extract),
        // so at most 71 distinct integer percents (0..=70) can ever be crossed.
        assert!(emissions <= 71, "expected throttled emissions, got {emissions} (of 501 updates)");
        assert!(emissions > 1, "expected more than one emission as the percent climbs");
        assert!((last_frac - 0.7).abs() < 1e-9, "final fraction should reach 0.7, got {last_frac}");
    }

    #[test]
    fn epoch_to_rfc3339_known_values() {
        assert_eq!(epoch_to_rfc3339(0), "1970-01-01T00:00:00Z");
        // 2021-01-01T00:00:00Z
        assert_eq!(epoch_to_rfc3339(1_609_459_200), "2021-01-01T00:00:00Z");
        // A leap-day timestamp: 2020-02-29T12:34:56Z = 1582979696
        assert_eq!(epoch_to_rfc3339(1_582_979_696), "2020-02-29T12:34:56Z");
    }

    #[test]
    fn short_id_strips_sha_prefix() {
        assert_eq!(short_id("sha256:abcdef0123456789"), "abcdef012345");
        assert_eq!(short_id("abcdef0123456789"), "abcdef012345");
    }

    #[test]
    fn is_update_available_compares_digests() {
        // No local digest known → treat as available (can't prove up-to-date).
        assert!(super::is_update_available(None, "sha256:bbb"));
        // Same digest → up to date.
        assert!(!super::is_update_available(Some("sha256:aaa"), "sha256:aaa"));
        // Different digest → update available.
        assert!(super::is_update_available(Some("sha256:aaa"), "sha256:bbb"));
    }

    #[test]
    fn benign_stdin_write_errors_classified() {
        use std::io::{Error, ErrorKind};
        // Early-exit teardown shapes are benign (exit code still comes from inspect_exec).
        for kind in [
            ErrorKind::BrokenPipe,
            ErrorKind::ConnectionReset,
            ErrorKind::UnexpectedEof,
            ErrorKind::WriteZero,
        ] {
            assert!(is_benign_stdin_write_error(&Error::new(kind, "closed")), "{kind:?}");
        }
        // Genuine transport failures are not.
        for kind in [ErrorKind::PermissionDenied, ErrorKind::Other, ErrorKind::TimedOut] {
            assert!(!is_benign_stdin_write_error(&Error::new(kind, "boom")), "{kind:?}");
        }
    }

    #[test]
    fn extract_container_id_from_mountinfo_finds_64hex() {
        let sample = "1 2 0:1 / / rw,relatime shared:1 - overlay overlay rw\n\
            42 41 0:50 /var/lib/docker/containers/\
            0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef/hostname /etc/hostname rw\n";
        assert_eq!(
            extract_container_id_from_mountinfo(sample).as_deref(),
            Some("0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef")
        );
        // No container path → None (dev mode).
        assert_eq!(extract_container_id_from_mountinfo("1 2 0:1 / / rw - overlay overlay rw\n"), None);
    }

    #[test]
    fn env_report_projects_rows_and_dev_mode() {
        // Dev mode (no self-container) is an info row that stays `ok=true`.
        let report = EnvReport {
            daemon_ok: true,
            daemon_version: Some("29.0.1 (API 1.51)".into()),
            daemon_detail: None,
            self_container: None,
            control_host: Some("10.99.0.1".into()),
            network_detail: None,
            sock_mount_ok: true,
            sock_mount_detail: "dev".into(),
            dri_ok: true,
            lxcfs_ok: true,
        };
        assert!(report.required_ok());
        let env = report.to_setup_env();
        assert_eq!(env.rows.len(), 5);

        // A network / self-attach failure is non-fatal: it doesn't fail the required checks
        // (the wizard-finish caller surfaces `network_detail` as a warning) and adds no row.
        let net_fail = EnvReport { network_detail: Some("connect self to rmng failed".into()), ..report.clone() };
        assert!(net_fail.required_ok());
        assert_eq!(net_fail.to_setup_env().rows.len(), 5);
        let by = |id: &str| env.rows.iter().find(|r| r.id == id).unwrap();
        assert!(by("dockerDaemon").ok && by("dockerDaemon").required);
        // self-container info row: not required, ok even in dev mode.
        assert!(by("selfContainer").ok && !by("selfContainer").required);
        assert!(by("selfContainer").detail.contains("dev mode"));
        assert!(by("sockMount").required);
        assert!(by("renderNode").required);
        // lxcfs is advisory (never required); present ⇒ ok + "present".
        assert!(by("lxcfs").ok && !by("lxcfs").required);
        assert_eq!(by("lxcfs").detail, "present");
        // Absent lxcfs stays non-required (advisory) and doesn't fail the required set.
        let no_lxcfs = EnvReport { lxcfs_ok: false, ..report.clone() };
        assert!(no_lxcfs.required_ok());
        let no_lxcfs_env = no_lxcfs.to_setup_env();
        let lxcfs_row = no_lxcfs_env.rows.iter().find(|r| r.id == "lxcfs").unwrap();
        assert!(!lxcfs_row.ok && !lxcfs_row.required);
        assert!(lxcfs_row.detail.contains("apt install lxcfs"));

        // A missing sock mount fails the required check.
        let bad = EnvReport { sock_mount_ok: false, ..report.clone() };
        assert!(!bad.required_ok());
        // A down daemon fails too.
        let down = EnvReport { daemon_ok: false, ..report };
        assert!(!down.required_ok());
        assert!(!down.to_setup_env().rows.iter().find(|r| r.id == "dockerDaemon").unwrap().ok);

        // A stored client-build error (the no-socket boot path) becomes the row detail.
        let dead = EnvReport {
            daemon_detail: Some("Socket not found: /var/run/docker.sock".into()),
            ..down
        };
        let env = dead.to_setup_env();
        let row = env.rows.iter().find(|r| r.id == "dockerDaemon").unwrap();
        assert!(!row.ok);
        assert!(row.detail.contains("Socket not found"), "detail: {}", row.detail);
    }

    #[test]
    fn dind_volume_name_shape() {
        assert_eq!(DockerCtl::dind_volume_name("pega-dev-1"), "rmng-dind-pega-dev-1");
    }

    #[test]
    fn lxcfs_proc_mounts_gated_on_probe() {
        // Absent ⇒ no binds at all (the clone keeps host-wide /proc; no junk risk).
        assert!(lxcfs_proc_mounts(false).is_empty());

        // Present ⇒ exactly the six virtualized files, each host lxcfs source bound rw over
        // the clone's matching /proc/<same-basename> target.
        let mounts = lxcfs_proc_mounts(true);
        assert_eq!(mounts.len(), 6);
        for m in &mounts {
            assert_eq!(m.typ, Some(MountTypeEnum::BIND));
            assert_eq!(m.read_only, Some(false), "lxcfs binds are rw (upstream convention)");
            let src = m.source.as_deref().unwrap();
            let tgt = m.target.as_deref().unwrap();
            assert!(src.starts_with("/var/lib/lxcfs/proc/"), "source under lxcfs dir: {src}");
            assert!(tgt.starts_with("/proc/"), "target under /proc: {tgt}");
            // Same basename on both sides (meminfo → /proc/meminfo, etc.).
            assert_eq!(src.rsplit('/').next(), tgt.rsplit('/').next());
        }
        let targets: Vec<&str> = mounts.iter().map(|m| m.target.as_deref().unwrap()).collect();
        for want in ["/proc/meminfo", "/proc/cpuinfo", "/proc/stat", "/proc/uptime", "/proc/loadavg", "/proc/swaps"] {
            assert!(targets.contains(&want), "missing bind target {want}");
        }
    }
}
