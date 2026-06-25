//! SOCK_SEQPACKET media socket (server/receive side). Mirrors the clone-daemon
//! transport: each datagram is a JSON `DaemonMsg`, dmabuf fds via `SCM_RIGHTS`.

use std::io::{IoSlice, IoSliceMut};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};

use anyhow::{Context, Result, anyhow};
use nix::sys::socket::{
    AddressFamily, ControlMessage, ControlMessageOwned, MsgFlags, SockFlag, SockType, UnixAddr,
    accept, bind, listen, recvmsg, sendmsg, socket, Backlog,
};
use wire::socket::{DaemonMsg, ServerMsg};

pub struct Listener {
    fd: OwnedFd,
}

impl Listener {
    pub fn bind(path: &str) -> Result<Self> {
        let _ = std::fs::remove_file(path); // stale socket
        let fd = socket(AddressFamily::Unix, SockType::SeqPacket, SockFlag::empty(), None)
            .context("socket")?;
        let addr = UnixAddr::new(path).context("UnixAddr")?;
        bind(fd.as_raw_fd(), &addr).with_context(|| format!("bind {path}"))?;
        listen(&fd, Backlog::new(4).unwrap()).context("listen")?;
        // World-accessible so clone-daemons in *other* (uid-mapped) CTs can connect
        // over the bind-mounted host socket — connecting needs write perm on the node.
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o777));
        Ok(Self { fd })
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
        let mut buf = vec![0u8; 65536];
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
