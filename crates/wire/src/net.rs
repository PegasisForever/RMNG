//! Connection tuning for the port-1 viewer ⇄ control-server TCP link.
//!
//! `wire` is otherwise a pure-types crate, but the keepalive policy is a property
//! of *this specific boundary* and has to match on both ends, so it lives here as
//! the single source of truth the viewer and control-server both apply — the
//! viewer right after `connect`, the server right after `accept`.

use std::net::TcpStream;
use std::time::Duration;

/// Enable aggressive TCP keepalive (and bound unacked-write retransmit) on a
/// connected port-1 stream.
///
/// Without this, a *silently* dead link never surfaces as a socket error, so a
/// blocking `read` parks forever and neither side's reconnect/cleanup runs. The
/// cases that hit this — none of which deliver a FIN/RST to the peer:
///   - Wi-Fi/cable drop, route or NAT-mapping change mid-session;
///   - an idle remote desktop (damage-driven capture sends no frames, and there
///     is no app-level heartbeat) — so there is *zero* traffic to fail on;
///   - laptop suspend→resume, where the socket comes back stale on a new path.
///
/// Keepalive probes the idle connection and tears it down within ~19 s (10 s idle,
/// then 3 probes at 3 s each), at which point the parked read returns an error and
/// the caller reconnects (viewer) or reaps the per-viewer input thread (server).
///
/// `TCP_USER_TIMEOUT` does the same for the *write* direction: input events / video
/// queued to a vanished peer fail in ~20 s instead of riding the kernel's ~15 min
/// `tcp_retries2` default.
///
/// Linux-only socket options (this whole stack is Linux). Returns the first
/// `setsockopt` error; callers treat it as best-effort (log + continue) — a
/// failure leaves a working but slow-to-fail connection, which is strictly better
/// than refusing the connection.
pub fn set_keepalive(sock: &TcpStream) -> std::io::Result<()> {
    let sock = socket2::SockRef::from(sock);
    sock.set_tcp_keepalive(
        &socket2::TcpKeepalive::new()
            .with_time(Duration::from_secs(10)) // idle before the first probe
            .with_interval(Duration::from_secs(3)) // between probes
            .with_retries(3), // unanswered probes before declaring the peer dead
    )?;
    // `TCP_USER_TIMEOUT` bounds unacked-write retransmit to ~20 s; Linux-only.
    #[cfg(target_os = "linux")]
    sock.set_tcp_user_timeout(Some(Duration::from_secs(20)))?;
    Ok(())
}
