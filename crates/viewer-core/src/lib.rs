//! `viewer-core` — the toolkit-free pieces every RMNG viewer front-end shares: the persisted
//! config, the auto pointer-lock policy, the port-forward listeners, and the Carbon kVK → evdev
//! table. The GTK viewer (`crates/viewer`) and the native macOS viewer (`crates/viewer-macos`)
//! both build on it; nothing here touches a window, a socket beyond plain `std::net`, or a
//! platform framework.

pub mod auto_lock;
pub mod config;
pub mod forward;
pub mod kvk_evdev;
pub mod terminal;
