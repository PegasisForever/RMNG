//! Shared application state handed to every request handler and background job:
//! the state store, the live config, and the shared HTTP client.

use std::sync::{Arc, RwLock};

use wire::AppConfig;

use crate::chat::ChatState;
use crate::claude::ClaudeStore;
use crate::clonekey::CloneKeys;
use crate::codex::CodexStore;
use crate::docker::DockerCtl;
use crate::state::StateStore;

#[derive(Clone)]
pub struct App {
    pub store: Arc<StateStore>,
    /// Live config (mutable via `/api/config` in Phase 2; read per use elsewhere).
    pub cfg: Arc<RwLock<AppConfig>>,
    pub http: reqwest::Client,
    /// Claude accounts: the 0600 OAuth secret store + last-good usage cache. The server owns
    /// each account's refresh lifecycle and pushes only short-lived access tokens into clones
    /// (see [`crate::claude`]) — nothing that can refresh ever leaves this process.
    pub claude: Arc<ClaudeStore>,
    /// Codex (ChatGPT) accounts — the sibling of [`Self::claude`] (see [`crate::codex`]).
    pub codex: Arc<CodexStore>,
    /// Per-clone identity bearers (`RMNG_PROXY_KEY`). Outlived the group proxy that minted them:
    /// sub-clone parent detection and clone↔clone SSH both key off this (see [`crate::clonekey`]).
    pub clone_keys: Arc<CloneKeys>,
    /// Per-clone chat fan-out + in-flight state.
    pub chat: Arc<ChatState>,
    /// Media plane shared state (clone conns + latest frames).
    pub media: Arc<crate::mediaplane::MediaHandle>,
    /// The Docker fleet backend (bollard). Constructed I/O-free at startup; every call
    /// surfaces its own daemon-connection failure, so the server still boots the wizard
    /// even when Docker is down.
    pub docker: Arc<DockerCtl>,
    /// Volatile per-clone CPU/RAM usage bus. The monitor poller publishes a stats map each
    /// tick; `/events` fans it out as a named `stats` SSE event. SSE-only — never persisted
    /// to `state.json` (see [`crate::monitor::StatsBus`]).
    pub stats: Arc<crate::monitor::StatsBus>,
    /// Volatile CT 105-wide resource usage, published as the named `lxcStats` SSE event.
    /// This includes the control-server and Docker infrastructure, unlike the clone map.
    pub lxc_stats: Arc<crate::monitor::LxcStatsBus>,
    /// Volatile port-forward runtime status. Published by the media plane (viewer
    /// reports + data-conn counts); `/events` fans it out as a named `forwards` SSE
    /// event. SSE-only — never persisted (see [`crate::forward::ForwardBus`]).
    pub forwards: Arc<crate::forward::ForwardBus>,
    /// Volatile per-clone agent-activity timestamps, fed by the agent-wrapper's `busy`/`activity`
    /// SSE frames. The `working` vs `idle` signal (see [`crate::monitor::ActivityBus`]).
    pub activity: Arc<crate::monitor::ActivityBus>,
    /// Volatile per-clone "operator last looked at this clone" timestamps. Set on selection
    /// changes (`web::activate`) and read by the monitor to suppress a `working → idle`
    /// notification for a clone whose latest output the operator has already seen.
    pub views: Arc<crate::monitor::ViewTracker>,
    /// What this process is, for the browser's benefit. Starts as a per-boot id and becomes
    /// the running image's git revision once Docker answers at startup. `/events` sends it on
    /// connect, and a page that sees it change reloads itself, so an upgraded server never
    /// leaves an old bundle talking to it.
    build_id: Arc<RwLock<String>>,
}

/// A value unique to this process. Used until (and instead of) the image revision, so a dev
/// run without an image label still tells its browsers apart across restarts.
fn boot_id() -> String {
    let millis = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    format!("boot-{}-{millis}", std::process::id())
}

impl App {
    pub fn new(store: Arc<StateStore>, cfg: AppConfig) -> Self {
        let claude = Arc::new(ClaudeStore::load(&cfg.data_dir));
        let codex = Arc::new(CodexStore::load(&cfg.data_dir));
        let clone_keys = Arc::new(CloneKeys::load(&cfg.data_dir));
        // `DockerCtl::connect` is infallible and I/O-free: even a missing socket FILE
        // (bare `docker run` without the sock bind) boots the server — the failure is
        // surfaced per call and by `self_setup`'s env report, so the wizard shows it.
        let docker = Arc::new(DockerCtl::connect(&cfg.docker));
        Self {
            store,
            cfg: Arc::new(RwLock::new(cfg)),
            // A paused clone keeps its IP and its listening sockets, so a connection to one
            // is accepted by the kernel and then answered by nobody — a dial that used to
            // fail instantly against a stopped clone now hangs instead. These two bounds are
            // what keep an archived clone from parking a caller forever.
            //
            // `read_timeout`, not `timeout`: it bounds the gap between bytes rather than the
            // whole request, so the agent's `/events` stream and other long-lived reads stay
            // open as long as they are actually delivering.
            http: reqwest::Client::builder()
                .user_agent("rmng-control-server")
                .connect_timeout(std::time::Duration::from_secs(5))
                .read_timeout(std::time::Duration::from_secs(120))
                .build()
                .expect("reqwest client"),
            claude,
            codex,
            clone_keys,
            chat: Arc::new(ChatState::default()),
            media: Arc::new(crate::mediaplane::MediaHandle::default()),
            docker,
            stats: Arc::new(crate::monitor::StatsBus::new()),
            lxc_stats: Arc::new(crate::monitor::LxcStatsBus::new()),
            forwards: Arc::new(crate::forward::ForwardBus::new()),
            activity: Arc::new(crate::monitor::ActivityBus::new()),
            views: Arc::new(crate::monitor::ViewTracker::new()),
            build_id: Arc::new(RwLock::new(boot_id())),
        }
    }

    /// Publish the running image's revision as this process's build identity, replacing the
    /// boot id. Called once at startup after Docker answers; a dev run (no self container, or
    /// an image built without `GIT_SHA`) keeps the boot id.
    pub fn set_build_id(&self, revision: &str) {
        if revision.is_empty() {
            return;
        }
        *self.build_id.write().unwrap() = revision.to_string();
    }

    /// What `/events` reports as `version`. A browser reloads itself when this changes, so it
    /// must be stable for the life of a process and different across an upgrade.
    pub fn build_id(&self) -> String {
        self.build_id.read().unwrap().clone()
    }

    /// A cheap snapshot of the current config.
    pub fn config(&self) -> AppConfig {
        self.cfg.read().unwrap().clone()
    }

    /// A minimal App backed by a throwaway temp data dir, for unit tests in sibling
    /// modules (state + stores are file-isolated; Docker is constructed I/O-free).
    #[cfg(test)]
    #[allow(dead_code)] // reusable test fixture; sibling-module tests may use it
    pub fn test_app() -> Self {
        use std::sync::atomic::{AtomicU32, Ordering};
        static N: AtomicU32 = AtomicU32::new(0);
        let dir = std::env::temp_dir().join(format!(
            "rmng-cloneops-test-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let store =
            std::sync::Arc::new(crate::state::StateStore::load(dir.join("state.json")).unwrap());
        let cfg = wire::AppConfig {
            data_dir: dir.to_string_lossy().into_owned(),
            ..Default::default()
        };
        Self::new(store, cfg)
    }

    /// What to dial a clone's in-clone services at (agent-wrapper chat and the clone-daemon
    /// MCP). Managed clones are addressed by container name (== clone id):
    /// Docker's embedded DNS serves it on the rmng bridge. In dev mode the server runs
    /// on the Docker host, which can't use that resolver — so resolve the clone's bridge
    /// IP via an inspect instead (host processes can route to bridge IPs directly).
    /// Unmanaged rows keep their literal `host` endpoint.
    pub async fn dial_clone(&self, host: &wire::RmngClone) -> String {
        if !host.managed {
            return host.host.clone();
        }
        if self.docker.env().await.self_container.is_some() {
            return host.id.clone();
        }
        match self.docker.inspect_ip(&host.id).await {
            Ok(Some(ip)) => ip,
            // Stopped/gone or daemon hiccup: fall back to the name — the dial will fail
            // with a connection error, which callers already treat as offline.
            _ => host.id.clone(),
        }
    }
}
