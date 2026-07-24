//! Shared application state handed to every request handler and background job:
//! the state store, the live config, and a shared HTTP client.

use std::sync::{Arc, RwLock};

use wire::AppConfig;

use crate::chat::ChatState;
use crate::cliproxy::CliProxyManager;
use crate::docker::DockerCtl;
use crate::state::StateStore;

#[derive(Clone)]
pub struct App {
    pub store: Arc<StateStore>,
    /// Live config (mutable via `/api/config` in Phase 2; read per use elsewhere).
    pub cfg: Arc<RwLock<AppConfig>>,
    pub http: reqwest::Client,
    /// Transparent CLIProxyAPI transport. Redirects stay client-visible rather than being
    /// followed by the control server, preserving the upstream method and response exactly.
    pub proxy_http: reqwest::Client,
    /// Group-proxy supervisor: per-group CLIProxyAPI instance lifecycle, per-clone router
    /// keys, and management helpers (see [`crate::cliproxy`]).
    pub cliproxy: Arc<CliProxyManager>,
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
    /// Per-clone newly processed token totals. Its browser projection is SSE-only while its
    /// server-private records persist independently from `ControlState`.
    pub tokens: Arc<crate::tokens::TokenBus>,
    /// Volatile per-clone "operator last looked at this clone" timestamps. Set on selection
    /// changes (`web::activate`) and read by the monitor to suppress a `working → idle`
    /// notification for a clone whose latest output the operator has already seen.
    pub views: Arc<crate::monitor::ViewTracker>,
}

impl App {
    pub fn new(store: Arc<StateStore>, cfg: AppConfig) -> Self {
        let cliproxy = Arc::new(CliProxyManager::load(&cfg.data_dir));
        let tokens = Arc::new(crate::tokens::TokenBus::load(&cfg.data_dir));
        tokens.sync_clones(&store.get().hosts);
        // `DockerCtl::connect` is infallible and I/O-free: even a missing socket FILE
        // (bare `docker run` without the sock bind) boots the server — the failure is
        // surfaced per call and by `self_setup`'s env report, so the wizard shows it.
        let docker = Arc::new(DockerCtl::connect(&cfg.docker));
        Self {
            store,
            cfg: Arc::new(RwLock::new(cfg)),
            http: reqwest::Client::builder()
                .user_agent("rmng-control-server")
                .build()
                .expect("reqwest client"),
            proxy_http: reqwest::Client::builder()
                .user_agent("rmng-control-server")
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .expect("transparent proxy client"),
            cliproxy,
            chat: Arc::new(ChatState::default()),
            media: Arc::new(crate::mediaplane::MediaHandle::default()),
            docker,
            stats: Arc::new(crate::monitor::StatsBus::new()),
            lxc_stats: Arc::new(crate::monitor::LxcStatsBus::new()),
            forwards: Arc::new(crate::forward::ForwardBus::new()),
            tokens,
            views: Arc::new(crate::monitor::ViewTracker::new()),
        }
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
