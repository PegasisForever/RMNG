//! SOCK_SEQPACKET media socket (server/receive side). Mirrors the clone-daemon
//! transport: each datagram is a JSON `DaemonMsg`, dmabuf fds via `SCM_RIGHTS`.

use std::io::{IoSlice, IoSliceMut};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};

use anyhow::{Context, Result, anyhow, bail};
use nix::sys::socket::{
    AddressFamily, ControlMessage, ControlMessageOwned, MsgFlags, SockFlag, SockType, UnixAddr,
    accept, bind, listen, recvmsg, sendmsg, socket, Backlog,
};
use wire::socket::{DaemonMsg, ServerMsg};

const MAX_PACKET_BYTES: usize = 32 * 1024 * 1024;

pub struct Listener {
    fd: OwnedFd,
    /// Held for the listener's lifetime — see [`Listener::bind`]. Dropping it releases the
    /// `flock`, which is what lets a genuine crash-restart re-acquire the socket.
    _lock: nix::fcntl::Flock<std::fs::File>,
}

impl Listener {
    /// Bind the media socket at `path`, taking exclusive ownership of it.
    ///
    /// Binding a unix socket requires unlinking whatever sits at the path first, and an
    /// unconditional unlink cannot distinguish "a crashed predecessor left this behind" from
    /// "another server is serving on this right now". Getting that wrong is silent and total:
    /// the newcomer unlinks the path and binds a fresh inode, while the incumbent goes on
    /// `accept()`ing an inode nothing can reach any more. Neither side errors, and every client
    /// that reconnects afterwards gets ECONNREFUSED forever.
    ///
    /// That is not hypothetical — it took out video for the CT 105 fleet: a dev control-server
    /// running inside a clone bound the same `/srv/rmng-sock/clones.sock` the production server
    /// was serving (the whole volume is mounted into clones), orphaning it. Clones that had
    /// connected earlier kept working, so it stayed invisible until the first clone restarted.
    ///
    /// So take an `flock` on a sidecar `<path>.lock` FIRST and only unlink once we hold it. The
    /// lock is advisory but sufficient: every binder runs this same code. It is released by the
    /// kernel when the holder exits, so a crashed predecessor leaves it free and recovery still
    /// works — while a LIVE holder makes this fail loudly instead of silently stealing the path.
    ///
    /// Clones additionally mount the socket dir read-only (see `docker.rs`), so a clone cannot
    /// unlink the path even if it bypasses this code entirely. Belt and braces: the lock stops
    /// well-behaved binders racing, the read-only mount stops the ill-behaved ones.
    pub fn bind(path: &str) -> Result<Self> {
        let lock = Self::acquire_lock(path)?;
        let _ = std::fs::remove_file(path); // safe now: we hold the lock, so no live owner
        let fd = socket(AddressFamily::Unix, SockType::SeqPacket, SockFlag::empty(), None)
            .context("socket")?;
        let addr = UnixAddr::new(path).context("UnixAddr")?;
        bind(fd.as_raw_fd(), &addr).with_context(|| format!("bind {path}"))?;
        listen(&fd, Backlog::new(4).unwrap()).context("listen")?;
        // World-writable: the control-server binds this as root in its container, and
        // each clone-daemon connects as the uid-1000 clone user from a sibling container
        // over the shared sock volume. No Docker idmapping is involved, but that
        // root-vs-1000 split still needs write perm for the non-root uid.
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o777));
        Ok(Self { fd, _lock: lock })
    }

    /// Take the exclusive `flock` guarding `path`, or fail describing who holds it.
    ///
    /// Non-blocking on purpose: waiting would hang startup behind a healthy server, and the
    /// honest outcome here is a hard error naming the conflict. `LOCK_EX | LOCK_NB` returns
    /// `EWOULDBLOCK` when another process holds it.
    fn acquire_lock(path: &str) -> Result<nix::fcntl::Flock<std::fs::File>> {
        use nix::fcntl::{Flock, FlockArg};

        let lock_path = format!("{path}.lock");
        if let Some(dir) = std::path::Path::new(&lock_path).parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let file = std::fs::OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(&lock_path)
            .with_context(|| format!("opening media socket lock {lock_path}"))?;
        // Same root-vs-1000 reasoning as the socket mode above: a non-root binder must be able
        // to reopen this lock file. Best-effort — a pre-existing file owned by another uid keeps
        // its mode, and the open above already proved we can use it.
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&lock_path, std::fs::Permissions::from_mode(0o666));

        match Flock::lock(file, FlockArg::LockExclusiveNonblock) {
            Ok(flock) => Ok(flock),
            Err((_, errno)) => bail!(
                "another process is already serving the media socket at {path} \
                 (lock {lock_path} held: {errno}). Refusing to steal it — stealing would \
                 silently orphan that server's listener and break video for every client \
                 that reconnects. Stop the other server first."
            ),
        }
    }

    pub fn accept(&self) -> Result<Conn> {
        let fd = accept(self.fd.as_raw_fd()).context("accept")?;
        // SAFETY: accept() returns a fresh owned fd.
        Ok(Conn { fd: unsafe { OwnedFd::from_raw_fd(fd) } })
    }
}

pub struct Conn {
    fd: OwnedFd,
}

impl Conn {
    /// Receive one `DaemonMsg` + any dmabuf fds (as owned fds).
    pub fn recv(&self) -> Result<(DaemonMsg, Vec<OwnedFd>)> {
        let packet_len = recv_packet_len(self.fd.as_raw_fd())?;
        let mut buf = vec![0u8; packet_len];
        let mut iov = [IoSliceMut::new(&mut buf)];
        let mut cmsg = nix::cmsg_space!([RawFd; 8]);
        let msg: nix::sys::socket::RecvMsg<()> =
            recvmsg(self.fd.as_raw_fd(), &mut iov, Some(&mut cmsg), MsgFlags::empty()).context("recvmsg")?;
        let mut fds = Vec::new();
        if let Ok(cmsgs) = msg.cmsgs() {
            for c in cmsgs {
                if let ControlMessageOwned::ScmRights(raw) = c {
                    // SAFETY: SCM_RIGHTS handed us fresh owned fds.
                    fds.extend(raw.into_iter().map(|f| unsafe { OwnedFd::from_raw_fd(f) }));
                }
            }
        }
        let n = msg.bytes;
        if n == 0 {
            return Err(anyhow!("peer closed"));
        }
        let dm = serde_json::from_slice(&buf[..n]).context("decode DaemonMsg")?;
        Ok((dm, fds))
    }

    /// Send a `ServerMsg` (no fds).
    pub fn send(&self, msg: &ServerMsg) -> Result<()> {
        let json = serde_json::to_vec(msg)?;
        let iov = [IoSlice::new(&json)];
        let cmsgs: &[ControlMessage] = &[];
        sendmsg::<()>(self.fd.as_raw_fd(), &iov, cmsgs, MsgFlags::empty(), None).context("sendmsg")?;
        Ok(())
    }
}

fn recv_packet_len(fd: RawFd) -> Result<usize> {
    let mut one = [0u8; 1];
    let mut iov = [IoSliceMut::new(&mut one)];
    let msg: nix::sys::socket::RecvMsg<()> =
        recvmsg(fd, &mut iov, None, MsgFlags::MSG_PEEK | MsgFlags::MSG_TRUNC)
            .context("recvmsg peek")?;
    let n = msg.bytes;
    if n == 0 {
        return Err(anyhow!("peer closed"));
    }
    if n > MAX_PACKET_BYTES {
        return Err(anyhow!("packet too large: {n} bytes > {MAX_PACKET_BYTES}"));
    }
    Ok(n)
}

#[cfg(test)]
mod tests {
    use super::*;
    use nix::sys::socket::connect;
    use std::os::unix::fs::MetadataExt;
    use wire::socket::{ClipboardData, DaemonMsg};

    fn tmp_sock_path(name: &str) -> String {
        std::env::temp_dir()
            .join(format!("rmng-{name}-{}-{}.sock", std::process::id(), rand_suffix()))
            .to_string_lossy()
            .into_owned()
    }

    fn rand_suffix() -> u64 {
        use std::sync::atomic::{AtomicU64, Ordering};
        static N: AtomicU64 = AtomicU64::new(0);
        N.fetch_add(1, Ordering::Relaxed)
    }

    fn send_daemon_msg(path: &str, msg: &DaemonMsg) -> Result<()> {
        let fd = socket(AddressFamily::Unix, SockType::SeqPacket, SockFlag::empty(), None)
            .context("socket")?;
        connect(fd.as_raw_fd(), &UnixAddr::new(path).unwrap()).context("connect")?;
        let json = serde_json::to_vec(msg)?;
        let iov = [IoSlice::new(&json)];
        sendmsg::<()>(fd.as_raw_fd(), &iov, &[], MsgFlags::empty(), None).context("sendmsg")?;
        Ok(())
    }

    /// A second binder must NOT be able to steal a live socket.
    ///
    /// Regression (this is the CT 105 outage): `bind` unconditionally unlinked the path, so a
    /// second server silently took it over — its `bind` succeeded, and the first server kept
    /// `accept()`ing an unlinked inode no client could reach. Both sides stayed quiet. Now the
    /// second binder fails loudly and, critically, the FIRST listener keeps working.
    #[test]
    fn a_second_bind_cannot_steal_a_live_socket() {
        let path = tmp_sock_path("steal");
        let first = Listener::bind(&path).unwrap();
        let inode_before = std::fs::metadata(&path).unwrap().ino();

        let msg = match Listener::bind(&path) {
            Ok(_) => panic!("second bind must be refused while the first is live"),
            Err(e) => format!("{e:#}"),
        };
        assert!(
            msg.contains("already serving"),
            "error should name the conflict, got: {msg}"
        );

        // The incumbent must be untouched — same inode, still accepting real traffic.
        assert_eq!(
            std::fs::metadata(&path).unwrap().ino(),
            inode_before,
            "the live socket file must not have been replaced"
        );
        let sent = DaemonMsg::ClipboardData(ClipboardData {
            serial: 1,
            mime_type: "text/plain".into(),
            bytes: b"still-alive".to_vec(),
        });
        send_daemon_msg(&path, &sent).unwrap();
        let (got, _) = first.accept().unwrap().recv().unwrap();
        assert_eq!(got, sent, "the first listener must still serve clients");

        drop(first);
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(format!("{path}.lock"));
    }

    /// A crashed predecessor must not wedge the socket forever: the kernel drops its `flock`
    /// when the process (here, the `Listener`) goes away, so a restart re-acquires cleanly.
    /// This is why the lock is used instead of a "connect-probe then unlink" heuristic.
    #[test]
    fn a_dead_predecessor_does_not_block_rebinding() {
        let path = tmp_sock_path("rebind");
        let first = Listener::bind(&path).unwrap();
        drop(first); // simulate the previous server exiting (crash or clean stop)

        let second = Listener::bind(&path).expect("must rebind after the holder is gone");
        let sent = DaemonMsg::ClipboardData(ClipboardData {
            serial: 2,
            mime_type: "text/plain".into(),
            bytes: b"reborn".to_vec(),
        });
        send_daemon_msg(&path, &sent).unwrap();
        let (got, _) = second.accept().unwrap().recv().unwrap();
        assert_eq!(got, sent);

        drop(second);
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(format!("{path}.lock"));
    }

    #[test]
    fn recv_accepts_large_clipboard_data_messages() {
        let path = tmp_sock_path("large-clip");
        let listener = Listener::bind(&path).unwrap();
        let payload = vec![0x42; 128 * 1024];
        let sent = DaemonMsg::ClipboardData(ClipboardData {
            serial: 7,
            mime_type: "image/png".into(),
            bytes: payload.clone(),
        });

        send_daemon_msg(&path, &sent).unwrap();
        let conn = listener.accept().unwrap();
        let (got, fds) = conn.recv().unwrap();
        let _ = std::fs::remove_file(&path);

        assert!(fds.is_empty());
        assert_eq!(got, sent);
    }
}
