//! Side-effect-free helpers shared by the `claude` and `codex` usage subsystems: a
//! millisecond clock and an error-body snippet formatter for the usage-fetch error paths.

use std::time::{SystemTime, UNIX_EPOCH};

/// Milliseconds since the Unix epoch (0 if the clock is before the epoch).
pub(crate) fn now_ms() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0)
}

/// A short `: <prefix>` of an error body for log lines (empty stays empty).
pub(crate) fn snippet(s: &str) -> String {
    if s.is_empty() { String::new() } else { format!(": {}", &s[..s.len().min(120)]) }
}
