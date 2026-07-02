//! Shared application state handed to every request handler and background job:
//! the state store, the live config, and a shared HTTP client.

use std::sync::{Arc, RwLock};

use wire::AppConfig;

use crate::chat::ChatState;
use crate::claude::ClaudeStore;
use crate::docker::DockerCtl;
use crate::state::StateStore;

#[derive(Clone)]
pub struct App {
    pub store: Arc<StateStore>,
    /// Live config (mutable via `/api/config` in Phase 2; read per use elsewhere).
    pub cfg: Arc<RwLock<AppConfig>>,
    pub http: reqwest::Client,
    /// Claude secret store + usage cache.
    pub claude: Arc<ClaudeStore>,
    /// Per-host chat fan-out + in-flight state.
    pub chat: Arc<ChatState>,
    /// Media plane shared state (clone conns + latest frames).
    pub media: Arc<crate::mediaplane::MediaHandle>,
    /// The Docker fleet backend (bollard). Constructed I/O-free at startup; every call
    /// surfaces its own daemon-connection failure, so the server still boots the wizard
    /// even when Docker is down.
    pub docker: Arc<DockerCtl>,
}

impl App {
    pub fn new(store: Arc<StateStore>, cfg: AppConfig) -> Self {
        let claude = Arc::new(ClaudeStore::load(&cfg.data_dir));
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
            claude,
            chat: Arc::new(ChatState::default()),
            media: Arc::new(crate::mediaplane::MediaHandle::default()),
            docker,
        }
    }

    /// A cheap snapshot of the current config.
    pub fn config(&self) -> AppConfig {
        self.cfg.read().unwrap().clone()
    }
}
