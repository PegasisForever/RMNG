//! SOCK_SEQPACKET transport to control-server's media ingest. Each datagram is a
//! JSON `DaemonMsg`/`ServerMsg`; dmabuf fds ride along via `SCM_RIGHTS`. SEQPACKET
//! gives one-message-per-datagram framing, so a frame and its fds stay together.

use std::io::{IoSlice, IoSliceMut};
use std::os::fd::{AsRawFd, OwnedFd, RawFd};

use anyhow::{Context, Result, anyhow};
use nix::sys::socket::{
    AddressFamily, ControlMessage, ControlMessageOwned, MsgFlags, SockFlag, SockType, UnixAddr,
    connect, recvmsg, sendmsg, socket,
};
use wire::socket::{DaemonMsg, ServerMsg};

pub struct Transport {
    fd: OwnedFd,
}

impl Transport {
    /// Connect to control-server's bind-mounted media socket.
    pub fn connect(path: &str) -> Result<Self> {
        let fd = socket(AddressFamily::Unix, SockType::SeqPacket, SockFlag::empty(), None)
            .context("socket(AF_UNIX, SEQPACKET)")?;
        let addr = UnixAddr::new(path).context("UnixAddr")?;
        connect(fd.as_raw_fd(), &addr).with_context(|| format!("connect {path}"))?;
        Ok(Self { fd })
    }

    /// Send a framed message + optional dmabuf fds (SCM_RIGHTS).
    pub fn send(&self, msg: &DaemonMsg, fds: &[RawFd]) -> Result<()> {
        let json = serde_json::to_vec(msg)?;
        let iov = [IoSlice::new(&json)];
        let cmsgs: &[ControlMessage] =
            if fds.is_empty() { &[] } else { &[ControlMessage::ScmRights(fds)] };
        sendmsg::<()>(self.fd.as_raw_fd(), &iov, cmsgs, MsgFlags::empty(), None)
            .context("sendmsg")?;
        Ok(())
    }

    /// Receive one `ServerMsg` (server → daemon carries no fds). Blocking.
    pub fn recv(&self) -> Result<ServerMsg> {
        let mut buf = vec![0u8; 65536];
        let mut iov = [IoSliceMut::new(&mut buf)];
        let mut cmsg = nix::cmsg_space!([RawFd; 8]);
        let msg: nix::sys::socket::RecvMsg<()> =
            recvmsg(self.fd.as_raw_fd(), &mut iov, Some(&mut cmsg), MsgFlags::empty())
                .context("recvmsg")?;
        // Drain any (unexpected) fds so they don't leak.
        if let Ok(cmsgs) = msg.cmsgs() {
            for c in cmsgs {
                if let ControlMessageOwned::ScmRights(fds) = c {
                    for fd in fds {
                        let _ = nix::unistd::close(fd);
                    }
                }
            }
        }
        let n = msg.bytes;
        if n == 0 {
            return Err(anyhow!("peer closed the media socket"));
        }
        serde_json::from_slice(&buf[..n]).context("decode ServerMsg")
    }
}
