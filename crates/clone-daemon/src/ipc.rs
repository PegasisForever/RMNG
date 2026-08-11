//! The `SOCK_SEQPACKET` socket between the daemon and the session holder.
//!
//! Same framing as [`crate::transport`]: one JSON message per datagram, oversized messages
//! split by [`wire::socket::chunk`]. No file descriptors cross, so there is no `SCM_RIGHTS`
//! handling here, and both directions are symmetric enough for one [`Conn`] type.
//!
//! Sends are blocking with a send timeout rather than non-blocking with a drop, because a
//! chunked clipboard payload cannot survive losing a datagram in the middle. The timeout is
//! what stops a peer that has stopped reading from wedging the sender forever.

use std::io::{IoSlice, IoSliceMut};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};

use anyhow::{Context, Result, anyhow, bail};
use nix::sys::time::{TimeVal, TimeValLike};
use nix::sys::socket::{
    AddressFamily, Backlog, MsgFlags, SockFlag, SockType, UnixAddr, accept, bind, connect, listen,
    recvmsg, sendmsg, setsockopt, socket, sockopt,
};
use serde::Serialize;
use serde::de::DeserializeOwned;

/// Refuses a datagram larger than any message this protocol sends, so a corrupt length can
/// never make us allocate wildly. Matches the media transport's ceiling.
const MAX_PACKET_BYTES: usize = 32 * 1024 * 1024;

/// How long a send may block before we call the peer dead, in seconds.
///
/// Only reached when the peer has stopped draining its receive queue entirely: 208 KB of
/// buffer at 64 KiB a chunk means a healthy peer never gets close.
const SEND_TIMEOUT_SECS: i64 = 5;

/// One connected end of the holder socket.
pub struct Conn {
    fd: OwnedFd,
    /// Serializes whole messages. Without it two threads splitting large messages could
    /// interleave their chunks on the wire, and the reassembler at the far end takes one
    /// message at a time.
    send_lock: std::sync::Mutex<()>,
    /// Puts a chunked message back together. One thread reads a connection, so this sits
    /// behind a mutex rather than in `recv`'s signature.
    joiner: std::sync::Mutex<wire::socket::chunk::Reassembler>,
    next_id: std::sync::atomic::AtomicU64,
}

impl Conn {
    fn adopt(fd: OwnedFd) -> Result<Self> {
        setsockopt(&fd, sockopt::SendTimeout, &TimeVal::seconds(SEND_TIMEOUT_SECS))
            .context("SO_SNDTIMEO")?;
        Ok(Self {
            fd,
            send_lock: std::sync::Mutex::new(()),
            joiner: std::sync::Mutex::new(Default::default()),
            next_id: std::sync::atomic::AtomicU64::new(1),
        })
    }

    /// Connect to a holder listening at `path`.
    pub fn connect(path: &str) -> Result<Self> {
        let fd = socket(AddressFamily::Unix, SockType::SeqPacket, SockFlag::empty(), None)
            .context("socket(AF_UNIX, SEQPACKET)")?;
        let addr = UnixAddr::new(path).context("UnixAddr")?;
        connect(fd.as_raw_fd(), &addr).with_context(|| format!("connect {path}"))?;
        Self::adopt(fd)
    }

    /// Send one message, split across datagrams if it does not fit in one.
    pub fn send<T: Serialize>(&self, msg: &T) -> Result<()> {
        use std::sync::atomic::Ordering;
        let json = serde_json::to_vec(msg)?;
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let _guard = self.send_lock.lock().unwrap();
        for datagram in wire::socket::chunk::split(&json, id) {
            let iov = [IoSlice::new(&datagram)];
            sendmsg::<()>(self.fd.as_raw_fd(), &iov, &[], MsgFlags::empty(), None)
                .context("sendmsg")?;
        }
        Ok(())
    }

    /// End this connection, so a reader parked in [`Conn::recv`] returns instead of waiting
    /// for a peer that has been replaced. Closing the fd would not do it: the reader thread
    /// holds its own reference to this connection.
    pub fn shutdown(&self) {
        let _ = nix::sys::socket::shutdown(
            self.fd.as_raw_fd(),
            nix::sys::socket::Shutdown::Both,
        );
    }

    /// Receive one message, blocking until every chunk of it has arrived.
    pub fn recv<T: DeserializeOwned>(&self) -> Result<T> {
        loop {
            let packet_len = recv_packet_len(self.fd.as_raw_fd())?;
            let mut buf = vec![0u8; packet_len];
            let mut iov = [IoSliceMut::new(&mut buf)];
            let msg: nix::sys::socket::RecvMsg<()> =
                recvmsg(self.fd.as_raw_fd(), &mut iov, None, MsgFlags::empty())
                    .context("recvmsg")?;
            let n = msg.bytes;
            if n == 0 {
                return Err(anyhow!("the holder socket peer closed"));
            }
            let Some(whole) = self.joiner.lock().unwrap().push(&buf[..n]) else {
                continue; // A chunk, and not the last one.
            };
            return serde_json::from_slice(&whole).context("decoding a holder-socket message");
        }
    }
}

/// The holder's end: binds the path and accepts one daemon at a time.
pub struct Listener {
    fd: OwnedFd,
    /// Held for the listener's lifetime. Dropping it releases the `flock`, so a crashed
    /// predecessor leaves the path free while a live one keeps it.
    _lock: nix::fcntl::Flock<std::fs::File>,
}

impl Listener {
    /// Bind `path`, taking exclusive ownership of it.
    ///
    /// The unlink-then-bind dance needs the same guard the media socket uses: unlinking a
    /// path a live holder is serving would leave it accepting an inode no daemon can reach,
    /// silently, forever. Take the `flock` first, and only unlink once we hold it.
    pub fn bind(path: &str) -> Result<Self> {
        use nix::fcntl::{Flock, FlockArg};
        use std::os::unix::fs::PermissionsExt;

        let lock_path = format!("{path}.lock");
        let file = std::fs::OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(&lock_path)
            .with_context(|| format!("opening holder socket lock {lock_path}"))?;
        let lock = match Flock::lock(file, FlockArg::LockExclusiveNonblock) {
            Ok(l) => l,
            Err((_, errno)) => bail!(
                "another session holder is already serving {path} (lock {lock_path} held: \
                 {errno}). Refusing to steal it, because stealing would orphan that holder's \
                 listener while it keeps the clone's monitors open."
            ),
        };
        let _ = std::fs::remove_file(path); // safe now: we hold the lock

        let fd = socket(AddressFamily::Unix, SockType::SeqPacket, SockFlag::empty(), None)
            .context("socket")?;
        let addr = UnixAddr::new(path).context("UnixAddr")?;
        bind(fd.as_raw_fd(), &addr).with_context(|| format!("bind {path}"))?;
        listen(&fd, Backlog::new(4).unwrap()).context("listen")?;
        // Holder and daemon both run as the clone user, so owner-only is enough.
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
        Ok(Self { fd, _lock: lock })
    }

    /// Block until a daemon connects.
    pub fn accept(&self) -> Result<Conn> {
        let fd = accept(self.fd.as_raw_fd()).context("accept")?;
        // SAFETY: accept() returns a fresh owned fd.
        Conn::adopt(unsafe { OwnedFd::from_raw_fd(fd) })
    }
}

/// Peek the next datagram's length so the read buffer is exactly its size (`MSG_TRUNC`
/// reports the true length even though only one byte is copied).
fn recv_packet_len(fd: RawFd) -> Result<usize> {
    let mut one = [0u8; 1];
    let mut iov = [IoSliceMut::new(&mut one)];
    let msg: nix::sys::socket::RecvMsg<()> =
        recvmsg(fd, &mut iov, None, MsgFlags::MSG_PEEK | MsgFlags::MSG_TRUNC)
            .context("recvmsg peek")?;
    let n = msg.bytes;
    if n == 0 {
        return Err(anyhow!("the holder socket peer closed"));
    }
    if n > MAX_PACKET_BYTES {
        return Err(anyhow!("packet too large: {n} bytes > {MAX_PACKET_BYTES}"));
    }
    Ok(n)
}

#[cfg(test)]
mod tests {
    use super::*;
    use wire::holder::{FromHolder, HolderMonitor, ToHolder};
    use wire::socket::InputMsg;

    fn temp_path(name: &str) -> String {
        let dir = std::env::var("TMPDIR").unwrap_or_else(|_| "/tmp".into());
        format!("{dir}/rmng-ipc-test-{}-{name}.sock", std::process::id())
    }

    #[test]
    fn a_message_crosses_and_keeps_its_shape() {
        let path = temp_path("round-trip");
        let listener = Listener::bind(&path).unwrap();
        let client = std::thread::spawn({
            let path = path.clone();
            move || {
                let c = Conn::connect(&path).unwrap();
                c.send(&ToHolder::Input(InputMsg::Key { keysym: 0x61, pressed: true })).unwrap();
                c.recv::<FromHolder>().unwrap()
            }
        });

        let server = listener.accept().unwrap();
        let got: ToHolder = server.recv().unwrap();
        assert_eq!(got, ToHolder::Input(InputMsg::Key { keysym: 0x61, pressed: true }));
        server.send(&FromHolder::SwapDone { generation: 3 }).unwrap();

        assert_eq!(client.join().unwrap(), FromHolder::SwapDone { generation: 3 });
        let _ = std::fs::remove_file(&path);
    }

    /// A pasted image is far past the datagram ceiling, so the chunking has to work on this
    /// socket too, not only on the media one.
    #[test]
    fn an_image_sized_message_crosses_whole() {
        let path = temp_path("chunked");
        let listener = Listener::bind(&path).unwrap();
        let bytes: Vec<u8> = (0..2 * 1024 * 1024).map(|i| (i % 251) as u8).collect();
        let sent = wire::socket::ClipboardData {
            serial: 9,
            mime_type: "image/png".into(),
            bytes: bytes.clone(),
        };

        let client = std::thread::spawn({
            let (path, sent) = (path.clone(), sent.clone());
            move || {
                let c = Conn::connect(&path).unwrap();
                c.send(&ToHolder::ClipboardData(sent)).unwrap();
            }
        });

        let server = listener.accept().unwrap();
        let got: ToHolder = server.recv().unwrap();
        client.join().unwrap();
        match got {
            ToHolder::ClipboardData(d) => {
                assert_eq!(d.bytes.len(), bytes.len());
                assert_eq!(d.bytes, bytes);
            }
            other => panic!("expected clipboard data, got {other:?}"),
        }
        let _ = std::fs::remove_file(&path);
    }

    /// Stealing a live holder's path would orphan it while it still holds the clone's
    /// monitors open, so a second bind has to fail instead.
    #[test]
    fn a_second_bind_cannot_steal_a_live_socket() {
        let path = temp_path("steal");
        let _first = Listener::bind(&path).unwrap();
        let err = match Listener::bind(&path) {
            Ok(_) => panic!("second bind must be refused while the first is live"),
            Err(e) => e.to_string(),
        };
        assert!(err.contains("already serving"), "unhelpful error: {err}");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn monitors_survive_the_wire() {
        let path = temp_path("monitors");
        let listener = Listener::bind(&path).unwrap();
        let mons = vec![
            HolderMonitor { monitor_id: 0, node_id: 51, width: 1920, height: 1080, x: 0, y: 0, primary: true },
            HolderMonitor { monitor_id: 1, node_id: 52, width: 1920, height: 1080, x: 1920, y: 0, primary: false },
        ];
        let client = std::thread::spawn({
            let path = path.clone();
            move || Conn::connect(&path).unwrap().recv::<FromHolder>().unwrap()
        });
        let server = listener.accept().unwrap();
        server.send(&FromHolder::HelloOk { proto: 1, generation: 2, monitors: mons.clone() }).unwrap();
        assert_eq!(
            client.join().unwrap(),
            FromHolder::HelloOk { proto: 1, generation: 2, monitors: mons }
        );
        let _ = std::fs::remove_file(&path);
    }
}
