//! Shared application state handed to every request handler and background job:
//! the state store, the live config, and a shared HTTP client.

use std::sync::{Arc, RwLock};

use wire::AppConfig;

use crate::chat::ChatState;
use crate::claude::ClaudeStore;
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
}

impl App {
    pub fn new(store: Arc<StateStore>, cfg: AppConfig) -> Self {
        let claude = Arc::new(ClaudeStore::load(&cfg.data_dir));
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
        }
    }

    /// A cheap snapshot of the current config.
    pub fn config(&self) -> AppConfig {
        self.cfg.read().unwrap().clone()
    }
}
