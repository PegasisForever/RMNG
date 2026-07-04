//! Clipboard bridge over Mutter RemoteDesktop's selection API — **rich + lazy**.
//!
//! Rich: any MIME type Mutter advertises (text, HTML, images, …), not just text.
//! Lazy: a copy only *advertises* the available types ([`ClipboardOffer`]); the
//! bytes move only when something actually pastes ([`ClipboardRequest`] →
//! [`ClipboardData`]). control-server brokers the offers/requests centrally.
//!
//! Four flows:
//!   1. A clone app copies → `SelectionOwnerChanged` (we're not the owner) →
//!      ship `ClipboardOffer{serial, all mime types}` (no bytes yet).
//!   2. A remote endpoint pastes our clone's selection → broker sends
//!      `ServerMsg::ClipboardRequest{serial, mime}` → `SelectionRead(mime)` →
//!      ship `ClipboardData{serial, mime, bytes}`.
//!   3. A remote clipboard becomes available → `ServerMsg::ClipboardOffer` →
//!      `SetSelection(mime types)` (we own the clone's selection) + remember it.
//!   4. A clone app pastes that remote selection → `SelectionTransfer{mime,serial}`
//!      → ship `ClipboardRequest` to the broker → on the `ClipboardData` reply →
//!      `SelectionWrite` the bytes to Mutter's fd.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use futures::StreamExt;
use tokio::sync::Mutex;
use tokio::sync::mpsc::UnboundedReceiver;
use wire::socket::{ClipboardData, ClipboardOffer, ClipboardRequest, DaemonMsg};
use zbus::zvariant::{OwnedValue, Value};

use crate::ActiveSession;
use crate::transport::Transport;

/// A clipboard message arriving from control-server (routed by `main`'s reader).
pub enum FromServer {
    Offer(ClipboardOffer),
    Request(ClipboardRequest),
    Data(ClipboardData),
}

/// Wire up the four clipboard flows. Runs for the process lifetime.
///
/// Reads the CURRENT Mutter session from `active` per selection op (and re-subscribes its
/// signal streams when they end), so it follows the make-before-break session swap: after
/// a swap the old session's rd is stopped, and all ops must target the new one. The new
/// session's clipboard is enabled by `reconfigure` (`EnableClipboard` on the new rd).
pub async fn run(
    active: ActiveSession,
    transport: Arc<Transport>,
    mut from_server: UnboundedReceiver<FromServer>,
) {
    {
        let rd = active.lock().await.rd.clone();
        if let Err(e) = rd.enable_clipboard(HashMap::new()).await {
            tracing::warn!("EnableClipboard failed (clipboard sync off): {e}");
            return;
        }
    }
    tracing::info!("clipboard sync enabled (rich + lazy)");

    let local_serial = Arc::new(AtomicU64::new(1));
    // The remote selection we currently own in the clone (to request on transfer).
    let remote: Arc<Mutex<Option<ClipboardOffer>>> = Arc::new(Mutex::new(None));
    // SelectionTransfers awaiting broker data, keyed by MIME → Mutter serials.
    let pending: Arc<Mutex<HashMap<String, Vec<u32>>>> = Arc::new(Mutex::new(HashMap::new()));

    // Flow 1: a clone app copied → advertise its MIME types (lazy, no bytes). Wrapped in a
    // re-subscribe loop: on a session swap the old session's signal stream ends, so we
    // re-lock `active` for the new rd and subscribe afresh.
    {
        let (active, transport, local_serial) = (active.clone(), transport.clone(), local_serial.clone());
        tokio::spawn(async move {
            loop {
                let rd = active.lock().await.rd.clone();
                let mut sig = match rd.receive_selection_owner_changed().await {
                    Ok(s) => s,
                    Err(e) => {
                        tracing::warn!("SelectionOwnerChanged subscribe: {e}");
                        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
                        continue;
                    }
                };
                while let Some(ev) = sig.next().await {
                    let Ok(args) = ev.args() else { continue };
                    let opts = args.options;
                    // Skip our own SetSelection (flow 3) — avoid an offer echo.
                    if opt_bool(&opts, "session-is-owner") {
                        continue;
                    }
                    let mimes = opt_mime_types(&opts);
                    if mimes.is_empty() {
                        continue;
                    }
                    let serial = local_serial.fetch_add(1, Ordering::Relaxed);
                    tracing::debug!(target: "clip", "clone copied: offering {mimes:?} serial={serial}");
                    let _ = transport.send(&DaemonMsg::ClipboardOffer(ClipboardOffer { serial, mime_types: mimes }), &[]);
                }
                // Stream ended (session swapped/closed): re-subscribe against the current rd.
                tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            }
        });
    }

    // Flow 4: a clone app pastes the remote selection → request its bytes lazily. Same
    // re-subscribe loop as flow 1 so it follows a session swap.
    {
        let (active, transport, remote, pending) = (active.clone(), transport.clone(), remote.clone(), pending.clone());
        tokio::spawn(async move {
            loop {
                let rd = active.lock().await.rd.clone();
                let mut sig = match rd.receive_selection_transfer().await {
                    Ok(s) => s,
                    Err(e) => {
                        tracing::warn!("SelectionTransfer subscribe: {e}");
                        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
                        continue;
                    }
                };
                while let Some(ev) = sig.next().await {
                    let Ok(args) = ev.args() else { continue };
                    let (mime, mutter_serial) = (args.mime_type, args.serial);
                    let serial = match remote.lock().await.as_ref() {
                        Some(o) => o.serial,
                        None => {
                            let _ = rd.selection_write_done(mutter_serial, false).await;
                            continue;
                        }
                    };
                    pending.lock().await.entry(mime.clone()).or_default().push(mutter_serial);
                    let _ = transport.send(&DaemonMsg::ClipboardRequest(ClipboardRequest { serial, mime_type: mime }), &[]);
                }
                // Stream ended (session swapped/closed): re-subscribe against the current rd.
                tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            }
        });
    }

    // Flows 2 & 3 & the data reply for flow 4. Each op uses the CURRENT session's rd so it
    // follows a swap.
    while let Some(msg) = from_server.recv().await {
        let rd = active.lock().await.rd.clone();
        match msg {
            // Flow 3: a remote clipboard is available — own it in the clone.
            FromServer::Offer(o) => {
                let mut opts: HashMap<&str, Value<'_>> = HashMap::new();
                opts.insert("mime-types", Value::new(o.mime_types.clone()));
                match rd.set_selection(opts).await {
                    Ok(()) => {
                        tracing::debug!(target: "clip", "owning remote selection in clone: {:?}", o.mime_types);
                        *remote.lock().await = Some(o);
                    }
                    Err(e) => tracing::warn!("SetSelection: {e}"),
                }
            }
            // Flow 2: a remote wants our clone's data for `mime` — read + ship it.
            // Spawned: a slow source app must not wedge this loop (Chromium can sit on
            // a SelectionRead), and the fd read gets a hard timeout. ALWAYS reply, with
            // empty bytes on failure — a dropped reply leaves the broker's pending entry
            // and the requester's clipboard silently stale.
            FromServer::Request(r) => {
                let (rd, transport) = (rd.clone(), transport.clone());
                tokio::spawn(async move {
                    let started = std::time::Instant::now();
                    let bytes = match rd.selection_read(&r.mime_type).await {
                        Ok(fd) => {
                            let read = tokio::task::spawn_blocking(move || read_all(fd));
                            match tokio::time::timeout(std::time::Duration::from_secs(5), read).await {
                                Ok(Ok(Ok(bytes))) => bytes,
                                Ok(Ok(Err(e))) => {
                                    tracing::warn!("SelectionRead({}) read: {e}", r.mime_type);
                                    Vec::new()
                                }
                                Ok(Err(e)) => {
                                    tracing::warn!("SelectionRead({}) join: {e}", r.mime_type);
                                    Vec::new()
                                }
                                Err(_) => {
                                    tracing::warn!("SelectionRead({}) timed out after 5s (source app not responding?)", r.mime_type);
                                    Vec::new()
                                }
                            }
                        }
                        Err(e) => {
                            tracing::warn!("SelectionRead({}): {e}", r.mime_type);
                            Vec::new()
                        }
                    };
                    tracing::debug!(target: "clip",
                        "read {} -> {} bytes in {:?}",
                        r.mime_type, bytes.len(), started.elapsed()
                    );
                    let _ = transport.send(
                        &DaemonMsg::ClipboardData(ClipboardData { serial: r.serial, mime_type: r.mime_type, bytes }),
                        &[],
                    );
                });
            }
            // Flow 4 reply: bytes for a pending paste — write them to Mutter's fd.
            FromServer::Data(d) => {
                let serials = pending.lock().await.remove(&d.mime_type).unwrap_or_default();
                for mutter_serial in serials {
                    match rd.selection_write(mutter_serial).await {
                        Ok(fd) => {
                            let ok = write_all(fd, &d.bytes).is_ok();
                            let _ = rd.selection_write_done(mutter_serial, ok).await;
                        }
                        Err(e) => {
                            tracing::warn!("SelectionWrite: {e}");
                            let _ = rd.selection_write_done(mutter_serial, false).await;
                        }
                    }
                }
            }
        }
    }
}

/// Mutter delivers `SelectionOwnerChanged` `a{sv}` values wrapped in a 1-field D-Bus
/// **struct** (`mime-types` is `(as)`, not bare `as`; `session-is-owner` is `(b)`), so a
/// flat `Vec::<String>::try_from` / `bool::try_from` sees a `Structure` and fails. These
/// helpers recurse through struct / variant wrapping to the leaf value(s).

/// Collect every string reachable under `mime-types` (struct → array → str).
fn opt_mime_types(opts: &HashMap<String, OwnedValue>) -> Vec<String> {
    fn collect(val: &Value, out: &mut Vec<String>) {
        match val {
            Value::Str(s) => out.push(s.as_str().to_string()),
            Value::Array(a) => a.inner().iter().for_each(|e| collect(e, out)),
            Value::Structure(s) => s.fields().iter().for_each(|f| collect(f, out)),
            Value::Value(b) => collect(b, out),
            _ => {}
        }
    }
    let mut out = Vec::new();
    if let Some(v) = opts.get("mime-types") {
        collect(v, &mut out);
    }
    out
}

/// First bool reachable under `key` (struct/variant-unwrapped); default false.
fn opt_bool(opts: &HashMap<String, OwnedValue>, key: &str) -> bool {
    fn find(val: &Value) -> Option<bool> {
        match val {
            Value::Bool(b) => Some(*b),
            Value::Structure(s) => s.fields().iter().find_map(find),
            Value::Value(b) => find(b),
            _ => None,
        }
    }
    opts.get(key).and_then(|v| find(v)).unwrap_or(false)
}

/// Mutter hands back **non-blocking** pipe fds for SelectionRead/Write, so a plain
/// `read_to_end`/`write_all` fails with EAGAIN (os error 11) instead of waiting. Clear
/// `O_NONBLOCK` first so the synchronous transfer blocks until data / EOF.
fn set_blocking(fd: std::os::fd::RawFd) {
    unsafe {
        let flags = libc::fcntl(fd, libc::F_GETFL);
        if flags >= 0 {
            let _ = libc::fcntl(fd, libc::F_SETFL, flags & !libc::O_NONBLOCK);
        }
    }
}

/// Read a SelectionRead fd to EOF.
fn read_all(fd: zbus::zvariant::OwnedFd) -> std::io::Result<Vec<u8>> {
    use std::os::fd::AsRawFd;
    let owned = std::os::fd::OwnedFd::from(fd);
    set_blocking(owned.as_raw_fd());
    let mut f = std::fs::File::from(owned);
    let mut buf = Vec::new();
    f.read_to_end(&mut buf)?;
    Ok(buf)
}

/// Write all bytes to a SelectionWrite fd, then close it.
fn write_all(fd: zbus::zvariant::OwnedFd, bytes: &[u8]) -> std::io::Result<()> {
    use std::os::fd::AsRawFd;
    let owned = std::os::fd::OwnedFd::from(fd);
    set_blocking(owned.as_raw_fd());
    let mut f = std::fs::File::from(owned);
    f.write_all(bytes)?;
    f.flush()
}
