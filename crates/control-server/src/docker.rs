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
//! - The `rmng` bridge is user-defined with a static IPAM: `.1` gateway, `.2`
//!   control-server (so recreating it never strands baked URLs), `.10+` clone pool. IPs
//!   are derived from the network *base* (`addr & mask`), never the raw config string.
//! - bollard exec output is chunk-, not line-aligned — [`LineSplitter`] reassembles
//!   complete lines per stream before the caller's callback fires (gotcha #1).
//! - tar uid/gid is applied verbatim by the daemon; callers set uid/gid 1000 on
//!   `home/rmng/**` entries (gotcha #2).
//! - Clone images need `StopSignal=SIGRTMIN+3` baked in or every stop is a 20 s hang +
//!   SIGKILL (gotcha #5); [`DockerCtl::commit`] with `set_boot_config` does this.
//! - Static IPs survive daemon restarts via endpoint IPAM config (gotcha #6); stale
//!   same-named containers 409 on create — callers get the daemon message verbatim
//!   (gotcha #7).

use std::collections::HashMap;
use std::net::Ipv4Addr;

use anyhow::{Context, Result, anyhow, bail};
use bollard::Docker;
use bollard::container::LogOutput;
use bollard::errors::Error as BollardError;
use bollard::exec::{CreateExecOptions, StartExecResults};
use bollard::models::{
    ContainerConfig, ContainerCreateBody, EndpointIpamConfig, EndpointSettings, HostConfig, Ipam,
    IpamConfig, Mount, MountBindOptions, MountBindOptionsPropagationEnum, MountPointTypeEnum,
    MountTypeEnum, NetworkConnectRequest, NetworkCreateRequest, NetworkingConfig, RestartPolicy,
    RestartPolicyNameEnum, VolumeCreateOptions,
};
use bollard::query_parameters::{
    CommitContainerOptionsBuilder, CreateContainerOptionsBuilder, CreateImageOptionsBuilder,
    ListImagesOptionsBuilder, RemoveContainerOptionsBuilder, RemoveImageOptionsBuilder,
    RemoveVolumeOptionsBuilder, StopContainerOptionsBuilder,
};
use futures::StreamExt;
use tokio::io::AsyncWriteExt;
use tokio::sync::RwLock;
use wire::{DockerConfig, EnvCheckRow, ImageInfo, SetupEnv};

// --- Constants ------------------------------------------------------------------------

/// The user-defined bridge every clone (and the control-server) attaches to. Created
/// lazily at wizard finish + before each clone; its subnet is one-time config.
pub const NETWORK: &str = "rmng";
/// Repository namespace for clone-source images (`rmng/template:<name>`).
pub const IMAGE_REPO: &str = "rmng/template";
/// The fixed base OS for the wizard-built base image. Not configurable: the patched
/// gnome-shell deb is compiled against this GNOME only (see gotcha in the LXC path).
pub const BASE_DOCKER_IMAGE: &str = "ubuntu:26.04";
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

/// The clone-daemon media socket bind target inside every clone.
const SOCK_DIR: &str = "/srv/rmng-sock";
/// Where each clone's per-clone named volume mounts (inner Docker state — never
/// committed, see gotcha #11).
const DIND_TARGET: &str = "/var/lib/docker";
/// Extra swap over the memory limit, in bytes (+8 GiB, matching LXC parity).
const SWAP_BYTES: i64 = 8 * 1024 * 1024 * 1024;

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
    /// The control-server's IP on the rmng network (subnet `.2`; dev mode = gateway `.1`).
    pub control_ip: Option<String>,
    /// The shared clone-socket mount is present on our own container (required).
    pub sock_mount_ok: bool,
    /// Detail for the sock-mount row (the discovered source path, or why it's missing).
    pub sock_mount_detail: String,
    /// `/dev/dri/renderD128` exists (required for the media/streaming plane).
    pub dri_ok: bool,
}

impl EnvReport {
    /// True when nothing *required* failed. `self_container = None` (dev mode) is an
    /// informational state, never a failure.
    pub fn required_ok(&self) -> bool {
        self.daemon_ok && self.sock_mount_ok && self.dri_ok
    }

    /// Project into the wire DTO `GET /api/setup/env` returns. Rows, in order: daemon
    /// reachability, self-container detection (info — absence = dev mode), sock-mount
    /// presence (required), `/dev/dri/renderD128` presence (required).
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
    subnet: String,
    env: RwLock<EnvReport>,
}

/// The set of things needed to create a clone container. `provision.rs` fills this from
/// config + the chosen image + allocated IP.
#[derive(Debug, Clone)]
pub struct CreateSpec {
    /// Container name (= host id, DNS-label-safe): e.g. `pega-dev-123`.
    pub name: String,
    /// Source image reference or id.
    pub image: String,
    /// Static IPv4 on the rmng network (from [`DockerCtl::allocate_ip`]).
    pub ip: String,
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
            subnet: cfg.subnet.clone(),
            env: RwLock::new(EnvReport::default()),
        }
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

    /// The control-server's own IP on the rmng network: `.2` normally, or the gateway
    /// `.1` in dev mode (server on the host, not a container on the bridge). Reads the
    /// cached report; falls back to computing from the subnet if `self_setup` hasn't run.
    pub async fn control_ip(&self) -> Result<String> {
        if let Some(ip) = self.env.read().await.control_ip.clone() {
            return Ok(ip);
        }
        // Not yet probed: derive from config (dev-mode gateway is the safe default until
        // a self-container is detected).
        let plan = SubnetPlan::parse(&self.subnet)?;
        Ok(plan.gateway().to_string())
    }

    // --- self-setup -------------------------------------------------------------------

    /// Probe the environment and refresh [`EnvReport`]. Called at startup, from
    /// `config_test("docker")`, and at wizard finish. Steps:
    /// 1. ping + version (daemon reachable?),
    /// 2. self-detect our own container id (hostname inspect → `/proc/self/mountinfo`
    ///    fallback → none = dev mode),
    /// 3. `ensure_network()` **only when** `setup_complete` (network is lazy),
    /// 4. control IP = `.2` (managed) / `.1` (dev mode); connect self at `.2` when both
    ///    a self-container and the network exist,
    /// 5. sock-mount discovery from our own mounts (required),
    /// 6. `dri_ok` = `/dev/dri/renderD128` exists.
    ///
    /// Never bails on a down daemon — it records the failure in the report so the wizard
    /// can render it. `setup_complete` is passed in by the caller (`App` reads config).
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
                report.control_ip = SubnetPlan::parse(&self.subnet).ok().map(|p| p.gateway().to_string());
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
            }
        }

        // 4. control IP + connect self at .2 (managed clone-fleet mode).
        match SubnetPlan::parse(&self.subnet) {
            Ok(plan) => {
                if let Some(id) = &report.self_container {
                    report.control_ip = Some(plan.control_server().to_string());
                    // Best-effort: attach ourselves to the network at .2 so baked
                    // RMNG_CONTROL_URLs resolve. Only meaningful once the network exists.
                    if setup_complete {
                        if let Err(e) = self.connect_self_to_network(id, &plan.control_server().to_string()).await {
                            tracing::debug!(target: "docker", "connect self to {NETWORK}: {e}");
                        }
                    }
                } else {
                    // dev mode: the server is on the host; clones reach it via the gateway.
                    report.control_ip = Some(plan.gateway().to_string());
                }
            }
            Err(e) => tracing::warn!(target: "docker", "subnet {:?} unparseable: {e}", self.subnet),
        }

        // 5. sock-mount discovery from our own container's mounts.
        let (ok, detail) = self.discover_sock_mount(report.self_container.as_deref()).await;
        report.sock_mount_ok = ok;
        report.sock_mount_detail = detail;

        // 6. render node.
        report.dri_ok = std::path::Path::new("/dev/dri/renderD128").exists();

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

    /// Attach our own container to the rmng network at the given static IP (idempotent —
    /// an already-connected endpoint is fine).
    async fn connect_self_to_network(&self, self_id: &str, ip: &str) -> Result<()> {
        let cfg = NetworkConnectRequest {
            container: Some(self_id.to_string()),
            endpoint_config: Some(EndpointSettings {
                ipam_config: Some(EndpointIpamConfig {
                    ipv4_address: Some(ip.to_string()),
                    ..Default::default()
                }),
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
        let plan = SubnetPlan::parse(&self.subnet)?;
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

    /// Allocate the lowest free clone IP: the pool is `.10`..=last-usable, minus the
    /// reserved `.1`/`.2` and anything already taken (from `state.json` — passed in — and
    /// live network endpoints). Errors on pool exhaustion.
    ///
    /// `reserved` carries IPs the caller already knows are in use (e.g. `state.json`
    /// hosts) that may not yet appear in the live network inspect.
    pub async fn allocate_ip(&self, reserved: &[String]) -> Result<String> {
        let plan = SubnetPlan::parse(&self.subnet)?;
        let mut taken: std::collections::BTreeSet<Ipv4Addr> = std::collections::BTreeSet::new();
        // From the live network (endpoints + explicit IPAM allocations).
        if let Ok(net) = self.daemon()?.inspect_network(NETWORK, None::<bollard::query_parameters::InspectNetworkOptions>).await {
            for c in net.containers.into_iter().flatten().map(|(_, v)| v) {
                if let Some(addr) = c.ipv4_address.as_deref().and_then(parse_cidr_ip) {
                    taken.insert(addr);
                }
            }
        }
        // From the caller's known set (state.json).
        for s in reserved {
            if let Ok(a) = s.parse::<Ipv4Addr>() {
                taken.insert(a);
            }
        }
        plan.lowest_free(&taken)
            .map(|a| a.to_string())
            .ok_or_else(|| anyhow!("the {NETWORK} clone IP pool ({}) is exhausted", plan.cidr()))
    }

    // --- images -----------------------------------------------------------------------

    /// Pull an image, streaming progress to the callback (deduped per layer id so the
    /// Operation log gets one line per layer instead of every byte tick). Surfaces the
    /// daemon error verbatim (e.g. Docker Hub rate limits on `ubuntu:26.04`, gotcha #9).
    pub async fn pull_image(&self, reference: &str, mut on_progress: impl FnMut(&str, &str)) -> Result<()> {
        let (image, tag) = split_reference(reference);
        let opts = CreateImageOptionsBuilder::new().from_image(&image).tag(&tag).build();
        let docker = self.daemon()?;
        let mut stream = docker.create_image(Some(opts), None, None);
        // Track the last status emitted per layer so we don't spam a line per byte.
        let mut last: HashMap<String, String> = HashMap::new();
        while let Some(item) = stream.next().await {
            let info = item.with_context(|| format!("pulling {reference}"))?;
            let id = info.id.clone().unwrap_or_default();
            let status = info.status.clone().unwrap_or_default();
            if status.is_empty() {
                continue;
            }
            // Emit once per (layer, status) transition.
            if last.get(&id).map(|s| s != &status).unwrap_or(true) {
                last.insert(id.clone(), status.clone());
                let msg = if id.is_empty() { status.clone() } else { format!("{id}: {status}") };
                on_progress("pull", &msg);
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

    /// List clone-source images (label `rmng.image=1`), newest first, projected to the
    /// wire [`ImageInfo`]. `in_use_by` is left empty here — the caller (Task 6) fills it
    /// from `state.json` (which host runs on which image), since that's control-plane
    /// state this layer doesn't own.
    pub async fn list_rmng_images(&self) -> Result<Vec<ImageInfo>> {
        let filters: HashMap<String, Vec<String>> =
            HashMap::from([("label".to_string(), vec![format!("{LABEL_IMAGE}=1")])]);
        let opts = ListImagesOptionsBuilder::new().all(false).filters(&filters).build();
        let summaries = self.daemon()?.list_images(Some(opts)).await.context("listing rmng images")?;
        let mut out: Vec<ImageInfo> = summaries
            .into_iter()
            .map(|s| {
                let reference = s
                    .repo_tags
                    .iter()
                    .find(|t| t.starts_with(&format!("{IMAGE_REPO}:")))
                    .or_else(|| s.repo_tags.first())
                    .cloned()
                    .unwrap_or_else(|| s.id.clone());
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

    /// Commit a container to an image at `rmng/template:<name>`. With `set_boot_config`,
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
            .repo(IMAGE_REPO)
            .tag(name)
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
            .with_context(|| format!("committing {container} to {IMAGE_REPO}:{name}"))?;
        tracing::info!(target: "docker", "committed {container} -> {IMAGE_REPO}:{name} ({})", short_id(&res.id));
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

    /// Create a privileged systemd-PID-1 clone container on the rmng network at a static
    /// IP. Bakes the stop signal + timeout, mounts the shared clone socket + a per-clone
    /// `rmng-dind-<name>` volume at `/var/lib/docker` (overlay-on-overlay fix), applies
    /// CPU/memory (+8 GiB swap) limits and `restart: unless-stopped`. Returns the new
    /// container id. Does NOT start it (caller decides). A stale same-named container
    /// 409s here — the daemon message is surfaced verbatim (gotcha #7).
    pub async fn create_clone_container(&self, spec: &CreateSpec) -> Result<String> {
        let dind_volume = format!("rmng-dind-{}", spec.name);
        // Ensure the per-clone inner-Docker volume exists (idempotent).
        self.ensure_volume(&dind_volume).await?;

        let mut mounts = vec![Mount {
            target: Some(DIND_TARGET.to_string()),
            source: Some(dind_volume.clone()),
            typ: Some(MountTypeEnum::VOLUME),
            ..Default::default()
        }];
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

        let mem = (spec.memory_mb as i64) * 1024 * 1024;
        let host_config = HostConfig {
            privileged: Some(true),
            nano_cpus: Some((spec.cpus as i64) * 1_000_000_000),
            memory: Some(mem),
            memory_swap: Some(mem + SWAP_BYTES),
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
                    EndpointSettings {
                        ipam_config: Some(EndpointIpamConfig {
                            ipv4_address: Some(spec.ip.clone()),
                            ..Default::default()
                        }),
                        ..Default::default()
                    },
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

    /// Create a throwaway build worker: a `sleep infinity` container on the **default
    /// bridge** (so NAT + apt work during base-image provisioning — no dependency on the
    /// rmng network existing yet), privileged (systemd unit ops in the build need it),
    /// from the given base image. Started here so the caller can immediately exec into
    /// it. Returns the container id.
    pub async fn create_build_container(&self, name: &str, image: &str) -> Result<String> {
        let host_config = HostConfig { privileged: Some(true), ..Default::default() };
        let body = ContainerCreateBody {
            hostname: Some(name.to_string()),
            image: Some(image.to_string()),
            entrypoint: Some(vec!["/bin/sleep".to_string()]),
            cmd: Some(vec!["infinity".to_string()]),
            labels: Some(HashMap::from([(LABEL_MANAGED.to_string(), "1".to_string())])),
            host_config: Some(host_config),
            ..Default::default()
        };
        let opts = CreateContainerOptionsBuilder::new().name(name).build();
        let res = self
            .daemon()?
            .create_container(Some(opts), body)
            .await
            .with_context(|| format!("creating build container {name}"))?;
        self.start_container(&res.id).await?;
        Ok(res.id)
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

    /// The container's IPv4 on the rmng network, or `None` if not attached / not running.
    /// Public primitive kept for reconciliation flows; `provision` allocates + tracks IPs via
    /// `state.json` today, so nothing in-crate calls it yet.
    #[allow(dead_code)]
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

    /// Exec a command and capture combined stdout+stderr as a single string, plus the
    /// exit code. For short probes (`id -u rmng`, `test -e …`) — no streaming. Non-zero
    /// exit is NOT an error here; the caller inspects `code`.
    pub async fn exec_capture(&self, container: &str, cmd: &[&str]) -> Result<(i64, String)> {
        let exec = self
            .daemon()?
            .create_exec(
                container,
                CreateExecOptions {
                    cmd: Some(cmd.iter().map(|s| s.to_string()).collect()),
                    attach_stdout: Some(true),
                    attach_stderr: Some(true),
                    ..Default::default()
                },
            )
            .await
            .with_context(|| format!("creating exec in {container}"))?;

        let mut out = String::new();
        if let StartExecResults::Attached { mut output, .. } =
            self.daemon()?.start_exec(&exec.id, None).await?
        {
            while let Some(chunk) = output.next().await {
                let chunk = chunk?;
                out.push_str(&String::from_utf8_lossy(chunk.as_ref()));
            }
        }
        let code = self.daemon()?.inspect_exec(&exec.id).await?.exit_code.unwrap_or(-1);
        Ok((code, out))
    }

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
}

/// Build a bollard client for `socket`. No daemon I/O — bollard only validates that the
/// socket path exists, which is exactly the failure [`DockerCtl::daemon`] retries on.
fn build_client(socket: &str) -> Result<Docker> {
    Docker::connect_with_unix(socket, 120, bollard::API_DEFAULT_VERSION)
        .with_context(|| format!("connecting to the Docker daemon at {socket}"))
}

// --- Pure helpers ---------------------------------------------------------------------

/// A parsed subnet with the derived reserved/pool addresses. Everything is computed from
/// the network *base* (`addr & mask`), so a config like `10.99.0.5/24` still yields the
/// correct `.1`/`.2`/`.10+` on the `10.99.0.0/24` network (host bits masked off).
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

    /// `.1` — the bridge gateway (also the control-server address in dev mode).
    pub fn gateway(&self) -> Ipv4Addr {
        Ipv4Addr::from(self.base + 1)
    }

    /// `.2` — the static control-server address on the bridge.
    pub fn control_server(&self) -> Ipv4Addr {
        Ipv4Addr::from(self.base + 2)
    }

    /// First clone-pool address (`.10`).
    pub fn pool_start(&self) -> Ipv4Addr {
        Ipv4Addr::from(self.base + 10)
    }

    /// Last usable address of the subnet (broadcast − 1).
    pub fn pool_end(&self) -> Ipv4Addr {
        Ipv4Addr::from(self.broadcast() - 1)
    }

    /// The subnet broadcast address (all host bits set).
    fn broadcast(&self) -> u32 {
        let mask = prefix_to_mask(self.prefix);
        self.base | !mask
    }

    /// Lowest free pool address not in `taken` and not the reserved `.1`/`.2`. `None` on
    /// exhaustion.
    pub fn lowest_free(&self, taken: &std::collections::BTreeSet<Ipv4Addr>) -> Option<Ipv4Addr> {
        let start = u32::from(self.pool_start());
        let end = u32::from(self.pool_end());
        let g = u32::from(self.gateway());
        let c = u32::from(self.control_server());
        (start..=end)
            .map(Ipv4Addr::from)
            .find(|a| {
                let n = u32::from(*a);
                n != g && n != c && !taken.contains(a)
            })
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

/// Parse the `10.99.0.5/24` form Docker returns for a container endpoint into just the
/// address.
fn parse_cidr_ip(s: &str) -> Option<Ipv4Addr> {
    s.split('/').next()?.parse().ok()
}

/// Split an image reference into `(name-without-tag, tag)`, defaulting the tag to
/// `latest`. Handles a registry host with a port (`host:5000/img:tag`) by only treating
/// the final `:` after the last `/` as the tag separator.
fn split_reference(reference: &str) -> (String, String) {
    let last_slash = reference.rfind('/').map(|i| i + 1).unwrap_or(0);
    match reference[last_slash..].rfind(':') {
        Some(rel) => {
            let abs = last_slash + rel;
            (reference[..abs].to_string(), reference[abs + 1..].to_string())
        }
        None => (reference.to_string(), "latest".to_string()),
    }
}

/// A short (12-hex) form of a full container/image id for log lines. `sha256:` prefixes
/// are stripped first.
fn short_id(id: &str) -> String {
    let id = id.strip_prefix("sha256:").unwrap_or(id);
    id.chars().take(12).collect()
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
fn epoch_to_rfc3339(secs: i64) -> String {
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
    use std::collections::BTreeSet;

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

    // --- IP allocator ---------------------------------------------------------------

    #[test]
    fn subnet_masks_host_bits() {
        // A config with host bits set still yields the correct network base + reserved.
        let plan = SubnetPlan::parse("10.99.0.5/24").unwrap();
        assert_eq!(plan.cidr(), "10.99.0.0/24");
        assert_eq!(plan.gateway().to_string(), "10.99.0.1");
        assert_eq!(plan.control_server().to_string(), "10.99.0.2");
        assert_eq!(plan.pool_start().to_string(), "10.99.0.10");
        assert_eq!(plan.pool_end().to_string(), "10.99.0.254"); // .255 is broadcast
    }

    #[test]
    fn subnet_16_wide_pool_bounds() {
        let plan = SubnetPlan::parse("172.30.0.0/16").unwrap();
        assert_eq!(plan.gateway().to_string(), "172.30.0.1");
        assert_eq!(plan.control_server().to_string(), "172.30.0.2");
        assert_eq!(plan.pool_start().to_string(), "172.30.0.10");
        assert_eq!(plan.pool_end().to_string(), "172.30.255.254"); // .255.255 broadcast
    }

    #[test]
    fn allocate_lowest_free_skips_reserved_and_taken() {
        let plan = SubnetPlan::parse("10.99.0.0/24").unwrap();
        // Empty → first pool address.
        assert_eq!(plan.lowest_free(&BTreeSet::new()).unwrap().to_string(), "10.99.0.10");
        // .10 and .11 taken → .12.
        let taken: BTreeSet<Ipv4Addr> =
            ["10.99.0.10", "10.99.0.11"].iter().map(|s| s.parse().unwrap()).collect();
        assert_eq!(plan.lowest_free(&taken).unwrap().to_string(), "10.99.0.12");
    }

    #[test]
    fn allocate_never_returns_reserved_even_if_freed() {
        // .1 and .2 are never handed out, even though they're below the pool start,
        // and even if a stale "taken" set omits them.
        let plan = SubnetPlan::parse("10.99.0.0/24").unwrap();
        let chosen = plan.lowest_free(&BTreeSet::new()).unwrap();
        assert_ne!(chosen, plan.gateway());
        assert_ne!(chosen, plan.control_server());
        assert!(u32::from(chosen) >= u32::from(plan.pool_start()));
    }

    #[test]
    fn allocate_pool_exhaustion() {
        // A tiny /30-equivalent isn't reachable via config (min /24), so simulate
        // exhaustion by taking every pool address in a /24.
        let plan = SubnetPlan::parse("10.99.0.0/24").unwrap();
        let start = u32::from(plan.pool_start());
        let end = u32::from(plan.pool_end());
        let full: BTreeSet<Ipv4Addr> = (start..=end).map(Ipv4Addr::from).collect();
        assert!(plan.lowest_free(&full).is_none());
    }

    #[test]
    fn prefix_mask_values() {
        assert_eq!(prefix_to_mask(24), 0xFFFF_FF00);
        assert_eq!(prefix_to_mask(16), 0xFFFF_0000);
        assert_eq!(prefix_to_mask(20), 0xFFFF_F000);
        assert_eq!(prefix_to_mask(32), 0xFFFF_FFFF);
    }

    #[test]
    fn parse_cidr_ip_strips_prefix() {
        assert_eq!(parse_cidr_ip("10.99.0.5/24"), Some("10.99.0.5".parse().unwrap()));
        assert_eq!(parse_cidr_ip("10.99.0.5"), Some("10.99.0.5".parse().unwrap()));
        assert_eq!(parse_cidr_ip(""), None);
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
            control_ip: Some("10.99.0.1".into()),
            sock_mount_ok: true,
            sock_mount_detail: "dev".into(),
            dri_ok: true,
        };
        assert!(report.required_ok());
        let env = report.to_setup_env();
        assert_eq!(env.rows.len(), 4);
        let by = |id: &str| env.rows.iter().find(|r| r.id == id).unwrap();
        assert!(by("dockerDaemon").ok && by("dockerDaemon").required);
        // self-container info row: not required, ok even in dev mode.
        assert!(by("selfContainer").ok && !by("selfContainer").required);
        assert!(by("selfContainer").detail.contains("dev mode"));
        assert!(by("sockMount").required);
        assert!(by("renderNode").required);

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
}
