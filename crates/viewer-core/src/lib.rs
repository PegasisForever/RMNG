//! `viewer-core` — the toolkit-free pieces every RMNG viewer front-end shares: the persisted
//! config, the auto pointer-lock policy, the port-forward listeners, the Carbon kVK → evdev
//! table, and how a macOS modifier's up/down state is read from an event's flag word.
//! The GTK viewer (`crates/viewer`) and the native macOS viewer (`crates/viewer-macos`)
//! both build on it; nothing here touches a window, a socket beyond plain `std::net`, or a
//! platform framework.

pub mod auto_lock;
pub mod config;
pub mod forward;
pub mod kvk_evdev;
pub mod kvk_modifiers;
pub mod terminal;
